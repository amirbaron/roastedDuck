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

/// The connection's configuration as functions see it (DuckDB's
/// `ClientContext::TryGetCurrentSetting` and `DBConfig::GetOptions`).
pub trait SettingsSource: Send + Sync + std::fmt::Debug {
    /// The current value of option `name` (case-insensitive), or DuckDB's unrecognized
    /// option error.
    fn current_setting(&self, name: &str) -> Result<Value>;
    /// Every option with its current value.
    fn settings(&self) -> Vec<SettingInfo>;
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
