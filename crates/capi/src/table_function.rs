//! Table functions defined by the client (DuckDB's `capi/table_function-c.cpp` and the
//! shared parts in `capi_internal_table.hpp`).
//!
//! A `duckdb_table_function` is a definition the client fills in; registering it copies
//! it into an engine [`TableFunction`] whose bind and parallel scan call the client's
//! callbacks (`CTableFunctionBind`, `CTableFunctionInit`, `CTableFunctionLocalInit`,
//! `CTableFunction`). The callbacks and the extra info live in a [`CTableFunctionInfo`]
//! shared by the definition and its copies; COPY formats reuse the bind for their
//! `COPY ... FROM` reader.

use std::any::Any;
use std::ffi::{CString, c_char, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_function::{
    FunctionClient, FunctionData, ParallelScanGlobal, ParallelScanLocal, ParallelTableScan, TableFunction,
    TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
};
use rustdb_planner::ClientContext as BinderContext;
use rustdb_types::{DataChunk, LogicalType, STANDARD_VECTOR_SIZE, Value};

use crate::data_chunk::DuckdbDataChunk;
use crate::function::{
    DuckdbBindInfo, DuckdbDeleteCallback, DuckdbFunctionInfo, DuckdbInitInfo, UserData, error_bytes,
    is_valid_parameter_type, is_valid_return_type,
};
use crate::helper::{catch, opt_str, to_cstring};
use crate::logical_types::{DuckdbLogicalType, type_ref, wrap_type};
use crate::open::{DuckdbClientContext, DuckdbConnection, connection_ref, wrap_client_context};
use crate::types::*;
use crate::value::{DuckdbValue, wrap_value};

/// `duckdb_table_function_bind_t`
pub type DuckdbTableFunctionBind = Option<unsafe extern "C" fn(DuckdbBindInfo)>;
/// `duckdb_table_function_init_t`
pub type DuckdbTableFunctionInit = Option<unsafe extern "C" fn(DuckdbInitInfo)>;
/// `duckdb_table_function_t`
pub type DuckdbTableFunctionT = Option<unsafe extern "C" fn(DuckdbFunctionInfo, DuckdbDataChunk)>;

/// The client's callbacks and extra info of a table function.
#[derive(Clone, Copy, Debug)]
struct TableCallbacks {
    bind: DuckdbTableFunctionBind,
    init: DuckdbTableFunctionInit,
    local_init: DuckdbTableFunctionInit,
    function: DuckdbTableFunctionT,
    extra_info: *mut c_void,
    delete_callback: DuckdbDeleteCallback,
}

impl Default for TableCallbacks {
    fn default() -> Self {
        Self {
            bind: None,
            init: None,
            local_init: None,
            function: None,
            extra_info: std::ptr::null_mut(),
            delete_callback: None,
        }
    }
}

/// DuckDB's `CTableFunctionInfo`: the client's callbacks and extra info, shared by a
/// definition and its copies. The extra info is released with its delete callback when
/// the last of them is gone.
#[derive(Debug, Default)]
pub(crate) struct CTableFunctionInfo {
    callbacks: RwLock<TableCallbacks>,
}

// SAFETY: the extra info belongs to the client, which the C API contract makes
// responsible for its use from the threads that run the function's callbacks; the
// callbacks are plain C function pointers.
unsafe impl Send for CTableFunctionInfo {}
// SAFETY: see above; the fields are only changed under the lock.
unsafe impl Sync for CTableFunctionInfo {}

impl CTableFunctionInfo {
    fn callbacks(&self) -> TableCallbacks {
        *self.callbacks.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_callbacks(&self, update: impl FnOnce(&mut TableCallbacks)) {
        update(&mut self.callbacks.write().unwrap_or_else(PoisonError::into_inner));
    }
}

impl Drop for CTableFunctionInfo {
    fn drop(&mut self) {
        let callbacks = self.callbacks();
        drop(UserData::new(callbacks.extra_info, callbacks.delete_callback));
    }
}

/// What a `duckdb_table_function` points to: the function's signature and properties
/// (DuckDB's `TableFunction`) and its shared callbacks.
#[derive(Clone, Debug)]
pub struct CTableFunction {
    pub(crate) name: String,
    arguments: Vec<LogicalType>,
    /// `named_parameters`, a map in DuckDB: the first type given for a name stays.
    named_parameters: Vec<(String, LogicalType)>,
    projection_pushdown: bool,
    pub(crate) info: Arc<CTableFunctionInfo>,
}

/// `duckdb_table_function`
pub type DuckdbTableFunction = *mut CTableFunction;

impl CTableFunction {
    /// The engine function calling the client's callbacks.
    pub(crate) fn to_function(&self) -> TableFunction {
        let mut function = TableFunction::new(self.name.as_str(), self.arguments.clone());
        function.named_parameters.clone_from(&self.named_parameters);
        function.projection_pushdown = self.projection_pushdown;
        function.projected_output = self.projection_pushdown;
        let info = Arc::clone(&self.info);
        function.bind = Some(Arc::new(move |input| c_table_function_bind(&info, input)));
        function.cardinality = Some(Arc::new(|bind_data| {
            c_table_bind_data(bind_data)
                .ok()
                .and_then(|bind_data| bind_data.cardinality)
        }));
        function.parallel_scan = Some(c_table_function_scan(self.projection_pushdown));
        function
    }

    /// Whether the definition can be registered or read COPY files: it needs its bind,
    /// init and function callbacks and resolved parameter types.
    pub(crate) fn is_registrable(&self) -> bool {
        let callbacks = self.info.callbacks();
        callbacks.bind.is_some()
            && callbacks.init.is_some()
            && callbacks.function.is_some()
            && self.arguments.iter().all(is_valid_parameter_type)
            && self
                .named_parameters
                .iter()
                .all(|(_, ty)| is_valid_parameter_type(ty))
    }

    /// The declared type of the named parameter `name` (case-insensitive).
    pub(crate) fn named_parameter(&self, name: &str) -> Option<&LogicalType> {
        self.named_parameters
            .iter()
            .find(|(parameter, _)| parameter.eq_ignore_ascii_case(name))
            .map(|(_, ty)| ty)
    }
}

/// The bind data of a call (DuckDB's `CTableBindData`): the client's bind data, the
/// cardinality it reported and the columns the call produces.
#[derive(Debug)]
pub(crate) struct CTableBindData {
    info: Arc<CTableFunctionInfo>,
    /// Shared with the scans' global states, which hand it to the callbacks.
    bind_data: Arc<UserData>,
    /// `stats`: the estimated row count (`duckdb_bind_set_cardinality`).
    cardinality: Option<u64>,
    return_types: Vec<LogicalType>,
}

impl FunctionData for CTableBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The C API bind data of a scan.
fn c_table_bind_data(bind_data: Option<&dyn FunctionData>) -> Result<&CTableBindData> {
    bind_data
        .and_then(|data| data.as_any().downcast_ref::<CTableBindData>())
        .ok_or_else(|| Error::internal("C API table function without its bind data"))
}

/// What a `duckdb_bind_info` of a table function points to (DuckDB's
/// `CTableInternalBindInfo`).
struct CTableInternalBindInfo<'a> {
    client: Option<&'a Arc<FunctionClient>>,
    parameters: &'a [Value],
    named_parameters: &'a CaseInsensitiveMap<Value>,
    return_types: Vec<LogicalType>,
    names: Vec<CString>,
    bind_data: UserData,
    cardinality: Option<u64>,
    extra_info: *mut c_void,
    error: Option<Vec<u8>>,
}

/// The bind info behind a handle.
///
/// # Safety
/// `info` must be NULL or the `duckdb_bind_info` of a table function's running bind
/// callback.
unsafe fn bind_info<'a>(info: DuckdbBindInfo) -> Option<&'a mut CTableInternalBindInfo<'a>> {
    // SAFETY: guaranteed by the caller.
    unsafe { info.cast::<CTableInternalBindInfo<'a>>().as_mut() }
}

/// Runs the client's bind callback (`CTableFunctionBind`, and `CCopyFromBind` with the
/// target table's columns as `return_types` / `names`): returns the bind data and the
/// columns the call produces; the callback's error is a binder error.
pub(crate) fn c_table_bind(
    info: &Arc<CTableFunctionInfo>,
    client: Option<&Arc<FunctionClient>>,
    parameters: &[Value],
    named_parameters: &CaseInsensitiveMap<Value>,
    return_types: Vec<LogicalType>,
    names: &[String],
) -> Result<(CTableBindData, Vec<String>)> {
    let callbacks = info.callbacks();
    let Some(bind) = callbacks.bind else {
        return Err(Error::internal("C API table function without a bind callback"));
    };
    let mut bind_info = CTableInternalBindInfo {
        client,
        parameters,
        named_parameters,
        return_types,
        names: names.iter().map(to_cstring).collect(),
        bind_data: UserData::none(),
        cardinality: None,
        extra_info: callbacks.extra_info,
        error: None,
    };
    // SAFETY: the client's callback receives the bind info, valid for the call.
    unsafe { bind(std::ptr::from_mut(&mut bind_info).cast()) };
    if let Some(error) = bind_info.error {
        return Err(Error::new_bytes(rustdb_common::ErrorKind::Binder, error));
    }
    let names = bind_info
        .names
        .iter()
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    Ok((
        CTableBindData {
            info: Arc::clone(info),
            bind_data: Arc::new(bind_info.bind_data),
            cardinality: bind_info.cardinality,
            return_types: bind_info.return_types,
        },
        names,
    ))
}

/// `CTableFunctionBind`
fn c_table_function_bind(
    info: &Arc<CTableFunctionInfo>,
    input: &TableFunctionBindInput<'_>,
) -> Result<TableFunctionBindOutput> {
    let client = input
        .binder
        .and_then(|binder| binder.downcast_ref::<BinderContext>())
        .map(|binder| &binder.function_client);
    let named_parameters: CaseInsensitiveMap<Value> = input
        .named_parameters
        .iter()
        .map(|(name, value)| (name.as_str(), value.clone()))
        .collect();
    let (bind_data, names) = c_table_bind(info, client, input.inputs, &named_parameters, Vec::new(), &[])?;
    Ok(TableFunctionBindOutput {
        return_types: bind_data.return_types.clone(),
        names,
        bind_data: Some(Arc::new(bind_data)),
    })
}

/// The shared state of a scan (DuckDB's `CTableGlobalInitData`).
#[derive(Debug)]
struct CTableGlobalState {
    info: Arc<CTableFunctionInfo>,
    bind_data: Arc<UserData>,
    column_ids: Vec<u64>,
    init_data: UserData,
    max_threads: u64,
    /// The batch index of the next chunk produced: the chunks are numbered in the order
    /// the threads produce them, as the function has no partitions of its own.
    next_batch: AtomicU64,
}

/// The state of one thread of a scan (DuckDB's `CTableLocalInitData`).
#[derive(Debug)]
struct CTableLocalState {
    init_data: UserData,
    batch_index: u64,
}

/// What a `duckdb_init_info` points to (DuckDB's `CTableInternalInitInfo`).
struct CTableInternalInitInfo<'a> {
    extra_info: *mut c_void,
    bind_data: *mut c_void,
    column_ids: &'a [u64],
    init_data: UserData,
    max_threads: u64,
    error: Option<Vec<u8>>,
}

