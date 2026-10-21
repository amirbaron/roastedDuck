//! Connection state reachable from function callbacks (the parts of DuckDB's
//! `ClientContext` that functions use).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use rustdb_common::Result;
use rustdb_types::Value;

use crate::random::RandomEngine;

/// One configuration option as `duckdb_settings()` lists it.
#[derive(Clone, Debug)]
pub struct SettingInfo {
    pub name: String,
    /// The current value (`Value::ToString`).
    pub value: String,
    pub description: String,
    pub input_type: String,
    /// `GLOBAL` (else `LOCAL`) scope.
    pub global: bool,
    pub aliases: Vec<String>,
}

/// Checks a new value of an extension option and may normalise it (DuckDB's
/// `set_option_callback_t`).
pub type ExtensionOptionSet = std::sync::Arc<dyn Fn(&mut Value) -> Result<()> + Send + Sync>;

/// A configuration option an extension adds (DuckDB's `ExtensionOption`, registered with
/// `DBConfig::AddExtensionOption`); its scope defaults to the session.
#[derive(Clone)]
pub struct ExtensionOption {
    pub name: String,
    pub description: String,
    pub input_type: rustdb_types::LogicalType,
    pub default_value: Value,
    pub set_function: Option<ExtensionOptionSet>,
}

impl std::fmt::Debug for ExtensionOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionOption")
            .field("name", &self.name)
            .field("default_value", &self.default_value)
            .finish_non_exhaustive()
    }
}

/// The connection's configuration as functions see it (DuckDB's
/// `ClientContext::TryGetCurrentSetting` and `DBConfig::GetOptions`).
pub trait SettingsSource: Send + Sync + std::fmt::Debug {
    /// The current value of option `name` (case-insensitive), or DuckDB's unrecognized
    /// option error.
    fn current_setting(&self, name: &str) -> Result<Value>;
    /// Every option with its current value.
    fn settings(&self) -> Vec<SettingInfo>;
}

/// Objects a database keeps for its lifetime, by key (DuckDB's `ObjectCache`), such as
/// the parsed footers of Parquet files.
#[derive(Default)]
pub struct ObjectCache {
    entries: Mutex<std::collections::HashMap<String, Arc<dyn std::any::Any + Send + Sync>>>,
}

impl std::fmt::Debug for ObjectCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectCache").finish_non_exhaustive()
    }
}

impl ObjectCache {
    /// The object stored under `key`, if it has type `T`.
    pub fn get<T: Send + Sync + 'static>(&self, key: &str) -> Option<Arc<T>> {
        let entry = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned()?;
        entry.downcast::<T>().ok()
    }

    /// Stores `value` under `key`, replacing what was there.
    pub fn put<T: Send + Sync + 'static>(&self, key: String, value: Arc<T>) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, value);
    }
}

/// A change of the connection's own configuration that a function makes when it runs
/// (DuckDB's functions change the `ClientConfig` directly; ours hand the change to the
/// connection, which applies it once the function has run).
#[derive(Clone, Debug, PartialEq)]
pub enum ClientRequest {
    /// `enable_profiling(...)` (`EnableProfiling`): the options given.
    EnableProfiling {
        format: Option<String>,
        coverage: Option<String>,
        save_location: Option<String>,
        mode: Option<String>,
        metrics: Option<Value>,
    },
    /// `disable_profiling()` (`DisableProfiling`).
    DisableProfiling,
    /// Sets a database-wide option (`DBConfig::SetOptionByName`), e.g. the
    /// `allow_parser_override_extension` of `enable_peg_parser()`.
    SetGlobalOption { name: String, value: Value },
}

/// Connection ids handed out so far (DuckDB's `ConnectionManager` counter).
static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(0);

/// Per-connection state shared by the functions a connection runs.
#[derive(Debug)]
pub struct FunctionClient {
    random: Mutex<RandomEngine>,
    /// The id of the current transaction and its start time in microseconds since the
    /// epoch (DuckDB's `MetaTransaction::start_timestamp`).
    transaction: Mutex<Option<(u64, i64)>>,
    /// The text of the query being run (DuckDB's `ClientContext::GetCurrentQuery`).
    query: Mutex<String>,
    /// The number of statements run so far (DuckDB's query id).
    query_id: AtomicU64,
    /// The default database (DuckDB's `DatabaseManager::GetDefaultDatabase`).
    database: Mutex<String>,
    connection_id: u64,
    /// The configuration of the statement being run.
    settings: Mutex<Option<Arc<dyn SettingsSource>>>,
    /// The number of open connections of the database (DuckDB's
    /// `ConnectionManager::GetConnectionCount`), shared by its connections.
    connections: Mutex<Option<Arc<AtomicU64>>>,
    /// The connection's loggers (DuckDB's `ClientContext::GetLogger`).
    logger: Mutex<Option<Arc<dyn crate::logging::ClientLogger>>>,
    /// The database's object cache (`ObjectCache::GetObjectCache`).
    object_cache: Mutex<Option<Arc<ObjectCache>>>,
    /// Configuration changes functions made, not yet applied by the connection.
    client_requests: Mutex<Vec<ClientRequest>>,
}

