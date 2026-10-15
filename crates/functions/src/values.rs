//! Typed access to constant arguments (DuckDB's `Value::GetValue<T>()`, which casts).

use rustdb_common::{Error, Result};
use rustdb_function::cast::ValueCast;
use rustdb_types::{LogicalType, Value};

/// The value cast to DOUBLE (DuckDB's `Value::GetValue<double>()`).
pub(crate) fn as_double(value: &Value) -> Result<f64> {
    if let Some(value) = value.as_f64() {
        return Ok(value);
    }
    value
        .default_cast_as(&LogicalType::DOUBLE)?
        .as_f64()
        .ok_or_else(|| Error::internal("DOUBLE cast did not produce a double"))
}

/// The value cast to BIGINT (DuckDB's `Value::GetValue<int64_t>()`).
pub(crate) fn as_bigint(value: &Value) -> Result<i64> {
    if let Some(value) = value.as_i64() {
        return Ok(value);
    }
    value
        .default_cast_as(&LogicalType::BIGINT)?
        .as_i64()
        .ok_or_else(|| Error::internal("BIGINT cast did not produce a bigint"))
}

/// DuckDB's `CastExceptionText` for a numeric value that does not fit `target` (raised as
/// an Invalid Input Error by `Cast::Operation`).
pub(crate) fn cast_error<T: rustdb_function::cast::numeric::CastNumber>(value: T, target: &str) -> Error {
    Error::invalid_input(format!(
        "Type {} with value {} can't be cast because the value is out of range for the destination type {target}",
        T::PHYSICAL.name(),
        value.to_text()
    ))
}