/// The init info behind a handle.
///
/// # Safety
/// `info` must be NULL or the `duckdb_init_info` of a table function's running init or
/// local init callback.
unsafe fn init_info<'a>(info: DuckdbInitInfo) -> Option<&'a mut CTableInternalInitInfo<'a>> {
    // SAFETY: guaranteed by the caller.
    unsafe { info.cast::<CTableInternalInitInfo<'a>>().as_mut() }
}

/// What a `duckdb_function_info` of a table function points to (DuckDB's
/// `CTableInternalFunctionInfo`).
struct CTableInternalFunctionInfo {
    extra_info: *mut c_void,
    bind_data: *mut c_void,
    init_data: *mut c_void,
    local_data: *mut c_void,
    error: Option<Vec<u8>>,
}

/// The function info behind a handle.
///
/// # Safety
/// `info` must be NULL or the `duckdb_function_info` of a table function's running
/// function callback.
unsafe fn function_info<'a>(info: DuckdbFunctionInfo) -> Option<&'a mut CTableInternalFunctionInfo> {
    // SAFETY: guaranteed by the caller.
    unsafe { info.cast::<CTableInternalFunctionInfo>().as_mut() }
}

/// Runs an init callback (`init` or `local_init`) of a scan of `column_ids`: returns its
/// init data and maximum thread count; the callback's error is an invalid-input error.
fn run_init(
    init: unsafe extern "C" fn(DuckdbInitInfo),
    info: &CTableFunctionInfo,
    bind_data: &UserData,
    column_ids: &[u64],
) -> Result<(UserData, u64)> {
    let mut init_info = CTableInternalInitInfo {
        extra_info: info.callbacks().extra_info,
        bind_data: bind_data.get(),
        column_ids,
        init_data: UserData::none(),
        max_threads: 1,
        error: None,
    };
    // SAFETY: the client's callback receives the init info, valid for the call.
    unsafe { init(std::ptr::from_mut(&mut init_info).cast()) };
    if let Some(error) = init_info.error {
        return Err(Error::new_bytes(rustdb_common::ErrorKind::InvalidInput, error));
    }
    Ok((init_info.init_data, init_info.max_threads))
}

