//! Casts from VARCHAR (DuckDB's `string_cast.cpp` in `src/function/cast`).

use rustdb_common::Result;
use rustdb_types::{LogicalType, LogicalTypeId, NativeType, PhysicalType, Uuid, bit, blob};

use super::decimal::string_to_decimal_kernel;
use super::executor::{RowError, try_map_strings, try_map_strings_to_string};
use super::parse::{ParseFloat, ParseInteger, parse_bool, parse_float};
use super::{BoundCastInfo, CastFunctionSet};

/// `Could not convert string 'abc' to INT32` (DuckDB's `CastExceptionText` for strings).
pub(super) fn could_not_convert(text: &[u8], physical: PhysicalType) -> RowError {
    RowError::Invalid(format!(
        "Could not convert string '{}' to {}",
        String::from_utf8_lossy(text),
        physical.name()
    ))
}

/// VARCHAR -> `T` with `parse`, failing with DuckDB's generic string cast message.
fn parse_kernel<T: NativeType>(parse: fn(&[u8], bool) -> Option<T>) -> BoundCastInfo {
    BoundCastInfo::new(move |source, result, count, params| {
        let strict = params.strict;
        try_map_strings::<T>(source, result, count, params, |text| {
            parse(text, strict).ok_or_else(|| could_not_convert(text, T::PHYSICAL))
        })
    })
}

fn integer_kernel<T: ParseInteger + NativeType>() -> BoundCastInfo {
    parse_kernel::<T>(T::parse_text)
}

fn float_kernel<T: ParseFloat + NativeType>() -> BoundCastInfo {
    parse_kernel::<T>(parse_float::<T>)
}

/// VARCHAR -> BLOB/BIT: the target text format, failing with its specific message.
fn string_kernel(convert: fn(&[u8], &mut Vec<u8>) -> std::result::Result<(), String>) -> BoundCastInfo {
    BoundCastInfo::new(move |source, result, count, params| {
        try_map_strings_to_string(source, result, count, params, |text, out| {
            convert(text, out).map_err(RowError::Invalid)
        })
    })
}

/// DuckDB's `StringCastSwitch`: casts from VARCHAR.
pub(super) fn cast_switch(
    set: &CastFunctionSet,
    _source: &LogicalType,
    target: &LogicalType,
) -> Result<Option<BoundCastInfo>> {
    use LogicalTypeId as L;
    Ok(Some(match target.id() {
        L::Boolean => parse_kernel::<bool>(parse_bool),
        L::TinyInt => integer_kernel::<i8>(),
        L::SmallInt => integer_kernel::<i16>(),
        L::Integer => integer_kernel::<i32>(),
        L::BigInt => integer_kernel::<i64>(),
        L::HugeInt => integer_kernel::<i128>(),
        L::UTinyInt => integer_kernel::<u8>(),
        L::USmallInt => integer_kernel::<u16>(),
        L::UInteger => integer_kernel::<u32>(),
        L::UBigInt => integer_kernel::<u64>(),
        L::UHugeInt => integer_kernel::<u128>(),
        L::Float => float_kernel::<f32>(),
        L::Double => float_kernel::<f64>(),
        L::Decimal => return Ok(string_to_decimal_kernel(target)),
        L::Uuid => parse_kernel::<Uuid>(Uuid::parse),
        L::Blob => string_kernel(blob::parse_text),
        L::Bit => string_kernel(bit::parse_text),
        L::Geometry => super::geometry::from_string(),
        L::Varchar => BoundCastInfo::reinterpret(),
        L::Enum => return Ok(super::enums::string_to_enum(target)),
        L::Date
        | L::Time
        | L::TimeNs
        | L::TimeTz
        | L::Timestamp
        | L::TimestampTz
        | L::TimestampNs
        | L::TimestampMs
        | L::TimestampSec
        | L::Interval => return Ok(super::temporal::from_string(target)),
        L::List | L::Array | L::Struct | L::Map => return super::string_nested::cast_switch(set, target),
        _ => return Ok(None),
    }))
}
