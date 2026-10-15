//! `sum`, `sum_no_overflow`, `kahan_sum`/`fsum` and `count_if` (DuckDB's `sum.cpp` and
//! `sum_helpers.hpp`).

use std::marker::PhantomData;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::{Unary, UnaryAggregate};
use rustdb_function::{AggregateFunction, AggregateFunctionSet, BindInput, FunctionData};
use rustdb_types::{LogicalType, LogicalTypeId, NativeType, PhysicalType};

/// `Overflow in HUGEINT addition: a + b` (DuckDB's `Hugeint::Add`).
pub(crate) fn hugeint_add(left: i128, right: i128) -> Result<i128> {
    left.checked_add(right)
        .ok_or_else(|| Error::out_of_range(format!("Overflow in HUGEINT addition: {left} + {right}")))
}

/// `Overflow in HUGEINT multiplication: a * b` (DuckDB's `Hugeint::Multiply`).
pub(crate) fn hugeint_multiply(left: i128, right: i128) -> Result<i128> {
    left.checked_mul(right)
        .ok_or_else(|| Error::out_of_range(format!("Overflow in HUGEINT multiplication: {left} * {right}")))
}

/// A running sum and whether any value was added (DuckDB's `SumState`).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SumState<T> {
    pub(crate) is_set: bool,
    pub(crate) value: T,
}

/// Sums small integers (BOOLEAN, SMALLINT and narrow DECIMALs) into 64 bits
/// (DuckDB's `IntegerSumOperation`).
struct IntegerSum<I>(PhantomData<fn() -> I>);

impl<I: NativeType + Into<i64>> UnaryAggregate for IntegerSum<I> {
    type Input = I;
    type State = SumState<i64>;
    type Output = i128;

    #[inline]
    fn update(state: &mut SumState<i64>, value: I) -> Result<()> {
        state.is_set = true;
        state.value = state.value.wrapping_add(value.into());
        Ok(())
    }

    fn update_repeated(state: &mut SumState<i64>, value: I, count: usize) -> Result<()> {
        state.is_set = true;
        state.value = state.value.wrapping_add(value.into().wrapping_mul(count as i64));
        Ok(())
    }

    fn combine(source: &SumState<i64>, target: &mut SumState<i64>) -> Result<()> {
        target.is_set |= source.is_set;
        target.value = target.value.wrapping_add(source.value);
        Ok(())
    }

    fn finalize(state: &SumState<i64>) -> Result<Option<i128>> {
        Ok(state.is_set.then_some(i128::from(state.value)))
    }
}

/// Sums INTEGER/BIGINT values into a 128-bit integer without overflow checks
/// (DuckDB's `SumToHugeintOperation` with `AddToHugeint`).
struct SumToHugeint<I>(PhantomData<fn() -> I>);

impl<I: NativeType + Into<i128>> UnaryAggregate for SumToHugeint<I> {
    type Input = I;
    type State = SumState<i128>;
    type Output = i128;

    #[inline]
    fn update(state: &mut SumState<i128>, value: I) -> Result<()> {
        state.is_set = true;
        state.value = state.value.wrapping_add(value.into());
        Ok(())
    }

    fn update_repeated(state: &mut SumState<i128>, value: I, count: usize) -> Result<()> {
        state.is_set = true;
        state.value = state.value.wrapping_add(value.into().wrapping_mul(count as i128));
        Ok(())
    }

    fn combine(source: &SumState<i128>, target: &mut SumState<i128>) -> Result<()> {
        target.is_set |= source.is_set;
        target.value = target.value.wrapping_add(source.value);
        Ok(())
    }

    fn finalize(state: &SumState<i128>) -> Result<Option<i128>> {
        Ok(state.is_set.then_some(state.value))
    }
}

/// Sums HUGEINT values with overflow checks (DuckDB's `HugeintSumOperation`).
struct HugeintSum;

impl UnaryAggregate for HugeintSum {
    type Input = i128;
    type State = SumState<i128>;
    type Output = i128;

    #[inline]
    fn update(state: &mut SumState<i128>, value: i128) -> Result<()> {
        state.is_set = true;
        state.value = hugeint_add(state.value, value)?;
        Ok(())
    }

    fn update_repeated(state: &mut SumState<i128>, value: i128, count: usize) -> Result<()> {
        state.is_set = true;
        let product = hugeint_multiply(value, count as i128)?;
        state.value = hugeint_add(state.value, product)?;
        Ok(())
    }

    fn combine(source: &SumState<i128>, target: &mut SumState<i128>) -> Result<()> {
        target.is_set |= source.is_set;
        target.value = target.value.wrapping_add(source.value);
        Ok(())
    }

    fn finalize(state: &SumState<i128>) -> Result<Option<i128>> {
        Ok(state.is_set.then_some(state.value))
    }
}

