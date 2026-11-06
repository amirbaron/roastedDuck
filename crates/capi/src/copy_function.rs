//! COPY formats defined by the client (DuckDB's `capi/copy_function-c.cpp`).
//!
//! `COPY ... TO` calls the client's bind, global init, sink and finalize callbacks;
//! `COPY ... FROM` binds the client's table function with the target table's columns,
//! the statement's options as named parameters and the file path as its parameter.

use std::any::Any;
use std::ffi::{CString, c_char, c_void};
use std::sync::{Arc, PoisonError, RwLock};

use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_function::copy::{CopyFromBindInput, CopyFunctionBindInput, CopyGlobalState};
use rustdb_function::{CopyFunction, FunctionClient, FunctionData};
use rustdb_planner::ClientContext as BinderContext;
use rustdb_types::{DataChunk, LogicalType, Value};

use crate::data_chunk::DuckdbDataChunk;
use crate::function::{DuckdbDeleteCallback, UserData, error_bytes};
use crate::helper::{catch, opt_str, to_cstring};
use crate::logical_types::{DuckdbLogicalType, wrap_type};
use crate::open::{DuckdbClientContext, DuckdbConnection, connection_ref, wrap_client_context};
use crate::table_function::{self, CTableFunction, DuckdbTableFunction, c_table_bind};
use crate::types::*;
use crate::value::{DuckdbValue, wrap_value};

/// `duckdb_copy_function_bind_info`
pub type DuckdbCopyFunctionBindInfo = *mut c_void;
/// `duckdb_copy_function_global_init_info`
pub type DuckdbCopyFunctionGlobalInitInfo = *mut c_void;
/// `duckdb_copy_function_sink_info`
pub type DuckdbCopyFunctionSinkInfo = *mut c_void;
/// `duckdb_copy_function_finalize_info`
pub type DuckdbCopyFunctionFinalizeInfo = *mut c_void;

/// `duckdb_copy_function_bind_t`
pub type DuckdbCopyFunctionBind = Option<unsafe extern "C" fn(DuckdbCopyFunctionBindInfo)>;
/// `duckdb_copy_function_global_init_t`
pub type DuckdbCopyFunctionGlobalInit = Option<unsafe extern "C" fn(DuckdbCopyFunctionGlobalInitInfo)>;
/// `duckdb_copy_function_sink_t`
pub type DuckdbCopyFunctionSink = Option<unsafe extern "C" fn(DuckdbCopyFunctionSinkInfo, DuckdbDataChunk)>;
/// `duckdb_copy_function_finalize_t`
pub type DuckdbCopyFunctionFinalize = Option<unsafe extern "C" fn(DuckdbCopyFunctionFinalizeInfo)>;

/// The client's callbacks and extra info of a COPY format.
#[derive(Clone, Copy, Debug)]
struct CopyCallbacks {
    bind_to: DuckdbCopyFunctionBind,
    global_init: DuckdbCopyFunctionGlobalInit,
    sink: DuckdbCopyFunctionSink,
    finalize: DuckdbCopyFunctionFinalize,
    extra_info: *mut c_void,
    delete_callback: DuckdbDeleteCallback,
}

impl Default for CopyCallbacks {
    fn default() -> Self {
        Self {
            bind_to: None,
            global_init: None,
            sink: None,
            finalize: None,
            extra_info: std::ptr::null_mut(),
            delete_callback: None,
        }
    }
}

/// DuckDB's `CCopyFunctionInfo`: the client's callbacks and extra info, shared by the
/// definition and the registered format. The extra info is released with its delete
/// callback when both are gone.
#[derive(Debug, Default)]
struct CCopyFunctionInfo {
    callbacks: RwLock<CopyCallbacks>,
}

// SAFETY: the extra info belongs to the client, which the C API contract makes
// responsible for its use from the threads that run the format's callbacks; the
// callbacks are plain C function pointers.
unsafe impl Send for CCopyFunctionInfo {}
// SAFETY: see above; the fields are only changed under the lock.
unsafe impl Sync for CCopyFunctionInfo {}

