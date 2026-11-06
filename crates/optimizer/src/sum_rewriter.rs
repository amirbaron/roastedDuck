//! `SUM(x + C)` as `SUM(x) + C * COUNT(x)` in ungrouped aggregates (DuckDB's
//! `SumRewriterOptimizer`), so that several sums of shifted columns share one input.

use std::collections::HashMap;

use rustdb_common::Result;
use rustdb_planner::function_binder::add_cast_to_type;
use rustdb_planner::{
    ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};
use rustdb_types::LogicalTypeId;

use crate::optimizer::{Optimizer, bind_aggregate_function};

/// Rewrites the sums in `plan` (DuckDB's `SumRewriterOptimizer::Optimize` from the root).
pub fn rewrite_sums(optimizer: &Optimizer, plan: &mut LogicalOperator) -> Result<()> {
    let mut scope = HashMap::new();
    optimize(optimizer, plan, &mut scope)
}

/// DuckDB's `Optimize`: rewrites an aggregate, then visits the result.
fn optimize(
    optimizer: &Optimizer,
    op: &mut LogicalOperator,
    aggregate_map: &mut HashMap<ColumnBinding, ColumnBinding>,
) -> Result<()> {
    if op.operator_type() == LogicalOperatorType::LogicalAggregateAndGroupBy {
        rewrite_aggregate(optimizer, op, aggregate_map)?;
    }
    visit_operator(optimizer, op, aggregate_map)
}

/// DuckDB's `VisitOperator`: set operations, CTEs and projections start a new scope of
/// remapped aggregates.
fn visit_operator(
    optimizer: &Optimizer,
    op: &mut LogicalOperator,
    aggregate_map: &mut HashMap<ColumnBinding, ColumnBinding>,
) -> Result<()> {
    match op.operator_type() {
        LogicalOperatorType::LogicalUnion
        | LogicalOperatorType::LogicalExcept
        | LogicalOperatorType::LogicalIntersect
        | LogicalOperatorType::LogicalMaterializedCte
        | LogicalOperatorType::LogicalProjection => {
            standard_visit_operator(optimizer, op, &mut HashMap::new())
        }
        _ => standard_visit_operator(optimizer, op, aggregate_map),
    }
}

/// Visits the children, then points this operator's references to rewritten aggregates
/// at the projection that recomputes them.
fn standard_visit_operator(
    optimizer: &Optimizer,
    op: &mut LogicalOperator,
    aggregate_map: &mut HashMap<ColumnBinding, ColumnBinding>,
) -> Result<()> {
    for child in &mut op.children {
        optimize(optimizer, child, aggregate_map)?;
    }
    if !aggregate_map.is_empty() {
        op.for_each_expression_mut(|expression| remap(expression, aggregate_map));
    }
    Ok(())
}

fn remap(expression: &mut Expression, aggregate_map: &HashMap<ColumnBinding, ColumnBinding>) {
    if let ExpressionKind::ColumnRef(column) = &mut expression.kind {
        if let Some(&binding) = aggregate_map.get(&column.binding) {
            column.binding = binding;
        }
        return;
    }
    expression.for_each_child_mut(|child| remap(child, aggregate_map));
}

/// DuckDB's `GetSmallIntegerTypesMatcher`.
fn is_small_integer(expression: &Expression) -> bool {
    matches!(
        expression.return_type.id(),
        LogicalTypeId::TinyInt
            | LogicalTypeId::SmallInt
            | LogicalTypeId::Integer
            | LogicalTypeId::BigInt
            | LogicalTypeId::UTinyInt
            | LogicalTypeId::USmallInt
            | LogicalTypeId::UInteger
            | LogicalTypeId::UBigInt
    )
}

/// DuckDB's `IntegerTypeMatcher` (`LogicalType::IsIntegral`).
fn is_integral(expression: &Expression) -> bool {
    is_small_integer(expression)
        || matches!(
            expression.return_type.id(),
            LogicalTypeId::HugeInt | LogicalTypeId::UHugeInt
        )
}

