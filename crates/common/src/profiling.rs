//! Query profiling metrics (DuckDB's `MetricType` / `MetricGroup` / `MetricsUtils` of
//! `metric_type.hpp`, and the `QueryMetrics` / `ActiveTimer` of `profiling_utils.hpp`).
//!
//! DuckDB reaches the running query's metrics through the `ClientContext` it passes
//! everywhere (`QueryProfiler::Get(context).AddToCounter(...)`). Storage code here does
//! not see the connection, so the connection activates its query's [`QueryMetrics`] on
//! the threads working on the query ([`activate`]), and [`add_to_counter`] /
//! [`start_timer`] record into the active ones.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

macro_rules! metric_types {
    ($($variant:ident => $name:literal,)*) => {
        /// A profiling metric (DuckDB's `MetricType`).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum MetricType {
            $($variant,)*
        }

        impl MetricType {
            /// Every metric (`MetricsUtils::GetAllMetrics`).
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// The metric's name (`EnumUtil::ToString`).
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }
        }
    };
}

metric_types! {
    QueryName => "QUERY_NAME",
    BlockedThreadTime => "BLOCKED_THREAD_TIME",
    CpuTime => "CPU_TIME",
    ExtraInfo => "EXTRA_INFO",
    CumulativeCardinality => "CUMULATIVE_CARDINALITY",
    OperatorType => "OPERATOR_TYPE",
    OperatorCardinality => "OPERATOR_CARDINALITY",
    CumulativeRowsScanned => "CUMULATIVE_ROWS_SCANNED",
    OperatorRowsScanned => "OPERATOR_ROWS_SCANNED",
    OperatorTiming => "OPERATOR_TIMING",
    ResultSetSize => "RESULT_SET_SIZE",
    Latency => "LATENCY",
    RowsReturned => "ROWS_RETURNED",
    OperatorName => "OPERATOR_NAME",
    SystemPeakBufferMemory => "SYSTEM_PEAK_BUFFER_MEMORY",
    SystemPeakTempDirSize => "SYSTEM_PEAK_TEMP_DIR_SIZE",
    TotalBytesRead => "TOTAL_BYTES_READ",
    TotalBytesWritten => "TOTAL_BYTES_WRITTEN",
    AllOptimizers => "ALL_OPTIMIZERS",
    CumulativeOptimizerTiming => "CUMULATIVE_OPTIMIZER_TIMING",
    Planner => "PLANNER",
    PlannerBinding => "PLANNER_BINDING",
    PhysicalPlanner => "PHYSICAL_PLANNER",
    PhysicalPlannerColumnBinding => "PHYSICAL_PLANNER_COLUMN_BINDING",
    PhysicalPlannerResolveTypes => "PHYSICAL_PLANNER_RESOLVE_TYPES",
    PhysicalPlannerCreatePlan => "PHYSICAL_PLANNER_CREATE_PLAN",
    OptimizerExpressionRewriter => "OPTIMIZER_EXPRESSION_REWRITER",
    OptimizerFilterPullup => "OPTIMIZER_FILTER_PULLUP",
    OptimizerFilterPushdown => "OPTIMIZER_FILTER_PUSHDOWN",
    OptimizerEmptyResultPullup => "OPTIMIZER_EMPTY_RESULT_PULLUP",
    OptimizerCteFilterPusher => "OPTIMIZER_CTE_FILTER_PUSHER",
    OptimizerRegexRange => "OPTIMIZER_REGEX_RANGE",
    OptimizerInClause => "OPTIMIZER_IN_CLAUSE",
    OptimizerJoinOrder => "OPTIMIZER_JOIN_ORDER",
    OptimizerDeliminator => "OPTIMIZER_DELIMINATOR",
    OptimizerUnnestRewriter => "OPTIMIZER_UNNEST_REWRITER",
    OptimizerUnusedColumns => "OPTIMIZER_UNUSED_COLUMNS",
    OptimizerStatisticsPropagation => "OPTIMIZER_STATISTICS_PROPAGATION",
    OptimizerCommonSubexpressions => "OPTIMIZER_COMMON_SUBEXPRESSIONS",
    OptimizerCommonAggregate => "OPTIMIZER_COMMON_AGGREGATE",
    OptimizerColumnLifetime => "OPTIMIZER_COLUMN_LIFETIME",
    OptimizerBuildSideProbeSide => "OPTIMIZER_BUILD_SIDE_PROBE_SIDE",
    OptimizerLimitPushdown => "OPTIMIZER_LIMIT_PUSHDOWN",
    OptimizerTopN => "OPTIMIZER_TOP_N",
    OptimizerCompressedMaterialization => "OPTIMIZER_COMPRESSED_MATERIALIZATION",
    OptimizerDuplicateGroups => "OPTIMIZER_DUPLICATE_GROUPS",
    OptimizerReorderFilter => "OPTIMIZER_REORDER_FILTER",
    OptimizerSamplingPushdown => "OPTIMIZER_SAMPLING_PUSHDOWN",
    OptimizerJoinFilterPushdown => "OPTIMIZER_JOIN_FILTER_PUSHDOWN",
    OptimizerExtension => "OPTIMIZER_EXTENSION",
    OptimizerMaterializedCte => "OPTIMIZER_MATERIALIZED_CTE",
    OptimizerSumRewriter => "OPTIMIZER_SUM_REWRITER",
    OptimizerLateMaterialization => "OPTIMIZER_LATE_MATERIALIZATION",
    OptimizerCteInlining => "OPTIMIZER_CTE_INLINING",
    OptimizerRowGroupPruner => "OPTIMIZER_ROW_GROUP_PRUNER",
    OptimizerTopNWindowElimination => "OPTIMIZER_TOP_N_WINDOW_ELIMINATION",
    OptimizerCommonSubplan => "OPTIMIZER_COMMON_SUBPLAN",
    OptimizerJoinElimination => "OPTIMIZER_JOIN_ELIMINATION",
    OptimizerWindowSelfJoin => "OPTIMIZER_WINDOW_SELF_JOIN",
    TotalMemoryAllocated => "TOTAL_MEMORY_ALLOCATED",
    AttachLoadStorageLatency => "ATTACH_LOAD_STORAGE_LATENCY",
    AttachReplayWalLatency => "ATTACH_REPLAY_WAL_LATENCY",
    CheckpointLatency => "CHECKPOINT_LATENCY",
    CommitLocalStorageLatency => "COMMIT_LOCAL_STORAGE_LATENCY",
    WaitingToAttachLatency => "WAITING_TO_ATTACH_LATENCY",
    WalReplayEntryCount => "WAL_REPLAY_ENTRY_COUNT",
    WriteToWalLatency => "WRITE_TO_WAL_LATENCY",
    OperatorRowGroupsScanned => "OPERATOR_ROW_GROUPS_SCANNED",
    OperatorTotalRowGroupsToScan => "OPERATOR_TOTAL_ROW_GROUPS_TO_SCAN",
    CumulativeRowGroupsScanned => "CUMULATIVE_ROW_GROUPS_SCANNED",
    CumulativeTotalRowGroupsToScan => "CUMULATIVE_TOTAL_ROW_GROUPS_TO_SCAN",
    CumulativeVacuumTime => "CUMULATIVE_VACUUM_TIME",
}

