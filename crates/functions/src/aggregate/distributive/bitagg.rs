//! `bit_and`, `bit_or` and `bit_xor` over integers and BIT strings (DuckDB's
//! `bitagg.cpp`).

use std::marker::PhantomData;
use std::ops::{BitAnd, BitOr, BitXor};

use rustdb_common::Result;
use rustdb_function::aggregate::{Aggregate, AggregateInput, States, Typed, Unary, UnaryAggregate};
use rustdb_function::{AggregateFunction, AggregateFunctionSet};
use rustdb_types::{LogicalType, NativeType, StringT, Vector};

use crate::bitstring::{BitwiseOp, bitwise_in_place};

/// An integer type the bitwise aggregates accept.
trait Bits: NativeType + BitAnd<Output = Self> + BitOr<Output = Self> + BitXor<Output = Self> {}
impl<T: NativeType + BitAnd<Output = T> + BitOr<Output = T> + BitXor<Output = T>> Bits for T {}

/// The first value is assigned, later values are combined with `OP`.
struct BitAgg<T, const OP: u8>(PhantomData<fn() -> T>);

const AND: u8 = 0;
const OR: u8 = 1;
const XOR: u8 = 2;

impl<T: Bits, const OP: u8> BitAgg<T, OP> {
    #[inline]
    fn apply(left: T, right: T) -> T {
        match OP {
            AND => left & right,
            OR => left | right,
            _ => left ^ right,
        }
    }
}

impl<T: Bits, const OP: u8> UnaryAggregate for BitAgg<T, OP> {
    type Input = T;
    type State = Option<T>;
    type Output = T;

    #[inline]
    fn update(state: &mut Option<T>, value: T) -> Result<()> {
        *state = Some(match *state {
            Some(current) => Self::apply(current, value),
            None => value,
        });
        Ok(())
    }

    fn update_repeated(state: &mut Option<T>, value: T, count: usize) -> Result<()> {
        // AND/OR are idempotent; XOR applies the value once per row (DuckDB's
        // `ConstantOperation`).
        let times = if OP == XOR { count } else { 1 };
        for _ in 0..times {
            Self::update(state, value)?;
        }
        Ok(())
    }

    fn combine(source: &Option<T>, target: &mut Option<T>) -> Result<()> {
        if let Some(value) = *source {
            Self::update(target, value)?;
        }
        Ok(())
    }

    fn finalize(state: &Option<T>) -> Result<Option<T>> {
        Ok(*state)
    }
}

/// The bitwise aggregates over BIT strings (DuckDB's `BitStringBitwiseOperation`).
struct BitStringAgg {
    op: BitwiseOp,
}

impl BitStringAgg {
    fn offer(&self, state: &mut Option<Vec<u8>>, value: &[u8]) -> Result<()> {
        match state {
            Some(current) => bitwise_in_place(self.op, current, value),
            None => {
                *state = Some(value.to_vec());
                Ok(())
            }
        }
    }
}

impl Aggregate for BitStringAgg {
    type State = Option<Vec<u8>>;

    fn initialize(&self) -> Self::State {
        None
    }

    fn update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        states: &mut States<'_, Self::State>,
        count: usize,
    ) -> Result<()> {
        let input = &inputs[0];
        let values = input.unified::<StringT>();
        // A constant input is applied once (AND/OR) or once per row (XOR).
        let rows = if input.is_constant() && self.op != BitwiseOp::Xor && states.is_single() {
            count.min(1)
        } else {
            count
        };
        for row in 0..rows {
            if let Some(value) = values.get(row) {
                self.offer(states.get(row), input.string_value(&value))?;
            }
        }
        Ok(())
    }

    fn combine(&self, source: &Self::State, target: &mut Self::State, _: AggregateInput<'_>) -> Result<()> {
        if let Some(value) = source {
            self.offer(target, value)?;
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
        match state {
            Some(value) => {
                let string = result.add_string(value);
                result.values_mut::<StringT>()[row] = string;
            }
            None => {
                let capacity = result.capacity();
                result.validity_mut().set_invalid(row, capacity);
            }
        }
        Ok(())
    }
}

fn integral<T: Bits, const OP: u8>(name: &str, ty: LogicalType) -> AggregateFunction {
    AggregateFunction::new(name, vec![ty.clone()], ty, Unary::<BitAgg<T, OP>>::default())
}

fn bit_set<const OP: u8>(name: &str, op: BitwiseOp) -> AggregateFunctionSet {
    let mut set = AggregateFunctionSet::new(name);
    set.add(integral::<i8, OP>(name, LogicalType::TINYINT));
    set.add(integral::<i16, OP>(name, LogicalType::SMALLINT));
    set.add(integral::<i32, OP>(name, LogicalType::INTEGER));
    set.add(integral::<i64, OP>(name, LogicalType::BIGINT));
    set.add(integral::<i128, OP>(name, LogicalType::HUGEINT));
    set.add(integral::<u8, OP>(name, LogicalType::UTINYINT));
    set.add(integral::<u16, OP>(name, LogicalType::USMALLINT));
    set.add(integral::<u32, OP>(name, LogicalType::UINTEGER));
    set.add(integral::<u64, OP>(name, LogicalType::UBIGINT));
    set.add(integral::<u128, OP>(name, LogicalType::UHUGEINT));
    set.add(AggregateFunction::new(
        name,
        vec![LogicalType::BIT],
        LogicalType::BIT,
        Typed(BitStringAgg { op }),
    ));
    set
}

/// The `bit_and` set.
pub fn bit_and_functions() -> AggregateFunctionSet {
    bit_set::<AND>("bit_and", BitwiseOp::And)
}

/// The `bit_or` set.
pub fn bit_or_functions() -> AggregateFunctionSet {
    bit_set::<OR>("bit_or", BitwiseOp::Or)
}

/// The `bit_xor` set.
pub fn bit_xor_functions() -> AggregateFunctionSet {
    bit_set::<XOR>("bit_xor", BitwiseOp::Xor)
}
