//! Answering `count(*)`, `min` and `max` over a whole table from row group statistics
//! (DuckDB's `StatisticsPropagator::TryExecuteAggregates`): the aggregate becomes a
//! single row of constants.

use std::cmp::Ordering;
use std::sync::Arc;

use rustdb_catalog::EntryKind;
use rustdb_planner::{
    ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};
use rustdb_storage::statistics::{BaseStatistics, FilterPropagateResult, NumericStats};
use rustdb_storage::table::row_group::RowGroup;
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use super::StatisticsPropagator;

/// The longest string whose min/max statistics are exact (DuckDB's
/// `StringStatsData::MAX_STRING_MINMAX_SIZE`).
const MAX_STRING_MINMAX_SIZE: u64 = 8;

/// One row group's statistics (DuckDB's `PartitionStatistics`).
struct Partition {
    source: PartitionSource,
    count: u64,
    /// Whether `count` (and for tables, the min/max statistics) are exact.
    exact: bool,
}

/// Where a partition's column statistics come from.
enum PartitionSource {
    /// A row group of a table.
    RowGroup(Arc<RowGroup>),
    /// A partition of a table function (`None` without column statistics).
    Function(Option<Arc<dyn rustdb_function::PartitionRowGroup>>),
}

impl Partition {
    /// The statistics of column `column` (a physical column of a table, a column of a
    /// table function).
    fn column_statistics(&self, column: usize) -> Option<BaseStatistics> {
        match &self.source {
            PartitionSource::RowGroup(row_group) => row_group.column_statistics(column).ok(),
            PartitionSource::Function(row_group) => row_group
                .as_ref()?
                .column_statistics(column as u64)
                .and_then(|stats| stats.downcast::<BaseStatistics>().ok())
                .map(|stats| *stats),
        }
    }
}

/// Whether an aggregate is `min` (or `max`) of a column.
#[derive(Clone, Copy)]
struct MinMax {
    binding: ColumnBinding,
    is_min: bool,
}

/// Whether min/max statistics of `ty` can answer the aggregate (DuckDB's `GetComparator`).
fn has_comparator(ty: &LogicalType) -> bool {
    ty.id() == LogicalTypeId::Varchar
        || ty.is_numeric()
        || matches!(
            ty.id(),
            LogicalTypeId::Date
                | LogicalTypeId::Time
                | LogicalTypeId::Timestamp
                | LogicalTypeId::TimestampTz
                | LogicalTypeId::TimestampSec
                | LogicalTypeId::TimestampMs
                | LogicalTypeId::TimestampNs
                | LogicalTypeId::Interval
                | LogicalTypeId::TimeTz
        )
}

/// The exact minimum or maximum of a row group's column (DuckDB's `TryGetValueFromStats`).
fn value_from_stats(partition: &Partition, column: usize, is_min: bool) -> Option<Value> {
    let stats = partition.column_statistics(column)?;
    let exact = match &partition.source {
        PartitionSource::RowGroup(_) => min_max_is_exact(partition, &stats),
        PartitionSource::Function(row_group) => {
            row_group.as_ref()?.min_max_is_exact(column as u64) && function_bounds_usable(&stats)
        }
    };
    if !exact {
        return None;
    }
    if let Some(numeric) = stats.numeric() {
        if !numeric.has_min_max() {
            return None;
        }
        return if is_min {
            stats.numeric_min()
        } else {
            stats.numeric_max()
        };
    }
    let string = stats.string()?;
    let bytes = if is_min {
        string.min_bytes()
    } else {
        string.max_bytes()
    };
    Some(Value::varchar(&*String::from_utf8_lossy(bytes)))
}

/// DuckDB's `DuckDBPartitionRowGroup::MinMaxIsExact`: no version information or updates,
/// and for strings, all values as long as the (complete) stored bounds.
fn min_max_is_exact(partition: &Partition, stats: &BaseStatistics) -> bool {
    let PartitionSource::RowGroup(row_group) = &partition.source else {
        return false;
    };
    if !partition.exact || row_group.columns().iter().any(|column| column.has_any_updates()) {
        return false;
    }
    if let Some(string) = stats.string() {
        if !string.has_max_string_length || u64::from(string.max_string_length) > MAX_STRING_MINMAX_SIZE {
            return false;
        }
        let length = string.max_string_length;
        return usize::try_from(length)
            .is_ok_and(|length| string.min_bytes().len() == length && string.max_bytes().len() == length);
    }
    stats.numeric().is_some_and(NumericStats::has_min_max)
}

