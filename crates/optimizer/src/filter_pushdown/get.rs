//! Filter pushdown into table scans (DuckDB's `pushdown_get.cpp`): comparisons of
//! columns with constants become table filters that the scan applies (and uses to skip
//! row groups by their statistics).

use rustdb_ast::ExpressionType;
use rustdb_common::Result;
use rustdb_planner::operator::{COLUMN_IDENTIFIER_ROW_ID, LogicalGet};
use rustdb_planner::{Expression, ExpressionKind, LogicalKind, LogicalOperator};

use super::{FilterPushdown, empty_result};
use crate::expression_util::can_throw;
use crate::filter_combiner::{FilterPushdownResult, FilterResult, try_pushdown_generic_expression};

/// Whether a scan applies table filters itself (DuckDB's `TableFunction::filter_pushdown`):
/// base table scans and table functions that take filters do.
pub fn supports_filter_pushdown(get: &LogicalGet) -> bool {
    get.table.is_some() || (get.function.filter_pushdown && get.function.pushdown_filters.is_some())
}

/// Renames column references to scanned columns back to the scan's column names
/// (DuckDB's `NormalizeColumnRefAliases`).
fn normalize_column_ref_aliases(expression: &mut Expression, get: &LogicalGet) {
    if let ExpressionKind::ColumnRef(column) = &expression.kind {
        let binding = column.binding;
        if binding.table_index != get.table_index {
            return;
        }
        let Some(column) = get.column_ids.get(binding.column_index as usize) else {
            return;
        };
        if column.index == COLUMN_IDENTIFIER_ROW_ID {
            "rowid".clone_into(&mut expression.alias);
        } else if let Some(name) = get.names.get(column.index as usize) {
            expression.alias.clone_from(name);
        }
        return;
    }
    expression.for_each_child_mut(|child| normalize_column_ref_aliases(child, get));
}

impl FilterPushdown<'_> {
    pub(super) fn pushdown_get(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let LogicalKind::Get(get) = &mut op.kind else {
            return self.finish_pushdown(op);
        };
        for filter in &mut self.filters {
            normalize_column_ref_aliases(&mut filter.filter, get);
        }
        if !get.table_filters.is_empty() || !supports_filter_pushdown(get) {
            return self.finish_pushdown(op);
        }
        if self.push_filters()? == FilterResult::Unsatisfiable {
            return empty_result(op);
        }
        let LogicalKind::Get(get) = &mut op.kind else {
            return self.finish_pushdown(op);
        };
        let mut pushdown_results = Vec::new();
        get.table_filters = self
            .combiner
            .generate_table_scan_filters(&get.column_ids, &mut pushdown_results)?;
        self.generate_filters();
        while pushdown_results.len() < self.filters.len() {
            pushdown_results.push(FilterPushdownResult::NoPushdown);
        }
        // Filters that did not become specialized table filters are pushed as expression
        // filters if they read a single column.
        let mut index = 0;
        while index < self.filters.len() {
            if pushdown_results[index] != FilterPushdownResult::NoPushdown {
                index += 1;
                continue;
            }
            let expression = &self.filters[index].filter;
            if expression.is_volatile()
                || (can_throw(expression)
                    && (expression.expression_type == ExpressionType::CompareIn || self.filters.len() > 1))
            {
                index += 1;
                continue;
            }
            let LogicalKind::Get(get) = &mut op.kind else {
                break;
            };
            if try_pushdown_generic_expression(get, expression) == FilterPushdownResult::PushedDownFully {
                self.filters.remove(index);
                pushdown_results.remove(index);
            } else {
                index += 1;
            }
        }
        self.finish_pushdown(op)
    }
}
