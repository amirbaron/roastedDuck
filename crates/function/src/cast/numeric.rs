//! Casts from BOOLEAN, integer and floating point types (DuckDB's `numeric_casts.cpp`,
//! `numeric_cast.hpp` and the `TryCastWithOverflowCheck` family).

use rustdb_types::bit::{self, BitNumeric};
use rustdb_types::format;
use rustdb_types::{LogicalType, LogicalTypeId, NativeType, PhysicalType};

use super::BoundCastInfo;
use super::decimal::{ToDecimal, to_decimal_kernel};
use super::executor::{RowError, try_map_values, values_to_string};

/// A numeric value that takes part in casts.
pub(super) trait CastNumber: NativeType + Copy {
    /// Appends the VARCHAR text of the value (DuckDB's `StringCast`).
    fn write_text(self, out: &mut Vec<u8>);

    /// The text used in error messages (DuckDB's `ConvertToString`).
    fn to_text(self) -> String {
        let mut out = Vec::new();
        self.write_text(&mut out);
        String::from_utf8_lossy(&out).into_owned()
    }
}

macro_rules! cast_number {
    ($($ty:ty => $write:expr),* $(,)?) => {$(
        impl CastNumber for $ty {
            #[inline]
            fn write_text(self, out: &mut Vec<u8>) {
                let write: fn($ty, &mut Vec<u8>) = $write;
                write(self, out);
            }
        }
    )*};
}

cast_number! {
    bool => |v, out| out.extend_from_slice(if v { b"true" } else { b"false" }),
    i8 => |v, out| format::write_i64(v.into(), out),
    i16 => |v, out| format::write_i64(v.into(), out),
    i32 => |v, out| format::write_i64(v.into(), out),
    i64 => format::write_i64,
    i128 => format::write_i128,
    u8 => |v, out| format::write_u64(v.into(), out),
    u16 => |v, out| format::write_u64(v.into(), out),
    u32 => |v, out| format::write_u64(v.into(), out),
    u64 => format::write_u64,
    u128 => format::write_u128,
    f32 => format::write_f32,
    f64 => format::write_f64,
}

/// A checked numeric conversion (DuckDB's `TryCastWithOverflowCheck`).
pub(super) trait TryCastTo<D>: Sized {
    fn try_cast_to(self) -> Option<D>;
}

/// Integer to integer: value-preserving or failing.
macro_rules! int_to_int {
    ($src:ty => $($dst:ty),*) => {$(
        impl TryCastTo<$dst> for $src {
            #[inline]
            fn try_cast_to(self) -> Option<$dst> {
                <$dst>::try_from(self).ok()
            }
        }
    )*};
}

macro_rules! int_casts {
    ($($src:ty),*) => {$(
        int_to_int!($src => i8, i16, i32, i64, i128, u8, u16, u32, u64, u128);
        impl TryCastTo<bool> for $src {
            #[inline]
            fn try_cast_to(self) -> Option<bool> {
                Some(self != 0)
            }
        }
    )*};
}
int_casts!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128);

/// Integers of at most 64 bits convert to floating point with one correct rounding.
macro_rules! small_int_to_float {
    ($($src:ty),*) => {$(
        impl TryCastTo<f32> for $src {
            #[inline]
            fn try_cast_to(self) -> Option<f32> {
                Some(self as f32)
            }
        }
        impl TryCastTo<f64> for $src {
            #[inline]
            fn try_cast_to(self) -> Option<f64> {
                Some(self as f64)
            }
        }
    )*};
}
small_int_to_float!(i8, i16, i32, i64, u8, u16, u32, u64);

/// DuckDB's HUGEINT to DOUBLE conversion, which combines the two 64-bit halves (and so
/// may round twice): `lower + upper * 2^64`, with a special case for small negatives.
pub(super) fn i128_to_f64(value: i128) -> f64 {
    let lower = value as u64;
    let upper = (value >> 64) as i64;
    if upper == -1 {
        -((u64::MAX - lower) as f64) - 1.0
    } else {
        lower as f64 + upper as f64 * (u64::MAX as f64 + 1.0)
    }
}

/// DuckDB's UHUGEINT to DOUBLE conversion: `lower + upper * 2^64`.
pub(super) fn u128_to_f64(value: u128) -> f64 {
    let lower = value as u64;
    let upper = (value >> 64) as u64;
    lower as f64 + upper as f64 * u64::MAX as f64
}

