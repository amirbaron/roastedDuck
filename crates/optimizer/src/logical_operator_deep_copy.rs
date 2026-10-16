//! Copying a plan subtree with fresh table indexes (DuckDB's `LogicalOperatorDeepCopy`
//! and `TableBindingReplacer`), so that the copy can be used next to the original.

use std::collections::HashMap;

use rustdb_ast::JoinType;
use rustdb_planner::{Expression, ExpressionKind, LogicalKind, LogicalOperator};

use crate::optimizer::Optimizer;

/// A copy of `op` whose operators define new table indexes, with every column
/// reference in the copy following them.
pub fn deep_copy(optimizer: &Optimizer, op: &LogicalOperator) -> LogicalOperator {
    let mut copy = op.clone();
    let mut replacements = HashMap::new();
    replace_table_indexes(optimizer, &mut copy, &mut replacements);
    replace_bindings(&mut copy, &replacements);
    copy
}

/// Gives every operator below and including `op` new table indexes, children first.
fn replace_table_indexes(
    optimizer: &Optimizer,
    op: &mut LogicalOperator,
    replacements: &mut HashMap<u64, u64>,
) {
    for child in &mut op.children {
        replace_table_indexes(optimizer, child, replacements);
    }
    let mut replace = |index: &mut u64| {
        let new_index = optimizer.generate_table_index();
        replacements.insert(*index, new_index);
        *index = new_index;
    };
    match &mut op.kind {
        LogicalKind::Projection { table_index }
        | LogicalKind::ChunkGet { table_index, .. }
        | LogicalKind::DelimGet { table_index, .. }
        | LogicalKind::ExpressionGet { table_index, .. }
        | LogicalKind::DummyScan { table_index }
        | LogicalKind::CteRef { table_index, .. }
        | LogicalKind::SetOperation { table_index, .. }
        | LogicalKind::RecursiveCte { table_index, .. }
        | LogicalKind::MaterializedCte { table_index, .. } => replace(table_index),
        LogicalKind::Aggregate(aggregate) => {
            replace(&mut aggregate.group_index);
            replace(&mut aggregate.aggregate_index);
            replace(&mut aggregate.groupings_index);
        }
        LogicalKind::Window { window_index } => replace(window_index),
        LogicalKind::Unnest { unnest_index } => replace(unnest_index),
        LogicalKind::Pivot { pivot_index, .. } => replace(pivot_index),
        LogicalKind::Get(get) => replace(&mut get.table_index),
        LogicalKind::ComparisonJoin(join) if join.join_type == JoinType::Mark => {
            replace(&mut join.mark_index);
        }
        LogicalKind::DependentJoin(join) if join.join.join_type == JoinType::Mark => {
            replace(&mut join.join.mark_index);
        }
        LogicalKind::Insert(insert) => replace(&mut insert.table_index),
        LogicalKind::Delete(delete) => replace(&mut delete.table_index),
        LogicalKind::MergeInto(merge) => replace(&mut merge.table_index),
        _ => {}
    }
}

/// Points the column references and CTE references of the copy at the new table indexes
/// (DuckDB's `TableBindingReplacer`).
fn replace_bindings(op: &mut LogicalOperator, replacements: &HashMap<u64, u64>) {
    match &mut op.kind {
        LogicalKind::CteRef { cte_index, .. } => {
            if let Some(&new_index) = replacements.get(cte_index) {
                *cte_index = new_index;
            }
        }
        LogicalKind::RecursiveCte {
            correlated_columns, ..
        }
        | LogicalKind::MaterializedCte {
            correlated_columns, ..
        } => {
            for column in correlated_columns.iter_mut() {
                if let Some(&new_index) = replacements.get(&column.binding.table_index) {
                    column.binding.table_index = new_index;
                }
            }
        }
        LogicalKind::DependentJoin(join) => {
            for column in &mut join.correlated_columns {
                if let Some(&new_index) = replacements.get(&column.binding.table_index) {
                    column.binding.table_index = new_index;
                }
            }
        }
        _ => {}
    }
    for child in &mut op.children {
        replace_bindings(child, replacements);
    }
    op.for_each_expression_mut(|expression| replace_expression_bindings(expression, replacements));
}

fn replace_expression_bindings(expression: &mut Expression, replacements: &HashMap<u64, u64>) {
    if let ExpressionKind::ColumnRef(column) = &mut expression.kind
        && let Some(&new_index) = replacements.get(&column.binding.table_index)
    {
        column.binding.table_index = new_index;
    }
    expression.for_each_child_mut(|child| replace_expression_bindings(child, replacements));
}
