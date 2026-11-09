//! Scalar functions defined by the client (DuckDB's `capi/scalar_function-c.cpp`).
//!
//! A `duckdb_scalar_function` is a definition the client fills in; adding it to a set or
//! registering it copies it into an engine [`ScalarFunction`] whose bind callback and
//! kernel call the client's callbacks (`CScalarFunctionBind`, `CScalarFunctionInit`,
//! `CAPIScalarFunction`). The callbacks and the extra info live in a
//! [`CScalarFunctionInfo`] that the definition and every copy share, as DuckDB's
//! `ScalarFunctionInfo` is shared by the copies of a `ScalarFunction`: it is released,
//! calling the extra info's delete callback, when the last of them is gone.

use std::any::Any;
use std::ffi::{c_char, c_void};
use std::sync::{Arc, PoisonError, RwLock};

use roastedduck_common::{Error, Result};
use roastedduck_function::{
    BindInput, FunctionClient, FunctionData, FunctionState, NullHandling, ScalarFunction, ScalarFunctionSet,
    Stability,
};
use roastedduck_planner::{ClientContext as BinderContext, Expression};
use roastedduck_types::{DataChunk, LogicalType, STANDARD_VECTOR_SIZE, Vector};

use crate::data_chunk::{DuckdbDataChunk, DuckdbVector};
use crate::expression::{DuckdbExpression, wrap_expression};
use crate::function::{
    DuckdbBindInfo, DuckdbCopyCallback, DuckdbDeleteCallback, DuckdbFunctionInfo, DuckdbInitInfo, UserData,
    error_bytes, is_valid_parameter_type, is_valid_return_type,
};
use crate::helper::{catch, opt_str};
use crate::logical_types::{DuckdbLogicalType, type_ref};
use crate::open::{DuckdbClientContext, DuckdbConnection, connection_ref, wrap_client_context};
use crate::types::*;

/// `duckdb_scalar_function_bind_t`
pub type DuckdbScalarFunctionBind = Option<unsafe extern "C" fn(DuckdbBindInfo)>;
/// `duckdb_scalar_function_init_t`
pub type DuckdbScalarFunctionInit = Option<unsafe extern "C" fn(DuckdbInitInfo)>;
/// `duckdb_scalar_function_t`
pub type DuckdbScalarFunctionT =
    Option<unsafe extern "C" fn(DuckdbFunctionInfo, DuckdbDataChunk, DuckdbVector)>;

/// The client's callbacks and extra info of a scalar function.
#[derive(Clone, Copy, Debug)]
struct ScalarCallbacks {
    bind: DuckdbScalarFunctionBind,
    init: DuckdbScalarFunctionInit,
    function: DuckdbScalarFunctionT,
    extra_info: *mut c_void,
    delete_callback: DuckdbDeleteCallback,
}

impl Default for ScalarCallbacks {
    fn default() -> Self {
        Self {
            bind: None,
            init: None,
            function: None,
            extra_info: std::ptr::null_mut(),
            delete_callback: None,
        }
    }
}

/// DuckDB's `CScalarFunctionInfo`: the client's callbacks and extra info, shared by a
/// definition and its copies, which see later changes to it. The extra info is released
/// with its delete callback when the last of them is gone.
#[derive(Debug, Default)]
struct CScalarFunctionInfo {
    callbacks: RwLock<ScalarCallbacks>,
}

// SAFETY: the extra info belongs to the client, which the C API contract makes
// responsible for its use from the threads that run the function's callbacks; the
// callbacks are plain C function pointers.
unsafe impl Send for CScalarFunctionInfo {}
// SAFETY: see above; the fields are only changed under the lock.
unsafe impl Sync for CScalarFunctionInfo {}

