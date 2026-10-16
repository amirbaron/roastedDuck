//! Function calls (DuckDB's `bind_function_expression.cpp`).

use rustdb_ast::{
    ExpressionClass, ExpressionType, ParsedExpression, ParsedExpressionKind, lambda_parameters,
};
use rustdb_catalog::EntryKind;
use rustdb_common::{Error, Result};
use rustdb_function::{BindLambda, LambdaInput, ScalarFunctionSet};
use rustdb_types::{LogicalType, LogicalTypeId};

use crate::binder::Binding;
use crate::expression::{BoundLambdaExpression, Expression, ExpressionKind};
use crate::expression_binder::{
    BindResult, ExpressionBinder, bind_and_qualify_function, is_lambda_function, peek_bound, take_bound,
};
use crate::function_binder::{FunctionBinder, add_cast_to_type};

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
                let functions = functions.clone();
                return try_bind_lambda_or_json(binder, expr, &functions, depth);
            }
            let functions = functions.clone();
            bind_scalar_function(binder, expr, &functions, depth, location)
        }
        EntryKind::AggregateFunction(_) if binder.state_ref().inside_try => Err(Error::binder(
            "aggregates are not allowed inside the TRY expression",
        )),
        EntryKind::AggregateFunction(functions) => {
            let functions = functions.clone();
            binder.bind_aggregate(expr, &functions, depth)
        }
        EntryKind::Macro(macro_entry) => {
            let macros = macro_entry.macros.clone();
            super::bind_macro::bind_macro(binder, expr, entry.name(), &macros, depth)
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

/// DuckDB's `TryBindLambdaOrJson`: an arrow (`->`) that does not bind as a lambda may
/// be the JSON arrow operator; when both fail the error mentions both attempts.
fn try_bind_lambda_or_json<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    functions: &ScalarFunctionSet,
    depth: u64,
) -> Result<BindResult> {
    use rustdb_ast::LambdaSyntaxType;
    let syntax = match &expr.kind {
        ParsedExpressionKind::Function(function) => {
            function.children.iter().find_map(|child| match &child.kind {
                ParsedExpressionKind::Lambda(lambda) => Some(lambda.syntax_type),
                _ => None,
            })
        }
        _ => None,
    };
    let keyword_syntax = syntax == Some(LambdaSyntaxType::LambdaKeyword);
    // The `lambda_syntax` setting may reject the deprecated single arrow.
    let context = binder.binder_ref().context.clone();
    let setting = match context.function_client.settings() {
        Some(settings) if !keyword_syntax => settings
            .current_setting("lambda_syntax")?
            .as_str()
            .map(str::to_uppercase)
            .unwrap_or_default(),
        _ => String::new(),
    };
    let invalid_syntax = setting == "DISABLE_SINGLE_ARROW" && syntax == Some(LambdaSyntaxType::SingleArrow);
    let result = match bind_lambda_function(binder, expr, functions, depth) {
        Err(error) if invalid_syntax && error.kind() == rustdb_common::ErrorKind::ParameterNotResolved => {
            return Err(Error::binder(DEPRECATED_LAMBDA_ARROW));
        }
        result => result?,
    };
    let BindResult::Unbound(lambda_error) = result else {
        if invalid_syntax {
            return Ok(BindResult::error(DEPRECATED_LAMBDA_ARROW));
        }
        return Ok(result);
    };
    if keyword_syntax
        || lambda_error
            .error
            .raw_message()
            .contains("Deprecated lambda arrow (->) detected.")
    {
        return Ok(BindResult::Unbound(lambda_error));
    }
    // Without the JSON extension the arrow operator only binds its operands; the first
    // operand error is reported next to the lambda error.
    let ParsedExpressionKind::Function(function) = &mut expr.kind else {
        return Ok(BindResult::Unbound(lambda_error));
    };
    let mut json_error = None;
    for child in &mut function.children {
        if let ParsedExpressionKind::Lambda(lambda) = &mut child.kind {
            binder.bind_child(&mut lambda.lhs, depth, &mut json_error)?;
            if json_error.is_none() {
                binder.bind_child(&mut lambda.expr, depth, &mut json_error)?;
            }
            if json_error.is_some() {
                break;
            }
        }
    }
    Ok(match json_error {
        Some(json_error) => BindResult::error(format!(
            "failed to bind function, either: {}\n or: {}",
            lambda_error.error.raw_message(),
            json_error.error.raw_message()
        )),
        None => BindResult::Unbound(lambda_error),
    })
}

/// The error for the single arrow under `lambda_syntax = 'DISABLE_SINGLE_ARROW'`.
const DEPRECATED_LAMBDA_ARROW: &str = "Deprecated lambda arrow (->) detected. Please transition to the new lambda \
     syntax, i.e.., lambda x, i: x + i, before DuckDB's next release.\nUse SET \
     lambda_syntax='ENABLE_SINGLE_ARROW' to revert to the deprecated behavior.\nFor more information, see \
     https://duckdb.org/docs/stable/sql/functions/lambda.html.";

