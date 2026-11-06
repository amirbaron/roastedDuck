//! Replacement scans defined by the client (DuckDB's `capi/replacement_scan-c.cpp`): for a
//! table that does not exist, the client may name a table function to read instead.

use std::ffi::{c_char, c_void};
use std::sync::Arc;

use rustdb_ast::{
    ExpressionType, FunctionExpression, OrderModifier, OrdinalityType, ParsedExpression,
    ParsedExpressionKind, TableFunctionRef, TableRef, TableRefKind,
};
use rustdb_common::{Error, Result};
use rustdb_planner::{ReplacementScan, ReplacementScanInput};
use rustdb_types::Value;

use crate::function::{DuckdbDeleteCallback, error_bytes};
use crate::helper::to_cstring;
use crate::open::DuckdbDatabase;
use crate::value::{DuckdbValue, value_ref};

/// `duckdb_replacement_scan_info`
pub type DuckdbReplacementScanInfo = *mut c_void;
/// `duckdb_replacement_callback_t`
pub type DuckdbReplacementCallback =
    Option<unsafe extern "C" fn(DuckdbReplacementScanInfo, *const c_char, *mut c_void)>;

/// The client's callback and data (DuckDB's `CAPIReplacementScanData`), released with
/// the database: the delete callback receives the data, even when it is NULL.
struct CReplacementScanData {
    callback: unsafe extern "C" fn(DuckdbReplacementScanInfo, *const c_char, *mut c_void),
    extra_data: *mut c_void,
    delete_callback: DuckdbDeleteCallback,
}

// SAFETY: the data belongs to the client, which the C API contract makes responsible for
// its use from the threads that bind queries; the callbacks are plain C function
// pointers.
unsafe impl Send for CReplacementScanData {}
// SAFETY: see above; nothing is mutated through a shared reference.
unsafe impl Sync for CReplacementScanData {}

impl Drop for CReplacementScanData {
    fn drop(&mut self) {
        if let Some(delete) = self.delete_callback {
            // SAFETY: the client registered `delete` for this data, released once, here.
            unsafe { delete(self.extra_data) };
        }
    }
}

/// What a `duckdb_replacement_scan_info` points to (DuckDB's `CAPIReplacementScanInfo`).
#[derive(Debug, Default)]
struct CReplacementScanInfo {
    function_name: rustdb_common::ByteString,
    parameters: Vec<Value>,
    error: Option<Vec<u8>>,
}

/// `duckdb_capi_replacement_callback`: asks the client about the table; a function name
/// makes the table a call of that table function with the parameters given.
fn capi_replacement_callback(
    data: &CReplacementScanData,
    input: &ReplacementScanInput<'_>,
) -> Result<Option<TableRef>> {
    let mut info = CReplacementScanInfo::default();
    let table_name = to_cstring(input.table_name);
    // SAFETY: the client's callback receives the info and the table name, both valid for
    // the call, and its own data.
    unsafe {
        (data.callback)(
            std::ptr::from_mut(&mut info).cast(),
            table_name.as_ptr(),
            data.extra_data,
        );
    }
    if let Some(error) = info.error.filter(|error| !error.is_empty()) {
        let mut message = b"Error in replacement scan: ".to_vec();
        message.extend_from_slice(&error);
        message.push(b'\n');
        return Err(Error::new_bytes(rustdb_common::ErrorKind::Binder, message));
    }
    if info.function_name.is_empty() {
        return Ok(None);
    }
    let function = FunctionExpression {
        function_name: info.function_name,
        schema: rustdb_common::ByteString::new(),
        children: info
            .parameters
            .into_iter()
            .map(ParsedExpression::constant)
            .collect(),
        filter: None,
        order_bys: OrderModifier { orders: Vec::new() },
        distinct: false,
        is_operator: false,
        export_state: false,
        catalog: rustdb_common::ByteString::new(),
    };
    Ok(Some(TableRef {
        external_dependency: rustdb_ast::ExternalDependency::default(),
        alias: rustdb_common::ByteString::new(),
        sample: None,
        query_location: None,
        kind: TableRefKind::TableFunction(TableFunctionRef {
            function: Box::new(ParsedExpression::new(
                ExpressionType::Function,
                ParsedExpressionKind::Function(Box::new(function)),
            )),
            column_name_alias: Vec::new(),
            with_ordinality: OrdinalityType::WithoutOrdinality,
        }),
    }))
}

/// The info behind a handle.
///
/// # Safety
/// `info` must be NULL or the `duckdb_replacement_scan_info` of a running replacement
/// scan callback.
unsafe fn scan_info<'a>(info: DuckdbReplacementScanInfo) -> Option<&'a mut CReplacementScanInfo> {
    // SAFETY: guaranteed by the caller.
    unsafe { info.cast::<CReplacementScanInfo>().as_mut() }
}

/// `void duckdb_add_replacement_scan(duckdb_database db, duckdb_replacement_callback_t
/// replacement, void *extra_data, duckdb_delete_callback_t delete_callback)`
/// (capi/replacement_scan-c.cpp): tried after the built-in replacement scans and those
/// added before.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_add_replacement_scan(
    db: DuckdbDatabase,
    replacement: DuckdbReplacementCallback,
    extra_data: *mut c_void,
    delete_callback: DuckdbDeleteCallback,
) {
    // SAFETY: `db` is NULL or a live handle.
    let (Some(wrapper), Some(callback)) = (unsafe { db.as_ref() }, replacement) else {
        return;
    };
    let data = CReplacementScanData {
        callback,
        extra_data,
        delete_callback,
    };
    wrapper
        .database
        .add_replacement_scan(ReplacementScan(Arc::new(move |input| {
            capi_replacement_callback(&data, input)
        })));
}

/// `void duckdb_replacement_scan_set_function_name(duckdb_replacement_scan_info info,
/// const char *function_name)` (capi/replacement_scan-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_replacement_scan_set_function_name(
    info: DuckdbReplacementScanInfo,
    function_name: *const c_char,
) {
    // SAFETY: `info` is NULL or the info of a running callback; the name is NULL or a C
    // string.
    if let (Some(info), Some(name)) = unsafe { (scan_info(info), crate::helper::opt_bytes(function_name)) } {
        info.function_name = name.into();
    }
}

/// `void duckdb_replacement_scan_add_parameter(duckdb_replacement_scan_info info,
/// duckdb_value parameter)` (capi/replacement_scan-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_replacement_scan_add_parameter(
    info: DuckdbReplacementScanInfo,
    parameter: DuckdbValue,
) {
    // SAFETY: `info` is NULL or the info of a running callback; `parameter` NULL or live.
    if let (Some(info), Some(value)) = unsafe { (scan_info(info), value_ref(parameter)) } {
        info.parameters.push(value.clone());
    }
}

/// `void duckdb_replacement_scan_set_error(duckdb_replacement_scan_info info,
/// const char *error)` (capi/replacement_scan-c.cpp): binding fails with a binder error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_replacement_scan_set_error(
    info: DuckdbReplacementScanInfo,
    error: *const c_char,
) {
    // SAFETY: `info` is NULL or the info of a running callback.
    if let Some(info) = unsafe { scan_info(info) }
        && !error.is_null()
    {
        // SAFETY: `error` is a C string per the API contract.
        info.error = Some(unsafe { error_bytes(error) });
    }
}