impl CScalarFunctionInfo {
    fn callbacks(&self) -> ScalarCallbacks {
        *self.callbacks.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_callbacks(&self, update: impl FnOnce(&mut ScalarCallbacks)) {
        update(&mut self.callbacks.write().unwrap_or_else(PoisonError::into_inner));
    }
}

impl Drop for CScalarFunctionInfo {
    fn drop(&mut self) {
        let callbacks = self.callbacks();
        drop(UserData::new(callbacks.extra_info, callbacks.delete_callback));
    }
}

/// What a `duckdb_scalar_function` points to: the function's signature and properties
/// (DuckDB's `ScalarFunction`) and its shared callbacks.
#[derive(Debug)]
pub struct CScalarFunction {
    name: String,
    arguments: Vec<LogicalType>,
    varargs: Option<LogicalType>,
    return_type: LogicalType,
    null_handling: NullHandling,
    stability: Stability,
    info: Arc<CScalarFunctionInfo>,
}

/// `duckdb_scalar_function`
pub type DuckdbScalarFunction = *mut CScalarFunction;

impl CScalarFunction {
    /// The engine function calling the client's callbacks.
    fn to_function(&self) -> ScalarFunction {
        let kernel_info = Arc::clone(&self.info);
        let volatile = self.stability == Stability::Volatile;
        let mut function = ScalarFunction::new(
            self.name.as_str(),
            self.arguments.clone(),
            self.return_type.clone(),
            move |input, state, result| capi_scalar_function(&kernel_info, volatile, input, state, result),
        );
        let bind_info = Arc::clone(&self.info);
        function =
            function.with_bind(move |function, input| c_scalar_function_bind(&bind_info, function, input));
        function.varargs.clone_from(&self.varargs);
        function.null_handling = self.null_handling;
        function.stability = self.stability;
        function
    }

    /// Whether `duckdb_register_scalar_function_set` accepts the function.
    fn is_registrable(&self) -> bool {
        !self.name.is_empty()
            && self.info.callbacks().function.is_some()
            && is_valid_return_type(&self.return_type)
            && self.arguments.iter().all(is_valid_parameter_type)
    }
}

/// What a `duckdb_scalar_function_set` points to (DuckDB's `ScalarFunctionSet`): copies of
/// the functions added to it.
#[derive(Debug)]
pub struct CScalarFunctionSet {
    name: String,
    functions: Vec<CScalarFunction>,
}

/// `duckdb_scalar_function_set`
pub type DuckdbScalarFunctionSet = *mut CScalarFunctionSet;

/// The bind data of a call (DuckDB's `CScalarFunctionBindData`): the client's bind data
/// and the connection that bound the call, whose context init callbacks see.
#[derive(Debug)]
struct CScalarFunctionBindData {
    info: Arc<CScalarFunctionInfo>,
    bind_data: UserData,
    client: Arc<FunctionClient>,
}

impl FunctionData for CScalarFunctionBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    /// `CScalarFunctionBindData::Equals`: the same extra info and function callback.
    fn equals(&self, other: &dyn FunctionData) -> bool {
        other.as_any().downcast_ref::<Self>().is_some_and(|other| {
            let (mine, theirs) = (self.info.callbacks(), other.info.callbacks());
            mine.extra_info == theirs.extra_info
                && mine.function.map(|function| function as usize)
                    == theirs.function.map(|function| function as usize)
        })
    }
}

/// What a `duckdb_bind_info` of a scalar function points to (DuckDB's
/// `CScalarFunctionInternalBindInfo`).
struct CScalarFunctionInternalBindInfo<'a> {
    client: &'a Arc<FunctionClient>,
    /// The bound argument expressions and the evaluator that folds them.
    arguments: &'a [Expression],
    binder: Option<&'a BinderContext>,
    bind_data: &'a mut CScalarFunctionBindData,
    error: Option<Vec<u8>>,
}

/// The local state of one execution (DuckDB's `CScalarFunctionLocalState`).
#[derive(Debug)]
struct CScalarFunctionLocalState {
    local_state: UserData,
}

/// What a `duckdb_init_info` of a scalar function points to (DuckDB's
/// `CScalarFunctionInternalInitInfo`).
struct CScalarFunctionInternalInitInfo<'a> {
    bind_data: &'a CScalarFunctionBindData,
    local_state: &'a mut CScalarFunctionLocalState,
    error: Option<Vec<u8>>,
}

/// What a `duckdb_function_info` of a scalar function points to (DuckDB's
/// `CScalarFunctionInternalFunctionInfo`).
struct CScalarFunctionInternalFunctionInfo<'a> {
    bind_data: &'a CScalarFunctionBindData,
    local_state: &'a CScalarFunctionLocalState,
    extra_info: *mut c_void,
    error: Option<Vec<u8>>,
}

/// The bind info behind a handle.
///
/// # Safety
/// `info` must be NULL or the `duckdb_bind_info` of a scalar function's running bind
/// callback.
unsafe fn bind_info<'a>(info: DuckdbBindInfo) -> Option<&'a mut CScalarFunctionInternalBindInfo<'a>> {
    // SAFETY: guaranteed by the caller.
    unsafe { info.cast::<CScalarFunctionInternalBindInfo<'a>>().as_mut() }
}