/// The parallel scan of a client table function: `CTableFunctionInit` once,
/// `CTableFunctionLocalInit` per thread, `CTableFunction` per chunk. With projection
/// pushdown, the function fills one column per scanned column id, otherwise all of its
/// columns.
fn c_table_function_scan(projection_pushdown: bool) -> ParallelTableScan {
    ParallelTableScan {
        init_global: Arc::new(|bind_data, column_ids, _, _| {
            let bind_data = c_table_bind_data(bind_data)?;
            let Some(init) = bind_data.info.callbacks().init else {
                return Err(Error::internal("C API table function without an init callback"));
            };
            let (init_data, max_threads) = run_init(init, &bind_data.info, &bind_data.bind_data, column_ids)?;
            Ok(Arc::new(CTableGlobalState {
                info: Arc::clone(&bind_data.info),
                bind_data: Arc::clone(&bind_data.bind_data),
                column_ids: column_ids.to_vec(),
                init_data,
                max_threads,
                next_batch: AtomicU64::new(0),
            }) as ParallelScanGlobal)
        }),
        init_local: Arc::new(|global| {
            let global = global_state(global)?;
            let init_data = match global.info.callbacks().local_init {
                Some(local_init) => {
                    run_init(local_init, &global.info, &global.bind_data, &global.column_ids)?.0
                }
                None => UserData::none(),
            };
            Ok(Box::new(CTableLocalState {
                init_data,
                batch_index: 0,
            }) as ParallelScanLocal)
        }),
        scan: Arc::new(move |bind_data, column_ids, global, local, output| {
            let bind_data = c_table_bind_data(bind_data)?;
            let global = global_state(global)?;
            let local = local
                .downcast_mut::<CTableLocalState>()
                .ok_or_else(|| Error::internal("C API table function without its local state"))?;
            let types: Vec<LogicalType> = if projection_pushdown {
                column_ids
                    .iter()
                    .map(|&id| {
                        usize::try_from(id)
                            .ok()
                            .and_then(|index| bind_data.return_types.get(index))
                            .cloned()
                            .unwrap_or(LogicalType::BIGINT)
                    })
                    .collect()
            } else {
                bind_data.return_types.clone()
            };
            *output = DataChunk::with_types(&types, STANDARD_VECTOR_SIZE);
            let callbacks = global.info.callbacks();
            let Some(function) = callbacks.function else {
                return Err(Error::internal(
                    "C API table function without a function callback",
                ));
            };
            let mut function_info = CTableInternalFunctionInfo {
                extra_info: callbacks.extra_info,
                bind_data: global.bind_data.get(),
                init_data: global.init_data.get(),
                local_data: local.init_data.get(),
                error: None,
            };
            // SAFETY: the client's callback receives the function info and the output
            // chunk, both valid for the call.
            unsafe {
                function(
                    std::ptr::from_mut(&mut function_info).cast(),
                    std::ptr::from_mut(output),
                );
            }
            if let Some(error) = function_info.error {
                return Err(Error::new_bytes(rustdb_common::ErrorKind::InvalidInput, error));
            }
            local.batch_index = global.next_batch.fetch_add(1, Ordering::Relaxed);
            Ok(!output.is_empty())
        }),
        max_threads: Arc::new(|global| {
            global_state(global).map_or(1, |global| {
                usize::try_from(global.max_threads).unwrap_or(usize::MAX)
            })
        }),
        batch_index: Arc::new(|_, local| {
            local
                .downcast_ref::<CTableLocalState>()
                .map_or(0, |local| local.batch_index)
        }),
        partition_data: None,
        dynamic_to_string: None,
        row_group_metrics: None,
    }
}

