//! Computing identical aggregates of one aggregate operator once (DuckDB's
//! `CommonAggregateOptimizer`), e.g. `SELECT sum(x), sum(x) + 1`.

use std::collections::HashMap;

use rustdb_planner::{
    ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};

/// Removes duplicate aggregates throughout `plan`.
pub fn extract_common_aggregates(plan: &mut LogicalOperator) {
    let mut aggregate_map = HashMap::new();
    visit_operator(plan, &mut aggregate_map);
}

/// DuckDB's `VisitOperator`: set operations and projections start a new scope.
fn visit_operator(op: &mut LogicalOperator, aggregate_map: &mut HashMap<ColumnBinding, ColumnBinding>) {
    if matches!(
        op.operator_type(),
        LogicalOperatorType::LogicalUnion
            | LogicalOperatorType::LogicalExcept
            | LogicalOperatorType::LogicalIntersect
            | LogicalOperatorType::LogicalProjection
    ) {
        standard_visit_operator(op, &mut HashMap::new());
        return;
    }
    standard_visit_operator(op, aggregate_map);
    if op.operator_type() == LogicalOperatorType::LogicalAggregateAndGroupBy {
        extract(op, aggregate_map);
    }
}

/// Visits the children, then remaps this operator's references to removed aggregates.
fn standard_visit_operator(
    op: &mut LogicalOperator,
    aggregate_map: &mut HashMap<ColumnBinding, ColumnBinding>,
) {
    for child in &mut op.children {
        visit_operator(child, aggregate_map);
    }
    if !aggregate_map.is_empty() {
        op.for_each_expression_mut(|expression| remap(expression, aggregate_map));
    }
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

/// DuckDB's `ExtractCommonAggregates`.
fn extract(op: &mut LogicalOperator, aggregate_map: &mut HashMap<ColumnBinding, ColumnBinding>) {
    let LogicalKind::Aggregate(aggregate) = &op.kind else {
        return;
    };
    let aggregate_index = aggregate.aggregate_index;
    let mut kept: Vec<Expression> = Vec::with_capacity(op.expressions.len());
    for (original_index, expression) in std::mem::take(&mut op.expressions).into_iter().enumerate() {
        let original = ColumnBinding::new(aggregate_index, original_index as u64);
        if let Some(existing) = kept.iter().position(|kept| kept.equals(&expression)) {
            aggregate_map.insert(original, ColumnBinding::new(aggregate_index, existing as u64));
            continue;
        }
        if kept.len() != original_index {
            aggregate_map.insert(original, ColumnBinding::new(aggregate_index, kept.len() as u64));
        }
        kept.push(expression);
    }
    op.expressions = kept;
}
