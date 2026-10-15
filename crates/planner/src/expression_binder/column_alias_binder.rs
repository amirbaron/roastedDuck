//! References to select-list aliases from WHERE, HAVING and QUALIFY (DuckDB's
//! `ColumnAliasBinder`).

use std::collections::HashSet;

use rustdb_ast::{ParsedExpression, ParsedExpressionKind};
use rustdb_common::Result;

use super::base_select_binder::SelectBindState;
use super::is_potential_alias;

/// Replaces alias references by the aliased select-list expression. An expansion is
/// bracketed by [`ColumnAliasBinder::begin`] and [`ColumnAliasBinder::end`], between which
/// the enclosing binder binds the expanded expression (which may reference other aliases).
#[derive(Debug, Default)]
pub struct ColumnAliasBinder {
    /// Aliases being expanded, to reject self-references.
    visited_select_indexes: HashSet<usize>,
}

impl ColumnAliasBinder {
    /// Whether `names` refers to a select-list alias (DuckDB's `DoesColumnAliasExist`).
    pub fn does_column_alias_exist(bind_state: &SelectBindState, names: &[String]) -> bool {
        is_potential_alias(names)
            && names
                .first()
                .is_some_and(|name| bind_state.alias_map.contains_key(name))
    }

    /// If `expr` names an alias that is not being expanded already, replaces it by the
    /// aliased expression and returns the alias's select-list index (first half of
    /// DuckDB's `BindAlias`).
    pub fn begin(
        &mut self,
        bind_state: &mut SelectBindState,
        expr: &mut ParsedExpression,
    ) -> Result<Option<usize>> {
        let ParsedExpressionKind::ColumnRef(column) = &expr.kind else {
            return Ok(None);
        };
        if !is_potential_alias(&column.column_names) {
            return Ok(None);
        }
        let Some(name) = column.column_names.last() else {
            return Ok(None);
        };
        let Some(&index) = bind_state.alias_map.get(name) else {
            return Ok(None);
        };
        if self.visited_select_indexes.contains(&index) {
            return Ok(None);
        }
        *expr = bind_state.bind_alias(index)?;
        self.visited_select_indexes.insert(index);
        Ok(Some(index))
    }

    /// Ends the expansion of alias `index`.
    pub fn end(&mut self, index: usize) {
        self.visited_select_indexes.remove(&index);
    }
}
