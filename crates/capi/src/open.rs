//! Databases, connections and the database instance cache (DuckDB's `capi/duckdb-c.cpp`).

use std::collections::HashMap;
use std::ffi::c_char;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use rustdb::{ClientHandle, Config, Connection, Database, LogicalType, Value};
use rustdb_function::FunctionClient;

use crate::config::{DuckdbConfig, capi_config};
use crate::helper::{c_string, catch};
use crate::result::translate_result;
use crate::types::*;
use crate::value::{DuckdbValue, wrap_value};

/// What a `duckdb_database` points to (DuckDB's `DatabaseWrapper`): a handle on the
/// database, shared with the other handles the instance cache gave out for it.
#[derive(Debug)]
pub struct DatabaseWrapper {
    pub(crate) database: Arc<Database>,
}

/// `duckdb_database`
pub type DuckdbDatabase = *mut DatabaseWrapper;

/// A connection shared by the objects made from it (DuckDB's `ClientContext`, which
/// prepared statements, results and appenders hold on to): the connection, used by one
/// thread at a time, the handle that interrupts its query from any thread, and the state
/// its functions see, which callbacks reach while it runs a query.
#[derive(Debug)]
pub(crate) struct SharedConnection {
    connection: Mutex<Connection>,
    client: Arc<ClientHandle>,
    function_client: Arc<FunctionClient>,
}

impl SharedConnection {
    pub(crate) fn new(connection: Connection) -> Arc<Self> {
        Arc::new(Self {
            client: connection.client_handle(),
            function_client: connection.function_client(),
            connection: Mutex::new(connection),
        })
    }

    /// The connection, for the calling thread (`ClientContext::LockContext`).
    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.connection.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn client(&self) -> &ClientHandle {
        &self.client
    }

    pub(crate) fn function_client(&self) -> &Arc<FunctionClient> {
        &self.function_client
    }
}

/// What a `duckdb_connection` points to (DuckDB's `Connection`).
#[derive(Debug)]
pub struct ConnectionWrapper {
    pub(crate) context: Arc<SharedConnection>,
    /// The profile `duckdb_get_profiling_info` handed out last, which its pointers read.
    pub(crate) profiling: Mutex<Option<Arc<rustdb::ProfilingTree>>>,
}

/// `duckdb_connection`
pub type DuckdbConnection = *mut ConnectionWrapper;

/// The connection behind a handle.
///
/// # Safety
/// `connection` must be NULL or a live `duckdb_connection`.
pub(crate) unsafe fn connection_ref<'a>(connection: DuckdbConnection) -> Option<&'a Arc<SharedConnection>> {
    // SAFETY: guaranteed by the caller.
    unsafe { connection.as_ref() }.map(|wrapper| &wrapper.context)
}

/// What a `duckdb_client_context` points to (DuckDB's `CClientContextWrapper`): the
/// connection's state as functions see it, which does not lock the connection, so
/// callbacks of the query it runs can use it.
#[derive(Debug)]
pub struct ClientContextWrapper {
    pub(crate) client: Arc<FunctionClient>,
}

/// Hands a client context to C (`new CClientContextWrapper(context)`).
pub(crate) fn wrap_client_context(client: &Arc<FunctionClient>) -> DuckdbClientContext {
    Box::into_raw(Box::new(ClientContextWrapper {
        client: Arc::clone(client),
    }))
}

/// The connection state behind a client context handle.
///
/// # Safety
/// `context` must be NULL or a live `duckdb_client_context`.
pub(crate) unsafe fn client_context_ref<'a>(context: DuckdbClientContext) -> Option<&'a Arc<FunctionClient>> {
    // SAFETY: guaranteed by the caller.
    unsafe { context.as_ref() }.map(|wrapper| &wrapper.client)
}

/// `duckdb_client_context`
pub type DuckdbClientContext = *mut ClientContextWrapper;

/// The databases opened through an instance cache, by canonical path (DuckDB's
/// `DBInstanceCache`).
#[derive(Debug, Default)]
pub struct InstanceCacheWrapper {
    instances: Mutex<HashMap<rustdb_common::ByteString, (Weak<Database>, Config)>>,
}

