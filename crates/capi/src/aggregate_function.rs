//! Aggregate functions defined by the client (DuckDB's `capi/aggregate_function-c.cpp`).
//!
//! A `duckdb_aggregate_function` is a definition the client fills in; registering it
//! copies it into an engine [`AggregateFunction`] whose [`AggregateOps`] call the client's
//! callbacks on the engine's raw state memory (`CAPIAggregateStateSize`,
//! `CAPIAggregateUpdate`, ...). The callbacks and the extra info live in a
//! [`CAggregateFunctionInfo`] shared by the definition and its copies.

use std::alloc::Layout;
use std::any::Any;
use std::ffi::{c_char, c_void};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use roastedduck_common::{Error, Result};
use roastedduck_function::aggregate::AggregateInput;
use roastedduck_function::{
    AggregateFunction, AggregateFunctionSet, AggregateOps, FunctionData, NullHandling, StatePtr,
};
use roastedduck_types::{DataChunk, LogicalType, Vector};

use crate::data_chunk::{DuckdbDataChunk, DuckdbVector};
use crate::function::{
    DuckdbDeleteCallback, DuckdbFunctionInfo, UserData, error_bytes, is_valid_parameter_type,
    is_valid_return_type,
};
use crate::helper::{catch, opt_str};
use crate::logical_types::{DuckdbLogicalType, type_ref};
use crate::open::{DuckdbConnection, connection_ref};
use crate::types::*;

/// `duckdb_aggregate_state`: a pointer to one state in memory the engine owns.
pub type DuckdbAggregateState = *mut c_void;
/// `duckdb_aggregate_state_size`
pub type DuckdbAggregateStateSize = Option<unsafe extern "C" fn(DuckdbFunctionInfo) -> Idx>;
/// `duckdb_aggregate_init_t`
pub type DuckdbAggregateInit = Option<unsafe extern "C" fn(DuckdbFunctionInfo, DuckdbAggregateState)>;
/// `duckdb_aggregate_update_t`
pub type DuckdbAggregateUpdate =
    Option<unsafe extern "C" fn(DuckdbFunctionInfo, DuckdbDataChunk, *mut DuckdbAggregateState)>;
/// `duckdb_aggregate_combine_t`
pub type DuckdbAggregateCombine = Option<
    unsafe extern "C" fn(DuckdbFunctionInfo, *mut DuckdbAggregateState, *mut DuckdbAggregateState, Idx),
>;
/// `duckdb_aggregate_finalize_t`
pub type DuckdbAggregateFinalize =
    Option<unsafe extern "C" fn(DuckdbFunctionInfo, *mut DuckdbAggregateState, DuckdbVector, Idx, Idx)>;
/// `duckdb_aggregate_destroy_t`
pub type DuckdbAggregateDestroy = Option<unsafe extern "C" fn(*mut DuckdbAggregateState, Idx)>;

/// The client's callbacks and extra info of an aggregate function.
#[derive(Clone, Copy, Debug)]
struct AggregateCallbacks {
    state_size: DuckdbAggregateStateSize,
    state_init: DuckdbAggregateInit,
    update: DuckdbAggregateUpdate,
    combine: DuckdbAggregateCombine,
    finalize: DuckdbAggregateFinalize,
    destroy: DuckdbAggregateDestroy,
    extra_info: *mut c_void,
    delete_callback: DuckdbDeleteCallback,
}

impl Default for AggregateCallbacks {
    fn default() -> Self {
        Self {
            state_size: None,
            state_init: None,
            update: None,
            combine: None,
            finalize: None,
            destroy: None,
            extra_info: std::ptr::null_mut(),
            delete_callback: None,
        }
    }
}

/// DuckDB's `CAggregateFunctionInfo`: the client's callbacks and extra info, shared by a
/// definition and its copies. The extra info is released with its delete callback when
/// the last of them is gone.
#[derive(Debug, Default)]
struct CAggregateFunctionInfo {
    callbacks: RwLock<AggregateCallbacks>,
}

// SAFETY: the extra info belongs to the client, which the C API contract makes
// responsible for its use from the threads that run the function's callbacks; the
// callbacks are plain C function pointers.
unsafe impl Send for CAggregateFunctionInfo {}
// SAFETY: see above; the fields are only changed under the lock.
unsafe impl Sync for CAggregateFunctionInfo {}

