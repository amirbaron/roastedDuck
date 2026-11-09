//! `date_trunc(part, column) <op> constant` as a range on the column (DuckDB's
//! `DateTruncSimplificationRule`), so that it can become a table filter, e.g.
//! `date_trunc('day', d) = '2025-01-08'` as `d >= '2025-01-08' AND d < '2025-01-09'`.

use roastedduck_ast::ExpressionType;
use roastedduck_common::Result;
use roastedduck_planner::function_binder::add_cast_to_type;
use roastedduck_planner::{Expression, ExpressionKind};
use roastedduck_types::{LogicalType, Value};

use super::RuleOutcome;
use super::date_part::part_function;
use crate::expression_rewriter::RuleContext;
use crate::expression_util::{flip_comparison, is_comparison, try_evaluate_scalar};
use crate::optimizer::bind_scalar_function;

/// The interval constructor for a date part, if the part is an interval unit (DuckDB's
/// `DatePartToFunc`).
fn interval_function(specifier: &str) -> Result<Option<&'static str>> {
    Ok(match part_function(specifier)? {
        Some("year") => Some("to_years"),
        Some("month") => Some("to_months"),
        Some("day") => Some("to_days"),
        Some("decade") => Some("to_decades"),
        Some("century") => Some("to_centuries"),
        Some("millennium") => Some("to_millennia"),
        Some("microsecond") => Some("to_microseconds"),
        Some("millisecond") => Some("to_milliseconds"),
        Some("second") => Some("to_seconds"),
        Some("minute") => Some("to_minutes"),
        Some("hour") => Some("to_hours"),
        Some("week") => Some("to_weeks"),
        Some("quarter") => Some("to_quarters"),
        _ => None,
    })
}

/// Casts `expression` to `target` unless it already has the type id, folding constants.
fn cast_and_fold(
    context: &RuleContext<'_>,
    expression: Expression,
    target: &LogicalType,
) -> Result<Expression> {
    let expression = if expression.return_type.id() == target.id() {
        expression
    } else {
        add_cast_to_type(context.client, expression, target, true)?
    };
    if expression.is_foldable()
        && let Some(value) = try_evaluate_scalar(context.client, &expression)
    {
        return Ok(Expression::constant(value));
    }
    Ok(expression)
}

/// `date_trunc(part, rhs)` as the column's type, folded (DuckDB's `CreateTrunc`).
fn create_trunc(
    context: &RuleContext<'_>,
    part: &Expression,
    rhs: &Expression,
    target: &LogicalType,
) -> Result<Option<Expression>> {
    let Ok(trunc) = bind_scalar_function(
        context.client,
        "date_trunc",
        vec![part.clone(), rhs.clone()],
        false,
    ) else {
        return Ok(None);
    };
    cast_and_fold(context, trunc, target).map(Some)
}

/// `date_trunc(part, rhs + INTERVAL 1 part)` as the column's type, folded (DuckDB's
/// `CreateTruncAdd`); `None` when the part is no interval unit.
fn create_trunc_add(
    context: &RuleContext<'_>,
    part: &Expression,
    specifier: &str,
    rhs: &Expression,
    target: &LogicalType,
) -> Result<Option<Expression>> {
    let Some(function) = interval_function(specifier)? else {
        return Ok(None);
    };
    let Ok(interval) = bind_scalar_function(
        context.client,
        function,
        vec![Expression::constant(Value::integer(1))],
        false,
    ) else {
        return Ok(None);
    };
    let Ok(add) = bind_scalar_function(context.client, "+", vec![rhs.clone(), interval], true) else {
        return Ok(None);
    };
    let Ok(trunc) = bind_scalar_function(context.client, "date_trunc", vec![part.clone(), add], false) else {
        return Ok(None);
    };
    cast_and_fold(context, trunc, target).map(Some)
}

