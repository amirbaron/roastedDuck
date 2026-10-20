//! `bitstring_agg(x[, min, max])` (DuckDB's `bitstring_agg.cpp`): a BIT string with one
//! bit per value of `[min, max]`, set for every value seen.
//!
//! Without explicit bounds DuckDB takes them from the column statistics; rustdb does not
//! propagate statistics to aggregates yet, so that form fails like DuckDB does when no
//! statistics are available.

use std::any::Any;
use std::fmt::Display;
use std::marker::PhantomData;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::{Aggregate, AggregateInput, States, Typed};
use rustdb_function::{AggregateFunction, AggregateFunctionSet, BindInput, FunctionData};
use rustdb_types::{LogicalType, NativeType, StringT, Value, Vector};

use crate::bitstring::{BitwiseOp, bitwise_in_place, set_bit, zeros};

/// DuckDB's `MAX_BIT_RANGE`.
const MAX_BIT_RANGE: u64 = 1_000_000_000;

/// The explicit bounds of a call (DuckDB's `BitstringAggBindData`).
#[derive(Debug)]
struct Bounds(Option<(Value, Value)>);

impl FunctionData for Bounds {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An integral input type of `bitstring_agg`.
trait BitInput: NativeType + Copy + PartialOrd + Display + Send + Sync {
    /// The value of a constant bound.
    fn from_value(value: &Value) -> Result<Self>;
    /// DuckDB's `GetRange`: the number of bits for `[min, max]`, saturating at `u64::MAX`.
    fn range(min: Self, max: Self) -> u64;
    /// The bit index of `input` (DuckDB's `Execute`).
    fn offset(input: Self, min: Self) -> Result<u64>;
}

macro_rules! small_input {
    ($($ty:ty),*) => {$(
        impl BitInput for $ty {
            fn from_value(value: &Value) -> Result<Self> {
                value
                    .as_i128()
                    .and_then(|value| <$ty>::try_from(value).ok())
                    .ok_or_else(|| Error::conversion(format!("Could not convert bitstring_agg bound to {}", stringify!($ty))))
            }

            fn range(min: Self, max: Self) -> u64 {
                match max.checked_sub(min) {
                    // The difference is non-negative here and fits in 64 bits.
                    Some(difference) => {
                        let difference = difference as u64;
                        if difference == u64::MAX { difference } else { difference + 1 }
                    }
                    None => u64::MAX,
                }
            }

            fn offset(input: Self, min: Self) -> Result<u64> {
                Ok((i128::from(input) - i128::from(min)) as u64)
            }
        }
    )*};
}
small_input!(i8, i16, i32, i64, u8, u16, u32, u64);

impl BitInput for i128 {
    fn from_value(value: &Value) -> Result<Self> {
        value
            .as_i128()
            .ok_or_else(|| Error::conversion("Could not convert bitstring_agg bound to HUGEINT"))
    }

    fn range(min: Self, max: Self) -> u64 {
        match max.checked_sub(min) {
            Some(difference) if difference != i128::MAX => u64::try_from(difference + 1).unwrap_or(u64::MAX),
            _ => u64::MAX,
        }
    }

    fn offset(input: Self, min: Self) -> Result<u64> {
        input
            .checked_sub(min)
            .and_then(|difference| u64::try_from(difference).ok())
            .ok_or_else(|| Error::out_of_range("Range too large for bitstring aggregation"))
    }
}

impl BitInput for u128 {
    fn from_value(value: &Value) -> Result<Self> {
        match value.as_i128() {
            Some(value) => u128::try_from(value).ok(),
            None => value.to_varchar().ok().and_then(|text| text.parse().ok()),
        }
        .ok_or_else(|| Error::conversion("Could not convert bitstring_agg bound to UHUGEINT"))
    }

    fn range(min: Self, max: Self) -> u64 {
        match max.checked_sub(min) {
            Some(difference) if difference != u128::MAX => u64::try_from(difference + 1).unwrap_or(u64::MAX),
            _ => u64::MAX,
        }
    }

    fn offset(input: Self, min: Self) -> Result<u64> {
        input
            .checked_sub(min)
            .and_then(|difference| u64::try_from(difference).ok())
            .ok_or_else(|| Error::out_of_range("Range too large for bitstring aggregation"))
    }
}

/// The bits so far and the bounds (DuckDB's `BitAggState`).
struct BitAggState<T> {
    bits: Vec<u8>,
    min: T,
}

struct BitstringAgg<T>(PhantomData<T>);

impl<T: BitInput + 'static> BitstringAgg<T> {
    /// The first value's state: the empty bit string of the range.
    fn start(bounds: &Bounds) -> Result<BitAggState<T>> {
        let Some((min, max)) = &bounds.0 else {
            return Err(Error::binder(
                "Could not retrieve required statistics. Alternatively, try by providing the statistics explicitly: \
                 BITSTRING_AGG(col, min, max) ",
            ));
        };
        let (min, max) = (T::from_value(min)?, T::from_value(max)?);
        if min > max {
            return Err(Error::invalid_input(format!(
                "Invalid explicit bitstring range: Minimum ({min}) > maximum ({max})"
            )));
        }
        let range = T::range(min, max);
        if range > MAX_BIT_RANGE {
            return Err(Error::out_of_range(format!(
                "The range between min and max value ({min} <-> {max}) is too large for bitstring aggregation"
            )));
        }
        Ok(BitAggState {
            bits: zeros(range as usize),
            min,
        })
    }

