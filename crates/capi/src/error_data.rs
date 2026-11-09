//! Error objects handed to C (DuckDB's `capi/error_data-c.cpp`).

use std::ffi::{CString, c_char};

use roastedduck_common::Error;

use crate::helper::{error_type_from_c, error_type_to_c, lossy_str};
use crate::types::{DUCKDB_ERROR_INVALID_TYPE, DuckdbErrorType};

/// What a `duckdb_error_data` points to (DuckDB's `ErrorDataWrapper`).
#[derive(Debug)]
pub struct ErrorDataWrapper {
    error: Option<Error>,
    /// The raw message, kept alive for `duckdb_error_data_message`.
    message: CString,
}

impl ErrorDataWrapper {
    /// A wrapper of `error` (none for an object without an error).
    pub(crate) fn new(error: Option<Error>) -> Self {
        let message = error
            .as_ref()
            .map(|error| crate::helper::to_cstring(error.raw_message_bytes()))
            .unwrap_or_default();
        Self { error, message }
    }

    /// Hands the wrapper to C.
    pub(crate) fn into_handle(self) -> DuckdbErrorData {
        Box::into_raw(Box::new(self))
    }
}

/// `duckdb_error_data`
pub type DuckdbErrorData = *mut ErrorDataWrapper;

/// `duckdb_error_data duckdb_create_error_data(duckdb_error_type type, const char *message)`
/// (capi/error_data-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_error_data(
    ty: DuckdbErrorType,
    message: *const c_char,
) -> DuckdbErrorData {
    let message = if message.is_null() {
        String::new()
    } else {
        // SAFETY: `message` is a NUL-terminated string per the API contract.
        unsafe { lossy_str(message) }
    };
    ErrorDataWrapper::new(Some(Error::new(error_type_from_c(ty), message))).into_handle()
}

/// `void duckdb_destroy_error_data(duckdb_error_data *error_data)` (capi/error_data-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_error_data(error_data: *mut DuckdbErrorData) {
    // SAFETY: `error_data` is NULL or points to NULL or a handle from this module.
    unsafe {
        let Some(slot) = error_data.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `duckdb_error_type duckdb_error_data_error_type(duckdb_error_data error_data)`
/// (capi/error_data-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_error_data_error_type(error_data: DuckdbErrorData) -> DuckdbErrorType {
    // SAFETY: `error_data` is NULL or a live handle.
    let Some(wrapper) = (unsafe { error_data.as_ref() }) else {
        return DUCKDB_ERROR_INVALID_TYPE;
    };
    wrapper
        .error
        .as_ref()
        .map_or(0, |error| error_type_to_c(error.kind()))
}

/// `const char *duckdb_error_data_message(duckdb_error_data error_data)`
/// (capi/error_data-c.cpp): the raw message, without the error type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_error_data_message(error_data: DuckdbErrorData) -> *const c_char {
    // SAFETY: `error_data` is NULL or a live handle.
    unsafe { error_data.as_ref() }.map_or(std::ptr::null(), |wrapper| wrapper.message.as_ptr())
}

/// `bool duckdb_error_data_has_error(duckdb_error_data error_data)` (capi/error_data-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_error_data_has_error(error_data: DuckdbErrorData) -> bool {
    // SAFETY: `error_data` is NULL or a live handle.
    unsafe { error_data.as_ref() }.is_some_and(|wrapper| wrapper.error.is_some())
}
