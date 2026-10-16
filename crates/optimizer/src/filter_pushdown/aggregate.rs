//! Filter pushdown through aggregates (DuckDB's `pushdown_aggregate.cpp`): filters on
//! groups present in every grouping set go below the aggregate.

use rustdb_common::Result;
use rustdb_planner::{LogicalKind, LogicalOperator};

use super::{FilterPushdown, empty_result};
use crate::expression_util::{column_bindings, replace_column_refs, visit_column_refs};
use crate::filter_combiner::FilterResult;

impl FilterPushdown<'_> {
    pub(super) fn pushdown_aggregate(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let LogicalKind::Aggregate(aggregate) = &op.kind else {
            return self.finish_pushdown(op);
        };
        let mut child_pushdown = self.child();
        let mut index = 0;
        while index < self.filters.len() {
            let filter = &self.filters[index];
            if filter.bindings.contains(&aggregate.aggregate_index)
                || filter.bindings.contains(&aggregate.groupings_index)
                || aggregate.groups.is_empty()
            {
                index += 1;
                continue;
            }
            let bindings = column_bindings(&filter.filter);
            if bindings.is_empty() {
                index += 1;
                continue;
            }
            let in_every_set = aggregate
                .grouping_sets
                .iter()
                .all(|set| bindings.iter().all(|binding| set.contains(&binding.column_index)));
            if !in_every_set {
                index += 1;
                continue;
            }
            let mut is_volatile = false;
            visit_column_refs(&filter.filter, &mut |column| {
                if aggregate
                    .groups
                    .get(column.binding.column_index as usize)
                    .is_some_and(rustdb_planner::Expression::is_volatile)
                {
                    is_volatile = true;
                }
            });
            if is_volatile {
                index += 1;
                continue;
            }
            let mut filter = self.filters.remove(index).filter;
            replace_column_refs(&mut filter, &mut |column| {
                let binding = column.as_column_ref()?.binding;
                aggregate.groups.get(binding.column_index as usize).cloned()
            });
            if child_pushdown.add_filter(filter)? == FilterResult::Unsatisfiable {
                return empty_result(op);
            }
        }
        child_pushdown.generate_filters();
        op.children = child_pushdown.rewrite_children_with_self(std::mem::take(&mut op.children))?;
        self.finish_pushdown(op)
    }
}