/// A named group of metrics (DuckDB's `MetricGroup`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricGroup {
    All,
    Core,
    Default,
    Execution,
    File,
    Operator,
    Optimizer,
    PhaseTiming,
}

impl MetricGroup {
    /// The group called `name` (`EnumUtil::FromString<MetricGroup>`, upper case).
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "ALL" => Self::All,
            "CORE" => Self::Core,
            "DEFAULT" => Self::Default,
            "EXECUTION" => Self::Execution,
            "FILE" => Self::File,
            "OPERATOR" => Self::Operator,
            "OPTIMIZER" => Self::Optimizer,
            "PHASE_TIMING" => Self::PhaseTiming,
            _ => return None,
        })
    }

    /// The group's metrics (`MetricsUtils::GetMetricsByGroupType`).
    pub fn metrics(self) -> Vec<MetricType> {
        let in_group: fn(MetricType) -> bool = match self {
            Self::All => |_| true,
            Self::Core => MetricType::is_core,
            Self::Default => MetricType::is_default,
            Self::Execution => MetricType::is_execution,
            Self::File => MetricType::is_file,
            Self::Operator => MetricType::is_operator,
            Self::Optimizer => MetricType::is_optimizer,
            Self::PhaseTiming => MetricType::is_phase_timing,
        };
        MetricType::ALL
            .iter()
            .copied()
            .filter(|&metric| in_group(metric))
            .collect()
    }
}

