//! The binders of the SELECT list, HAVING and QUALIFY (DuckDB's `BaseSelectBinder` and its
//! subclasses `SelectBinder`, `HavingBinder`, `QualifyBinder`): they resolve references to
//! GROUP BY terms, compute aggregates and window functions into the select node, and
//! resolve select-list aliases.

use std::collections::{HashMap, HashSet};

use rustdb_ast::{AggregateHandling, ExpressionType, OrderType, ParsedExpression, ParsedExpressionKind};
use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_function::AggregateFunctionSet;
use rustdb_types::{LogicalType, Value};

use super::base::{BinderBase, expression_binder_accessors, expression_binder_base};
use super::column_alias_binder::ColumnAliasBinder;
use super::qualify::default_qualify_column_name;
use super::simple_binders::AggregateBinder;
use super::{BindError, BindResult, ExpressionBinder, is_potential_alias, peek_bound, take_bound, unsupported};
use crate::binder::expression::{bind_column_ref, bind_expression_base, push_collation};
use crate::binder::query_node::BoundSelectNode;
use crate::binder::{Binder, Scope};
use crate::expression::{
    AggregateType, BoundCaseCheck, BoundCaseExpression, BoundOrderByNode, BoundOrderModifier, Expression,
    ExpressionKind,
};
use crate::function_binder::{FunctionBinder, add_cast_to_type};
use crate::parsed_equality::ParsedExpressionMap;
use crate::ColumnBinding;

/// What the select list makes available to other clauses (DuckDB's `SelectBindState`).
#[derive(Clone, Debug, Default)]
pub struct SelectBindState {
    /// Select-list aliases and their positions.
    pub alias_map: CaseInsensitiveMap<usize>,
    /// Select-list expressions and their positions.
    pub projection_map: ParsedExpressionMap<usize>,
    /// The select list before binding.
    pub original_expressions: Vec<ParsedExpression>,
    referenced_aliases: HashSet<usize>,
    volatile_expressions: HashSet<usize>,
    subquery_expressions: HashSet<usize>,
    expanded_column_indices: Vec<usize>,
}

impl SelectBindState {
    /// A copy of the aliased select-list expression (DuckDB's `BindAlias`).
    pub fn bind_alias(&mut self, index: usize) -> Result<ParsedExpression> {
        let original = self
            .original_expressions
            .get(index)
            .ok_or_else(|| Error::internal("alias index out of range"))?;
        if self.volatile_expressions.contains(&index) {
            return Err(Error::binder(format!(
                "Alias \"{}\" referenced - but the expression has side effects. This is not yet supported.",
                original.alias
            )));
        }
        self.referenced_aliases.insert(index);
        Ok(original.clone())
    }

    /// Marks a select-list entry as volatile (DuckDB's `SetExpressionIsVolatile`).
    pub fn set_expression_is_volatile(&mut self, index: usize) -> Result<()> {
        if self.referenced_aliases.contains(&index) {
            let alias = self.original_expressions.get(index).map(|e| e.alias.clone()).unwrap_or_default();
            return Err(Error::binder(format!(
                "Alias \"{alias}\" referenced - but the expression has side effects. This is not yet supported."
            )));
        }
        self.volatile_expressions.insert(index);
        Ok(())
    }

    pub fn set_expression_has_subquery(&mut self, index: usize) {
        self.subquery_expressions.insert(index);
    }

    pub fn alias_has_subquery(&self, index: usize) -> bool {
        self.subquery_expressions.contains(&index)
    }

    /// Records that a select-list entry expanded into `count` columns.
    pub fn add_expanded_column(&mut self, count: usize) {
        if self.expanded_column_indices.is_empty() {
            self.expanded_column_indices.push(0);
        }
        let last = self.expanded_column_indices.last().copied().unwrap_or_default();
        self.expanded_column_indices.push(last + count);
    }

    pub fn add_regular_column(&mut self) {
        self.add_expanded_column(1);
    }

    /// The output position of select-list entry `index` after expansions.
    pub fn final_index(&self, index: usize) -> usize {
        self.expanded_column_indices.get(index).copied().unwrap_or(index)
    }
}