/// The checks of `TryGetValueFromStats` on a table function's statistics: numeric bounds
/// must be set, string bounds complete (at most 8 bytes long).
fn function_bounds_usable(stats: &BaseStatistics) -> bool {
    if let Some(string) = stats.string() {
        return string.has_max_string_length && u64::from(string.max_string_length) <= MAX_STRING_MINMAX_SIZE;
    }
    stats.numeric().is_some_and(NumericStats::has_min_max)
}

/// `left` is better than `right` for the aggregate.
fn better(left: &Value, right: &Value, is_min: bool) -> bool {
    let ordering = match (left.as_bytes(), right.as_bytes()) {
        (Some(left), Some(right)) => left.cmp(right),
        _ => crate::expression_util::compare_values(left, right).unwrap_or(Ordering::Equal),
    };
    if is_min {
        ordering == Ordering::Less
    } else {
        ordering == Ordering::Greater
    }
}

impl StatisticsPropagator<'_> {
    /// DuckDB's `TryExecuteAggregates`.
    #[allow(clippy::too_many_lines)]
    pub(super) fn try_execute_aggregates(&self, node: &mut LogicalOperator) {
        let LogicalKind::Aggregate(aggregate) = &node.kind else {
            return;
        };
        if !aggregate.groups.is_empty() {
            return;
        }
        let (group_index, aggregate_index) = (aggregate.group_index, aggregate.aggregate_index);
        let mut count_star = Vec::new();
        let mut min_max = Vec::new();
        for (index, expression) in node.expressions.iter().enumerate() {
            let ExpressionKind::Aggregate(aggregate) = &expression.kind else {
                return;
            };
            if aggregate.filter.is_some() {
                return;
            }
            match &*aggregate.function.name {
                name @ ("min" | "max") => {
                    let [child] = aggregate.children.as_slice() else {
                        return;
                    };
                    let ExpressionKind::ColumnRef(column) = &child.kind else {
                        return;
                    };
                    if !has_comparator(&child.return_type) {
                        return;
                    }
                    min_max.push(MinMax {
                        binding: column.binding,
                        is_min: name == "min",
                    });
                }
                "count_star" => count_star.push(index),
                _ => return,
            }
        }
        // Skip projections down to the scan.
        let Some(mut child) = node.children.first() else {
            return;
        };
        while child.operator_type() == LogicalOperatorType::LogicalProjection {
            for entry in &mut min_max {
                let Some(ExpressionKind::ColumnRef(column)) = child
                    .expressions
                    .get(entry.binding.column_index as usize)
                    .map(|expression| &expression.kind)
                else {
                    return;
                };
                entry.binding = column.binding;
            }
            let Some(next) = child.children.first() else {
                return;
            };
            child = next;
        }
        let LogicalKind::Get(get) = &child.kind else {
            return;
        };
        if get.table.is_none() {
            let get = get.clone();
            Self::try_execute_function_aggregates(
                node,
                &get,
                &count_star,
                &min_max,
                group_index,
                aggregate_index,
            );
            return;
        }
        let Some(entry) = &get.table else {
            return;
        };
        let EntryKind::Table(table) = &entry.kind else {
            return;
        };
        let Some(storage) = &table.storage else {
            return;
        };
        // Transaction-local rows have no row group statistics here.
        if self
            .optimizer
            .context()
            .transaction
            .local_storage()
            .added_rows(storage)
            > 0
        {
            return;
        }
        let partitions: Vec<Partition> = storage
            .row_groups()
            .row_groups()
            .tree()
            .nodes()
            .into_iter()
            .map(|node| Partition {
                count: node.node.count(),
                exact: node.node.version_info().is_none(),
                source: PartitionSource::RowGroup(node.node),
            })
            .collect();
        if partitions.is_empty() {
            return;
        }
        let physical = |logical: u64| {
            usize::try_from(logical)
                .ok()
                .and_then(|logical| table.logical_to_physical(logical))
        };
        let mut min_max_columns = Vec::with_capacity(min_max.len());
        for entry in &min_max {
            let Some(column) = get.column_ids.get(entry.binding.column_index as usize) else {
                return;
            };
            let Some(physical) = physical(column.index) else {
                return;
            };
            min_max_columns.push(physical);
        }
        let mut filters = Vec::new();
        for (&column, filter) in &get.table_filters.filters {
            let Some(physical) = physical(column as u64) else {
                return;
            };
            filters.push((physical, filter.clone()));
        }
        Self::rewrite_with_partitions(
            node,
            partitions,
            &filters,
            &min_max,
            &min_max_columns,
            &count_star,
            group_index,
            aggregate_index,
        );
    }

    /// The end of `TryExecuteAggregates`: the aggregate over `partitions` (filtered by
    /// `filters`, by column) answered from their statistics, if possible.
    #[allow(clippy::too_many_arguments)]
    fn rewrite_with_partitions(
        node: &mut LogicalOperator,
        mut partitions: Vec<Partition>,
        filters: &[(usize, rustdb_storage::TableFilter)],
        min_max: &[MinMax],
        min_max_columns: &[usize],
        count_star: &[usize],
        group_index: u64,
        aggregate_index: u64,
    ) {
        if !filters.is_empty() {
            // Every row group must pass or fail the filters entirely.
            let mut remaining = Vec::new();
            for partition in partitions {
                let mut result = FilterPropagateResult::FilterAlwaysTrue;
                for (column, filter) in filters {
                    let Some(stats) = partition.column_statistics(*column) else {
                        return;
                    };
                    let column_result = filter.check_statistics(&stats);
                    if column_result == FilterPropagateResult::FilterAlwaysFalse {
                        result = column_result;
                        break;
                    }
                    if column_result != FilterPropagateResult::FilterAlwaysTrue {
                        result = column_result;
                    }
                }
                match result {
                    FilterPropagateResult::FilterAlwaysTrue => remaining.push(partition),
                    FilterPropagateResult::FilterAlwaysFalse => {}
                    _ => return,
                }
            }
            partitions = remaining;
        }
        if partitions.is_empty() {
            return;
        }
        let mut results: Vec<Expression> = Vec::new();
        let mut types: Vec<LogicalType> = Vec::new();
        for (entry, &column) in min_max.iter().zip(min_max_columns) {
            let mut result: Option<Value> = None;
            for partition in &partitions {
                let Some(value) = value_from_stats(partition, column, entry.is_min) else {
                    return;
                };
                result = match result {
                    Some(current) if !better(&value, &current, entry.is_min) => Some(current),
                    _ => Some(value),
                };
            }
            let Some(value) = result else {
                return;
            };
            types.push(value.logical_type().clone());
            results.push(Expression::constant(value));
        }
        if !count_star.is_empty() {
            if partitions.iter().any(|partition| !partition.exact) {
                // Deleted rows make the counts approximate.
                return;
            }
            let count: u64 = partitions.iter().map(|partition| partition.count).sum();
            let value = Value::bigint(i64::try_from(count).unwrap_or(i64::MAX));
            for &index in count_star {
                results.insert(index, Expression::constant(value.clone()));
                types.insert(index, LogicalType::BIGINT);
            }
        }
        for (result, expression) in results.iter_mut().zip(&node.expressions) {
            result.alias.clone_from(&expression.alias);
        }
        *node = LogicalOperator::new(LogicalKind::ExpressionGet {
            table_index: aggregate_index,
            expr_types: types,
            expressions: vec![results],
        })
        .with_children(vec![LogicalOperator::new(LogicalKind::DummyScan {
            table_index: group_index,
        })]);
    }

    /// `TryExecuteAggregates` over a table function with partition statistics
    /// (`get_partition_stats`).
    fn try_execute_function_aggregates(
        node: &mut LogicalOperator,
        get: &rustdb_planner::operator::LogicalGet,
        count_star: &[usize],
        min_max: &[MinMax],
        group_index: u64,
        aggregate_index: u64,
    ) {
        let Some(partition_stats) = &get.function.get_partition_stats else {
            return;
        };
        if get.sample_options.is_some() {
            return;
        }
        let partitions: Vec<Partition> = partition_stats(get.bind_data.as_deref())
            .into_iter()
            .map(|partition| Partition {
                count: partition.count,
                exact: partition.exact,
                source: PartitionSource::Function(partition.row_group),
            })
            .collect();
        if partitions.is_empty() {
            return;
        }
        // `TryGetStorageIndex`: no row id or virtual columns.
        let storage_index = |column: u64| {
            (column < (1 << 63))
                .then(|| usize::try_from(column).ok())
                .flatten()
        };
        let mut min_max_columns = Vec::with_capacity(min_max.len());
        for entry in min_max {
            let Some(column) = get.column_ids.get(entry.binding.column_index as usize) else {
                return;
            };
            let Some(index) = storage_index(column.index) else {
                return;
            };
            min_max_columns.push(index);
        }
        let mut filters = Vec::new();
        for (&column, filter) in &get.table_filters.filters {
            let Some(index) = storage_index(column as u64) else {
                return;
            };
            filters.push((index, filter.clone()));
        }
        Self::rewrite_with_partitions(
            node,
            partitions,
            &filters,
            min_max,
            &min_max_columns,
            count_star,
            group_index,
            aggregate_index,
        );
    }
}
