//! Casts to and from DECIMAL (DuckDB's `decimal_cast.cpp` and the decimal operators of
//! `cast_operators.cpp`).
//!
//! DECIMAL values are stored as INT16/INT32/INT64/INT128 depending on the width; the
//! arithmetic here works on the unscaled value `to_i128ed` to `i128`, which gives the same
//! results as DuckDB's per-width templates since no intermediate can overflow.

use rustdb_types::format;
use rustdb_types::{LogicalType, LogicalTypeId, NativeType, PhysicalType};

use super::BoundCastInfo;
use super::executor::{RowError, RowResult, addressable, cast_rows, try_map_strings, values_to_string};
use super::numeric::CastNumber;
use super::parse::parse_decimal;

/// `10^exponent` for DECIMAL arithmetic (`exponent <= 38`).
fn pow10(exponent: u8) -> i128 {
    10i128.pow(u32::from(exponent))
}

/// `NumericHelper::DOUBLE_POWERS_OF_TEN`: correctly rounded `1e0` .. `1e39`.
const DOUBLE_POWERS_OF_TEN: [f64; 40] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16, 1e17, 1e18,
    1e19, 1e20, 1e21, 1e22, 1e23, 1e24, 1e25, 1e26, 1e27, 1e28, 1e29, 1e30, 1e31, 1e32, 1e33, 1e34, 1e35,
    1e36, 1e37, 1e38, 1e39,
];

fn double_pow10(exponent: u8) -> f64 {
    DOUBLE_POWERS_OF_TEN[usize::from(exponent).min(DOUBLE_POWERS_OF_TEN.len() - 1)]
}

/// The physical storage types of DECIMAL.
pub(super) trait DecimalStorage: NativeType + Copy {
    fn to_i128(self) -> i128;
    fn from_i128(value: i128) -> Self;
}

macro_rules! decimal_storage {
    ($($ty:ty),*) => {$(
        impl DecimalStorage for $ty {
            #[inline]
            fn to_i128(self) -> i128 {
                self.into()
            }
            #[inline]
            fn from_i128(value: i128) -> Self {
                value as $ty
            }
        }
    )*};
}
decimal_storage!(i16, i32, i64, i128);

/// Calls `$body` with `$storage` bound to the storage type of the DECIMAL `$ty`.
macro_rules! with_storage {
    ($ty:expr, $storage:ident => $body:expr) => {
        match $ty.physical_type() {
            PhysicalType::Int16 => {
                type $storage = i16;
                Some($body)
            }
            PhysicalType::Int32 => {
                type $storage = i32;
                Some($body)
            }
            PhysicalType::Int64 => {
                type $storage = i64;
                Some($body)
            }
            PhysicalType::Int128 => {
                type $storage = i128;
                Some($body)
            }
            _ => None,
        }
    };
}

/// `printf("%f")` of a double as DuckDB formats it in decimal error messages.
fn printf_f(value: f64) -> String {
    let mut out = Vec::new();
    format::write_printf_fixed(value, &mut out);
    String::from_utf8_lossy(&out).into_owned()
}

fn could_not_cast(value: &str, width: u8, scale: u8) -> RowError {
    RowError::Invalid(format!(
        "Could not cast value {value} to DECIMAL({width},{scale})"
    ))
}

/// Numeric values that can be cast to DECIMAL (DuckDB's `TryCastToDecimal`).
pub(super) trait ToDecimal: Copy {
    /// The unscaled DECIMAL(`width`, `scale`) value.
    fn to_decimal(self, width: u8, scale: u8) -> RowResult<i128>;
}

impl ToDecimal for bool {
    fn to_decimal(self, width: u8, scale: u8) -> RowResult<i128> {
        Ok(match (self, width > scale) {
            (true, true) => pow10(scale),
            (true, false) => 1,
            (false, _) => 0,
        })
    }
}

macro_rules! integer_to_decimal {
    ($($ty:ty),*) => {$(
        impl ToDecimal for $ty {
            #[allow(unused_comparisons, clippy::cast_lossless)]
            fn to_decimal(self, width: u8, scale: u8) -> RowResult<i128> {
                let value = self as i128;
                let limit = pow10(width - scale);
                if value >= limit || value <= -limit {
                    let mut text = Vec::new();
                    self.write_text(&mut text);
                    return Err(could_not_cast(&String::from_utf8_lossy(&text), width, scale));
                }
                Ok(value * pow10(scale))
            }
        }
    )*};
}
integer_to_decimal!(i8, i16, i32, i64, i128, u8, u16, u32, u64);

