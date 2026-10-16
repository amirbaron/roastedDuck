//! Finding the table scans that a run-time filter on some columns can be pushed into
//! (DuckDB's `JoinFilterPushdownOptimizer::GetPushdownFilterTargets`).

use rustdb_planner::{
    ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};
use rustdb_types::{LogicalType, LogicalTypeId};

use std::sync::Arc;

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_planner::operator::{JoinFilter, JoinFilterTarget};
use rustdb_storage::TableFilter;
use rustdb_storage::table_filter::DynamicFilterData;

use crate::filter_pushdown::supports_filter_pushdown;

/// A column a run-time filter applies to (DuckDB's `JoinFilterPushdownColumn`).
#[derive(Clone, Debug)]
pub struct PushdownColumn {
    /// The column as seen by the operator being searched.
    pub binding: ColumnBinding,
    /// The scan's type of the column, once a scan is reached.
    pub storage_type: Option<LogicalType>,
}

/// A scan that can receive a run-time filter (DuckDB's `PushdownFilterTarget`).
#[derive(Clone, Debug)]
pub struct PushdownFilterTarget {
    /// The child indexes leading from the searched operator to the scan.
    pub path: Vec<usize>,
    pub columns: Vec<PushdownColumn>,
}

/// Whether a filter on `expression` can be pushed through it; updates `column` to the
/// column below (DuckDB's `PushdownJoinFilterExpression`). Casts are only crossed when
/// `allow_casts` is set.
pub fn pushdown_expression(expression: &Expression, column: &mut PushdownColumn, allow_casts: bool) -> bool {
    let ty = &expression.return_type;
    if is_nested(ty) || ty.id() == LogicalTypeId::Interval {
        return false;
    }
    match &expression.kind {
        ExpressionKind::ColumnRef(reference) => {
            column.binding = reference.binding;
            true
        }
        ExpressionKind::Cast(cast) if allow_casts => {
            let source = &cast.child.return_type;
            let small_integral = |ty: &LogicalType| {
                is_integral(ty) && ty.physical_type().size() <= rustdb_types::PhysicalType::Int64.size()
            };
            if !small_integral(source) || !small_integral(ty) {
                return false;
            }
            pushdown_expression(&cast.child, column, allow_casts)
        }
        _ => false,
    }
}

/// DuckDB's `LogicalType::IsNested`.
fn is_nested(ty: &LogicalType) -> bool {
    matches!(
        ty.id(),
        LogicalTypeId::Struct
            | LogicalTypeId::List
            | LogicalTypeId::Map
            | LogicalTypeId::Array
            | LogicalTypeId::Union
    )
}

/// DuckDB's `LogicalType::IsIntegral`.
fn is_integral(ty: &LogicalType) -> bool {
    matches!(
        ty.id(),
        LogicalTypeId::TinyInt
            | LogicalTypeId::SmallInt
            | LogicalTypeId::Integer
            | LogicalTypeId::BigInt
            | LogicalTypeId::UTinyInt
            | LogicalTypeId::USmallInt
            | LogicalTypeId::UInteger
            | LogicalTypeId::UBigInt
            | LogicalTypeId::HugeInt
            | LogicalTypeId::UHugeInt
    )
}

/// How a run-time filter may be pushed down.
#[derive(Clone, Copy, Debug)]
pub struct PushdownOptions {
    /// Whether integral casts may be crossed (the filter value is cast to the scan type).
    pub allow_casts: bool,
    /// Whether LIMIT and Top-N may be crossed: only for filters that cannot change which
    /// rows those keep (the Top-N's own boundary).
    pub through_limits: bool,
}