impl CAggregateFunctionInfo {
    fn callbacks(&self) -> AggregateCallbacks {
        *self.callbacks.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_callbacks(&self, update: impl FnOnce(&mut AggregateCallbacks)) {
        update(&mut self.callbacks.write().unwrap_or_else(PoisonError::into_inner));
    }
}

impl Drop for CAggregateFunctionInfo {
    fn drop(&mut self) {
        let callbacks = self.callbacks();
        drop(UserData::new(callbacks.extra_info, callbacks.delete_callback));
    }
}

/// What a `duckdb_function_info` of an aggregate function points to (DuckDB's
/// `CAggregateExecuteInfo`).
#[derive(Debug)]
struct CAggregateExecuteInfo {
    extra_info: *mut c_void,
    error: Option<Vec<u8>>,
}

/// The function info behind a handle.
///
/// # Safety
/// `info` must be NULL or the `duckdb_function_info` of an aggregate function's running
/// callback.
unsafe fn execute_info<'a>(info: DuckdbFunctionInfo) -> Option<&'a mut CAggregateExecuteInfo> {
    // SAFETY: guaranteed by the caller.
    unsafe { info.cast::<CAggregateExecuteInfo>().as_mut() }
}

/// The engine operations of a client aggregate (DuckDB's `CAPIAggregate*` callbacks).
/// DuckDB raises the errors the state size and init callbacks report; the engine asks
/// for them where it cannot fail, so they are raised by the next update, combine or
/// finalize.
#[derive(Debug)]
struct CAggregateOps {
    info: Arc<CAggregateFunctionInfo>,
    deferred_error: Mutex<Option<Vec<u8>>>,
}

impl CAggregateOps {
    /// Runs `call` with a fresh execute info; its error is an invalid-input error.
    fn execute(&self, call: impl FnOnce(DuckdbFunctionInfo)) -> Result<()> {
        let mut info = CAggregateExecuteInfo {
            extra_info: self.info.callbacks().extra_info,
            error: None,
        };
        call(std::ptr::from_mut(&mut info).cast());
        match info.error {
            Some(error) => Err(Error::new_bytes(
                roastedduck_common::ErrorKind::InvalidInput,
                error,
            )),
            None => Ok(()),
        }
    }

    /// Like [`CAggregateOps::execute`] for the callbacks that cannot fail here.
    fn execute_deferred(&self, call: impl FnOnce(DuckdbFunctionInfo)) {
        if let Err(error) = self.execute(call) {
            self.deferred_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_or_insert_with(|| error.raw_message_bytes().to_vec());
        }
    }

    /// Fails with an error the state size or init callback reported.
    fn check_deferred(&self) -> Result<()> {
        match self
            .deferred_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            Some(error) => Err(Error::new_bytes(
                roastedduck_common::ErrorKind::InvalidInput,
                error,
            )),
            None => Ok(()),
        }
    }
}

/// The state pointers of `states` as the C array of `duckdb_aggregate_state` the
/// callbacks read (a `StatePtr` is a plain address).
fn state_array(states: &[StatePtr]) -> *mut DuckdbAggregateState {
    states.as_ptr().cast_mut().cast()
}

impl AggregateOps for CAggregateOps {
    fn has_simple_update(&self) -> bool {
        // CAPIAggregateFunction installs shared scatter callbacks and no simple
        // update callback. Its Rust fallback does not change physical selection.
        false
    }

    fn verify_state_layout(&self) -> Result<()> {
        let _ = self.state_layout();
        self.check_deferred()
    }

    /// `CAPIAggregateStateSize`, aligned like DuckDB's states.
    fn state_layout(&self) -> Layout {
        let mut size = 0;
        if let Some(state_size) = self.info.callbacks().state_size {
            // SAFETY: the client's callback receives the execute info, valid for the call.
            self.execute_deferred(|info| size = unsafe { state_size(info) });
        }
        Layout::from_size_align(size as usize, 8).unwrap_or_else(|_| Layout::new::<u64>())
    }

