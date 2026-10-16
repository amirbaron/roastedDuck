//! The binders of the SELECT list, HAVING and QUALIFY (DuckDB's `BaseSelectBinder` and its
//! subclasses `SelectBinder`, `HavingBinder`, `QualifyBinder`): they resolve references to
//! GROUP BY terms, compute aggregates and window functions into the select node, and
//! resolve select-list aliases.

use std::collections::{HashMap, HashSet};

use rustdb_ast::{
    AggregateHandling, ExpressionType, OrderType, ParsedExpression, ParsedExpressionKind,
    expression_type_to_string,
};
use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_function::AggregateFunctionSet;
use rustdb_function::cast::ValueCast;
use rustdb_types::{LogicalType, LogicalTypeId, TypeInfo, Value};

use super::base::{BinderBase, expression_binder_accessors, expression_binder_base};
use super::column_alias_binder::ColumnAliasBinder;
use super::qualify::default_qualify_column_name;
use super::simple_binders::AggregateBinder;
use super::{
    BindError, BindResult, ExpressionBinder, is_potential_alias, peek_bound, set_bound, take_bound,
    unsupported,
};
use crate::ColumnBinding;
use crate::binder::expression::{bind_column_ref, bind_expression_base, push_collation};
use crate::binder::query_node::{BoundSelectNode, BoundUnnestNode};
use crate::binder::{Binder, Scope};
use crate::context::ClientContext;
use crate::expression::{
    AggregateType, BoundCaseCheck, BoundCaseExpression, BoundOrderByNode, BoundOrderModifier,
    BoundUnnestExpression, BoundWindowExpression, Expression, ExpressionKind,
};
use crate::function_binder::{FunctionBinder, add_cast_to_type};
use crate::parsed_equality::ParsedExpressionMap;

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
            let alias = self
                .original_expressions
                .get(index)
                .map(|e| e.alias.clone())
                .unwrap_or_default();
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
    /// How many UNNEST calls enclose the expression being bound.
    unnest_level: u64,
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
            unnest_level: 0,
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
            let when = Expression::operator(
                ExpressionType::OperatorIsNull,
                LogicalType::BOOLEAN,
                vec![collated],
            );
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
        Ok(Expression::column_ref(
            name,
            group_type,
            ColumnBinding::new(self.node.group_index, index as u64),
            depth,
        )
        .into())
    }

    /// HAVING's column references (DuckDB's `HavingBinder::BindColumnRef`).
    fn bind_having_column_ref(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
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
                return Ok(crate::binder::expression::bind_lambda_parameter(
                    &mut self.base.state.lambda_bindings,
                    index as u64,
                    name,
                    depth,
                )?
                .into());
            }
            if let Some(mut function) = self.sql_value_function(name) {
                return self.bind_expression(&mut function, depth, false);
            }
        }
        let SelectRole::Having {
            aggregate_handling, ..
        } = &self.role
        else {
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
        let binding = ColumnBinding::new(
            self.node.group_index,
            self.node.groups.group_expressions.len() as u64,
        );
        self.node.groups.group_expressions.push(bound);
        Ok(Expression::column_ref("", return_type, binding, 0).into())
    }

    /// QUALIFY's column references (DuckDB's `QualifyBinder::BindColumnRef`).
    fn bind_qualify_column_ref(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
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
    fn bind_alias(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<Option<BindResult>> {
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
        if let SelectRole::Having { alias_binder, .. } | SelectRole::Qualify { alias_binder } = &mut self.role
        {
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
        // Ordered-set aggregates (`quantile_cont(0.5) WITHIN GROUP (ORDER BY x)`): the
        // ordering expression becomes the first argument when it is missing.
        let ordered_set_agg = match (function.order_bys.orders.len(), function.function_name.as_str()) {
            (1, "mode") => 1,
            (1, "quantile_cont" | "quantile_disc") => 2,
            _ => 0,
        };
        let negate_fractions = ordered_set_agg == 2
            && context
                .config
                .resolve_order(function.order_bys.orders[0].order_type)
                == OrderType::Descending;
        let child_count = function.children.len();
        for (index, child) in function.children.iter_mut().enumerate() {
            aggregate_binder.bind_child(child, 0, &mut error)?;
            // `PERCENTILE_XXX ... ORDER BY x DESC` negates the fractions.
            if error.is_none() && ordered_set_agg > 0 && index + 1 == child_count {
                negate_percentile_fractions(&context, child, negate_fractions)?;
            }
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
            Some(filter) => Some(add_cast_to_type(
                &context,
                take_bound(filter)?,
                &LogicalType::BOOLEAN,
                false,
            )?),
            None => None,
        };
        let mut children = Vec::with_capacity(function.children.len() + 1);
        if ordered_set_agg > 0 {
            let order_sensitive = function.function_name == "mode";
            if function.children.len() < ordered_set_agg {
                for order in &function.order_bys.orders {
                    if let Some(bound) = peek_bound(&order.expression) {
                        children.push(bound.clone());
                    }
                }
            }
            if !order_sensitive {
                function.order_bys.orders.clear();
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
                let expression = push_collation(&context, expression, &ty)?;
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
        let index = if let Some(index) = self
            .node
            .aggregates
            .iter()
            .position(|existing| existing.equals(&aggregate))
        {
            index
        } else {
            self.node.aggregates.push(aggregate);
            self.node.aggregates.len() - 1
        };
        let aggregate = &self.node.aggregates[index];
        let name = if alias.is_empty() {
            aggregate.to_string()
        } else {
            alias
        };
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
    if expression.is_foldable()
        || dependencies
            .iter()
            .any(|dependency| expression.equals(dependency))
    {
        return true;
    }
    let children = expression.children();
    !children.is_empty()
        && children
            .into_iter()
            .all(|child| is_functionally_dependent(child, dependencies))
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

    fn bind_expression(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
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
                Error::binder("GROUPING statement cannot be used without groups")
                    .with_position(location)
                    .into(),
            ));
        }
        if operator.children.len() >= 64 {
            return Ok(BindResult::Unbound(
                Error::binder("GROUPING statement cannot have more than 64 groups")
                    .with_position(location)
                    .into(),
            ));
        }
        let mut indexes = Vec::with_capacity(operator.children.len());
        for child in &mut operator.children {
            super::qualify_column_names(self.base.binder, child)?;
            let Some(index) = self.try_bind_group(child) else {
                return Ok(BindResult::Unbound(
                    Error::binder(format!(
                        "GROUPING child \"{}\" must be a grouping column",
                        child.name()
                    ))
                    .with_position(location)
                    .into(),
                ));
            };
            indexes.push(index as u64);
        }
        let column = self.node.grouping_functions.len() as u64;
        self.node.grouping_functions.push(indexes);
        Ok(Expression::column_ref(
            name,
            LogicalType::BIGINT,
            ColumnBinding::new(self.node.groupings_index, column),
            depth,
        )
        .into())
    }

    fn bind_aggregate(
        &mut self,
        expr: &mut ParsedExpression,
        functions: &AggregateFunctionSet,
        depth: u64,
    ) -> Result<BindResult> {
        self.bind_select_aggregate(expr, functions, depth)
    }

    fn unnest_bindings(&self) -> Vec<(u64, u64)> {
        self.node
            .unnests
            .values()
            .map(|node| (node.index, node.expressions.len() as u64))
            .collect()
    }

    fn bind_unnest(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        if matches!(self.role, SelectRole::Select) {
            return self.bind_select_unnest(expr, depth, root_expression);
        }
        let message = self.unsupported_unnest_message();
        crate::binder::expression::bind_unsupported_expression(self, expr, depth, &message)
    }

    fn bind_window(&mut self, expr: &mut ParsedExpression, depth: u64) -> Result<BindResult> {
        if matches!(self.role, SelectRole::Having { .. }) {
            return Err(unsupported(expr, "HAVING clause cannot contain window functions!").error);
        }
        self.bind_select_window(expr, depth)
    }
}

/// The result type of a non-aggregate window function (DuckDB's
/// `ResolveWindowExpressionType`).
fn resolve_window_expression_type(
    window_type: ExpressionType,
    child_types: &[LogicalType],
) -> Result<LogicalType> {
    use ExpressionType as E;
    let parameter_count = match window_type {
        E::WindowRank
        | E::WindowRankDense
        | E::WindowRowNumber
        | E::WindowPercentRank
        | E::WindowCumeDist => 0,
        E::WindowNtile
        | E::WindowFirstValue
        | E::WindowLastValue
        | E::WindowLead
        | E::WindowLag
        | E::WindowFill => 1,
        E::WindowNthValue => 2,
        _ => {
            return Err(Error::internal(format!(
                "Unrecognized window expression type {}",
                expression_type_to_string(window_type)
            )));
        }
    };
    if child_types.len() != parameter_count {
        return Err(Error::binder(format!(
            "{} needs {parameter_count} parameter{}, got {}",
            expression_type_to_string(window_type),
            if parameter_count == 1 { "" } else { "s" },
            child_types.len()
        )));
    }
    if child_types.iter().any(|ty| ty.id() == LogicalTypeId::Unknown) {
        return Err(super::parameter_not_resolved());
    }
    Ok(match window_type {
        E::WindowPercentRank | E::WindowCumeDist => LogicalType::DOUBLE,
        E::WindowRowNumber | E::WindowRank | E::WindowRankDense | E::WindowNtile => LogicalType::BIGINT,
        _ => child_types[0].clone(),
    })
}

/// DuckDB's `IsRangeType`.
fn is_range_type(ty: &LogicalType) -> bool {
    use rustdb_types::PhysicalType as P;
    matches!(
        ty.physical_type(),
        P::Int8
            | P::Int16
            | P::Int32
            | P::Int64
            | P::UInt8
            | P::UInt16
            | P::UInt32
            | P::UInt64
            | P::Int128
            | P::UInt128
            | P::Float
            | P::Double
            | P::Interval
    )
}

/// DuckDB's `IsFillType`.
fn is_fill_type(ty: &LogicalType) -> bool {
    use LogicalTypeId as L;
    // DuckDB's `LogicalType::IsTemporal`.
    let temporal = matches!(
        ty.id(),
        L::Date
            | L::Time
            | L::TimeNs
            | L::Timestamp
            | L::TimeTz
            | L::TimestampTz
            | L::TimestampSec
            | L::TimestampMs
            | L::TimestampNs
    );
    ty.is_numeric() || (temporal && ty.id() != L::TimeTz)
}

/// `order <op> bound` for a RANGE frame boundary (DuckDB's `BindRangeExpression`).
fn bind_range_expression(
    context: &ClientContext,
    name: &str,
    bound: Expression,
    order: &Expression,
) -> Result<Expression> {
    let location = bound.query_location;
    if bound.return_type.id() == LogicalTypeId::SqlNull {
        return Err(Error::binder("Window RANGE expressions cannot be NULL").with_position(location));
    }
    let entry = context
        .get_entry(
            "",
            "main",
            rustdb_catalog::CatalogType::ScalarFunction,
            name,
            false,
        )?
        .ok_or_else(|| Error::internal("function lookup returned nothing"))?;
    let rustdb_catalog::EntryKind::ScalarFunction(functions) = &entry.entry.kind else {
        return Err(Error::internal(format!("{name} is not a scalar function")));
    };
    let function = FunctionBinder::new(context)
        .bind_scalar_function(functions, vec![order.clone(), bound], true)?
        .map_err(|error| error.error)?;
    if !is_range_type(&function.return_type) {
        return Err(Error::binder("Invalid type for Window RANGE expression").with_position(location));
    }
    Ok(function)
}

impl SelectBinder<'_> {
    /// DuckDB's `BaseSelectBinder::BindWindow`.
    #[allow(clippy::too_many_lines)]
    fn bind_select_window(&mut self, expr: &mut ParsedExpression, depth: u64) -> Result<BindResult> {
        use rustdb_ast::WindowBoundary as B;

        let location = expr.query_location;
        let window_type = expr.expression_type;
        let name = expr.alias.clone();
        let ParsedExpressionKind::Window(window) = &mut expr.kind else {
            return Err(Error::internal("expected a window function"));
        };
        let context = self.base.binder.context.clone();
        let entry = if window_type == ExpressionType::WindowAggregate {
            context
                .get_entry(
                    &window.catalog,
                    &window.schema,
                    rustdb_catalog::CatalogType::ScalarFunction,
                    &window.function_name,
                    true,
                )
                .map_err(|error| error.with_position(location))?
        } else {
            None
        };
        if let Some(found) = &entry
            && let rustdb_catalog::EntryKind::Macro(macro_entry) = &found.entry.kind
        {
            // A macro used as a windowed aggregate: its body takes the window's place.
            let mut function = rustdb_ast::FunctionExpression {
                function_name: window.function_name.clone(),
                schema: window.schema.clone(),
                children: window.children.clone(),
                filter: window.filter_expr.clone(),
                order_bys: rustdb_ast::OrderModifier { orders: Vec::new() },
                distinct: window.distinct,
                is_operator: false,
                export_state: false,
                catalog: window.catalog.clone(),
            };
            let macros = macro_entry.macros.clone();
            let name = found.entry.name().to_owned();
            let mut macro_expr = expr.clone();
            crate::binder::expression::bind_macro::unfold_macro_expression(
                self.base.binder,
                &mut function,
                &name,
                &macros,
                &mut macro_expr,
                depth,
            )?;
            return self.bind_expression(&mut macro_expr, depth, false);
        }
        if self.inside_window {
            return Err(Error::binder("window function calls cannot be nested").with_position(location));
        }
        if depth > 0 {
            return Err(
                Error::binder("correlated columns in window functions not supported").with_position(location),
            );
        }
        let is_range = matches!(window.start, B::ExprPrecedingRange | B::ExprFollowingRange)
            || matches!(window.end, B::ExprPrecedingRange | B::ExprFollowingRange);
        if is_range && window.orders.len() != 1 {
            return Err(
                Error::binder("RANGE frames must have only one ORDER BY expression").with_position(location),
            );
        }
        if window_type == ExpressionType::WindowFill
            && (window.arg_orders.len() > 1 || (window.arg_orders.is_empty() && window.orders.len() != 1))
        {
            return Err(
                Error::binder("FILL functions must have only one ORDER BY expression")
                    .with_position(location),
            );
        }
        // Bind the children; nested window functions are rejected meanwhile.
        self.inside_window = true;
        let mut error: Option<BindError> = None;
        let bound = (|| -> Result<()> {
            for child in &mut window.children {
                self.bind_child(child, depth, &mut error)?;
            }
            for child in &mut window.partitions {
                self.bind_child(child, depth, &mut error)?;
            }
            for order in &mut window.orders {
                self.bind_child(&mut order.expression, depth, &mut error)?;
                // A TIME ordering of a RANGE frame becomes epoch + time, a timestamp, so
                // that the frame boundaries do not wrap around midnight.
                if is_range
                    && error.is_none()
                    && peek_bound(&order.expression).is_some_and(|bound| {
                        matches!(
                            bound.return_type.id(),
                            LogicalTypeId::Time | LogicalTypeId::TimeTz
                        )
                    })
                {
                    let bound = take_bound(&mut order.expression)?;
                    let epoch = Expression::constant(Value::date(rustdb_types::Date(0)));
                    let shifted = bind_range_expression(&context, "+", bound, &epoch)?;
                    super::set_bound(&mut order.expression, shifted);
                }
            }
            for child in [
                &mut window.filter_expr,
                &mut window.start_expr,
                &mut window.end_expr,
                &mut window.offset_expr,
                &mut window.default_expr,
            ]
            .into_iter()
            .flatten()
            {
                self.bind_child(child, depth, &mut error)?;
            }
            for order in &mut window.arg_orders {
                self.bind_child(&mut order.expression, depth, &mut error)?;
            }
            Ok(())
        })();
        self.inside_window = false;
        bound?;
        if let Some(error) = error {
            return Ok(BindResult::Unbound(error));
        }
        // Collations of the orders and partitions.
        let take = |expr: &mut ParsedExpression| -> Result<Expression> {
            let bound = take_bound(expr)?;
            let ty = bound.return_type.clone();
            push_collation(&context, bound, &ty)
        };
        let mut arg_orders = Vec::with_capacity(window.arg_orders.len());
        for order in &mut window.arg_orders {
            arg_orders.push(take(&mut order.expression)?);
        }
        let mut partitions = Vec::with_capacity(window.partitions.len());
        for partition in &mut window.partitions {
            partitions.push(take(partition)?);
        }
        let mut orders = Vec::with_capacity(window.orders.len());
        for order in &mut window.orders {
            orders.push(take(&mut order.expression)?);
        }
        let mut children = Vec::with_capacity(window.children.len());
        for child in &mut window.children {
            let mut bound = take_bound(child)?;
            let position = children.len();
            if (window_type == ExpressionType::WindowNtile && position == 0)
                || (window_type == ExpressionType::WindowNthValue && position == 1)
            {
                bound = add_cast_to_type(&context, bound, &LogicalType::BIGINT, false)?;
            }
            children.push(bound);
        }
        // The children's own types (`bound->return_type`), not their literal types.
        let types: Vec<LogicalType> = children.iter().map(|child| child.return_type.clone()).collect();
        let (aggregate, bind_info, return_type) = if window_type == ExpressionType::WindowAggregate {
            let functions = match entry.as_ref().map(|entry| &entry.entry.kind) {
                Some(rustdb_catalog::EntryKind::AggregateFunction(functions)) => functions.clone(),
                Some(_) => {
                    return Err(Error::catalog(format!(
                        "{} is not an aggregate function",
                        window.function_name
                    ))
                    .with_position(location));
                }
                None => {
                    let error = context
                        .get_entry(
                            &window.catalog,
                            &window.schema,
                            rustdb_catalog::CatalogType::AggregateFunction,
                            &window.function_name,
                            false,
                        )
                        .err()
                        .unwrap_or_else(|| Error::internal("aggregate lookup"));
                    return Err(crate::context::position_missing_entry(error, location));
                }
            };
            let function_binder = FunctionBinder::new(&context);
            let overload = function_binder
                .bind_aggregate_overload(&functions, &types)?
                .map_err(|error| error.with_position(location))?;
            let bound = function_binder.bind_aggregate_function(
                overload,
                std::mem::take(&mut children),
                None,
                AggregateType::NonDistinct,
                None,
            )?;
            let return_type = bound.return_type.clone();
            let ExpressionKind::Aggregate(aggregate) = bound.kind else {
                return Err(Error::internal("expected a bound aggregate"));
            };
            let aggregate = *aggregate;
            children = aggregate.children;
            (Some(aggregate.function), aggregate.bind_info, return_type)
        } else {
            (None, None, resolve_window_expression_type(window_type, &types)?)
        };
        // RANGE boundaries become `order +/- offset`; for ORDER BY DESC the arithmetic is
        // reversed.
        let mut range_sense = None;
        let mut start_type = LogicalType::BIGINT;
        let mut end_type = LogicalType::BIGINT;
        let mut start_expr = match &mut window.start_expr {
            Some(expr) => Some(take_bound(expr)?),
            None => None,
        };
        let mut end_expr = match &mut window.end_expr {
            Some(expr) => Some(take_bound(expr)?),
            None => None,
        };
        let order_sense = window
            .orders
            .first()
            .map(|order| context.config.resolve_order(order.order_type));
        let range_operator = |preceding: bool, sense: OrderType| {
            if (sense == OrderType::Ascending) == preceding {
                "-"
            } else {
                "+"
            }
        };
        if matches!(window.start, B::ExprPrecedingRange | B::ExprFollowingRange)
            && let (Some(sense), Some(bound)) = (order_sense, start_expr.take())
        {
            range_sense = Some(sense);
            let op = range_operator(window.start == B::ExprPrecedingRange, sense);
            let expression = bind_range_expression(&context, op, bound, &orders[0])?;
            start_type = expression.return_type.clone();
            start_expr = Some(expression);
        }
        if matches!(window.end, B::ExprPrecedingRange | B::ExprFollowingRange)
            && let (Some(sense), Some(bound)) = (order_sense, end_expr.take())
        {
            range_sense = Some(sense);
            let op = range_operator(window.end == B::ExprPrecedingRange, sense);
            let expression = bind_range_expression(&context, op, bound, &orders[0])?;
            end_type = expression.return_type.clone();
            end_expr = Some(expression);
        }
        if range_sense.is_some() {
            // The ORDER BY expression and the boundaries are compared at one type.
            let mut order_type = orders[0].return_type.clone();
            if start_expr.is_some() {
                order_type = crate::function_binder::max_logical_type(&order_type, &start_type)?;
            }
            if end_expr.is_some() {
                order_type = crate::function_binder::max_logical_type(&order_type, &end_type)?;
            }
            let order = std::mem::replace(&mut orders[0], Expression::constant(Value::default()));
            orders[0] = add_cast_to_type(&context, order, &order_type, false)?;
            start_type = order_type.clone();
            end_type = order_type;
        }
        let mut bound_orders = Vec::with_capacity(orders.len());
        for (order, expression) in window.orders.iter().zip(orders) {
            let sense = context.config.resolve_order(order.order_type);
            let null_order = context.config.resolve_null_order(sense, order.null_order);
            bound_orders.push(BoundOrderByNode::new(sense, null_order, expression));
        }
        let mut bound_arg_orders = Vec::with_capacity(arg_orders.len());
        for (order, expression) in window.arg_orders.iter().zip(arg_orders) {
            let sense = context.config.resolve_order(order.order_type);
            let null_order = context.config.resolve_null_order(sense, order.null_order);
            bound_arg_orders.push(BoundOrderByNode::new(sense, null_order, expression));
        }
        if window_type == ExpressionType::WindowFill {
            if !children
                .first()
                .is_some_and(|child| is_fill_type(&child.return_type))
            {
                return Err(Error::binder("FILL argument must support subtraction").with_position(location));
            }
            let order_type = bound_arg_orders
                .first()
                .or(bound_orders.first())
                .map(|order| order.expression.return_type.clone())
                .unwrap_or_default();
            if !is_fill_type(&order_type) {
                return Err(Error::binder("FILL ordering must support subtraction").with_position(location));
            }
        }
        let cast = |expr: Option<Expression>, ty: &LogicalType| -> Result<Option<Expression>> {
            expr.map(|expr| add_cast_to_type(&context, expr, ty, false))
                .transpose()
        };
        let take_optional = |expr: &mut Option<Box<ParsedExpression>>| -> Result<Option<Expression>> {
            expr.as_deref_mut().map(take_bound).transpose()
        };
        let filter_expr = cast(take_optional(&mut window.filter_expr)?, &LogicalType::BOOLEAN)?;
        let start_expr = cast(start_expr, &start_type)?;
        let end_expr = cast(end_expr, &end_type)?;
        let offset_expr = cast(take_optional(&mut window.offset_expr)?, &LogicalType::BIGINT)?;
        let default_expr = cast(take_optional(&mut window.default_expr)?, &return_type)?;
        let result = Expression::new(
            window_type,
            return_type.clone(),
            ExpressionKind::Window(Box::new(BoundWindowExpression {
                aggregate,
                bind_info,
                children,
                partitions,
                orders: bound_orders,
                filter_expr,
                ignore_nulls: window.ignore_nulls,
                distinct: window.distinct,
                start: window.start,
                end: window.end,
                exclude_clause: window.exclude_clause,
                start_expr,
                end_expr,
                offset_expr,
                default_expr,
                arg_orders: bound_arg_orders,
            })),
        );
        let binding = ColumnBinding::new(self.node.window_index, self.node.windows.len() as u64);
        self.node.windows.push(result);
        Ok(Expression::column_ref(name, return_type, binding, depth).into())
    }
}

impl SelectBinder<'_> {
    /// DuckDB's `SelectBinder::BindUnnest`: list unnests become references to the
    /// node's UNNEST operators, one per nesting level.
    #[allow(clippy::too_many_lines)]
    fn bind_select_unnest(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        let location = expr.query_location;
        let alias = expr.alias.clone();
        let positioned =
            |message: String| BindResult::Unbound(BindError::binder(message).with_position(location));
        let ParsedExpressionKind::Function(function) = &mut expr.kind else {
            return Err(Error::internal("expected an UNNEST call"));
        };
        if depth > 0 {
            return Ok(positioned(
                "UNNEST() for correlated expressions is not supported yet".to_owned(),
            ));
        }
        if function.children.is_empty() {
            return Ok(positioned("UNNEST() requires at lease one argument".to_owned()));
        }
        if self.inside_window {
            return Ok(positioned(self.unsupported_unnest_message()));
        }
        if function.distinct || function.filter.is_some() || !function.order_bys.orders.is_empty() {
            return Err(Error::invalid_input(
                "\"DISTINCT\", \"FILTER\", and \"ORDER BY\" are not applicable to \"UNNEST\"",
            ));
        }
        let mut max_depth = 1_u64;
        let mut keep_parent_names = false;
        let mut error: Option<BindError> = None;
        if function.children.len() != 1 {
            let mut supported_argument = false;
            let context = self.base.binder.context.clone();
            for i in 1..function.children.len() {
                let child = &mut function.children[i];
                if super::parsed_has_parameter(child) {
                    return Err(Error::new(
                        rustdb_common::ErrorKind::ParameterNotAllowed,
                        "Parameter not allowed in unnest parameter",
                    ));
                }
                if !super::parsed_is_scalar(child) {
                    break;
                }
                let name = child.alias.to_lowercase();
                self.bind_child(child, depth, &mut error)?;
                if let Some(error) = error.take() {
                    return Ok(BindResult::Unbound(error));
                }
                let bound = peek_bound(child).ok_or_else(|| Error::internal("unnest argument not bound"))?;
                let value = context.evaluator.evaluate_scalar(bound)?;
                match name.as_str() {
                    "recursive" => {
                        if value_get_bool(&value)? {
                            max_depth = u64::MAX;
                        }
                    }
                    "max_depth" => {
                        max_depth = value
                            .as_i128()
                            .and_then(|depth| u64::try_from(depth).ok())
                            .ok_or_else(|| Error::conversion("max_depth must be an unsigned integer"))?;
                        if max_depth == 0 {
                            return Err(Error::binder("UNNEST cannot have a max depth of 0"));
                        }
                    }
                    "keep_parent_names" => keep_parent_names = value_get_bool(&value)?,
                    "" => break,
                    other => {
                        return Err(Error::binder(format!(
                            "Unsupported parameter \"{other}\" for unnest"
                        )));
                    }
                }
                supported_argument = true;
            }
            if !supported_argument {
                return Ok(positioned(
                    "UNNEST - unsupported extra argument, unnest only supports recursive := [true/false], max_depth := \
                     # or keep_parent_names := [true/false]"
                        .to_owned(),
                ));
            }
        }
        self.unnest_level += 1;
        let first = &mut function.children[0];
        self.bind_child(first, depth, &mut error)?;
        if let Some(bind_error) = error.take() {
            if let Some(correlated_error) = self.bind_correlated_columns(first, bind_error)? {
                self.unnest_level -= 1;
                return Ok(BindResult::Unbound(correlated_error));
            }
            if let Some(bound) = peek_bound(first).cloned() {
                self.base.binder.extract_correlated_expressions(&bound);
            }
        }
        let context = self.base.binder.context.clone();
        let mut child = take_bound(first)?;
        // Arrays unnest like lists (DuckDB's `AddArrayCastToList`).
        if child.return_type.id() == LogicalTypeId::Array
            && let Some((element, _)) = child.return_type.array_child()
        {
            let list = LogicalType::list(element.clone());
            child = add_cast_to_type(&context, child, &list, false)?;
        }
        self.unnest_level -= 1;
        if self.unnest_level > 0 {
            return Err(Error::binder(
                "Nested UNNEST calls are not supported - use UNNEST(x, recursive := true) to unnest multiple levels",
            )
            .with_position(location));
        }
        let child_type = child.return_type.clone();
        match child_type.id() {
            LogicalTypeId::Unknown => return Err(super::parameter_not_resolved()),
            LogicalTypeId::List | LogicalTypeId::Struct | LogicalTypeId::SqlNull => {}
            _ => {
                return Ok(positioned(format!(
                    "UNNEST() can only be applied to lists, structs and NULL, not {child_type}"
                )));
            }
        }
        let mut list_unnests = 0_u64;
        let mut struct_unnests = 0_u64;
        if child_type.id() == LogicalTypeId::SqlNull {
            list_unnests = 1;
        } else {
            let mut ty = child_type.clone();
            while ty.id() == LogicalTypeId::List {
                ty = ty.child_type().cloned().unwrap_or_default();
                list_unnests += 1;
                if list_unnests >= max_depth {
                    break;
                }
            }
            if ty.id() == LogicalTypeId::Struct {
                struct_unnests = max_depth - list_unnests;
            }
        }
        if struct_unnests > 0 && !root_expression {
            return Ok(positioned(
                "UNNEST() on a struct column can only be applied as the root element of a SELECT expression"
                    .to_owned(),
            ));
        }
        let mut unnest_expr = child;
        let mut return_type = child_type;
        for current_depth in 0..list_unnests {
            if return_type.id() == LogicalTypeId::List {
                return_type = return_type.child_type().cloned().unwrap_or_default();
            }
            let result = Expression::new(
                ExpressionType::BoundUnnest,
                return_type.clone(),
                ExpressionKind::Unnest(Box::new(BoundUnnestExpression { child: unnest_expr })),
            );
            let name = if alias.is_empty() {
                result.to_string()
            } else {
                alias.clone()
            };
            let level = self.unnest_level + list_unnests - current_depth - 1;
            let (table_index, column_index) = if let Some(node) = self.node.unnests.get_mut(&level) {
                node.expressions.push(result);
                (node.index, node.expressions.len() as u64 - 1)
            } else {
                let index = self.base.binder.generate_table_index();
                self.node.unnests.insert(
                    level,
                    BoundUnnestNode {
                        index,
                        expressions: vec![result],
                    },
                );
                (index, 0)
            };
            unnest_expr = Expression::column_ref(
                name,
                return_type.clone(),
                ColumnBinding::new(table_index, column_index),
                depth,
            );
        }
        if struct_unnests > 0 {
            // Each struct is replaced by its fields, level by level (DuckDB's
            // CreateBoundStructExtract / CreateBoundStructExtractIndex).
            let mut struct_expressions = vec![unnest_expr];
            for _ in 0..struct_unnests {
                let mut expanded = Vec::new();
                let mut has_structs = false;
                for expression in struct_expressions {
                    let Some(fields) = (expression.return_type.id() == LogicalTypeId::Struct)
                        .then(|| expression.return_type.struct_children().map(<[_]>::to_vec))
                        .flatten()
                    else {
                        expanded.push(expression);
                        continue;
                    };
                    has_structs = true;
                    if fields.iter().all(|(name, _)| name.is_empty()) {
                        for index in 0..fields.len() {
                            let key = Value::bigint(index as i64 + 1);
                            let mut field = self.bind_struct_extract(expression.clone(), key, depth)?;
                            field.alias = format!("element{}", index + 1);
                            expanded.push(field);
                        }
                    } else {
                        for (name, _) in &fields {
                            let mut key_path = Vec::new();
                            if keep_parent_names
                                && expression.expression_type == ExpressionType::BoundFunction
                            {
                                key_path.push(expression.alias.clone());
                            }
                            key_path.push(name.clone());
                            let mut field =
                                self.bind_struct_extract(expression.clone(), Value::varchar(name), depth)?;
                            field.alias = if keep_parent_names {
                                let alias = key_path.join(".");
                                alias.strip_prefix('.').map_or(alias.clone(), str::to_owned)
                            } else {
                                key_path[0].clone()
                            };
                            expanded.push(field);
                        }
                    }
                }
                struct_expressions = expanded;
                if !has_structs {
                    break;
                }
            }
            unnest_expr = Expression::new(
                ExpressionType::BoundExpanded,
                LogicalType::INTEGER,
                ExpressionKind::Expanded(crate::expression::BoundExpandedExpression {
                    expanded_expressions: struct_expressions,
                }),
            );
        }
        Ok(unnest_expr.into())
    }

    /// `struct_extract(child, key)` bound as a function call.
    fn bind_struct_extract(&mut self, child: Expression, key: Value, depth: u64) -> Result<Expression> {
        let mut argument = ParsedExpression::constant(Value::null(LogicalType::SQLNULL));
        crate::expression_binder::set_bound(&mut argument, child);
        let mut call = crate::expression_binder::function_call(
            "struct_extract",
            vec![argument, ParsedExpression::constant(key)],
        );
        match self.bind_expression(&mut call, depth, false)? {
            BindResult::Bound(expression) => Ok(expression),
            BindResult::Unbound(error) => Err(error.error),
        }
    }
}

