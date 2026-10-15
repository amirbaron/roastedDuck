//! Expression analyses and rewrites used when flattening dependent joins (DuckDB's
//! `HasCorrelatedExpressions`, `RewriteCorrelatedExpressions`,
//! `RewriteCorrelatedRecursive`, `RewriteCountAggregates` and `RewriteCTEScan`).

use std::collections::HashMap;

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, Value};

use crate::binder::{CorrelatedColumnInfo, CorrelatedColumns};
use crate::expression::{BoundCaseCheck, BoundCaseExpression, Expression, ExpressionKind};
use crate::operator::{LogicalKind, LogicalOperator};
use crate::{ColumnBinding, ExpressionType};

/// Positions of correlated columns among the duplicate-eliminated columns.
pub type CorrelatedMap = HashMap<ColumnBinding, u64>;

/// Whether `expression` references `correlated` (DuckDB's `HasCorrelatedExpressions`
/// visiting one expression).
fn expression_has_correlation(
    expression: &Expression,
    correlated: &CorrelatedColumns,
    lateral: bool,
    lateral_depth: u64,
) -> Result<bool> {
    let mut found = false;
    match &expression.kind {
        ExpressionKind::ColumnRef(column) if column.depth > lateral_depth => {
            if column.depth > lateral_depth + 1 {
                return Err(if lateral {
                    Error::binder("Invalid lateral depth encountered for an expression")
                } else {
                    Error::internal("Expression with depth > 1 detected in non-lateral join")
                });
            }
            found = correlated.contains(column.binding);
        }
        ExpressionKind::Subquery(subquery) if subquery.is_correlated() => {
            found = correlated
                .iter()
                .any(|column| subquery.binder.correlated_columns.contains(column.binding));
        }
        _ => {}
    }
    for child in expression.children() {
        found |= expression_has_correlation(child, correlated, lateral, lateral_depth)?;
    }
    Ok(found)
}

/// Whether the expressions of `op` itself reference `correlated` (DuckDB's
/// `HasCorrelatedExpressions::VisitOperator`, which does not visit children).
pub fn has_correlated_expressions(
    op: &mut LogicalOperator,
    correlated: &CorrelatedColumns,
    lateral: bool,
    lateral_depth: u64,
) -> Result<bool> {
    let mut found = false;
    op.try_for_each_expression_mut(|expression| {
        found |= expression_has_correlation(expression, correlated, lateral, lateral_depth)?;
        Ok::<(), Error>(())
    })?;
    Ok(found)
}

/// Points a correlated column at the duplicate-eliminated column replacing it.
fn rebind(binding: &mut ColumnBinding, base: ColumnBinding, map: &CorrelatedMap) -> bool {
    match map.get(binding) {
        Some(&offset) => {
            *binding = ColumnBinding::new(base.table_index, base.column_index + offset);
            true
        }
        None => false,
    }
}

fn rebind_correlated_columns(columns: &mut CorrelatedColumns, base: ColumnBinding, map: &CorrelatedMap) {
    for column in columns.iter_mut() {
        rebind(&mut column.binding, base, map);
    }
}

/// The expression part of [`rewrite_correlated_expressions`].
fn rewrite_expression(
    expression: &mut Expression,
    base: ColumnBinding,
    map: &CorrelatedMap,
    lateral_depth: u64,
    recursive: bool,
) {
    match &mut expression.kind {
        ExpressionKind::ColumnRef(column) if column.depth > lateral_depth => {
            if rebind(&mut column.binding, base, map) {
                column.depth = if recursive { column.depth - 1 } else { 0 };
            }
        }
        ExpressionKind::Subquery(subquery) if subquery.is_correlated() => {
            rewrite_correlated_recursive(&mut subquery.subquery.plan, base, map);
        }
        _ => {}
    }
    expression.for_each_child_mut(|child| rewrite_expression(child, base, map, lateral_depth, recursive));
}

/// Replaces correlated column references by the duplicate-eliminated columns (DuckDB's
/// `RewriteCorrelatedExpressions::VisitOperator`): only `op`'s own expressions, unless
/// `recursive`, which also enters the children (one lateral level deeper on the right
/// of dependent joins).
pub fn rewrite_correlated_expressions(
    op: &mut LogicalOperator,
    base: ColumnBinding,
    map: &CorrelatedMap,
    lateral_depth: u64,
    recursive: bool,
) {
    if recursive {
        if matches!(op.kind, LogicalKind::DependentJoin(_)) {
            if let [left, right] = op.children.as_mut_slice() {
                rewrite_correlated_expressions(left, base, map, lateral_depth, true);
                rewrite_correlated_expressions(right, base, map, lateral_depth + 1, true);
            }
        } else {
            for child in &mut op.children {
                rewrite_correlated_expressions(child, base, map, lateral_depth, true);
            }
        }
    }
    if let LogicalKind::DependentJoin(join) = &mut op.kind {
        rebind_correlated_columns(&mut join.correlated_columns, base, map);
    }
    op.for_each_expression_mut(|expression| {
        rewrite_expression(expression, base, map, lateral_depth, recursive)
    });
}