impl CCopyFunctionInfo {
    fn callbacks(&self) -> CopyCallbacks {
        *self.callbacks.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_callbacks(&self, update: impl FnOnce(&mut CopyCallbacks)) {
        update(&mut self.callbacks.write().unwrap_or_else(PoisonError::into_inner));
    }
}

impl Drop for CCopyFunctionInfo {
    fn drop(&mut self) {
        let callbacks = self.callbacks();
        drop(UserData::new(callbacks.extra_info, callbacks.delete_callback));
    }
}

/// What a `duckdb_copy_function` points to (DuckDB's `CopyFunction` with its
/// `CCopyFunctionInfo`).
#[derive(Debug)]
pub struct CCopyFunction {
    name: String,
    info: Arc<CCopyFunctionInfo>,
    /// The reader of `COPY ... FROM` (`copy_from_function`), if it was set.
    copy_from: Option<CTableFunction>,
}

/// `duckdb_copy_function`
pub type DuckdbCopyFunction = *mut CCopyFunction;

/// The bind data of a write (DuckDB's `CCopyToBindInfo`), with the connection that bound
/// it, whose context the later callbacks see.
#[derive(Debug)]
struct CCopyToBindData {
    info: Arc<CCopyFunctionInfo>,
    bind_data: UserData,
    client: Option<Arc<FunctionClient>>,
}

impl FunctionData for CCopyToBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The state of a written file (DuckDB's `CCopyToGlobalState`).
#[derive(Debug)]
struct CCopyToGlobalState {
    global_state: UserData,
}

/// What a `duckdb_copy_function_bind_info` points to (DuckDB's
/// `CCopyFunctionToInternalBindInfo`).
struct CCopyToInternalBindInfo<'a> {
    client: Option<&'a Arc<FunctionClient>>,
    options: &'a CaseInsensitiveMap<Vec<Value>, rustdb_common::ByteString>,
    types: &'a [LogicalType],
    extra_info: *mut c_void,
    bind_data: UserData,
    error: Option<Vec<u8>>,
}

/// What a `duckdb_copy_function_global_init_info` points to (DuckDB's
/// `CCopyToGlobalInitInfo`).
struct CCopyToGlobalInitInfo<'a> {
    bind_data: &'a CCopyToBindData,
    file_path: CString,
    global_state: UserData,
    error: Option<Vec<u8>>,
}

/// What the infos of the sink and finalize callbacks point to (DuckDB's
/// `CCopyToSinkInfo` and `CCopyToFinalizeInfo`).
struct CCopyToExecuteInfo<'a> {
    bind_data: &'a CCopyToBindData,
    global_state: &'a CCopyToGlobalState,
    error: Option<Vec<u8>>,
}

/// The C API bind data of a write.
fn copy_bind_data(bind_data: &dyn FunctionData) -> Result<&CCopyToBindData> {
    bind_data
        .as_any()
        .downcast_ref::<CCopyToBindData>()
        .ok_or_else(|| Error::internal("C API COPY function without its bind data"))
}

/// The C API state of a written file.
fn copy_global_state(state: &CopyGlobalState) -> Result<&CCopyToGlobalState> {
    state
        .downcast_ref::<CCopyToGlobalState>()
        .ok_or_else(|| Error::internal("C API COPY function without its global state"))
}

/// The connection behind a binder context.
fn binder_client(binder: Option<&dyn Any>) -> Option<&Arc<FunctionClient>> {
    binder
        .and_then(|binder| binder.downcast_ref::<BinderContext>())
        .map(|binder| &binder.function_client)
}

/// `MakeValueFromCopyOptions`: the options as a STRUCT with upper-case names, NULL without
/// options. An option without values is NULL, one value is itself, values of one type a
/// LIST, others an unnamed STRUCT.
fn options_value(options: &CaseInsensitiveMap<Vec<Value>, rustdb_common::ByteString>) -> Value {
    if options.is_empty() {
        return Value::default();
    }
    let fields = options
        .iter()
        .map(|(name, values)| {
            let value = match values.as_slice() {
                [] => Value::default(),
                [value] => value.clone(),
                [first, rest @ ..]
                    if rest
                        .iter()
                        .all(|value| value.logical_type() == first.logical_type()) =>
                {
                    Value::list(first.logical_type().clone(), values.clone())
                }
                _ => Value::struct_value(
                    values
                        .iter()
                        .map(|value| (rustdb_common::ByteString::new(), value.clone()))
                        .collect(),
                ),
            };
            (name.to_ascii_uppercase().into(), value)
        })
        .collect();
    Value::struct_value(fields)
}