impl TryCastTo<f64> for i128 {
    fn try_cast_to(self) -> Option<f64> {
        Some(i128_to_f64(self))
    }
}

impl TryCastTo<f32> for i128 {
    fn try_cast_to(self) -> Option<f32> {
        // Through DOUBLE, like DuckDB.
        Some(i128_to_f64(self) as f32)
    }
}

impl TryCastTo<f64> for u128 {
    fn try_cast_to(self) -> Option<f64> {
        Some(u128_to_f64(self))
    }
}

impl TryCastTo<f32> for u128 {
    fn try_cast_to(self) -> Option<f32> {
        Some(u128_to_f64(self) as f32)
    }
}

/// Floating point to integer: the value must lie in `[min, max)` and is rounded half to
/// even. Rounding can still reach `max`; the conversion then behaves like the reference
/// build (arm64), which converts through a 32-bit register and truncates for 8/16-bit
/// targets but saturates for wider ones.
macro_rules! float_to_int {
    ($float:ty => $($dst:ty: $min:expr, $max:expr, $via:ty);* $(;)?) => {$(
        impl TryCastTo<$dst> for $float {
            #[inline]
            fn try_cast_to(self) -> Option<$dst> {
                if !($min..$max).contains(&self) {
                    return None;
                }
                Some(self.round_ties_even() as $via as $dst)
            }
        }
    )*};
}

float_to_int!(f32 =>
    i8: -128.0, 128.0, i32;
    i16: -32768.0, 32768.0, i32;
    i32: -2_147_483_648.0, 2_147_483_648.0, i32;
    i64: -9_223_372_036_854_775_808.0, 9_223_372_036_854_775_808.0, i64;
    u8: 0.0, 256.0, u32;
    u16: 0.0, 65536.0, u32;
    u32: 0.0, 4_294_967_296.0, u32;
    u64: 0.0, 18_446_744_073_709_551_616.0, u64;
);

float_to_int!(f64 =>
    i8: -128.0, 128.0, i32;
    i16: -32768.0, 32768.0, i32;
    i32: -2_147_483_648.0, 2_147_483_648.0, i32;
    i64: -9_223_372_036_854_775_808.0, 9_223_372_036_854_775_808.0, i64;
    u8: 0.0, 256.0, u32;
    u16: 0.0, 65536.0, u32;
    u32: 0.0, 4_294_967_296.0, u32;
    u64: 0.0, 18_446_744_073_709_551_615.0, u64;
);

/// DuckDB's `ConvertFloatingToBigint` (after `nearbyint`).
fn f64_to_i128(value: f64) -> Option<i128> {
    if !value.is_finite()
        || value <= -170_141_183_460_469_231_731_687_303_715_884_105_728.0
        || value >= 1.701_411_834_604_692_3e38
    {
        return None;
    }
    let magnitude = value.abs();
    let two_64 = u64::MAX as f64;
    let lower = (magnitude % two_64) as u64;
    let upper = (magnitude / two_64) as i64;
    let result = (i128::from(upper) << 64) | i128::from(lower);
    Some(if value < 0.0 {
        result.wrapping_neg()
    } else {
        result
    })
}

/// DuckDB's `ConvertFloatingToUhugeint` (after `nearbyint`).
fn f64_to_u128(value: f64) -> Option<u128> {
    if !value.is_finite() || value < 0.0 || value >= 3.402_823_669_209_385e38 {
        return None;
    }
    let two_64 = u64::MAX as f64;
    let lower = (value % two_64) as u64;
    let upper = (value / two_64) as u64;
    Some((u128::from(upper) << 64) | u128::from(lower))
}

impl TryCastTo<i128> for f64 {
    fn try_cast_to(self) -> Option<i128> {
        f64_to_i128(self.round_ties_even())
    }
}

impl TryCastTo<i128> for f32 {
    fn try_cast_to(self) -> Option<i128> {
        f64_to_i128(f64::from(self.round_ties_even()))
    }
}

impl TryCastTo<u128> for f64 {
    fn try_cast_to(self) -> Option<u128> {
        f64_to_u128(self.round_ties_even())
    }
}

