//! Conversion of resolved logical plans into physical plans (DuckDB's
//! `PhysicalPlanGenerator`). Logical operators without a ported physical counterpart
//! raise `Not implemented`.

use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::expression::BoundConjunctionExpression;
use rustdb_planner::operator::LogicalAggregate;
use rustdb_planner::{
    BoundLimitNode, Expression, ExpressionKind, ExpressionType, LogicalKind, LogicalOperator,
    LogicalOperatorType,
};
use rustdb_types::{DataChunk, LogicalType};

use crate::expression_executor::ExpressionExecutor;
use crate::operator::PhysicalRef;
use crate::operators::{
    PhysicalColumnDataScan, PhysicalDummyScan, PhysicalFilter, PhysicalHashAggregate, PhysicalOrder,
    PhysicalProjection, PhysicalStreamingLimit, PhysicalTableScan, PhysicalUngroupedAggregate,
};

/// Converts a logical plan whose column references are resolved into a physical plan.
pub fn create_physical_plan(op: LogicalOperator) -> Result<PhysicalRef> {
    let operator_type = op.operator_type();
    let LogicalOperator {
        kind,
        children,
        expressions,
        types,
        ..
    } = op;
    // VALUES over a dummy scan is evaluated here, like DuckDB's foldable fast path.
    if let LogicalKind::ExpressionGet {
        expr_types,
        expressions: rows,
        ..
    } = &kind
        && children
            .first()
            .is_some_and(|child| matches!(child.kind, LogicalKind::DummyScan { .. }))
    {
        let chunks = evaluate_rows(expr_types, rows)?;
        return Ok(Arc::new(PhysicalColumnDataScan::new(types, chunks.into())));
    }
    let mut children = children
        .into_iter()
        .map(create_physical_plan)
        .collect::<Result<Vec<_>>>()?
        .into_iter();
    let mut next_child = |what: &str| {
        children
            .next()
            .ok_or_else(|| Error::internal(format!("{what} without input")))
    };
    let plan: PhysicalRef = match kind {
        LogicalKind::DummyScan { .. } => Arc::new(PhysicalDummyScan::new(types)),
        LogicalKind::EmptyResult { .. } => Arc::new(PhysicalColumnDataScan::new(types, Arc::from([]))),
        LogicalKind::ChunkGet { collection, .. } => Arc::new(PhysicalColumnDataScan::new(types, collection)),
        LogicalKind::Get(get) if get.function.scan.is_some() && get.projection_ids.is_empty() => {
            let column_ids = get.column_ids.iter().map(|column| column.index).collect();
            Arc::new(PhysicalTableScan::new(
                types,
                get.function,
                get.bind_data,
                column_ids,
            ))
        }
        LogicalKind::Projection { .. } => Arc::new(PhysicalProjection::new(
            types,
            expressions,
            next_child("projection")?,
        )),
        LogicalKind::Filter { projection_map } => {
            if !projection_map.is_empty() {
                return Err(Error::not_implemented("filter with a projection map"));
            }
            let child = next_child("filter")?;
            let mut expressions = expressions;
            let predicate = if expressions.len() == 1 {
                expressions.remove(0)
            } else {
                Expression::new(
                    ExpressionType::ConjunctionAnd,
                    LogicalType::BOOLEAN,
                    ExpressionKind::Conjunction(BoundConjunctionExpression {
                        children: expressions,
                    }),
                )
            };
            Arc::new(PhysicalFilter::new(types, predicate, child))
        }
        LogicalKind::Aggregate(aggregate) => {
            let child = next_child("aggregate")?;
            if !aggregate.groups.is_empty() || aggregate.grouping_sets.len() > 1 {
                let aggregates = expressions
                    .into_iter()
                    .map(|expression| match expression.kind {
                        ExpressionKind::Aggregate(aggregate) => Ok(*aggregate),
                        _ => Err(Error::internal(format!(
                            "non-aggregate {expression} in an aggregate operator"
                        ))),
                    })
                    .collect::<Result<Vec<_>>>()?;
                let LogicalAggregate {
                    groups,
                    grouping_sets,
                    grouping_functions,
                    ..
                } = *aggregate;
                return Ok(Arc::new(PhysicalHashAggregate::new(
                    types,
                    groups,
                    aggregates,
                    grouping_sets,
                    grouping_functions,
                    child,
                )?));
            }
            let aggregates = expressions
                .into_iter()
                .map(|expression| match expression.kind {
                    ExpressionKind::Aggregate(aggregate) => Ok(*aggregate),
                    _ => Err(Error::internal(format!(
                        "non-aggregate {expression} in an aggregate operator"
                    ))),
                })
                .collect::<Result<Vec<_>>>()?;
            Arc::new(PhysicalUngroupedAggregate::new(types, aggregates, child)?)
        }
        LogicalKind::Limit { limit, offset } => {
            let child = next_child("limit")?;
            let constant = |bound: &BoundLimitNode, unset: u64| match bound {
                BoundLimitNode::Unset => Ok(unset),
                BoundLimitNode::ConstantValue(value) => Ok(*value),
                _ => Err(Error::not_implemented("non-constant or percentage LIMIT/OFFSET")),
            };
            Arc::new(PhysicalStreamingLimit::new(
                types,
                constant(&limit, u64::MAX)?,
                constant(&offset, 0)?,
                child,
            ))
        }
        LogicalKind::Order {
            orders,
            projection_map,
        } => {
            let projections = projection_map.iter().map(|&index| index as usize).collect();
            Arc::new(PhysicalOrder::new(
                types,
                orders,
                projections,
                next_child("order")?,
            ))
        }
        _ => return Err(not_implemented(operator_type)),
    };
    Ok(plan)
}

fn not_implemented(operator_type: LogicalOperatorType) -> Error {
    Error::not_implemented(format!(
        "physical plan for logical operator {}",
        operator_type.name()
    ))
}

/// Evaluates the rows of a `VALUES` list into chunks (DuckDB materialises foldable rows
/// into a `ColumnDataCollection`).
fn evaluate_rows(types: &[LogicalType], rows: &[Vec<Expression>]) -> Result<Vec<DataChunk>> {
    let mut chunks = Vec::new();
    for batch in rows.chunks(STANDARD_VECTOR_SIZE) {
        let mut chunk = DataChunk::with_types(types, batch.len());
        for (row_index, row) in batch.iter().enumerate() {
            for (column, expression) in row.iter().enumerate() {
                let value = ExpressionExecutor::evaluate_scalar(expression)?;
                chunk.column_mut(column).set_value(row_index, &value)?;
            }
        }
        chunk.set_len(batch.len());
        chunks.push(chunk);
    }
    Ok(chunks)
}
