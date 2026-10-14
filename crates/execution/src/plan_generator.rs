use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_planner::{ConjunctionOp, Expression, LogicalKind, LogicalOperator};

use crate::operator::PhysicalRef;
use crate::operators::{PhysicalColumnDataScan, PhysicalDummyScan, PhysicalFilter, PhysicalProjection};

/// Converts a logical plan into a physical plan (DuckDB's `PhysicalPlanGenerator`).
pub fn create_physical_plan(op: LogicalOperator) -> Result<PhysicalRef> {
    let LogicalOperator {
        kind,
        children,
        expressions,
        types,
        ..
    } = op;
    let mut children = children
        .into_iter()
        .map(create_physical_plan)
        .collect::<Result<Vec<_>>>()?
        .into_iter();
    let plan: PhysicalRef = match kind {
        LogicalKind::DummyScan { .. } => Arc::new(PhysicalDummyScan::new(types)),
        LogicalKind::Projection { .. } => {
            let child = children
                .next()
                .ok_or_else(|| Error::internal("projection without input"))?;
            Arc::new(PhysicalProjection::new(types, expressions, child))
        }
        LogicalKind::Filter => {
            let child = children
                .next()
                .ok_or_else(|| Error::internal("filter without input"))?;
            let mut expressions = expressions;
            let predicate = if expressions.len() == 1 {
                expressions.remove(0)
            } else {
                Expression::Conjunction {
                    op: ConjunctionOp::And,
                    children: expressions,
                }
            };
            Arc::new(PhysicalFilter::new(types, predicate, child))
        }
        LogicalKind::ColumnDataGet { chunks, .. } => Arc::new(PhysicalColumnDataScan::new(types, chunks)),
    };
    Ok(plan)
}