impl MetricType {
    /// The metric called `name` (`EnumUtil::FromString<MetricType>`, upper case).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|metric| metric.name() == name)
    }

    /// `MetricsUtils::IsCoreMetric`.
    pub const fn is_core(self) -> bool {
        matches!(
            self,
            Self::CpuTime
                | Self::CumulativeCardinality
                | Self::CumulativeRowsScanned
                | Self::CumulativeRowGroupsScanned
                | Self::CumulativeTotalRowGroupsToScan
                | Self::ExtraInfo
                | Self::Latency
                | Self::QueryName
                | Self::ResultSetSize
                | Self::RowsReturned
        )
    }

    /// `MetricsUtils::IsDefaultMetric`.
    pub const fn is_default(self) -> bool {
        matches!(
            self,
            Self::AttachLoadStorageLatency
                | Self::AttachReplayWalLatency
                | Self::BlockedThreadTime
                | Self::CheckpointLatency
                | Self::CommitLocalStorageLatency
                | Self::CpuTime
                | Self::CumulativeCardinality
                | Self::CumulativeRowsScanned
                | Self::ExtraInfo
                | Self::Latency
                | Self::OperatorCardinality
                | Self::OperatorName
                | Self::OperatorRowsScanned
                | Self::OperatorTiming
                | Self::OperatorType
                | Self::QueryName
                | Self::ResultSetSize
                | Self::RowsReturned
                | Self::SystemPeakBufferMemory
                | Self::SystemPeakTempDirSize
                | Self::TotalBytesRead
                | Self::TotalBytesWritten
                | Self::TotalMemoryAllocated
                | Self::WaitingToAttachLatency
                | Self::WalReplayEntryCount
                | Self::WriteToWalLatency
        )
    }

    /// `MetricsUtils::IsExecutionMetric`.
    pub const fn is_execution(self) -> bool {
        matches!(
            self,
            Self::BlockedThreadTime
                | Self::SystemPeakBufferMemory
                | Self::SystemPeakTempDirSize
                | Self::TotalMemoryAllocated
        )
    }

    /// `MetricsUtils::IsFileMetric`.
    pub const fn is_file(self) -> bool {
        matches!(
            self,
            Self::AttachLoadStorageLatency
                | Self::AttachReplayWalLatency
                | Self::CheckpointLatency
                | Self::CommitLocalStorageLatency
                | Self::CumulativeVacuumTime
                | Self::TotalBytesRead
                | Self::TotalBytesWritten
                | Self::WaitingToAttachLatency
                | Self::WalReplayEntryCount
                | Self::WriteToWalLatency
        )
    }

    /// `MetricsUtils::IsOperatorMetric`.
    pub const fn is_operator(self) -> bool {
        matches!(
            self,
            Self::OperatorCardinality
                | Self::OperatorName
                | Self::OperatorRowsScanned
                | Self::OperatorRowGroupsScanned
                | Self::OperatorTiming
                | Self::OperatorTotalRowGroupsToScan
                | Self::OperatorType
        )
    }

    /// `MetricsUtils::IsOptimizerMetric`: the timing of one optimizer pass.
    pub fn is_optimizer(self) -> bool {
        self.name().starts_with("OPTIMIZER_")
    }

    /// `MetricsUtils::IsPhaseTimingMetric`.
    pub const fn is_phase_timing(self) -> bool {
        matches!(
            self,
            Self::AllOptimizers
                | Self::CumulativeOptimizerTiming
                | Self::PhysicalPlanner
                | Self::PhysicalPlannerColumnBinding
                | Self::PhysicalPlannerCreatePlan
                | Self::PhysicalPlannerResolveTypes
                | Self::Planner
                | Self::PlannerBinding
        )
    }

    /// `MetricsUtils::IsRootScopeMetric`: metrics only the query root carries.
    pub const fn is_root_scope(self) -> bool {
        matches!(
            self,
            Self::AttachLoadStorageLatency
                | Self::AttachReplayWalLatency
                | Self::BlockedThreadTime
                | Self::CheckpointLatency
                | Self::CommitLocalStorageLatency
                | Self::CumulativeVacuumTime
                | Self::Latency
                | Self::QueryName
                | Self::RowsReturned
                | Self::TotalBytesRead
                | Self::TotalBytesWritten
                | Self::TotalMemoryAllocated
                | Self::WaitingToAttachLatency
                | Self::WalReplayEntryCount
                | Self::WriteToWalLatency
        )
    }

    /// The optimizer metric of the pass called `name` in `disabled_optimizers`
    /// (`MetricsUtils::GetOptimizerMetricByType`).
    pub fn optimizer_metric(name: &str) -> Option<Self> {
        Self::from_name(&format!("OPTIMIZER_{}", name.to_ascii_uppercase()))
    }

    /// The pass name of an optimizer metric (`MetricsUtils::GetOptimizerTypeByMetric`).
    pub fn optimizer_name(self) -> Option<String> {
        self.name()
            .strip_prefix("OPTIMIZER_")
            .map(str::to_ascii_lowercase)
    }

    /// The slot of an actively tracked metric (`QueryMetrics::GetMetricsIndex`).
    const fn tracked_index(self) -> Option<usize> {
        Some(match self {
            Self::AttachLoadStorageLatency => 0,
            Self::AttachReplayWalLatency => 1,
            Self::CheckpointLatency => 2,
            Self::CommitLocalStorageLatency => 3,
            Self::CumulativeVacuumTime => 4,
            Self::Latency => 5,
            Self::WaitingToAttachLatency => 6,
            Self::WriteToWalLatency => 7,
            Self::TotalBytesRead => 8,
            Self::TotalBytesWritten => 9,
            Self::TotalMemoryAllocated => 10,
            Self::WalReplayEntryCount => 11,
            _ => return None,
        })
    }
}

