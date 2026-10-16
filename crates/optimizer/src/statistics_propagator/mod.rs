//! Statistics propagation (DuckDB's `StatisticsPropagator`): column statistics of the
//! scans are propagated up through the expressions and operators of the plan. On the
//! way, comparisons whose outcome the statistics decide are folded, filters that are
//! always true are removed, subtrees that cannot produce rows become empty results and
//! joins derive filters for their inputs.
//!
//! The statistics callbacks DuckDB attaches to functions live in [`function`], keyed by
//! the function name.

mod expression;
mod function;

pub use function::WithoutStatistics;
mod operator;

use std::cmp::Ordering;
use std::collections::HashMap;

use rustdb_common::Result;
use rustdb_planner::{ColumnBinding, LogicalOperator};
use rustdb_storage::statistics::{BaseStatistics, FilterPropagateResult, NumericValue};
use rustdb_types::Value;

use crate::optimizer::Optimizer;

/// Cardinality bounds of an operator's output (DuckDB's `NodeStatistics`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeStatistics {
    pub estimated_cardinality: Option<u64>,
    pub max_cardinality: Option<u64>,
}

impl NodeStatistics {
    /// Statistics with both an estimate and a maximum.
    pub const fn new(estimated_cardinality: u64, max_cardinality: u64) -> Self {
        Self {
            estimated_cardinality: Some(estimated_cardinality),
            max_cardinality: Some(max_cardinality),
        }
    }
}

/// Propagates statistics through a plan (DuckDB's `StatisticsPropagator`).
pub struct StatisticsPropagator<'a> {
    optimizer: &'a Optimizer,
    /// The statistics of every column binding seen so far.
    statistics_map: HashMap<ColumnBinding, BaseStatistics>,
    /// The cardinality statistics of the current node.
    node_stats: Option<NodeStatistics>,
}

impl std::fmt::Debug for StatisticsPropagator<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatisticsPropagator")
            .field("statistics_map", &self.statistics_map.len())
            .finish_non_exhaustive()
    }
}

impl<'a> StatisticsPropagator<'a> {
    /// A propagator for `root`, whose types are resolved first.
    pub fn new(optimizer: &'a Optimizer, root: &mut LogicalOperator) -> Result<Self> {
        root.resolve_operator_types()?;
        Ok(Self {
            optimizer,
            statistics_map: HashMap::new(),
            node_stats: None,
        })
    }

    /// The statistics of the column bindings (DuckDB's `GetStatisticsMap`).
    pub fn into_statistics_map(self) -> HashMap<ColumnBinding, BaseStatistics> {
        self.statistics_map
    }
}

/// The minimum of numeric statistics with a valid range.
fn numeric_min(stats: &BaseStatistics) -> Option<NumericValue> {
    stats.numeric().and_then(|numeric| numeric.min)
}

/// The maximum of numeric statistics with a valid range.
fn numeric_max(stats: &BaseStatistics) -> Option<NumericValue> {
    stats.numeric().and_then(|numeric| numeric.max)
}

/// Whether numeric statistics have a valid range (DuckDB's `NumericStats::HasMinMax`).
fn has_min_max(stats: &BaseStatistics) -> bool {
    stats
        .numeric()
        .is_some_and(rustdb_storage::statistics::NumericStats::has_min_max)
}

/// Sets the minimum of numeric statistics (DuckDB's `NumericStats::SetMin`); a NULL value
/// makes it unknown.
fn set_min(stats: &mut BaseStatistics, value: Option<NumericValue>) {
    if let Some(numeric) = stats.numeric_mut() {
        numeric.min = value;
    }
}

/// Sets the maximum of numeric statistics (DuckDB's `NumericStats::SetMax`).
fn set_max(stats: &mut BaseStatistics, value: Option<NumericValue>) {
    if let Some(numeric) = stats.numeric_mut() {
        numeric.max = value;
    }
}

/// `left > right` for two bounds of the same type.
fn greater(left: NumericValue, right: NumericValue) -> bool {
    left.compare(right) == Some(Ordering::Greater)
}

/// `left >= right` for two bounds of the same type.
fn greater_equal(left: NumericValue, right: NumericValue) -> bool {
    matches!(left.compare(right), Some(Ordering::Greater | Ordering::Equal))
}

/// The bound of a constant for statistics of its type.
fn numeric_value(value: &Value) -> Option<NumericValue> {
    NumericValue::from_value(value)
}

/// Whether a prune result means the filter never passes a row.
const fn is_always_false(result: FilterPropagateResult) -> bool {
    matches!(
        result,
        FilterPropagateResult::FilterAlwaysFalse | FilterPropagateResult::FilterFalseOrNull
    )
}
