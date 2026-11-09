//! Moving constants across a comparison: `x + 1 = 10` becomes `x = 9` (DuckDB's
//! `MoveConstantsRule`).

use roastedduck_ast::ExpressionType;
use roastedduck_common::Result;
use roastedduck_function::cast::ValueCast;
use roastedduck_planner::{Expression, ExpressionKind};
use roastedduck_types::{LogicalType, Value};

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::expression_util::{constant_or_null, flip_comparison};

/// The position of the integral constant child of an integral `+`, `-` or `*` call
/// whose other child is integral too (the rule's arithmetic matcher).
fn arithmetic_constant_child(expression: &Expression) -> Option<usize> {
    let ExpressionKind::Function(function) = &expression.kind else {
        return None;
    };
    if !matches!(&*function.function.name, b"+" | b"-" | b"*")
        || !expression.return_type.is_integral()
        || function.children.len() != 2
    {
        return None;
    }
    let constant = function.children.iter().position(|child| {
        matches!(child.kind, ExpressionKind::Constant(_)) && child.return_type.is_integral()
    })?;
    function.children[1 - constant]
        .return_type
        .is_integral()
        .then_some(constant)
}

/// `value` cast to `ty`, or `None` if it does not fit (DuckDB's in-place
/// `Value::DefaultTryCastAs`).
fn try_cast(value: i128, ty: &LogicalType) -> Option<Value> {
    Value::hugeint(value).try_cast(ty).ok()
}

pub fn move_constants(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    let comparison_type = expression.expression_type;
    let return_type = expression.return_type.clone();
    let ExpressionKind::Comparison(comparison) = &mut expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    // UNORDERED policy: one side a constant, the other the arithmetic (left tried first).
    let outer_left = if matches!(comparison.left.kind, ExpressionKind::Constant(_))
        && arithmetic_constant_child(&comparison.right).is_some()
    {
        true
    } else if matches!(comparison.right.kind, ExpressionKind::Constant(_))
        && arithmetic_constant_child(&comparison.left).is_some()
    {
        false
    } else {
        return Ok(RuleOutcome::Unchanged);
    };
    let (outer, arithmetic) = if outer_left {
        (&mut comparison.left, &mut comparison.right)
    } else {
        (&mut comparison.right, &mut comparison.left)
    };
    let Some(inner_index) = arithmetic_constant_child(arithmetic) else {
        return Ok(RuleOutcome::Unchanged);
    };
    let arithmetic_child_index = 1 - inner_index;
    let (Some(outer_constant), ExpressionKind::Function(function)) =
        (outer.as_constant().cloned(), &mut arithmetic.kind)
    else {
        return Ok(RuleOutcome::Unchanged);
    };
    let Some(inner_constant) = function.children[inner_index].as_constant().cloned() else {
        return Ok(RuleOutcome::Unchanged);
    };
    if inner_constant.is_null() || outer_constant.is_null() {
        if matches!(
            comparison_type,
            ExpressionType::CompareDistinctFrom | ExpressionType::CompareNotDistinctFrom
        ) {
            return Ok(RuleOutcome::Unchanged);
        }
        return Ok(RuleOutcome::Replaced(Expression::constant(Value::null(
            return_type,
        ))));
    }
    let constant_type = outer.return_type.clone();
    let (Some(outer_value), Some(inner_value)) = (outer_constant.as_i128(), inner_constant.as_i128()) else {
        return Ok(RuleOutcome::Unchanged);
    };
    let mut new_comparison = comparison_type;
    let impossible = |function: &mut roastedduck_planner::expression::BoundFunctionExpression,
                      result: bool| {
        let child = function.children.swap_remove(arithmetic_child_index);
        constant_or_null(context.client, vec![child], Value::boolean(result)).map(RuleOutcome::Replaced)
    };
    let name = function.function.name.clone();
    let result_value = match &*name {
        b"+" => {
            let Some(value) = outer_value.checked_sub(inner_value) else {
                return Ok(RuleOutcome::Unchanged);
            };
            match try_cast(value, &constant_type) {
                Some(value) => value,
                None if comparison_type == ExpressionType::CompareEqual => {
                    return impossible(function, false);
                }
                None => return Ok(RuleOutcome::Unchanged),
            }
        }
        b"-" if arithmetic_child_index == 0 => {
            let Some(value) = outer_value.checked_add(inner_value) else {
                return Ok(RuleOutcome::Unchanged);
            };
            match try_cast(value, &constant_type) {
                Some(value) => value,
                None if comparison_type == ExpressionType::CompareEqual => {
                    return impossible(function, false);
                }
                None => return Ok(RuleOutcome::Unchanged),
            }
        }
        b"-" => {
            let Some(value) = inner_value.checked_sub(outer_value) else {
                return Ok(RuleOutcome::Unchanged);
            };
            let value = match try_cast(value, &constant_type) {
                Some(value) => value,
                None if comparison_type == ExpressionType::CompareEqual => {
                    return impossible(function, false);
                }
                None => return Ok(RuleOutcome::Unchanged),
            };
            new_comparison = flip_comparison(comparison_type);
            value
        }
        _ => {
            if inner_value == 0 {
                // `x * 0` is left to the arithmetic simplification.
                return Ok(RuleOutcome::Unchanged);
            }
            if (outer_value == i128::MIN && inner_value == -1) || outer_value % inner_value != 0 {
                let is_equality = comparison_type == ExpressionType::CompareEqual;
                let is_inequality = comparison_type == ExpressionType::CompareNotequal;
                if is_equality || is_inequality {
                    return impossible(function, is_inequality);
                }
                return Ok(RuleOutcome::Unchanged);
            }
            if inner_value < 0 {
                new_comparison = flip_comparison(comparison_type);
            }
            match try_cast(outer_value / inner_value, &constant_type) {
                Some(value) => value,
                None => return impossible(function, false),
            }
        }
    };
    let arithmetic_child = function.children.swap_remove(arithmetic_child_index);
    *outer = Expression::constant(result_value);
    *arithmetic = arithmetic_child;
    expression.expression_type = new_comparison;
    Ok(RuleOutcome::Changed)
}