/// Collects the scans below `op` that a filter on `columns` can be pushed into.
pub fn get_pushdown_filter_targets(
    op: &LogicalOperator,
    columns: Vec<PushdownColumn>,
    options: PushdownOptions,
    path: &mut Vec<usize>,
    targets: &mut Vec<PushdownFilterTarget>,
) {
    let allow_casts = options.allow_casts;
    let mut recurse =
        |child: usize, columns: Vec<PushdownColumn>, targets: &mut Vec<PushdownFilterTarget>| {
            if let Some(node) = op.children.get(child) {
                path.push(child);
                get_pushdown_filter_targets(node, columns, options, path, targets);
                path.pop();
            }
        };
    match op.operator_type() {
        LogicalOperatorType::LogicalLimit | LogicalOperatorType::LogicalTopN => {
            if options.through_limits {
                recurse(0, columns, targets);
            }
        }
        LogicalOperatorType::LogicalFilter
        | LogicalOperatorType::LogicalOrderBy
        | LogicalOperatorType::LogicalDistinct
        | LogicalOperatorType::LogicalComparisonJoin
        | LogicalOperatorType::LogicalCrossProduct => recurse(0, columns, targets),
        LogicalOperatorType::LogicalUnnest => {
            if let LogicalKind::Unnest { unnest_index } = op.kind
                && columns
                    .iter()
                    .any(|column| column.binding.table_index == unnest_index)
            {
                return;
            }
            recurse(0, columns, targets);
        }
        LogicalOperatorType::LogicalExcept
        | LogicalOperatorType::LogicalIntersect
        | LogicalOperatorType::LogicalUnion => {
            let LogicalKind::SetOperation { table_index, .. } = op.kind else {
                return;
            };
            if columns
                .iter()
                .any(|column| column.binding.table_index != table_index)
            {
                return;
            }
            for (index, child) in op.children.iter().enumerate() {
                let bindings = child.column_bindings();
                let child_columns: Option<Vec<PushdownColumn>> = columns
                    .iter()
                    .map(|column| {
                        bindings
                            .get(column.binding.column_index as usize)
                            .map(|&binding| PushdownColumn {
                                binding,
                                storage_type: None,
                            })
                    })
                    .collect();
                let Some(child_columns) = child_columns else {
                    return;
                };
                recurse(index, child_columns, targets);
                if op.operator_type() == LogicalOperatorType::LogicalExcept {
                    break;
                }
            }
        }
        LogicalOperatorType::LogicalGet => {
            let LogicalKind::Get(get) = &op.kind else {
                return;
            };
            if !supports_filter_pushdown(get) {
                return;
            }
            let bindings = op.column_bindings();
            let mut columns = columns;
            for column in &mut columns {
                if column.binding.table_index != get.table_index {
                    return;
                }
                let position = bindings
                    .iter()
                    .position(|binding| binding.column_index == column.binding.column_index);
                column.storage_type = position
                    .and_then(|_| get.column_ids.get(column.binding.column_index as usize))
                    .map(|id| get.column_type(id));
            }
            targets.push(PushdownFilterTarget {
                path: path.clone(),
                columns,
            });
        }
        LogicalOperatorType::LogicalProjection => {
            let LogicalKind::Projection { table_index } = op.kind else {
                return;
            };
            let mut columns = columns;
            for column in &mut columns {
                if column.binding.table_index != table_index {
                    return;
                }
                let Some(expression) = op.expressions.get(column.binding.column_index as usize) else {
                    return;
                };
                if !pushdown_expression(expression, column, allow_casts) {
                    return;
                }
            }
            recurse(0, columns, targets);
        }
        LogicalOperatorType::LogicalAggregateAndGroupBy => {
            let LogicalKind::Aggregate(aggregate) = &op.kind else {
                return;
            };
            let mut columns = columns;
            for column in &mut columns {
                if column.binding.table_index != aggregate.group_index {
                    return;
                }
                let Some(expression) = aggregate.groups.get(column.binding.column_index as usize) else {
                    return;
                };
                if !pushdown_expression(expression, column, allow_casts) {
                    return;
                }
            }
            recurse(0, columns, targets);
        }
        _ => {}
    }
}

/// The operator at `path` below `op`.
pub fn operator_at_path<'a>(op: &'a mut LogicalOperator, path: &[usize]) -> Option<&'a mut LogicalOperator> {
    let mut node = op;
    for &index in path {
        node = node.children.get_mut(index)?;
    }
    Some(node)
}

/// Sets up the run-time filters of every hash join in `plan` (DuckDB's
/// `JoinFilterPushdownOptimizer`): the probe-side scans get `>= min` / `<= max` filters
/// that the join fills from its build keys.
pub fn push_join_filters(plan: &mut LogicalOperator) {
    if let LogicalKind::ComparisonJoin(join) = &plan.kind
        && join.operator_type == LogicalOperatorType::LogicalComparisonJoin
    {
        generate_join_filters(plan);
    }
    for child in &mut plan.children {
        push_join_filters(child);
    }
}

