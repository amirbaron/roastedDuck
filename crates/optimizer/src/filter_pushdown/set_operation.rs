//! Filter pushdown through set operations (DuckDB's `pushdown_set_operation.cpp`): each
//! input gets a copy of the filters with references rewritten to its own columns.

use roastedduck_common::Result;
use roastedduck_planner::{LogicalKind, LogicalOperator, LogicalOperatorType};

use super::{Filter, FilterPushdown, empty_result};
use crate::expression_util::replace_column_refs;

fn is_empty_result(op: &LogicalOperator) -> bool {
    matches!(op.kind, LogicalKind::EmptyResult { .. })
}

impl FilterPushdown<'_> {
    pub(super) fn pushdown_set_operation(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let LogicalKind::SetOperation {
            table_index,
            setop_all,
            ..
        } = op.kind
        else {
            return self.finish_pushdown(op);
        };
        let operator_type = op.operator_type();
        let mut children = std::mem::take(&mut op.children);
        for child in &mut children {
            let child_bindings = child.column_bindings();
            let mut child_pushdown = self.child();
            for original in &self.filters {
                let mut filter = original.filter.clone();
                replace_column_refs(&mut filter, &mut |column| {
                    let mut column = column.clone();
                    if let roastedduck_planner::ExpressionKind::ColumnRef(reference) = &mut column.kind
                        && let Some(binding) = child_bindings.get(reference.binding.column_index as usize)
                    {
                        reference.binding = *binding;
                    }
                    Some(column)
                });
                child_pushdown.filters.push(Filter::new(filter));
            }
            let input = std::mem::replace(
                child,
                LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 }),
            );
            *child = child_pushdown.rewrite(input)?;
        }
        self.filters.clear();
        op.children = children;
        if op.children.iter().all(is_empty_result) {
            return empty_result(op);
        }
        if operator_type == LogicalOperatorType::LogicalUnion {
            op.children.retain(|child| !is_empty_result(child));
            return Ok(op);
        }
        let left_empty = is_empty_result(&op.children[0]);
        let right_empty = op.children.get(1).is_some_and(is_empty_result);
        if left_empty && setop_all {
            return empty_result(op);
        }
        if right_empty && setop_all {
            if operator_type == LogicalOperatorType::LogicalIntersect {
                return empty_result(op);
            }
            if matches!(op.children[0].kind, LogicalKind::Projection { .. }) {
                let mut left = op.children.swap_remove(0);
                if let LogicalKind::Projection { table_index: index } = &mut left.kind {
                    *index = table_index;
                }
                return Ok(left);
            }
        }
        Ok(op)
    }
}