impl TryCastTo<u128> for f32 {
    fn try_cast_to(self) -> Option<u128> {
        f64_to_u128(f64::from(self.round_ties_even()))
    }
}

impl TryCastTo<bool> for f32 {
    fn try_cast_to(self) -> Option<bool> {
        Some(self != 0.0)
    }
}

impl TryCastTo<bool> for f64 {
    fn try_cast_to(self) -> Option<bool> {
        Some(self != 0.0)
    }
}

impl TryCastTo<f32> for f32 {
    fn try_cast_to(self) -> Option<f32> {
        Some(self)
    }
}

impl TryCastTo<f64> for f64 {
    fn try_cast_to(self) -> Option<f64> {
        Some(self)
    }
}

impl TryCastTo<f64> for f32 {
    fn try_cast_to(self) -> Option<f64> {
        Some(self.into())
    }
}

impl TryCastTo<f32> for f64 {
    /// Finite doubles must stay finite; infinities and NaN pass through.
    fn try_cast_to(self) -> Option<f32> {
        let narrowed = self as f32;
        (!self.is_finite() || narrowed.is_finite()).then_some(narrowed)
    }
}

macro_rules! bool_casts {
    ($($dst:ty),*) => {$(
        impl TryCastTo<$dst> for bool {
            #[inline]
            fn try_cast_to(self) -> Option<$dst> {
                Some(u8::from(self).into())
            }
        }
    )*};
}
bool_casts!(i16, i32, i64, i128, u8, u16, u32, u64, u128, f32, f64);

impl TryCastTo<bool> for bool {
    fn try_cast_to(self) -> Option<bool> {
        Some(self)
    }
}

impl TryCastTo<i8> for bool {
    fn try_cast_to(self) -> Option<i8> {
        Some(i8::from(self))
    }
}

/// `Type INT64 with value 300 can't be cast because the value is out of range for the
/// destination type INT8` (DuckDB's `CastExceptionText` for numbers).
pub(super) fn out_of_range_message<S: CastNumber, D: NativeType>(value: S) -> String {
    format!(
        "Type {} with value {} can't be cast because the value is out of range for the destination type {}",
        S::PHYSICAL.name(),
        value.to_text(),
        D::PHYSICAL.name()
    )
}

/// A numeric source type with every numeric conversion.
pub(super) trait NumericSource:
    CastNumber
    + TryCastTo<bool>
    + TryCastTo<i8>
    + TryCastTo<i16>
    + TryCastTo<i32>
    + TryCastTo<i64>
    + TryCastTo<i128>
    + TryCastTo<u8>
    + TryCastTo<u16>
    + TryCastTo<u32>
    + TryCastTo<u64>
    + TryCastTo<u128>
    + TryCastTo<f32>
    + TryCastTo<f64>
    + ToDecimal
    + BitNumeric
{
}

impl<T> NumericSource for T where
    T: CastNumber
        + TryCastTo<bool>
        + TryCastTo<i8>
        + TryCastTo<i16>
        + TryCastTo<i32>
        + TryCastTo<i64>
        + TryCastTo<i128>
        + TryCastTo<u8>
        + TryCastTo<u16>
        + TryCastTo<u32>
        + TryCastTo<u64>
        + TryCastTo<u128>
        + TryCastTo<f32>
        + TryCastTo<f64>
        + ToDecimal
        + BitNumeric
{
}

/// The kernel converting `S` values to `D` values with overflow checks.
pub(super) fn numeric_kernel<S, D>() -> BoundCastInfo
where
    S: CastNumber + TryCastTo<D>,
    D: NativeType,
{
    BoundCastInfo::new(|source, result, count, params| {
        try_map_values::<S, D>(source, result, count, params, |value| {
            value
                .try_cast_to()
                .ok_or_else(|| RowError::Invalid(out_of_range_message::<S, D>(value)))
        })
    })
}

