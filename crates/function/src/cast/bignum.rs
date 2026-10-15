//! Casts from and to BIGNUM (DuckDB's `bignum_casts.cpp`).

use rustdb_types::{LogicalType, LogicalTypeId, NativeType, PhysicalType};

use super::BoundCastInfo;
use super::executor::{
    RowError, try_map_strings, try_map_strings_to_string, try_values_to_string, values_to_string,
};
use super::string::could_not_convert;
use crate::bignum;

/// BIGNUM -> integer (`BignumToIntCast`). Out-of-range values raise an error even under
/// `TRY_CAST`; negative values wrap into unsigned targets like DuckDB.
fn to_integer<T: NativeType>(max: u128, from_bits: fn(u128) -> T) -> BoundCastInfo {
    BoundCastInfo::new(move |source, result, count, params| {
        try_map_strings::<T>(source, result, count, params, |bytes| {
            let (negative, magnitude) = bignum::to_integer(bytes, max)?;
            let bits = if negative {
                magnitude.wrapping_neg()
            } else {
                magnitude
            };
            Ok(from_bits(bits))
        })
    })
}

/// DuckDB's `BignumCastSwitch`.
#[allow(clippy::cast_possible_truncation, reason = "C integer conversions")]
pub(super) fn from_bignum(target: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    // `std::numeric_limits` has no specialisation for DuckDB's 128-bit types, so their
    // maximum is 0 in `BignumToInt`.
    Some(match target.id() {
        L::TinyInt => to_integer::<i8>(i8::MAX as u128, |bits| bits as i8),
        L::UTinyInt => to_integer::<u8>(u128::from(u8::MAX), |bits| bits as u8),
        L::SmallInt => to_integer::<i16>(i16::MAX as u128, |bits| bits as i16),
        L::USmallInt => to_integer::<u16>(u128::from(u16::MAX), |bits| bits as u16),
        L::Integer => to_integer::<i32>(i32::MAX as u128, |bits| bits as i32),
        L::UInteger => to_integer::<u32>(u128::from(u32::MAX), |bits| bits as u32),
        L::BigInt => to_integer::<i64>(i64::MAX as u128, |bits| bits as i64),
        L::UBigInt => to_integer::<u64>(u128::from(u64::MAX), |bits| bits as u64),
        L::HugeInt => to_integer::<i128>(0, |bits| bits as i128),
        L::UHugeInt => to_integer::<u128>(0, |bits| bits),
        L::Varchar => BoundCastInfo::new(|source, result, count, params| {
            try_map_strings_to_string(source, result, count, params, |bytes, out| {
                out.extend_from_slice(bignum::to_text(bytes)?.as_bytes());
                Ok(())
            })
        }),
        L::Double => BoundCastInfo::new(|source, result, count, params| {
            try_map_strings::<f64>(source, result, count, params, |bytes| Ok(bignum::to_f64(bytes)?))
        }),
        _ => return None,
    })
}

fn from_int<T: NativeType>(convert: fn(T) -> Vec<u8>) -> BoundCastInfo {
    BoundCastInfo::new(move |source, result, count, params| {
        values_to_string::<T>(source, result, count, params, |value, out| {
            out.extend_from_slice(&convert(value));
        })
    })
}

/// FLOAT/DOUBLE -> BIGNUM: infinities and NaN fail with the generic numeric message.
fn from_float<T: NativeType>(to_f64: fn(T) -> f64, name: &'static str) -> BoundCastInfo {
    BoundCastInfo::new(move |source, result, count, params| {
        try_values_to_string::<T>(source, result, count, params, |value, out| {
            let value = to_f64(value);
            let bytes = bignum::from_f64(value).ok_or_else(|| {
                let text = if value.is_nan() {
                    "nan"
                } else if value > 0.0 {
                    "inf"
                } else {
                    "-inf"
                };
                RowError::Invalid(format!(
                    "Type {name} with value {text} can't be cast to the destination type VARCHAR"
                ))
            })?;
            out.extend_from_slice(&bytes);
            Ok(())
        })
    })
}

/// DuckDB's `NumericToBignumCastSwitch` and the VARCHAR -> BIGNUM cast.
pub(super) fn to_bignum(source: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    Some(match source.id() {
        L::TinyInt => from_int::<i8>(|v| bignum::from_i64(i64::from(v))),
        L::UTinyInt => from_int::<u8>(|v| bignum::from_u64(false, u64::from(v))),
        L::SmallInt => from_int::<i16>(|v| bignum::from_i64(i64::from(v))),
        L::USmallInt => from_int::<u16>(|v| bignum::from_u64(false, u64::from(v))),
        L::Integer => from_int::<i32>(|v| bignum::from_i64(i64::from(v))),
        L::UInteger => from_int::<u32>(|v| bignum::from_u64(false, u64::from(v))),
        L::BigInt => from_int::<i64>(bignum::from_i64),
        L::UBigInt => from_int::<u64>(|v| bignum::from_u64(false, v)),
        L::HugeInt => from_int::<i128>(bignum::from_i128),
        L::UHugeInt => from_int::<u128>(bignum::from_u128),
        L::Float => from_float::<f32>(f64::from, "FLOAT"),
        L::Double => from_float::<f64>(|v| v, "DOUBLE"),
        L::Varchar => BoundCastInfo::new(|source, result, count, params| {
            try_map_strings_to_string(source, result, count, params, |text, out| {
                let bytes =
                    bignum::from_text(text).ok_or_else(|| could_not_convert(text, PhysicalType::Varchar))?;
                out.extend_from_slice(&bytes);
                Ok(())
            })
        }),
        _ => return None,
    })
}
