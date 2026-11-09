//! Removing redundant joins with duplicate-eliminated scans (DuckDB's `Deliminator`).
//!
//! Flattened correlated subqueries join their body with a `DELIM_GET` of the correlated
//! values. When that join only filters the body by the correlated columns, it is
//! redundant: the delim join above checks the same condition. The join is replaced by
//! its other side (with `IS NOT NULL` filters where the join removed NULLs), references
//! to the delim columns are pointed at the body's columns, and a delim join left without
//! delim scans becomes a plain comparison join.
//!
//! Operators are addressed by their child-index path from the root, as the plan is
//! owned.

use roastedduck_ast::{ExpressionType, JoinType};
use roastedduck_planner::{
    ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};
use roastedduck_storage::TableFilter;
use roastedduck_types::LogicalType;

use crate::column_binding_replacer::{ColumnBindingReplacer, ReplacementBinding};
use crate::expression_util::flip_comparison;
use crate::join_filter_pushdown::operator_at_path;

/// A join with a delim scan below a delim join (DuckDB's `JoinWithDelimGet`).
struct JoinWithDelimGet {
    path: Vec<usize>,
    depth: usize,
}

/// A delim join and the joins with its delim scans (DuckDB's `DelimCandidate`).
struct DelimCandidate {
    path: Vec<usize>,
    joins: Vec<JoinWithDelimGet>,
    delim_get_count: usize,
}

const fn is_equality(comparison: ExpressionType) -> bool {
    matches!(
        comparison,
        ExpressionType::CompareEqual | ExpressionType::CompareNotDistinctFrom
    )
}

/// Removes the redundant delim joins of `plan` (DuckDB's `Deliminator::Optimize`).
pub fn deliminate(plan: &mut LogicalOperator) {
    let mut candidates = Vec::new();
    find_candidates(plan, &mut Vec::new(), &mut candidates);
    for mut candidate in candidates {
        // The deepest joins first.
        candidate.joins.sort_by_key(|join| std::cmp::Reverse(join.depth));
        let Some(delim_join) = operator_at_path(plan, &candidate.path) else {
            continue;
        };
        let mut all_removed = true;
        if !candidate.joins.is_empty() && has_selection(delim_join) {
            // A selective input makes the deepest join worth keeping: it reduces the
            // work on the right side.
            candidate.joins.remove(0);
            all_removed = false;
        }
        let mut all_equality_conditions = true;
        let join_count = candidate.joins.len();
        for join in &candidate.joins {
            let removed = remove_join_with_delim_get(
                plan,
                &candidate.path,
                candidate.delim_get_count,
                &join.path,
                &mut all_equality_conditions,
            );
            all_removed = removed && all_removed;
        }
        let Some(delim_join) = operator_at_path(plan, &candidate.path) else {
            continue;
        };
        let LogicalKind::ComparisonJoin(join) = &mut delim_join.kind else {
            continue;
        };
        if join_count == candidate.delim_get_count && all_removed {
            // No duplicate-eliminated scans remain.
            join.operator_type = LogicalOperatorType::LogicalComparisonJoin;
            join.duplicate_eliminated_columns.clear();
        }
        if join.join_type == JoinType::Single {
            try_switch_single_to_left(delim_join);
        }
    }
}

/// The delim joins below `op`, children first (DuckDB's `FindCandidates`).
fn find_candidates(op: &LogicalOperator, path: &mut Vec<usize>, candidates: &mut Vec<DelimCandidate>) {
    for (index, child) in op.children.iter().enumerate() {
        path.push(index);
        find_candidates(child, path, candidates);
        path.pop();
    }
    if op.operator_type() != LogicalOperatorType::LogicalDelimJoin {
        return;
    }
    let mut candidate = DelimCandidate {
        path: path.clone(),
        joins: Vec::new(),
        delim_get_count: 0,
    };
    if let Some(right) = op.children.get(1) {
        path.push(1);
        find_join_with_delim_get(right, path, &mut candidate, 0);
        path.pop();
    }
    candidates.push(candidate);
}

