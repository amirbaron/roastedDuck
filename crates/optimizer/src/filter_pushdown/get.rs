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
            expression.alias = "rowid".into();
        } else {
            let name = get.column_name(column);
            if !name.is_empty() {
                expression.alias = name;
            }
        }
        return;
    }
    expression.for_each_child_mut(|child| normalize_column_ref_aliases(child, get));
}

/// Replaces the references to columns of `get` whose value `known` holds (by binding
/// position) with constants (`ConvertKnownColRefToConstants`).
fn convert_known_columns(
    expression: &mut Expression,
    known: &[Option<rustdb_types::Value>],
    table_index: u64,
) {
    if let ExpressionKind::ColumnRef(column) = &expression.kind {
        if column.binding.table_index != table_index {
            return;
        }
        if let Some(Some(value)) = usize::try_from(column.binding.column_index)
            .ok()
            .and_then(|index| known.get(index))
        {
            *expression = Expression::constant(value.clone());
        }
        return;
    }
    expression.for_each_child_mut(|child| convert_known_columns(child, known, table_index));
}

impl FilterPushdown<'_> {
    /// `MultiFileComplexFilterPushdown` (`HivePartitioning::ApplyFiltersToFileList`): the
    /// files for which a filter on columns their paths determine is false are pruned, and
    /// such filters are dropped; the others remain.
    fn pushdown_file_filters(&mut self, get: &mut LogicalGet) -> Result<()> {
        let (Some(pushdown), Some(bind_data)) =
            (get.function.file_filter_pushdown.clone(), get.bind_data.clone())
        else {
            return Ok(());
        };
        if self.filters.is_empty() {
            return Ok(());
        }
        let Some(files) = (pushdown.files)(bind_data.as_ref()) else {
            return Ok(());
        };
        // The scanned columns by name (`HivePartitioningFilterInfo::column_map`).
        let mut column_map: Vec<(rustdb_common::ByteString, usize, rustdb_types::LogicalType)> = Vec::new();
        for (position, column) in get.column_ids.iter().enumerate() {
            let Some(index) = usize::try_from(column.index)
                .ok()
                .filter(|&index| index < get.names.len())
            else {
                continue;
            };
            let name = &get.names[index];
            if column_map.iter().any(|(existing, ..)| existing == name) {
                continue;
            }
            column_map.push((name.clone(), position, get.returned_types[index].clone()));
        }
        let mut kept_files = Vec::new();
        let mut preserved = vec![false; self.filters.len()];
        let mut applied = vec![false; self.filters.len()];
        let mut file_filters = String::new();
        for (file_index, path) in files.iter().enumerate() {
            let mut known = vec![None; get.column_ids.len()];
            for (name, position, ty) in &column_map {
                known[*position] = (pushdown.known_value)(bind_data.as_ref(), path, name, ty)?;
            }
            let mut prune = false;
            for (index, filter) in self.filters.iter().enumerate() {
                let mut copy = filter.filter.clone();
                convert_known_columns(&mut copy, &known, get.table_index);
                let value = (copy.is_scalar() && copy.is_foldable())
                    .then(|| crate::expression_util::try_evaluate_scalar(self.optimizer.context(), &copy))
                    .flatten();
                match value {
                    None => preserved[index] = true,
                    Some(value) => {
                        if value.is_null() || !value.as_bool().unwrap_or(false) {
                            prune = true;
                            if !applied[index] {
                                file_filters.push_str(&filter.filter.to_string());
                                applied[index] = true;
                            }
                        }
                    }
                }
            }
            if !prune {
                kept_files.push(file_index);
            }
        }
        get.file_filters.filters.push_str(&file_filters);
        get.file_filters.total_files = Some(files.len() as u64);
        get.file_filters.filtered_files = Some(kept_files.len() as u64);
        let mut index = 0;
        self.filters.retain(|_| {
            let keep = preserved[index];
            index += 1;
            keep
        });
        if kept_files.len() != files.len() {
            get.bind_data = Some((pushdown.with_files)(bind_data.as_ref(), &kept_files)?);
        }
        Ok(())
    }

    pub(super) fn pushdown_get(&mut self, mut op: LogicalOperator) -> Result<LogicalOperator> {
        let LogicalKind::Get(get) = &mut op.kind else {
            return self.finish_pushdown(op);
        };
        for filter in &mut self.filters {
            normalize_column_ref_aliases(&mut filter.filter, get);
        }
        if get.function.file_filter_pushdown.is_some() {
            self.pushdown_file_filters(get)?;
            if self.filters.is_empty() {
                return Ok(op);
            }
        }
        let LogicalKind::Get(get) = &mut op.kind else {
            return self.finish_pushdown(op);
        };
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
        let supported = |column, filter: &rustdb_storage::TableFilter| match (
            &get.function.filter_support,
            &get.bind_data,
        ) {
            (Some(supported), Some(data)) => supported(data.as_ref(), column, filter),
            _ => true,
        };
        get.table_filters =
            self.combiner
                .generate_table_scan_filters(&get.column_ids, &mut pushdown_results, &supported)?;
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
