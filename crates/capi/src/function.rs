//! What the function APIs of `duckdb.h` share: the info handles callbacks receive, the
//! user data callbacks hand around and its delete callback, and the checks every
//! registration makes (DuckDB's `capi_internal.hpp`).

use std::ffi::{CStr, c_char, c_void};

use rustdb_types::{LogicalType, LogicalTypeId};

use crate::logical_types::contains_type;

/// `duckdb_function_info`: what a function callback receives (each API points it to its
/// own info struct).
pub type DuckdbFunctionInfo = *mut c_void;
/// `duckdb_bind_info`
pub type DuckdbBindInfo = *mut c_void;
/// `duckdb_init_info`
pub type DuckdbInitInfo = *mut c_void;

/// `duckdb_delete_callback_t`
pub type DuckdbDeleteCallback = Option<unsafe extern "C" fn(*mut c_void)>;
/// `duckdb_copy_callback_t`
pub type DuckdbCopyCallback = Option<unsafe extern "C" fn(*mut c_void) -> *mut c_void>;

/// A pointer to client data and the callback that releases it, which runs when this is
/// dropped (the `extra_info` / `bind_data` / `init_data` members of DuckDB's C function
/// infos, whose destructors call `delete_callback`).
#[derive(Debug)]
pub(crate) struct UserData {
    data: *mut c_void,
    delete_callback: DuckdbDeleteCallback,
}

// SAFETY: the data belongs to the client, which the C API contract makes responsible
// for its use from the threads that run the function's callbacks; this type only hands
// the pointer back and calls the client's delete callback once.
unsafe impl Send for UserData {}
// SAFETY: see above; `UserData` is never mutated through a shared reference.
unsafe impl Sync for UserData {}

impl UserData {
    pub(crate) const fn new(data: *mut c_void, delete_callback: DuckdbDeleteCallback) -> Self {
        Self {
            data,
            delete_callback,
        }
    }

    /// No data.
    pub(crate) const fn none() -> Self {
        Self::new(std::ptr::null_mut(), None)
    }

    pub(crate) const fn get(&self) -> *mut c_void {
        self.data
    }
}

impl Drop for UserData {
    fn drop(&mut self) {
        if let Some(delete) = self.delete_callback
            && !self.data.is_null()
        {
            // SAFETY: the client registered `delete` for this pointer, which is released
            // exactly once, here.
            unsafe { delete(self.data) };
        }
    }
}

/// Whether a function may take `ty` as a parameter (`TypeVisitor::Contains(type,
/// INVALID)` fails registration).
pub(crate) fn is_valid_parameter_type(ty: &LogicalType) -> bool {
    !contains_type(ty, LogicalTypeId::Invalid)
}

/// Whether a function may return `ty` (neither INVALID nor ANY inside it).
pub(crate) fn is_valid_return_type(ty: &LogicalType) -> bool {
    !contains_type(ty, LogicalTypeId::Invalid) && !contains_type(ty, LogicalTypeId::Any)
}

/// Copies the byte string supplied by a C callback without UTF-8 conversion.
///
/// # Safety
/// `error` must point to a NUL-terminated string.
pub(crate) unsafe fn error_bytes(error: *const c_char) -> Vec<u8> {
    // SAFETY: guaranteed by the caller.
    unsafe { CStr::from_ptr(error) }.to_bytes().to_vec()
}