/// `duckdb_instance_cache`
pub type DuckdbInstanceCache = *mut InstanceCacheWrapper;

impl InstanceCacheWrapper {
    /// The database at `path`, opened before with the same configuration and still open,
    /// or opened now (`DBInstanceCache::GetOrCreateInstance`). Unnamed in-memory databases
    /// are never shared.
    fn get_or_create(&self, path: &[u8], config: &Config) -> rustdb::Result<Arc<Database>> {
        let key = cache_key(path);
        let mut instances = self.instances.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(key) = &key
            && let Some((database, existing)) = instances.get(key)
            && let Some(database) = database.upgrade()
        {
            if existing != config {
                return Err(rustdb::Error::new(
                    rustdb::ErrorKind::Connection,
                    "Can't open a connection to same database file with a different configuration than \
                     existing connections",
                ));
            }
            return Ok(database);
        }
        // A named in-memory database (`:memory:name`) is an in-memory instance the cache
        // shares; opened directly, the path would name a file.
        let instance_path = if path.starts_with(b":memory:") {
            b":memory:".as_slice()
        } else {
            path
        };
        let database = Arc::new(Database::open(instance_path, config)?);
        if let Some(key) = key {
            instances.insert(key, (Arc::downgrade(&database), config.clone()));
        }
        Ok(database)
    }
}

/// The cache key of a database path (`GetCacheKey`): the canonical absolute path,
/// lower-cased on Apple platforms and Windows; `None` for an unnamed in-memory database.
fn cache_key(path: &[u8]) -> Option<rustdb_common::ByteString> {
    if path.is_empty() || path == b":memory:" {
        return None;
    }
    if path.starts_with(b":memory:") {
        return Some(if cfg!(any(target_vendor = "apple", windows)) {
            rustdb_common::ByteString::from(path).to_ascii_lowercase()
        } else {
            path.into()
        });
    }
    let expanded = if let Some(rest) = path.strip_prefix(b"~/") {
        match std::env::var_os("HOME") {
            Some(home) => Path::new(&home).join(rustdb_common::native_file_path(rest).ok()?),
            None => rustdb_common::native_file_path(path).ok()?,
        }
    } else {
        rustdb_common::native_file_path(path).ok()?
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir().unwrap_or_default().join(expanded)
    };
    let mut canonical = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                canonical.pop();
            }
            other => canonical.push(other),
        }
    }
    let key = rustdb_common::ByteString::from(canonical.as_os_str().as_encoded_bytes());
    Some(if cfg!(any(target_vendor = "apple", windows)) {
        key.to_ascii_lowercase()
    } else {
        key
    })
}

/// `duckdb_instance_cache duckdb_create_instance_cache()` (capi/duckdb-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_instance_cache() -> DuckdbInstanceCache {
    Box::into_raw(Box::default())
}

/// `void duckdb_destroy_instance_cache(duckdb_instance_cache *instance_cache)` (capi/duckdb-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_instance_cache(instance_cache: *mut DuckdbInstanceCache) {
    // SAFETY: `instance_cache` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = instance_cache.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `duckdb_open_internal` (capi/duckdb-c.cpp): opens the database at `path` with `config`
/// (or a default configuration marked as the C API's), through `cache` if given.
///
/// # Safety
/// `path` must be NULL or a C string; `out` must be writable; `config` NULL or a live
/// configuration; `out_error` NULL or writable.
unsafe fn open_internal(
    cache: Option<&InstanceCacheWrapper>,
    path: *const c_char,
    out: *mut DuckdbDatabase,
    config: DuckdbConfig,
    out_error: *mut *mut c_char,
) -> DuckdbState {
    crate::register_extension_loader();
    let default_config;
    // SAFETY: `config` is NULL or a live handle.
    let config = if let Some(wrapper) = unsafe { config.as_ref() } {
        &wrapper.config
    } else {
        default_config = capi_config();
        &default_config
    };
    // SAFETY: `path` is NULL or a C string.
    let path = unsafe { crate::helper::opt_bytes(path) }.unwrap_or_default();
    let opened = catch(|| match cache {
        Some(cache) => cache.get_or_create(path, config),
        None => Database::open(path, config).map(Arc::new),
    });
    match opened {
        Ok(database) => {
            if !out.is_null() {
                // SAFETY: `out` is writable per the API contract.
                unsafe { *out = Box::into_raw(Box::new(DatabaseWrapper { database })) };
            }
            DuckdbState::DuckDBSuccess
        }
        Err(error) => {
            if !out_error.is_null() {
                // SAFETY: `out_error` is writable per the API contract.
                unsafe { *out_error = c_string(error.to_bytes()) };
            }
            DuckdbState::DuckDBError
        }
    }
}