/// `CCopyToBind`: runs the client's bind callback, whose error is a binder error.
fn c_copy_to_bind(
    info: &Arc<CCopyFunctionInfo>,
    input: &CopyFunctionBindInput<'_>,
    types: &[LogicalType],
) -> Result<Arc<dyn FunctionData>> {
    let client = binder_client(input.binder);
    let callbacks = info.callbacks();
    let mut bind_data = UserData::none();
    if let Some(bind_to) = callbacks.bind_to {
        let mut bind_info = CCopyToInternalBindInfo {
            client,
            options: input.options,
            types,
            extra_info: callbacks.extra_info,
            bind_data: UserData::none(),
            error: None,
        };
        // SAFETY: the client's callback receives the bind info, valid for the call.
        unsafe { bind_to(std::ptr::from_mut(&mut bind_info).cast()) };
        bind_data = bind_info.bind_data;
        if let Some(error) = bind_info.error {
            return Err(Error::new_bytes(rustdb_common::ErrorKind::Binder, error));
        }
    }
    Ok(Arc::new(CCopyToBindData {
        info: Arc::clone(info),
        bind_data,
        client: client.cloned(),
    }))
}

/// `CCopyToGlobalInit`: runs the client's global init callback for the file at `path`,
/// whose error is an invalid-input error.
fn c_copy_to_global_init(bind_data: &dyn FunctionData, path: &[u8]) -> Result<CopyGlobalState> {
    let bind_data = copy_bind_data(bind_data)?;
    let mut global_state = UserData::none();
    if let Some(global_init) = bind_data.info.callbacks().global_init {
        let mut init_info = CCopyToGlobalInitInfo {
            bind_data,
            file_path: to_cstring(path),
            global_state: UserData::none(),
            error: None,
        };
        // SAFETY: the client's callback receives the init info, valid for the call.
        unsafe { global_init(std::ptr::from_mut(&mut init_info).cast()) };
        global_state = init_info.global_state;
        if let Some(error) = init_info.error {
            return Err(Error::new_bytes(rustdb_common::ErrorKind::InvalidInput, error));
        }
    }
    Ok(Box::new(CCopyToGlobalState { global_state }))
}

/// `CCopyToSink`: hands the flattened chunk to the client's sink callback, whose error is
/// an invalid-input error.
fn c_copy_to_sink(
    bind_data: &dyn FunctionData,
    global_state: &CopyGlobalState,
    input: &DataChunk,
) -> Result<()> {
    let bind_data = copy_bind_data(bind_data)?;
    let global_state = copy_global_state(global_state)?;
    let Some(sink) = bind_data.info.callbacks().sink else {
        return Err(Error::internal("C API COPY function without a sink callback"));
    };
    let count = input.len();
    let mut input = input.clone();
    for column in 0..input.column_count() {
        input.column_mut(column).flatten(count);
    }
    let mut sink_info = CCopyToExecuteInfo {
        bind_data,
        global_state,
        error: None,
    };
    // SAFETY: the client's callback receives the sink info and the chunk, valid for the
    // call.
    unsafe { sink(std::ptr::from_mut(&mut sink_info).cast(), &raw mut input) };
    match sink_info.error {
        Some(error) => Err(Error::new_bytes(rustdb_common::ErrorKind::InvalidInput, error)),
        None => Ok(()),
    }
}

