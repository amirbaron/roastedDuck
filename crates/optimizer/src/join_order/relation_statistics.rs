//! Statistics of join relations (DuckDB's `RelationStats` and `RelationStatisticsHelper`):
//! cardinalities and per-column distinct counts that feed the cardinality estimator.

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_catalog::EntryKind;
use rustdb_planner::operator::{COLUMN_IDENTIFIER_ROW_ID, ColumnIndex, LogicalAggregate, LogicalGet};
use rustdb_planner::{
    ClientContext, ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator,
};
use rustdb_storage::TableFilter;
use rustdb_storage::statistics::BaseStatistics;
use rustdb_types::compare::CompareOp;

/// The selectivity assumed for a filter without statistics (DuckDB's
/// `RelationStatisticsHelper::DEFAULT_SELECTIVITY`).
pub const DEFAULT_SELECTIVITY: f64 = 0.2;

/// A column's estimated distinct count and whether it came from a `HyperLogLog` (DuckDB's
/// `DistinctCount`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DistinctCount {
    pub distinct_count: u64,
    pub from_hll: bool,
}

/// What the join order optimizer knows of a relation (DuckDB's `RelationStats`).
#[derive(Clone, Debug)]
pub struct RelationStats {
    pub column_distinct_count: Vec<DistinctCount>,
    pub cardinality: u64,
    pub filter_strength: f64,
    pub stats_initialized: bool,
    pub column_names: Vec<String>,
    pub table_name: String,
}

impl Default for RelationStats {
    fn default() -> Self {
        Self {
            column_distinct_count: Vec::new(),
            cardinality: 1,
            filter_strength: 1.0,
            stats_initialized: false,
            column_names: Vec::new(),
            table_name: String::new(),
        }
    }
}

/// The statistics of a scanned column of a base table (DuckDB's table scan
/// `statistics_extended`): none while the transaction has uncommitted changes to the
/// table, none for the row id and generated columns.
pub fn table_column_statistics(
    context: &ClientContext,
    get: &LogicalGet,
    column_index: &ColumnIndex,
) -> Option<BaseStatistics> {
    let column = column_index.index;
    if column == COLUMN_IDENTIFIER_ROW_ID {
        return None;
    }
    if get.table.is_none() {
        // A table function's own statistics (`get.function.statistics`).
        let statistics = get.function.statistics.as_ref()?;
        return statistics(get.bind_data.as_deref(), column)
            .and_then(|stats| stats.downcast::<BaseStatistics>().ok())
            .map(|stats| *stats);
    }
    let entry = get.table.as_ref()?;
    let EntryKind::Table(table) = &entry.kind else {
        return None;
    };
    let storage = table.storage.as_ref()?;
    if context.transaction.local_storage().added_rows(storage) > 0 {
        return None;
    }
    let physical = table.logical_to_physical(usize::try_from(column).ok()?)?;
    let stats = storage.column_statistics(physical)?;
    if column_index.is_pushdown_extract() {
        // The statistics of the extracted field (`TableStatistics::CopyStats`).
        return crate::statistics_propagator::pushdown_extract_statistics(&stats, column_index.child_indexes.first()?);
    }
    Some(stats)
}

/// A column's distinct count from the table statistics, 0 if unknown (DuckDB's
/// `RelationStatisticsHelper::GetDistinctCount`).
fn distinct_count(context: &ClientContext, get: &LogicalGet, column: &ColumnIndex) -> u64 {
    table_column_statistics(context, get, column).map_or(0, |stats| stats.distinct_count())
}

/// The rows an equality filter keeps, by the column's distinct count (DuckDB's
/// `InspectTableFilter`).
fn inspect_table_filter(cardinality: u64, filter: &TableFilter, stats: &BaseStatistics) -> u64 {
    match filter {
        TableFilter::And(children) => children
            .iter()
            .map(|child| inspect_table_filter(cardinality, child, stats))
            .fold(cardinality, u64::min),
        TableFilter::ConstantComparison {
            op: CompareOp::Equal, ..
        } => {
            let column_count = stats.distinct_count();
            if column_count > 0 {
                cardinality.div_ceil(column_count)
            } else {
                cardinality
            }
        }
        _ => cardinality,
    }
}