/// Whether the subtree filters rows (DuckDB's `HasSelection`).
fn has_selection(op: &LogicalOperator) -> bool {
    match &op.kind {
        LogicalKind::Get(get)
            if get
                .table_filters
                .filters
                .values()
                .any(|filter| !matches!(filter, TableFilter::IsNotNull)) =>
        {
            return true;
        }
        LogicalKind::Filter { .. } => return true,
        _ => {}
    }
    op.children.iter().any(has_selection)
}

/// A delim scan, possibly under a filter (DuckDB's `OperatorIsDelimGet`).
fn is_delim_get(op: &LogicalOperator) -> bool {
    match op.operator_type() {
        LogicalOperatorType::LogicalDelimGet => true,
        LogicalOperatorType::LogicalFilter => op
            .children
            .first()
            .is_some_and(|child| child.operator_type() == LogicalOperatorType::LogicalDelimGet),
        _ => false,
    }
}

/// Collects the comparison joins with a delim scan side and counts the delim scans
/// (DuckDB's `FindJoinWithDelimGet`); nested delim joins only on their left side.
fn find_join_with_delim_get(
    op: &LogicalOperator,
    path: &mut Vec<usize>,
    candidate: &mut DelimCandidate,
    depth: usize,
) {
    match op.operator_type() {
        LogicalOperatorType::LogicalDelimJoin => {
            if let Some(left) = op.children.first() {
                path.push(0);
                find_join_with_delim_get(left, path, candidate, depth + 1);
                path.pop();
            }
        }
        LogicalOperatorType::LogicalDelimGet => candidate.delim_get_count += 1,
        _ => {
            for (index, child) in op.children.iter().enumerate() {
                path.push(index);
                find_join_with_delim_get(child, path, candidate, depth + 1);
                path.pop();
            }
        }
    }
    if op.operator_type() == LogicalOperatorType::LogicalComparisonJoin
        && op.children.len() == 2
        && (is_delim_get(&op.children[0]) || is_delim_get(&op.children[1]))
    {
        candidate.joins.push(JoinWithDelimGet {
            path: path.clone(),
            depth,
        });
    }
}

/// DuckDB's `RemoveJoinWithDelimGet`: replaces the join at `join_path` by its non-delim
/// side if the join is redundant.
fn remove_join_with_delim_get(
    plan: &mut LogicalOperator,
    delim_path: &[usize],
    delim_get_count: usize,
    join_path: &[usize],
    all_equality_conditions: &mut bool,
) -> bool {
    let Some(join_op) = operator_at_path(plan, join_path) else {
        return false;
    };
    let LogicalKind::ComparisonJoin(comparison_join) = &join_op.kind else {
        return false;
    };
    if !matches!(comparison_join.join_type, JoinType::Inner | JoinType::Semi) {
        return false;
    }
    let delim_index = usize::from(!is_delim_get(&join_op.children[0]));
    let delim_side_op = &join_op.children[delim_index];
    let mut filter_expressions: Vec<Expression> = Vec::new();
    let delim_get = if delim_side_op.operator_type() == LogicalOperatorType::LogicalFilter {
        filter_expressions.extend(delim_side_op.expressions.iter().cloned());
        &delim_side_op.children[0]
    } else {
        delim_side_op
    };
    let LogicalKind::DelimGet { chunk_types, .. } = &delim_get.kind else {
        return false;
    };
    if comparison_join.conditions.len() != chunk_types.len() {
        // Joining with the delim scan adds information.
        return false;
    }
    let mut replacements = Vec::new();
    for condition in &comparison_join.conditions {
        *all_equality_conditions = *all_equality_conditions && is_equality(condition.comparison);
        let (delim_side, other_side) = if delim_index == 0 {
            (&condition.left, &condition.right)
        } else {
            (&condition.right, &condition.left)
        };
        let (ExpressionKind::ColumnRef(delim_column), ExpressionKind::ColumnRef(other_column)) =
            (&delim_side.kind, &other_side.kind)
        else {
            return false;
        };
        replacements.push(ReplacementBinding::new(
            delim_column.binding,
            other_column.binding,
        ));
        // Regular comparisons remove NULLs; DISTINCT FROM variants keep them.
        if !matches!(
            condition.comparison,
            ExpressionType::CompareNotDistinctFrom | ExpressionType::CompareDistinctFrom
        ) {
            filter_expressions.push(Expression::operator(
                ExpressionType::OperatorIsNotNull,
                LogicalType::BOOLEAN,
                vec![other_side.clone()],
            ));
        }
    }
    let join_conditions = comparison_join.conditions.clone();
    if !*all_equality_conditions
        && !remove_inequality_join_with_delim_get(
            plan,
            delim_path,
            delim_get_count,
            join_path,
            &join_conditions,
            delim_index,
        )
    {
        return false;
    }
    let Some(join_op) = operator_at_path(plan, join_path) else {
        return false;
    };
    let mut replacement = join_op.children.swap_remove(1 - delim_index);
    if !filter_expressions.is_empty() {
        replacement = LogicalOperator::new(LogicalKind::Filter {
            projection_map: Vec::new(),
        })
        .with_expressions(filter_expressions)
        .with_children(vec![replacement]);
    }
    *join_op = replacement;
    ColumnBindingReplacer {
        replacement_bindings: replacements,
    }
    .visit_operator(plan);
    true
}

