//! `CAST(ts AS DATE) = CAST('...' AS DATE)` as a timestamp range (DuckDB's
//! `TimeStampComparison`): `ts >= DATE AND ts < DATE + 1`.

use roastedduck_ast::ExpressionType;
use roastedduck_common::Result;
use roastedduck_planner::{Expression, ExpressionKind};
use roastedduck_types::{Date, LogicalTypeId, Timestamp, Value};

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::expression_util::try_evaluate_scalar;

/// A DATE cast of a TIMESTAMP column.
fn is_date_cast_of_timestamp_column(expression: &Expression) -> bool {
    matches!(&expression.kind, ExpressionKind::Cast(cast)
        if expression.return_type.id() == LogicalTypeId::Date
            && matches!(cast.child.kind, ExpressionKind::ColumnRef(_))
            && cast.child.return_type.id() == LogicalTypeId::Timestamp)
}

/// A DATE cast of a foldable expression (DuckDB's `FoldableConstantMatcher` ignores the
/// class and type it is given).
fn is_date_cast_of_varchar_constant(expression: &Expression) -> bool {
    matches!(&expression.kind, ExpressionKind::Cast(cast)
        if expression.return_type.id() == LogicalTypeId::Date && cast.child.is_foldable())
}

pub fn timestamp_comparison(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    if expression.expression_type != ExpressionType::CompareEqual {
        return Ok(RuleOutcome::Unchanged);
    }
    let ExpressionKind::Comparison(comparison) = &expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    let (column_cast, constant_cast) = if is_date_cast_of_timestamp_column(&comparison.left)
        && is_date_cast_of_varchar_constant(&comparison.right)
    {
        (&comparison.left, &comparison.right)
    } else if is_date_cast_of_timestamp_column(&comparison.right)
        && is_date_cast_of_varchar_constant(&comparison.left)
    {
        (&comparison.right, &comparison.left)
    } else {
        return Ok(RuleOutcome::Unchanged);
    };
    let ExpressionKind::Cast(cast) = &column_cast.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    let column = &cast.child;
    let Some(date) = try_evaluate_scalar(context.client, constant_cast).and_then(|value| value.as_date())
    else {
        return Ok(RuleOutcome::Unchanged);
    };
    let Some(start) = Timestamp::from_date(date) else {
        return Ok(RuleOutcome::Unchanged);
    };
    let Some(next_day) = date.0.checked_add(1).map(Date).filter(|next| next.is_finite()) else {
        return Ok(RuleOutcome::Unchanged);
    };
    let Some(end) = Timestamp::from_date(next_day) else {
        return Ok(RuleOutcome::Unchanged);
    };
    let greater_equal = Expression::comparison(
        ExpressionType::CompareGreaterthanorequalto,
        column.clone(),
        Expression::constant(Value::timestamp(start)),
    );
    let less_than = Expression::comparison(
        ExpressionType::CompareLessthan,
        column.clone(),
        Expression::constant(Value::timestamp(end)),
    );
    Ok(RuleOutcome::Replaced(Expression::conjunction(
        ExpressionType::ConjunctionAnd,
        vec![greater_equal, less_than],
    )))
}