/// Whether `date_trunc(part, rhs) = rhs` (DuckDB's `DateIsTruncated`).
fn date_is_truncated(
    context: &RuleContext<'_>,
    part: &Expression,
    rhs: &Expression,
    rhs_value: &Value,
) -> Result<bool> {
    if rhs_value.is_null() {
        return Ok(true);
    }
    let Some(trunc) = create_trunc(context, part, rhs, &rhs.return_type)? else {
        return Ok(false);
    };
    let Some(truncated) = try_evaluate_scalar(context.client, &trunc) else {
        return Ok(false);
    };
    Ok(truncated == *rhs_value)
}

/// The date part constant and the input of `date_trunc(constant, input)`.
fn as_trunc(side: &Expression) -> Option<(&Expression, &Expression)> {
    let ExpressionKind::Function(function) = &side.kind else {
        return None;
    };
    if !matches!(&*function.function.name, b"date_trunc" | b"datetrunc") || function.children.len() != 2 {
        return None;
    }
    if !matches!(function.children[0].kind, ExpressionKind::Constant(_)) {
        return None;
    }
    Some((&function.children[0], &function.children[1]))
}

/// The matched parts: the date part constant, the column, the constant and whether the
/// truncation is on the left.
fn match_date_trunc(expression: &Expression) -> Option<(&Expression, &Expression, &Expression, bool)> {
    let ExpressionKind::Comparison(comparison) = &expression.kind else {
        return None;
    };
    // UNORDERED matching: the truncation first on the left, then on the right.
    if let Some((part, column)) = as_trunc(&comparison.left)
        && matches!(comparison.right.kind, ExpressionKind::Constant(_))
    {
        return Some((part, column, &comparison.right, true));
    }
    if let Some((part, column)) = as_trunc(&comparison.right)
        && matches!(comparison.left.kind, ExpressionKind::Constant(_))
    {
        return Some((part, column, &comparison.left, false));
    }
    None
}

