//! Computing identical aggregates of one aggregate operator once (DuckDB's
//! `CommonAggregateOptimizer`), e.g. `SELECT sum(x), sum(x) + 1`.

use std::collections::HashMap;

use roastedduck_planner::{
    ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};

/// Removes duplicate aggregates throughout `plan`.
pub fn extract_common_aggregates(plan: &mut LogicalOperator) {
    let mut aggregate_map = HashMap::new();
    visit_operator(plan, &mut aggregate_map);
}

/// DuckDB's `VisitOperator`: set operations and projections start a new scope. Returns,
/// for an aggregate that dropped duplicates, the new position of each old output column.
fn visit_operator(
    op: &mut LogicalOperator,
    aggregate_map: &mut HashMap<ColumnBinding, ColumnBinding>,
) -> Option<Vec<usize>> {
    if matches!(
        op.operator_type(),
        LogicalOperatorType::LogicalUnion
            | LogicalOperatorType::LogicalExcept
            | LogicalOperatorType::LogicalIntersect
            | LogicalOperatorType::LogicalProjection
    ) {
        standard_visit_operator(op, &mut HashMap::new());
        return None;
    }
    standard_visit_operator(op, aggregate_map);
    if op.operator_type() == LogicalOperatorType::LogicalAggregateAndGroupBy {
        return extract(op, aggregate_map);
    }
    None
}

/// Visits the children, then remaps this operator's references to removed aggregates.
fn standard_visit_operator(
    op: &mut LogicalOperator,
    aggregate_map: &mut HashMap<ColumnBinding, ColumnBinding>,
) {
    for index in 0..op.children.len() {
        if let Some(positions) = visit_operator(&mut op.children[index], aggregate_map) {
            remap_projection_map(op, index, &positions);
        }
    }
    if !aggregate_map.is_empty() {
        op.for_each_expression_mut(|expression| remap(expression, aggregate_map));
    }
}

/// Points a projection map over child `child` at the child's new columns, after its
/// duplicate aggregates were dropped: the references to them now read the kept ones.
fn remap_projection_map(op: &mut LogicalOperator, child: usize, positions: &[usize]) {
    let map = match &mut op.kind {
        LogicalKind::ComparisonJoin(join) if child == 0 => &mut join.left_projection_map,
        LogicalKind::ComparisonJoin(join) => &mut join.right_projection_map,
        LogicalKind::Filter { projection_map } | LogicalKind::Order { projection_map, .. } => projection_map,
        _ => return,
    };
    if map.is_empty() {
        return;
    }
    let mut remapped: Vec<u64> = Vec::with_capacity(map.len());
    for &old in map.iter() {
        let Some(&new) = positions.get(old as usize) else {
            continue;
        };
        if !remapped.contains(&(new as u64)) {
            remapped.push(new as u64);
        }
    }
    *map = remapped;
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

/// DuckDB's `ExtractCommonAggregates`. Returns the new position of each old output
/// column if a duplicate was dropped.
fn extract(
    op: &mut LogicalOperator,
    aggregate_map: &mut HashMap<ColumnBinding, ColumnBinding>,
) -> Option<Vec<usize>> {
    let LogicalKind::Aggregate(aggregate) = &op.kind else {
        return None;
    };
    let aggregate_index = aggregate.aggregate_index;
    let group_count = aggregate.groups.len();
    let groupings = aggregate.grouping_functions.len();
    let original_count = op.expressions.len();
    let mut kept: Vec<Expression> = Vec::with_capacity(original_count);
    let mut new_positions = Vec::with_capacity(original_count);
    for (original_index, expression) in std::mem::take(&mut op.expressions).into_iter().enumerate() {
        let original = ColumnBinding::new(aggregate_index, original_index as u64);
        if let Some(existing) = kept.iter().position(|kept| kept.equals(&expression)) {
            aggregate_map.insert(original, ColumnBinding::new(aggregate_index, existing as u64));
            new_positions.push(existing);
            continue;
        }
        if kept.len() != original_index {
            aggregate_map.insert(original, ColumnBinding::new(aggregate_index, kept.len() as u64));
        }
        new_positions.push(kept.len());
        kept.push(expression);
    }
    let removed = original_count - kept.len();
    op.expressions = kept;
    if removed == 0 {
        return None;
    }
    // The output: groups, aggregates, then the GROUPING() columns.
    let mut positions: Vec<usize> = (0..group_count).collect();
    positions.extend(new_positions.iter().map(|position| group_count + position));
    positions.extend((0..groupings).map(|index| group_count + original_count - removed + index));
    Some(positions)
}
