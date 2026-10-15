//! Expression binders that only restrict the base behaviour (DuckDB's `WhereBinder`,
//! `ConstantBinder`, `AggregateBinder`, `InsertBinder` and `TableFunctionBinder`).

use rustdb_ast::{ParsedExpression, ParsedExpressionKind};
use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, Value};

use super::base::{BinderBase, expression_binder_accessors, expression_binder_base};
use super::base_select_binder::SelectBindState;
use super::column_alias_binder::ColumnAliasBinder;
use super::{
    BindError, BindResult, ExpressionBinder, is_potential_alias, parameter_not_resolved, unsupported,
};
use crate::binder::expression::{bind_expression_base, bind_unsupported_expression};
use crate::binder::{Binder, DUMMY_NAME, Scope};
use crate::expression::Expression;

/// The base expression binder without specialisation (DuckDB's `ExpressionBinder` used
/// directly, as for LIMIT values).
#[derive(Debug)]
pub struct BaseBinder<'a> {
    base: BinderBase<'a>,
}

impl<'a> BaseBinder<'a> {
    pub fn new(binder: &'a mut Binder, outer: Option<&'a mut dyn Scope>) -> Self {
        Self {
            base: BinderBase::new(binder, outer),
        }
    }
}

expression_binder_base!(BaseBinder);

impl ExpressionBinder for BaseBinder<'_> {
    expression_binder_accessors!();
}

/// The WHERE clause (DuckDB's `WhereBinder`): select-list aliases may be referenced.
#[derive(Debug)]
pub struct WhereBinder<'a> {
    base: BinderBase<'a>,
    /// The select list whose aliases may be referenced, if any.
    bind_state: Option<&'a mut SelectBindState>,
    alias_binder: ColumnAliasBinder,
}

impl<'a> WhereBinder<'a> {
    pub fn new(
        binder: &'a mut Binder,
        outer: Option<&'a mut dyn Scope>,
        bind_state: Option<&'a mut SelectBindState>,
    ) -> Self {
        Self {
            base: BinderBase::new(binder, outer).with_target_type(LogicalType::BOOLEAN),
            bind_state,
            alias_binder: ColumnAliasBinder::default(),
        }
    }
}

expression_binder_base!(WhereBinder {
    fn does_column_alias_exist(&self, names: &[String]) -> bool {
        self.bind_state
            .as_deref()
            .is_some_and(|state| ColumnAliasBinder::does_column_alias_exist(state, names))
    }
});

impl ExpressionBinder for WhereBinder<'_> {
    expression_binder_accessors!();

    fn bind_expression(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        match &expr.kind {
            ParsedExpressionKind::Default => {
                bind_unsupported_expression(self, expr, depth, "WHERE clause cannot contain DEFAULT clause")
            }
            ParsedExpressionKind::Window(_) => bind_unsupported_expression(
                self,
                expr,
                depth,
                "WHERE clause cannot contain window functions!",
            ),
            _ => bind_expression_base(self, expr, depth, root_expression),
        }
    }

    fn unsupported_aggregate_message(&self) -> String {
        "WHERE clause cannot contain aggregates!".to_owned()
    }

    fn try_resolve_alias_reference(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<Option<BindResult>> {
        let Some(bind_state) = self.bind_state.as_deref_mut() else {
            return Ok(None);
        };
        let Some(index) = self.alias_binder.begin(bind_state, expr)? else {
            return Ok(None);
        };
        let result = self.bind_expression(expr, depth, root_expression);
        self.alias_binder.end(index);
        result.map(Some)
    }
}

/// Expressions that must be constant, such as LIMIT values (DuckDB's `ConstantBinder`).
#[derive(Debug)]
pub struct ConstantBinder<'a> {
    base: BinderBase<'a>,
    clause: String,
}

