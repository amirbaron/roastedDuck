//! The statistics callbacks of functions (DuckDB's `FunctionStatisticsInput` callbacks,
//! e.g. `PropagateNumericStats`, `PropagateAbsStats`, `CountPropagateStats`), selected
//! by the bound function's name and signature.

use std::sync::Arc;

use rustdb_common::Result;
use rustdb_function::{FunctionState, ScalarKernel};
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_storage::statistics::{BaseStatistics, NumericValue};
use rustdb_types::kernels::binary;
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, PhysicalType, Value, Vector};

use super::{StatisticsPropagator, greater, has_min_max, numeric_max, numeric_min, set_max, set_min};
use crate::expression_util::try_evaluate_scalar;
use crate::optimizer::bind_aggregate_function;
use rustdb_function::cast::ValueCast;

/// The statistics of a scalar function call, possibly simplifying it.
#[allow(clippy::unnecessary_wraps)] // callbacks may bind replacement functions
pub(super) fn propagate_scalar_function(
    propagator: &mut StatisticsPropagator<'_>,
    expression: &mut Expression,
    child_stats: &[BaseStatistics],
) -> Result<Option<BaseStatistics>> {
    let ExpressionKind::Function(function) = &expression.kind else {
        return Ok(None);
    };
    match &*function.function.name {
        "+" | "-" | "*" if child_stats.len() == 2 => Ok(propagate_arithmetic(expression, child_stats)),
        "abs" | "@" if child_stats.len() == 1 => Ok(propagate_abs(expression, child_stats)),
        name if child_stats.len() == 1 && date_part_kind(name).is_some() => Ok(date_part_kind(name)
            .and_then(|kind| propagate_date_part(propagator, expression, kind, 0, child_stats))),
        // DuckDB binds `date_part('epoch' | 'julian', x)` as `epoch(x)` / `julian(x)`,
        // which have statistics.
        "date_part" if child_stats.len() == 2 => {
            let specifier = function
                .children
                .first()
                .and_then(Expression::as_constant)
                .and_then(|value| value.as_str().map(str::to_lowercase));
            Ok(match specifier.as_deref() {
                Some("epoch" | "julian" | "jd") => propagate_date_part(
                    propagator,
                    expression,
                    DatePartKind::Monotonic { double: true },
                    1,
                    child_stats,
                ),
                _ => None,
            })
        }
        "struct_extract" | "array_extract" if child_stats.len() == 2 => {
            Ok(propagate_struct_extract(expression, child_stats))
        }
        "date_trunc" if child_stats.len() == 2 => {
            Ok(propagate_date_trunc(propagator, expression, child_stats))
        }
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

/// How a date part function propagates statistics.
#[derive(Clone, Copy)]
enum DatePartKind {
    /// Monotonic in its input (`PropagateDatePartStatistics`): the part of the minimum
    /// and of the maximum; DOUBLE parts are typed DOUBLE.
    Monotonic { double: bool },
    /// Within fixed bounds (`PropagateSimpleDatePartStatistics`); `time` if TIME inputs
    /// propagate too.
    Bounded { min: i64, max: i64, time: bool },
}

/// The statistics behavior of the date part function `name` (DuckDB's
/// `PropagateStatistics` of each `DatePart` operator).
fn date_part_kind(name: &str) -> Option<DatePartKind> {
    use DatePartKind::{Bounded, Monotonic};
    let bounded = |min, max| Bounded {
        min,
        max,
        time: false,
    };
    let time = |min, max| Bounded { min, max, time: true };
    Some(match name {
        "year" | "decade" | "century" | "millennium" | "isoyear" | "yearweek" | "epoch_ns" | "epoch_us"
        | "epoch_ms" => Monotonic { double: false },
        "epoch" | "julian" => Monotonic { double: true },
        "month" => bounded(1, 12),
        "day" | "dayofmonth" => bounded(1, 31),
        "quarter" => bounded(1, 4),
        "dayofweek" | "weekday" => bounded(0, 6),
        "isodow" => bounded(1, 7),
        "dayofyear" => bounded(1, 366),
        "week" | "weekofyear" => bounded(1, 54),
        "era" => bounded(0, 1),
        "timezone" | "timezone_hour" | "timezone_minute" => bounded(0, 0),
        "nanosecond" => time(0, 60_000_000_000),
        "microsecond" => time(0, 60_000_000),
        "millisecond" => time(0, 60_000),
        "second" | "minute" => time(0, 60),
        "hour" => time(0, 24),
        _ => return None,
    })
}

/// Whether a DATE/TIMESTAMP bound is finite (`Value::IsFinite`); TIME bounds always are.
fn is_finite_bound(value: NumericValue) -> bool {
    match value {
        NumericValue::I32(v) => v != i32::MAX && v != -i32::MAX,
        NumericValue::I64(v) => v != i64::MAX && v != -i64::MAX,
        _ => true,
    }
}

/// DuckDB's date part statistics callbacks, for DATE and TIMESTAMP (and, for time
/// parts, TIME) inputs with finite bounds; the input is argument `input_index`.
fn propagate_date_part(
    propagator: &StatisticsPropagator<'_>,
    expression: &Expression,
    kind: DatePartKind,
    input_index: usize,
    child_stats: &[BaseStatistics],
) -> Option<BaseStatistics> {
    let ExpressionKind::Function(function) = &expression.kind else {
        return None;
    };
    let child = child_stats.get(input_index)?;
    let input = function.children.get(input_index)?;
    let temporal = matches!(
        input.return_type.id(),
        LogicalTypeId::Date | LogicalTypeId::Timestamp
    );
    let time = matches!(
        input.return_type.id(),
        LogicalTypeId::Time | LogicalTypeId::TimeNs | LogicalTypeId::TimeTz
    );
    if !(temporal || (time && matches!(kind, DatePartKind::Bounded { time: true, .. }))) {
        return None;
    }
    if !has_min_max(child) {
        return None;
    }
    let (min, max) = (numeric_min(child)?, numeric_max(child)?);
    if greater(min, max) || !is_finite_bound(min) || !is_finite_bound(max) {
        return None;
    }
    let mut result = match kind {
        DatePartKind::Bounded { min, max, .. } => {
            let mut stats = BaseStatistics::create_empty(LogicalType::BIGINT);
            set_min(&mut stats, Some(NumericValue::I64(min)));
            set_max(&mut stats, Some(NumericValue::I64(max)));
            stats
        }
        DatePartKind::Monotonic { double } => {
            let part = |bound: NumericValue| {
                let mut call = expression.clone();
                if let ExpressionKind::Function(call) = &mut call.kind
                    && let Some(argument) = call.children.get_mut(input_index)
                {
                    *argument = Expression::constant(bound.to_value(&input.return_type));
                }
                try_evaluate_scalar(propagator.optimizer.context(), &call).and_then(|value| {
                    let target = if double {
                        LogicalType::DOUBLE
                    } else {
                        LogicalType::BIGINT
                    };
                    value.default_cast_as(&target).ok()
                })
            };
            let (min_part, max_part) = (part(min)?, part(max)?);
            let stats_type = if double {
                LogicalType::DOUBLE
            } else {
                LogicalType::BIGINT
            };
            let mut stats = BaseStatistics::create_empty(stats_type);
            set_min(&mut stats, NumericValue::from_value(&min_part));
            set_max(&mut stats, NumericValue::from_value(&max_part));
            stats
        }
    };
    // `CopyValidity`.
    result.set_validity(child.can_have_null(), child.can_have_no_null());
    Some(result)
}

/// DuckDB's `PropagateStructExtractStats`: the statistics of the field a key extract
/// (`struct_extract(s, 'name')`, also bound for `s['name']`) reads.
fn propagate_struct_extract(
    expression: &Expression,
    child_stats: &[BaseStatistics],
) -> Option<BaseStatistics> {
    let ExpressionKind::Function(function) = &expression.kind else {
        return None;
    };
    let [input, key] = function.children.as_slice() else {
        return None;
    };
    if function.function.arguments.get(1)?.id() != LogicalTypeId::Varchar {
        return None;
    }
    let key = key.as_constant()?.as_str()?.to_lowercase();
    let index = input
        .return_type
        .struct_children()?
        .iter()
        .position(|(name, _)| name.to_lowercase() == key)?;
    child_stats.first()?.struct_children()?.get(index).cloned()
}

/// DuckDB's `DateTruncStatistics`: `date_trunc(part, x)` over a DATE or TIMESTAMP with a
/// constant part (the callback `DateTruncBind` installs) is monotonic, so truncating the
/// input's bounds bounds the result; infinite bounds pass through the function's cast.
fn propagate_date_trunc(
    propagator: &StatisticsPropagator<'_>,
    expression: &Expression,
    child_stats: &[BaseStatistics],
) -> Option<BaseStatistics> {
    let ExpressionKind::Function(function) = &expression.kind else {
        return None;
    };
    let [part_stats, child] = child_stats else {
        return None;
    };
    let [part, input] = function.children.as_slice() else {
        return None;
    };
    if part.as_constant().is_none_or(Value::is_null)
        || !matches!(
            input.return_type.id(),
            LogicalTypeId::Date | LogicalTypeId::Timestamp
        )
        || !has_min_max(child)
    {
        return None;
    }
    let (min, max) = (numeric_min(child)?, numeric_max(child)?);
    if greater(min, max) {
        return None;
    }
    let truncate = |bound: NumericValue| {
        let mut call = expression.clone();
        if let ExpressionKind::Function(call) = &mut call.kind
            && let Some(argument) = call.children.get_mut(1)
        {
            *argument = Expression::constant(bound.to_value(&input.return_type));
        }
        try_evaluate_scalar(propagator.optimizer.context(), &call)
    };
    let (min_part, max_part) = (truncate(min)?, truncate(max)?);
    let mut result = BaseStatistics::create_empty(expression.return_type.clone());
    set_min(&mut result, NumericValue::from_value(&min_part));
    set_max(&mut result, NumericValue::from_value(&max_part));
    // `CombineValidity`.
    result.set_validity(
        part_stats.can_have_null() || child.can_have_null(),
        part_stats.can_have_no_null() || child.can_have_no_null(),
    );
    Some(result)
}

/// Bind data marking an aggregate bound without DuckDB's statistics callback (e.g. the
/// `CountFunctionBase` COUNT the sum rewriter adds).
#[derive(Debug)]
pub struct WithoutStatistics;

impl rustdb_function::FunctionData for WithoutStatistics {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    /// DuckDB has no bind data here (the missing statistics callback is not part of
    /// `AggregateFunction::operator==`), so such aggregates are equal to each other and
    /// `CommonAggregateOptimizer` computes them once.
    fn equals(&self, other: &dyn rustdb_function::FunctionData) -> bool {
        other.as_any().is::<Self>()
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
    // DuckDB's `SumPropagateStats`: a sum of 32/64-bit integers that cannot leave the
    // BIGINT range is summed in 64 bits.
    if &*aggregate.function.name == "sum"
        && let Some(max_cardinality) = propagator.node_stats.and_then(|stats| stats.max_cardinality)
        && let Some(stats) = child_stats.first()
        && let Some(argument) = sum_no_overflow_argument(stats, max_cardinality)
    {
        // The sum keeps its result type (DECIMAL sums store their HUGEINT as a DECIMAL).
        let return_type = aggregate.function.return_type.clone();
        aggregate.function = sum_no_overflow(propagator, &argument)?;
        aggregate.function.return_type = return_type;
    }
    Ok(None)
}

/// The argument type of the `sum_no_overflow` overload replacing a sum over values with
/// statistics `stats` and at most `max_cardinality` rows, if the sum fits in a BIGINT.
fn sum_no_overflow_argument(stats: &BaseStatistics, max_cardinality: u64) -> Option<LogicalType> {
    let numeric = stats.numeric().filter(|numeric| numeric.has_min_max())?;
    let (min, max, argument) = match (numeric.min?, numeric.max?) {
        (NumericValue::I32(min), NumericValue::I32(max)) => {
            (i128::from(min), i128::from(max), LogicalType::INTEGER)
        }
        (NumericValue::I64(min), NumericValue::I64(max)) => {
            (i128::from(min), i128::from(max), LogicalType::BIGINT)
        }
        _ => return None,
    };
    let rows = i128::from(max_cardinality);
    let max_sum_negative = min.checked_mul(rows)?;
    let max_sum_positive = max.checked_mul(rows)?;
    (max_sum_positive < i128::from(i64::MAX) && max_sum_negative > i128::from(i64::MIN)).then_some(argument)
}

/// The `sum_no_overflow` overload for `argument` (DuckDB's `GetSumAggregateNoOverflow`;
/// it refuses to bind from SQL).
fn sum_no_overflow(
    propagator: &StatisticsPropagator<'_>,
    argument: &LogicalType,
) -> Result<rustdb_function::AggregateFunction> {
    let entry = propagator
        .optimizer
        .context()
        .get_entry(
            "system",
            "main",
            rustdb_catalog::CatalogType::AggregateFunction,
            "sum_no_overflow",
            true,
        )?
        .ok_or_else(|| rustdb_common::Error::internal("aggregate function sum_no_overflow not found"))?;
    let rustdb_catalog::EntryKind::AggregateFunction(functions) = &entry.entry.kind else {
        return Err(rustdb_common::Error::internal(
            "sum_no_overflow is not an aggregate function",
        ));
    };
    functions
        .functions
        .iter()
        .find(|function| function.arguments.as_slice() == std::slice::from_ref(argument))
        .cloned()
        .ok_or_else(|| rustdb_common::Error::internal("Unsupported internal type for sum_no_overflow"))
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
fn propagate_arithmetic(
    expression: &mut Expression,
    child_stats: &[BaseStatistics],
) -> Option<BaseStatistics> {
    if !has_arithmetic_statistics(expression) {
        return None;
    }
    let return_type = expression.return_type.clone();
    let ExpressionKind::Function(function) = &mut expression.kind else {
        return None;
    };
    let (physical, range) = result_range(&return_type)?;
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
    if new_bounds.is_some()
        && let Some(kernel) = unchecked_kernel(&function.function.name, physical)
    {
        // No overflow is possible: the plain operator replaces the overflow-checking one
        // (`SetFunctionCallback(GetScalarIntegerFunction<OP>(...))`).
        function.function.kernel = kernel;
    }
    let mut result = BaseStatistics::create_empty(return_type);
    set_bounds(&mut result, physical, range, new_bounds);
    result.set_validity(
        left.can_have_null() || right.can_have_null(),
        left.can_have_no_null() || right.can_have_no_null(),
    );
    Some(result)
}

/// The kernel of `+`, `-` or `*` over two columns of `physical` without overflow checks
/// (DuckDB's `GetScalarIntegerFunction<OP>`).
fn unchecked_kernel(name: &str, physical: PhysicalType) -> Option<ScalarKernel> {
    macro_rules! kernel {
        ($t:ty, $op:ident) => {{
            let kernel: ScalarKernel =
                Arc::new(|args: &DataChunk, _: &mut FunctionState, result: &mut Vector| {
                    binary::<$t, $t, $t>(args.column(0), args.column(1), args.len(), result, |l, r| {
                        l.$op(r)
                    });
                    Ok(())
                });
            Some(kernel)
        }};
    }
    macro_rules! by_type {
        ($op:ident) => {
            match physical {
                PhysicalType::Int8 => kernel!(i8, $op),
                PhysicalType::Int16 => kernel!(i16, $op),
                PhysicalType::Int32 => kernel!(i32, $op),
                PhysicalType::Int64 => kernel!(i64, $op),
                _ => None,
            }
        };
    }
    match name {
        "+" => by_type!(wrapping_add),
        "-" => by_type!(wrapping_sub),
        "*" => by_type!(wrapping_mul),
        _ => None,
    }
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
