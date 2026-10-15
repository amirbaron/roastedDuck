//! Overflow-checked arithmetic with DuckDB's error messages (DuckDB's
//! `common/operator/{add,subtract,multiply}.hpp` and the interval operators in
//! `function/scalar/operator`), shared by the arithmetic functions and aggregates.

use std::fmt::Display;

use rustdb_common::{Error, Result};
use rustdb_types::{Interval, NativeType};

/// An integer type with checked arithmetic.
pub(crate) trait CheckedInt: NativeType + Display + Ord {
    const ZERO: Self;
    fn checked_add_(self, other: Self) -> Option<Self>;
    fn checked_sub_(self, other: Self) -> Option<Self>;
    fn checked_mul_(self, other: Self) -> Option<Self>;
}

macro_rules! checked_int {
    ($($ty:ty),*) => {$(
        impl CheckedInt for $ty {
            const ZERO: Self = 0;
            #[inline]
            fn checked_add_(self, other: Self) -> Option<Self> { self.checked_add(other) }
            #[inline]
            fn checked_sub_(self, other: Self) -> Option<Self> { self.checked_sub(other) }
            #[inline]
            fn checked_mul_(self, other: Self) -> Option<Self> { self.checked_mul(other) }
        }
    )*};
}
checked_int!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128);

/// `Overflow in addition of INT32 (2147483647 + 1)!`
pub(crate) fn add_overflow<T: NativeType + Display>(left: T, right: T) -> Error {
    Error::out_of_range(format!(
        "Overflow in addition of {} ({left} + {right})!",
        T::PHYSICAL.name()
    ))
}

/// `Overflow in subtraction of INT32 (a - b)!`
pub(crate) fn subtract_overflow<T: NativeType + Display>(left: T, right: T) -> Error {
    Error::out_of_range(format!(
        "Overflow in subtraction of {} ({left} - {right})!",
        T::PHYSICAL.name()
    ))
}

/// `Overflow in multiplication of INT32 (a * b)!`
pub(crate) fn multiply_overflow<T: NativeType + Display>(left: T, right: T) -> Error {
    Error::out_of_range(format!(
        "Overflow in multiplication of {} ({left} * {right})!",
        T::PHYSICAL.name()
    ))
}

/// DuckDB's `AddOperatorOverflowCheck`.
#[inline]
pub(crate) fn checked_add<T: CheckedInt>(left: T, right: T) -> Result<T> {
    left.checked_add_(right).ok_or_else(|| add_overflow(left, right))
}

/// DuckDB's `SubtractOperatorOverflowCheck`.
#[inline]
pub(crate) fn checked_subtract<T: CheckedInt>(left: T, right: T) -> Result<T> {
    left.checked_sub_(right)
        .ok_or_else(|| subtract_overflow(left, right))
}

/// DuckDB's `MultiplyOperatorOverflowCheck`.
#[inline]
pub(crate) fn checked_multiply<T: CheckedInt>(left: T, right: T) -> Result<T> {
    left.checked_mul_(right)
        .ok_or_else(|| multiply_overflow(left, right))
}

/// `interval + interval`, field by field (DuckDB's `AddOperator` on intervals).
pub(crate) fn interval_add(left: Interval, right: Interval) -> Result<Interval> {
    Ok(Interval {
        months: checked_add(left.months, right.months)?,
        days: checked_add(left.days, right.days)?,
        micros: checked_add(left.micros, right.micros)?,
    })
}

/// `interval - interval` (DuckDB's `SubtractOperator` on intervals).
pub(crate) fn interval_subtract(left: Interval, right: Interval) -> Result<Interval> {
    let months = left
        .months
        .checked_sub(right.months)
        .ok_or_else(|| Error::out_of_range("Interval months subtraction out of range"))?;
    let days = left
        .days
        .checked_sub(right.days)
        .ok_or_else(|| Error::out_of_range("Interval days subtraction out of range"))?;
    let micros = left
        .micros
        .checked_sub(right.micros)
        .ok_or_else(|| Error::out_of_range("Interval micros subtraction out of range"))?;
    Ok(Interval { months, days, micros })
}

/// `interval * factor` (DuckDB's `MultiplyOperator<interval_t, int64_t>`).
pub(crate) fn interval_multiply(left: Interval, factor: i64) -> Result<Interval> {
    let factor32 = i32::try_from(factor).map_err(|_| {
        Error::invalid_input(format!(
            "Type INT64 with value {factor} can't be cast because the value is out of range for the destination \
             type INT32"
        ))
    })?;
    Ok(Interval {
        months: checked_multiply(left.months, factor32)?,
        days: checked_multiply(left.days, factor32)?,
        micros: checked_multiply(left.micros, factor)?,
    })
}
