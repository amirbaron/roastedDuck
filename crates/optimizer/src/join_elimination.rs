//! Removing outer joins whose inner side contributes nothing (DuckDB's
//! `JoinElimination`): a LEFT (or RIGHT, SINGLE) join whose inner columns are not used
//! above it and whose inner side matches each outer row at most once (it is distinct on
//! the join keys) returns exactly its outer side.
//!
//! The plan is processed in "pipes": chains of single-child operators. A pipe that ends
//! in a comparison join (below its first operator) may eliminate that join; the join's
//! children and the children of multi-child operators start new pipes.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use roastedduck_ast::{ExpressionType, JoinType};
use roastedduck_planner::{
    ColumnBinding, DistinctType, Expression, ExpressionKind, LogicalKind, LogicalOperator,
    LogicalOperatorType,
};

use crate::expression_util::visit_column_refs;

/// What a pipe knows (DuckDB's `PipeInfo`).
#[derive(Default)]
struct PipeInfo {
    /// For a table index, a set of its columns on which the rows are distinct.
    distinct_groups: BTreeMap<u64, BTreeSet<ColumnBinding>>,
    /// Whether a scan of the pipe has table filters.
    has_filter: bool,
    /// The table indexes the pipe's operators reference.
    ref_table_ids: HashSet<u64>,
}

impl PipeInfo {
    /// The information a child pipe starts with (DuckDB's `PipelineInfo::CreateChild`).
    fn child(&self) -> Self {
        Self {
            distinct_groups: self.distinct_groups.clone(),
            has_filter: false,
            ref_table_ids: self.ref_table_ids.clone(),
        }
    }
}

/// Where a pipe's walk stopped at a comparison join: its depth below the pipe's root and
/// the information its children inherit.
struct JoinPoint {
    depth: usize,
    inherited: PipeInfo,
}

/// Eliminates the redundant joins of `plan` (DuckDB's `JoinElimination::Optimize`).
pub fn eliminate_joins(plan: &mut LogicalOperator) {
    process_pipe(plan, PipeInfo::default());
}

/// Processes the pipe starting at `root` with the initial information `info`; returns
/// its final information.
fn process_pipe(root: &mut LogicalOperator, mut info: PipeInfo) -> PipeInfo {
    let mut join_point = None;
    walk_chain(root, &mut info, 0, &mut join_point);
    if let Some(join_point) = join_point {
        try_eliminate_join(root, join_point, &mut info);
    }
    info
}

/// The operator `depth` steps down the first-child chain.
fn chain_operator(op: &mut LogicalOperator, depth: usize) -> Option<&mut LogicalOperator> {
    let mut node = op;
    for _ in 0..depth {
        node = node.children.first_mut()?;
    }
    Some(node)
}

/// DuckDB's `OptimizeChildren` along the chain: collects references, distinct groups and
/// filters top-down and the projections' distinct groups bottom-up; stops at a join
/// (recording its depth) or at an operator with several children, whose children are
/// separate pipes.
fn walk_chain(
    op: &mut LogicalOperator,
    info: &mut PipeInfo,
    depth: usize,
    join_point: &mut Option<JoinPoint>,
) {
    if op.operator_type() == LogicalOperatorType::LogicalComparisonJoin {
        if depth > 0 {
            *join_point = Some(JoinPoint {
                depth,
                inherited: info.child(),
            });
        }
        return;
    }
    op.for_each_expression_mut(|expression| {
        visit_column_refs(expression, &mut |column| {
            info.ref_table_ids.insert(column.binding.table_index);
        });
    });
    match &op.kind {
        LogicalKind::Distinct {
            distinct_type: DistinctType::Distinct,
            distinct_targets,
            ..
        } => {
            if let Some(ExpressionKind::ColumnRef(first)) =
                distinct_targets.first().map(|target| &target.kind)
            {
                let table_index = first.binding.table_index;
                let group: Option<BTreeSet<ColumnBinding>> = distinct_targets
                    .iter()
                    .map(|target| match &target.kind {
                        ExpressionKind::ColumnRef(column) => Some(column.binding),
                        _ => None,
                    })
                    .collect();
                if let Some(group) = group {
                    info.distinct_groups.insert(table_index, group);
                }
            }
        }
        LogicalKind::Aggregate(aggregate) if aggregate.grouping_sets.len() <= 1 => {
            let group: BTreeSet<ColumnBinding> = (0..aggregate.groups.len() as u64)
                .map(|index| ColumnBinding::new(aggregate.group_index, index))
                .collect();
            if !group.is_empty() {
                info.distinct_groups.insert(aggregate.group_index, group);
            }
        }
        LogicalKind::Projection { table_index } => {
            // A distinct group above on this projection's columns maps to its input.
            if let Some(group) = info.distinct_groups.get(table_index).cloned() {
                project_group_down(&op.expressions, &group, info);
            }
        }
        LogicalKind::Get(get) if !get.table_filters.is_empty() => info.has_filter = true,
        _ => {}
    }
    if op.children.len() == 1 {
        walk_chain(&mut op.children[0], info, depth + 1, join_point);
    } else {
        for child in &mut op.children {
            process_pipe(child, info.child());
        }
        return;
    }
    if let LogicalKind::Projection { table_index } = op.kind {
        project_group_up(&op.expressions, table_index, info);
    }
}

