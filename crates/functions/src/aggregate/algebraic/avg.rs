//! `avg`/`mean` and `favg` (DuckDB's `avg.cpp`).

use std::marker::PhantomData;
use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_function::aggregate::{AggregateInput, Unary, UnaryAggregate};
use roastedduck_function::cast::numeric::i128_to_f64;
use roastedduck_function::{AggregateFunction, AggregateFunctionSet, BindInput, FunctionData};
use roastedduck_types::{Interval, LogicalType, LogicalTypeId, NativeType, PhysicalType, TimeTz};

use crate::aggregate::distributive::sum::{
    hugeint_add, hugeint_multiply, kahan_add, wrapping_sum_i64, wrapping_sum_i128,
};
use crate::operator::{interval_add, interval_multiply};
use crate::temporal::time_tz_normalized_micros;

/// The divisor applied to DECIMAL averages: `10^scale` (DuckDB's `AverageDecimalBindData`).
#[derive(Debug, Clone, Copy, PartialEq)]
struct DecimalScale(f64);

impl FunctionData for DecimalScale {
    fn equals(&self, other: &dyn FunctionData) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self == other)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// `count * 10^scale` for DECIMAL averages, `count` otherwise (`GetAverageDivident`).
fn divident(count: u64, input: AggregateInput<'_>) -> f64 {
    let count = count as f64;
    match input
        .bind_data
        .and_then(|data| data.as_any().downcast_ref::<DecimalScale>())
    {
        Some(scale) => count * scale.0,
        None => count,
    }
}

/// Match the reference target's long-double intermediate without enlarging the state.
fn wide_average(sum: i128, count: u64, input: AggregateInput<'_>) -> f64 {
    let scale = input
        .bind_data
        .and_then(|data| data.as_any().downcast_ref::<DecimalScale>())
        .map(|scale| scale.0);
    super::avg_precision::average(sum, count, scale)
}

/// Count and running sum (DuckDB's `AvgState`), at most 8-byte aligned like DuckDB's (see
/// `SumState`).
#[derive(Clone, Copy, Debug, Default)]
#[repr(C, packed(8))]
struct AvgState<T> {
    count: u64,
    value: T,
}

/// SMALLINT (and narrow DECIMAL) averages, summed in 64 bits
/// (DuckDB's `IntegerAverageOperation`).
struct SmallIntegerAverage<I>(PhantomData<fn() -> I>);

impl<I: NativeType + Into<i64>> UnaryAggregate for SmallIntegerAverage<I> {
    type Input = I;
    type State = AvgState<i64>;
    type Output = f64;

    #[inline]
    fn update(state: &mut AvgState<i64>, value: I) -> Result<()> {
        state.count += 1;
        state.value = state.value.wrapping_add(value.into());
        Ok(())
    }

    fn update_values(state: &mut AvgState<i64>, values: &[I]) -> Result<()> {
        state.count += values.len() as u64;
        state.value = state.value.wrapping_add(wrapping_sum_i64(values));
        Ok(())
    }

    fn update_repeated(state: &mut AvgState<i64>, value: I, count: usize) -> Result<()> {
        state.count += count as u64;
        state.value = state.value.wrapping_add(value.into().wrapping_mul(count as i64));
        Ok(())
    }

    fn combine(source: &AvgState<i64>, target: &mut AvgState<i64>) -> Result<()> {
        target.count += source.count;
        target.value = target.value.wrapping_add(source.value);
        Ok(())
    }

    fn finalize(_: &AvgState<i64>) -> Result<Option<f64>> {
        Err(Error::internal("average finalized without input"))
    }

    fn finalize_with(state: &AvgState<i64>, input: AggregateInput<'_>) -> Result<Option<f64>> {
        if state.count == 0 {
            return Ok(None);
        }
        Ok(Some(state.value as f64 / divident(state.count, input)))
    }
}

/// INTEGER/BIGINT (and DECIMAL) averages, summed into 128 bits
/// (DuckDB's `IntegerAverageOperationHugeint`).
struct IntegerAverage<I>(PhantomData<fn() -> I>);

impl<I: NativeType + Into<i128>> UnaryAggregate for IntegerAverage<I> {
    type Input = I;
    type State = AvgState<i128>;
    type Output = f64;

