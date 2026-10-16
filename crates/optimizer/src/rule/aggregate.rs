//! Aggregate modifiers that do not matter: `ORDER BY` of order-insensitive aggregates or
//! on grouped columns (DuckDB's `OrderedAggregateOptimizer`) and `DISTINCT` of
//! duplicate-insensitive aggregates (DuckDB's `DistinctAggregateOptimizer` and
//! `DistinctWindowedOptimizer`).

use rustdb_ast::{OrderByNullType, OrderType};
use rustdb_catalog::CatalogType;
use rustdb_catalog::EntryKind;
use rustdb_common::{Error, Result};
use rustdb_function::aggregate::{DistinctDependence, OrderDependence};
use rustdb_planner::expression::{AggregateType, BoundOrderByNode};
use rustdb_planner::function_binder::FunctionBinder;
use rustdb_planner::operator::GroupingSet;
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_types::Value;

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::optimizer::bind_scalar_function;

/// Drops the sort keys that cannot change the order within a group: duplicates, and
/// groups present in every grouping set (DuckDB's `BoundOrderModifier::Simplify`).
/// Returns whether no key is left.
pub fn simplify_orders(
    orders: &mut Vec<BoundOrderByNode>,
    groups: &[Expression],
    grouping_sets: Option<&[GroupingSet]>,
) -> bool {
    let mut seen: Vec<Expression> = Vec::new();
    let mut kept = Vec::with_capacity(orders.len());
    for order in std::mem::take(orders) {
        if seen.iter().any(|expression| expression.equals(&order.expression)) {
            continue;
        }
        let group = groups.iter().position(|group| group.equals(&order.expression));
        let mut add_to_new_order = group.is_none();
        if let (Some(group), Some(grouping_sets)) = (group, grouping_sets) {
            add_to_new_order = grouping_sets.iter().any(|set| !set.contains(&(group as u64)));
        }
        seen.push(order.expression.clone());
        if add_to_new_order {
            kept.push(order);
        }
    }
    *orders = kept;
    orders.is_empty()
}

/// The `create_sort_key` modifier text of an ORDER BY term (DuckDB's
/// `BoundOrderByNode::GetOrderModifier`).
fn order_modifier(order: &BoundOrderByNode) -> String {
    let direction = if order.order_type == OrderType::Descending {
        "DESC"
    } else {
        "ASC"
    };
    let nulls = if order.null_order == OrderByNullType::NullsFirst {
        "NULLS FIRST"
    } else {
        "NULLS LAST"
    };
    format!("{direction} {nulls}")
}

pub fn ordered_aggregate_optimizer(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    let ExpressionKind::Aggregate(aggregate) = &mut expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    let Some(order_bys) = &mut aggregate.order_bys else {
        return Ok(RuleOutcome::Unchanged);
    };
    if aggregate.function.order_dependence == OrderDependence::NotOrderDependent {
        aggregate.order_bys = None;
        return Ok(RuleOutcome::Changed);
    }
    let (groups, grouping_sets) = context
        .aggregate
        .as_ref()
        .map_or((&[][..], None), |(groups, sets)| {
            (groups.as_slice(), Some(sets.as_slice()))
        });
    if simplify_orders(&mut order_bys.orders, groups, grouping_sets) {
        aggregate.order_bys = None;
        return Ok(RuleOutcome::Changed);
    }
    let arg_name = match &*aggregate.function.name {
        "last" => "arg_max_null",
        "first" | "arbitrary" => "arg_min_null",
        "any_value" => "arg_min",
        _ => return Ok(RuleOutcome::Unchanged),
    };
    let mut sort_children = Vec::new();
    for order in std::mem::take(&mut order_bys.orders) {
        let modifier = order_modifier(&order);
        sort_children.push(order.expression);
        sort_children.push(Expression::constant(Value::varchar(modifier)));
    }
    aggregate.order_bys = None;
    let sort_key = bind_scalar_function(context.client, "create_sort_key", sort_children, false)?;
    let mut children = std::mem::take(&mut aggregate.children);
    children.push(sort_key);
    let entry = context
        .client
        .get_entry("system", "main", CatalogType::AggregateFunction, arg_name, false)?
        .ok_or_else(|| Error::internal(format!("aggregate function {arg_name} not found")))?;
    let EntryKind::AggregateFunction(functions) = &entry.entry.kind else {
        return Err(Error::internal(format!(
            "{arg_name} is not an aggregate function"
        )));
    };
    let binder = FunctionBinder::new(context.client);
    let types = FunctionBinder::argument_types(&children);
    let function = binder.bind_aggregate_overload(functions, &types)??;
    let aggr_type = if aggregate.is_distinct() {
        AggregateType::Distinct
    } else {
        AggregateType::NonDistinct
    };
    let filter = aggregate.filter.take();
    Ok(RuleOutcome::Replaced(binder.bind_aggregate_function(
        function, children, filter, aggr_type, None,
    )?))
}

pub fn distinct_aggregate_optimizer(
    _: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    let ExpressionKind::Aggregate(aggregate) = &mut expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    if aggregate.is_distinct()
        && aggregate.function.distinct_dependence == DistinctDependence::NotDistinctDependent
    {
        aggregate.aggr_type = AggregateType::NonDistinct;
        return Ok(RuleOutcome::Changed);
    }
    Ok(RuleOutcome::Unchanged)
}

pub fn distinct_windowed_optimizer(
    _: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    let ExpressionKind::Window(window) = &mut expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    if window.distinct
        && window.aggregate.as_ref().is_some_and(|aggregate| {
            aggregate.distinct_dependence == DistinctDependence::NotDistinctDependent
        })
    {
        window.distinct = false;
        return Ok(RuleOutcome::Changed);
    }
    Ok(RuleOutcome::Unchanged)
}