    fn needs_destructor(&self) -> bool {
        self.info.callbacks().destroy.is_some()
    }

    /// `CAPIAggregateStateInit`
    unsafe fn initialize(&self, state: StatePtr) {
        if let Some(state_init) = self.info.callbacks().state_init {
            // SAFETY: `state` points to memory of the client's state size.
            self.execute_deferred(|info| unsafe { state_init(info, state.as_ptr().cast()) });
        }
    }

    /// `CAPIAggregateUpdate`: the flattened inputs as a chunk and one state per row.
    unsafe fn update(
        &self,
        inputs: &[Vector],
        _input: AggregateInput<'_>,
        states: &[StatePtr],
        count: usize,
    ) -> Result<()> {
        self.check_deferred()?;
        let Some(update) = self.info.callbacks().update else {
            return Err(Error::internal("C API aggregate without an update callback"));
        };
        let mut chunk = DataChunk::new(inputs.iter().map(|input| input.flattened(count)).collect(), count);
        // SAFETY: the client's callback receives the execute info, the chunk and `count`
        // valid states, all valid for the call.
        self.execute(|info| unsafe { update(info, &raw mut chunk, state_array(states)) })
    }

    /// Updates one state with every row (`simple_update` is DuckDB's update with all rows
    /// pointing to the same state).
    unsafe fn simple_update(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        state: StatePtr,
        count: usize,
    ) -> Result<()> {
        // SAFETY: forwarded contract; every row points to `state`.
        unsafe { self.update(inputs, input, &vec![state; count], count) }
    }

    /// `CAPIAggregateCombine`
    unsafe fn combine(
        &self,
        source: &[StatePtr],
        target: &[StatePtr],
        _input: AggregateInput<'_>,
    ) -> Result<()> {
        self.check_deferred()?;
        let Some(combine) = self.info.callbacks().combine else {
            return Err(Error::internal("C API aggregate without a combine callback"));
        };
        // SAFETY: the client's callback receives the execute info and two arrays of
        // `source.len()` valid states.
        self.execute(|info| unsafe {
            combine(
                info,
                state_array(source),
                state_array(target),
                source.len() as Idx,
            );
        })
    }

    /// `CAPIAggregateFinalize`: the results go to rows `offset..` of `result`.
    unsafe fn finalize(
        &self,
        states: &[StatePtr],
        _input: AggregateInput<'_>,
        result: &mut Vector,
        offset: usize,
    ) -> Result<()> {
        self.check_deferred()?;
        let Some(finalize) = self.info.callbacks().finalize else {
            return Err(Error::internal("C API aggregate without a finalize callback"));
        };
        // SAFETY: the client's callback receives the execute info, `states.len()` valid
        // states and the result vector, all valid for the call.
        self.execute(|info| unsafe {
            finalize(
                info,
                state_array(states),
                std::ptr::from_mut(result),
                states.len() as Idx,
                offset as Idx,
            );
        })
    }

    /// `CAPIAggregateDestructor`
    unsafe fn destroy(&self, states: &[StatePtr]) {
        if let Some(destroy) = self.info.callbacks().destroy {
            // SAFETY: the client's callback receives `states.len()` valid states.
            unsafe { destroy(state_array(states), states.len() as Idx) };
        }
    }
}

/// The bind data of a call (DuckDB's `CAggregateFunctionBindData`).
#[derive(Debug)]
struct CAggregateFunctionBindData {
    info: Arc<CAggregateFunctionInfo>,
}

impl FunctionData for CAggregateFunctionBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    /// `CAggregateFunctionBindData::Equals`: the same extra info and callbacks.
    fn equals(&self, other: &dyn FunctionData) -> bool {
        other.as_any().downcast_ref::<Self>().is_some_and(|other| {
            let (mine, theirs) = (self.info.callbacks(), other.info.callbacks());
            mine.extra_info == theirs.extra_info
                && mine.update.map(|f| f as usize) == theirs.update.map(|f| f as usize)
                && mine.combine.map(|f| f as usize) == theirs.combine.map(|f| f as usize)
                && mine.finalize.map(|f| f as usize) == theirs.finalize.map(|f| f as usize)
        })
    }
}

