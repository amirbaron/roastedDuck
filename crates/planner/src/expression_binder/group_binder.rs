//! GROUP BY terms (DuckDB's `GroupBinder`): columns of the FROM clause first, then
//! select-list aliases and positions.

use std::collections::HashSet;

use rustdb_ast::{ParsedExpression, ParsedExpressionKind};
use rustdb_common::{CaseInsensitiveMap, Error, ErrorKind, Result};
use rustdb_types::Value;

use super::base::{BinderBase, expression_binder_accessors, expression_binder_base};
use super::base_select_binder::SelectBindState;
use super::{BindResult, ExpressionBinder};
use crate::binder::expression::{bind_column_ref, bind_expression_base, bind_unsupported_expression};
use crate::binder::{Binder, Scope};
use crate::expression::Expression;

/// Binds the GROUP BY terms of one select node.
#[derive(Debug)]
pub struct GroupBinder<'a> {
    base: BinderBase<'a>,
    /// The select node's (expanded) select list; entries used as groups are replaced by
    /// references to the group.
    select_list: &'a mut Vec<ParsedExpression>,
    bind_state: &'a SelectBindState,
    /// Select-list aliases used as groups, and their group indexes.
    group_alias_map: &'a mut CaseInsensitiveMap<usize>,
    /// The index of the group being bound.
    pub bind_index: usize,
    /// The group before binding, for matching it in other clauses.
    pub unbound_expression: Option<ParsedExpression>,
    used_aliases: HashSet<usize>,
}

impl<'a> GroupBinder<'a> {
    pub fn new(
        binder: &'a mut Binder,
        outer: Option<&'a mut dyn Scope>,
        select_list: &'a mut Vec<ParsedExpression>,
        bind_state: &'a SelectBindState,
        group_alias_map: &'a mut CaseInsensitiveMap<usize>,
    ) -> Self {
        Self {
            base: BinderBase::new(binder, outer),
            select_list,
            bind_state,
            group_alias_map,
            bind_index: 0,
            unbound_expression: None,
            used_aliases: HashSet::new(),
        }
    }

    /// Binds select-list entry `entry` as the group (DuckDB's `BindSelectRef`).
    fn bind_select_ref(&mut self, entry: usize) -> Result<BindResult> {
        if self.used_aliases.contains(&entry) {
            // Grouping on the same entry twice has no effect.
            return Ok(Expression::constant(Value::integer(42)).into());
        }
        if entry >= self.select_list.len() {
            return Err(Error::binder(format!(
                "GROUP BY term out of range - should be between 1 and {}",
                self.select_list.len()
            )));
        }
        self.unbound_expression = Some(self.select_list[entry].clone());
        let mut select_entry = std::mem::replace(
            &mut self.select_list[entry],
            ParsedExpression::column_ref([entry.to_string()]),
        );
        let bound = self.bind(&mut select_entry, false)?;
        self.group_alias_map.insert(entry.to_string(), self.bind_index);
        self.used_aliases.insert(entry);
        Ok(bound.into())
    }
}

expression_binder_base!(GroupBinder);

impl ExpressionBinder for GroupBinder<'_> {
    expression_binder_accessors!();

    fn bind_expression(&mut self, expr: &mut ParsedExpression, depth: u64, root_expression: bool) -> Result<BindResult> {
        if root_expression && depth == 0 {
            match &expr.kind {
                ParsedExpressionKind::ColumnRef(_) => return bind_column_ref(self, expr, 0, true),
                ParsedExpressionKind::Constant(constant) => {
                    if !constant.value.logical_type().is_integral() {
                        return Ok(Expression::constant(constant.value.clone()).into());
                    }
                    let index = constant.value.as_i128().unwrap_or_default();
                    return self.bind_select_ref((index - 1) as usize);
                }
                ParsedExpressionKind::Parameter(_) => {
                    return Err(Error::new(
                        ErrorKind::ParameterNotAllowed,
                        "Parameter not supported in GROUP BY clause",
                    ));
                }
                _ => {}
            }
        }
        match &expr.kind {
            ParsedExpressionKind::Default => {
                bind_unsupported_expression(self, expr, depth, "GROUP BY clause cannot contain DEFAULT clause")
            }
            ParsedExpressionKind::Window(_) => {
                bind_unsupported_expression(self, expr, depth, "GROUP BY clause cannot contain window functions!")
            }
            _ => bind_expression_base(self, expr, depth, root_expression),
        }
    }

    fn unsupported_aggregate_message(&self) -> String {
        "GROUP BY clause cannot contain aggregates!".to_owned()
    }

    fn try_resolve_alias_reference(
        &mut self,
        expr: &mut ParsedExpression,
        _depth: u64,
        root_expression: bool,
    ) -> Result<Option<BindResult>> {
        let ParsedExpressionKind::ColumnRef(column) = &expr.kind else {
            return Ok(None);
        };
        let Some(alias_name) = column.column_names.last().cloned() else {
            return Ok(None);
        };
        let Some(&entry) = self.bind_state.alias_map.get(&alias_name) else {
            return Ok(None);
        };
        if !root_expression {
            return Ok(Some(BindResult::Unbound(
                Error::binder(format!(
                    "Alias with name \"{alias_name}\" exists, but aliases cannot be used as part of an expression in \
                     the GROUP BY"
                ))
                .with_position(expr.query_location)
                .into(),
            )));
        }
        let result = self.bind_select_ref(entry)?;
        if matches!(result, BindResult::Bound(_)) {
            self.group_alias_map.insert(alias_name, self.bind_index);
        }
        Ok(Some(result))
    }
}