/// The bound GROUP BY terms other clauses can refer to (DuckDB's `BoundGroupInformation`).
#[derive(Clone, Debug, Default)]
pub struct BoundGroupInformation {
    /// Unbound (qualified) group expressions and their indexes.
    pub map: ParsedExpressionMap<usize>,
    /// Select-list aliases used as groups.
    pub alias_map: CaseInsensitiveMap<usize>,
    /// Groups with a collation: group index → index of the `first()` aggregate.
    pub collated_groups: HashMap<usize, usize>,
}

/// Which clause a [`SelectBinder`] binds.
#[derive(Debug)]
pub enum SelectRole {
    /// The select list.
    Select,
    /// HAVING (DuckDB's `HavingBinder`).
    Having {
        aggregate_handling: AggregateHandling,
        alias_binder: ColumnAliasBinder,
    },
    /// QUALIFY (DuckDB's `QualifyBinder`).
    Qualify { alias_binder: ColumnAliasBinder },
}

/// The binder of the select list, HAVING and QUALIFY of one select node.
#[derive(Debug)]
pub struct SelectBinder<'a> {
    base: BinderBase<'a>,
    node: &'a mut BoundSelectNode,
    info: &'a BoundGroupInformation,
    role: SelectRole,
    inside_window: bool,
    bound_aggregate: bool,
}

/// The HAVING binder (a [`SelectBinder`] with the HAVING role).
pub type HavingBinder<'a> = SelectBinder<'a>;
/// The QUALIFY binder (a [`SelectBinder`] with the QUALIFY role).
pub type QualifyBinder<'a> = SelectBinder<'a>;

impl<'a> SelectBinder<'a> {
    pub fn new(
        binder: &'a mut Binder,
        outer: Option<&'a mut dyn Scope>,
        node: &'a mut BoundSelectNode,
        info: &'a BoundGroupInformation,
        role: SelectRole,
    ) -> Self {
        let mut base = BinderBase::new(binder, outer);
        if !matches!(role, SelectRole::Select) {
            base.state.target_type = Some(LogicalType::BOOLEAN);
        }
        Self {
            base,
            node,
            info,
            role,
            inside_window: false,
            bound_aggregate: false,
        }
    }

    /// The select node being bound.
    pub fn node_mut(&mut self) -> &mut BoundSelectNode {
        self.node
    }

    /// Whether an aggregate was bound (DuckDB's `BoundAggregates`).
    pub const fn bound_aggregates(&self) -> bool {
        self.bound_aggregate
    }

    /// Forgets bound columns and aggregates, for GROUP BY ALL (DuckDB's `ResetBindings`).
    pub fn reset_bindings(&mut self) {
        self.bound_aggregate = false;
        self.base.state.bound_columns.clear();
    }

    /// The group a parsed expression refers to (DuckDB's `TryBindGroup`).
    fn try_bind_group(&self, expr: &ParsedExpression) -> Option<usize> {
        if let ParsedExpressionKind::ColumnRef(column) = &expr.kind
            && let [name] = column.column_names.as_slice()
            && let Some(&index) = self.info.alias_map.get(name)
        {
            return Some(index);
        }
        self.info.map.get(expr).copied()
    }

    /// A reference to group `index` (DuckDB's `BindGroup`).
    fn bind_group(&self, expr: &ParsedExpression, depth: u64, index: usize) -> Result<BindResult> {
        let name = expr.name();
        if let Some(&aggregate_index) = self.info.collated_groups.get(&index) {
            let return_type = self
                .node
                .aggregates
                .get(aggregate_index)
                .map(|aggregate| aggregate.return_type.clone())
                .unwrap_or_default();
            let first = Expression::column_ref(
                &name,
                return_type.clone(),
                ColumnBinding::new(self.node.aggregate_index, aggregate_index as u64),
                depth,
            );
            if self.node.groups.grouping_sets.len() <= 1 {
                return Ok(first.into());
            }
            let group_type = self.node.groups.group_expressions[index].return_type.clone();
            let collated = Expression::column_ref(
                &name,
                group_type,
                ColumnBinding::new(self.node.group_index, index as u64),
                depth,
            );
            let when = Expression::operator(ExpressionType::OperatorIsNull, LogicalType::BOOLEAN, vec![collated]);
            let then = Expression::constant(Value::null(return_type.clone()));
            return Ok(Expression::new(
                ExpressionType::CaseExpr,
                return_type,
                ExpressionKind::Case(Box::new(BoundCaseExpression {
                    case_checks: vec![BoundCaseCheck {
                        when_expr: when,
                        then_expr: then,
                    }],
                    else_expr: first,
                })),
            )
            .into());
        }
        let group_type = self
            .node
            .groups
            .group_expressions
            .get(index)
            .map(|group| group.return_type.clone())
            .ok_or_else(|| Error::internal("group index out of range"))?;
        Ok(Expression::column_ref(name, group_type, ColumnBinding::new(self.node.group_index, index as u64), depth).into())
    }