/// DuckDB's `InternalNumericCastSwitch<SRC>`.
fn switch_from<S: NumericSource>(target: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    Some(match target.id() {
        L::Boolean => numeric_kernel::<S, bool>(),
        L::TinyInt => numeric_kernel::<S, i8>(),
        L::SmallInt => numeric_kernel::<S, i16>(),
        L::Integer => numeric_kernel::<S, i32>(),
        L::BigInt => numeric_kernel::<S, i64>(),
        L::HugeInt => numeric_kernel::<S, i128>(),
        L::UTinyInt => numeric_kernel::<S, u8>(),
        L::USmallInt => numeric_kernel::<S, u16>(),
        L::UInteger => numeric_kernel::<S, u32>(),
        L::UBigInt => numeric_kernel::<S, u64>(),
        L::UHugeInt => numeric_kernel::<S, u128>(),
        L::Float => numeric_kernel::<S, f32>(),
        L::Double => numeric_kernel::<S, f64>(),
        L::Decimal => to_decimal_kernel::<S>(target)?,
        L::Varchar => BoundCastInfo::new(|source, result, count, params| {
            values_to_string::<S>(source, result, count, params, S::write_text)
        }),
        L::Bit => BoundCastInfo::new(|source, result, count, params| {
            values_to_string::<S>(source, result, count, params, |value, out| {
                bit::from_numeric(value, out);
            })
        }),
        _ => return None,
    })
}

/// DuckDB's `NumericCastSwitch`: casts from BOOLEAN, integers, FLOAT and DOUBLE.
pub(super) fn cast_switch(source: &LogicalType, target: &LogicalType) -> Option<BoundCastInfo> {
    match source.physical_type() {
        PhysicalType::Bool => switch_from::<bool>(target),
        PhysicalType::Int8 => switch_from::<i8>(target),
        PhysicalType::Int16 => switch_from::<i16>(target),
        PhysicalType::Int32 => switch_from::<i32>(target),
        PhysicalType::Int64 => switch_from::<i64>(target),
        PhysicalType::Int128 => switch_from::<i128>(target),
        PhysicalType::UInt8 => switch_from::<u8>(target),
        PhysicalType::UInt16 => switch_from::<u16>(target),
        PhysicalType::UInt32 => switch_from::<u32>(target),
        PhysicalType::UInt64 => switch_from::<u64>(target),
        PhysicalType::UInt128 => switch_from::<u128>(target),
        PhysicalType::Float => switch_from::<f32>(target),
        PhysicalType::Double => switch_from::<f64>(target),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_to_int_matches_reference_rounding() {
        assert_eq!(TryCastTo::<i8>::try_cast_to(127.6f32), Some(-128));
        assert_eq!(TryCastTo::<i8>::try_cast_to(127.5f64), Some(-128));
        assert_eq!(TryCastTo::<u8>::try_cast_to(255.7f64), Some(0));
        assert_eq!(TryCastTo::<i32>::try_cast_to(2_147_483_647.6f64), Some(i32::MAX));
        assert_eq!(TryCastTo::<u32>::try_cast_to(4_294_967_295.7f64), Some(u32::MAX));
        assert_eq!(TryCastTo::<i32>::try_cast_to(2.5f64), Some(2));
        assert_eq!(TryCastTo::<i32>::try_cast_to(-2.5f64), Some(-2));
        assert_eq!(TryCastTo::<u8>::try_cast_to(-0.5f64), None);
        assert_eq!(TryCastTo::<i64>::try_cast_to(9.223_372_036_854_776e18_f64), None);
        assert_eq!(TryCastTo::<f32>::try_cast_to(1e39f64), None);
        assert!(TryCastTo::<f32>::try_cast_to(f64::NAN).is_some_and(f32::is_nan));
    }

    #[test]
    fn wide_integer_float_conversions() {
        assert_eq!(i128_to_f64(-1), -1.0);
        assert_eq!(i128_to_f64(1 << 64), 18_446_744_073_709_551_616.0);
        assert_eq!(
            TryCastTo::<i128>::try_cast_to(1e30f64),
            Some(1_000_000_000_000_000_019_884_624_838_656)
        );
        assert_eq!(TryCastTo::<i128>::try_cast_to(-2.5f64), Some(-2));
        assert_eq!(TryCastTo::<i128>::try_cast_to(1.701_411_834_604_692_3e38_f64), None);
        assert_eq!(TryCastTo::<u128>::try_cast_to(-1.0f64), None);
    }

    #[test]
    fn messages_use_physical_type_names() {
        assert_eq!(
            out_of_range_message::<f64, i32>(1e30),
            "Type DOUBLE with value 1e+30 can't be cast because the value is out of range for the destination type INT32"
        );
    }
}