/// What a `duckdb_aggregate_function` points to: the function's signature and properties
/// (DuckDB's `AggregateFunction`) and its shared callbacks.
#[derive(Clone, Debug)]
pub struct CAggregateFunction {
    name: String,
    arguments: Vec<LogicalType>,
    return_type: LogicalType,
    null_handling: NullHandling,
    info: Arc<CAggregateFunctionInfo>,
}

/// `duckdb_aggregate_function`
pub type DuckdbAggregateFunction = *mut CAggregateFunction;

impl CAggregateFunction {
    /// The engine function calling the client's callbacks.
    fn to_function(&self) -> AggregateFunction {
        let ops = CAggregateOps {
            info: Arc::clone(&self.info),
            deferred_error: Mutex::default(),
        };
        let info = Arc::clone(&self.info);
        AggregateFunction::new(
            self.name.as_str(),
            self.arguments.clone(),
            self.return_type.clone(),
            ops,
        )
        .with_null_handling(self.null_handling)
        // `CAPIAggregateBind`.
        .with_bind(move |_, _| {
            Ok(Some(Arc::new(CAggregateFunctionBindData {
                info: Arc::clone(&info),
            }) as Arc<dyn FunctionData>))
        })
    }

    /// Whether `duckdb_register_aggregate_function_set` accepts the function.
    fn is_registrable(&self) -> bool {
        let callbacks = self.info.callbacks();
        !self.name.is_empty()
            && callbacks.update.is_some()
            && callbacks.combine.is_some()
            && callbacks.finalize.is_some()
            && is_valid_return_type(&self.return_type)
            && self.arguments.iter().all(is_valid_parameter_type)
    }
}

/// What a `duckdb_aggregate_function_set` points to (DuckDB's `AggregateFunctionSet`):
/// copies of the functions added to it.
#[derive(Debug)]
pub struct CAggregateFunctionSet {
    name: String,
    functions: Vec<CAggregateFunction>,
}

/// `duckdb_aggregate_function_set`
pub type DuckdbAggregateFunctionSet = *mut CAggregateFunctionSet;

/// The definition behind a handle.
///
/// # Safety
/// `function` must be NULL or a live `duckdb_aggregate_function`.
unsafe fn definition<'a>(function: DuckdbAggregateFunction) -> Option<&'a mut CAggregateFunction> {
    // SAFETY: guaranteed by the caller.
    unsafe { function.as_mut() }
}

/// `duckdb_aggregate_function duckdb_create_aggregate_function()`
/// (capi/aggregate_function-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_aggregate_function() -> DuckdbAggregateFunction {
    Box::into_raw(Box::new(CAggregateFunction {
        name: String::new(),
        arguments: Vec::new(),
        return_type: LogicalType::INVALID,
        null_handling: NullHandling::Default,
        info: Arc::default(),
    }))
}

/// `void duckdb_destroy_aggregate_function(duckdb_aggregate_function *aggregate_function)`
/// (capi/aggregate_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_aggregate_function(aggregate_function: *mut DuckdbAggregateFunction) {
    // SAFETY: `aggregate_function` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = aggregate_function.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `void duckdb_aggregate_function_set_name(duckdb_aggregate_function aggregate_function,
/// const char *name)` (capi/aggregate_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_aggregate_function_set_name(
    aggregate_function: DuckdbAggregateFunction,
    name: *const c_char,
) {
    // SAFETY: the handle is NULL or live; `name` is NULL or a C string.
    if let (Some(function), Some(name)) = unsafe { (definition(aggregate_function), opt_str(name)) } {
        name.clone_into(&mut function.name);
    }
}

/// `void duckdb_aggregate_function_add_parameter(duckdb_aggregate_function
/// aggregate_function, duckdb_logical_type type)` (capi/aggregate_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_aggregate_function_add_parameter(
    aggregate_function: DuckdbAggregateFunction,
    ty: DuckdbLogicalType,
) {
    // SAFETY: both handles are NULL or live.
    if let (Some(function), Some(ty)) = unsafe { (definition(aggregate_function), type_ref(ty)) } {
        function.arguments.push(ty.clone());
    }
}