/// DuckDB's `RelationStatisticsHelper::ExtractGetStats`; also sets the scan's estimate.
pub fn extract_get_stats(op: &mut LogicalOperator, context: &ClientContext) -> RelationStats {
    let base_table_cardinality = op.estimate_cardinality();
    let LogicalKind::Get(get) = &mut op.kind else {
        return RelationStats::default();
    };
    let mut stats = RelationStats::default();
    let mut cardinality_after_filters = base_table_cardinality;
    let name = get
        .table
        .as_ref()
        .map_or_else(|| "some table".to_owned(), |table| table.name().to_owned());
    if get.table.is_some() {
        stats.table_name.clone_from(&name);
    }
    for column in &get.column_ids {
        let count = distinct_count(context, get, column);
        let column_name = get
            .names
            .get(column.index as usize)
            .cloned()
            .unwrap_or_else(|| "column".to_owned());
        if count > 0 {
            stats.column_distinct_count.push(DistinctCount {
                distinct_count: count,
                from_hll: true,
            });
            stats.column_names.push(format!("{name}.{column_name}"));
        } else {
            stats.column_distinct_count.push(DistinctCount {
                distinct_count: cardinality_after_filters,
                from_hll: false,
            });
            stats.column_names.push(format!(
                "{}.{column_name}",
                get.function.name.to_ascii_uppercase()
            ));
        }
    }
    if !get.table_filters.is_empty() {
        let mut has_non_optional_filters = false;
        for (&column, filter) in &get.table_filters.filters {
            if let Some(column_stats) = table_column_statistics(context, get, &ColumnIndex::new(column as u64)) {
                let with_filter = inspect_table_filter(base_table_cardinality, filter, &column_stats);
                cardinality_after_filters = cardinality_after_filters.min(with_filter);
            }
            if !filter.is_optional() {
                has_non_optional_filters = true;
            }
        }
        let has_equality_filter = cardinality_after_filters != base_table_cardinality;
        if !has_equality_filter && has_non_optional_filters {
            cardinality_after_filters = ((base_table_cardinality as f64 * DEFAULT_SELECTIVITY) as u64).max(1);
        }
        if base_table_cardinality == 0 {
            cardinality_after_filters = 0;
        }
    }
    stats.cardinality = cardinality_after_filters;
    op.estimated_cardinality = Some(cardinality_after_filters);
    stats.stats_initialized = true;
    stats
}

/// The first column reference (or constant) an expression is computed from (DuckDB's
/// `GetChildColumnBinding`): `(binding, is_constant)`.
fn child_column_binding(expression: &Expression) -> Option<(Option<ColumnBinding>, bool)> {
    match &expression.kind {
        ExpressionKind::Function(function) if function.children.is_empty() => return Some((None, true)),
        ExpressionKind::ColumnRef(column) => return Some((Some(column.binding), false)),
        ExpressionKind::LambdaRef(_)
        | ExpressionKind::Constant(_)
        | ExpressionKind::Default
        | ExpressionKind::Parameter(_)
        | ExpressionKind::Reference(_) => return Some((None, true)),
        _ => {}
    }
    let mut result = None;
    for child in expression.children() {
        if matches!(result, Some((Some(_), false))) {
            break;
        }
        if let Some(found) = child_column_binding(child) {
            result = Some(found);
        }
    }
    result
}

