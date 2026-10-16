//! Comparisons with constants (DuckDB's `ComparisonSimplificationRule`): a comparison
//! with NULL is NULL, and `CAST(x AS T) <op> constant` compares `x` with the constant
//! cast back when the cast is invertible. Also `CAST(x AS T) IN (constants)` (DuckDB's
//! `InClauseSimplificationRule`).

use rustdb_ast::ExpressionType;
use rustdb_common::Result;
use rustdb_function::cast::ValueCast;
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_types::{Date, LogicalType, LogicalTypeId, Value};

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::expression_util::{cast_is_invertible, constant_or_null, evaluate_scalar, try_evaluate_scalar};

/// The microseconds in a day.
const MICROS_PER_DAY: i64 = 86_400_000_000;

/// What comparing a DATE column against a non-midnight TIMESTAMP constant amounts to
/// (DuckDB's `DateTimestampComparisonIsInvertible`): `Err(expression)` replaces the whole
/// comparison; `Ok(Some(date))` is the date to compare with; `Ok(None)` gives up.
fn date_timestamp_comparison(
    context: &RuleContext<'_>,
    comparison_type: ExpressionType,
    column: &Expression,
    constant: &Value,
    cast_constant: &Value,
    column_ref_left: bool,
) -> Result<std::result::Result<Option<Value>, Expression>> {
    use ExpressionType as E;
    let Some(timestamp) = constant.as_timestamp() else {
        return Ok(Ok(Some(cast_constant.clone())));
    };
    if timestamp.0.rem_euclid(MICROS_PER_DAY) == 0 {
        return Ok(Ok(Some(cast_constant.clone())));
    }
    let add_one_day = match comparison_type {
        E::CompareEqual => {
            return Ok(Err(constant_or_null(
                context.client,
                vec![column.clone()],
                Value::boolean(false),
            )?));
        }
        E::CompareNotequal => {
            return Ok(Err(constant_or_null(
                context.client,
                vec![column.clone()],
                Value::boolean(true),
            )?));
        }
        E::CompareDistinctFrom => return Ok(Err(Expression::constant(Value::boolean(true)))),
        E::CompareNotDistinctFrom => return Ok(Err(Expression::constant(Value::boolean(false)))),
        E::CompareLessthan | E::CompareGreaterthanorequalto => column_ref_left,
        E::CompareLessthanorequalto | E::CompareGreaterthan => !column_ref_left,
        _ => return Ok(Ok(None)),
    };
    if add_one_day {
        let Some(date) = cast_constant.as_date() else {
            return Ok(Ok(None));
        };
        return Ok(Ok(Some(Value::date(Date(date.0 + 1)))));
    }
    Ok(Ok(Some(cast_constant.clone())))
}

pub fn comparison_simplification(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    let comparison_type = expression.expression_type;
    let ExpressionKind::Comparison(comparison) = &mut expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    // SOME policy with one foldable matcher: the left side if foldable, else the right.
    let column_ref_left = if comparison.left.is_foldable() {
        false
    } else if comparison.right.is_foldable() {
        true
    } else {
        return Ok(RuleOutcome::Unchanged);
    };
    let constant_expr = if column_ref_left {
        &comparison.right
    } else {
        &comparison.left
    };
    let Some(constant_value) = try_evaluate_scalar(context.client, constant_expr) else {
        return Ok(RuleOutcome::Unchanged);
    };
    if constant_value.is_null()
        && !matches!(
            comparison_type,
            ExpressionType::CompareNotDistinctFrom | ExpressionType::CompareDistinctFrom
        )
    {
        return Ok(RuleOutcome::Replaced(Expression::constant(Value::null(
            LogicalType::BOOLEAN,
        ))));
    }
    let column_ref_expr = if column_ref_left {
        &comparison.left
    } else {
        &comparison.right
    };
    let ExpressionKind::Cast(cast) = &column_ref_expr.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    let target_type = cast.child.return_type.clone();
    if !cast_is_invertible(&target_type, &column_ref_expr.return_type) {
        return Ok(RuleOutcome::Unchanged);
    }
    let Ok(cast_constant) = constant_value.try_cast_as(&context.client.casts, &target_type, true) else {
        return Ok(RuleOutcome::Unchanged);
    };
    let mut new_constant = cast_constant.clone();
    if !cast_constant.is_null() && !cast_is_invertible(&column_ref_expr.return_type, &target_type) {
        if target_type.id() != LogicalTypeId::Date
            || column_ref_expr.return_type.id() != LogicalTypeId::Timestamp
        {
            return Ok(RuleOutcome::Unchanged);
        }
        match date_timestamp_comparison(
            context,
            comparison_type,
            &cast.child,
            &constant_value,
            &cast_constant,
            column_ref_left,
        )? {
            Err(replacement) => return Ok(RuleOutcome::Replaced(replacement)),
            Ok(None) => return Ok(RuleOutcome::Unchanged),
            Ok(Some(value)) => new_constant = value,
        }
    }
    let column_side = if column_ref_left {
        &mut comparison.left
    } else {
        &mut comparison.right
    };
    let ExpressionKind::Cast(cast) = std::mem::replace(&mut column_side.kind, ExpressionKind::Default) else {
        return Ok(RuleOutcome::Unchanged);
    };
    *column_side = cast.child;
    let constant_side = if column_ref_left {
        &mut comparison.right
    } else {
        &mut comparison.left
    };
    *constant_side = Expression::constant(new_constant);
    Ok(RuleOutcome::Changed)
}

/// `CAST(x AS T) IN (c1, c2, ...)` as `x IN (c1', c2', ...)` when every cast is invertible.
pub fn in_clause_simplification(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    if expression.expression_type != ExpressionType::CompareIn {
        return Ok(RuleOutcome::Unchanged);
    }
    let ExpressionKind::Operator(operator) = &mut expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    let Some(probe) = operator.children.first() else {
        return Ok(RuleOutcome::Unchanged);
    };
    let ExpressionKind::Cast(cast) = &probe.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    if !matches!(cast.child.kind, ExpressionKind::ColumnRef(_)) {
        return Ok(RuleOutcome::Unchanged);
    }
    let target_type = cast.child.return_type.clone();
    if !cast_is_invertible(&target_type, &probe.return_type) {
        return Ok(RuleOutcome::Unchanged);
    }
    let mut cast_list = Vec::with_capacity(operator.children.len() - 1);
    for child in &operator.children[1..] {
        if !matches!(child.kind, ExpressionKind::Constant(_)) {
            return Ok(RuleOutcome::Unchanged);
        }
        let constant_value = evaluate_scalar(context.client, child)?;
        if !cast_is_invertible(constant_value.logical_type(), &target_type) {
            return Ok(RuleOutcome::Unchanged);
        }
        let Ok(new_constant) = constant_value.try_cast(&target_type) else {
            return Ok(RuleOutcome::Unchanged);
        };
        cast_list.push(Expression::constant(new_constant));
    }
    for (slot, constant) in operator.children[1..].iter_mut().zip(cast_list) {
        *slot = constant;
    }
    let probe = std::mem::replace(&mut operator.children[0], Expression::constant(Value::default()));
    if let ExpressionKind::Cast(cast) = probe.kind {
        operator.children[0] = cast.child;
    }
    // DuckDB changes the expression in place without reporting it.
    Ok(RuleOutcome::Unchanged)
}