    #[inline]
    fn update(state: &mut AvgState<i128>, value: I) -> Result<()> {
        state.count += 1;
        state.value = state.value.wrapping_add(value.into());
        Ok(())
    }

    fn update_values(state: &mut AvgState<i128>, values: &[I]) -> Result<()> {
        state.count += values.len() as u64;
        state.value = state.value.wrapping_add(wrapping_sum_i128(values));
        Ok(())
    }

    fn update_repeated(state: &mut AvgState<i128>, value: I, count: usize) -> Result<()> {
        state.count += count as u64;
        state.value = state.value.wrapping_add(value.into().wrapping_mul(count as i128));
        Ok(())
    }

    fn combine(source: &AvgState<i128>, target: &mut AvgState<i128>) -> Result<()> {
        target.count += source.count;
        target.value = target.value.wrapping_add(source.value);
        Ok(())
    }

    fn finalize(_: &AvgState<i128>) -> Result<Option<f64>> {
        Err(Error::internal("average finalized without input"))
    }

    fn finalize_with(state: &AvgState<i128>, input: AggregateInput<'_>) -> Result<Option<f64>> {
        if state.count == 0 {
            return Ok(None);
        }
        Ok(Some(wide_average(state.value, state.count, input)))
    }
}

/// HUGEINT averages with overflow checks (DuckDB's `HugeintAverageOperation`).
struct HugeintAverage;

impl UnaryAggregate for HugeintAverage {
    type Input = i128;
    type State = AvgState<i128>;
    type Output = f64;

    #[inline]
    fn update(state: &mut AvgState<i128>, value: i128) -> Result<()> {
        state.count += 1;
        state.value = hugeint_add(state.value, value)?;
        Ok(())
    }

    fn update_repeated(state: &mut AvgState<i128>, value: i128, count: usize) -> Result<()> {
        state.count += count as u64;
        state.value = hugeint_add(state.value, hugeint_multiply(value, count as i128)?)?;
        Ok(())
    }

    fn combine(source: &AvgState<i128>, target: &mut AvgState<i128>) -> Result<()> {
        target.count += source.count;
        target.value = target.value.wrapping_add(source.value);
        Ok(())
    }

    fn finalize(_: &AvgState<i128>) -> Result<Option<f64>> {
        Err(Error::internal("average finalized without input"))
    }

    fn finalize_with(state: &AvgState<i128>, input: AggregateInput<'_>) -> Result<Option<f64>> {
        if state.count == 0 {
            return Ok(None);
        }
        Ok(Some(wide_average(state.value, state.count, input)))
    }
}

/// DOUBLE averages (DuckDB's `NumericAverageOperation`).
struct DoubleAverage;

impl UnaryAggregate for DoubleAverage {
    type Input = f64;
    type State = AvgState<f64>;
    type Output = f64;

    #[inline]
    fn update(state: &mut AvgState<f64>, value: f64) -> Result<()> {
        state.count += 1;
        state.value += value;
        Ok(())
    }

    fn update_repeated(state: &mut AvgState<f64>, value: f64, count: usize) -> Result<()> {
        state.count += count as u64;
        state.value = value.mul_add(count as i64 as f64, state.value);
        Ok(())
    }

    fn combine(source: &AvgState<f64>, target: &mut AvgState<f64>) -> Result<()> {
        target.count += source.count;
        target.value += source.value;
        Ok(())
    }

    fn finalize(state: &AvgState<f64>) -> Result<Option<f64>> {
        Ok((state.count > 0).then(|| state.value / state.count as f64))
    }
}

/// Kahan-compensated average (`favg`, DuckDB's `KahanAverageOperation`).
#[derive(Clone, Copy, Debug, Default)]
struct KahanAvgState {
    count: u64,
    value: f64,
    error: f64,
}

struct KahanAverage;

impl UnaryAggregate for KahanAverage {
    type Input = f64;
    type State = KahanAvgState;
    type Output = f64;

    #[inline]
    fn update(state: &mut KahanAvgState, value: f64) -> Result<()> {
        state.count += 1;
        kahan_add(value, &mut state.value, &mut state.error);
        Ok(())
    }

