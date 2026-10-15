//! Function calls (DuckDB's `bind_function_expression.cpp`).

use rustdb_ast::{ParsedExpression, ParsedExpressionKind};
use rustdb_catalog::EntryKind;
use rustdb_common::{Error, Result};
use rustdb_function::ScalarFunctionSet;

use crate::expression::Expression;
use crate::expression_binder::{
    BindResult, ExpressionBinder, bind_and_qualify_function, is_lambda_function, take_bound,
};
use crate::function_binder::FunctionBinder;

/// DuckDB's `BindExpression(FunctionExpression &)`: resolves the function in the catalog
/// and binds it as a scalar function, aggregate or macro.
pub fn bind_function_expression<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
) -> Result<BindResult> {
    let location = expr.query_location;
    let ParsedExpressionKind::Function(function) = &mut expr.kind else {
        return Err(Error::internal("expected a function call"));
    };
    let lookup = bind_and_qualify_function(&*binder, function, location, true)?
        .ok_or_else(|| Error::internal("function lookup returned nothing"))?;
    let entry = lookup.entry;
    let is_aggregate = matches!(entry.kind, EntryKind::AggregateFunction(_));
    if !is_aggregate
        && (function.distinct || function.filter.is_some() || !function.order_bys.orders.is_empty())
    {
        return Err(Error::invalid_input(format!(
            "Function \"{}\" is a {}. \"DISTINCT\", \"FILTER\", and \"ORDER BY\" are only applicable to aggregate \
             functions.",
            function.function_name,
            entry.catalog_type().display_name()
        )));
    }
    match &entry.kind {
        EntryKind::ScalarFunction(functions) => {
            if is_lambda_function(function) {
                return Err(Error::not_implemented("lambda functions"));
            }
            let functions = functions.clone();
            bind_scalar_function(binder, expr, &functions, depth, location)
        }
        EntryKind::AggregateFunction(functions) => {
            let functions = functions.clone();
            binder.bind_aggregate(expr, &functions, depth)
        }
        _ => Err(Error::not_implemented(format!(
            "binding calls to {}",
            entry.catalog_type().display_name()
        ))),
    }
}

/// DuckDB's `ExpressionBinder::BindFunction`.
fn bind_scalar_function<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    functions: &ScalarFunctionSet,
    depth: u64,
    location: Option<u64>,
) -> Result<BindResult> {
    let ParsedExpressionKind::Function(function) = &mut expr.kind else {
        return Err(Error::internal("expected a function call"));
    };
    let mut error = None;
    for child in &mut function.children {
        binder.bind_child(child, depth, &mut error)?;
    }
    if let Some(error) = error {
        return Ok(BindResult::Unbound(error));
    }
    let mut children: Vec<Expression> = Vec::with_capacity(function.children.len());
    for child in &mut function.children {
        children.push(take_bound(child)?);
    }
    let context = binder.binder_ref().context.clone();
    let result =
        FunctionBinder::new(&context).bind_scalar_function(functions, children, function.is_operator)?;
    let bound = result.map_err(|error| error.with_position(location).error)?;
    if let crate::expression::ExpressionKind::Function(bound_function) = &bound.kind
        && bound_function.function.stability == rustdb_function::Stability::ConsistentWithinQuery
    {
        binder.binder_ref().set_always_require_rebind();
    }
    Ok(bound.into())
}