    /// HAVING's column references (DuckDB's `HavingBinder::BindColumnRef`).
    fn bind_having_column_ref(&mut self, expr: &mut ParsedExpression, depth: u64, root_expression: bool) -> Result<BindResult> {
        let ParsedExpressionKind::ColumnRef(column) = &expr.kind else {
            return Err(Error::internal("expected a column reference"));
        };
        let names = column.column_names.clone();
        let column_name = names.last().cloned().unwrap_or_default();
        if let [name] = names.as_slice() {
            if let Some(index) = self
                .base
                .state
                .lambda_bindings
                .iter()
                .rposition(|binding| binding.has_matching_binding(name))
            {
                return Ok(self.base.state.lambda_bindings[index].bind(name, "", depth)?.into());
            }
            if let Some(mut function) = self.sql_value_function(name) {
                return self.bind_expression(&mut function, depth, false);
            }
        }
        let SelectRole::Having { aggregate_handling, .. } = &self.role else {
            return Err(Error::internal("not a HAVING binder"));
        };
        let aggregate_handling = *aggregate_handling;
        if let Some(result) = self.bind_alias(expr, depth, root_expression)? {
            if depth > 0 {
                return Err(Error::binder(format!(
                    "Having clause cannot reference alias \"{column_name}\" in correlated subquery"
                )));
            }
            return Ok(result);
        }
        if aggregate_handling != AggregateHandling::ForceAggregates {
            return Ok(BindResult::error(format!(
                "column {column_name} must appear in the GROUP BY clause or be used in an aggregate function"
            )));
        }
        if depth > 0 {
            return Err(Error::binder(format!(
                "Having clause cannot reference column \"{column_name}\" in correlated subquery and group by all"
            )));
        }
        let bound = match bind_column_ref(self, expr, depth, root_expression)? {
            BindResult::Bound(bound) => bound,
            unbound @ BindResult::Unbound(_) => return Ok(unbound),
        };
        let return_type = bound.return_type.clone();
        let binding = ColumnBinding::new(self.node.group_index, self.node.groups.group_expressions.len() as u64);
        self.node.groups.group_expressions.push(bound);
        Ok(Expression::column_ref("", return_type, binding, 0).into())
    }

    /// QUALIFY's column references (DuckDB's `QualifyBinder::BindColumnRef`).
    fn bind_qualify_column_ref(&mut self, expr: &mut ParsedExpression, depth: u64, root_expression: bool) -> Result<BindResult> {
        let text = expr.to_string();
        let location = expr.query_location;
        if let BindResult::Bound(bound) = bind_column_ref(self, expr, depth, root_expression)? {
            return Ok(bound.into());
        }
        if let Some(result) = self.bind_alias(expr, depth, root_expression)? {
            return Ok(result);
        }
        Ok(BindResult::Unbound(
            Error::binder(format!(
                "Referenced column {text} not found in FROM clause and can't find in alias map."
            ))
            .with_position(location)
            .into(),
        ))
    }

