//! Opening databases and connections.

use std::ffi::{CStr, c_char, c_void};

use rustdb::{Config, Connection, Database};

use crate::memory::c_string;
use crate::types::{DuckdbConnection, DuckdbDatabase, DuckdbState, Handle};

/// Reads an optional C string; NULL and invalid UTF-8 are reported as `None`.
///
/// # Safety
/// `ptr` must be NULL or point to a NUL-terminated string.
pub(crate) unsafe fn opt_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller.
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

fn boxed_handle<T>(value: T) -> *mut Handle {
    Box::into_raw(Box::new(Handle {
        internal_ptr: Box::into_raw(Box::new(value)).cast::<c_void>(),
    }))
}

/// Borrows the object behind a handle.
///
/// # Safety
/// `handle` must be NULL or a live handle created by `boxed_handle::<T>`.
pub(crate) unsafe fn handle_ref<'a, T>(handle: *mut Handle) -> Option<&'a mut T> {
    // SAFETY: guaranteed by the caller.
    unsafe { handle.as_ref().and_then(|h| h.internal_ptr.cast::<T>().as_mut()) }
}

/// Frees a handle created by `boxed_handle::<T>` and nulls the caller's pointer.
///
/// # Safety
/// `slot` must be NULL or point to NULL or to a live handle created by `boxed_handle::<T>`.
unsafe fn free_handle<T>(slot: *mut *mut Handle) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let Some(slot) = slot.as_mut() else { return };
        if slot.is_null() {
            return;
        }
        let handle = Box::from_raw(*slot);
        if !handle.internal_ptr.is_null() {
            drop(Box::from_raw(handle.internal_ptr.cast::<T>()));
        }
        *slot = std::ptr::null_mut();
    }
}

/// `duckdb_state duckdb_open_ext(const char *path, duckdb_database *out, duckdb_config config, char **out_error)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_open_ext(
    path: *const c_char,
    out_database: *mut DuckdbDatabase,
    _config: *mut c_void,
    out_error: *mut *mut c_char,
) -> DuckdbState {
    if out_database.is_null() {
        return DuckdbState::DuckDBError;
    }
    // SAFETY: `path` is NULL or a C string per the API contract.
    let path = unsafe { opt_str(path) }.unwrap_or("");
    match Database::open(path, &Config::default()) {
        Ok(database) => {
            // SAFETY: checked non-null above.
            unsafe { *out_database = boxed_handle(database) };
            DuckdbState::DuckDBSuccess
        }
        Err(error) => {
            if !out_error.is_null() {
                // SAFETY: the caller provided a writable `char **`.
                unsafe { *out_error = c_string(&error.to_string()) };
            }
            DuckdbState::DuckDBError
        }
    }
}

/// `duckdb_state duckdb_open(const char *path, duckdb_database *out_database)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_open(path: *const c_char, out_database: *mut DuckdbDatabase) -> DuckdbState {
    // SAFETY: forwarded contract.
    unsafe { duckdb_open_ext(path, out_database, std::ptr::null_mut(), std::ptr::null_mut()) }
}

/// `void duckdb_close(duckdb_database *database)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_close(database: *mut DuckdbDatabase) {
    // SAFETY: `database` holds a handle from `duckdb_open*` (or NULL).
    unsafe { free_handle::<Database>(database) };
}

/// `duckdb_state duckdb_connect(duckdb_database database, duckdb_connection *out_connection)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_connect(
    database: DuckdbDatabase,
    out_connection: *mut DuckdbConnection,
) -> DuckdbState {
    // SAFETY: `database` is a live handle from `duckdb_open*` (or NULL).
    let Some(database) = (unsafe { handle_ref::<Database>(database) }) else {
        return DuckdbState::DuckDBError;
    };
    if out_connection.is_null() {
        return DuckdbState::DuckDBError;
    }
    match database.connect() {
        Ok(connection) => {
            // SAFETY: checked non-null above.
            unsafe { *out_connection = boxed_handle(connection) };
            DuckdbState::DuckDBSuccess
        }
        Err(_) => DuckdbState::DuckDBError,
    }
}

/// `void duckdb_disconnect(duckdb_connection *connection)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_disconnect(connection: *mut DuckdbConnection) {
    // SAFETY: `connection` holds a handle from `duckdb_connect` (or NULL).
    unsafe { free_handle::<Connection>(connection) };
}

/// `const char *duckdb_library_version()`
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_library_version() -> *const c_char {
    c"v1.5.6".as_ptr()
}