/// DuckDB's `LambdaExpression::InvalidParametersErrorMessage`.
const INVALID_LAMBDA_PARAMETERS: &str =
    "Invalid lambda parameters! Parameters must be unqualified comma-separated names like x or (x, y).";

/// DuckDB's `ExpressionBinder::BindLambdaFunction`: binds the list and other arguments,
/// then the lambda body with its parameters in scope; the body is attached to the bound
/// function and the columns it captures become extra arguments.
fn bind_lambda_function<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    functions: &ScalarFunctionSet,
    depth: u64,
) -> Result<BindResult> {
    const LAMBDA_INDEX: usize = 1;
    let Some(bind_lambda) = functions
        .functions
        .first()
        .and_then(|function| function.bind_lambda)
    else {
        return Ok(BindResult::error(
            "This scalar function does not support lambdas!",
        ));
    };
    let location = expr.query_location;
    let call_text = expr.to_string();
    let ParsedExpressionKind::Function(function) = &mut expr.kind else {
        return Err(Error::internal("expected a function call"));
    };
    let is_operator = function.is_operator;
    let no_match = || {
        BindResult::error(format!(
            "No function matches the given name and argument types: '{call_text}'. You might need to add explicit type \
             casts."
        ))
    };
    if function
        .children
        .get(LAMBDA_INDEX)
        .map(ParsedExpression::expression_class)
        != Some(ExpressionClass::Lambda)
    {
        return Ok(no_match());
    }
    let mut child_types = Vec::with_capacity(function.children.len());
    let mut error = None;
    for (index, child) in function.children.iter_mut().enumerate() {
        if index == LAMBDA_INDEX {
            child_types.push(LogicalType::simple(LogicalTypeId::Lambda));
            continue;
        }
        if child.expression_class() == ExpressionClass::Lambda {
            return Ok(no_match());
        }
        binder.bind_child(child, depth, &mut error)?;
        if let Some(error) = error {
            return Ok(BindResult::Unbound(error));
        }
        let bound = peek_bound(child).ok_or_else(|| Error::internal("lambda function argument not bound"))?;
        child_types.push(bound.return_type.clone());
    }
    if !matches!(
        child_types[0].id(),
        LogicalTypeId::List | LogicalTypeId::Array | LogicalTypeId::SqlNull | LogicalTypeId::Unknown
    ) {
        return Ok(BindResult::error(
            "Invalid LIST argument during lambda function binding!",
        ));
    }

    // Bind the lambda body with a dummy binding for its parameters.
    let ParsedExpressionKind::Lambda(lambda) = &mut function.children[LAMBDA_INDEX].kind else {
        return Err(Error::internal("expected a lambda"));
    };
    let parameters = lambda_parameters(lambda).ok_or_else(|| Error::binder(INVALID_LAMBDA_PARAMETERS))?;
    let mut names = Vec::with_capacity(parameters.len());
    let mut aliases = Vec::with_capacity(parameters.len());
    for parameter in parameters {
        let ParsedExpressionKind::ColumnRef(column) = &parameter.kind else {
            return Err(Error::binder(INVALID_LAMBDA_PARAMETERS));
        };
        if column.column_names.len() != 1 {
            return Err(Error::binder(INVALID_LAMBDA_PARAMETERS));
        }
        names.push(column.column_names[0].clone());
        aliases.push(parameter.to_string());
    }
    let types = (0..names.len())
        .map(|index| bind_lambda(&child_types, index))
        .collect::<Result<Vec<_>>>()?;
    let mut table_alias = aliases.join(", ");
    if aliases.len() > 1 {
        table_alias = format!("({table_alias})");
    }
    let parameter_count = names.len() as u64;
    binder
        .state()
        .lambda_bindings
        .push(Binding::dummy(types, names, &table_alias)?);
    let mut body_error = None;
    let bound_body = binder.bind_child(&mut lambda.expr, depth, &mut body_error);
    binder.state().lambda_bindings.pop();
    bound_body?;
    if let Some(error) = body_error {
        return Err(error.error);
    }
    let body = take_bound(&mut lambda.expr)?;

    // Collect the arguments, with the lambda (and its captures) in second position.
    let mut bound_lambda = BoundLambdaExpression {
        lambda_expr: body,
        captures: Vec::new(),
        parameter_count,
    };
    let outer_bindings = binder.state_ref().lambda_bindings.clone();
    let unnests = binder.unnest_bindings();
    let mut lambda_expr = std::mem::replace(
        &mut bound_lambda.lambda_expr,
        Expression::constant(rustdb_types::Value::null(LogicalType::SQLNULL)),
    );
    capture_lambda_columns(
        &mut lambda_expr,
        &mut bound_lambda,
        &LambdaScope {
            outer: &outer_bindings,
            unnests: &unnests,
        },
        bind_lambda,
        &child_types,
    )?;
    bound_lambda.lambda_expr = lambda_expr;
    let lambda_input = LambdaInput {
        parameter_count: parameter_count as usize,
        return_type: bound_lambda.lambda_expr.return_type.clone(),
    };
    let mut children = Vec::with_capacity(function.children.len());
    let mut lambda_node = Some(Expression::new(
        ExpressionType::Lambda,
        LogicalType::simple(LogicalTypeId::Lambda),
        ExpressionKind::Lambda(Box::new(bound_lambda)),
    ));
    for (index, child) in function.children.iter_mut().enumerate() {
        if index == LAMBDA_INDEX {
            children.push(
                lambda_node
                    .take()
                    .ok_or_else(|| Error::internal("lambda bound twice"))?,
            );
        } else {
            children.push(take_bound(child)?);
        }
    }

    let context = binder.binder_ref().context.clone();
    let result = FunctionBinder::new(&context)
        .with_lambda(lambda_input)
        .bind_scalar_function(functions, children, is_operator)?;
    let mut bound = result.map_err(|error| error.with_position(location).error)?;
    let ExpressionKind::Function(bound_function) = &mut bound.kind else {
        return Ok(bound.into());
    };
    if bound_function.children.len() <= LAMBDA_INDEX {
        return Err(Error::internal("lambda function lost its lambda argument"));
    }
    let lambda_child = bound_function.children.remove(LAMBDA_INDEX);
    let ExpressionKind::Lambda(bound_lambda) = lambda_child.kind else {
        return Err(Error::internal("lambda argument was rewritten"));
    };
    let BoundLambdaExpression {
        lambda_expr,
        captures,
        ..
    } = *bound_lambda;
    let lambda_expr = match &bound_function.function.lambda_cast {
        Some(target) => add_cast_to_type(&context, lambda_expr, target, false)?,
        None => lambda_expr,
    };
    // The parameters of enclosing lambdas, innermost first, then the captured columns.
    let mut offset = 0;
    for binding in outer_bindings.iter().rev() {
        for column in (0..binding.names.len()).rev() {
            bound_function.children.push(Expression::reference(
                binding.names[column].clone(),
                binding.types[column].clone(),
                offset,
            ));
            offset += 1;
        }
    }
    bound_function.children.extend(captures);
    bound_function.lambda = Some(Box::new(lambda_expr));
    Ok(bound.into())
}

