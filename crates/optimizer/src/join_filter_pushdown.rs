//! Finding the table scans that a run-time filter on some columns can be pushed into
//! (DuckDB's `JoinFilterPushdownOptimizer::GetPushdownFilterTargets`).

use rustdb_planner::{
    ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};
use rustdb_types::{LogicalType, LogicalTypeId};

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
fn pushdown_expression(expression: &Expression, column: &mut PushdownColumn, allow_casts: bool) -> bool {
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

/// Collects the scans below `op` that a filter on `columns` can be pushed into.
pub fn get_pushdown_filter_targets(
    op: &LogicalOperator,
    columns: Vec<PushdownColumn>,
    allow_casts: bool,
    path: &mut Vec<usize>,
    targets: &mut Vec<PushdownFilterTarget>,
) {
    let mut recurse =
        |child: usize, columns: Vec<PushdownColumn>, targets: &mut Vec<PushdownFilterTarget>| {
            if let Some(node) = op.children.get(child) {
                path.push(child);
                get_pushdown_filter_targets(node, columns, allow_casts, path, targets);
                path.pop();
            }
        };
    match op.operator_type() {
        LogicalOperatorType::LogicalLimit
        | LogicalOperatorType::LogicalFilter
        | LogicalOperatorType::LogicalOrderBy
        | LogicalOperatorType::LogicalTopN
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