/// DuckDB's projection case before the children: the group's columns as the input
/// columns they pass through.
fn project_group_down(expressions: &[Expression], group: &BTreeSet<ColumnBinding>, info: &mut PipeInfo) {
    let Some(first) = group.iter().next() else {
        return;
    };
    let Some(ExpressionKind::ColumnRef(first)) =
        expressions.get(first.column_index as usize).map(|e| &e.kind)
    else {
        return;
    };
    let ref_id = first.binding.table_index;
    let mut new_group = BTreeSet::new();
    for column in group {
        let Some(ExpressionKind::ColumnRef(reference)) =
            expressions.get(column.column_index as usize).map(|e| &e.kind)
        else {
            return;
        };
        if reference.binding.table_index != ref_id {
            return;
        }
        new_group.insert(reference.binding);
    }
    info.distinct_groups.insert(ref_id, new_group);
}

/// DuckDB's projection case after the children: a projection passing all columns of an
/// input's distinct group through is distinct on them.
fn project_group_up(expressions: &[Expression], table_index: u64, info: &mut PipeInfo) {
    // For each referenced table: the group's unseen column ids and the projected group.
    let mut references: BTreeMap<u64, (BTreeSet<u64>, BTreeSet<ColumnBinding>)> = BTreeMap::new();
    for (index, expression) in expressions.iter().enumerate() {
        let ExpressionKind::ColumnRef(column) = &expression.kind else {
            continue;
        };
        let Some(group) = info.distinct_groups.get(&column.binding.table_index) else {
            continue;
        };
        let entry = references.entry(column.binding.table_index).or_insert_with(|| {
            (
                group.iter().map(|binding| binding.column_index).collect(),
                BTreeSet::new(),
            )
        });
        entry.1.insert(ColumnBinding::new(table_index, index as u64));
        entry.0.remove(&column.binding.column_index);
    }
    for (_, (unseen, group)) in references {
        if unseen.is_empty() {
            info.distinct_groups.insert(table_index, group);
        }
    }
}

/// Whether `bindings` cover a whole distinct group of the first binding's table (DuckDB's
/// `ContainDistinctGroup`).
fn contains_distinct_group(info: &PipeInfo, bindings: &[ColumnBinding]) -> bool {
    let Some(first) = bindings.first() else {
        return false;
    };
    let Some(group) = info.distinct_groups.get(&first.table_index) else {
        return false;
    };
    let used: BTreeSet<u64> = bindings
        .iter()
        .filter(|binding| group.contains(binding))
        .map(|binding| binding.column_index)
        .collect();
    used.len() == group.len()
}

/// DuckDB's `TryEliminateJoin` for the join where the walk of `root`'s pipe stopped.
fn try_eliminate_join(root: &mut LogicalOperator, join_point: JoinPoint, info: &mut PipeInfo) {
    let Some(join_op) = chain_operator(root, join_point.depth) else {
        return;
    };
    if join_op.children.len() != 2 {
        return;
    }
    let left_info = process_pipe(&mut join_op.children[0], join_point.inherited.child());
    let right_info = process_pipe(&mut join_op.children[1], join_point.inherited);
    let LogicalKind::ComparisonJoin(join) = &join_op.kind else {
        return;
    };
    let (mut is_output_unique, inner_index, outer_index) = match join.join_type {
        JoinType::Left => (false, 1, 0),
        JoinType::Single => (true, 1, 0),
        JoinType::Right => (false, 0, 1),
        _ => return,
    };
    let inner_info = if inner_index == 0 { left_info } else { right_info };
    if inner_info.has_filter || !join.join_filters.is_empty() {
        return;
    }
    // The join's output above must only use outer columns.
    if join_op.children[inner_index]
        .column_bindings()
        .iter()
        .any(|binding| info.ref_table_ids.contains(&binding.table_index))
    {
        return;
    }
    info.distinct_groups.extend(inner_info.distinct_groups);
    if info.distinct_groups.is_empty() {
        return;
    }
    if !is_output_unique {
        // The inner join keys cover a distinct group.
        let mut inner_bindings = Vec::with_capacity(join.conditions.len());
        let mut all_column_equalities = true;
        for condition in &join.conditions {
            let (ExpressionKind::ColumnRef(left), ExpressionKind::ColumnRef(right)) =
                (&condition.left.kind, &condition.right.kind)
            else {
                all_column_equalities = false;
                break;
            };
            if condition.comparison != ExpressionType::CompareEqual {
                all_column_equalities = false;
                break;
            }
            inner_bindings.push(if inner_index == 0 {
                left.binding
            } else {
                right.binding
            });
        }
        is_output_unique = all_column_equalities && contains_distinct_group(info, &inner_bindings);
    }
    if !is_output_unique {
        let outer_bindings = join_op.children[outer_index].column_bindings();
        is_output_unique = contains_distinct_group(info, &outer_bindings);
    }
    if is_output_unique {
        let outer = join_op.children.swap_remove(outer_index);
        *join_op = outer;
    }
}