/// The number of actively tracked metrics (`ACTIVELY_TRACKED_METRICS`).
const ACTIVELY_TRACKED_METRICS: usize = 12;

/// The counters of a running query (DuckDB's `QueryMetrics::active_metrics`): byte
/// counts, entry counts and timings in nanoseconds.
#[derive(Debug, Default)]
pub struct QueryMetrics {
    active: [AtomicU64; ACTIVELY_TRACKED_METRICS],
}

impl QueryMetrics {
    /// Adds `amount` to `metric` (`QueryMetrics::UpdateMetric`); metrics that are not
    /// actively tracked are ignored.
    pub fn update(&self, metric: MetricType, amount: u64) {
        if let Some(index) = metric.tracked_index() {
            self.active[index].fetch_add(amount, Ordering::Relaxed);
        }
    }

    /// The value of `metric` (`QueryMetrics::GetMetricValue`).
    pub fn value(&self, metric: MetricType) -> u64 {
        metric
            .tracked_index()
            .map_or(0, |index| self.active[index].load(Ordering::Relaxed))
    }

    /// A timing metric in seconds (`QueryMetrics::GetMetricInSeconds`).
    pub fn seconds(&self, metric: MetricType) -> f64 {
        self.value(metric) as f64 / 1e9
    }

    /// Clears every counter (`QueryMetrics::Reset`).
    pub fn reset(&self) {
        for counter in &self.active {
            counter.store(0, Ordering::Relaxed);
        }
    }
}