/// The C API state of a scan.
fn global_state(global: &ParallelScanGlobal) -> Result<&CTableGlobalState> {
    global
        .downcast_ref::<CTableGlobalState>()
        .ok_or_else(|| Error::internal("C API table function without its global state"))
}

/// The definition behind a handle.
///
/// # Safety
/// `function` must be NULL or a live `duckdb_table_function`.
pub(crate) unsafe fn definition<'a>(function: DuckdbTableFunction) -> Option<&'a mut CTableFunction> {
    // SAFETY: guaranteed by the caller.
    unsafe { function.as_mut() }
}

/// `duckdb_table_function duckdb_create_table_function()` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_table_function() -> DuckdbTableFunction {
    Box::into_raw(Box::new(CTableFunction {
        name: String::new(),
        arguments: Vec::new(),
        named_parameters: Vec::new(),
        projection_pushdown: false,
        info: Arc::default(),
    }))
}

/// `void duckdb_destroy_table_function(duckdb_table_function *table_function)`
/// (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_table_function(table_function: *mut DuckdbTableFunction) {
    // SAFETY: `table_function` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = table_function.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `void duckdb_table_function_set_name(duckdb_table_function table_function,
/// const char *name)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_function_set_name(
    table_function: DuckdbTableFunction,
    name: *const c_char,
) {
    // SAFETY: the handle is NULL or live; `name` is NULL or a C string.
    if let (Some(function), Some(name)) = unsafe { (definition(table_function), opt_str(name)) } {
        name.clone_into(&mut function.name);
    }
}

