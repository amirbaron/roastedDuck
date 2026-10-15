//! Connection state reachable from function callbacks (the parts of DuckDB's
//! `ClientContext` that functions use).

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::random::RandomEngine;

/// Per-connection state shared by the functions a connection runs.
#[derive(Debug, Default)]
pub struct FunctionClient {
    random: Mutex<RandomEngine>,
    /// The id of the current transaction and its start time in microseconds since the
    /// epoch (DuckDB's `MetaTransaction::start_timestamp`).
    transaction: Mutex<Option<(u64, i64)>>,
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
