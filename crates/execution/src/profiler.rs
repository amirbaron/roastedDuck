//! Per-operator profiling (DuckDB's `OperatorProfiler` / `OperatorInformation` and the
//! operator side of `QueryProfiler::Flush`): the rows every physical operator produced,
//! the time spent in it, the bytes of its output and what its scans read.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use roastedduck_buffer::BufferManager;
use roastedduck_common::profiling::QueryMetrics;

use crate::operator::PhysicalOperator;

/// What one operator did (DuckDB's `OperatorInformation`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OperatorProfile {
    /// The rows the operator produced.
    pub rows: u64,
    /// The time spent in the operator's calls.
    pub time: Duration,
    /// The bytes of the chunks the operator produced (`RESULT_SET_SIZE`).
    pub result_set_size: u64,
}

/// What a table scan read, summed over its threads (the `TABLE_SCAN_METRICS` of
/// `OperatorProfiler::FinalizeSourceProfiling`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanMetrics {
    /// `OPERATOR_ROWS_SCANNED`: the rows read before filters.
    pub rows_scanned: Option<u64>,
    /// `OPERATOR_ROW_GROUPS_SCANNED`.
    pub row_groups_scanned: Option<u64>,
    /// `OPERATOR_TOTAL_ROW_GROUPS_TO_SCAN`: the row groups the table had (the same for
    /// every thread).
    pub total_row_groups: Option<u64>,
}

/// The profile of one query run.
#[derive(Debug)]
pub struct QueryProfiler {
    start: Instant,
    operators: Mutex<HashMap<usize, OperatorProfile>>,
    /// Parameters sources report once exhausted (`OperatorInformation::extra_info`).
    extra_info: Mutex<HashMap<usize, Vec<(roastedduck_common::ByteString, roastedduck_common::ByteString)>>>,
    /// What the table scans read, by operator.
    scans: Mutex<HashMap<usize, ScanMetrics>>,
    /// The metrics of the query the threads record into while they work on it.
    query_metrics: Option<Arc<QueryMetrics>>,
    /// Whether `EXPLAIN ANALYZE` without a format renders JSON (the connection prints
    /// profiles as JSON: `QueryProfiler::GetPrintFormat`).
    json_by_default: bool,
    /// The buffer manager sampled after every operator call, and the largest memory and
    /// temporary directory use seen (`SYSTEM_PEAK_BUFFER_MEMORY`,
    /// `SYSTEM_PEAK_TEMP_DIR_SIZE`).
    memory: Option<BufferManager>,
    peak_memory: AtomicU64,
    peak_temp_dir_size: AtomicU64,
}

impl Default for QueryProfiler {
    fn default() -> Self {
        Self::new()
    }
}

/// The address identifying an operator.
pub fn operator_key(op: &dyn PhysicalOperator) -> usize {
    std::ptr::from_ref(op).cast::<()>().addr()
}

