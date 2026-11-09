//! Filter pushdown through projections (DuckDB's `pushdown_projection.cpp`): column
//! references are replaced by the projected expressions.

use roastedduck_common::Result;
use roastedduck_planner::{LogicalKind, LogicalOperator};

use super::{FilterPushdown, empty_result};
use crate::expression_util::{can_throw, replace_column_refs, visit_column_refs};
use crate::filter_combiner::FilterResult;

impl FilterPushdown<'_> {
    pub(super) fn pushdown_projection(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let mut child_pushdown = self.child();
        let mut remain_expressions = Vec::new();
        for filter in std::mem::take(&mut self.filters) {
            let mut is_volatile = false;
            visit_column_refs(&filter.filter, &mut |column| {
                if op
                    .expressions
                    .get(column.binding.column_index as usize)
                    .is_some_and(roastedduck_planner::Expression::is_volatile)
                {
                    is_volatile = true;
                }
            });
            if is_volatile || can_throw(&filter.filter) {
                remain_expressions.push(filter.filter);
                continue;
            }
            let mut expression = filter.filter;
            replace_column_refs(&mut expression, &mut |column| {
                let binding = column.as_column_ref()?.binding;
                let mut copy = op.expressions.get(binding.column_index as usize)?.clone();
                if !column.alias.is_empty() {
                    copy.alias.clone_from(&column.alias);
                }
                Some(copy)
            });
            if child_pushdown.add_filter(expression)? == FilterResult::Unsatisfiable {
                return empty_result(op);
            }
        }
        child_pushdown.generate_filters();
        op.children = child_pushdown.rewrite_children_with_self(std::mem::take(&mut op.children))?;
        if op
            .children
            .first()
            .is_some_and(|child| matches!(child.kind, LogicalKind::EmptyResult { .. }))
        {
            return empty_result(op);
        }
        Ok(Self::add_logical_filter(op, remain_expressions))
    }
}
