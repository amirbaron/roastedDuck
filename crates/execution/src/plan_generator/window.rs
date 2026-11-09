//! Physical planning of window functions (DuckDB's `plan_window.cpp`).
//!
//! Window functions are grouped by equivalent partitioning and compatible orderings, so
//! each group shares one sort. Functions that can be computed in a single pass over the
//! input (`ROW_NUMBER() OVER ()`, running sums, ...) get a streaming operator. Equal
//! functions are computed once, and a projection restores the original column order.

use std::collections::HashMap;
use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_planner::{Expression, ExpressionKind};
use roastedduck_types::LogicalType;

use crate::operator::PhysicalRef;
use crate::operators::{PhysicalProjection, PhysicalStreamingWindow, PhysicalWindow};
use crate::window::{partitions_are_equivalent, shared_orders};

/// The number of pipeline sources feeding `op` (DuckDB's `PhysicalOperator::GetSources`):
/// a window can only stream when rows arrive in one ordered stream.
pub(crate) fn source_count(op: &PhysicalRef) -> usize {
    let children = op.children();
    if op.is_union() {
        return children.iter().map(source_count).sum();
    }
    if op.is_cte() {
        return children.get(1).map_or(1, source_count);
    }
    match (
        op.as_sink().is_some(),
        op.as_operator().is_some(),
        children.first(),
    ) {
        // Joins: the probe side's sources, plus the join itself if it emits build rows.
        (true, true, Some(probe)) => source_count(probe) + usize::from(op.as_source().is_some()),
        // Sinks and leaf sources start a pipeline.
        (true, false, _) | (_, _, None) => 1,
        (false, _, Some(child)) => source_count(child),
    }
}

/// The window definition of a window expression.
fn window_of(expression: &Expression) -> Result<&roastedduck_planner::expression::BoundWindowExpression> {
    match &expression.kind {
        ExpressionKind::Window(window) => Ok(window),
        _ => Err(Error::internal(format!("{expression} is not a window function"))),
    }
}