/// Follows `bindings` through projection expressions that are column references
/// (DuckDB's `FindAndReplaceBindings`).
fn find_and_replace_bindings(bindings: &mut [ColumnBinding], op: &LogicalOperator) -> bool {
    let current = op.column_bindings();
    for binding in bindings.iter_mut() {
        let Some(index) = current.iter().position(|candidate| candidate == binding) else {
            return false;
        };
        let Some(ExpressionKind::ColumnRef(column)) =
            op.expressions.get(index).map(|expression| &expression.kind)
        else {
            return false;
        };
        *binding = column.binding;
    }
    true
}

/// DuckDB's `RemoveInequalityJoinWithDelimGet`: an inequality join with the delim scan
/// can move its comparison into the delim join.
fn remove_inequality_join_with_delim_get(
    plan: &mut LogicalOperator,
    delim_path: &[usize],
    delim_get_count: usize,
    join_path: &[usize],
    join_conditions: &[roastedduck_planner::operator::JoinCondition],
    delim_index: usize,
) -> bool {
    let Some(delim_op) = operator_at_path(plan, delim_path) else {
        return false;
    };
    let LogicalKind::ComparisonJoin(delim_join) = &delim_op.kind else {
        return false;
    };
    if delim_get_count != 1
        || !matches!(
            delim_join.join_type,
            JoinType::Anti | JoinType::Mark | JoinType::Semi | JoinType::Single
        )
        || delim_join.conditions.len() != join_conditions.len()
    {
        return false;
    }
    // Pure inequality SINGLE/MARK joins are not supported.
    if matches!(delim_join.join_type, JoinType::Single | JoinType::Mark)
        && !join_conditions
            .iter()
            .any(|condition| is_equality(condition.comparison))
    {
        return false;
    }
    let mut traced_bindings = Vec::with_capacity(delim_join.conditions.len());
    for condition in &delim_join.conditions {
        let ExpressionKind::ColumnRef(column) = &condition.right.kind else {
            return false;
        };
        traced_bindings.push(column.binding);
    }
    // Trace the bindings down to the join through projections and filters.
    if join_path.len() <= delim_path.len() || join_path[..delim_path.len()] != *delim_path {
        return false;
    }
    let relative = &join_path[delim_path.len()..];
    if relative.first() != Some(&1) {
        return false;
    }
    let mut current = &delim_op.children[1];
    for &step in &relative[1..] {
        if current.children.len() != 1 {
            return false;
        }
        match current.operator_type() {
            LogicalOperatorType::LogicalProjection => {
                find_and_replace_bindings(&mut traced_bindings, current);
            }
            LogicalOperatorType::LogicalFilter => {}
            _ => return false,
        }
        let Some(next) = current.children.get(step) else {
            return false;
        };
        current = next;
    }
    let delim_join_type = delim_join.join_type;
    let mut new_comparisons = Vec::with_capacity(traced_bindings.len());
    for (condition_index, traced) in traced_bindings.iter().enumerate() {
        let delim_comparison = delim_join.conditions[condition_index].comparison;
        let mut found = None;
        for join_condition in join_conditions {
            let delim_side = if delim_index == 0 {
                &join_condition.left
            } else {
                &join_condition.right
            };
            let ExpressionKind::ColumnRef(column) = &delim_side.kind else {
                continue;
            };
            if column.binding != *traced {
                continue;
            }
            let original = join_condition.comparison;
            let mut join_comparison = original;
            if matches!(
                delim_comparison,
                ExpressionType::CompareDistinctFrom | ExpressionType::CompareNotDistinctFrom
            ) {
                // NULLs must compare.
                join_comparison = match join_comparison {
                    ExpressionType::CompareEqual => ExpressionType::CompareNotDistinctFrom,
                    ExpressionType::CompareNotequal => ExpressionType::CompareDistinctFrom,
                    ExpressionType::CompareDistinctFrom | ExpressionType::CompareNotDistinctFrom => {
                        join_comparison
                    }
                    _ => break,
                };
            }
            let mut comparison = flip_comparison(join_comparison);
            // The join's != / = removed NULLs, which the delim join must now do.
            if delim_join_type != JoinType::Mark
                && !matches!(
                    original,
                    ExpressionType::CompareDistinctFrom | ExpressionType::CompareNotDistinctFrom
                )
            {
                comparison = match comparison {
                    ExpressionType::CompareDistinctFrom => ExpressionType::CompareNotequal,
                    ExpressionType::CompareNotDistinctFrom => ExpressionType::CompareEqual,
                    other => other,
                };
            }
            found = Some(comparison);
            break;
        }
        new_comparisons.push(found);
    }
    // Only a complete match moves the comparisons into the delim join.
    let all_found = new_comparisons.iter().all(Option::is_some);
    if !all_found {
        return false;
    }
    if let Some(delim_op) = operator_at_path(plan, delim_path)
        && let LogicalKind::ComparisonJoin(delim_join) = &mut delim_op.kind
    {
        for (condition, comparison) in delim_join.conditions.iter_mut().zip(new_comparisons) {
            if let Some(comparison) = comparison {
                condition.comparison = comparison;
            }
        }
    }
    all_found
}

