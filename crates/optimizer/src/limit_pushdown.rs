//! Moving small constant LIMITs below projections (DuckDB's `LimitPushdown`), so that
//! the projection only computes the rows that are returned.

use roastedduck_planner::{BoundLimitNode, LogicalKind, LogicalOperator, LogicalOperatorType};

/// Whether `op` is a LIMIT below 8192 rows over a projection that can run after it
/// (DuckDB's `LimitPushdown::CanOptimize`).
fn can_optimize(op: &LogicalOperator) -> bool {
    let LogicalKind::Limit { limit, offset } = &op.kind else {
        return false;
    };
    let Some(projection) = op.children.first() else {
        return false;
    };
    if projection.operator_type() != LogicalOperatorType::LogicalProjection {
        return false;
    }
    if matches!(
        offset,
        BoundLimitNode::ExpressionPercentage(_) | BoundLimitNode::ExpressionValue(_)
    ) {
        return false;
    }
    let BoundLimitNode::ConstantValue(limit) = limit else {
        return false;
    };
    if *limit >= 8192 {
        return false;
    }
    // With an offset, volatile expressions would be computed for different rows.
    if matches!(offset, BoundLimitNode::ConstantValue(offset) if *offset > 0)
        && projection
            .expressions
            .iter()
            .any(roastedduck_planner::Expression::is_volatile)
    {
        return false;
    }
    true
}

/// Pushes LIMITs below projections throughout `op` (DuckDB's `LimitPushdown::Optimize`).
pub fn optimize(mut op: LogicalOperator) -> LogicalOperator {
    if can_optimize(&op)
        && let Some(mut projection) = op.children.pop()
    {
        op.children = std::mem::take(&mut projection.children);
        projection.estimated_cardinality = Some(op.estimated_cardinality.unwrap_or(0));
        projection.children = vec![op];
        op = projection;
    }
    op.children = std::mem::take(&mut op.children)
        .into_iter()
        .map(optimize)
        .collect();
    op
}