/// `CCopyToFinalize`: runs the client's finalize callback, if any, whose error is an
/// invalid-input error.
fn c_copy_to_finalize(bind_data: &dyn FunctionData, global_state: &CopyGlobalState) -> Result<()> {
    let bind_data = copy_bind_data(bind_data)?;
    let global_state = copy_global_state(global_state)?;
    let Some(finalize) = bind_data.info.callbacks().finalize else {
        return Ok(());
    };
    let mut finalize_info = CCopyToExecuteInfo {
        bind_data,
        global_state,
        error: None,
    };
    // SAFETY: the client's callback receives the finalize info, valid for the call.
    unsafe { finalize(std::ptr::from_mut(&mut finalize_info).cast()) };
    match finalize_info.error {
        Some(error) => Err(Error::new_bytes(rustdb_common::ErrorKind::InvalidInput, error)),
        None => Ok(()),
    }
}

/// `CCopyFromBind`: binds the reader with the options as named parameters (one value as
/// itself, values of one type as a LIST) and the file path as its parameter.
fn c_copy_from_bind(
    function: &CTableFunction,
    input: &CopyFromBindInput<'_>,
    names: &[rustdb_common::ByteString],
    types: &[LogicalType],
) -> Result<Arc<dyn FunctionData>> {
    let mut named_parameters = CaseInsensitiveMap::default();
    for (name, values) in input.options.iter() {
        if function.named_parameter(name).is_none() {
            return Err(Error::binder(rustdb_common::byte_format!(
                "'{name}' is not a supported option for copy function '{}'",
                function.name
            )));
        }
        let value = match values.as_slice() {
            [] => continue,
            [value] => value.clone(),
            [first, rest @ ..] => {
                if rest
                    .iter()
                    .any(|value| value.logical_type() != first.logical_type())
                {
                    return Err(Error::binder(rustdb_common::byte_format!(
                        "Cannot pass multiple values of different types for copy option '{name}'"
                    )));
                }
                Value::list(first.logical_type().clone(), values.clone())
            }
        };
        named_parameters.insert(name, value);
    }
    let parameters = [Value::try_varchar(input.file_path)?];
    let (bind_data, _) = c_table_bind(
        &function.info,
        binder_client(input.binder),
        &parameters,
        &named_parameters,
        types.to_vec(),
        names,
    )?;
    Ok(Arc::new(bind_data))
}

/// The definition behind a handle.
///
/// # Safety
/// `copy_function` must be NULL or a live `duckdb_copy_function`.
unsafe fn definition<'a>(copy_function: DuckdbCopyFunction) -> Option<&'a mut CCopyFunction> {
    // SAFETY: guaranteed by the caller.
    unsafe { copy_function.as_mut() }
}

/// `duckdb_copy_function duckdb_create_copy_function()` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_copy_function() -> DuckdbCopyFunction {
    Box::into_raw(Box::new(CCopyFunction {
        name: String::new(),
        info: Arc::default(),
        copy_from: None,
    }))
}

/// `void duckdb_copy_function_set_name(duckdb_copy_function copy_function, const char *name)`
/// (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_set_name(
    copy_function: DuckdbCopyFunction,
    name: *const c_char,
) {
    // SAFETY: the handle is NULL or live; `name` is NULL or a C string.
    if let (Some(function), Some(name)) = unsafe { (definition(copy_function), opt_str(name)) } {
        name.clone_into(&mut function.name);
    }
}

/// `void duckdb_destroy_copy_function(duckdb_copy_function *copy_function)`
/// (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_copy_function(copy_function: *mut DuckdbCopyFunction) {
    // SAFETY: `copy_function` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = copy_function.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `void duckdb_copy_function_set_extra_info(duckdb_copy_function copy_function,
/// void *extra_info, duckdb_delete_callback_t destroy)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_set_extra_info(
    copy_function: DuckdbCopyFunction,
    extra_info: *mut c_void,
    destroy: DuckdbDeleteCallback,
) {
    // SAFETY: the handle is NULL or live.
    if let Some(function) = unsafe { definition(copy_function) } {
        function.info.set_callbacks(|callbacks| {
            callbacks.extra_info = extra_info;
            callbacks.delete_callback = destroy;
        });
    }
}

/// `void duckdb_copy_function_set_bind(duckdb_copy_function copy_function,
/// duckdb_copy_function_bind_t bind)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_set_bind(
    copy_function: DuckdbCopyFunction,
    bind: DuckdbCopyFunctionBind,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(function), Some(bind)) = (unsafe { definition(copy_function) }, bind) {
        function
            .info
            .set_callbacks(|callbacks| callbacks.bind_to = Some(bind));
    }
}