/// The init info behind a handle.
///
/// # Safety
/// `info` must be NULL or the `duckdb_init_info` of a scalar function's running init
/// callback.
unsafe fn init_info<'a>(info: DuckdbInitInfo) -> Option<&'a mut CScalarFunctionInternalInitInfo<'a>> {
    // SAFETY: guaranteed by the caller.
    unsafe { info.cast::<CScalarFunctionInternalInitInfo<'a>>().as_mut() }
}

/// The function info behind a handle.
///
/// # Safety
/// `info` must be NULL or the `duckdb_function_info` of a scalar function's running
/// function callback.
unsafe fn function_info<'a>(
    info: DuckdbFunctionInfo,
) -> Option<&'a mut CScalarFunctionInternalFunctionInfo<'a>> {
    // SAFETY: guaranteed by the caller.
    unsafe { info.cast::<CScalarFunctionInternalFunctionInfo<'a>>().as_mut() }
}

/// `CScalarFunctionBind`: runs the client's bind callback, whose error is a binder error.
fn c_scalar_function_bind(
    info: &Arc<CScalarFunctionInfo>,
    _function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let mut bind_data = CScalarFunctionBindData {
        info: Arc::clone(info),
        bind_data: UserData::none(),
        client: Arc::clone(input.client),
    };
    if let Some(bind) = info.callbacks().bind {
        let arguments = input
            .arguments
            .and_then(|arguments| arguments.downcast_ref::<Vec<Expression>>())
            .map_or(&[][..], Vec::as_slice);
        let mut bind_info = CScalarFunctionInternalBindInfo {
            client: input.client,
            arguments,
            binder: input
                .binder
                .and_then(|binder| binder.downcast_ref::<BinderContext>()),
            bind_data: &mut bind_data,
            error: None,
        };
        // SAFETY: the client's callback receives the bind info, valid for the call.
        unsafe { bind(std::ptr::from_mut(&mut bind_info).cast()) };
        if let Some(error) = bind_info.error {
            return Err(Error::new_bytes(roastedduck_common::ErrorKind::Binder, error));
        }
    }
    Ok(Some(Arc::new(bind_data)))
}

/// `CScalarFunctionInit`: runs the client's init callback for one execution, whose error
/// is an invalid-input error. DuckDB runs it when the expression's state is created; the
/// engine creates states before it knows of failures, so it runs before the first chunk.
fn c_scalar_function_init(bind_data: &CScalarFunctionBindData) -> Result<CScalarFunctionLocalState> {
    let mut local_state = CScalarFunctionLocalState {
        local_state: UserData::none(),
    };
    if let Some(init) = bind_data.info.callbacks().init {
        let mut init_info = CScalarFunctionInternalInitInfo {
            bind_data,
            local_state: &mut local_state,
            error: None,
        };
        // SAFETY: the client's callback receives the init info, valid for the call.
        unsafe { init(std::ptr::from_mut(&mut init_info).cast()) };
        if let Some(error) = init_info.error {
            return Err(Error::new_bytes(
                roastedduck_common::ErrorKind::InvalidInput,
                error,
            ));
        }
    }
    Ok(local_state)
}

/// `CAPIScalarFunction`: calls the client's function on the flattened input, with a
/// result vector of `STANDARD_VECTOR_SIZE` rows; constant inputs give a constant result
/// unless the function is volatile.
fn capi_scalar_function(
    info: &CScalarFunctionInfo,
    volatile: bool,
    input: &DataChunk,
    state: &mut FunctionState,
    result: &mut Vector,
) -> Result<()> {
    let bind_data = state
        .bind_data
        .as_deref()
        .and_then(|data| data.as_any().downcast_ref::<CScalarFunctionBindData>())
        .ok_or_else(|| Error::internal("C API scalar function without its bind data"))?;
    if state.local.is_none() {
        state.local = Some(Box::new(c_scalar_function_init(bind_data)?));
    }
    let local_state = state
        .local
        .as_ref()
        .and_then(|local| local.downcast_ref::<CScalarFunctionLocalState>())
        .ok_or_else(|| Error::internal("C API scalar function without its local state"))?;
    let count = input.len();
    let all_constant = input.columns().iter().all(Vector::is_constant);
    let mut flat_input = input.clone();
    for column in 0..flat_input.column_count() {
        flat_input.column_mut(column).flatten(count);
    }
    *result = Vector::flat(result.logical_type().clone(), STANDARD_VECTOR_SIZE.max(count));
    let callbacks = info.callbacks();
    let Some(function) = callbacks.function else {
        return Err(Error::internal(
            "C API scalar function without a function callback",
        ));
    };
    let mut function_info = CScalarFunctionInternalFunctionInfo {
        bind_data,
        local_state,
        extra_info: callbacks.extra_info,
        error: None,
    };
    // SAFETY: the client's callback receives the function info, the input chunk and the
    // result vector, all valid for the call.
    unsafe {
        function(
            std::ptr::from_mut(&mut function_info).cast(),
            &raw mut flat_input,
            std::ptr::from_mut(result),
        );
    }
    if let Some(error) = function_info.error {
        return Err(Error::new_bytes(
            roastedduck_common::ErrorKind::InvalidInput,
            error,
        ));
    }
    if all_constant && (count == 1 || !volatile) {
        result.make_constant();
    }
    Ok(())
}

/// The definition behind a handle.
///
/// # Safety
/// `function` must be NULL or a live `duckdb_scalar_function`.
unsafe fn definition<'a>(function: DuckdbScalarFunction) -> Option<&'a mut CScalarFunction> {
    // SAFETY: guaranteed by the caller.
    unsafe { function.as_mut() }
}

/// `duckdb_scalar_function duckdb_create_scalar_function()` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_scalar_function() -> DuckdbScalarFunction {
    Box::into_raw(Box::new(CScalarFunction {
        name: String::new(),
        arguments: Vec::new(),
        varargs: None,
        return_type: LogicalType::INVALID,
        null_handling: NullHandling::Default,
        stability: Stability::Consistent,
        info: Arc::default(),
    }))
}