/// `NegatePercentileValue`: the fraction for a descending order (DECIMALs stay DECIMAL,
/// everything else becomes DOUBLE).
fn negate_percentile_value(value: &Value, descending: bool) -> Result<Value> {
    if value.is_null() {
        return Ok(value.clone());
    }
    let fraction = value
        .default_cast_as(&LogicalType::DOUBLE)?
        .as_f64()
        .ok_or_else(|| Error::internal("percentile is not numeric"))?;
    if !(0.0..=1.0).contains(&fraction) {
        return Err(Error::binder(
            "PERCENTILEs can only take parameters in the range [0, 1]",
        ));
    }
    if !descending {
        return Ok(value.clone());
    }
    if let Some(TypeInfo::Decimal(info)) = value.logical_type().info() {
        let unscaled = value
            .as_i128()
            .ok_or_else(|| Error::internal("DECIMAL percentile without a value"))?;
        return Ok(Value::decimal(-unscaled, info.width, info.scale));
    }
    Ok(Value::double(-fraction))
}

/// `NegatePercentileFractions`: folds the constant fractions of an ordered-set
/// aggregate, negated for a descending order.
fn negate_percentile_fractions(
    context: &ClientContext,
    fractions: &mut ParsedExpression,
    descending: bool,
) -> Result<()> {
    let Some(bound) = peek_bound(fractions) else {
        return Ok(());
    };
    if !bound.is_foldable() {
        return Ok(());
    }
    let value = context.evaluator.evaluate_scalar(bound)?;
    let folded = if value.logical_type().id() == LogicalTypeId::List {
        let elements = value
            .children()
            .unwrap_or_default()
            .iter()
            .map(|element| negate_percentile_value(element, descending))
            .collect::<Result<Vec<_>>>()?;
        if elements.is_empty() {
            return Err(Error::binder("Empty list in percentile not allowed"));
        }
        let child_type = elements[0].logical_type().clone();
        Value::list(child_type, elements)
    } else {
        negate_percentile_value(&value, descending)?
    };
    set_bound(fractions, Expression::constant(folded));
    Ok(())
}

/// `Value::GetValue<bool>`: a non-boolean value is read as a TINYINT.
fn value_get_bool(value: &Value) -> Result<bool> {
    if value.logical_type().id() == LogicalTypeId::Boolean {
        return Ok(value.as_bool() == Some(true));
    }
    let number = value.try_cast(&LogicalType::TINYINT).map_err(|error| {
        if error.kind() == rustdb_common::ErrorKind::Conversion {
            Error::invalid_input(error.raw_message().to_owned())
        } else {
            error
        }
    })?;
    Ok(number.as_i128().is_some_and(|number| number != 0))
}