/// `void duckdb_copy_function_bind_set_error(duckdb_copy_function_bind_info info,
/// const char *error)` (capi/copy_function-c.cpp): binding fails with a binder error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_bind_set_error(
    info: DuckdbCopyFunctionBindInfo,
    error: *const c_char,
) {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    if let Some(info) = unsafe { info.cast::<CCopyToInternalBindInfo<'_>>().as_mut() }
        && !error.is_null()
    {
        // SAFETY: `error` is a C string per the API contract.
        info.error = Some(unsafe { error_bytes(error) });
    }
}

/// `void *duckdb_copy_function_bind_get_extra_info(duckdb_copy_function_bind_info info)`
/// (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_bind_get_extra_info(
    info: DuckdbCopyFunctionBindInfo,
) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    unsafe { info.cast::<CCopyToInternalBindInfo<'_>>().as_ref() }
        .map_or(std::ptr::null_mut(), |info| info.extra_info)
}

/// `duckdb_client_context duckdb_copy_function_bind_get_client_context(
/// duckdb_copy_function_bind_info info)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_bind_get_client_context(
    info: DuckdbCopyFunctionBindInfo,
) -> DuckdbClientContext {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    unsafe { info.cast::<CCopyToInternalBindInfo<'_>>().as_ref() }
        .and_then(|info| info.client)
        .map_or(std::ptr::null_mut(), wrap_client_context)
}

/// `idx_t duckdb_copy_function_bind_get_column_count(duckdb_copy_function_bind_info info)`
/// (capi/copy_function-c.cpp): the columns written.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_bind_get_column_count(info: DuckdbCopyFunctionBindInfo) -> Idx {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    unsafe { info.cast::<CCopyToInternalBindInfo<'_>>().as_ref() }.map_or(0, |info| info.types.len() as Idx)
}

/// `duckdb_logical_type duckdb_copy_function_bind_get_column_type(
/// duckdb_copy_function_bind_info info, idx_t col_idx)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_bind_get_column_type(
    info: DuckdbCopyFunctionBindInfo,
    col_idx: Idx,
) -> DuckdbLogicalType {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    unsafe { info.cast::<CCopyToInternalBindInfo<'_>>().as_ref() }
        .and_then(|info| info.types.get(col_idx as usize))
        .map_or(std::ptr::null_mut(), |ty| wrap_type(ty.clone()))
}

/// `duckdb_value duckdb_copy_function_bind_get_options(duckdb_copy_function_bind_info info)`
/// (capi/copy_function-c.cpp): the options as a STRUCT (`MakeValueFromCopyOptions`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_bind_get_options(
    info: DuckdbCopyFunctionBindInfo,
) -> DuckdbValue {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    unsafe { info.cast::<CCopyToInternalBindInfo<'_>>().as_ref() }.map_or(std::ptr::null_mut(), |info| {
        wrap_value(options_value(info.options))
    })
}

/// `void duckdb_copy_function_bind_set_bind_data(duckdb_copy_function_bind_info info,
/// void *bind_data, duckdb_delete_callback_t destructor)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_bind_set_bind_data(
    info: DuckdbCopyFunctionBindInfo,
    bind_data: *mut c_void,
    destructor: DuckdbDeleteCallback,
) {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    if let Some(info) = unsafe { info.cast::<CCopyToInternalBindInfo<'_>>().as_mut() } {
        info.bind_data = UserData::new(bind_data, destructor);
    }
}

/// `void duckdb_copy_function_set_global_init(duckdb_copy_function copy_function,
/// duckdb_copy_function_global_init_t init)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_set_global_init(
    copy_function: DuckdbCopyFunction,
    init: DuckdbCopyFunctionGlobalInit,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(function), Some(init)) = (unsafe { definition(copy_function) }, init) {
        function
            .info
            .set_callbacks(|callbacks| callbacks.global_init = Some(init));
    }
}

