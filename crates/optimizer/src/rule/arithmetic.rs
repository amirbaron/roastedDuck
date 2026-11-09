//! Arithmetic with neutral or absorbing constants: `x + 0`, `x * 1`, `x * 0`, `x // 1`,
//! `x // 0` (DuckDB's `ArithmeticSimplificationRule`).

use roastedduck_common::Result;
use roastedduck_planner::{Expression, ExpressionKind};
use roastedduck_types::Value;

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::expression_util::constant_or_null;

/// The numeric value of an integral constant.
fn integral(value: &Value) -> Option<i128> {
    value.as_i128()
}

/// `0` of an integral type (DuckDB's `Value::Numeric(type, 0)`).
fn zero_of(ty: &roastedduck_types::LogicalType) -> Result<Value> {
    use roastedduck_function::cast::ValueCast;
    Value::integer(0).default_cast_as(ty)
}

pub fn arithmetic_simplification(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    if !expression.return_type.is_integral() {
        return Ok(RuleOutcome::Unchanged);
    }
    let ExpressionKind::Function(function) = &mut expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    let name = function.function.name.clone();
    if !matches!(&*name, b"+" | b"-" | b"*" | b"//") || function.children.len() != 2 {
        return Ok(RuleOutcome::Unchanged);
    }
    // SOME policy: the first integral constant child, then any other integral child.
    let Some(constant_child) = function.children.iter().position(|child| {
        child.return_type.is_integral() && matches!(child.kind, ExpressionKind::Constant(_))
    }) else {
        return Ok(RuleOutcome::Unchanged);
    };
    if !function.children[1 - constant_child].return_type.is_integral() {
        return Ok(RuleOutcome::Unchanged);
    }
    let Some(constant) = function.children[constant_child].as_constant().cloned() else {
        return Ok(RuleOutcome::Unchanged);
    };
    let return_type = expression.return_type.clone();
    if constant.is_null() {
        return Ok(RuleOutcome::Replaced(Expression::constant(Value::null(
            return_type,
        ))));
    }
    let value = integral(&constant);
    let mut other = || function.children.swap_remove(1 - constant_child);
    let replaced = match &*name {
        b"+" if value == Some(0) => Some(other()),
        b"-" if constant_child == 1 && value == Some(0) => Some(other()),
        b"*" if value == Some(1) => Some(other()),
        b"*" if value == Some(0) => {
            let child = other();
            Some(constant_or_null(
                context.client,
                vec![child],
                zero_of(&return_type)?,
            )?)
        }
        b"//" if constant_child == 1 && value == Some(1) => Some(other()),
        b"//" if constant_child == 1 && value == Some(0) => {
            Some(Expression::constant(Value::null(return_type)))
        }
        _ => None,
    };
    Ok(replaced.map_or(RuleOutcome::Unchanged, RuleOutcome::Replaced))
}