/// DuckDB's `ExtractProjectionStats`.
pub fn extract_projection_stats(op: &LogicalOperator, child_stats: &RelationStats) -> RelationStats {
    let mut stats = RelationStats {
        cardinality: child_stats.cardinality,
        table_name: op.explain_name(),
        ..RelationStats::default()
    };
    for expression in &op.expressions {
        stats.column_names.push(expression.name());
        let distinct = match child_column_binding(expression) {
            Some((_, true)) => DistinctCount {
                distinct_count: 1,
                from_hll: true,
            },
            found => {
                let column_index = found
                    .and_then(|(binding, _)| binding)
                    .map_or(usize::MAX, |binding| binding.column_index as usize);
                if column_index >= child_stats.column_distinct_count.len()
                    && expression.to_string() == "count_star()"
                {
                    DistinctCount {
                        distinct_count: 1,
                        from_hll: true,
                    }
                } else if let Some(count) = child_stats.column_distinct_count.get(column_index) {
                    *count
                } else {
                    DistinctCount {
                        distinct_count: stats.cardinality,
                        from_hll: false,
                    }
                }
            }
        };
        stats.column_distinct_count.push(distinct);
    }
    stats.stats_initialized = true;
    stats
}

/// Stats with one distinct-count entry per output column (DuckDB's
/// `ExtractDummyScanStats` / `ExtractExpressionGetStats` / `ExtractEmptyResultStats`).
pub fn uniform_stats(
    op: &LogicalOperator,
    cardinality: u64,
    distinct: u64,
    name: &str,
    table: &str,
) -> RelationStats {
    let columns = op.column_bindings().len();
    RelationStats {
        column_distinct_count: vec![
            DistinctCount {
                distinct_count: distinct,
                from_hll: false,
            };
            columns
        ],
        cardinality,
        column_names: vec![name.to_owned(); columns],
        table_name: table.to_owned(),
        stats_initialized: true,
        ..RelationStats::default()
    }
}

/// DuckDB's `ExtractWindowStats`.
pub fn extract_window_stats(op: &LogicalOperator, child_stats: &RelationStats) -> RelationStats {
    let mut stats = RelationStats {
        cardinality: child_stats.cardinality,
        column_distinct_count: child_stats.column_distinct_count.clone(),
        column_names: child_stats.column_names.clone(),
        stats_initialized: true,
        ..RelationStats::default()
    };
    let columns = op.column_bindings().len();
    for _ in child_stats.column_distinct_count.len()..columns {
        stats.column_distinct_count.push(DistinctCount {
            distinct_count: child_stats.cardinality,
            from_hll: false,
        });
        stats.column_names.push("window".to_owned());
    }
    stats
}

/// DuckDB's `ExtractAggregationStats`: the group count estimated from the distinct
/// counts of the group columns ("occupancy problem").
pub fn extract_aggregation_stats(
    aggregate: &LogicalAggregate,
    bindings: &[ColumnBinding],
    child_stats: &RelationStats,
) -> RelationStats {
    let mut distinct_counts: Vec<f64> = Vec::new();
    for grouping_set in &aggregate.grouping_sets {
        let mut set_counts = Vec::new();
        for &index in grouping_set {
            let Some(column) = aggregate
                .groups
                .get(index as usize)
                .and_then(Expression::as_column_ref)
            else {
                continue;
            };
            let Some(count) = child_stats
                .column_distinct_count
                .get(column.binding.column_index as usize)
            else {
                continue;
            };
            let count = count.distinct_count as f64;
            set_counts.push(if count == 0.0 { 1.0 } else { count });
        }
        if set_counts.len() > distinct_counts.len() {
            distinct_counts = set_counts;
        }
    }
    let child_cardinality = child_stats.cardinality as f64;
    let new_card = if distinct_counts.is_empty() {
        child_cardinality / 2.0
    } else {
        let mut product: f64 = distinct_counts.iter().product();
        product *= 0.95f64.powf((distinct_counts.len() - 1) as f64);
        let mult = 1.0 - (-child_cardinality / product).exp();
        let estimate = if mult == 0.0 {
            child_cardinality
        } else {
            product * mult
        };
        estimate.min(child_cardinality)
    };
    let mut stats = RelationStats {
        cardinality: if aggregate.groups.is_empty() {
            1
        } else {
            new_card as u64
        },
        column_distinct_count: child_stats.column_distinct_count.clone(),
        column_names: child_stats.column_names.clone(),
        stats_initialized: true,
        ..RelationStats::default()
    };
    for (column_index, binding) in bindings.iter().enumerate() {
        if binding.table_index == aggregate.group_index && column_index < distinct_counts.len() {
            stats.column_distinct_count.push(DistinctCount {
                distinct_count: distinct_counts[column_index] as u64,
                from_hll: true,
            });
        } else {
            stats.column_distinct_count.push(DistinctCount {
                distinct_count: child_stats.cardinality,
                from_hll: false,
            });
        }
        stats.column_names.push("aggregate".to_owned());
    }
    stats
}