/// `void duckdb_copy_function_global_init_set_error(duckdb_copy_function_global_init_info
/// info, const char *error)` (capi/copy_function-c.cpp): the write fails with an
/// invalid-input error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_global_init_set_error(
    info: DuckdbCopyFunctionGlobalInitInfo,
    error: *const c_char,
) {
    // SAFETY: `info` is NULL or the info of a running global init callback.
    if let Some(info) = unsafe { info.cast::<CCopyToGlobalInitInfo<'_>>().as_mut() }
        && !error.is_null()
    {
        // SAFETY: `error` is a C string per the API contract.
        info.error = Some(unsafe { error_bytes(error) });
    }
}

/// `void *duckdb_copy_function_global_init_get_extra_info(
/// duckdb_copy_function_global_init_info info)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_global_init_get_extra_info(
    info: DuckdbCopyFunctionGlobalInitInfo,
) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running global init callback.
    unsafe { info.cast::<CCopyToGlobalInitInfo<'_>>().as_ref() }.map_or(std::ptr::null_mut(), |info| {
        info.bind_data.info.callbacks().extra_info
    })
}

/// `duckdb_client_context duckdb_copy_function_global_init_get_client_context(
/// duckdb_copy_function_global_init_info info)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_global_init_get_client_context(
    info: DuckdbCopyFunctionGlobalInitInfo,
) -> DuckdbClientContext {
    // SAFETY: `info` is NULL or the info of a running global init callback.
    unsafe { info.cast::<CCopyToGlobalInitInfo<'_>>().as_ref() }
        .and_then(|info| info.bind_data.client.as_ref())
        .map_or(std::ptr::null_mut(), wrap_client_context)
}

/// `void *duckdb_copy_function_global_init_get_bind_data(
/// duckdb_copy_function_global_init_info info)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_global_init_get_bind_data(
    info: DuckdbCopyFunctionGlobalInitInfo,
) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running global init callback.
    unsafe { info.cast::<CCopyToGlobalInitInfo<'_>>().as_ref() }
        .map_or(std::ptr::null_mut(), |info| info.bind_data.bind_data.get())
}

/// `void duckdb_copy_function_global_init_set_global_state(
/// duckdb_copy_function_global_init_info info, void *global_state,
/// duckdb_delete_callback_t destructor)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_global_init_set_global_state(
    info: DuckdbCopyFunctionGlobalInitInfo,
    global_state: *mut c_void,
    destructor: DuckdbDeleteCallback,
) {
    // SAFETY: `info` is NULL or the info of a running global init callback.
    if let Some(info) = unsafe { info.cast::<CCopyToGlobalInitInfo<'_>>().as_mut() } {
        info.global_state = UserData::new(global_state, destructor);
    }
}

/// `const char *duckdb_copy_function_global_init_get_file_path(
/// duckdb_copy_function_global_init_info info)` (capi/copy_function-c.cpp): the path lives
/// as long as the callback runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_global_init_get_file_path(
    info: DuckdbCopyFunctionGlobalInitInfo,
) -> *const c_char {
    // SAFETY: `info` is NULL or the info of a running global init callback.
    unsafe { info.cast::<CCopyToGlobalInitInfo<'_>>().as_ref() }
        .map_or(std::ptr::null(), |info| info.file_path.as_ptr())
}

/// `void duckdb_copy_function_set_sink(duckdb_copy_function copy_function,
/// duckdb_copy_function_sink_t function)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_set_sink(
    copy_function: DuckdbCopyFunction,
    function: DuckdbCopyFunctionSink,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(definition), Some(sink)) = (unsafe { definition(copy_function) }, function) {
        definition
            .info
            .set_callbacks(|callbacks| callbacks.sink = Some(sink));
    }
}

/// `void duckdb_copy_function_sink_set_error(duckdb_copy_function_sink_info info,
/// const char *error)` (capi/copy_function-c.cpp): the write fails with an invalid-input
/// error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_sink_set_error(
    info: DuckdbCopyFunctionSinkInfo,
    error: *const c_char,
) {
    // SAFETY: `info` is NULL or the info of a running sink callback.
    unsafe { set_execute_error(info, error) };
}

/// `void *duckdb_copy_function_sink_get_extra_info(duckdb_copy_function_sink_info info)`
/// (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_sink_get_extra_info(
    info: DuckdbCopyFunctionSinkInfo,
) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running sink callback.
    unsafe { execute_extra_info(info) }
}