/// The position of the constant in `SUM(x + C)` / `SUM(C + x)`, if `expression` is one
/// (DuckDB's `sum_matcher`).
fn match_sum_of_addition(expression: &Expression) -> Option<usize> {
    let ExpressionKind::Aggregate(aggregate) = &expression.kind else {
        return None;
    };
    if &*aggregate.function.name != "sum"
        || aggregate.filter.is_some()
        || aggregate.order_bys.is_some()
        || aggregate.is_distinct()
        || aggregate.children.len() != 1
    {
        return None;
    }
    let addition = &aggregate.children[0];
    let ExpressionKind::Function(function) = &addition.kind else {
        return None;
    };
    if &*function.function.name != b"+" || !is_integral(addition) || function.children.len() != 2 {
        return None;
    }
    let is_constant =
        |child: &Expression| matches!(child.kind, ExpressionKind::Constant(_)) && is_small_integer(child);
    let is_stable = |child: &Expression| !child.is_volatile() && is_small_integer(child);
    let (first, second) = (&function.children[0], &function.children[1]);
    // SOME policy: the constant and the stable expression match different children.
    if !((is_constant(first) && is_stable(second)) || (is_constant(second) && is_stable(first))) {
        return None;
    }
    Some(usize::from(!matches!(first.kind, ExpressionKind::Constant(_))))
}

/// DuckDB's `RewriteSums`.
fn rewrite_aggregate(
    optimizer: &Optimizer,
    op: &mut LogicalOperator,
    aggregate_map: &mut HashMap<ColumnBinding, ColumnBinding>,
) -> Result<()> {
    let LogicalKind::Aggregate(aggregate) = &op.kind else {
        return Ok(());
    };
    if !aggregate.groups.is_empty() {
        return Ok(());
    }
    let aggregate_index = aggregate.aggregate_index;
    let aggregate_count = op.expressions.len();
    let mut rewritten = Vec::new();
    let mut constants = Vec::new();
    for index in 0..aggregate_count {
        let Some(constant_index) = match_sum_of_addition(&op.expressions[index]) else {
            continue;
        };
        let ExpressionKind::Aggregate(sum) = &mut op.expressions[index].kind else {
            continue;
        };
        let addition = std::mem::replace(
            &mut sum.children[0],
            Expression::constant(rustdb_types::Value::integer(0)),
        );
        let ExpressionKind::Function(function) = addition.kind else {
            continue;
        };
        let mut children = function.children;
        let constant = children.remove(constant_index);
        let input = children.remove(0);
        sum.children[0] = input.clone();
        let mut count = bind_aggregate_function(optimizer.context(), "count", vec![input])?;
        // DuckDB binds `CountFunctionBase`, which has no statistics callback.
        if let ExpressionKind::Aggregate(count) = &mut count.kind
            && count.bind_info.is_none()
        {
            count.bind_info = Some(std::sync::Arc::new(
                crate::statistics_propagator::WithoutStatistics,
            ));
        }
        op.expressions.push(count);
        constants.push(constant);
        rewritten.push(index);
    }
    if rewritten.is_empty() {
        return Ok(());
    }
    // A projection on top recomputes the original results.
    let projection_index = optimizer.generate_table_index();
    let mut expressions = Vec::with_capacity(aggregate_count);
    let mut constants = constants.into_iter();
    let mut rewritten_index = 0;
    for index in 0..aggregate_count {
        let aggregate_binding = ColumnBinding::new(aggregate_index, index as u64);
        aggregate_map.insert(
            aggregate_binding,
            ColumnBinding::new(projection_index, index as u64),
        );
        let aggregate_type = op.expressions[index].return_type.clone();
        let reference = Expression::column_ref(String::new(), aggregate_type.clone(), aggregate_binding, 0);
        if !rewritten.contains(&index) {
            expressions.push(reference);
            continue;
        }
        let count_index = aggregate_count + rewritten_index;
        let count_reference = Expression::column_ref(
            String::new(),
            op.expressions[count_index].return_type.clone(),
            ColumnBinding::new(aggregate_index, count_index as u64),
            0,
        );
        let count = add_cast_to_type(optimizer.context(), count_reference, &aggregate_type, false)?;
        let Some(constant) = constants.next() else {
            break;
        };
        let constant = add_cast_to_type(optimizer.context(), constant, &aggregate_type, false)?;
        let multiply = optimizer.bind_scalar_function("*", vec![count, constant])?;
        expressions.push(optimizer.bind_scalar_function("+", vec![reference, multiply])?);
        rewritten_index += 1;
    }
    let input = std::mem::replace(op, LogicalOperator::new(LogicalKind::CrossProduct));
    let estimate = input.estimated_cardinality;
    *op = LogicalOperator::projection(projection_index, expressions, input);
    op.estimated_cardinality = estimate;
    Ok(())
}
