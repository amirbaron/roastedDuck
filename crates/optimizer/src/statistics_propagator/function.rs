//! The statistics callbacks of functions (DuckDB's `FunctionStatisticsInput` callbacks,
//! e.g. `PropagateNumericStats`, `PropagateAbsStats`, `CountPropagateStats`), selected
//! by the bound function's name and signature.

use rustdb_common::Result;
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_storage::statistics::{BaseStatistics, NumericValue};
use rustdb_types::{LogicalType, LogicalTypeId, PhysicalType};

use super::{StatisticsPropagator, has_min_max, numeric_max, numeric_min, set_max, set_min};
use crate::optimizer::bind_aggregate_function;

/// The statistics of a scalar function call, possibly simplifying it.
#[allow(clippy::unnecessary_wraps)] // callbacks may bind replacement functions
pub(super) fn propagate_scalar_function(
    _propagator: &mut StatisticsPropagator<'_>,
    expression: &mut Expression,
    child_stats: &[BaseStatistics],
) -> Result<Option<BaseStatistics>> {
    let ExpressionKind::Function(function) = &expression.kind else {
        return Ok(None);
    };
    match &*function.function.name {
        "+" | "-" | "*" if child_stats.len() == 2 => Ok(propagate_arithmetic(expression, child_stats)),
        "abs" | "@" if child_stats.len() == 1 => Ok(propagate_abs(expression, child_stats)),
        "stats" => {
            // DuckDB's `StatsPropagateStats`: record the argument's statistics.
            if let (Some(data), Some(stats)) = (
                function
                    .bind_info
                    .as_ref()
                    .and_then(|data| data.as_any().downcast_ref::<rustdb_function::StatsBindData>()),
                child_stats.first(),
            ) {
                *data
                    .stats
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = stats.to_string();
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// Bind data marking an aggregate bound without DuckDB's statistics callback (e.g. the
/// `CountFunctionBase` COUNT the sum rewriter adds).
#[derive(Debug)]
pub struct WithoutStatistics;

impl rustdb_function::FunctionData for WithoutStatistics {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The statistics of an aggregate, possibly simplifying it.
pub(super) fn propagate_aggregate_function(
    propagator: &mut StatisticsPropagator<'_>,
    expression: &mut Expression,
    child_stats: &[BaseStatistics],
) -> Result<Option<BaseStatistics>> {
    let ExpressionKind::Aggregate(aggregate) = &mut expression.kind else {
        return Ok(None);
    };
    // DuckDB's `CountPropagateStats`: COUNT of a column without NULLs is COUNT(*).
    let without_statistics = aggregate
        .bind_info
        .as_ref()
        .is_some_and(|data| data.as_any().is::<WithoutStatistics>());
    if &*aggregate.function.name == "count"
        && !without_statistics
        && aggregate.children.len() == 1
        && !aggregate.is_distinct()
        && child_stats.first().is_some_and(|stats| !stats.can_have_null())
    {
        let count_star = bind_aggregate_function(propagator.optimizer.context(), "count_star", Vec::new())?;
        if let ExpressionKind::Aggregate(count_star) = count_star.kind {
            aggregate.function = count_star.function;
            aggregate.bind_info = count_star.bind_info;
            aggregate.children.clear();
        }
    }
    Ok(None)
}

/// The value of an integer bound.
fn integer(value: NumericValue) -> Option<i128> {
    match value {
        NumericValue::I8(v) => Some(i128::from(v)),
        NumericValue::I16(v) => Some(i128::from(v)),
        NumericValue::I32(v) => Some(i128::from(v)),
        NumericValue::I64(v) => Some(i128::from(v)),
        _ => None,
    }
}

/// The bound of a value of `physical` type, if it fits in `range`.
fn bound(physical: PhysicalType, range: (i128, i128), value: i128) -> Option<NumericValue> {
    if value < range.0 || value > range.1 {
        return None;
    }
    Some(match physical {
        PhysicalType::Int8 => NumericValue::I8(i8::try_from(value).ok()?),
        PhysicalType::Int16 => NumericValue::I16(i16::try_from(value).ok()?),
        PhysicalType::Int32 => NumericValue::I32(i32::try_from(value).ok()?),
        PhysicalType::Int64 => NumericValue::I64(i64::try_from(value).ok()?),
        _ => return None,
    })
}

/// The range of results the arithmetic of `ty` accepts without an overflow error:
/// the physical type's range, or for decimals the values of its maximum width
/// (DuckDB's `TryDecimalAdd`/`TryDecimalMultiply` bounds).
fn result_range(ty: &LogicalType) -> Option<(PhysicalType, (i128, i128))> {
    let physical = ty.physical_type();
    let range = match (ty.id(), physical) {
        (LogicalTypeId::Decimal, PhysicalType::Int16) => (-9999, 9999),
        (LogicalTypeId::Decimal, PhysicalType::Int32) => (-999_999_999, 999_999_999),
        (LogicalTypeId::Decimal, PhysicalType::Int64) => (-999_999_999_999_999_999, 999_999_999_999_999_999),
        (LogicalTypeId::Decimal, _) => return None,
        (_, PhysicalType::Int8) => (i128::from(i8::MIN), i128::from(i8::MAX)),
        (_, PhysicalType::Int16) => (i128::from(i16::MIN), i128::from(i16::MAX)),
        (_, PhysicalType::Int32) => (i128::from(i32::MIN), i128::from(i32::MAX)),
        (_, PhysicalType::Int64) => (i128::from(i64::MIN), i128::from(i64::MAX)),
        _ => return None,
    };
    Some((physical, range))
}

/// Sets the range of `result`, unknown without `bounds`.
fn set_bounds(
    result: &mut BaseStatistics,
    physical: PhysicalType,
    range: (i128, i128),
    bounds: Option<(i128, i128)>,
) {
    let (min, max) = bounds.map_or((None, None), |(min, max)| {
        (bound(physical, range, min), bound(physical, range, max))
    });
    set_min(result, min);
    set_max(result, max);
}

/// Whether the arithmetic overload has DuckDB's statistics callback: integral
/// `T op T -> T`, or decimals below HUGEINT.
fn has_arithmetic_statistics(expression: &Expression) -> bool {
    let ExpressionKind::Function(function) = &expression.kind else {
        return false;
    };
    let ty = &expression.return_type;
    let integral = matches!(
        ty.id(),
        LogicalTypeId::TinyInt | LogicalTypeId::SmallInt | LogicalTypeId::Integer | LogicalTypeId::BigInt
    );
    let decimal = ty.id() == LogicalTypeId::Decimal && ty.physical_type() != PhysicalType::Int128;
    if decimal {
        return function
            .children
            .iter()
            .all(|child| child.return_type.id() == LogicalTypeId::Decimal);
    }
    integral && function.children.iter().all(|child| &child.return_type == ty)
}

/// DuckDB's `PropagateNumericStats` for `+`, `-` and `*`: the result range, unknown
/// when the operation could overflow.
fn propagate_arithmetic(expression: &Expression, child_stats: &[BaseStatistics]) -> Option<BaseStatistics> {
    let ExpressionKind::Function(function) = &expression.kind else {
        return None;
    };
    if !has_arithmetic_statistics(expression) {
        return None;
    }
    let (physical, range) = result_range(&expression.return_type)?;
    let (left, right) = (&child_stats[0], &child_stats[1]);
    let mut new_bounds = None;
    if has_min_max(left) && has_min_max(right) {
        let values = (
            numeric_min(left).and_then(integer),
            numeric_max(left).and_then(integer),
            numeric_min(right).and_then(integer),
            numeric_max(right).and_then(integer),
        );
        let (Some(lmin), Some(lmax), Some(rmin), Some(rmax)) = values else {
            return None;
        };
        let within = |value: i128| value >= range.0 && value <= range.1;
        new_bounds = match &*function.function.name {
            "+" => {
                let (min, max) = (lmin + rmin, lmax + rmax);
                (within(min) && within(max)).then_some((min, max))
            }
            "-" => {
                let (min, max) = (lmin - rmax, lmax - rmin);
                (within(min) && within(max)).then_some((min, max))
            }
            _ => {
                let products = [lmin * rmin, lmin * rmax, lmax * rmin, lmax * rmax];
                products.iter().all(|&product| within(product)).then(|| {
                    let min = products.iter().copied().min().unwrap_or_default();
                    let max = products.iter().copied().max().unwrap_or_default();
                    (min, max)
                })
            }
        };
    }
    let mut result = BaseStatistics::create_empty(expression.return_type.clone());
    set_bounds(&mut result, physical, range, new_bounds);
    result.set_validity(
        left.can_have_null() || right.can_have_null(),
        left.can_have_no_null() || right.can_have_no_null(),
    );
    Some(result)
}

/// DuckDB's `PropagateAbsStats`: `abs` of non-negative values is removed.
fn propagate_abs(expression: &mut Expression, child_stats: &[BaseStatistics]) -> Option<BaseStatistics> {
    let ty = expression.return_type.clone();
    if !matches!(
        ty.id(),
        LogicalTypeId::TinyInt | LogicalTypeId::SmallInt | LogicalTypeId::Integer | LogicalTypeId::BigInt
    ) {
        return None;
    }
    let (physical, range) = result_range(&ty)?;
    let stats = &child_stats[0];
    let mut new_bounds = None;
    if has_min_max(stats) {
        let (Some(current_min), Some(current_max)) = (
            numeric_min(stats).and_then(integer),
            numeric_max(stats).and_then(integer),
        ) else {
            return None;
        };
        if current_min != range.0 {
            if current_min < 0 && current_max < 0 {
                new_bounds = Some((current_max.abs(), current_min.abs()));
            } else if current_min < 0 {
                new_bounds = Some((0, current_min.abs().max(current_max)));
            } else {
                // abs of non-negative values is a no-op.
                if let ExpressionKind::Function(function) = &mut expression.kind
                    && let Some(child) = function.children.pop()
                {
                    *expression = child;
                }
                return Some(stats.clone());
            }
        }
    }
    let mut result = BaseStatistics::create_empty(ty);
    set_bounds(&mut result, physical, range, new_bounds);
    result.set_validity(stats.can_have_null(), stats.can_have_no_null());
    Some(result)
}
