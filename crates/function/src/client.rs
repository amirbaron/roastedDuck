//! Connection state reachable from function callbacks (the parts of DuckDB's
//! `ClientContext` that functions use).

use std::sync::{Mutex, MutexGuard};

use crate::random::RandomEngine;

/// Per-connection state shared by the functions a connection runs.
#[derive(Debug, Default)]
pub struct FunctionClient {
    random: Mutex<RandomEngine>,
}

impl FunctionClient {
    /// The connection's random engine (DuckDB's `RandomEngine::Get(context)` with its
    /// lock held).
    pub fn random_engine(&self) -> MutexGuard<'_, RandomEngine> {
        // A poisoned lock only means another thread panicked mid-draw; the engine state
        // is still a valid generator state.
        self.random.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