/// Sums DOUBLE values in input order (DuckDB's `NumericSumOperation`).
struct DoubleSum;

impl UnaryAggregate for DoubleSum {
    type Input = f64;
    type State = SumState<f64>;
    type Output = f64;

    #[inline]
    fn update(state: &mut SumState<f64>, value: f64) -> Result<()> {
        state.is_set = true;
        state.value += value;
        Ok(())
    }

    fn update_repeated(state: &mut SumState<f64>, value: f64, count: usize) -> Result<()> {
        state.is_set = true;
        state.value = value.mul_add(count as i64 as f64, state.value);
        Ok(())
    }

    fn combine(source: &SumState<f64>, target: &mut SumState<f64>) -> Result<()> {
        target.is_set |= source.is_set;
        target.value += source.value;
        Ok(())
    }

    fn finalize(state: &SumState<f64>) -> Result<Option<f64>> {
        Ok(state.is_set.then_some(state.value))
    }
}

/// One step of Kahan summation (DuckDB's `KahanAddInternal`).
#[inline]
pub(crate) fn kahan_add(input: f64, summed: &mut f64, error: &mut f64) {
    let diff = input - *error;
    let new_value = *summed + diff;
    *error = (new_value - *summed) - diff;
    *summed = new_value;
}

/// A compensated sum (DuckDB's `KahanSumState`).
#[derive(Clone, Copy, Debug, Default)]
struct KahanState {
    is_set: bool,
    value: f64,
    error: f64,
}

/// `kahan_sum`/`fsum` (DuckDB's `KahanSumOperation`).
struct KahanSum;

impl UnaryAggregate for KahanSum {
    type Input = f64;
    type State = KahanState;
    type Output = f64;

    #[inline]
    fn update(state: &mut KahanState, value: f64) -> Result<()> {
        state.is_set = true;
        kahan_add(value, &mut state.value, &mut state.error);
        Ok(())
    }

    fn update_repeated(state: &mut KahanState, value: f64, count: usize) -> Result<()> {
        state.is_set = true;
        kahan_add(value * count as f64, &mut state.value, &mut state.error);
        Ok(())
    }

    fn combine(source: &KahanState, target: &mut KahanState) -> Result<()> {
        target.is_set |= source.is_set;
        kahan_add(source.value, &mut target.value, &mut target.error);
        kahan_add(source.error, &mut target.value, &mut target.error);
        Ok(())
    }

    fn finalize(state: &KahanState) -> Result<Option<f64>> {
        Ok(state.is_set.then_some(state.value))
    }
}

/// The `sum` aggregate of a physical input type (DuckDB's `GetSumAggregate`), with
/// `HUGEINT` result.
fn sum_aggregate(physical: PhysicalType, input: LogicalType) -> Result<AggregateFunction> {
    let ret = LogicalType::HUGEINT;
    let args = vec![input];
    let function = match physical {
        PhysicalType::Bool => AggregateFunction::new("sum", args, ret, Unary::<IntegerSum<bool>>::default()),
        PhysicalType::Int16 => AggregateFunction::new("sum", args, ret, Unary::<IntegerSum<i16>>::default()),
        PhysicalType::Int32 => {
            AggregateFunction::new("sum", args, ret, Unary::<SumToHugeint<i32>>::default())
        }
        PhysicalType::Int64 => {
            AggregateFunction::new("sum", args, ret, Unary::<SumToHugeint<i64>>::default())
        }
        PhysicalType::Int128 => AggregateFunction::new("sum", args, ret, Unary::<HugeintSum>::default()),
        _ => return Err(Error::internal("Unimplemented sum aggregate")),
    };
    Ok(function.order_independent())
}

/// DuckDB's `BindDecimalSum`: sums the unscaled values into `DECIMAL(38, scale)`.
fn bind_decimal_sum(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let decimal_type = input.argument_types[0].clone();
    let scale = decimal_type.decimal_properties().map_or(0, |(_, scale)| scale);
    let mut bound = sum_aggregate(decimal_type.physical_type(), decimal_type)?;
    bound.return_type = LogicalType::decimal(LogicalType::MAX_DECIMAL_WIDTH, scale);
    function.replace_with(bound);
    Ok(None)
}