    fn update_repeated(state: &mut KahanAvgState, value: f64, count: usize) -> Result<()> {
        state.count += count as u64;
        kahan_add(value * count as f64, &mut state.value, &mut state.error);
        Ok(())
    }

    fn combine(source: &KahanAvgState, target: &mut KahanAvgState) -> Result<()> {
        target.count += source.count;
        kahan_add(source.value, &mut target.value, &mut target.error);
        kahan_add(source.error, &mut target.value, &mut target.error);
        Ok(())
    }

    fn finalize(state: &KahanAvgState) -> Result<Option<f64>> {
        if state.count == 0 {
            return Ok(None);
        }
        let count = state.count as f64;
        Ok(Some((state.value / count) + (state.error / count)))
    }
}

/// INTERVAL averages (DuckDB's `IntervalAverageOperation`).
struct IntervalAverage;

impl UnaryAggregate for IntervalAverage {
    type Input = Interval;
    type State = AvgState<Interval>;
    type Output = Interval;

    fn update(state: &mut AvgState<Interval>, value: Interval) -> Result<()> {
        state.count += 1;
        state.value = interval_add(state.value, value)?;
        Ok(())
    }

    fn update_repeated(state: &mut AvgState<Interval>, value: Interval, count: usize) -> Result<()> {
        state.count += count as u64;
        let total = interval_multiply(value, count as i64)?;
        state.value = interval_add(state.value, total)?;
        Ok(())
    }

    fn combine(source: &AvgState<Interval>, target: &mut AvgState<Interval>) -> Result<()> {
        target.count += source.count;
        target.value = interval_add(target.value, source.value)?;
        Ok(())
    }

    fn finalize(state: &AvgState<Interval>) -> Result<Option<Interval>> {
        if state.count == 0 {
            return Ok(None);
        }
        let value = state.value;
        let count = state.count as i64;
        let months = i64::from(value.months);
        let days = i64::from(value.days);
        let mut target_days = days / count;
        let mut days_remainder = days % count;
        let months_remainder = (months % count) * Interval::DAYS_PER_MONTH;
        target_days += months_remainder / count;
        days_remainder += months_remainder % count;
        days_remainder *= Interval::MICROS_PER_DAY;
        let micros = value.micros / count + (value.micros % count + days_remainder / count);
        Ok(Some(Interval {
            months: (months / count) as i32,
            days: target_days as i32,
            micros,
        }))
    }
}

/// TIMESTAMP/TIME averages: the rounded integer mean (DuckDB's `DiscreteAverageOperation`).
struct DiscreteAverage;

impl UnaryAggregate for DiscreteAverage {
    type Input = i64;
    type State = AvgState<i128>;
    type Output = i64;

    #[inline]
    fn update(state: &mut AvgState<i128>, value: i64) -> Result<()> {
        state.count += 1;
        state.value = state.value.wrapping_add(i128::from(value));
        Ok(())
    }

    fn update_repeated(state: &mut AvgState<i128>, value: i64, count: usize) -> Result<()> {
        state.count += count as u64;
        state.value = state
            .value
            .wrapping_add(i128::from(value).wrapping_mul(count as i128));
        Ok(())
    }

    fn combine(source: &AvgState<i128>, target: &mut AvgState<i128>) -> Result<()> {
        target.count += source.count;
        target.value = target.value.wrapping_add(source.value);
        Ok(())
    }

    fn finalize(state: &AvgState<i128>) -> Result<Option<i64>> {
        if state.count == 0 {
            return Ok(None);
        }
        let count = i128::from(state.count);
        let quotient = state.value / count;
        let remainder = state.value % count;
        let rounded = quotient + i128::from(remainder > count / 2);
        i64::try_from(rounded)
            .map(Some)
            .map_err(|_| Error::out_of_range(format!("Failed to cast from hugeint to int64: {rounded}")))
    }
}

/// TIME WITH TIME ZONE averages over the UTC-normalized times (DuckDB's
/// `TimeTZAverageOperation`); the result has offset 0.
struct TimeTzAverage;

impl UnaryAggregate for TimeTzAverage {
    type Input = TimeTz;
    type State = AvgState<i128>;
    type Output = TimeTz;

    #[inline]
    fn update(state: &mut AvgState<i128>, value: TimeTz) -> Result<()> {
        DiscreteAverage::update(state, time_tz_normalized_micros(value))
    }

