//! Native ADBC driver, ported from DuckDB 1.5.6 `src/common/adbc/adbc.cpp`.
//! Callback coverage is tracked in docs/adbc.md; an exported initializer is not full acceptance.
mod batch;
mod errors;
mod ffi;
mod info;
mod ingest;
mod init;
mod manual;
mod metadata;
mod options;
mod statement;
mod statistics;
mod stream;
use crate::helper::{c_string, opt_bytes, to_cstring};
use crate::*;
pub use ffi::*;
pub use init::duckdb_adbc_init;
use roastedduck_arrow::ffi::{ArrowArray, ArrowArrayStream, ArrowSchema};
use roastedduck_common::{ByteString, byte_format};
use std::collections::{BTreeMap, VecDeque};
use std::ffi::{CString, c_char, c_int};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use stream::StreamState;
const OK: u8 = 0;
const NOT_IMPLEMENTED: u8 = 2;
const NOT_FOUND: u8 = 3;
const INVALID_ARGUMENT: u8 = 5;
const INVALID_STATE: u8 = 6;
const INTERNAL: u8 = 9;
const CANCELLED: u8 = 11;
const AUTOCOMMIT: &[u8] = b"adbc.connection.autocommit";
const CURRENT_CATALOG: &[u8] = b"adbc.connection.catalog";
const CURRENT_SCHEMA: &[u8] = b"adbc.connection.db_schema";
fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}
unsafe extern "C" fn release_error(error: *mut AdbcError) {
    // SAFETY: ADBC owns the error prefix and this callback owns its allocated message.
    unsafe {
        if !error.is_null() {
            duckdb_free((*error).message.cast());
            (*error).message = std::ptr::null_mut();
            (*error).release = None;
        }
    }
}
unsafe fn set_error(error: *mut AdbcError, message: impl AsRef<[u8]>) {
    // SAFETY: only the stable 1.0 error prefix is accessed. Message ownership follows release.
    unsafe {
        if error.is_null() {
            return;
        }
        let mut text = ByteString::new();
        if let Some(old) = opt_bytes((*error).message) {
            text.push_bytes(old);
            text.push(b'\n');
        }
        text.push_bytes(message);
        if let Some(release) = (*error).release {
            release(error);
        } else if !(*error).message.is_null() {
            duckdb_free((*error).message.cast());
        }
        (*error).message = c_string(text);
        (*error).release = Some(release_error);
    }
}
unsafe fn fail(error: *mut AdbcError, code: u8, message: impl AsRef<[u8]>) -> u8 {
    unsafe {
        set_error(error, message);
    }
    code
}
unsafe fn check(state: DuckdbState, error: *mut AdbcError, message: impl AsRef<[u8]>) -> u8 {
    if error.is_null() {
        return INVALID_ARGUMENT;
    }
    if state == DuckdbState::DuckDBSuccess {
        OK
    } else {
        unsafe { fail(error, INTERNAL, message) }
    }
}
unsafe fn option_string(text: &[u8], value: *mut c_char, length: *mut usize, error: *mut AdbcError) -> u8 {
    // SAFETY: caller supplies length and a value allocation of its declared capacity.
    unsafe {
        if length.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing length pointer");
        }
        let text = to_cstring(text);
        let required = text.as_bytes_with_nul().len();
        if !value.is_null() && *length >= required {
            std::ptr::copy_nonoverlapping(text.as_ptr(), value, required);
        }
        *length = required;
        OK
    }
}
#[derive(Default)]
struct DatabaseState {
    config: DuckdbConfig,
    database: DuckdbDatabase,
    path: ByteString,
    uri: Option<ByteString>,
    options: BTreeMap<ByteString, ByteString>,
}
impl Drop for DatabaseState {
    fn drop(&mut self) {
        unsafe {
            duckdb_close(&raw mut self.database);
            duckdb_destroy_config(&raw mut self.config);
        }
    }
}
#[derive(Default)]
struct ConnectionState {
    connection: AtomicPtr<crate::open::ConnectionWrapper>,
    options: Mutex<BTreeMap<ByteString, ByteString>>,
    streams: Mutex<Vec<Weak<Mutex<StreamState>>>>,
}
impl ConnectionState {
    fn handle(&self) -> DuckdbConnection {
        self.connection.load(Ordering::Acquire)
    }
    fn materialize(&self) {
        let mut streams = lock(&self.streams);
        streams.retain(|weak| {
            if let Some(stream) = weak.upgrade() {
                lock(&stream).materialize();
                true
            } else {
                false
            }
        });
    }
    fn auto_commit(&self) -> bool {
        let connection = self.handle();
        if connection.is_null() {
            return lock(&self.options).get(AUTOCOMMIT).is_none_or(|v| v == "true");
        }
        unsafe { crate::open::connection_ref(connection) }
            .is_none_or(|connection| connection.lock().is_auto_commit())
    }
}
impl Drop for ConnectionState {
    fn drop(&mut self) {
        let mut connection = self.connection.swap(std::ptr::null_mut(), Ordering::AcqRel);
        unsafe {
            duckdb_disconnect(&raw mut connection);
        }
    }
}
struct StatementState {
    connection: Option<Arc<ConnectionState>>,
    prepared: DuckdbPreparedStatement,
    input: ArrowArrayStream,
    ingest: options::IngestOptions,
}
impl Default for StatementState {
    fn default() -> Self {
        Self {
            connection: None,
            prepared: std::ptr::null_mut(),
            input: ArrowArrayStream::empty(),
            ingest: options::IngestOptions::default(),
        }
    }
}
impl Drop for StatementState {
    fn drop(&mut self) {
        unsafe {
            duckdb_destroy_prepare(&raw mut self.prepared);
            if let Some(release) = self.input.release {
                release(&raw mut self.input);
            }
        }
    }
}
unsafe fn database<'a>(value: *mut AdbcDatabase) -> Option<&'a mut DatabaseState> {
    unsafe { value.as_ref()?.private_data.cast::<DatabaseState>().as_mut() }
}
unsafe fn connection<'a>(value: *mut AdbcConnection) -> Option<&'a Arc<ConnectionState>> {
    unsafe {
        value
            .as_ref()?
            .private_data
            .cast::<Arc<ConnectionState>>()
            .as_ref()
    }
}
unsafe fn statement<'a>(value: *mut AdbcStatement) -> Option<&'a mut StatementState> {
    unsafe { value.as_ref()?.private_data.cast::<StatementState>().as_mut() }
}
unsafe extern "C" fn database_new(value: *mut AdbcDatabase, error: *mut AdbcError) -> u8 {
    // SAFETY: the caller owns a writable ADBC database and valid error prefix.
    unsafe {
        if value.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing database object");
        }
        let mut state = Box::<DatabaseState>::default();
        let status = duckdb_create_config(&raw mut state.config);
        (*value).private_data = Box::into_raw(state).cast();
        check(status, error, b"Failed to allocate")
    }
}
unsafe extern "C" fn database_set_option(
    value: *mut AdbcDatabase,
    key: *const c_char,
    input: *const c_char,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: handles and option strings follow ADBC ownership and C-string contracts.
    unsafe {
        if value.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing database object");
        }
        let Some(key_bytes) = opt_bytes(key) else {
            return fail(error, INVALID_ARGUMENT, b"Missing key");
        };
        let Some(state) = database(value) else {
            return fail(error, INVALID_ARGUMENT, b"Missing database object");
        };
        let bytes = opt_bytes(input).unwrap_or_default();
        match key_bytes {
            b"path" => state.path = bytes.into(),
            b"username" | b"password" => {
                return fail(error, NOT_IMPLEMENTED, b"DuckDB does not support authentication");
            }
            b"uri" => {
                let Some(mut path) = bytes
                    .strip_prefix(b"file:")
                    .or_else(|| bytes.strip_prefix(b"duckdb:"))
                else {
                    state.uri = Some(bytes.into());
                    return OK;
                };
                path = &path[..path
                    .iter()
                    .position(|b| matches!(b, b'?' | b'#'))
                    .unwrap_or(path.len())];
                if let Some(authority) = path.strip_prefix(b"//") {
                    let slash = authority.iter().position(|b| *b == b'/');
                    let host = &authority[..slash.unwrap_or(authority.len())];
                    match slash {
                        None => {
                            path = if host.is_empty() || host.eq_ignore_ascii_case(b"localhost") {
                                b""
                            } else {
                                host
                            }
                        }
                        Some(slash) => {
                            if !host.is_empty() && !host.eq_ignore_ascii_case(b"localhost") {
                                return fail(
                                    error,
                                    INVALID_ARGUMENT,
                                    b"URI with a non-empty authority is not supported",
                                );
                            }
                            path = &authority[slash..];
                        }
                    }
                }
                state.uri = Some(path.into());
            }
            _ => {
                let status = duckdb_set_config(state.config, key, input);
                if status == DuckdbState::DuckDBSuccess {
                    state.options.insert(key_bytes.into(), bytes.into());
                }
                return check(status, error, b"Failed to set configuration option");
            }
        }
        OK
    }
}
unsafe extern "C" fn database_init(value: *mut AdbcDatabase, error: *mut AdbcError) -> u8 {
    // SAFETY: initialized handles own the C API database/config, error receives malloc memory.
    unsafe {
        if error.is_null() {
            return INVALID_ARGUMENT;
        }
        if value.is_null() {
            return fail(error, INVALID_ARGUMENT, b"ADBC Database has an invalid pointer");
        }
        let Some(state) = database(value) else {
            return fail(error, INVALID_ARGUMENT, b"Invalid database");
        };
        let path = to_cstring(state.uri.as_ref().unwrap_or(&state.path));
        let mut message = std::ptr::null_mut();
        let status = duckdb_open_ext(
            path.as_ptr(),
            &raw mut state.database,
            state.config,
            &raw mut message,
        );
        let result = check(status, error, opt_bytes(message).unwrap_or_default());
        duckdb_free(message.cast());
        result
    }
}
unsafe extern "C" fn database_release(value: *mut AdbcDatabase, _error: *mut AdbcError) -> u8 {
    // SAFETY: an initialized private_data is a Box allocated by database_new, released once.
    unsafe {
        if !value.is_null() && !(*value).private_data.is_null() {
            drop(Box::from_raw((*value).private_data.cast::<DatabaseState>()));
            (*value).private_data = std::ptr::null_mut();
        }
        OK
    }
}
unsafe extern "C" fn database_get_option(
    value: *mut AdbcDatabase,
    key: *const c_char,
    out: *mut c_char,
    length: *mut usize,
    error: *mut AdbcError,
) -> u8 {
    unsafe {
        let Some(state) = database(value) else {
            return fail(error, INVALID_ARGUMENT, b"Missing database object");
        };
        let Some(key) = opt_bytes(key) else {
            return fail(error, INVALID_ARGUMENT, b"Missing key");
        };
        let found = match key {
            b"path" => Some(&state.path),
            b"uri" => state.uri.as_ref(),
            b"username" | b"password" => {
                return fail(error, NOT_IMPLEMENTED, b"DuckDB does not support authentication");
            }
            _ => state.options.get(key),
        };
        match found {
            Some(text) => option_string(text, out, length, error),
            None => fail(error, NOT_FOUND, byte_format!("Option not found: {key}")),
        }
    }
}
unsafe extern "C" fn database_set_int(
    value: *mut AdbcDatabase,
    key: *const c_char,
    input: i64,
    error: *mut AdbcError,
) -> u8 {
    let text = to_cstring(input.to_string());
    unsafe { database_set_option(value, key, text.as_ptr(), error) }
}
unsafe extern "C" fn database_set_double(
    value: *mut AdbcDatabase,
    key: *const c_char,
    input: f64,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: libc formats a double with the same %f conversion used by std::to_string.
    // The two-pass allocation holds the complete native output, including the terminator.
    unsafe {
        let required = libc::snprintf(std::ptr::null_mut(), 0, c"%f".as_ptr(), input);
        if required < 0 {
            return fail(error, INTERNAL, b"Failed to format double option");
        }
        let mut text = vec![0 as c_char; required as usize + 1];
        if libc::snprintf(text.as_mut_ptr(), text.len(), c"%f".as_ptr(), input) < 0 {
            return fail(error, INTERNAL, b"Failed to format double option");
        }
        database_set_option(value, key, text.as_ptr(), error)
    }
}
unsafe extern "C" fn connection_new(value: *mut AdbcConnection, error: *mut AdbcError) -> u8 {
    unsafe {
        if value.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing connection object");
        }
        (*value).private_data = Box::into_raw(Box::new(Arc::new(ConnectionState::default()))).cast();
        OK
    }
}
unsafe fn execute_connection(connection: &ConnectionState, sql: &[u8], error: *mut AdbcError) -> u8 {
    unsafe {
        connection.materialize();
        let mut result = empty_result();
        let sql = to_cstring(sql);
        let status = duckdb_query(connection.handle(), sql.as_ptr(), &raw mut result);
        let message = opt_bytes(duckdb_result_error(&raw mut result)).unwrap_or_default();
        let code = if status == DuckdbState::DuckDBSuccess {
            OK
        } else {
            fail(
                error,
                INTERNAL,
                byte_format!("Failed to execute query \"{}\": {message}", sql.as_bytes()),
            )
        };
        duckdb_destroy_result(&raw mut result);
        code
    }
}
unsafe fn apply_autocommit(connection: &ConnectionState, enabled: bool, error: *mut AdbcError) -> u8 {
    if enabled && !connection.auto_commit() {
        unsafe { execute_connection(connection, b"COMMIT", error) }
    } else if !enabled && connection.auto_commit() {
        unsafe { execute_connection(connection, b"START TRANSACTION", error) }
    } else {
        OK
    }
}
unsafe extern "C" fn connection_init(
    value: *mut AdbcConnection,
    db: *mut AdbcDatabase,
    error: *mut AdbcError,
) -> u8 {
    unsafe {
        if db.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing database object");
        }
        let Some(db) = database(db) else {
            return fail(error, INVALID_ARGUMENT, b"Invalid database");
        };
        if value.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing connection object");
        }
        let Some(state) = connection(value) else {
            return fail(error, INVALID_ARGUMENT, b"Invalid connection object");
        };
        let mut handle = std::ptr::null_mut();
        let status = duckdb_connect(db.database, &raw mut handle);
        state.connection.store(handle, Ordering::Release);
        let status = check(status, error, b"Failed to connect to Database");
        if status != OK {
            return status;
        }
        let enabled = lock(&state.options).get(AUTOCOMMIT).is_none_or(|v| v == "true");
        let result = apply_autocommit(state, enabled, error);
        lock(&state.options).clear();
        result
    }
}
unsafe extern "C" fn connection_release(value: *mut AdbcConnection, _error: *mut AdbcError) -> u8 {
    unsafe {
        if !value.is_null() && !(*value).private_data.is_null() {
            let state = Box::from_raw((*value).private_data.cast::<Arc<ConnectionState>>());
            state.materialize();
            let mut handle = state.connection.swap(std::ptr::null_mut(), Ordering::AcqRel);
            duckdb_disconnect(&raw mut handle);
            drop(state);
            (*value).private_data = std::ptr::null_mut();
        }
        OK
    }
}
unsafe extern "C" fn connection_set_option(
    value: *mut AdbcConnection,
    key: *const c_char,
    input: *const c_char,
    error: *mut AdbcError,
) -> u8 {
    unsafe {
        if value.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Connection is not set");
        }
        let Some(input) = opt_bytes(input) else {
            return fail(error, INVALID_ARGUMENT, b"Option value must not be NULL");
        };
        let Some(key) = opt_bytes(key) else {
            return fail(error, INVALID_ARGUMENT, b"Missing key");
        };
        let Some(state) = connection(value) else {
            return fail(error, INVALID_ARGUMENT, b"Connection is not set");
        };
        if key == AUTOCOMMIT {
            if input != b"true" && input != b"false" {
                return fail(
                    error,
                    INVALID_ARGUMENT,
                    byte_format!("Invalid connection option value {key}={input}"),
                );
            }
            lock(&state.options).insert(key.into(), input.into());
            if state.handle().is_null() {
                return OK;
            }
            let result = apply_autocommit(state, input == b"true", error);
            lock(&state.options).clear();
            return result;
        }
        if key == CURRENT_CATALOG || key == CURRENT_SCHEMA {
            if state.handle().is_null() {
                return fail(error, INVALID_STATE, b"Connection is not initialized");
            }
            let prefix = if key == CURRENT_CATALOG {
                b"USE ".as_slice()
            } else {
                b"SET schema = "
            };
            return execute_connection(
                state,
                &byte_format!(
                    "{prefix}{}",
                    roastedduck_ast::keyword::write_optionally_quoted_bytes(input)
                ),
                error,
            );
        }
        fail(
            error,
            NOT_IMPLEMENTED,
            byte_format!("Unknown connection option {key}={input}"),
        )
    }
}
unsafe extern "C" fn connection_get_option(
    value: *mut AdbcConnection,
    key: *const c_char,
    out: *mut c_char,
    length: *mut usize,
    error: *mut AdbcError,
) -> u8 {
    unsafe {
        let Some(state) = connection(value) else {
            return fail(error, INVALID_ARGUMENT, b"Connection is not set");
        };
        let Some(key) = opt_bytes(key) else {
            return fail(error, INVALID_ARGUMENT, b"Missing key");
        };
        if key == AUTOCOMMIT {
            return option_string(
                if state.auto_commit() { b"true" } else { b"false" },
                out,
                length,
                error,
            );
        }
        if key == CURRENT_CATALOG || key == CURRENT_SCHEMA {
            return metadata::current_option(value, key == CURRENT_CATALOG, out, length, error);
        }
        fail(error, NOT_FOUND, byte_format!("Option not found: {key}"))
    }
}
unsafe extern "C" fn connection_get_int(
    value: *mut AdbcConnection,
    key: *const c_char,
    out: *mut i64,
    error: *mut AdbcError,
) -> u8 {
    unsafe {
        let Some(state) = connection(value) else {
            return fail(error, INVALID_ARGUMENT, b"Connection is not set");
        };
        let Some(key) = opt_bytes(key) else {
            return fail(error, INVALID_ARGUMENT, b"Missing key");
        };
        if key == AUTOCOMMIT && !out.is_null() {
            *out = i64::from(state.auto_commit());
            OK
        } else {
            fail(error, NOT_FOUND, byte_format!("Option not found: {key}"))
        }
    }
}
unsafe extern "C" fn connection_set_int(
    value: *mut AdbcConnection,
    key: *const c_char,
    input: i64,
    error: *mut AdbcError,
) -> u8 {
    unsafe {
        let Some(bytes) = opt_bytes(key) else {
            return fail(error, INVALID_ARGUMENT, b"Missing key");
        };
        let text = if bytes == AUTOCOMMIT {
            to_cstring(if input == 0 { b"false".as_slice() } else { b"true" })
        } else {
            to_cstring(input.to_string())
        };
        connection_set_option(value, key, text.as_ptr(), error)
    }
}
unsafe fn transaction(value: *mut AdbcConnection, commit: bool, error: *mut AdbcError) -> u8 {
    unsafe {
        let Some(state) = connection(value) else {
            return fail(error, INVALID_ARGUMENT, b"Connection is not set");
        };
        if state.auto_commit() {
            return fail(
                error,
                INVALID_STATE,
                if commit {
                    b"No active transaction, cannot commit".as_slice()
                } else {
                    b"No active transaction, cannot rollback"
                },
            );
        }
        let code = execute_connection(state, if commit { b"COMMIT" } else { b"ROLLBACK" }, error);
        if code == OK {
            execute_connection(state, b"START TRANSACTION", error)
        } else {
            code
        }
    }
}
unsafe extern "C" fn connection_commit(value: *mut AdbcConnection, error: *mut AdbcError) -> u8 {
    unsafe { transaction(value, true, error) }
}
unsafe extern "C" fn connection_rollback(value: *mut AdbcConnection, error: *mut AdbcError) -> u8 {
    unsafe { transaction(value, false, error) }
}
unsafe extern "C" fn connection_cancel(value: *mut AdbcConnection, error: *mut AdbcError) -> u8 {
    unsafe {
        if value.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing connection object");
        }
        let Some(state) = connection(value) else {
            return fail(error, INVALID_ARGUMENT, b"Connection is invalid");
        };
        let handle = state.handle();
        if handle.is_null() {
            return fail(error, INVALID_STATE, b"Connection is not initialized");
        }
        duckdb_interrupt(handle);
        OK
    }
}