/// `void duckdb_aggregate_function_set_return_type(duckdb_aggregate_function
/// aggregate_function, duckdb_logical_type type)` (capi/aggregate_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_aggregate_function_set_return_type(
    aggregate_function: DuckdbAggregateFunction,
    ty: DuckdbLogicalType,
) {
    // SAFETY: both handles are NULL or live.
    if let (Some(function), Some(ty)) = unsafe { (definition(aggregate_function), type_ref(ty)) } {
        function.return_type = ty.clone();
    }
}

/// `void duckdb_aggregate_function_set_functions(duckdb_aggregate_function
/// aggregate_function, duckdb_aggregate_state_size state_size, duckdb_aggregate_init_t
/// state_init, duckdb_aggregate_update_t update, duckdb_aggregate_combine_t combine,
/// duckdb_aggregate_finalize_t finalize)` (capi/aggregate_function-c.cpp): ignored unless
/// every callback is given.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_aggregate_function_set_functions(
    aggregate_function: DuckdbAggregateFunction,
    state_size: DuckdbAggregateStateSize,
    state_init: DuckdbAggregateInit,
    update: DuckdbAggregateUpdate,
    combine: DuckdbAggregateCombine,
    finalize: DuckdbAggregateFinalize,
) {
    // SAFETY: the handle is NULL or live.
    let Some(function) = (unsafe { definition(aggregate_function) }) else {
        return;
    };
    if state_size.is_none()
        || state_init.is_none()
        || update.is_none()
        || combine.is_none()
        || finalize.is_none()
    {
        return;
    }
    function.info.set_callbacks(|callbacks| {
        callbacks.state_size = state_size;
        callbacks.state_init = state_init;
        callbacks.update = update;
        callbacks.combine = combine;
        callbacks.finalize = finalize;
    });
}

/// `void duckdb_aggregate_function_set_destructor(duckdb_aggregate_function
/// aggregate_function, duckdb_aggregate_destroy_t destroy)` (capi/aggregate_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_aggregate_function_set_destructor(
    aggregate_function: DuckdbAggregateFunction,
    destroy: DuckdbAggregateDestroy,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(function), Some(destroy)) = (unsafe { definition(aggregate_function) }, destroy) {
        function
            .info
            .set_callbacks(|callbacks| callbacks.destroy = Some(destroy));
    }
}

/// `duckdb_state duckdb_register_aggregate_function(duckdb_connection con,
/// duckdb_aggregate_function aggregate_function)` (capi/aggregate_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_register_aggregate_function(
    con: DuckdbConnection,
    aggregate_function: DuckdbAggregateFunction,
) -> DuckdbState {
    // SAFETY: the handle is NULL or live.
    let Some(function) = (unsafe { aggregate_function.as_ref() }) else {
        return DuckdbState::DuckDBError;
    };
    // SAFETY: forwarded API contract.
    unsafe { register_aggregate_functions(con, &function.name, std::slice::from_ref(function)) }
}

/// `void duckdb_aggregate_function_set_special_handling(duckdb_aggregate_function
/// aggregate_function)` (capi/aggregate_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_aggregate_function_set_special_handling(
    aggregate_function: DuckdbAggregateFunction,
) {
    // SAFETY: the handle is NULL or live.
    if let Some(function) = unsafe { definition(aggregate_function) } {
        function.null_handling = NullHandling::Special;
    }
}

/// `void duckdb_aggregate_function_set_extra_info(duckdb_aggregate_function
/// aggregate_function, void *extra_info, duckdb_delete_callback_t destroy)`
/// (capi/aggregate_function-c.cpp): `destroy` releases the extra info once the function
/// and its copies are gone; extra info set before is not released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_aggregate_function_set_extra_info(
    aggregate_function: DuckdbAggregateFunction,
    extra_info: *mut c_void,
    destroy: DuckdbDeleteCallback,
) {
    // SAFETY: the handle is NULL or live.
    if let Some(function) = unsafe { definition(aggregate_function) }
        && !extra_info.is_null()
    {
        function.info.set_callbacks(|callbacks| {
            callbacks.extra_info = extra_info;
            callbacks.delete_callback = destroy;
        });
    }
}