thread_local! {
    /// The metrics of the query this thread works on.
    static ACTIVE: RefCell<Option<Arc<QueryMetrics>>> = const { RefCell::new(None) };
}

/// Restores the thread's previously active metrics when dropped.
#[derive(Debug)]
pub struct ActiveMetricsGuard {
    previous: Option<Option<Arc<QueryMetrics>>>,
}

impl Drop for ActiveMetricsGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            ACTIVE.with(|active| *active.borrow_mut() = previous);
        }
    }
}

/// Makes `metrics` the metrics this thread records into until the guard is dropped;
/// `None` leaves the thread's metrics as they are.
pub fn activate(metrics: Option<&Arc<QueryMetrics>>) -> ActiveMetricsGuard {
    ActiveMetricsGuard {
        previous: metrics
            .map(|metrics| ACTIVE.with(|active| active.borrow_mut().replace(Arc::clone(metrics)))),
    }
}

/// The metrics this thread records into, if a profiled query runs on it.
pub fn active_metrics() -> Option<Arc<QueryMetrics>> {
    ACTIVE.with(|active| active.borrow().clone())
}

/// Adds `amount` to `metric` of the running query (`QueryProfiler::AddToCounter`).
pub fn add_to_counter(metric: MetricType, amount: u64) {
    ACTIVE.with(|active| {
        if let Some(metrics) = active.borrow().as_ref() {
            metrics.update(metric, amount);
        }
    });
}

/// Times `metric` of the running query until the timer ends or is dropped
/// (`QueryProfiler::StartTimer`).
pub fn start_timer(metric: MetricType) -> ActiveTimer {
    ActiveTimer {
        metrics: active_metrics(),
        metric,
        start: Instant::now(),
    }
}

/// A running timer of a query metric (DuckDB's `ActiveTimer`); inactive when no
/// profiled query runs.
#[derive(Debug)]
pub struct ActiveTimer {
    metrics: Option<Arc<QueryMetrics>>,
    metric: MetricType,
    start: Instant,
}

impl ActiveTimer {
    /// Stops the timer and records the elapsed time (`ActiveTimer::EndTimer`).
    pub fn end(&mut self) {
        if let Some(metrics) = self.metrics.take() {
            let nanos = u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX);
            metrics.update(self.metric, nanos);
        }
    }
}

impl Drop for ActiveTimer {
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_follow_duckdb() {
        assert_eq!(MetricType::ALL.len(), 72);
        assert_eq!(MetricGroup::Default.metrics().len(), 26);
        assert_eq!(MetricGroup::Optimizer.metrics().len(), 33);
        assert_eq!(MetricGroup::PhaseTiming.metrics().len(), 8);
        assert_eq!(
            MetricType::optimizer_metric("join_order"),
            Some(MetricType::OptimizerJoinOrder)
        );
    }

    #[test]
    fn counters_record_into_the_active_query() {
        let metrics = Arc::new(QueryMetrics::default());
        add_to_counter(MetricType::TotalBytesRead, 5);
        {
            let _guard = activate(Some(&metrics));
            add_to_counter(MetricType::TotalBytesRead, 7);
            drop(start_timer(MetricType::CheckpointLatency));
        }
        add_to_counter(MetricType::TotalBytesRead, 11);
        assert_eq!(metrics.value(MetricType::TotalBytesRead), 7);
    }
}