impl ToDecimal for u128 {
    fn to_decimal(self, width: u8, scale: u8) -> RowResult<i128> {
        match i128::try_from(self) {
            Ok(value) if value < pow10(width - scale) => Ok(value * pow10(scale)),
            _ => Err(could_not_cast(&self.to_string(), width, scale)),
        }
    }
}

/// DuckDB's `DoubleToDecimalCast`: scale, round half away from zero, range check.
fn float_to_decimal(input: f64, narrow: impl Fn(f64) -> f64, width: u8, scale: u8) -> RowResult<i128> {
    let rounded = (input * double_pow10(scale)).round();
    let limit = double_pow10(width);
    if rounded <= -limit || rounded >= limit || !rounded.is_finite() {
        return Err(could_not_cast(&printf_f(input), width, scale));
    }
    // The rounded value goes back through the source type before the integer cast.
    Ok(narrow(rounded) as i128)
}

impl ToDecimal for f32 {
    fn to_decimal(self, width: u8, scale: u8) -> RowResult<i128> {
        float_to_decimal(self.into(), |v| f64::from(v as f32), width, scale)
    }
}

impl ToDecimal for f64 {
    fn to_decimal(self, width: u8, scale: u8) -> RowResult<i128> {
        float_to_decimal(self, |v| v, width, scale)
    }
}

/// The kernel casting numeric `S` values to the DECIMAL `target`.
pub(super) fn to_decimal_kernel<S: ToDecimal + NativeType>(target: &LogicalType) -> Option<BoundCastInfo> {
    let (width, scale) = target.decimal_properties()?;
    with_storage!(target, Storage => BoundCastInfo::new(move |source, result, count, params| {
        let source = addressable(source, count);
        let values = source.values::<S>();
        cast_rows::<Storage>(&source, result, count, params, |slot| {
            values[slot].to_decimal(width, scale).map(Storage::from_i128)
        })
    }))
}

/// The kernel parsing VARCHAR into the DECIMAL `target`.
pub(super) fn string_to_decimal_kernel(target: &LogicalType) -> Option<BoundCastInfo> {
    let (width, scale) = target.decimal_properties()?;
    with_storage!(target, Storage => BoundCastInfo::new(move |source, result, count, params| {
        try_map_strings::<Storage>(source, result, count, params, |text| {
            parse_decimal(text, width, scale).map(Storage::from_i128).ok_or_else(|| {
                RowError::Invalid(format!(
                    "Could not convert string \"{}\" to DECIMAL({width},{scale})",
                    String::from_utf8_lossy(text)
                ))
            })
        })
    }))
}

/// Numeric targets of DECIMAL casts (DuckDB's `TryCastFromDecimal`).
trait FromDecimal: NativeType {
    /// Converts an unscaled DECIMAL value whose storage has physical type `storage`.
    fn from_decimal(unscaled: i128, storage: PhysicalType, scale: u8) -> RowResult<Self>;
}

impl FromDecimal for bool {
    fn from_decimal(unscaled: i128, _: PhysicalType, _: u8) -> RowResult<Self> {
        Ok(unscaled != 0)
    }
}

/// Rounds the unscaled value half away from zero to an integer.
fn decimal_to_integer(unscaled: i128, scale: u8) -> i128 {
    let power = pow10(scale);
    let rounding = if unscaled < 0 { -power / 2 } else { power / 2 };
    (unscaled + rounding) / power
}

macro_rules! integer_from_decimal {
    ($($ty:ty),*) => {$(
        impl FromDecimal for $ty {
            fn from_decimal(unscaled: i128, _: PhysicalType, scale: u8) -> RowResult<Self> {
                let scaled = decimal_to_integer(unscaled, scale);
                <$ty>::try_from(scaled).map_err(|_| {
                    RowError::Invalid(format!(
                        "Failed to cast decimal value {scaled} to type {}",
                        <$ty as NativeType>::PHYSICAL.name()
                    ))
                })
            }
        }
    )*};
}
integer_from_decimal!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128);

/// Whether DuckDB converts the unscaled value directly (`IsRepresentableExactly`).
fn representable_exactly(
    unscaled: i128,
    storage: PhysicalType,
    max_exact: i128,
    small_storage: PhysicalType,
) -> bool {
    match storage {
        PhysicalType::Int16 => true,
        s if s == small_storage => true,
        _ => (-max_exact..=max_exact).contains(&unscaled),
    }
}