/// `duckdb_client_context duckdb_copy_function_sink_get_client_context(
/// duckdb_copy_function_sink_info info)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_sink_get_client_context(
    info: DuckdbCopyFunctionSinkInfo,
) -> DuckdbClientContext {
    // SAFETY: `info` is NULL or the info of a running sink callback.
    unsafe { execute_client_context(info) }
}

/// `void *duckdb_copy_function_sink_get_bind_data(duckdb_copy_function_sink_info info)`
/// (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_sink_get_bind_data(
    info: DuckdbCopyFunctionSinkInfo,
) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running sink callback.
    unsafe { execute_bind_data(info) }
}

/// `void *duckdb_copy_function_sink_get_global_state(duckdb_copy_function_sink_info info)`
/// (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_sink_get_global_state(
    info: DuckdbCopyFunctionSinkInfo,
) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running sink callback.
    unsafe { execute_global_state(info) }
}

/// `void duckdb_copy_function_set_finalize(duckdb_copy_function copy_function,
/// duckdb_copy_function_finalize_t finalize)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_set_finalize(
    copy_function: DuckdbCopyFunction,
    finalize: DuckdbCopyFunctionFinalize,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(function), Some(finalize)) = (unsafe { definition(copy_function) }, finalize) {
        function
            .info
            .set_callbacks(|callbacks| callbacks.finalize = Some(finalize));
    }
}

/// `void duckdb_copy_function_finalize_set_error(duckdb_copy_function_finalize_info info,
/// const char *error)` (capi/copy_function-c.cpp): the write fails with an invalid-input
/// error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_finalize_set_error(
    info: DuckdbCopyFunctionFinalizeInfo,
    error: *const c_char,
) {
    // SAFETY: `info` is NULL or the info of a running finalize callback.
    unsafe { set_execute_error(info, error) };
}

/// `void *duckdb_copy_function_finalize_get_extra_info(duckdb_copy_function_finalize_info
/// info)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_finalize_get_extra_info(
    info: DuckdbCopyFunctionFinalizeInfo,
) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running finalize callback.
    unsafe { execute_extra_info(info) }
}

/// `duckdb_client_context duckdb_copy_function_finalize_get_client_context(
/// duckdb_copy_function_finalize_info info)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_finalize_get_client_context(
    info: DuckdbCopyFunctionFinalizeInfo,
) -> DuckdbClientContext {
    // SAFETY: `info` is NULL or the info of a running finalize callback.
    unsafe { execute_client_context(info) }
}

/// `void *duckdb_copy_function_finalize_get_bind_data(duckdb_copy_function_finalize_info
/// info)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_finalize_get_bind_data(
    info: DuckdbCopyFunctionFinalizeInfo,
) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running finalize callback.
    unsafe { execute_bind_data(info) }
}

/// `void *duckdb_copy_function_finalize_get_global_state(
/// duckdb_copy_function_finalize_info info)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_finalize_get_global_state(
    info: DuckdbCopyFunctionFinalizeInfo,
) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running finalize callback.
    unsafe { execute_global_state(info) }
}

/// The sink or finalize info behind a handle.
///
/// # Safety
/// `info` must be NULL or the info of a running sink or finalize callback.
unsafe fn execute_info<'a>(info: *mut c_void) -> Option<&'a mut CCopyToExecuteInfo<'a>> {
    // SAFETY: guaranteed by the caller.
    unsafe { info.cast::<CCopyToExecuteInfo<'a>>().as_mut() }
}

/// Records the error of a sink or finalize callback.
///
/// # Safety
/// As for [`execute_info`]; `error` must be NULL or a C string.
unsafe fn set_execute_error(info: *mut c_void, error: *const c_char) {
    // SAFETY: forwarded.
    if let Some(info) = unsafe { execute_info(info) }
        && !error.is_null()
    {
        // SAFETY: forwarded.
        info.error = Some(unsafe { error_bytes(error) });
    }
}