/// A SINGLE delim join whose right side is grouped by the join columns matches at most
/// one row: a LEFT join (DuckDB's `TrySwitchSingleToLeft`).
fn try_switch_single_to_left(delim_op: &mut LogicalOperator) {
    let LogicalKind::ComparisonJoin(delim_join) = &delim_op.kind else {
        return;
    };
    let mut join_bindings = Vec::with_capacity(delim_join.conditions.len());
    for condition in &delim_join.conditions {
        if !is_equality(condition.comparison) {
            return;
        }
        let ExpressionKind::ColumnRef(column) = &condition.right.kind else {
            return;
        };
        join_bindings.push(column.binding);
    }
    let Some(mut current) = delim_op.children.get(1) else {
        return;
    };
    while current.operator_type() != LogicalOperatorType::LogicalAggregateAndGroupBy {
        if current.children.len() != 1 {
            return;
        }
        match current.operator_type() {
            LogicalOperatorType::LogicalProjection => {
                find_and_replace_bindings(&mut join_bindings, current);
            }
            LogicalOperatorType::LogicalFilter => {}
            _ => return,
        }
        current = &current.children[0];
    }
    let LogicalKind::Aggregate(aggregate) = &current.kind else {
        return;
    };
    if !aggregate.grouping_functions.is_empty() {
        return;
    }
    let all_groups_joined = (0..aggregate.groups.len())
        .all(|index| join_bindings.contains(&ColumnBinding::new(aggregate.group_index, index as u64)));
    if !all_groups_joined {
        return;
    }
    if let LogicalKind::ComparisonJoin(delim_join) = &mut delim_op.kind {
        delim_join.join_type = JoinType::Left;
    }
}