/// Plans a `LOGICAL_WINDOW` over `child` computing `expressions`; `types` are the
/// operator's output types (the input columns followed by one column per function).
pub(crate) fn plan_window(
    types: Vec<LogicalType>,
    mut expressions: Vec<Expression>,
    estimated_cardinality: usize,
    child: PhysicalRef,
) -> Result<PhysicalRef> {
    let input_width = types
        .len()
        .checked_sub(expressions.len())
        .ok_or_else(|| Error::internal("window operator with more functions than columns"))?;

    // Identify streaming windows.
    let can_stream = source_count(&child) == 1;
    let mut blocking = Vec::new();
    let mut streaming = Vec::new();
    for (index, expression) in expressions.iter().enumerate() {
        if can_stream && PhysicalStreamingWindow::is_streaming_function(expression)? {
            streaming.push(index);
        } else {
            blocking.push(index);
        }
    }

    // Group the functions by shared partitioning and ordering.
    let mut projection_map: HashMap<usize, usize> = HashMap::new();
    let mut window_expressions: Vec<Vec<usize>> = Vec::new();
    let mut streaming_count = 0;
    let mut output_pos = input_width;
    while !blocking.is_empty() || !streaming.is_empty() {
        let process_blocking = streaming.is_empty();
        let remaining = if process_blocking {
            &mut blocking
        } else {
            &mut streaming
        };
        if !process_blocking {
            streaming_count += 1;
        }
        // Functions sharing the partitioning of the first remaining one.
        let mut over_idx = remaining[0];
        let mut matching: Vec<usize> = Vec::new();
        let mut unprocessed = Vec::new();
        for &expr_idx in remaining.iter() {
            if expr_idx == over_idx {
                matching.push(expr_idx);
                continue;
            }
            let window = window_of(&expressions[expr_idx])?;
            let over = window_of(&expressions[over_idx])?;
            if !partitions_are_equivalent(over, window) {
                unprocessed.push(expr_idx);
                continue;
            }
            // Common subexpression elimination.
            if let Some(position) = matching
                .iter()
                .position(|&match_idx| expressions[expr_idx].equals(&expressions[match_idx]))
            {
                projection_map.insert(input_width + expr_idx, output_pos + position);
                continue;
            }
            // Is there a common sort prefix?
            let prefix = shared_orders(&over.orders, &window.orders);
            if prefix != over.orders.len().min(window.orders.len()) {
                unprocessed.push(expr_idx);
                continue;
            }
            matching.push(expr_idx);
            // Switch to the longer prefix.
            if prefix < window.orders.len() {
                over_idx = expr_idx;
            }
        }
        *remaining = unprocessed;
        // Remember the projection order.
        for &expr_idx in &matching {
            projection_map.insert(input_width + expr_idx, output_pos);
            output_pos += 1;
        }
        window_expressions.push(matching);
    }

    // Build the window operators.
    let mut plan = child;
    let mut operator_types: Vec<LogicalType> = types[..input_width].to_vec();
    let placeholder = || Expression::constant(roastedduck_types::Value::default());
    for (index, matching) in window_expressions.iter().enumerate() {
        let mut select_list = Vec::with_capacity(matching.len());
        for &expr_idx in matching {
            select_list.push(std::mem::replace(&mut expressions[expr_idx], placeholder()));
            operator_types.push(types[input_width + expr_idx].clone());
        }
        plan = if index < streaming_count {
            Arc::new(PhysicalStreamingWindow::new(
                operator_types.clone(),
                select_list,
                plan,
            )?)
        } else {
            Arc::new(PhysicalWindow::new(
                operator_types.clone(),
                select_list,
                estimated_cardinality,
                plan,
            )?)
        };
    }

    // Put everything back into place (DuckDB always projects, even if nothing moved).
    if !projection_map.is_empty() {
        let mut select_list: Vec<Expression> = (0..input_width)
            .map(|index| Expression::reference("", types[index].clone(), index as u64))
            .collect();
        select_list.extend((input_width..types.len()).map(|column| {
            let source = projection_map.get(&column).copied().unwrap_or(column);
            Expression::reference("", types[column].clone(), source as u64)
        }));
        plan = Arc::new(PhysicalProjection::new(types, select_list, plan));
    }
    Ok(plan)
}

/// `child` with a `ROW_NUMBER() OVER (PARTITION BY <all columns>)` column appended, which
/// numbers duplicate rows (DuckDB's `CreatePartitionedRowNumExpression`, used by
/// `EXCEPT ALL` / `INTERSECT ALL`).
pub(crate) fn partitioned_row_number(
    client: Option<&roastedduck_planner::ClientContext>,
    child: PhysicalRef,
) -> Result<PhysicalRef> {
    let input_types = child.types().to_vec();
    let partitions = input_types
        .iter()
        .enumerate()
        .map(|(index, ty)| super::collated_reference(client, ty, index))
        .collect::<Result<_>>()?;
    let row_number = Expression::new(
        roastedduck_planner::ExpressionType::WindowRowNumber,
        LogicalType::BIGINT,
        ExpressionKind::Window(Box::new(roastedduck_planner::expression::BoundWindowExpression {
            aggregate: None,
            bind_info: None,
            children: Vec::new(),
            partitions,
            orders: Vec::new(),
            filter_expr: None,
            ignore_nulls: false,
            distinct: false,
            start: roastedduck_ast::WindowBoundary::UnboundedPreceding,
            end: roastedduck_ast::WindowBoundary::UnboundedFollowing,
            exclude_clause: roastedduck_ast::WindowExcludeMode::NoOther,
            start_expr: None,
            end_expr: None,
            offset_expr: None,
            default_expr: None,
            arg_orders: Vec::new(),
        })),
    );
    let mut types = input_types;
    types.push(LogicalType::BIGINT);
    Ok(Arc::new(PhysicalWindow::new(types, vec![row_number], 0, child)?))
}