impl<'a> ConstantBinder<'a> {
    pub fn new(binder: &'a mut Binder, outer: Option<&'a mut dyn Scope>, clause: impl Into<String>) -> Self {
        Self {
            base: BinderBase::new(binder, outer),
            clause: clause.into(),
        }
    }
}

expression_binder_base!(ConstantBinder);

impl ExpressionBinder for ConstantBinder<'_> {
    expression_binder_accessors!();

    fn bind_expression(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        match &expr.kind {
            ParsedExpressionKind::ColumnRef(column) => {
                if let [name] = column.column_names.as_slice()
                    && let Some(mut function) = self.sql_value_function(name)
                {
                    *expr = function.clone();
                    return self.bind_expression(&mut function, depth, root_expression);
                }
                Err(unsupported(expr, format!("{} cannot contain column names", self.clause)).error)
            }
            ParsedExpressionKind::Subquery(_) => Err(Error::binder(format!(
                "{} cannot contain subqueries",
                self.clause
            ))),
            ParsedExpressionKind::Default => {
                let message = format!("{} cannot contain DEFAULT clause", self.clause);
                bind_unsupported_expression(self, expr, depth, &message)
            }
            ParsedExpressionKind::Window(_) => {
                let message = format!("{} cannot contain window functions!", self.clause);
                bind_unsupported_expression(self, expr, depth, &message)
            }
            _ => bind_expression_base(self, expr, depth, root_expression),
        }
    }

    fn unsupported_aggregate_message(&self) -> String {
        format!("{} cannot contain aggregates!", self.clause)
    }
}

/// The arguments of an aggregate (DuckDB's `AggregateBinder`): no nested aggregates or
/// window functions. It replaces the select binder of its level while active.
#[derive(Debug)]
pub struct AggregateBinder<'a> {
    base: BinderBase<'a>,
}

impl<'a> AggregateBinder<'a> {
    pub fn new(binder: &'a mut Binder, outer: Option<&'a mut dyn Scope>) -> Self {
        Self {
            base: BinderBase::new(binder, outer),
        }
    }
}

expression_binder_base!(AggregateBinder);

impl ExpressionBinder for AggregateBinder<'_> {
    expression_binder_accessors!();

    fn bind_expression(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        if let ParsedExpressionKind::Window(_) = expr.kind {
            return Err(unsupported(
                expr,
                "aggregate function calls cannot contain window function calls",
            )
            .error);
        }
        bind_expression_base(self, expr, depth, root_expression)
    }

    fn unsupported_aggregate_message(&self) -> String {
        "aggregate function calls cannot be nested".to_owned()
    }
}

/// The rows of a VALUES list (DuckDB's `InsertBinder`).
#[derive(Debug)]
pub struct InsertBinder<'a> {
    base: BinderBase<'a>,
}

impl<'a> InsertBinder<'a> {
    pub fn new(binder: &'a mut Binder, outer: Option<&'a mut dyn Scope>) -> Self {
        Self {
            base: BinderBase::new(binder, outer),
        }
    }

    /// Sets the type the next expression is cast to.
    pub fn set_target_type(&mut self, target: Option<LogicalType>) {
        self.base.state.target_type = target;
    }
}

expression_binder_base!(InsertBinder);

impl ExpressionBinder for InsertBinder<'_> {
    expression_binder_accessors!();

    fn bind_expression(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        match &expr.kind {
            ParsedExpressionKind::Default => Ok(BindResult::Unbound(unsupported(
                expr,
                "DEFAULT is not allowed here!",
            ))),
            ParsedExpressionKind::Window(_) => Ok(BindResult::Unbound(unsupported(
                expr,
                "INSERT statement cannot contain window functions!",
            ))),
            _ => bind_expression_base(self, expr, depth, root_expression),
        }
    }

    fn unsupported_aggregate_message(&self) -> String {
        "INSERT statement cannot contain aggregates!".to_owned()
    }
}