/// The stats of a reordered join tree: the columns of all relations, the largest
/// cardinality (DuckDB's `CombineStatsOfReorderableOperator`).
pub fn combine_stats_of_reorderable_operator(relation_stats: &[RelationStats]) -> RelationStats {
    let mut stats = RelationStats::default();
    let mut max_card = 0;
    for child in relation_stats {
        stats
            .column_distinct_count
            .extend_from_slice(&child.column_distinct_count);
        stats.column_names.extend(child.column_names.iter().cloned());
        stats.table_name.push_str("joined with ");
        stats.table_name.push_str(&child.table_name);
        max_card = max_card.max(child.cardinality);
    }
    stats.stats_initialized = true;
    stats.cardinality = max_card;
    stats
}

/// DuckDB's `CombineStatsOfNonReorderableOperator`.
pub fn combine_stats_of_non_reorderable_operator(
    op: &LogicalOperator,
    child_stats: &[RelationStats],
) -> RelationStats {
    let mut stats = RelationStats {
        cardinality: 0,
        ..RelationStats::default()
    };
    let child_cardinalities: Vec<u64> = child_stats
        .iter()
        .map(|child| {
            if child.stats_initialized {
                child.cardinality
            } else {
                0
            }
        })
        .collect();
    stats.cardinality = child_cardinalities.iter().copied().max().unwrap_or(0);
    match &op.kind {
        LogicalKind::ComparisonJoin(join) if child_cardinalities.len() == 2 => match join.join_type {
            JoinType::RightAnti | JoinType::RightSemi => stats.cardinality = child_cardinalities[1],
            JoinType::Anti | JoinType::Semi | JoinType::Single | JoinType::Mark => {
                stats.cardinality = child_cardinalities[0];
            }
            _ => {}
        },
        LogicalKind::SetOperation {
            operator_type,
            setop_all,
            ..
        } => match operator_type {
            rustdb_planner::LogicalOperatorType::LogicalUnion if *setop_all => {
                stats.cardinality = child_cardinalities.iter().sum();
            }
            rustdb_planner::LogicalOperatorType::LogicalIntersect if child_cardinalities.len() == 2 => {
                stats.cardinality = child_cardinalities[0].min(child_cardinalities[1]);
            }
            rustdb_planner::LogicalOperatorType::LogicalExcept if child_cardinalities.len() == 2 => {
                stats.cardinality = child_cardinalities[0];
            }
            _ => {}
        },
        _ => {}
    }
    stats.stats_initialized = true;
    stats.filter_strength = 1.0;
    for child in child_stats {
        if !stats.table_name.is_empty() {
            stats.table_name.push_str(" joined with ");
        }
        stats.table_name.push_str(&child.table_name);
        if !child.stats_initialized {
            continue;
        }
        stats
            .column_distinct_count
            .extend_from_slice(&child.column_distinct_count);
        stats.column_names.extend(child.column_names.iter().cloned());
    }
    stats
}

/// The comparison type of a join filter, if it contains one.
pub fn comparison_type(filter: &Expression) -> Option<ExpressionType> {
    let mut found = None;
    crate::expression_util::enumerate_expression(filter, &mut |expression| {
        if matches!(expression.kind, ExpressionKind::Comparison(_)) {
            found = Some(expression.expression_type);
        }
    });
    found
}
