//! Turning `ORDER BY` + `LIMIT` into a Top-N (DuckDB's `TopN` optimizer).

use rustdb_planner::{BoundLimitNode, LogicalKind, LogicalOperator, LogicalOperatorType};

/// Whether `op` is a constant LIMIT (with an optional constant OFFSET) over an ORDER BY,
/// possibly through projections, that is cheaper as a Top-N (DuckDB's
/// `TopN::CanOptimize`; the child's cardinality is used when already estimated).
pub fn can_optimize(op: &LogicalOperator) -> bool {
    let LogicalKind::Limit { limit, offset } = &op.kind else {
        return false;
    };
    let BoundLimitNode::ConstantValue(limit) = limit else {
        return false;
    };
    let offset = match offset {
        BoundLimitNode::ExpressionValue(_) => return false,
        BoundLimitNode::ConstantValue(offset) => Some(*offset),
        _ => None,
    };
    let Some(mut child) = op.children.first() else {
        return false;
    };
    if let Some(child_cardinality) = child.estimated_cardinality {
        // Above 0.7% of the input (and 5000 rows) sorting everything is faster.
        #[allow(clippy::cast_precision_loss)]
        let constant_limit = *limit as f64 + offset.map_or(0.0, |offset| offset as f64);
        #[allow(clippy::cast_precision_loss)]
        let child_cardinality = child_cardinality as f64;
        if constant_limit > child_cardinality * 0.007 && constant_limit > 5000.0 {
            return false;
        }
    }
    while child.operator_type() == LogicalOperatorType::LogicalProjection {
        let Some(next) = child.children.first() else {
            return false;
        };
        child = next;
    }
    child.operator_type() == LogicalOperatorType::LogicalOrderBy
}
