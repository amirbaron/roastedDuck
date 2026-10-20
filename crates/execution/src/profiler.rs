//! Per-operator profiling for `EXPLAIN ANALYZE` (DuckDB's `QueryProfiler` /
//! `OperatorProfiler`): the rows every physical operator produced and the time spent in
//! it.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::operator::PhysicalOperator;

/// What one operator did (DuckDB's `OperatorInformation`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OperatorProfile {
    /// The rows the operator produced.
    pub rows: u64,
    /// The time spent in the operator's calls.
    pub time: Duration,
}

/// The profile of one query run.
#[derive(Debug)]
pub struct QueryProfiler {
    start: Instant,
    operators: Mutex<HashMap<usize, OperatorProfile>>,
    /// Parameters sources report once exhausted (`OperatorInformation::extra_info`).
    extra_info: Mutex<HashMap<usize, Vec<(String, String)>>>,
}

impl Default for QueryProfiler {
    fn default() -> Self {
        Self::new()
    }
}

/// The address identifying an operator.
pub(crate) fn operator_key(op: &dyn PhysicalOperator) -> usize {
    std::ptr::from_ref(op).cast::<()>().addr()
}

impl QueryProfiler {
    /// A profiler whose clock starts now.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            operators: Mutex::new(HashMap::new()),
            extra_info: Mutex::new(HashMap::new()),
        }
    }

    /// The time since the profiler was created (DuckDB's query `LATENCY`).
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Adds `rows` and `time` to the profile of operator `key`.
    pub(crate) fn record(&self, key: usize, rows: u64, time: Duration) {
        let mut operators = self.operators.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = operators.entry(key).or_default();
        entry.rows += rows;
        entry.time += time;
    }

    /// Records parameters of operator `key`, replacing earlier values of the same
    /// names (`FinalizeSourceProfiling`).
    pub(crate) fn record_extra_info(&self, key: usize, params: Vec<(String, String)>) {
        let mut extra_info = self.extra_info.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = extra_info.entry(key).or_default();
        for (name, value) in params {
            match entry.iter_mut().find(|(existing, _)| *existing == name) {
                Some(slot) => slot.1 = value,
                None => entry.push((name, value)),
            }
        }
    }

    /// The parameters operator `op` reported while running.
    pub fn extra_info(&self, op: &dyn PhysicalOperator) -> Vec<(String, String)> {
        self.extra_info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&operator_key(op))
            .cloned()
            .unwrap_or_default()
    }

    /// The profile of `op`.
    pub fn profile(&self, op: &dyn PhysicalOperator) -> OperatorProfile {
        self.operators
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&operator_key(op))
            .copied()
            .unwrap_or_default()
    }
}
