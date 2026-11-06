//! Removing groups that repeat a column of the same aggregate (DuckDB's
//! `RemoveDuplicateGroups`), e.g. after the unused column pass made a join key and its
//! counterpart the same column.

use std::collections::HashMap;

use rustdb_planner::{ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator};

/// Removes duplicate groups in every aggregate of `plan`.
pub fn remove_duplicate_groups(plan: &mut LogicalOperator) {
    let mut replacements = HashMap::new();
    visit(plan, &mut replacements);
    if !replacements.is_empty() {
        replace(plan, &replacements);
    }
}

fn visit(op: &mut LogicalOperator, replacements: &mut HashMap<ColumnBinding, ColumnBinding>) {
    if let LogicalKind::Aggregate(aggregate) = &mut op.kind {
        visit_aggregate(aggregate, replacements);
    }
    for child in &mut op.children {
        visit(child, replacements);
    }
}

/// DuckDB's `VisitAggregate`: later copies of a grouped column are dropped and their
/// references point at the first copy; the other groups shift down.
fn visit_aggregate(
    aggregate: &mut rustdb_planner::operator::LogicalAggregate,
    replacements: &mut HashMap<ColumnBinding, ColumnBinding>,
) {
    // With ROLLUP/CUBE the positions of groups matter.
    if !aggregate.grouping_functions.is_empty() || aggregate.grouping_sets.len() > 1 {
        return;
    }
    let mut first_by_binding: HashMap<ColumnBinding, usize> = HashMap::new();
    let mut duplicates: Vec<(usize, usize)> = Vec::new();
    for (index, group) in aggregate.groups.iter().enumerate() {
        let ExpressionKind::ColumnRef(column) = &group.kind else {
            continue;
        };
        match first_by_binding.get(&column.binding) {
            Some(&first) => duplicates.push((first, index)),
            None => {
                first_by_binding.insert(column.binding, index);
            }
        }
    }
    if duplicates.is_empty() {
        return;
    }
    // Remove from the back so earlier positions stay valid.
    duplicates.sort_by_key(|duplicate| std::cmp::Reverse(duplicate.1));
    let group_index = aggregate.group_index;
    let mut positions: Vec<u64> = (0..aggregate.groups.len() as u64).collect();
    for &(remaining, removed) in &duplicates {
        aggregate.groups.remove(removed);
        for grouping_set in &mut aggregate.grouping_sets {
            if grouping_set.remove(&(removed as u64)) {
                grouping_set.insert(remaining as u64);
            }
            let shifted: Vec<u64> = grouping_set
                .iter()
                .copied()
                .filter(|&entry| entry > removed as u64)
                .collect();
            for entry in &shifted {
                grouping_set.remove(entry);
            }
            for entry in shifted {
                grouping_set.insert(entry - 1);
            }
        }
        positions[removed] = remaining as u64;
        for position in &mut positions {
            if *position > removed as u64 {
                *position -= 1;
            }
        }
    }
    for (old, new) in positions.into_iter().enumerate() {
        if old as u64 != new {
            replacements.insert(
                ColumnBinding::new(group_index, old as u64),
                ColumnBinding::new(group_index, new),
            );
        }
    }
}

/// Points the references to moved groups at their new positions (the group bindings are
/// only read above their aggregate).
fn replace(op: &mut LogicalOperator, replacements: &HashMap<ColumnBinding, ColumnBinding>) {
    op.for_each_expression_mut(|expression| replace_expression(expression, replacements));
    for child in &mut op.children {
        replace(child, replacements);
    }
}

fn replace_expression(expression: &mut Expression, replacements: &HashMap<ColumnBinding, ColumnBinding>) {
    if let ExpressionKind::ColumnRef(column) = &mut expression.kind {
        if let Some(&binding) = replacements.get(&column.binding) {
            column.binding = binding;
        }
        return;
    }
    expression.for_each_child_mut(|child| replace_expression(child, replacements));
}
