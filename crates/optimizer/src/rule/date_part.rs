//! `date_part('year', x)` as `year(x)` (DuckDB's `DatePartSimplificationRule`).

use roastedduck_common::{Error, Result};
use roastedduck_planner::{Expression, ExpressionKind};
use roastedduck_types::Value;

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::optimizer::bind_scalar_function;

/// The function computing a date part, `None` for parts without one, or an error for an
/// unknown specifier (DuckDB's `GetDatePartSpecifier` and the rule's switch).
pub(crate) fn part_function(specifier: &str) -> Result<Option<&'static str>> {
    Ok(match specifier.to_lowercase().as_str() {
        "year" | "yr" | "y" | "years" | "yrs" => Some("year"),
        "month" | "mon" | "months" | "mons" => Some("month"),
        "day" | "days" | "d" | "dayofmonth" => Some("day"),
        "decade" | "dec" | "decades" | "decs" => Some("decade"),
        "century" | "cent" | "centuries" | "c" => Some("century"),
        "millennium" | "mil" | "millenniums" | "millennia" | "mils" | "millenium" => Some("millennium"),
        "microseconds" | "microsecond" | "us" | "usec" | "usecs" | "usecond" | "useconds" => {
            Some("microsecond")
        }
        "milliseconds" | "millisecond" | "ms" | "msec" | "msecs" | "msecond" | "mseconds" => {
            Some("millisecond")
        }
        "second" | "sec" | "seconds" | "secs" | "s" => Some("second"),
        "minute" | "min" | "minutes" | "mins" | "m" => Some("minute"),
        "hour" | "hr" | "hours" | "hrs" | "h" => Some("hour"),
        "dow" | "dayofweek" | "weekday" => Some("dayofweek"),
        "isodow" => Some("isodow"),
        "week" | "weeks" | "w" | "weekofyear" => Some("week"),
        "doy" | "dayofyear" => Some("dayofyear"),
        "quarter" | "quarters" => Some("quarter"),
        "yearweek" => Some("yearweek"),
        "epoch" | "isoyear" | "era" | "timezone" | "timezone_hour" | "timezone_minute" | "julian" | "jd" => {
            None
        }
        _ => {
            return Err(Error::conversion(format!(
                "extract specifier \"{specifier}\" not recognized"
            )));
        }
    })
}

pub fn date_part_simplification(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    let ExpressionKind::Function(function) = &mut expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    if &*function.function.name != b"date_part" || function.children.len() != 2 {
        return Ok(RuleOutcome::Unchanged);
    }
    let Some(constant) = function.children[0].as_constant() else {
        return Ok(RuleOutcome::Unchanged);
    };
    if constant.is_null() {
        return Ok(RuleOutcome::Replaced(Expression::constant(Value::null(
            expression.return_type.clone(),
        ))));
    }
    let Some(specifier) = constant.as_str() else {
        return Ok(RuleOutcome::Unchanged);
    };
    let Some(name) = part_function(specifier)? else {
        return Ok(RuleOutcome::Unchanged);
    };
    let child = function.children.remove(1);
    Ok(RuleOutcome::Replaced(bind_scalar_function(
        context.client,
        name,
        vec![child],
        false,
    )?))
}