fn empty_result() -> DuckdbResult {
    DuckdbResult {
        deprecated_column_count: 0,
        deprecated_row_count: 0,
        deprecated_rows_changed: 0,
        deprecated_columns: std::ptr::null_mut(),
        deprecated_error_message: std::ptr::null_mut(),
        internal_data: std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests;

/// Native handle bridge for the upstream test helpers, which otherwise cast C++ private state.
#[cfg(feature = "adbc-test-api")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn roastedduck_adbc_test_connection(value: *mut AdbcConnection) -> DuckdbConnection {
    // SAFETY: test harness passes a live ADBC connection and uses the returned borrow before releasing it.
    unsafe { connection(value).map_or(std::ptr::null_mut(), |state| state.handle()) }
}

/// Database handle for creating the independent Arrow-comparison connection in tests.
#[cfg(feature = "adbc-test-api")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn roastedduck_adbc_test_database(value: *mut AdbcDatabase) -> DuckdbDatabase {
    // SAFETY: the test harness keeps the ADBC database alive for the returned borrow.
    unsafe { database(value).map_or(std::ptr::null_mut(), |state| state.database) }
}

/// Mirrors the frozen private test initializer, including its empty release callback and legacy vendor code.
#[cfg(feature = "adbc-test-api")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn roastedduck_adbc_test_initialize_error(error: *mut AdbcError) {
    // SAFETY: tests supply a complete error; unrecognized callbacks are never invoked.
    unsafe {
        if error.is_null() {
            return;
        }
        if (*error)
            .release
            .is_some_and(|f| std::ptr::fn_addr_eq(f, release_error as unsafe extern "C" fn(*mut AdbcError)))
        {
            release_error(error);
        } else {
            errors::release_known(error);
        }
        (*error).message = std::ptr::null_mut();
        (*error).release = Some(release_error);
        (*error).sqlstate = [0; 5];
        (*error).vendor_code = -1;
    }
}
