//! Filter pushdown through window operators (DuckDB's `pushdown_window.cpp`): a filter
//! on columns every window function partitions by does not change any partition's
//! result, so it can go below the window.

use rustdb_common::Result;
use rustdb_planner::{ColumnBinding, ExpressionKind, LogicalOperator};

use super::FilterPushdown;
use crate::expression_util::column_bindings;

impl FilterPushdown<'_> {
    pub(super) fn pushdown_window(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let mut partition_bindings: Vec<Vec<ColumnBinding>> = Vec::new();
        for expression in &op.expressions {
            let ExpressionKind::Window(window) = &expression.kind else {
                continue;
            };
            if window.partitions.is_empty() {
                return self.finish_pushdown(op);
            }
            let bindings = window
                .partitions
                .iter()
                .filter_map(|partition| partition.as_column_ref().map(|column| column.binding))
                .collect();
            partition_bindings.push(bindings);
        }
        if partition_bindings.is_empty() {
            return self.finish_pushdown(op);
        }
        let mut pushdown = self.child();
        let mut leftover = Vec::new();
        for filter in std::mem::take(&mut self.filters) {
            let bindings = column_bindings(&filter.filter);
            let on_all_partitions = partition_bindings
                .iter()
                .all(|partition| bindings.iter().all(|binding| partition.contains(binding)));
            if on_all_partitions {
                pushdown.filters.push(filter);
            } else {
                leftover.push(filter);
            }
        }
        op.children = pushdown.rewrite_children_with_self(std::mem::take(&mut op.children))?;
        self.filters = leftover;
        self.finish_pushdown(op)
    }
}