    /// Binds a select-list alias referenced from HAVING/QUALIFY (DuckDB's `ColumnAliasBinder::BindAlias`).
    fn bind_alias(&mut self, expr: &mut ParsedExpression, depth: u64, root_expression: bool) -> Result<Option<BindResult>> {
        let index = match &mut self.role {
            SelectRole::Having { alias_binder, .. } | SelectRole::Qualify { alias_binder } => {
                alias_binder.begin(&mut self.node.bind_state, expr)?
            }
            SelectRole::Select => None,
        };
        let Some(index) = index else {
            return Ok(None);
        };
        let result = self.bind_expression(expr, depth, root_expression);
        if let SelectRole::Having { alias_binder, .. } | SelectRole::Qualify { alias_binder } = &mut self.role {
            alias_binder.end(index);
        }
        result.map(Some)
    }

    /// DuckDB's `BaseSelectBinder::BindAggregate`.
    fn bind_select_aggregate(
        &mut self,
        expr: &mut ParsedExpression,
        functions: &AggregateFunctionSet,
        depth: u64,
    ) -> Result<BindResult> {
        self.bound_aggregate = true;
        let location = expr.query_location;
        let alias = expr.alias.clone();
        let ParsedExpressionKind::Function(function) = &mut expr.kind else {
            return Err(Error::internal("expected an aggregate call"));
        };
        let context = self.base.binder.context.clone();
        let (inner, outer) = self.base.split();
        let mut aggregate_binder = AggregateBinder::new(inner, outer);
        let mut error: Option<BindError> = None;
        if let Some(filter) = &mut function.filter {
            aggregate_binder.bind_child(filter, 0, &mut error)?;
        }
        let ordered_set_agg = match (function.order_bys.orders.len(), function.function_name.as_str()) {
            (1, "mode") => 1,
            (1, "quantile_cont" | "quantile_disc") => 2,
            _ => 0,
        };
        for child in &mut function.children {
            aggregate_binder.bind_child(child, 0, &mut error)?;
        }
        if ordered_set_agg == 2 {
            return Err(Error::not_implemented("ordered-set aggregates (quantile WITHIN GROUP)"));
        }
        for order in &mut function.order_bys.orders {
            if let ParsedExpressionKind::Constant(constant) = &order.expression.kind
                && !constant.value.logical_type().is_integral()
                && !context.config.order_by_non_integer_literal
            {
                return Err(Error::binder(
                    "ORDER BY non-integer literal has no effect.\n* SET order_by_non_integer_literal=true to allow \
                     this behavior.\n\nPerhaps you misplaced ORDER BY; ORDER BY must appear after all regular \
                     arguments of the aggregate.",
                )
                .with_position(order.expression.query_location));
            }
            aggregate_binder.bind_child(&mut order.expression, 0, &mut error)?;
        }
        if let Some(error) = error {
            if !aggregate_binder.has_bound_columns() {
                // Try again in an enclosing query.
                return Ok(BindResult::Unbound(error));
            }
            // The aggregate belongs here: its unbound parts are correlated columns.
            let mut targets: Vec<&mut ParsedExpression> = function.children.iter_mut().collect();
            if let Some(filter) = &mut function.filter {
                targets.push(filter);
            }
            for order in &mut function.order_bys.orders {
                targets.push(&mut order.expression);
            }
            for target in targets {
                if let Some(error) = aggregate_binder.bind_correlated_columns(target, error.clone())? {
                    return Err(error.error);
                }
                if let Some(bound) = peek_bound(target).cloned() {
                    aggregate_binder.binder().extract_correlated_expressions(&bound);
                }
            }
        } else if depth > 0 && !aggregate_binder.has_bound_columns() {
            return Ok(BindResult::error(
                "Aggregate with only constant parameters has to be bound in the root subquery",
            ));
        }
        drop(aggregate_binder);
        let filter = match &mut function.filter {
            Some(filter) => Some(add_cast_to_type(&context, take_bound(filter)?, &LogicalType::BOOLEAN, false)?),
            None => None,
        };
        let mut children = Vec::with_capacity(function.children.len() + 1);
        if ordered_set_agg == 1 && function.children.is_empty() {
            for order in &function.order_bys.orders {
                if let Some(bound) = peek_bound(&order.expression) {
                    children.push(bound.clone());
                }
            }
        }
        for child in &mut function.children {
            children.push(take_bound(child)?);
        }
        let types = FunctionBinder::argument_types(&children);
        let binder = FunctionBinder::new(&context);
        let overload = binder
            .bind_aggregate_overload(functions, &types)?
            .map_err(|error| error.with_position(location))?;
        let mut order_bys = None;
        if !function.order_bys.orders.is_empty() {
            let mut orders = Vec::with_capacity(function.order_bys.orders.len());
            for order in &mut function.order_bys.orders {
                let expression = take_bound(&mut order.expression)?;
                let ty = expression.return_type.clone();
                let expression = push_collation(expression, &ty)?;
                let sense = context.config.resolve_order(order.order_type);
                let null_order = context.config.resolve_null_order(sense, order.null_order);
                orders.push(BoundOrderByNode::new(sense, null_order, expression));
            }
            order_bys = Some(BoundOrderModifier { orders });
        }
        if function.distinct
            && let Some(order_bys) = &order_bys
            && !order_bys
                .orders
                .iter()
                .all(|order| is_functionally_dependent(&order.expression, &children))
        {
            return Err(Error::binder(
                "In a DISTINCT aggregate, ORDER BY expressions must appear in the argument list",
            ));
        }
        if function.export_state {
            return Err(Error::not_implemented("EXPORT_STATE"));
        }
        let aggr_type = if function.distinct {
            AggregateType::Distinct
        } else {
            AggregateType::NonDistinct
        };
        let aggregate = binder.bind_aggregate_function(overload, children, filter, aggr_type, order_bys)?;
        let index = match self.node.aggregates.iter().position(|existing| existing.equals(&aggregate)) {
            Some(index) => index,
            None => {
                self.node.aggregates.push(aggregate);
                self.node.aggregates.len() - 1
            }
        };
        let aggregate = &self.node.aggregates[index];
        let name = if alias.is_empty() { aggregate.to_string() } else { alias };
        Ok(Expression::column_ref(
            name,
            aggregate.return_type.clone(),
            ColumnBinding::new(self.node.aggregate_index, index as u64),
            depth,
        )
        .into())
    }
}