/// `void duckdb_table_function_add_parameter(duckdb_table_function table_function,
/// duckdb_logical_type type)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_function_add_parameter(
    table_function: DuckdbTableFunction,
    ty: DuckdbLogicalType,
) {
    // SAFETY: both handles are NULL or live.
    if let (Some(function), Some(ty)) = unsafe { (definition(table_function), type_ref(ty)) } {
        function.arguments.push(ty.clone());
    }
}

/// `void duckdb_table_function_add_named_parameter(duckdb_table_function table_function,
/// const char *name, duckdb_logical_type type)` (capi/table_function-c.cpp): a name given
/// twice keeps its first type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_function_add_named_parameter(
    table_function: DuckdbTableFunction,
    name: *const c_char,
    ty: DuckdbLogicalType,
) {
    // SAFETY: the handles are NULL or live; `name` is NULL or a C string.
    let (Some(function), Some(name), Some(ty)) =
        (unsafe { (definition(table_function), opt_str(name), type_ref(ty)) })
    else {
        return;
    };
    if function.named_parameter(name).is_none() {
        function.named_parameters.push((name.to_owned(), ty.clone()));
    }
}

/// `void duckdb_table_function_set_extra_info(duckdb_table_function table_function,
/// void *extra_info, duckdb_delete_callback_t destroy)` (capi/table_function-c.cpp):
/// `destroy` releases the extra info once the function and its copies are gone; extra
/// info set before is not released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_function_set_extra_info(
    table_function: DuckdbTableFunction,
    extra_info: *mut c_void,
    destroy: DuckdbDeleteCallback,
) {
    // SAFETY: the handle is NULL or live.
    if let Some(function) = unsafe { definition(table_function) } {
        function.info.set_callbacks(|callbacks| {
            callbacks.extra_info = extra_info;
            callbacks.delete_callback = destroy;
        });
    }
}

/// `void duckdb_table_function_set_bind(duckdb_table_function table_function,
/// duckdb_table_function_bind_t bind)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_function_set_bind(
    table_function: DuckdbTableFunction,
    bind: DuckdbTableFunctionBind,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(function), Some(bind)) = (unsafe { definition(table_function) }, bind) {
        function
            .info
            .set_callbacks(|callbacks| callbacks.bind = Some(bind));
    }
}