/// `void duckdb_destroy_scalar_function(duckdb_scalar_function *scalar_function)`
/// (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_scalar_function(scalar_function: *mut DuckdbScalarFunction) {
    // SAFETY: `scalar_function` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = scalar_function.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `void duckdb_scalar_function_set_name(duckdb_scalar_function scalar_function,
/// const char *name)` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_set_name(
    scalar_function: DuckdbScalarFunction,
    name: *const c_char,
) {
    // SAFETY: the handle is NULL or live; `name` is NULL or a C string.
    if let (Some(function), Some(name)) = unsafe { (definition(scalar_function), opt_str(name)) } {
        name.clone_into(&mut function.name);
    }
}

/// `void duckdb_scalar_function_set_varargs(duckdb_scalar_function scalar_function,
/// duckdb_logical_type type)` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_set_varargs(
    scalar_function: DuckdbScalarFunction,
    ty: DuckdbLogicalType,
) {
    // SAFETY: both handles are NULL or live.
    if let (Some(function), Some(ty)) = unsafe { (definition(scalar_function), type_ref(ty)) } {
        function.varargs = Some(ty.clone());
    }
}

/// `void duckdb_scalar_function_set_special_handling(duckdb_scalar_function
/// scalar_function)` (capi/scalar_function-c.cpp): the function sees NULL inputs instead
/// of the call folding to NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_set_special_handling(scalar_function: DuckdbScalarFunction) {
    // SAFETY: the handle is NULL or live.
    if let Some(function) = unsafe { definition(scalar_function) } {
        function.null_handling = NullHandling::Special;
    }
}

/// `void duckdb_scalar_function_set_volatile(duckdb_scalar_function scalar_function)`
/// (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_set_volatile(scalar_function: DuckdbScalarFunction) {
    // SAFETY: the handle is NULL or live.
    if let Some(function) = unsafe { definition(scalar_function) } {
        function.stability = Stability::Volatile;
    }
}

/// `void duckdb_scalar_function_add_parameter(duckdb_scalar_function scalar_function,
/// duckdb_logical_type type)` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_add_parameter(
    scalar_function: DuckdbScalarFunction,
    ty: DuckdbLogicalType,
) {
    // SAFETY: both handles are NULL or live.
    if let (Some(function), Some(ty)) = unsafe { (definition(scalar_function), type_ref(ty)) } {
        function.arguments.push(ty.clone());
    }
}