/// DuckDB's `Cast::Operation<SRC, DOUBLE>` for a DECIMAL storage value.
fn storage_to_f64(value: i128, storage: PhysicalType) -> f64 {
    if storage == PhysicalType::Int128 {
        super::numeric::i128_to_f64(value)
    } else {
        value as f64
    }
}

/// DuckDB's `Cast::Operation<SRC, FLOAT>` for a DECIMAL storage value.
fn storage_to_f32(value: i128, storage: PhysicalType) -> f32 {
    if storage == PhysicalType::Int128 {
        super::numeric::i128_to_f64(value) as f32
    } else {
        value as f32
    }
}

impl FromDecimal for f64 {
    fn from_decimal(unscaled: i128, storage: PhysicalType, scale: u8) -> RowResult<Self> {
        let power = double_pow10(scale);
        if scale == 0 || representable_exactly(unscaled, storage, 1 << 53, PhysicalType::Int32) {
            return Ok(storage_to_f64(unscaled, storage) / power);
        }
        let divisor = pow10(scale);
        let (div, rem) = (unscaled / divisor, unscaled % divisor);
        Ok(storage_to_f64(div, storage) + storage_to_f64(rem, storage) / power)
    }
}

impl FromDecimal for f32 {
    fn from_decimal(unscaled: i128, storage: PhysicalType, scale: u8) -> RowResult<Self> {
        let power = double_pow10(scale) as f32;
        if scale == 0 || representable_exactly(unscaled, storage, 1 << 24, PhysicalType::Int16) {
            return Ok(storage_to_f32(unscaled, storage) / power);
        }
        let divisor = pow10(scale);
        let (div, rem) = (unscaled / divisor, unscaled % divisor);
        Ok(storage_to_f32(div, storage) + storage_to_f32(rem, storage) / power)
    }
}

fn from_decimal_kernel<D: FromDecimal>(source_type: &LogicalType) -> Option<BoundCastInfo> {
    let (_, scale) = source_type.decimal_properties()?;
    let storage = source_type.physical_type();
    with_storage!(source_type, Storage => BoundCastInfo::new(move |source, result, count, params| {
        let source = addressable(source, count);
        let values = source.values::<Storage>();
        cast_rows::<D>(&source, result, count, params, |slot| {
            D::from_decimal(values[slot].to_i128(), storage, scale)
        })
    }))
}

/// `Casting value "1.5" to type DECIMAL(1,0) failed: value is out of range!`
fn rescale_error(value: i128, width: u8, scale: u8, target: &LogicalType) -> RowError {
    let mut text = Vec::new();
    format::write_decimal(value, width, scale, &mut text);
    RowError::Invalid(format!(
        "Casting value \"{}\" to type {target} failed: value is out of range!",
        String::from_utf8_lossy(&text)
    ))
}

/// DECIMAL -> DECIMAL (DuckDB's `DecimalDecimalCastSwitch`): scale up with a range check
/// or scale down rounding half away from zero.
fn rescale(
    value: i128,
    (source_width, source_scale): (u8, u8),
    (target_width, target_scale): (u8, u8),
    target: &LogicalType,
) -> RowResult<i128> {
    if target_scale >= source_scale {
        let difference = target_scale - source_scale;
        let integral_width = target_width - difference;
        if source_width >= integral_width {
            let limit = pow10(integral_width);
            if value >= limit || value <= -limit {
                return Err(rescale_error(value, source_width, source_scale, target));
            }
        }
        return Ok(value * pow10(difference));
    }
    let difference = source_scale - target_scale;
    let factor = pow10(difference);
    if source_width >= target_width + difference {
        // Check that the rounded value fits.
        let limit = pow10(target_width + difference);
        let mut remainder = value % factor;
        let mut rounded = value;
        if rounded < 0 {
            rounded = -rounded;
            remainder = -remainder;
        }
        if remainder >= factor / 2 {
            rounded += factor;
        }
        if !(rounded < limit && rounded > -limit) {
            return Err(rescale_error(value, source_width, source_scale, target));
        }
    }
    // Scale first so rounding cannot overflow: divide by half the factor, push away from
    // zero, then halve.
    let halved = value / (factor / 2);
    let pushed = if halved < 0 { halved - 1 } else { halved + 1 };
    Ok(pushed / 2)
}