/// `void duckdb_table_function_set_init(duckdb_table_function table_function,
/// duckdb_table_function_init_t init)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_function_set_init(
    table_function: DuckdbTableFunction,
    init: DuckdbTableFunctionInit,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(function), Some(init)) = (unsafe { definition(table_function) }, init) {
        function
            .info
            .set_callbacks(|callbacks| callbacks.init = Some(init));
    }
}

/// `void duckdb_table_function_set_local_init(duckdb_table_function table_function,
/// duckdb_table_function_init_t init)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_function_set_local_init(
    table_function: DuckdbTableFunction,
    init: DuckdbTableFunctionInit,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(function), Some(init)) = (unsafe { definition(table_function) }, init) {
        function
            .info
            .set_callbacks(|callbacks| callbacks.local_init = Some(init));
    }
}

/// `void duckdb_table_function_set_function(duckdb_table_function table_function,
/// duckdb_table_function_t function)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_function_set_function(
    table_function: DuckdbTableFunction,
    function: DuckdbTableFunctionT,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(definition), Some(function)) = (unsafe { definition(table_function) }, function) {
        definition
            .info
            .set_callbacks(|callbacks| callbacks.function = Some(function));
    }
}

/// `void duckdb_table_function_supports_projection_pushdown(duckdb_table_function
/// table_function, bool pushdown)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_function_supports_projection_pushdown(
    table_function: DuckdbTableFunction,
    pushdown: bool,
) {
    // SAFETY: the handle is NULL or live.
    if let Some(function) = unsafe { definition(table_function) } {
        function.projection_pushdown = pushdown;
    }
}

/// `duckdb_state duckdb_register_table_function(duckdb_connection con,
/// duckdb_table_function function)` (capi/table_function-c.cpp): a table function of that
/// name already registered stays as it is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_register_table_function(
    con: DuckdbConnection,
    function: DuckdbTableFunction,
) -> DuckdbState {
    // SAFETY: both handles are NULL or live.
    let (Some(context), Some(function)) = (unsafe { connection_ref(con) }, unsafe { function.as_ref() })
    else {
        return DuckdbState::DuckDBError;
    };
    if function.name.is_empty() || !function.is_registrable() {
        return DuckdbState::DuckDBError;
    }
    let mut set = TableFunctionSet::new(function.name.as_str());
    set.add(function.to_function());
    DuckdbState::from_ok(catch(|| context.lock().create_table_function_set(set)).is_ok())
}

/// `void *duckdb_bind_get_extra_info(duckdb_bind_info info)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_get_extra_info(info: DuckdbBindInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    unsafe { bind_info(info) }.map_or(std::ptr::null_mut(), |info| info.extra_info)
}

/// `void duckdb_table_function_get_client_context(duckdb_bind_info info,
/// duckdb_client_context *out_context)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_function_get_client_context(
    info: DuckdbBindInfo,
    out_context: *mut DuckdbClientContext,
) {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    let Some(client) = (unsafe { bind_info(info) }).and_then(|info| info.client) else {
        return;
    };
    if !out_context.is_null() {
        // SAFETY: checked non-null; writable per the API contract.
        unsafe { *out_context = wrap_client_context(client) };
    }
}

/// `void duckdb_bind_add_result_column(duckdb_bind_info info, const char *name,
/// duckdb_logical_type type)` (capi/table_function-c.cpp): ignored for an unresolved type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_add_result_column(
    info: DuckdbBindInfo,
    name: *const c_char,
    ty: DuckdbLogicalType,
) {
    // SAFETY: `info` is NULL or the info of a running bind callback; `ty` is NULL or live.
    let (Some(info), Some(ty)) = (unsafe { bind_info(info) }, unsafe { type_ref(ty) }) else {
        return;
    };
    if name.is_null() || !is_valid_return_type(ty) {
        return;
    }
    // SAFETY: `name` is a C string per the API contract.
    info.names
        .push(unsafe { std::ffi::CStr::from_ptr(name) }.to_owned());
    info.return_types.push(ty.clone());
}

