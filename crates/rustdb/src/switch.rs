//! `switch` (DuckDB's `core_functions/scalar/map/switch.cpp`): a CASE written as a call,
//! `switch(x, MAP {key: value, ...}, default)`, which the binder replaces by the CASE.

use rustdb_ast::ExpressionType;
use rustdb_common::{Error, Result};
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_planner::ClientContext;
use rustdb_planner::expression::{BoundCaseCheck, BoundCaseExpression, Expression, ExpressionKind};
use rustdb_planner::function_binder::{RewriteExpression, add_cast_to_type, max_logical_type};
use rustdb_types::{LogicalType, LogicalTypeId, TemplateTypeInfo, TypeInfo, Value};

/// `LogicalType::TEMPLATE(name)`.
fn template(name: &str) -> LogicalType {
    LogicalType::with_info(
        LogicalTypeId::Template,
        TypeInfo::Template(TemplateTypeInfo {
            name: name.to_owned(),
        }),
    )
}

/// `SwitchFun::GetFunctions`.
pub(crate) fn switch_functions() -> ScalarFunctionSet {
    let key = template("K");
    let value = template("V");
    let map = LogicalType::map(key.clone(), value.clone());
    let mut set = ScalarFunctionSet::new("switch");
    for arguments in [
        vec![key.clone(), map.clone()],
        vec![key.clone(), map.clone(), value.clone()],
        vec![map.clone(), value.clone()],
        vec![map.clone()],
    ] {
        let mut function = ScalarFunction::new("switch", arguments, value.clone(), |_, _, _| {
            Err(Error::internal("switch is replaced by a CASE when bound"))
        });
        function.rewrite_expression = Some(RewriteExpression::erased(switch_bind_expression));
        set.add(function);
    }
    set
}

/// The call of `map` an argument binds to, under the cast to the overload's type.
fn map_call(expression: &Expression) -> Option<&Expression> {
    match &expression.kind {
        ExpressionKind::Function(function) if function.function.name.as_bytes() == b"map" => Some(expression),
        ExpressionKind::Cast(cast) => map_call(&cast.child),
        _ => None,
    }
}

/// `ExtractConstantExprFromList`: the constants of a `list_value` call.
fn constant_list(expression: Expression) -> Result<Vec<Expression>> {
    let ExpressionKind::Function(list) = expression.kind else {
        return Err(Error::binder("Expected a function for the cases"));
    };
    if list.function.name.as_bytes() != b"list_value" {
        return Err(Error::binder("Expected a list function"));
    }
    if list.children.is_empty() {
        return Err(Error::binder("No values provided for SWITCH expression"));
    }
    for child in &list.children {
        if !matches!(child.kind, ExpressionKind::Constant(_)) {
            return Err(Error::not_implemented(
                "Only constant expressions are supported for keys inside SWITCH",
            ));
        }
    }
    Ok(list.children)
}

/// `SwitchBindReturnType` and `SwitchBindExpression`: the call becomes
/// `CASE x WHEN key THEN value ... ELSE default END`.
fn switch_bind_expression(context: &ClientContext, bound: Expression) -> Result<Expression> {
    let ExpressionKind::Function(function) = bound.kind else {
        return Err(Error::internal("switch did not bind to a function call"));
    };
    let mut children = function.children;
    let Some(map_index) = children
        .iter()
        .position(|child| child.return_type.id() == LogicalTypeId::Map)
    else {
        return Err(Error::binder("Switch: No map argument found"));
    };
    let Some(cases) = map_call(&children[map_index]) else {
        return Err(Error::binder("SWITCH expected a constant map for the cases"));
    };
    // The map is evaluated while binding, which validates it (e.g. NULL keys).
    context.evaluator.evaluate_scalar(cases)?;
    let return_type = cases
        .return_type
        .map_key_value()
        .map_or(LogicalType::SQLNULL, |(_, value)| value.clone());
    let cases = cases.clone();
    let default = (children.len() > map_index + 1).then(|| children.swap_remove(map_index + 1));
    let base = (map_index == 1).then(|| children.swap_remove(0));
    let ExpressionKind::Function(cases) = cases.kind else {
        return Err(Error::internal("switch cases are not a map call"));
    };
    let mut lists = cases.children.into_iter();
    let (Some(keys), Some(values)) = (lists.next(), lists.next()) else {
        return Err(Error::internal("map call without keys and values"));
    };
    let keys = constant_list(keys)?;
    let values = constant_list(values)?;
    // The type the ELSE NULL is cast to, widened by every value.
    let mut else_type = return_type.clone();
    let mut case_checks = Vec::with_capacity(keys.len());
    for (key, value) in keys.into_iter().zip(values) {
        let when_expr = match &base {
            Some(base) => {
                let ty = max_logical_type(context, &base.return_type, &key.return_type)?;
                Expression::comparison(
                    ExpressionType::CompareEqual,
                    base.clone(),
                    add_cast_to_type(context, key, &ty, false)?,
                )
            }
            None => add_cast_to_type(context, key, &LogicalType::BOOLEAN, false)?,
        };
        else_type = context.try_max_logical_type(&else_type, &value.return_type).ok_or_else(|| {
            Error::binder(format!(
                "Cannot mix values of type {else_type} and {} in CASE expression - an explicit cast is required",
                value.return_type
            ))
        })?;
        case_checks.push(BoundCaseCheck {
            when_expr,
            then_expr: value,
        });
    }
    let else_expr = match default {
        Some(default) => default,
        None => add_cast_to_type(context, Expression::constant(Value::default()), &else_type, false)?,
    };
    Ok(Expression::new(
        ExpressionType::CaseExpr,
        return_type,
        ExpressionKind::Case(Box::new(BoundCaseExpression {
            case_checks,
            else_expr,
        })),
    ))
}