/// A plain bind callback.
type BindFn = fn(&mut AggregateFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>>;

fn decimal_overload(bind: BindFn) -> AggregateFunction {
    AggregateFunction::unbound(
        vec![LogicalType::simple(LogicalTypeId::Decimal)],
        LogicalType::simple(LogicalTypeId::Decimal),
        bind,
    )
}

/// The `sum` set (DuckDB's `SumFun::GetFunctions`). `sum(BIGNUM)` is not supported yet.
pub fn sum_functions() -> Result<AggregateFunctionSet> {
    let mut set = AggregateFunctionSet::new("sum");
    set.add(decimal_overload(bind_decimal_sum));
    set.add(sum_aggregate(PhysicalType::Bool, LogicalType::BOOLEAN)?);
    set.add(sum_aggregate(PhysicalType::Int16, LogicalType::SMALLINT)?);
    set.add(sum_aggregate(PhysicalType::Int32, LogicalType::INTEGER)?);
    set.add(sum_aggregate(PhysicalType::Int64, LogicalType::BIGINT)?);
    set.add(sum_aggregate(PhysicalType::Int128, LogicalType::HUGEINT)?);
    set.add(AggregateFunction::new(
        "sum",
        vec![LogicalType::DOUBLE],
        LogicalType::DOUBLE,
        Unary::<DoubleSum>::default(),
    ));
    set.add(AggregateFunction::new(
        "sum",
        vec![LogicalType::BIGNUM],
        LogicalType::BIGNUM,
        rustdb_function::aggregate::Typed(BignumSum),
    ));
    Ok(set.renamed("sum"))
}

/// `count_if(BOOLEAN) -> HUGEINT` (DuckDB's `CountIfFun`).
pub fn count_if_functions() -> Result<AggregateFunctionSet> {
    Ok(AggregateFunctionSet::single(
        "count_if",
        sum_aggregate(PhysicalType::Bool, LogicalType::BOOLEAN)?,
    )
    .renamed("count_if"))
}

/// DuckDB's `SumNoOverflowBind`.
fn sum_no_overflow_bind(
    _: &mut AggregateFunction,
    _: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    Err(Error::binder("sum_no_overflow is for internal use only!"))
}

/// `sum_no_overflow`, which the optimizer substitutes for `sum` when statistics prove the
/// sum fits; calling it directly is an error.
pub fn sum_no_overflow_functions() -> AggregateFunctionSet {
    let mut set = AggregateFunctionSet::new("sum_no_overflow");
    for (ty, function) in [
        (
            LogicalType::INTEGER,
            AggregateFunction::new(
                "",
                vec![],
                LogicalType::HUGEINT,
                Unary::<IntegerSum<i32>>::default(),
            ),
        ),
        (
            LogicalType::BIGINT,
            AggregateFunction::new(
                "",
                vec![],
                LogicalType::HUGEINT,
                Unary::<IntegerSum<i64>>::default(),
            ),
        ),
    ] {
        let mut function = function.with_bind(sum_no_overflow_bind).order_independent();
        function.arguments = vec![ty];
        set.add(function);
    }
    set.add(decimal_overload(sum_no_overflow_bind));
    set.renamed("sum_no_overflow")
}

/// `kahan_sum(DOUBLE) -> DOUBLE` (aliases `fsum`, `sumkahan`).
pub fn kahan_sum_functions() -> AggregateFunctionSet {
    AggregateFunctionSet::single(
        "kahan_sum",
        AggregateFunction::new(
            "kahan_sum",
            vec![LogicalType::DOUBLE],
            LogicalType::DOUBLE,
            Unary::<KahanSum>::default(),
        ),
    )
}

/// `sum(BIGNUM)` (DuckDB's `BignumOperation`): exact addition of variable-size integers.
#[derive(Debug, Clone, Copy)]
struct BignumSum;

impl rustdb_function::aggregate::Aggregate for BignumSum {
    type State = Option<rustdb_function::bignum::Intermediate>;

    fn initialize(&self) -> Self::State {
        None
    }

    fn update(
        &self,
        inputs: &[rustdb_types::Vector],
        _: rustdb_function::aggregate::AggregateInput<'_>,
        states: &mut rustdb_function::aggregate::States<'_, Self::State>,
        count: usize,
    ) -> Result<()> {
        use rustdb_function::bignum::Intermediate;
        let vector = &inputs[0];
        let values = vector.unified::<rustdb_types::StringT>();
        for row in 0..count {
            if let Some(value) = values.get(row) {
                let input = Intermediate::from_bignum(vector.string_value(&value));
                states
                    .get(row)
                    .get_or_insert_with(Intermediate::zero)
                    .add_in_place(&input)?;
            }
        }
        Ok(())
    }

    fn combine(
        &self,
        source: &Self::State,
        target: &mut Self::State,
        _: rustdb_function::aggregate::AggregateInput<'_>,
    ) -> Result<()> {
        if let Some(source) = source {
            target
                .get_or_insert_with(rustdb_function::bignum::Intermediate::zero)
                .add_in_place(source)?;
        }
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut Self::State,
        _: rustdb_function::aggregate::AggregateInput<'_>,
        result: &mut rustdb_types::Vector,
        row: usize,
    ) -> Result<()> {
        if let Some(value) = state {
            let string = result.add_string(&value.to_bignum());
            result.values_mut::<rustdb_types::StringT>()[row] = string;
        } else {
            let capacity = result.capacity();
            result.validity_mut().set_invalid(row, capacity);
        }
        Ok(())
    }
}