    fn update_repeated(state: &mut AvgState<i128>, value: TimeTz, count: usize) -> Result<()> {
        DiscreteAverage::update_repeated(state, time_tz_normalized_micros(value), count)
    }

    fn combine(source: &AvgState<i128>, target: &mut AvgState<i128>) -> Result<()> {
        DiscreteAverage::combine(source, target)
    }

    fn finalize(state: &AvgState<i128>) -> Result<Option<TimeTz>> {
        Ok(DiscreteAverage::finalize(state)?.map(|micros| TimeTz::new(roastedduck_types::Time(micros), 0)))
    }
}

fn average<A: UnaryAggregate>(input: LogicalType, output: LogicalType) -> AggregateFunction {
    AggregateFunction::new("avg", vec![input], output, Unary::<A>::default())
}

/// The average of a physical input type (DuckDB's `GetAverageAggregate`).
fn average_aggregate(physical: PhysicalType, input: LogicalType) -> Result<AggregateFunction> {
    Ok(match physical {
        PhysicalType::Int16 => average::<SmallIntegerAverage<i16>>(input, LogicalType::DOUBLE),
        PhysicalType::Int32 => average::<IntegerAverage<i32>>(input, LogicalType::DOUBLE),
        PhysicalType::Int64 => average::<IntegerAverage<i64>>(input, LogicalType::DOUBLE),
        PhysicalType::Int128 => average::<HugeintAverage>(input, LogicalType::DOUBLE),
        PhysicalType::Interval => average::<IntervalAverage>(input, LogicalType::INTERVAL),
        _ => return Err(Error::internal("Unimplemented average aggregate")),
    })
}

/// DuckDB's `BindDecimalAvg`: averages the unscaled values and divides by `10^scale`.
fn bind_decimal_avg(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let decimal_type = input.argument_types[0].clone();
    let scale = decimal_type.decimal_properties().map_or(0, |(_, scale)| scale);
    let mut bound = average_aggregate(decimal_type.physical_type(), decimal_type)?;
    bound.return_type = LogicalType::DOUBLE;
    function.replace_with(bound);
    Ok(Some(Arc::new(DecimalScale(i128_to_f64(
        10i128.pow(u32::from(scale)),
    )))))
}

/// The `avg` set (alias `mean`).
pub fn avg_functions() -> Result<AggregateFunctionSet> {
    let mut set = AggregateFunctionSet::new("avg");
    set.add(AggregateFunction::unbound(
        vec![LogicalType::simple(LogicalTypeId::Decimal)],
        LogicalType::simple(LogicalTypeId::Decimal),
        bind_decimal_avg,
    ));
    set.add(average_aggregate(PhysicalType::Int16, LogicalType::SMALLINT)?);
    set.add(average_aggregate(PhysicalType::Int32, LogicalType::INTEGER)?);
    set.add(average_aggregate(PhysicalType::Int64, LogicalType::BIGINT)?);
    set.add(average_aggregate(PhysicalType::Int128, LogicalType::HUGEINT)?);
    set.add(average_aggregate(PhysicalType::Interval, LogicalType::INTERVAL)?);
    set.add(average::<DoubleAverage>(LogicalType::DOUBLE, LogicalType::DOUBLE));
    set.add(average::<DiscreteAverage>(
        LogicalType::TIMESTAMP,
        LogicalType::TIMESTAMP,
    ));
    set.add(average::<DiscreteAverage>(
        LogicalType::TIMESTAMP_TZ,
        LogicalType::TIMESTAMP_TZ,
    ));
    set.add(average::<DiscreteAverage>(LogicalType::TIME, LogicalType::TIME));
    set.add(average::<TimeTzAverage>(
        LogicalType::TIME_TZ,
        LogicalType::TIME_TZ,
    ));
    Ok(set.renamed("avg"))
}

/// `favg(DOUBLE) -> DOUBLE`.
pub fn favg_functions() -> AggregateFunctionSet {
    AggregateFunctionSet::single(
        "favg",
        AggregateFunction::new(
            "favg",
            vec![LogicalType::DOUBLE],
            LogicalType::DOUBLE,
            Unary::<KahanAverage>::default(),
        ),
    )
}