/// Whether `expression` is determined by `dependencies` (DuckDB's `IsFunctionallyDependent`).
fn is_functionally_dependent(expression: &Expression, dependencies: &[Expression]) -> bool {
    if expression.is_volatile() {
        return false;
    }
    if expression.is_foldable() || dependencies.iter().any(|dependency| expression.equals(dependency)) {
        return true;
    }
    let children = expression.children();
    !children.is_empty() && children.into_iter().all(|child| is_functionally_dependent(child, dependencies))
}

expression_binder_base!(SelectBinder {
    fn does_column_alias_exist(&self, names: &[String]) -> bool {
        match &self.role {
            SelectRole::Select => names.last().is_some_and(|name| self.node.bind_state.alias_map.contains_key(name)),
            SelectRole::Having { .. } | SelectRole::Qualify { .. } => {
                ColumnAliasBinder::does_column_alias_exist(&self.node.bind_state, names)
            }
        }
    }

    fn qualify_column_name(
        &self,
        names: &[String],
        location: Option<u64>,
    ) -> Result<std::result::Result<ParsedExpression, BindError>> {
        let qualified = default_qualify_column_name(self, names, location)?;
        if !matches!(self.role, SelectRole::Having { .. }) {
            return Ok(qualified);
        }
        // HAVING prefers aliases over FROM columns that are not groups.
        let Ok(qualified) = qualified else {
            return Ok(qualified);
        };
        if self.try_bind_group(&qualified).is_some() {
            return Ok(Ok(qualified));
        }
        if ColumnAliasBinder::does_column_alias_exist(&self.node.bind_state, names) {
            return Ok(Err(BindError::binder("alias")));
        }
        Ok(Ok(qualified))
    }
});