/// `idx_t duckdb_bind_get_parameter_count(duckdb_bind_info info)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_get_parameter_count(info: DuckdbBindInfo) -> Idx {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    unsafe { bind_info(info) }.map_or(0, |info| info.parameters.len() as Idx)
}

/// `duckdb_value duckdb_bind_get_parameter(duckdb_bind_info info, idx_t index)`
/// (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_get_parameter(info: DuckdbBindInfo, index: Idx) -> DuckdbValue {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    unsafe { bind_info(info) }
        .and_then(|info| info.parameters.get(index as usize))
        .map_or(std::ptr::null_mut(), |value| wrap_value(value.clone()))
}

/// `duckdb_value duckdb_bind_get_named_parameter(duckdb_bind_info info, const char *name)`
/// (capi/table_function-c.cpp): NULL if the call does not give the parameter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_get_named_parameter(
    info: DuckdbBindInfo,
    name: *const c_char,
) -> DuckdbValue {
    // SAFETY: `info` is NULL or the info of a running bind callback; `name` is NULL or a C
    // string.
    let (Some(info), Some(name)) = (unsafe { bind_info(info) }, unsafe { opt_str(name) }) else {
        return std::ptr::null_mut();
    };
    info.named_parameters
        .get(name)
        .map_or(std::ptr::null_mut(), |value| wrap_value(value.clone()))
}

/// `void duckdb_bind_set_bind_data(duckdb_bind_info info, void *bind_data,
/// duckdb_delete_callback_t destroy)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_set_bind_data(
    info: DuckdbBindInfo,
    bind_data: *mut c_void,
    destroy: DuckdbDeleteCallback,
) {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    if let Some(info) = unsafe { bind_info(info) } {
        info.bind_data = UserData::new(bind_data, destroy);
    }
}

/// `void duckdb_bind_set_cardinality(duckdb_bind_info info, idx_t cardinality,
/// bool is_exact)` (capi/table_function-c.cpp): the row count the optimizer plans with;
/// the engine keeps one estimate, exact or not.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_set_cardinality(
    info: DuckdbBindInfo,
    cardinality: Idx,
    _is_exact: bool,
) {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    if let Some(info) = unsafe { bind_info(info) } {
        info.cardinality = Some(cardinality);
    }
}

/// `void duckdb_bind_set_error(duckdb_bind_info info, const char *error)`
/// (capi/table_function-c.cpp): binding fails with a binder error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_set_error(info: DuckdbBindInfo, error: *const c_char) {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    if let Some(info) = unsafe { bind_info(info) }
        && !error.is_null()
    {
        // SAFETY: `error` is a C string per the API contract.
        info.error = Some(unsafe { error_bytes(error) });
    }
}

/// `idx_t duckdb_table_function_bind_get_result_column_count(duckdb_bind_info info)`
/// (capi/copy_function-c.cpp): the columns so far (for a COPY reader, the target table's).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_function_bind_get_result_column_count(info: DuckdbBindInfo) -> Idx {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    unsafe { bind_info(info) }.map_or(0, |info| info.return_types.len() as Idx)
}

/// `duckdb_logical_type duckdb_table_function_bind_get_result_column_type(duckdb_bind_info
/// info, idx_t col_idx)` (capi/copy_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_function_bind_get_result_column_type(
    info: DuckdbBindInfo,
    col_idx: Idx,
) -> DuckdbLogicalType {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    unsafe { bind_info(info) }
        .and_then(|info| info.return_types.get(col_idx as usize))
        .map_or(std::ptr::null_mut(), |ty| wrap_type(ty.clone()))
}

/// `const char *duckdb_table_function_bind_get_result_column_name(duckdb_bind_info info,
/// idx_t col_idx)` (capi/copy_function-c.cpp): the name lives as long as the bind.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_table_function_bind_get_result_column_name(
    info: DuckdbBindInfo,
    col_idx: Idx,
) -> *const c_char {
    // SAFETY: `info` is NULL or the info of a running bind callback.
    unsafe { bind_info(info) }
        .and_then(|info| info.names.get(col_idx as usize))
        .map_or(std::ptr::null(), |name| name.as_ptr())
}