/// Whether the join type lets the probe side be filtered by the build keys: MARK/SINGLE
/// cannot change the probe cardinality, LEFT/OUTER keep every probe row.
const fn join_type_allows_filters(join_type: JoinType) -> bool {
    !matches!(
        join_type,
        JoinType::Mark
            | JoinType::Single
            | JoinType::Left
            | JoinType::Outer
            | JoinType::Anti
            | JoinType::RightAnti
            | JoinType::RightSemi
    )
}

/// A filter used to skip row groups and segments only (DuckDB pushes join filters as
/// `SelectivityOptionalFilter`s, which stop filtering rows when they are not selective).
fn optional(filter: TableFilter) -> TableFilter {
    TableFilter::Optional(Some(Box::new(filter)))
}

/// DuckDB's `GenerateJoinFilters`.
fn generate_join_filters(op: &mut LogicalOperator) {
    let LogicalKind::ComparisonJoin(join) = &mut op.kind else {
        return;
    };
    if !join_type_allows_filters(join.join_type) {
        return;
    }
    // Equalities first, as the physical join orders them, so the indexes stay valid.
    let (equal, other): (Vec<_>, Vec<_>) =
        std::mem::take(&mut join.conditions)
            .into_iter()
            .partition(|condition| {
                matches!(
                    condition.comparison,
                    ExpressionType::CompareEqual | ExpressionType::CompareNotDistinctFrom
                )
            });
    join.conditions = equal;
    join.conditions.extend(other);
    let mut conditions = Vec::new();
    let mut columns = Vec::new();
    for (index, condition) in join.conditions.iter().enumerate() {
        if !matches!(
            condition.comparison,
            ExpressionType::CompareEqual
                | ExpressionType::CompareLessthan
                | ExpressionType::CompareLessthanorequalto
                | ExpressionType::CompareGreaterthan
                | ExpressionType::CompareGreaterthanorequalto
        ) {
            continue;
        }
        let mut column = PushdownColumn {
            binding: ColumnBinding::default(),
            storage_type: None,
        };
        if !pushdown_expression(&condition.left, &mut column, true) {
            continue;
        }
        conditions.push((index, condition.comparison));
        columns.push(column);
    }
    if columns.is_empty() {
        return;
    }
    let mut targets = Vec::new();
    let mut path = vec![0];
    if let Some(probe) = op.children.first() {
        let options = PushdownOptions {
            allow_casts: true,
            through_limits: false,
        };
        get_pushdown_filter_targets(probe, columns, options, &mut path, &mut targets);
    }
    if targets.is_empty() {
        return;
    }
    let mut join_filters: Vec<JoinFilter> = conditions
        .iter()
        .map(|&(condition, _)| JoinFilter {
            condition,
            targets: Vec::new(),
        })
        .collect();
    for target in targets {
        let Some(LogicalKind::Get(get)) = operator_at_path(op, &target.path).map(|node| &mut node.kind)
        else {
            continue;
        };
        for (filter_index, column) in target.columns.iter().enumerate() {
            let Some(storage_type) = column.storage_type.clone() else {
                continue;
            };
            let Some(column_id) = get.column_ids.get(column.binding.column_index as usize) else {
                continue;
            };
            let key = usize::try_from(column_id.index).unwrap_or(usize::MAX);
            let min = Arc::new(DynamicFilterData::default());
            let max = Arc::new(DynamicFilterData::default());
            let comparison = conditions[filter_index].1;
            // `probe > build` needs only the lower bound, `probe < build` only the upper.
            if !matches!(
                comparison,
                ExpressionType::CompareLessthan | ExpressionType::CompareLessthanorequalto
            ) {
                get.dynamic_filters
                    .push_filter(key, optional(TableFilter::Dynamic(Arc::clone(&min))));
            }
            if !matches!(
                comparison,
                ExpressionType::CompareGreaterthan | ExpressionType::CompareGreaterthanorequalto
            ) {
                get.dynamic_filters
                    .push_filter(key, optional(TableFilter::Dynamic(Arc::clone(&max))));
            }
            join_filters[filter_index].targets.push(JoinFilterTarget {
                storage_type,
                min,
                max,
            });
        }
    }
    if let LogicalKind::ComparisonJoin(join) = &mut op.kind {
        join.join_filters = join_filters
            .into_iter()
            .filter(|filter| !filter.targets.is_empty())
            .collect();
    }
}
