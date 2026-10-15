//! The bitwise operators `&`, `|`, `xor`, `~`, `<<` and `>>` on integers and BIT strings
//! (DuckDB's `extension/core_functions/scalar/operators/bitwise.cpp`).

use std::fmt::Display;

use rustdb_common::{Error, Result};
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::bit::bit_length;
use rustdb_types::{DataChunk, LogicalType, NativeType, Vector};

use crate::bitstring::{self, BitwiseOp};
use crate::scalar::executor::{Str, StrOut, binary, unary};

/// The integral types in DuckDB's `LogicalType::Integral()` order.
fn integral_types() -> [LogicalType; 10] {
    [
        LogicalType::TINYINT,
        LogicalType::SMALLINT,
        LogicalType::INTEGER,
        LogicalType::BIGINT,
        LogicalType::HUGEINT,
        LogicalType::UTINYINT,
        LogicalType::USMALLINT,
        LogicalType::UINTEGER,
        LogicalType::UBIGINT,
        LogicalType::UHUGEINT,
    ]
}

/// An integer type the bitwise operators apply to.
trait BitwiseInt: NativeType + Display + Ord {
    const BITS: u32;
    const SIGNED: bool;
    const ZERO: Self;
    const ONE: Self;
    fn and(self, other: Self) -> Self;
    fn or(self, other: Self) -> Self;
    fn xor(self, other: Self) -> Self;
    fn not(self) -> Self;
    fn shl(self, shift: u32) -> Self;
    fn shr(self, shift: u32) -> Self;
    /// The value as a shift amount (saturating above `i128::MAX`).
    fn as_shift(self) -> i128;
}

macro_rules! bitwise_int {
    ($($ty:ty),*) => {$(
        impl BitwiseInt for $ty {
            const BITS: u32 = <$ty>::BITS;
            const SIGNED: bool = <$ty>::MIN != 0;
            const ZERO: Self = 0;
            const ONE: Self = 1;
            #[inline]
            fn and(self, other: Self) -> Self { self & other }
            #[inline]
            fn or(self, other: Self) -> Self { self | other }
            #[inline]
            fn xor(self, other: Self) -> Self { self ^ other }
            #[inline]
            fn not(self) -> Self { !self }
            #[inline]
            fn shl(self, shift: u32) -> Self { self.checked_shl(shift).unwrap_or(0) }
            #[inline]
            fn shr(self, shift: u32) -> Self { self.checked_shr(shift).unwrap_or(0) }
            #[inline]
            #[allow(clippy::cast_lossless, reason = "uniform conversion for every integer type")]
            fn as_shift(self) -> i128 { i128::try_from(self).unwrap_or(i128::MAX) }
        }
    )*};
}
bitwise_int!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128);

/// `input << shift` with DuckDB's range checks (`BitwiseShiftLeftOperator`).
fn shift_left<T: BitwiseInt>(input: T, shift: T) -> Result<T> {
    let max_shift = i128::from(T::BITS) + i128::from(!T::SIGNED);
    if input < T::ZERO {
        return Err(Error::out_of_range(format!(
            "Cannot left-shift negative number {input}"
        )));
    }
    let amount = shift.as_shift();
    if amount < 0 {
        return Err(Error::out_of_range(format!(
            "Cannot left-shift by negative number {shift}"
        )));
    }
    if amount >= max_shift {
        if input == T::ZERO {
            return Ok(T::ZERO);
        }
        return Err(Error::out_of_range(format!(
            "Left-shift value {shift} is out of range"
        )));
    }
    if amount == 0 {
        return Ok(input);
    }
    // `amount` is in `1..max_shift`, so both shifts fit in u32.
    let amount = u32::try_from(amount).map_err(|_| Error::internal("shift out of range"))?;
    let exponent = u32::try_from(max_shift).map_err(|_| Error::internal("shift out of range"))? - amount - 1;
    let max_value = T::ONE.shl(exponent);
    if input >= max_value {
        return Err(Error::out_of_range(format!(
            "Overflow in left shift ({input} << {shift})"
        )));
    }
    Ok(input.shl(amount))
}

/// `input >> shift`, 0 for shifts outside `0..bits` (`BitwiseShiftRightOperator`).
fn shift_right<T: BitwiseInt>(input: T, shift: T) -> T {
    let amount = shift.as_shift();
    if (0..i128::from(T::BITS)).contains(&amount) {
        // In range of u32 by the check above.
        input.shr(u32::try_from(amount).unwrap_or(0))
    } else {
        T::ZERO
    }
}

/// Integer overloads `name(T, T) -> T` for every integral type.
fn integer_binary_overloads(set: &mut ScalarFunctionSet, op: IntegerOp) {
    for ty in integral_types() {
        let name = set.name.clone();
        let function = crate::dispatch::with_integer_type!(
            ty.physical_type(),
            T => ScalarFunction::new(name, vec![ty.clone(), ty.clone()], ty.clone(), move |args, _, result| {
                binary::<T, T, T>(args, result, |left, right, out| {
                    *out = match op {
                        IntegerOp::And => left.and(right),
                        IntegerOp::Or => left.or(right),
                        IntegerOp::Xor => left.xor(right),
                        IntegerOp::ShiftLeft => shift_left(left, right)?,
                        IntegerOp::ShiftRight => shift_right(left, right),
                    };
                    Ok(true)
                })
            }),
            _ => continue,
        );
        set.add(function);
    }
}