/// Table function arguments and COLUMNS expressions (DuckDB's `TableFunctionBinder`):
/// unknown names become strings, lateral columns are rejected.
#[derive(Debug)]
pub struct TableFunctionBinder<'a> {
    base: BinderBase<'a>,
    table_function_name: String,
    clause: String,
    /// Whether `current_date` and similar names are evaluated.
    pub accept_sql_value_functions: bool,
}

impl<'a> TableFunctionBinder<'a> {
    pub fn new(
        binder: &'a mut Binder,
        outer: Option<&'a mut dyn Scope>,
        table_function_name: impl Into<String>,
        clause: impl Into<String>,
    ) -> Self {
        Self {
            base: BinderBase::new(binder, outer),
            table_function_name: table_function_name.into(),
            clause: clause.into(),
            accept_sql_value_functions: true,
        }
    }

    /// DuckDB's `TableFunctionBinder::BindColumnReference`.
    fn bind_column_reference(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        let ParsedExpressionKind::ColumnRef(column) = &expr.kind else {
            return Err(Error::internal("expected a column reference"));
        };
        let names = column.column_names.clone();
        let macro_parameter = self
            .base
            .binder
            .macro_binding
            .as_ref()
            .is_some_and(|binding| binding.has_matching_binding(&expr.name()));
        if names.len() == 1 {
            if let Some(index) = self
                .base
                .state
                .lambda_bindings
                .iter()
                .rposition(|binding| binding.has_matching_binding(&names[0]))
            {
                let binding = &mut self.base.state.lambda_bindings[index];
                return Ok(binding.bind(&names[0], "", depth)?.into());
            }
            if macro_parameter {
                return Err(parameter_not_resolved());
            }
        } else if names[0].contains(DUMMY_NAME) && macro_parameter {
            return Err(parameter_not_resolved());
        }
        let location = expr.query_location;
        let result_name = names.join(".");
        let placeholder = || BindError::binder("error");
        if !self.table_function_name.is_empty()
            && self.bind_correlated_columns(expr, placeholder())?.is_none()
        {
            return Err(Error::binder(format!(
                "Table function \"{}\" does not support lateral join column parameters - cannot use column \"{}\" \
                 in this context.\nThe function only supports literals as parameters.",
                self.table_function_name, result_name
            ))
            .with_position(location));
        }
        if self.accept_sql_value_functions
            && let Some(mut function) = names.last().and_then(|name| self.sql_value_function(name))
        {
            return self.bind_expression(&mut function, depth, root_expression);
        }
        if super::is_bound(expr) || self.bind_correlated_columns(expr, placeholder())?.is_none() {
            let bound = super::take_bound(expr)?;
            self.base.binder.extract_correlated_expressions(&bound);
            return Ok(bound.into());
        }
        if self.table_function_name.is_empty() {
            return Err(Error::binder(format!(
                "Failed to bind \"{result_name}\" - COLUMNS expression can only contain lambda parameters"
            ))
            .with_position(location));
        }
        let _ = is_potential_alias(&names);
        Ok(Expression::constant(Value::varchar(result_name)).into())
    }
}

expression_binder_base!(TableFunctionBinder);

impl ExpressionBinder for TableFunctionBinder<'_> {
    expression_binder_accessors!();

    fn bind_expression(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        match &expr.kind {
            ParsedExpressionKind::ColumnRef(_) => self.bind_column_reference(expr, depth, root_expression),
            ParsedExpressionKind::Subquery(_) => Err(Error::binder(format!(
                "{} cannot contain subqueries",
                self.clause
            ))),
            ParsedExpressionKind::Default => Ok(BindResult::error(format!(
                "{} cannot contain DEFAULT clause",
                self.clause
            ))),
            ParsedExpressionKind::Window(_) => Ok(BindResult::error(format!(
                "{} cannot contain window functions!",
                self.clause
            ))),
            _ => bind_expression_base(self, expr, depth, root_expression),
        }
    }

    fn unsupported_aggregate_message(&self) -> String {
        format!("{} cannot contain aggregates!", self.clause)
    }
}