/// The extra info, for a sink or finalize callback.
///
/// # Safety
/// As for [`execute_info`].
unsafe fn execute_extra_info(info: *mut c_void) -> *mut c_void {
    // SAFETY: forwarded.
    unsafe { execute_info(info) }.map_or(std::ptr::null_mut(), |info| {
        info.bind_data.info.callbacks().extra_info
    })
}

/// The client context, for a sink or finalize callback.
///
/// # Safety
/// As for [`execute_info`].
unsafe fn execute_client_context(info: *mut c_void) -> DuckdbClientContext {
    // SAFETY: forwarded.
    unsafe { execute_info(info) }
        .and_then(|info| info.bind_data.client.as_ref())
        .map_or(std::ptr::null_mut(), wrap_client_context)
}

/// The client's bind data, for a sink or finalize callback.
///
/// # Safety
/// As for [`execute_info`].
unsafe fn execute_bind_data(info: *mut c_void) -> *mut c_void {
    // SAFETY: forwarded.
    unsafe { execute_info(info) }.map_or(std::ptr::null_mut(), |info| info.bind_data.bind_data.get())
}

/// The client's global state, for a sink or finalize callback.
///
/// # Safety
/// As for [`execute_info`].
unsafe fn execute_global_state(info: *mut c_void) -> *mut c_void {
    // SAFETY: forwarded.
    unsafe { execute_info(info) }.map_or(std::ptr::null_mut(), |info| info.global_state.global_state.get())
}

/// `void duckdb_copy_function_set_copy_from_function(duckdb_copy_function copy_function,
/// duckdb_table_function table_function)` (capi/copy_function-c.cpp): the table function,
/// named after the format if it has no name, reads `COPY ... FROM`; ignored unless it has
/// its bind, init and function callbacks and resolved parameter types.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_copy_function_set_copy_from_function(
    copy_function: DuckdbCopyFunction,
    table_function: DuckdbTableFunction,
) {
    // SAFETY: both handles are NULL or live.
    let (Some(copy_function), Some(table_function)) = (unsafe { definition(copy_function) }, unsafe {
        table_function::definition(table_function)
    }) else {
        return;
    };
    if table_function.name.is_empty() {
        table_function.name.clone_from(&copy_function.name);
    }
    if !table_function.is_registrable() {
        return;
    }
    copy_function.copy_from = Some(table_function.clone());
}

/// `duckdb_state duckdb_register_copy_function(duckdb_connection connection,
/// duckdb_copy_function copy_function)` (capi/copy_function-c.cpp): the format needs a name
/// and a sink (for `COPY ... TO`) or a reader (for `COPY ... FROM`); a format of that name
/// already registered stays as it is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_register_copy_function(
    connection: DuckdbConnection,
    copy_function: DuckdbCopyFunction,
) -> DuckdbState {
    // SAFETY: both handles are NULL or live.
    let (Some(context), Some(definition)) = (unsafe { connection_ref(connection) }, unsafe {
        copy_function.as_ref()
    }) else {
        return DuckdbState::DuckDBError;
    };
    if definition.name.is_empty() {
        return DuckdbState::DuckDBError;
    }
    let is_copy_to = definition.info.callbacks().sink.is_some();
    if !is_copy_to && definition.copy_from.is_none() {
        return DuckdbState::DuckDBError;
    }
    let mut function = CopyFunction::new(definition.name.as_str());
    if is_copy_to {
        let info = Arc::clone(&definition.info);
        function.copy_to_bind = Some(Arc::new(move |input, _names, types| {
            c_copy_to_bind(&info, input, types)
        }));
        function.copy_to_initialize_global = Some(Arc::new(c_copy_to_global_init));
        function.copy_to_sink = Some(Arc::new(c_copy_to_sink));
        function.copy_to_finalize = Some(Arc::new(c_copy_to_finalize));
    }
    if let Some(reader) = &definition.copy_from {
        let reader_definition = reader.clone();
        function.copy_from_bind = Some(Arc::new(move |input, names, types| {
            c_copy_from_bind(&reader_definition, input, names, types)
        }));
        function.copy_from_function = Some(reader.to_function());
    }
    DuckdbState::from_ok(catch(|| context.lock().create_copy_function(function)).is_ok())
}