#[allow(clippy::too_many_lines)]
pub fn date_trunc_simplification(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    use ExpressionType as E;
    if !is_comparison(expression.expression_type) {
        return Ok(RuleOutcome::Unchanged);
    }
    let comparison_type = expression.expression_type;
    let Some((part, column, rhs, col_is_lhs)) = match_date_trunc(expression) else {
        return Ok(RuleOutcome::Unchanged);
    };
    if !matches!(column.kind, ExpressionKind::ColumnRef(_)) {
        return Ok(RuleOutcome::Unchanged);
    }
    let (part, column, rhs) = (part.clone(), column.clone(), rhs.clone());
    let (Some(part_value), Some(rhs_value)) = (part.as_constant().cloned(), rhs.as_constant().cloned())
    else {
        return Ok(RuleOutcome::Unchanged);
    };
    let specifier = part_value.to_varchar().unwrap_or_default();
    let rhs_comparison = if col_is_lhs {
        comparison_type
    } else {
        flip_comparison(comparison_type)
    };
    let is_truncated = date_is_truncated(context, &part, &rhs, &rhs_value)?;
    let column_type = column.return_type.clone();
    let operator = |expression_type, child: &Expression| {
        Expression::operator(expression_type, LogicalType::BOOLEAN, vec![child.clone()])
    };
    match rhs_comparison {
        E::CompareEqual | E::CompareNotDistinctFrom => {
            if rhs_comparison == E::CompareNotDistinctFrom && rhs_value.is_null() {
                return Ok(RuleOutcome::Replaced(operator(E::OperatorIsNull, &column)));
            }
            if !is_truncated {
                return Ok(RuleOutcome::Replaced(Expression::constant(Value::boolean(false))));
            }
            let Some(trunc) = create_trunc(context, &part, &rhs, &column_type)? else {
                return Ok(RuleOutcome::Unchanged);
            };
            let Some(trunc_add) = create_trunc_add(context, &part, &specifier, &rhs, &column_type)? else {
                return Ok(RuleOutcome::Unchanged);
            };
            let greater_equal = Expression::comparison(E::CompareGreaterthanorequalto, column.clone(), trunc);
            let less_than = Expression::comparison(E::CompareLessthan, column.clone(), trunc_add);
            let range = Expression::conjunction(E::ConjunctionAnd, vec![greater_equal, less_than]);
            if rhs_comparison == E::CompareNotDistinctFrom {
                return Ok(RuleOutcome::Replaced(Expression::conjunction(
                    E::ConjunctionAnd,
                    vec![range, operator(E::OperatorIsNotNull, &column)],
                )));
            }
            Ok(RuleOutcome::Replaced(range))
        }
        E::CompareNotequal | E::CompareDistinctFrom => {
            if rhs_comparison == E::CompareDistinctFrom && rhs_value.is_null() {
                return Ok(RuleOutcome::Replaced(operator(E::OperatorIsNotNull, &column)));
            }
            if !is_truncated {
                return Ok(RuleOutcome::Replaced(Expression::constant(Value::boolean(true))));
            }
            let Some(trunc) = create_trunc(context, &part, &rhs, &column_type)? else {
                return Ok(RuleOutcome::Unchanged);
            };
            let Some(trunc_add) = create_trunc_add(context, &part, &specifier, &rhs, &column_type)? else {
                return Ok(RuleOutcome::Unchanged);
            };
            let less_than = Expression::comparison(E::CompareLessthan, column.clone(), trunc);
            let greater_equal =
                Expression::comparison(E::CompareGreaterthanorequalto, column.clone(), trunc_add);
            let outside = Expression::conjunction(E::ConjunctionOr, vec![greater_equal, less_than]);
            if rhs_comparison == E::CompareDistinctFrom {
                return Ok(RuleOutcome::Replaced(Expression::conjunction(
                    E::ConjunctionOr,
                    vec![outside, operator(E::OperatorIsNull, &column)],
                )));
            }
            Ok(RuleOutcome::Replaced(outside))
        }
        E::CompareLessthan | E::CompareGreaterthanorequalto => {
            // With a truncated constant the constant itself is the boundary.
            let bound = if is_truncated {
                cast_and_fold(context, rhs, &column_type)?
            } else {
                let Some(trunc) = create_trunc_add(context, &part, &specifier, &rhs, &column_type)? else {
                    return Ok(RuleOutcome::Unchanged);
                };
                trunc
            };
            set_sides(expression, column, bound, col_is_lhs, None);
            Ok(RuleOutcome::Changed)
        }
        E::CompareLessthanorequalto | E::CompareGreaterthan => {
            let Some(trunc) = create_trunc_add(context, &part, &specifier, &rhs, &column_type)? else {
                return Ok(RuleOutcome::Unchanged);
            };
            // `>` becomes `>=` and `<=` becomes `<`.
            let new_type = match (rhs_comparison == E::CompareGreaterthan, col_is_lhs) {
                (true, true) => E::CompareGreaterthanorequalto,
                (true, false) => E::CompareLessthanorequalto,
                (false, true) => E::CompareLessthan,
                (false, false) => E::CompareGreaterthan,
            };
            set_sides(expression, column, trunc, col_is_lhs, Some(new_type));
            Ok(RuleOutcome::Changed)
        }
        _ => Ok(RuleOutcome::Unchanged),
    }
}

/// Puts the column and the boundary on the sides the truncation and constant had.
fn set_sides(
    expression: &mut Expression,
    column: Expression,
    bound: Expression,
    col_is_lhs: bool,
    new_type: Option<ExpressionType>,
) {
    if let ExpressionKind::Comparison(comparison) = &mut expression.kind {
        if col_is_lhs {
            comparison.left = column;
            comparison.right = bound;
        } else {
            comparison.right = column;
            comparison.left = bound;
        }
    }
    if let Some(new_type) = new_type {
        expression.expression_type = new_type;
    }
}