/// `void duckdb_scalar_function_set_return_type(duckdb_scalar_function scalar_function,
/// duckdb_logical_type type)` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_set_return_type(
    scalar_function: DuckdbScalarFunction,
    ty: DuckdbLogicalType,
) {
    // SAFETY: both handles are NULL or live.
    if let (Some(function), Some(ty)) = unsafe { (definition(scalar_function), type_ref(ty)) } {
        function.return_type = ty.clone();
    }
}

/// `void duckdb_scalar_function_set_extra_info(duckdb_scalar_function scalar_function,
/// void *extra_info, duckdb_delete_callback_t destroy)` (capi/scalar_function-c.cpp):
/// `destroy` releases the extra info once the function and its copies are gone; extra
/// info set before is not released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_set_extra_info(
    scalar_function: DuckdbScalarFunction,
    extra_info: *mut c_void,
    destroy: DuckdbDeleteCallback,
) {
    // SAFETY: the handle is NULL or live.
    if let Some(function) = unsafe { definition(scalar_function) }
        && !extra_info.is_null()
    {
        function.info.set_callbacks(|callbacks| {
            callbacks.extra_info = extra_info;
            callbacks.delete_callback = destroy;
        });
    }
}

/// `void duckdb_scalar_function_set_bind(duckdb_scalar_function scalar_function,
/// duckdb_scalar_function_bind_t bind)` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_set_bind(
    scalar_function: DuckdbScalarFunction,
    bind: DuckdbScalarFunctionBind,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(function), Some(bind)) = (unsafe { definition(scalar_function) }, bind) {
        function
            .info
            .set_callbacks(|callbacks| callbacks.bind = Some(bind));
    }
}

/// `void duckdb_scalar_function_set_init(duckdb_scalar_function scalar_function,
/// duckdb_scalar_function_init_t init)` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_set_init(
    scalar_function: DuckdbScalarFunction,
    init: DuckdbScalarFunctionInit,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(function), Some(init)) = (unsafe { definition(scalar_function) }, init) {
        function
            .info
            .set_callbacks(|callbacks| callbacks.init = Some(init));
    }
}

/// `void duckdb_scalar_function_set_function(duckdb_scalar_function scalar_function,
/// duckdb_scalar_function_t function)` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_set_function(
    scalar_function: DuckdbScalarFunction,
    function: DuckdbScalarFunctionT,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(definition), Some(function)) = (unsafe { definition(scalar_function) }, function) {
        definition
            .info
            .set_callbacks(|callbacks| callbacks.function = Some(function));
    }
}

/// `duckdb_state duckdb_register_scalar_function(duckdb_connection con,
/// duckdb_scalar_function scalar_function)` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_register_scalar_function(
    con: DuckdbConnection,
    scalar_function: DuckdbScalarFunction,
) -> DuckdbState {
    // SAFETY: the handle is NULL or live.
    let Some(function) = (unsafe { scalar_function.as_ref() }) else {
        return DuckdbState::DuckDBError;
    };
    // SAFETY: forwarded API contract.
    unsafe { register_scalar_functions(con, &function.name, std::slice::from_ref(function)) }
}

/// `duckdb_scalar_function_set duckdb_create_scalar_function_set(const char *name)`
/// (capi/scalar_function-c.cpp): NULL for a NULL or empty name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_scalar_function_set(name: *const c_char) -> DuckdbScalarFunctionSet {
    // SAFETY: `name` is NULL or a C string.
    match unsafe { opt_str(name) } {
        Some(name) if !name.is_empty() => Box::into_raw(Box::new(CScalarFunctionSet {
            name: name.to_owned(),
            functions: Vec::new(),
        })),
        _ => std::ptr::null_mut(),
    }
}

/// `void duckdb_destroy_scalar_function_set(duckdb_scalar_function_set *scalar_function_set)`
/// (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_scalar_function_set(
    scalar_function_set: *mut DuckdbScalarFunctionSet,
) {
    // SAFETY: `scalar_function_set` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = scalar_function_set.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `duckdb_state duckdb_add_scalar_function_to_set(duckdb_scalar_function_set set,
/// duckdb_scalar_function function)` (capi/scalar_function-c.cpp): the set gets a copy,
/// sharing the function's callbacks and extra info.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_add_scalar_function_to_set(
    set: DuckdbScalarFunctionSet,
    function: DuckdbScalarFunction,
) -> DuckdbState {
    // SAFETY: both handles are NULL or live.
    let (Some(set), Some(function)) = (unsafe { set.as_mut() }, unsafe { function.as_ref() }) else {
        return DuckdbState::DuckDBError;
    };
    set.functions.push(CScalarFunction {
        name: function.name.clone(),
        arguments: function.arguments.clone(),
        varargs: function.varargs.clone(),
        return_type: function.return_type.clone(),
        null_handling: function.null_handling,
        stability: function.stability,
        info: Arc::clone(&function.info),
    });
    DuckdbState::DuckDBSuccess
}

