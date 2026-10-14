use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_planner::{LogicalKind, LogicalOperator};

use crate::operator::PhysicalRef;
use crate::operators::{PhysicalDummyScan, PhysicalProjection};

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
        LogicalKind::Filter => return Err(Error::not_implemented("physical filter")),
    };
    Ok(plan)
}