/// `void *duckdb_aggregate_function_get_extra_info(duckdb_function_info info)`
/// (capi/aggregate_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_aggregate_function_get_extra_info(info: DuckdbFunctionInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running aggregate callback.
    unsafe { execute_info(info) }.map_or(std::ptr::null_mut(), |info| info.extra_info)
}

/// `void duckdb_aggregate_function_set_error(duckdb_function_info info, const char *error)`
/// (capi/aggregate_function-c.cpp): the query fails with an invalid-input error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_aggregate_function_set_error(info: DuckdbFunctionInfo, error: *const c_char) {
    // SAFETY: `info` is NULL or the info of a running aggregate callback.
    if let Some(info) = unsafe { execute_info(info) }
        && !error.is_null()
    {
        // SAFETY: `error` is a C string per the API contract.
        info.error = Some(unsafe { error_bytes(error) });
    }
}

/// `duckdb_aggregate_function_set duckdb_create_aggregate_function_set(const char *name)`
/// (capi/aggregate_function-c.cpp): NULL for a NULL or empty name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_aggregate_function_set(
    name: *const c_char,
) -> DuckdbAggregateFunctionSet {
    // SAFETY: `name` is NULL or a C string.
    match unsafe { opt_str(name) } {
        Some(name) if !name.is_empty() => Box::into_raw(Box::new(CAggregateFunctionSet {
            name: name.to_owned(),
            functions: Vec::new(),
        })),
        _ => std::ptr::null_mut(),
    }
}

/// `void duckdb_destroy_aggregate_function_set(duckdb_aggregate_function_set
/// *aggregate_function_set)` (capi/aggregate_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_aggregate_function_set(
    aggregate_function_set: *mut DuckdbAggregateFunctionSet,
) {
    // SAFETY: `aggregate_function_set` is NULL or points to NULL or a handle owned by the
    // caller.
    unsafe {
        let Some(slot) = aggregate_function_set.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `duckdb_state duckdb_add_aggregate_function_to_set(duckdb_aggregate_function_set set,
/// duckdb_aggregate_function function)` (capi/aggregate_function-c.cpp): the set gets a
/// copy, sharing the function's callbacks and extra info.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_add_aggregate_function_to_set(
    set: DuckdbAggregateFunctionSet,
    function: DuckdbAggregateFunction,
) -> DuckdbState {
    // SAFETY: both handles are NULL or live.
    let (Some(set), Some(function)) = (unsafe { set.as_mut() }, unsafe { function.as_ref() }) else {
        return DuckdbState::DuckDBError;
    };
    set.functions.push(function.clone());
    DuckdbState::DuckDBSuccess
}

/// `duckdb_state duckdb_register_aggregate_function_set(duckdb_connection con,
/// duckdb_aggregate_function_set set)` (capi/aggregate_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_register_aggregate_function_set(
    con: DuckdbConnection,
    set: DuckdbAggregateFunctionSet,
) -> DuckdbState {
    // SAFETY: the handle is NULL or live.
    let Some(set) = (unsafe { set.as_ref() }) else {
        return DuckdbState::DuckDBError;
    };
    // SAFETY: forwarded API contract.
    unsafe { register_aggregate_functions(con, &set.name, &set.functions) }
}

/// Registers `functions` as the overloads of the aggregate `name`
/// (`duckdb_register_aggregate_function_set`): every one needs a name, the update,
/// combine and finalize callbacks, and resolved parameter and return types.
///
/// # Safety
/// `con` must be NULL or a live `duckdb_connection`.
unsafe fn register_aggregate_functions(
    con: DuckdbConnection,
    name: &str,
    functions: &[CAggregateFunction],
) -> DuckdbState {
    // SAFETY: `con` is NULL or a live handle.
    let Some(context) = (unsafe { connection_ref(con) }) else {
        return DuckdbState::DuckDBError;
    };
    if !functions.iter().all(CAggregateFunction::is_registrable) {
        return DuckdbState::DuckDBError;
    }
    let mut set = AggregateFunctionSet::new(name);
    for function in functions {
        set.add(function.to_function());
    }
    DuckdbState::from_ok(catch(|| context.lock().create_aggregate_function_set(set)).is_ok())
}