    fn bounds(bounds: &Bounds) -> Result<(T, T)> {
        match &bounds.0 {
            Some((min, max)) => Ok((T::from_value(min)?, T::from_value(max)?)),
            None => Err(Error::internal("bitstring_agg state without bounds")),
        }
    }
}

impl<T: BitInput + 'static> Aggregate for BitstringAgg<T> {
    type State = Option<BitAggState<T>>;

    fn initialize(&self) -> Self::State {
        None
    }

    fn update(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        states: &mut States<'_, Self::State>,
        count: usize,
    ) -> Result<()> {
        let bounds = input.bind_data::<Bounds>()?;
        let values = inputs[0].unified::<T>();
        for row in 0..count {
            let Some(value) = values.get(row) else {
                continue;
            };
            let state = states.get(row);
            if state.is_none() {
                *state = Some(Self::start(bounds)?);
            }
            let (min, max) = Self::bounds(bounds)?;
            if value < min || value > max {
                return Err(Error::out_of_range(format!(
                    "Value {value} is outside of provided min and max range ({min} <-> {max})"
                )));
            }
            if let Some(state) = state {
                set_bit(&mut state.bits, T::offset(value, state.min)? as usize, true);
            }
        }
        Ok(())
    }

    fn combine(&self, source: &Self::State, target: &mut Self::State, _: AggregateInput<'_>) -> Result<()> {
        let Some(source) = source else {
            return Ok(());
        };
        match target {
            None => {
                *target = Some(BitAggState {
                    bits: source.bits.clone(),
                    min: source.min,
                });
            }
            Some(target) => bitwise_in_place(BitwiseOp::Or, &mut target.bits, &source.bits)?,
        }
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut Self::State,
        _: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        if let Some(state) = state {
            let string = result.add_string(&state.bits);
            result.values_mut::<StringT>()[row] = string;
        } else {
            result.set_null(row);
        }
        Ok(())
    }
}

/// DuckDB's `BindBitstringAgg`: constant bounds are taken out of the arguments.
fn bind_bitstring_agg(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    if input.argument_types.len() == 3 {
        let (Some(min), Some(max)) = (&input.constants[1], &input.constants[2]) else {
            return Err(Error::binder(
                "bitstring_agg requires a constant min and max argument",
            ));
        };
        let bounds = Bounds(Some((min.clone(), max.clone())));
        function.arguments.truncate(1);
        return Ok(Some(Arc::new(bounds)));
    }
    Ok(Some(Arc::new(Bounds(None))))
}

fn overloads<T: BitInput + 'static>(set: &mut AggregateFunctionSet, ty: &LogicalType) {
    let function = AggregateFunction::new(
        "bitstring_agg",
        vec![ty.clone()],
        LogicalType::BIT,
        Typed(BitstringAgg::<T>(PhantomData)),
    )
    .with_bind(bind_bitstring_agg)
    // `BitstringPropagateStats`: the argument's statistics give the bounds.
    .with_statistics(|_, arguments| {
        let (min, max) = arguments.first()?.clone()?;
        Some(Arc::new(Bounds(Some((min, max)))) as Arc<dyn FunctionData>)
    });
    let mut bounded = function.clone();
    bounded.arguments = vec![ty.clone(), ty.clone(), ty.clone()];
    // Min and max are given as arguments.
    bounded.statistics = None;
    set.add(function);
    set.add(bounded);
}

/// `bitstring_agg` over every integral type.
pub fn bitstring_agg_functions() -> AggregateFunctionSet {
    let mut set = AggregateFunctionSet::new("bitstring_agg");
    overloads::<i8>(&mut set, &LogicalType::TINYINT);
    overloads::<i16>(&mut set, &LogicalType::SMALLINT);
    overloads::<i32>(&mut set, &LogicalType::INTEGER);
    overloads::<i64>(&mut set, &LogicalType::BIGINT);
    overloads::<i128>(&mut set, &LogicalType::HUGEINT);
    overloads::<u8>(&mut set, &LogicalType::UTINYINT);
    overloads::<u16>(&mut set, &LogicalType::USMALLINT);
    overloads::<u32>(&mut set, &LogicalType::UINTEGER);
    overloads::<u64>(&mut set, &LogicalType::UBIGINT);
    overloads::<u128>(&mut set, &LogicalType::UHUGEINT);
    set
}
