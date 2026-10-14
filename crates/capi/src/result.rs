//! Running queries and reading materialised results.

use std::ffi::{c_char, c_void};

use rustdb::{Connection, MaterializedResult};

use crate::memory::c_string;
use crate::open::{handle_ref, opt_str};
use crate::types::{DuckdbConnection, DuckdbResult, DuckdbState, DuckdbType, Idx, duckdb_type};

/// What `duckdb_result::internal_data` points to.
struct ResultData {
    outcome: Result<MaterializedResult, rustdb::Error>,
    /// The error message, kept alive for `duckdb_result_error`.
    error_message: *mut c_char,
    /// Column names as C strings, kept alive for `duckdb_column_name`.
    names: Vec<*mut c_char>,
}

impl Drop for ResultData {
    fn drop(&mut self) {
        for ptr in std::iter::once(self.error_message).chain(self.names.iter().copied()) {
            // SAFETY: produced by `c_string` and owned exclusively by this result.
            unsafe { crate::memory::duckdb_free(ptr.cast()) };
        }
    }
}

/// # Safety
/// `result` must be NULL or a result filled in by `duckdb_query`.
unsafe fn data<'a>(result: *mut DuckdbResult) -> Option<&'a ResultData> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        result
            .as_ref()
            .and_then(|r| r.internal_data.cast::<ResultData>().as_ref())
    }
}

/// # Safety
/// As for [`data`].
unsafe fn materialized<'a>(result: *mut DuckdbResult) -> Option<&'a MaterializedResult> {
    // SAFETY: forwarded.
    unsafe { data(result) }.and_then(|data| data.outcome.as_ref().ok())
}

/// `duckdb_state duckdb_query(duckdb_connection connection, const char *query, duckdb_result *out_result)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_query(
    connection: DuckdbConnection,
    query: *const c_char,
    out_result: *mut DuckdbResult,
) -> DuckdbState {
    // SAFETY: `connection` is a live handle from `duckdb_connect` (or NULL).
    let Some(connection) = (unsafe { handle_ref::<Connection>(connection) }) else {
        return DuckdbState::DuckDBError;
    };
    // SAFETY: `query` is a NUL-terminated string per the API contract.
    let outcome = match unsafe { opt_str(query) } {
        Some(sql) => connection.query(sql),
        None => Err(rustdb::Error::invalid_input("query must be a valid UTF-8 string")),
    };
    let state = if outcome.is_ok() {
        DuckdbState::DuckDBSuccess
    } else {
        DuckdbState::DuckDBError
    };
    let (columns, rows, names) = match &outcome {
        Ok(result) => (
            result.column_count(),
            result.row_count(),
            result.names().iter().map(|n| c_string(n)).collect(),
        ),
        Err(_) => (0, 0, Vec::new()),
    };
    let error_message = outcome
        .as_ref()
        .err()
        .map_or(std::ptr::null_mut(), |e| c_string(&e.to_string()));
    let data = Box::new(ResultData {
        outcome,
        error_message,
        names,
    });
    // SAFETY: `out_result` is NULL or points to caller-owned `duckdb_result` storage.
    if let Some(out) = unsafe { out_result.as_mut() } {
        *out = DuckdbResult {
            deprecated_column_count: columns as Idx,
            deprecated_row_count: rows as Idx,
            deprecated_rows_changed: 0,
            deprecated_columns: std::ptr::null_mut(),
            deprecated_error_message: data.error_message,
            internal_data: Box::into_raw(data).cast::<c_void>(),
        };
    }
    state
}

/// `void duckdb_destroy_result(duckdb_result *result)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_result(result: *mut DuckdbResult) {
    // SAFETY: `result` is NULL or was filled in by `duckdb_query`.
    let Some(result) = (unsafe { result.as_mut() }) else {
        return;
    };
    if !result.internal_data.is_null() {
        // SAFETY: `internal_data` was created by `Box::into_raw` in `duckdb_query`.
        drop(unsafe { Box::from_raw(result.internal_data.cast::<ResultData>()) });
    }
    result.internal_data = std::ptr::null_mut();
    result.deprecated_error_message = std::ptr::null_mut();
}

/// `const char *duckdb_result_error(duckdb_result *result)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_result_error(result: *mut DuckdbResult) -> *const c_char {
    // SAFETY: forwarded API contract.
    unsafe { data(result) }.map_or(std::ptr::null(), |data| data.error_message.cast_const())
}

/// `idx_t duckdb_column_count(duckdb_result *result)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_column_count(result: *mut DuckdbResult) -> Idx {
    // SAFETY: forwarded API contract.
    unsafe { materialized(result) }.map_or(0, |r| r.column_count() as Idx)
}

/// `idx_t duckdb_row_count(duckdb_result *result)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_row_count(result: *mut DuckdbResult) -> Idx {
    // SAFETY: forwarded API contract.
    unsafe { materialized(result) }.map_or(0, |r| r.row_count() as Idx)
}

/// `const char *duckdb_column_name(duckdb_result *result, idx_t col)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_column_name(result: *mut DuckdbResult, col: Idx) -> *const c_char {
    // SAFETY: forwarded API contract.
    unsafe { data(result) }
        .and_then(|data| data.names.get(col as usize).copied())
        .map_or(std::ptr::null(), <*mut c_char>::cast_const)
}

/// `duckdb_type duckdb_column_type(duckdb_result *result, idx_t col)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_column_type(result: *mut DuckdbResult, col: Idx) -> DuckdbType {
    // SAFETY: forwarded API contract.
    unsafe { materialized(result) }
        .and_then(|r| r.types().get(col as usize))
        .map_or(0, |ty| duckdb_type(ty.id()))
}

/// `bool duckdb_value_is_null(duckdb_result *result, idx_t col, idx_t row)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_value_is_null(result: *mut DuckdbResult, col: Idx, row: Idx) -> bool {
    // SAFETY: forwarded API contract.
    unsafe { materialized(result) }
        .filter(|r| (col as usize) < r.column_count() && (row as usize) < r.row_count())
        .is_none_or(|r| r.value(col as usize, row as usize).is_null())
}

/// `char *duckdb_value_varchar(duckdb_result *result, idx_t col, idx_t row)`: the value cast
/// to VARCHAR (free with `duckdb_free`), or NULL for NULL values and invalid positions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_value_varchar(result: *mut DuckdbResult, col: Idx, row: Idx) -> *mut c_char {
    // SAFETY: forwarded API contract.
    let Some(result) = (unsafe { materialized(result) }) else {
        return std::ptr::null_mut();
    };
    if (col as usize) >= result.column_count() || (row as usize) >= result.row_count() {
        return std::ptr::null_mut();
    }
    let value = result.value(col as usize, row as usize);
    if value.is_null() {
        return std::ptr::null_mut();
    }
    value
        .to_varchar()
        .map_or(std::ptr::null_mut(), |text| c_string(&text))
}