fn rewrite_recursive_expression(expression: &mut Expression, base: ColumnBinding, map: &CorrelatedMap) {
    match &mut expression.kind {
        ExpressionKind::ColumnRef(column) => {
            if column.depth > 0 && rebind(&mut column.binding, base, map) {
                column.depth -= 1;
            }
            return;
        }
        ExpressionKind::Subquery(subquery) => {
            rewrite_correlated_recursive(&mut subquery.subquery.plan, base, map)
        }
        _ => {}
    }
    expression.for_each_child_mut(|child| rewrite_recursive_expression(child, base, map));
}

/// Rewrites correlated references inside a nested subquery's plan (DuckDB's
/// `RewriteCorrelatedRecursive`).
fn rewrite_correlated_recursive(op: &mut LogicalOperator, base: ColumnBinding, map: &CorrelatedMap) {
    if let LogicalKind::DependentJoin(join) = &mut op.kind {
        rebind_correlated_columns(&mut join.correlated_columns, base, map);
    }
    for child in &mut op.children {
        rewrite_correlated_recursive(child, base, map);
    }
    op.for_each_expression_mut(|expression| rewrite_recursive_expression(expression, base, map));
}

/// `CASE WHEN count IS NULL THEN 0 ELSE count END` for references to COUNT aggregates
/// that a LEFT join may turn into NULL (DuckDB's `RewriteCountAggregates`).
pub fn rewrite_count_aggregates(
    op: &mut LogicalOperator,
    replacements: &HashMap<ColumnBinding, u64>,
) -> Result<()> {
    for child in &mut op.children {
        rewrite_count_aggregates(child, replacements)?;
    }
    op.try_for_each_expression_mut(|expression| rewrite_count_expression(expression, replacements))
}

fn rewrite_count_expression(
    expression: &mut Expression,
    replacements: &HashMap<ColumnBinding, u64>,
) -> Result<()> {
    if let Some(column) = expression.as_column_ref()
        && replacements.contains_key(&column.binding)
    {
        let return_type = expression.return_type.clone();
        let reference = expression.clone();
        let is_null = Expression::operator(
            ExpressionType::OperatorIsNull,
            LogicalType::BOOLEAN,
            vec![reference.clone()],
        );
        let zero = Expression::constant(rustdb_function::cast::ValueCast::default_cast_as(
            &Value::integer(0),
            &return_type,
        )?);
        *expression = Expression::new(
            ExpressionType::CaseExpr,
            return_type,
            ExpressionKind::Case(Box::new(BoundCaseExpression {
                case_checks: vec![BoundCaseCheck {
                    when_expr: is_null,
                    then_expr: zero,
                }],
                else_expr: reference,
            })),
        );
        return Ok(());
    }
    expression.try_for_each_child_mut(|child| rewrite_count_expression(child, replacements))
}

/// Which operators [`rewrite_cte_scan`] updates (DuckDB's `CTEScanRewriteMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CteScanRewriteMode {
    /// Only CTE references.
    CteRefOnly,
    /// CTE references and dependent joins over them, appending columns.
    WithNonRecursiveDependentJoins,
    /// CTE references and all dependent joins, prepending columns.
    WithRecursiveDependentJoins,
}

fn contains_cte_ref(op: &LogicalOperator, table_index: u64) -> bool {
    matches!(op.kind, LogicalKind::CteRef { cte_index, .. } if cte_index == table_index)
        || op
            .children
            .iter()
            .any(|child| contains_cte_ref(child, table_index))
}

/// Adds the correlated columns to the references of a correlated CTE (DuckDB's
/// `RewriteCTEScan`).
pub fn rewrite_cte_scan(
    op: &mut LogicalOperator,
    table_index: u64,
    correlated: &CorrelatedColumns,
    mode: CteScanRewriteMode,
) {
    match &mut op.kind {
        LogicalKind::CteRef {
            cte_index,
            chunk_types,
            bound_columns,
            correlated_columns,
            ..
        } => {
            if *cte_index == table_index && *correlated_columns == 0 {
                for column in correlated {
                    chunk_types.push(column.logical_type.clone());
                    bound_columns.push(column.name.clone());
                }
                *correlated_columns += correlated.len() as u64;
            }
        }
        LogicalKind::DependentJoin(_) if mode != CteScanRewriteMode::CteRefOnly => {
            if mode == CteScanRewriteMode::WithNonRecursiveDependentJoins
                && !op
                    .children
                    .iter()
                    .any(|child| contains_cte_ref(child, table_index))
            {
                for child in &mut op.children {
                    rewrite_cte_scan(child, table_index, correlated, mode);
                }
                return;
            }
            if let LogicalKind::DependentJoin(join) = &mut op.kind {
                for column in correlated {
                    if join.correlated_columns.contains(column.binding) {
                        continue;
                    }
                    let column: CorrelatedColumnInfo = column.clone();
                    if mode == CteScanRewriteMode::WithRecursiveDependentJoins {
                        join.correlated_columns.add_column(column);
                    } else {
                        join.correlated_columns.add_column_to_back(column);
                    }
                }
            }
        }
        _ => {}
    }
    for child in &mut op.children {
        rewrite_cte_scan(child, table_index, correlated, mode);
    }
}