impl ExpressionBinder for SelectBinder<'_> {
    expression_binder_accessors!();

    fn bind_expression(&mut self, expr: &mut ParsedExpression, depth: u64, root_expression: bool) -> Result<BindResult> {
        if let Some(index) = self.try_bind_group(expr) {
            return self.bind_group(expr, depth, index);
        }
        match &expr.kind {
            ParsedExpressionKind::ColumnRef(_) => match self.role {
                SelectRole::Select => bind_column_ref(self, expr, depth, root_expression),
                SelectRole::Having { .. } => self.bind_having_column_ref(expr, depth, root_expression),
                SelectRole::Qualify { .. } => self.bind_qualify_column_ref(expr, depth, root_expression),
            },
            ParsedExpressionKind::Default => Ok(BindResult::Unbound(unsupported(
                expr,
                "SELECT clause cannot contain DEFAULT clause",
            ))),
            ParsedExpressionKind::Window(_) => self.bind_window(expr, depth),
            _ => bind_expression_base(self, expr, depth, root_expression),
        }
    }

    fn try_resolve_alias_reference(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        _root_expression: bool,
    ) -> Result<Option<BindResult>> {
        if !matches!(self.role, SelectRole::Select) {
            return Ok(None);
        }
        let ParsedExpressionKind::ColumnRef(column) = &expr.kind else {
            return Ok(None);
        };
        if !is_potential_alias(&column.column_names) {
            return Ok(None);
        }
        let Some(name) = column.column_names.last().cloned() else {
            return Ok(None);
        };
        let Some(&index) = self.node.bind_state.alias_map.get(&name) else {
            return Ok(None);
        };
        if index >= self.node.bound_column_count {
            return Err(Error::binder(format!(
                "Column \"{name}\" referenced that exists in the SELECT clause - but this column cannot be referenced \
                 before it is defined"
            )));
        }
        if self.node.bind_state.alias_has_subquery(index) {
            return Err(Error::binder(format!(
                "Alias \"{name}\" referenced in a SELECT clause - but the expression has a subquery. This is not yet \
                 supported."
            ))
            .with_position(expr.query_location));
        }
        let mut copy = self.node.bind_state.bind_alias(index)?;
        self.bind_expression(&mut copy, depth, false).map(Some)
    }

    fn sql_value_function(&self, column_name: &str) -> Option<ParsedExpression> {
        if self.node.bind_state.alias_map.contains_key(column_name) {
            return None;
        }
        super::sql_value_function_name(column_name).map(|name| super::function_call(name, Vec::new()))
    }

    fn bind_grouping_function(&mut self, expr: &mut ParsedExpression, depth: u64) -> Result<BindResult> {
        let location = expr.query_location;
        let name = expr.name();
        let ParsedExpressionKind::Operator(operator) = &mut expr.kind else {
            return Err(Error::internal("expected GROUPING"));
        };
        if operator.children.is_empty() {
            return Err(Error::internal("GROUPING requires at least one child"));
        }
        if self.node.groups.group_expressions.is_empty() {
            return Ok(BindResult::Unbound(
                Error::binder("GROUPING statement cannot be used without groups").with_position(location).into(),
            ));
        }
        if operator.children.len() >= 64 {
            return Ok(BindResult::Unbound(
                Error::binder("GROUPING statement cannot have more than 64 groups").with_position(location).into(),
            ));
        }
        let mut indexes = Vec::with_capacity(operator.children.len());
        for child in &mut operator.children {
            super::qualify_column_names(self.base.binder, child)?;
            let Some(index) = self.try_bind_group(child) else {
                return Ok(BindResult::Unbound(
                    Error::binder(format!("GROUPING child \"{}\" must be a grouping column", child.name()))
                        .with_position(location)
                        .into(),
                ));
            };
            indexes.push(index as u64);
        }
        let column = self.node.grouping_functions.len() as u64;
        self.node.grouping_functions.push(indexes);
        Ok(Expression::column_ref(name, LogicalType::BIGINT, ColumnBinding::new(self.node.groupings_index, column), depth)
            .into())
    }

    fn bind_aggregate(&mut self, expr: &mut ParsedExpression, functions: &AggregateFunctionSet, depth: u64) -> Result<BindResult> {
        self.bind_select_aggregate(expr, functions, depth)
    }

    fn bind_window(&mut self, expr: &mut ParsedExpression, _depth: u64) -> Result<BindResult> {
        if matches!(self.role, SelectRole::Having { .. }) {
            return Err(unsupported(expr, "HAVING clause cannot contain window functions!").error);
        }
        let _ = (self.inside_window, OrderType::Ascending);
        Err(Error::not_implemented("window functions"))
    }
}