impl QueryProfiler {
    /// A profiler whose clock starts now.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            operators: Mutex::new(HashMap::new()),
            extra_info: Mutex::new(HashMap::new()),
            scans: Mutex::new(HashMap::new()),
            query_metrics: None,
            json_by_default: false,
            memory: None,
            peak_memory: AtomicU64::new(0),
            peak_temp_dir_size: AtomicU64::new(0),
        }
    }

    /// The profiler of a query that samples the memory use of `buffer_manager`.
    #[must_use]
    pub fn with_memory(mut self, buffer_manager: BufferManager) -> Self {
        self.memory = Some(buffer_manager);
        self
    }

    /// The largest buffer memory use seen after an operator call.
    pub fn peak_memory(&self) -> u64 {
        self.peak_memory.load(Ordering::Relaxed)
    }

    /// The largest temporary directory use seen after an operator call.
    pub fn peak_temp_dir_size(&self) -> u64 {
        self.peak_temp_dir_size.load(Ordering::Relaxed)
    }

    /// The profiler of a query whose threads record into `metrics`.
    #[must_use]
    pub fn with_query_metrics(mut self, metrics: Option<Arc<QueryMetrics>>) -> Self {
        self.query_metrics = metrics;
        self
    }

    /// The profiler of a connection that prints profiles as JSON.
    #[must_use]
    pub const fn with_json_by_default(mut self, json: bool) -> Self {
        self.json_by_default = json;
        self
    }

    /// Whether `EXPLAIN ANALYZE` without a format renders JSON.
    pub const fn json_by_default(&self) -> bool {
        self.json_by_default
    }

    /// The metrics of the query the threads record into.
    pub fn query_metrics(&self) -> Option<&Arc<QueryMetrics>> {
        self.query_metrics.as_ref()
    }

    /// The time since the profiler was created (DuckDB's query `LATENCY`).
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Adds `rows`, `time` and `bytes` to the profile of operator `key`.
    pub(crate) fn record(&self, key: usize, rows: u64, time: Duration, bytes: u64) {
        let mut operators = self.operators.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = operators.entry(key).or_default();
        entry.rows += rows;
        entry.time += time;
        entry.result_set_size += bytes;
        drop(operators);
        if let Some(memory) = &self.memory {
            self.peak_memory
                .fetch_max(memory.used_memory_estimate(), Ordering::Relaxed);
            self.peak_temp_dir_size
                .fetch_max(memory.used_swap(), Ordering::Relaxed);
        }
    }

    /// Records parameters of operator `key`, replacing earlier values of the same
    /// names (`FinalizeSourceProfiling`).
    pub(crate) fn record_extra_info(
        &self,
        key: usize,
        params: Vec<(roastedduck_common::ByteString, roastedduck_common::ByteString)>,
    ) {
        let mut extra_info = self.extra_info.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = extra_info.entry(key).or_default();
        for (name, value) in params {
            match entry.iter_mut().find(|(existing, _)| *existing == name) {
                Some(slot) => slot.1 = value,
                None => entry.push((name, value)),
            }
        }
    }

    /// Adds what a thread's scan of operator `key` read (`QueryProfiler::Flush`: the
    /// counts are summed, the total of row groups replaced).
    pub(crate) fn record_scan(&self, key: usize, metrics: ScanMetrics) {
        let mut scans = self.scans.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = scans.entry(key).or_default();
        let sum = |total: &mut Option<u64>, value: Option<u64>| {
            if let Some(value) = value {
                *total = Some(total.unwrap_or(0) + value);
            }
        };
        sum(&mut entry.rows_scanned, metrics.rows_scanned);
        sum(&mut entry.row_groups_scanned, metrics.row_groups_scanned);
        if metrics.total_row_groups.is_some() {
            entry.total_row_groups = metrics.total_row_groups;
        }
    }

    /// The parameters operator `op` reported while running.
    pub fn extra_info(
        &self,
        op: &dyn PhysicalOperator,
    ) -> Vec<(roastedduck_common::ByteString, roastedduck_common::ByteString)> {
        self.extra_info_by_key(operator_key(op))
    }

    /// The parameters the operator with [`operator_key`] `key` reported while running.
    pub fn extra_info_by_key(
        &self,
        key: usize,
    ) -> Vec<(roastedduck_common::ByteString, roastedduck_common::ByteString)> {
        self.extra_info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .cloned()
            .unwrap_or_default()
    }

    /// The profile of `op`.
    pub fn profile(&self, op: &dyn PhysicalOperator) -> OperatorProfile {
        self.profile_by_key(operator_key(op))
    }

    /// The profile of the operator with [`operator_key`] `key`.
    pub fn profile_by_key(&self, key: usize) -> OperatorProfile {
        self.operators
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .copied()
            .unwrap_or_default()
    }

    /// What the scans of the operator with [`operator_key`] `key` read, if it is a
    /// table scan that reported it.
    pub fn scan_by_key(&self, key: usize) -> Option<ScanMetrics> {
        self.scans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .copied()
    }
}