fn decimal_to_decimal_kernel(source_type: &LogicalType, target: &LogicalType) -> Option<BoundCastInfo> {
    let source_props = source_type.decimal_properties()?;
    let target_props = target.decimal_properties()?;
    let target_type = target.clone();
    with_storage!(source_type, Source => {
        with_storage!(target, Target => {
            let target_type = target_type.clone();
            BoundCastInfo::new(move |source, result, count, params| {
                let source = addressable(source, count);
                let values = source.values::<Source>();
                cast_rows::<Target>(&source, result, count, params, |slot| {
                    rescale(values[slot].to_i128(), source_props, target_props, &target_type).map(Target::from_i128)
                })
            })
        })?
    })
}

/// DuckDB's `DecimalCastSwitch`: casts from DECIMAL.
pub(super) fn cast_switch(source: &LogicalType, target: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    match target.id() {
        L::Boolean => from_decimal_kernel::<bool>(source),
        L::TinyInt => from_decimal_kernel::<i8>(source),
        L::SmallInt => from_decimal_kernel::<i16>(source),
        L::Integer => from_decimal_kernel::<i32>(source),
        L::BigInt => from_decimal_kernel::<i64>(source),
        L::HugeInt => from_decimal_kernel::<i128>(source),
        L::UTinyInt => from_decimal_kernel::<u8>(source),
        L::USmallInt => from_decimal_kernel::<u16>(source),
        L::UInteger => from_decimal_kernel::<u32>(source),
        L::UBigInt => from_decimal_kernel::<u64>(source),
        L::UHugeInt => from_decimal_kernel::<u128>(source),
        L::Float => from_decimal_kernel::<f32>(source),
        L::Double => from_decimal_kernel::<f64>(source),
        L::Decimal => decimal_to_decimal_kernel(source, target),
        L::Varchar => {
            let (width, scale) = source.decimal_properties()?;
            with_storage!(source, Storage => BoundCastInfo::new(move |source, result, count, params| {
                values_to_string::<Storage>(source, result, count, params, |value, out| {
                    format::write_decimal(value.to_i128(), width, scale, out);
                })
            }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_to_decimal() {
        assert_eq!(5i32.to_decimal(4, 2), Ok(500));
        assert!(
            matches!(100i32.to_decimal(4, 2), Err(RowError::Invalid(m)) if m == "Could not cast value 100 to DECIMAL(4,2)")
        );
        assert_eq!(1.005f64.to_decimal(5, 2), Ok(100));
        assert_eq!(2.5f64.to_decimal(3, 0), Ok(3));
        assert!(
            matches!(1e30f64.to_decimal(18, 3), Err(RowError::Invalid(m)) if m == "Could not cast value 1000000000000000019884624838656.000000 to DECIMAL(18,3)")
        );
        assert_eq!(true.to_decimal(1, 1), Ok(1));
        assert_eq!(true.to_decimal(3, 1), Ok(10));
    }

    #[test]
    fn decimals_to_numbers() {
        assert_eq!(i32::from_decimal(15, PhysicalType::Int32, 1), Ok(2));
        assert_eq!(i32::from_decimal(-15, PhysicalType::Int32, 1), Ok(-2));
        assert_eq!(i32::from_decimal(14, PhysicalType::Int32, 1), Ok(1));
        assert!(u8::from_decimal(-15, PhysicalType::Int32, 1).is_err());
        assert_eq!(f64::from_decimal(12345, PhysicalType::Int32, 2), Ok(123.45));
    }

    #[test]
    fn rescaling_rounds_half_away() {
        let target = LogicalType::decimal(4, 1);
        assert_eq!(rescale(1234, (6, 3), (4, 1), &target), Ok(12));
        assert_eq!(rescale(1250, (6, 3), (4, 1), &target), Ok(13));
        assert_eq!(rescale(-1250, (6, 3), (4, 1), &target), Ok(-13));
        assert_eq!(rescale(15, (4, 1), (6, 3), &LogicalType::decimal(6, 3)), Ok(1500));
        let narrow = LogicalType::decimal(2, 1);
        assert!(matches!(
            rescale(123_456, (6, 3), (2, 1), &narrow),
            Err(RowError::Invalid(m)) if m == "Casting value \"123.456\" to type DECIMAL(2,1) failed: value is out of range!"
        ));
    }
}