impl Default for FunctionClient {
    fn default() -> Self {
        Self {
            random: Mutex::default(),
            transaction: Mutex::default(),
            query: Mutex::default(),
            query_id: AtomicU64::new(0),
            database: Mutex::new("memory".to_owned()),
            connection_id: NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed),
            settings: Mutex::default(),
            connections: Mutex::default(),
            logger: Mutex::default(),
            object_cache: Mutex::default(),
            client_requests: Mutex::default(),
        }
    }
}

impl FunctionClient {
    /// The connection's random engine (DuckDB's `RandomEngine::Get(context)` with its
    /// lock held).
    pub fn random_engine(&self) -> MutexGuard<'_, RandomEngine> {
        // A poisoned lock only means another thread panicked mid-draw; the engine state
        // is still a valid generator state.
        self.random.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records that statements now run in transaction `id`; a new transaction starts its
    /// clock now.
    pub fn observe_transaction(&self, id: u64) {
        let mut transaction = self.transaction.lock().unwrap_or_else(PoisonError::into_inner);
        if transaction.is_none_or(|(current, _)| current != id) {
            *transaction = Some((id, now_micros()));
        }
    }

    /// The id of the current transaction (0 if none was observed).
    pub fn transaction_id(&self) -> u64 {
        self.transaction
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .map_or(0, |(id, _)| id)
    }

    /// Records the text of the statement about to run.
    pub fn set_query(&self, query: &str) {
        query.clone_into(&mut self.query.lock().unwrap_or_else(PoisonError::into_inner));
        self.query_id.fetch_add(1, Ordering::Relaxed);
    }

    /// The text of the running query (`current_query()`).
    pub fn query(&self) -> String {
        self.query.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The id of the running query (`current_query_id()`).
    pub fn query_id(&self) -> u64 {
        self.query_id.load(Ordering::Relaxed)
    }

    /// Sets the default database name.
    pub fn set_database(&self, name: &str) {
        name.clone_into(&mut self.database.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// The default database (`current_database()`).
    pub fn database(&self) -> String {
        self.database
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Sets the configuration seen by the next statements.
    pub fn set_settings(&self, settings: Arc<dyn SettingsSource>) {
        *self.settings.lock().unwrap_or_else(PoisonError::into_inner) = Some(settings);
    }

    /// Sets the connection's loggers.
    pub fn set_logger(&self, logger: Arc<dyn crate::logging::ClientLogger>) {
        *self.logger.lock().unwrap_or_else(PoisonError::into_inner) = Some(logger);
    }

    /// The connection's loggers, if the database logs.
    pub fn logger(&self) -> Option<Arc<dyn crate::logging::ClientLogger>> {
        self.logger.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Hands a configuration change to the connection.
    pub fn push_client_request(&self, request: ClientRequest) {
        self.client_requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
    }

    /// The configuration changes functions made since the last call, in order.
    pub fn take_client_requests(&self) -> Vec<ClientRequest> {
        std::mem::take(
            &mut *self
                .client_requests
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Shares the database's object cache with this connection.
    pub fn set_object_cache(&self, cache: Arc<ObjectCache>) {
        *self.object_cache.lock().unwrap_or_else(PoisonError::into_inner) = Some(cache);
    }

    /// The database's object cache, if the connection belongs to a database.
    pub fn object_cache(&self) -> Option<Arc<ObjectCache>> {
        self.object_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Shares the database's open-connection counter with this connection.
    pub fn set_connection_counter(&self, counter: Arc<AtomicU64>) {
        *self.connections.lock().unwrap_or_else(PoisonError::into_inner) = Some(counter);
    }

    /// The number of open connections of the database, if it is tracked.
    pub fn connection_count(&self) -> Option<u64> {
        self.connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|counter| counter.load(Ordering::Relaxed))
    }

    /// The configuration of the current statement, if the connection provided one.
    pub fn settings(&self) -> Option<Arc<dyn SettingsSource>> {
        self.settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The connection id (`current_connection_id()`).
    pub fn connection_id(&self) -> u64 {
        self.connection_id
    }

    /// The start time of the current transaction in microseconds since the epoch
    /// (now, if no transaction was observed).
    pub fn transaction_start_micros(&self) -> i64 {
        self.transaction
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .map_or_else(now_micros, |(_, start)| start)
    }
}

/// The wall clock in microseconds since the epoch (DuckDB's
/// `Timestamp::GetCurrentTimestamp`).
fn now_micros() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
        i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
    })
}