/// The input chunk position of a parameter of an enclosing lambda (DuckDB's
/// `GetLambdaParamIndex`).
fn outer_parameter_index(outer: &[Binding], parameter_count: u64, lambda_index: usize, column: u64) -> u64 {
    let later: usize = outer[lambda_index + 1..]
        .iter()
        .map(|binding| binding.names.len())
        .sum();
    later as u64 + (outer[lambda_index].names.len() as u64 - column - 1) + parameter_count
}

/// What a lambda body sees around it: the enclosing lambdas' parameters and the UNNEST
/// nodes it may not reference.
struct LambdaScope<'a> {
    outer: &'a [Binding],
    unnests: &'a [(u64, u64)],
}

/// DuckDB's `CaptureLambdaColumns`: lambda parameters and captured columns of the body
/// become references into the lambda's input chunk.
fn capture_lambda_columns(
    expr: &mut Expression,
    lambda: &mut BoundLambdaExpression,
    scope: &LambdaScope<'_>,
    bind_lambda: BindLambda,
    child_types: &[LogicalType],
) -> Result<()> {
    let outer = scope.outer;
    if let ExpressionKind::ColumnRef(column) = &expr.kind
        && scope
            .unnests
            .iter()
            .any(|&(table, count)| column.binding.table_index == table && column.binding.column_index < count)
    {
        return Err(Error::binder("UNNEST in lambda expressions is not supported"));
    }
    match &expr.kind {
        ExpressionKind::Subquery(_) => Err(Error::binder(
            "subqueries in lambda expressions are not supported",
        )),
        ExpressionKind::Constant(_) => Ok(()),
        ExpressionKind::LambdaRef(reference) => {
            let lambda_index = reference.lambda_index as usize;
            let column = reference.binding.column_index;
            let alias = expr.alias.clone();
            *expr = if !outer.is_empty() && lambda_index != outer.len() {
                let binding = outer
                    .get(lambda_index)
                    .ok_or_else(|| Error::internal("Failed to bind lambda parameter internally"))?;
                let ty = binding
                    .types
                    .get(column as usize)
                    .cloned()
                    .ok_or_else(|| Error::internal("Failed to bind lambda parameter internally"))?;
                let name = binding.names[column as usize].clone();
                Expression::reference(
                    name,
                    ty,
                    outer_parameter_index(outer, lambda.parameter_count, lambda_index, column),
                )
            } else {
                let ty = bind_lambda(child_types, column as usize)?;
                Expression::reference(alias, ty, lambda.parameter_count - column - 1)
            };
            Ok(())
        }
        ExpressionKind::ColumnRef(_) | ExpressionKind::Parameter(_) => {
            let outer_count: usize = outer.iter().map(|binding| binding.names.len()).sum();
            let offset = outer_count as u64 + lambda.parameter_count + lambda.captures.len() as u64;
            let replacement = Expression::reference(expr.alias.clone(), expr.return_type.clone(), offset);
            let original = std::mem::replace(expr, replacement);
            lambda.captures.push(original);
            Ok(())
        }
        _ => expr.try_for_each_child_mut(|child| {
            capture_lambda_columns(child, lambda, scope, bind_lambda, child_types)
        }),
    }
}