/// `duckdb_state duckdb_get_or_create_from_cache(duckdb_instance_cache instance_cache,
/// const char *path, duckdb_database *out_database, duckdb_config config, char **out_error)`
/// (capi/duckdb-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_or_create_from_cache(
    instance_cache: DuckdbInstanceCache,
    path: *const c_char,
    out_database: *mut DuckdbDatabase,
    config: DuckdbConfig,
    out_error: *mut *mut c_char,
) -> DuckdbState {
    // SAFETY: `instance_cache` is NULL or a live handle.
    let Some(cache) = (unsafe { instance_cache.as_ref() }) else {
        if !out_error.is_null() {
            // SAFETY: `out_error` is writable per the API contract.
            unsafe { *out_error = c_string("instance cache cannot be nullptr") };
        }
        return DuckdbState::DuckDBError;
    };
    // SAFETY: forwarded API contract.
    unsafe { open_internal(Some(cache), path, out_database, config, out_error) }
}

/// `duckdb_state duckdb_open_ext(const char *path, duckdb_database *out_database,
/// duckdb_config config, char **out_error)` (capi/duckdb-c.cpp): an empty or NULL path,
/// or `:memory:`, is an in-memory database; the error message is allocated with
/// `duckdb_malloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_open_ext(
    path: *const c_char,
    out_database: *mut DuckdbDatabase,
    config: DuckdbConfig,
    out_error: *mut *mut c_char,
) -> DuckdbState {
    // SAFETY: forwarded API contract.
    unsafe { open_internal(None, path, out_database, config, out_error) }
}

/// `duckdb_state duckdb_open(const char *path, duckdb_database *out_database)` (capi/duckdb-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_open(path: *const c_char, out_database: *mut DuckdbDatabase) -> DuckdbState {
    // SAFETY: forwarded API contract.
    unsafe { duckdb_open_ext(path, out_database, std::ptr::null_mut(), std::ptr::null_mut()) }
}

/// `void duckdb_close(duckdb_database *database)` (capi/duckdb-c.cpp): the database closes
/// once its connections and other handles are gone.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_close(database: *mut DuckdbDatabase) {
    // SAFETY: `database` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = database.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `duckdb_state duckdb_connect(duckdb_database database, duckdb_connection *out_connection)`
/// (capi/duckdb-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_connect(
    database: DuckdbDatabase,
    out_connection: *mut DuckdbConnection,
) -> DuckdbState {
    // SAFETY: `database` is NULL or a live handle.
    let Some(wrapper) = (unsafe { database.as_ref() }) else {
        return DuckdbState::DuckDBError;
    };
    if out_connection.is_null() {
        return DuckdbState::DuckDBError;
    }
    match catch(|| wrapper.database.connect()) {
        Ok(connection) => {
            let context = SharedConnection::new(connection);
            let wrapper = ConnectionWrapper {
                context,
                profiling: Mutex::default(),
            };
            // SAFETY: checked non-null above.
            unsafe { *out_connection = Box::into_raw(Box::new(wrapper)) };
            DuckdbState::DuckDBSuccess
        }
        Err(_) => DuckdbState::DuckDBError,
    }
}

/// `void duckdb_interrupt(duckdb_connection connection)` (capi/duckdb-c.cpp): the running
/// query fails with `INTERRUPT Error: Interrupted!`; callable from any thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_interrupt(connection: DuckdbConnection) {
    // SAFETY: `connection` is NULL or a live handle.
    if let Some(context) = unsafe { connection_ref(connection) } {
        context.client().interrupt();
    }
}

