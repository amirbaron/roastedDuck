use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_planner::{BoundLimit, ConjunctionOp, Expression, LogicalKind, LogicalOperator};

use crate::operator::PhysicalRef;
use crate::operators::{
    PhysicalColumnDataScan, PhysicalDummyScan, PhysicalFilter, PhysicalProjection, PhysicalStreamingLimit,
    PhysicalUngroupedAggregate,
};

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
        LogicalKind::Aggregate { groups, .. } => {
            let child = children
                .next()
                .ok_or_else(|| Error::internal("aggregate without input"))?;
            if !groups.is_empty() {
                return Err(Error::not_implemented("GROUP BY (hash aggregate)"));
            }
            let aggregates = expressions
                .into_iter()
                .map(|expression| match expression {
                    Expression::Aggregate(aggregate) => Ok(aggregate),
                    other => Err(Error::internal(format!(
                        "non-aggregate {other:?} in an aggregate operator"
                    ))),
                })
                .collect::<Result<Vec<_>>>()?;
            Arc::new(PhysicalUngroupedAggregate::new(types, aggregates, child)?)
        }
        LogicalKind::Limit { limit, offset } => {
            let child = children
                .next()
                .ok_or_else(|| Error::internal("limit without input"))?;
            let constant = |bound: &BoundLimit, unset: u64| match bound {
                BoundLimit::Unset => Ok(unset),
                BoundLimit::Constant(value) => Ok(*value),
                _ => Err(Error::not_implemented("non-constant or percentage LIMIT/OFFSET")),
            };
            Arc::new(PhysicalStreamingLimit::new(
                types,
                constant(&limit, u64::MAX)?,
                constant(&offset, 0)?,
                child,
            ))
        }
    };
    Ok(plan)
}
