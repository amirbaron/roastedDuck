//! Log storages defined by the client (DuckDB's `capi/logging-c.cpp`).

use std::ffi::{c_char, c_void};
use std::sync::Arc;

use rustdb::LogStorage;
use rustdb_function::logging::LogLevel;

use crate::function::DuckdbDeleteCallback;
use crate::helper::{opt_str, to_cstring};
use crate::open::DuckdbDatabase;
use crate::types::*;

/// `duckdb_logger_write_log_entry_t`
pub type DuckdbLoggerWriteLogEntry = Option<
    unsafe extern "C" fn(*mut c_void, *mut DuckdbTimestamp, *const c_char, *const c_char, *const c_char),
>;

/// What a `duckdb_log_storage` points to (DuckDB's `LogStorageWrapper`).
#[derive(Debug)]
pub struct LogStorageWrapper {
    name: String,
    write_log_entry: DuckdbLoggerWriteLogEntry,
    extra_data: *mut c_void,
    delete_callback: DuckdbDeleteCallback,
}

/// `duckdb_log_storage`
pub type DuckdbLogStorage = *mut LogStorageWrapper;

/// A registered client storage (DuckDB's `CallbackLogStorage`): every entry goes to the
/// client's callback; the extra data is released with the database.
#[derive(Debug)]
struct CallbackLogStorage {
    write_log_entry:
        unsafe extern "C" fn(*mut c_void, *mut DuckdbTimestamp, *const c_char, *const c_char, *const c_char),
    extra_data: *mut c_void,
    delete_callback: DuckdbDeleteCallback,
}

// SAFETY: the extra data belongs to the client, which the C API contract makes
// responsible for its use from the threads that log; the callbacks are plain C function
// pointers.
unsafe impl Send for CallbackLogStorage {}
// SAFETY: see above; nothing is mutated through a shared reference.
unsafe impl Sync for CallbackLogStorage {}

impl LogStorage for CallbackLogStorage {
    fn write_log_entry(&self, timestamp: i64, level: LogLevel, log_type: &str, message: &[u8]) {
        let mut timestamp = DuckdbTimestamp { micros: timestamp };
        let level = to_cstring(level.name());
        let log_type = to_cstring(log_type);
        let message = to_cstring(message);
        // SAFETY: the client's callback receives its extra data and the entry's parts, all
        // valid for the call.
        unsafe {
            (self.write_log_entry)(
                self.extra_data,
                &raw mut timestamp,
                level.as_ptr(),
                log_type.as_ptr(),
                message.as_ptr(),
            );
        }
    }
}

impl Drop for CallbackLogStorage {
    fn drop(&mut self) {
        if let Some(delete) = self.delete_callback
            && !self.extra_data.is_null()
        {
            // SAFETY: the client registered `delete` for this data, released once, here.
            unsafe { delete(self.extra_data) };
        }
    }
}

/// `duckdb_log_storage duckdb_create_log_storage()` (capi/logging-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_log_storage() -> DuckdbLogStorage {
    Box::into_raw(Box::new(LogStorageWrapper {
        name: String::new(),
        write_log_entry: None,
        extra_data: std::ptr::null_mut(),
        delete_callback: None,
    }))
}

/// `void duckdb_destroy_log_storage(duckdb_log_storage *log_storage)` (capi/logging-c.cpp):
/// releases the extra data unless a registered storage owns it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_log_storage(log_storage: *mut DuckdbLogStorage) {
    // SAFETY: `log_storage` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = log_storage.as_mut() else { return };
        if slot.is_null() {
            return;
        }
        let wrapper = Box::from_raw(*slot);
        if let Some(delete) = wrapper.delete_callback
            && !wrapper.extra_data.is_null()
        {
            delete(wrapper.extra_data);
        }
        *slot = std::ptr::null_mut();
    }
}

/// `void duckdb_log_storage_set_write_log_entry(duckdb_log_storage log_storage,
/// duckdb_logger_write_log_entry_t function)` (capi/logging-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_log_storage_set_write_log_entry(
    log_storage: DuckdbLogStorage,
    function: DuckdbLoggerWriteLogEntry,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(wrapper), Some(function)) = (unsafe { log_storage.as_mut() }, function) {
        wrapper.write_log_entry = Some(function);
    }
}

/// `void duckdb_log_storage_set_extra_data(duckdb_log_storage log_storage, void *extra_data,
/// duckdb_delete_callback_t delete_callback)` (capi/logging-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_log_storage_set_extra_data(
    log_storage: DuckdbLogStorage,
    extra_data: *mut c_void,
    delete_callback: DuckdbDeleteCallback,
) {
    // SAFETY: the handle is NULL or live.
    if let Some(wrapper) = unsafe { log_storage.as_mut() } {
        wrapper.extra_data = extra_data;
        wrapper.delete_callback = delete_callback;
    }
}

/// `void duckdb_log_storage_set_name(duckdb_log_storage log_storage, const char *name)`
/// (capi/logging-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_log_storage_set_name(log_storage: DuckdbLogStorage, name: *const c_char) {
    // SAFETY: the handle is NULL or live; `name` is NULL or a C string.
    if let (Some(wrapper), Some(name)) = unsafe { (log_storage.as_mut(), opt_str(name)) } {
        name.clone_into(&mut wrapper.name);
    }
}

/// `duckdb_state duckdb_register_log_storage(duckdb_database database, duckdb_log_storage
/// log_storage)` (capi/logging-c.cpp): the storage needs a name and a write callback, and
/// no storage of that name may be registered; once registered it owns the extra data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_register_log_storage(
    database: DuckdbDatabase,
    log_storage: DuckdbLogStorage,
) -> DuckdbState {
    // SAFETY: both handles are NULL or live.
    let (Some(database), Some(wrapper)) = (unsafe { database.as_ref() }, unsafe { log_storage.as_mut() })
    else {
        return DuckdbState::DuckDBError;
    };
    let Some(write_log_entry) = wrapper.write_log_entry else {
        return DuckdbState::DuckDBError;
    };
    if wrapper.name.is_empty() {
        return DuckdbState::DuckDBError;
    }
    let storage = Arc::new(CallbackLogStorage {
        write_log_entry,
        extra_data: wrapper.extra_data,
        delete_callback: wrapper.delete_callback,
    });
    if !database
        .database
        .register_log_storage(&wrapper.name, Arc::clone(&storage) as Arc<dyn LogStorage>)
    {
        // The storage does not take the extra data over: the wrapper still releases it.
        if let Some(mut unregistered) = Arc::into_inner(storage) {
            unregistered.delete_callback = None;
        }
        return DuckdbState::DuckDBError;
    }
    wrapper.extra_data = std::ptr::null_mut();
    wrapper.delete_callback = None;
    DuckdbState::DuckDBSuccess
}