/// `duckdb_state duckdb_register_scalar_function_set(duckdb_connection con,
/// duckdb_scalar_function_set set)` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_register_scalar_function_set(
    con: DuckdbConnection,
    set: DuckdbScalarFunctionSet,
) -> DuckdbState {
    // SAFETY: the handle is NULL or live.
    let Some(set) = (unsafe { set.as_ref() }) else {
        return DuckdbState::DuckDBError;
    };
    // SAFETY: forwarded API contract.
    unsafe { register_scalar_functions(con, &set.name, &set.functions) }
}

/// Registers `functions` as the overloads of the scalar function `name`
/// (`duckdb_register_scalar_function_set`): every one needs a name and a function
/// callback, and resolved parameter and return types.
///
/// # Safety
/// `con` must be NULL or a live `duckdb_connection`.
unsafe fn register_scalar_functions(
    con: DuckdbConnection,
    name: &str,
    functions: &[CScalarFunction],
) -> DuckdbState {
    // SAFETY: `con` is NULL or a live handle.
    let Some(context) = (unsafe { connection_ref(con) }) else {
        return DuckdbState::DuckDBError;
    };
    if !functions.iter().all(CScalarFunction::is_registrable) {
        return DuckdbState::DuckDBError;
    }
    let mut set = ScalarFunctionSet::new(name);
    for function in functions {
        set.add(function.to_function());
    }
    DuckdbState::from_ok(catch(|| context.lock().create_scalar_function_set(set)).is_ok())
}

/// `void *duckdb_scalar_function_get_extra_info(duckdb_function_info info)`
/// (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_get_extra_info(info: DuckdbFunctionInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running function callback.
    unsafe { function_info(info) }.map_or(std::ptr::null_mut(), |info| info.extra_info)
}

/// `void *duckdb_scalar_function_bind_get_extra_info(duckdb_bind_info info)`
/// (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_bind_get_extra_info(info: DuckdbBindInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    unsafe { bind_info(info) }.map_or(std::ptr::null_mut(), |info| {
        info.bind_data.info.callbacks().extra_info
    })
}

/// `void *duckdb_scalar_function_get_bind_data(duckdb_function_info info)`
/// (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_get_bind_data(info: DuckdbFunctionInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running function callback.
    unsafe { function_info(info) }.map_or(std::ptr::null_mut(), |info| info.bind_data.bind_data.get())
}

/// `void duckdb_scalar_function_get_client_context(duckdb_bind_info info,
/// duckdb_client_context *out_context)` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_get_client_context(
    info: DuckdbBindInfo,
    out_context: *mut DuckdbClientContext,
) {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    let Some(info) = (unsafe { bind_info(info) }) else {
        return;
    };
    if !out_context.is_null() {
        // SAFETY: checked non-null; writable per the API contract.
        unsafe { *out_context = wrap_client_context(info.client) };
    }
}

/// `void duckdb_scalar_function_set_error(duckdb_function_info info, const char *error)`
/// (capi/scalar_function-c.cpp): the call fails with an invalid-input error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_set_error(info: DuckdbFunctionInfo, error: *const c_char) {
    // SAFETY: `info` is NULL or the info of a running function callback.
    if let Some(info) = unsafe { function_info(info) }
        && !error.is_null()
    {
        // SAFETY: `error` is a C string per the API contract.
        info.error = Some(unsafe { error_bytes(error) });
    }
}

/// `void duckdb_scalar_function_bind_set_error(duckdb_bind_info info, const char *error)`
/// (capi/scalar_function-c.cpp): binding fails with a binder error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_bind_set_error(info: DuckdbBindInfo, error: *const c_char) {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    if let Some(info) = unsafe { bind_info(info) }
        && !error.is_null()
    {
        // SAFETY: `error` is a C string per the API contract.
        info.error = Some(unsafe { error_bytes(error) });
    }
}