#[derive(Clone, Copy, Debug)]
enum IntegerOp {
    And,
    Or,
    Xor,
    ShiftLeft,
    ShiftRight,
}

/// `&`, `|` or `xor` over two BIT strings of equal length.
fn bit_binary_kernel(op: BitwiseOp, args: &DataChunk, result: &mut Vector) -> Result<()> {
    binary::<Str, Str, StrOut>(args, result, |left, right, out| {
        out.extend_from_slice(left);
        bitstring::bitwise_in_place(op, out, right)?;
        Ok(true)
    })
}

fn binary_set(name: &str, integer: IntegerOp, bit: BitwiseOp) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    integer_binary_overloads(&mut set, integer);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::BIT, LogicalType::BIT],
        LogicalType::BIT,
        move |args, _, result| bit_binary_kernel(bit, args, result),
    ));
    set
}

/// `&` (DuckDB's `BitwiseAndFun`).
pub fn bitwise_and_functions() -> ScalarFunctionSet {
    binary_set("&", IntegerOp::And, BitwiseOp::And)
}

/// `|` (DuckDB's `BitwiseOrFun`).
pub fn bitwise_or_functions() -> ScalarFunctionSet {
    binary_set("|", IntegerOp::Or, BitwiseOp::Or)
}

/// `xor` (DuckDB's `BitwiseXorFun`).
pub fn bitwise_xor_functions() -> ScalarFunctionSet {
    binary_set("xor", IntegerOp::Xor, BitwiseOp::Xor)
}

/// `~` (DuckDB's `BitwiseNotFun`).
pub fn bitwise_not_functions() -> ScalarFunctionSet {
    const NAME: &str = "~";
    let mut set = ScalarFunctionSet::new(NAME);
    for ty in integral_types() {
        let function = crate::dispatch::with_integer_type!(
            ty.physical_type(),
            T => ScalarFunction::new(NAME, vec![ty.clone()], ty.clone(), |args, _, result| {
                unary::<T, T>(args, result, |value, out| {
                    *out = value.not();
                    Ok(true)
                })
            }),
            _ => continue,
        );
        set.add(function);
    }
    set.add(ScalarFunction::new(
        NAME,
        vec![LogicalType::BIT],
        LogicalType::BIT,
        |args, _, result| {
            unary::<Str, StrOut>(args, result, |bits, out| {
                out.extend_from_slice(&bitstring::bitwise_not(bits));
                Ok(true)
            })
        },
    ));
    set
}

/// `bits << shift` on a BIT string (DuckDB's `BitwiseShiftLeftOperation`).
fn bit_shift_left(bits: &[u8], shift: i32, out: &mut Vec<u8>) -> Result<()> {
    if shift == 0 {
        out.extend_from_slice(bits);
        return Ok(());
    }
    let Ok(amount) = usize::try_from(shift) else {
        return Err(Error::out_of_range(format!(
            "Cannot left-shift by negative number {shift}"
        )));
    };
    if amount >= bit_length(bits) {
        out.extend_from_slice(&bitstring::zeros(bit_length(bits)));
    } else {
        out.extend_from_slice(&bitstring::left_shift(bits, amount));
    }
    Ok(())
}

/// `bits >> shift` on a BIT string (DuckDB's `BitwiseShiftRightOperation`).
fn bit_shift_right(bits: &[u8], shift: i32, out: &mut Vec<u8>) {
    if shift == 0 {
        out.extend_from_slice(bits);
        return;
    }
    match usize::try_from(shift) {
        Ok(amount) if amount < bit_length(bits) => {
            out.extend_from_slice(&bitstring::right_shift(bits, amount))
        }
        _ => out.extend_from_slice(&bitstring::zeros(bit_length(bits))),
    }
}

fn shift_set(name: &str, integer: IntegerOp) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    integer_binary_overloads(&mut set, integer);
    let left = matches!(integer, IntegerOp::ShiftLeft);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::BIT, LogicalType::INTEGER],
        LogicalType::BIT,
        move |args, _, result| {
            binary::<Str, i32, StrOut>(args, result, |bits, shift, out| {
                if left {
                    bit_shift_left(bits, shift, out)?;
                } else {
                    bit_shift_right(bits, shift, out);
                }
                Ok(true)
            })
        },
    ));
    set
}

/// `<<` (DuckDB's `LeftShiftFun`).
pub fn left_shift_functions() -> ScalarFunctionSet {
    shift_set("<<", IntegerOp::ShiftLeft)
}

/// `>>` (DuckDB's `RightShiftFun`).
pub fn right_shift_functions() -> ScalarFunctionSet {
    shift_set(">>", IntegerOp::ShiftRight)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_match_duckdb() {
        assert_eq!(shift_left(1u8, 7).ok(), Some(128));
        assert!(shift_left(2u8, 8).is_err());
        assert_eq!(shift_left(0i32, 100).ok(), Some(0));
        assert!(shift_left(1i32, 31).is_err());
        assert!(shift_left(-1i32, 1).is_err());
        assert_eq!(shift_right(-8i64, 1), -4);
        assert_eq!(shift_right(-8i64, 70), 0);
        assert_eq!(shift_left(1i128, 126).ok(), Some(1i128 << 126));
    }
}