/// `void *duckdb_init_get_extra_info(duckdb_init_info info)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_init_get_extra_info(info: DuckdbInitInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running init callback.
    unsafe { init_info(info) }.map_or(std::ptr::null_mut(), |info| info.extra_info)
}

/// `void *duckdb_init_get_bind_data(duckdb_init_info info)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_init_get_bind_data(info: DuckdbInitInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running init callback.
    unsafe { init_info(info) }.map_or(std::ptr::null_mut(), |info| info.bind_data)
}

/// `void duckdb_init_set_init_data(duckdb_init_info info, void *init_data,
/// duckdb_delete_callback_t destroy)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_init_set_init_data(
    info: DuckdbInitInfo,
    init_data: *mut c_void,
    destroy: DuckdbDeleteCallback,
) {
    // SAFETY: `info` is NULL or the info of a running init callback.
    if let Some(info) = unsafe { init_info(info) } {
        info.init_data = UserData::new(init_data, destroy);
    }
}

/// `void duckdb_init_set_error(duckdb_init_info info, const char *error)`
/// (capi/table_function-c.cpp): the scan fails with an invalid-input error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_init_set_error(info: DuckdbInitInfo, error: *const c_char) {
    // SAFETY: `info` is NULL or the info of a running init callback.
    if let Some(info) = unsafe { init_info(info) }
        && !error.is_null()
    {
        // SAFETY: `error` is a C string per the API contract.
        info.error = Some(unsafe { error_bytes(error) });
    }
}

/// `idx_t duckdb_init_get_column_count(duckdb_init_info info)` (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_init_get_column_count(info: DuckdbInitInfo) -> Idx {
    // SAFETY: `info` is NULL or the info of a running init callback.
    unsafe { init_info(info) }.map_or(0, |info| info.column_ids.len() as Idx)
}

/// `idx_t duckdb_init_get_column_index(duckdb_init_info info, idx_t column_index)`
/// (capi/table_function-c.cpp): 0 out of range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_init_get_column_index(info: DuckdbInitInfo, column_index: Idx) -> Idx {
    // SAFETY: `info` is NULL or the info of a running init callback.
    unsafe { init_info(info) }
        .and_then(|info| info.column_ids.get(column_index as usize).copied())
        .unwrap_or(0)
}

/// `void duckdb_init_set_max_threads(duckdb_init_info info, idx_t max_threads)`
/// (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_init_set_max_threads(info: DuckdbInitInfo, max_threads: Idx) {
    // SAFETY: `info` is NULL or the info of a running init callback.
    if let Some(info) = unsafe { init_info(info) } {
        info.max_threads = max_threads;
    }
}

/// `void *duckdb_function_get_extra_info(duckdb_function_info info)`
/// (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_function_get_extra_info(info: DuckdbFunctionInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running function callback.
    unsafe { function_info(info) }.map_or(std::ptr::null_mut(), |info| info.extra_info)
}

/// `void *duckdb_function_get_bind_data(duckdb_function_info info)`
/// (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_function_get_bind_data(info: DuckdbFunctionInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running function callback.
    unsafe { function_info(info) }.map_or(std::ptr::null_mut(), |info| info.bind_data)
}

/// `void *duckdb_function_get_init_data(duckdb_function_info info)`
/// (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_function_get_init_data(info: DuckdbFunctionInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running function callback.
    unsafe { function_info(info) }.map_or(std::ptr::null_mut(), |info| info.init_data)
}

/// `void *duckdb_function_get_local_init_data(duckdb_function_info info)`
/// (capi/table_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_function_get_local_init_data(info: DuckdbFunctionInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running function callback.
    unsafe { function_info(info) }.map_or(std::ptr::null_mut(), |info| info.local_data)
}

/// `void duckdb_function_set_error(duckdb_function_info info, const char *error)`
/// (capi/table_function-c.cpp): the scan fails with an invalid-input error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_function_set_error(info: DuckdbFunctionInfo, error: *const c_char) {
    // SAFETY: `info` is NULL or the info of a running function callback.
    if let Some(info) = unsafe { function_info(info) }
        && !error.is_null()
    {
        // SAFETY: `error` is a C string per the API contract.
        info.error = Some(unsafe { error_bytes(error) });
    }
}