/// `idx_t duckdb_scalar_function_bind_get_argument_count(duckdb_bind_info info)`
/// (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_bind_get_argument_count(info: DuckdbBindInfo) -> Idx {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    unsafe { bind_info(info) }.map_or(0, |info| info.arguments.len() as Idx)
}

/// `duckdb_expression duckdb_scalar_function_bind_get_argument(duckdb_bind_info info,
/// idx_t index)` (capi/scalar_function-c.cpp): a copy of the bound argument expression.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_bind_get_argument(
    info: DuckdbBindInfo,
    index: Idx,
) -> DuckdbExpression {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    let Some(info) = (unsafe { bind_info(info) }) else {
        return std::ptr::null_mut();
    };
    let (Some(argument), Some(binder)) = (info.arguments.get(index as usize), info.binder) else {
        return std::ptr::null_mut();
    };
    wrap_expression(argument.clone(), Arc::clone(&binder.evaluator))
}

/// `void duckdb_scalar_function_set_bind_data(duckdb_bind_info info, void *bind_data,
/// duckdb_delete_callback_t destroy)` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_set_bind_data(
    info: DuckdbBindInfo,
    bind_data: *mut c_void,
    destroy: DuckdbDeleteCallback,
) {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    if let Some(info) = unsafe { bind_info(info) } {
        info.bind_data.bind_data = UserData::new(bind_data, destroy);
    }
}

/// `void duckdb_scalar_function_set_bind_data_copy(duckdb_bind_info info,
/// duckdb_copy_callback_t copy)` (capi/scalar_function-c.cpp). DuckDB copies bind data
/// with it when it copies a bound expression; the engine shares bound expressions' bind
/// data instead of copying it, so the callback is never needed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_set_bind_data_copy(
    _info: DuckdbBindInfo,
    _copy: DuckdbCopyCallback,
) {
}

/// `void *duckdb_scalar_function_get_state(duckdb_function_info info)`
/// (capi/scalar_function-c.cpp): the state the init callback set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_get_state(info: DuckdbFunctionInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running function callback.
    unsafe { function_info(info) }.map_or(std::ptr::null_mut(), |info| info.local_state.local_state.get())
}

/// `void duckdb_scalar_function_init_set_error(duckdb_init_info info, const char *error)`
/// (capi/scalar_function-c.cpp): the execution fails with an invalid-input error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_init_set_error(info: DuckdbInitInfo, error: *const c_char) {
    // SAFETY: `info` is NULL or the info of a running init callback.
    if let Some(info) = unsafe { init_info(info) }
        && !error.is_null()
    {
        // SAFETY: `error` is a C string per the API contract.
        info.error = Some(unsafe { error_bytes(error) });
    }
}

/// `void duckdb_scalar_function_init_set_state(duckdb_init_info info, void *state,
/// duckdb_delete_callback_t destroy)` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_init_set_state(
    info: DuckdbInitInfo,
    state: *mut c_void,
    destroy: DuckdbDeleteCallback,
) {
    // SAFETY: `info` is NULL or the info of a running init callback.
    if let Some(info) = unsafe { init_info(info) } {
        info.local_state.local_state = UserData::new(state, destroy);
    }
}

/// `void duckdb_scalar_function_init_get_client_context(duckdb_init_info info,
/// duckdb_client_context *out_context)` (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_init_get_client_context(
    info: DuckdbInitInfo,
    out_context: *mut DuckdbClientContext,
) {
    // SAFETY: `info` is NULL or the info of a running init callback.
    let Some(info) = (unsafe { init_info(info) }) else {
        return;
    };
    if !out_context.is_null() {
        // SAFETY: checked non-null; writable per the API contract.
        unsafe { *out_context = wrap_client_context(&info.bind_data.client) };
    }
}

/// `void *duckdb_scalar_function_init_get_bind_data(duckdb_init_info info)`
/// (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_init_get_bind_data(info: DuckdbInitInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running init callback.
    unsafe { init_info(info) }.map_or(std::ptr::null_mut(), |info| info.bind_data.bind_data.get())
}

/// `void *duckdb_scalar_function_init_get_extra_info(duckdb_init_info info)`
/// (capi/scalar_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_scalar_function_init_get_extra_info(info: DuckdbInitInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running init callback.
    unsafe { init_info(info) }.map_or(std::ptr::null_mut(), |info| {
        info.bind_data.info.callbacks().extra_info
    })
}
