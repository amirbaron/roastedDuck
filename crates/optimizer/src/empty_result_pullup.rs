//! Propagating empty results up the plan (DuckDB's `EmptyResultPullup`): an operator
//! over an input that produces no rows produces none either, as do inner and semi joins
//! with an empty side.

use roastedduck_ast::JoinType;
use roastedduck_common::Result;
use roastedduck_planner::{LogicalKind, LogicalOperator, LogicalOperatorType};

use crate::filter_pushdown::empty_result;

fn is_empty_result(op: &LogicalOperator) -> bool {
    op.operator_type() == LogicalOperatorType::LogicalEmptyResult
}

/// DuckDB's `PullUpEmptyJoinChildren`.
fn pull_up_empty_join_children(mut op: LogicalOperator) -> Result<LogicalOperator> {
    let join_type = match &op.kind {
        LogicalKind::ComparisonJoin(join) => join.join_type,
        LogicalKind::AnyJoin { join_type, .. } => *join_type,
        _ if op.operator_type() == LogicalOperatorType::LogicalExcept => JoinType::Anti,
        _ => return Ok(op),
    };
    match join_type {
        JoinType::Semi | JoinType::Inner => {
            if op.children.iter().any(is_empty_result) {
                return empty_result(op);
            }
        }
        JoinType::Anti => {
            // An anti join with an empty right side is its left side.
            if op.children.get(1).is_some_and(is_empty_result)
                && op.operator_type() != LogicalOperatorType::LogicalExcept
            {
                return Ok(op.children.swap_remove(0));
            }
            if op.children.first().is_some_and(is_empty_result) {
                return empty_result(op);
            }
        }
        JoinType::Mark | JoinType::Single | JoinType::Left
            if op.children.first().is_some_and(is_empty_result) =>
        {
            return empty_result(op);
        }
        _ => {}
    }
    Ok(op)
}

/// Pulls empty results up through `op`, children first (DuckDB's
/// `EmptyResultPullup::Optimize`).
pub fn pull_up_empty_results(mut op: LogicalOperator) -> Result<LogicalOperator> {
    op.children = std::mem::take(&mut op.children)
        .into_iter()
        .map(pull_up_empty_results)
        .collect::<Result<_>>()?;
    match op.operator_type() {
        LogicalOperatorType::LogicalProjection
        | LogicalOperatorType::LogicalFilter
        | LogicalOperatorType::LogicalDistinct
        | LogicalOperatorType::LogicalWindow
        | LogicalOperatorType::LogicalGet
        | LogicalOperatorType::LogicalIntersect
        | LogicalOperatorType::LogicalPivot
        | LogicalOperatorType::LogicalCrossProduct => {
            if op.children.iter().any(is_empty_result) {
                return empty_result(op);
            }
            Ok(op)
        }
        LogicalOperatorType::LogicalMaterializedCte => {
            if op.children.get(1).is_some_and(is_empty_result) {
                return empty_result(op);
            }
            Ok(op)
        }
        LogicalOperatorType::LogicalExcept
        | LogicalOperatorType::LogicalAnyJoin
        | LogicalOperatorType::LogicalDelimJoin
        | LogicalOperatorType::LogicalAsofJoin
        | LogicalOperatorType::LogicalComparisonJoin => pull_up_empty_join_children(op),
        _ => Ok(op),
    }
}