/// `duckdb_query_progress_type duckdb_query_progress(duckdb_connection connection)`
/// (capi/duckdb-c.cpp): a percentage of -1 when no query runs or the progress bar is off.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_query_progress(connection: DuckdbConnection) -> DuckdbQueryProgressType {
    // SAFETY: `connection` is NULL or a live handle.
    let Some(context) = (unsafe { connection_ref(connection) }) else {
        return DuckdbQueryProgressType {
            percentage: -1.0,
            rows_processed: 0,
            total_rows_to_process: 0,
        };
    };
    let progress = context.client().query_progress();
    DuckdbQueryProgressType {
        percentage: progress.percentage,
        rows_processed: progress.rows_processed,
        total_rows_to_process: progress.total_rows_to_process,
    }
}

/// `void duckdb_disconnect(duckdb_connection *connection)` (capi/duckdb-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_disconnect(connection: *mut DuckdbConnection) {
    // SAFETY: `connection` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = connection.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `void duckdb_connection_get_client_context(duckdb_connection connection,
/// duckdb_client_context *out_context)` (capi/duckdb-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_connection_get_client_context(
    connection: DuckdbConnection,
    out_context: *mut DuckdbClientContext,
) {
    // SAFETY: `connection` is NULL or a live handle.
    let Some(context) = (unsafe { connection_ref(connection) }) else {
        return;
    };
    if out_context.is_null() {
        return;
    }
    // SAFETY: checked non-null above.
    unsafe { *out_context = wrap_client_context(context.function_client()) };
}

/// `idx_t duckdb_client_context_get_connection_id(duckdb_client_context context)`
/// (capi/duckdb-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_client_context_get_connection_id(context: DuckdbClientContext) -> Idx {
    // SAFETY: `context` is NULL or a live handle.
    unsafe { client_context_ref(context) }.map_or(0, |client| client.connection_id())
}

/// `void duckdb_destroy_client_context(duckdb_client_context *context)` (capi/duckdb-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_client_context(context: *mut DuckdbClientContext) {
    // SAFETY: `context` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = context.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `duckdb_state duckdb_query(duckdb_connection connection, const char *query,
/// duckdb_result *out_result)` (capi/duckdb-c.cpp): runs every statement of the query; the
/// result is that of the last one, or of the first that failed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_query(
    connection: DuckdbConnection,
    query: *const c_char,
    out_result: *mut DuckdbResult,
) -> DuckdbState {
    // SAFETY: `connection` is NULL or a live handle.
    let Some(context) = (unsafe { connection_ref(connection) }) else {
        return DuckdbState::DuckDBError;
    };
    // SAFETY: `query` is NULL or a C string.
    let Some(sql) = (unsafe { crate::helper::opt_bytes(query) }) else {
        return DuckdbState::DuckDBError;
    };
    let (outcome, properties) = {
        let mut connection = context.lock();
        let outcome = catch(|| connection.query(sql)).map(rustdb::QueryResult::Materialized);
        (outcome, connection.client_properties())
    };
    // SAFETY: `out_result` is NULL or writable.
    unsafe { translate_result(context, outcome, properties, out_result) }
}

/// `duckdb_value duckdb_get_table_names(duckdb_connection connection, const char *query,
/// bool qualified)` (capi/duckdb-c.cpp): a `VARCHAR[]` of the tables the single statement
/// in `query` uses, or NULL when it cannot be bound.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_table_names(
    connection: DuckdbConnection,
    query: *const c_char,
    qualified: bool,
) -> DuckdbValue {
    // SAFETY: `connection` is NULL or a live handle; `query` is NULL or a C string.
    let (Some(context), Some(sql)) =
        (unsafe { (connection_ref(connection), crate::helper::opt_bytes(query)) })
    else {
        return std::ptr::null_mut();
    };
    match catch(|| {
        let names = context.lock().table_names(sql, qualified)?;
        Ok(Value::list(
            LogicalType::VARCHAR,
            names
                .into_iter()
                .map(|name| Value::try_varchar(&name))
                .collect::<rustdb::Result<_>>()?,
        ))
    }) {
        Ok(value) => wrap_value(value),
        Err(_) => std::ptr::null_mut(),
    }
}

/// `const char *duckdb_library_version()` (capi/duckdb-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_library_version() -> *const c_char {
    c"v1.5.6".as_ptr()
}
