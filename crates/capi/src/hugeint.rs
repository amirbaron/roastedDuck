//! 128-bit integer and decimal conversions (DuckDB's `capi/hugeint-c.cpp`).

use roastedduck_function::cast::ValueCast;
use roastedduck_function::cast::numeric::{f64_to_i128, f64_to_u128, i128_to_f64, u128_to_f64};
use roastedduck_types::{LogicalType, Value};

use crate::types::{DuckdbDecimal, DuckdbHugeint, DuckdbUhugeint};

/// `double duckdb_hugeint_to_double(duckdb_hugeint val)` (capi/hugeint-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_hugeint_to_double(val: DuckdbHugeint) -> f64 {
    i128_to_f64(val.to_i128())
}

/// `double duckdb_uhugeint_to_double(duckdb_uhugeint val)` (capi/hugeint-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_uhugeint_to_double(val: DuckdbUhugeint) -> f64 {
    u128_to_f64(val.to_u128())
}

/// `duckdb_hugeint duckdb_double_to_hugeint(double val)` (capi/hugeint-c.cpp): the value
/// truncated toward zero (`Hugeint::TryConvert`), 0 if it is not finite or out of range.
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_double_to_hugeint(val: f64) -> DuckdbHugeint {
    DuckdbHugeint::from_i128(f64_to_i128(val).unwrap_or(0))
}

/// `duckdb_uhugeint duckdb_double_to_uhugeint(double val)` (capi/hugeint-c.cpp): the value
/// truncated toward zero (`Uhugeint::TryConvert`), 0 if it is not finite or out of range.
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_double_to_uhugeint(val: f64) -> DuckdbUhugeint {
    DuckdbUhugeint::from_u128(f64_to_u128(val).unwrap_or(0))
}

/// The C decimal of a DECIMAL value, or the zero `duckdb_decimal` for anything else
/// (`FetchDefaultValue::Operation<duckdb_decimal>`).
pub(crate) fn decimal_to_c(value: &Value) -> DuckdbDecimal {
    match (value.logical_type().decimal_properties(), value.as_i128()) {
        (Some((width, scale)), Some(unscaled)) if !value.is_null() => DuckdbDecimal {
            width,
            scale,
            value: DuckdbHugeint::from_i128(unscaled),
        },
        _ => DuckdbDecimal::default(),
    }
}

/// `duckdb_decimal duckdb_double_to_decimal(double val, uint8_t width, uint8_t scale)`
/// (capi/hugeint-c.cpp): the DOUBLE to DECIMAL cast, or the zero decimal if the width or
/// scale is invalid or the value does not fit.
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_double_to_decimal(val: f64, width: u8, scale: u8) -> DuckdbDecimal {
    if scale > width || width > LogicalType::MAX_DECIMAL_WIDTH {
        return DuckdbDecimal::default();
    }
    Value::double(val)
        .try_cast(&LogicalType::decimal(width, scale))
        .map_or_else(|_| DuckdbDecimal::default(), |value| decimal_to_c(&value))
}

/// `double duckdb_decimal_to_double(duckdb_decimal val)` (capi/hugeint-c.cpp): the value
/// read as a 128-bit decimal of `val.scale` (`TryCastFromDecimal` from `hugeint_t`).
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_decimal_to_double(val: DuckdbDecimal) -> f64 {
    let decimal = Value::decimal(val.value.to_i128(), LogicalType::MAX_DECIMAL_WIDTH, val.scale);
    decimal
        .try_cast(&LogicalType::DOUBLE)
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0)
}
