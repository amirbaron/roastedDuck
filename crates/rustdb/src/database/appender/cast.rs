//! The conversions of typed appends (`BaseAppender::AppendValueInternal<T>` in
//! `main/appender.cpp`).
//!
//! A value appended through a typed call (`Append<int32_t>`, `duckdb_append_date`,
//! `duckdb_append_varchar`, ...) goes into a column of one of the common types through
//! DuckDB's direct C++ casts, `Cast::Operation<T, DST>`, not through the value casts of
//! `AppendValue`. So their errors carry no "Failed to cast value" prefix, some pairs have no
//! cast at all (a number into a DATE, a string into an INTERVAL), strings are not validated
//! as UTF-8, and a TIMESTAMP WITH TIME ZONE or TIME WITH TIME ZONE column takes the value
//! as it is, without a time zone conversion.

use rustdb_common::{Error, Result};
use rustdb_function::cast::ValueCast;
use rustdb_types::{Date, LogicalType, LogicalTypeId, PhysicalType, Time, TimeNs, TimeTz, Timestamp, Value};

/// `GetTypeId<T>()` of the C type a value of `ty` is held in: its physical type, except
/// that `dtime_tz_t` reports INT64.
fn type_id(ty: &LogicalType) -> PhysicalType {
    if ty.id() == LogicalTypeId::TimeTz {
        PhysicalType::Int64
    } else {
        ty.physical_type()
    }
}

/// `TypeIsNumber<T>()`: the integral types (`bool` among them), the floating point types and
/// the 128-bit integers.
fn is_number(id: LogicalTypeId) -> bool {
    use LogicalTypeId as L;
    matches!(
        id,
        L::Boolean
            | L::TinyInt
            | L::SmallInt
            | L::Integer
            | L::BigInt
            | L::UTinyInt
            | L::USmallInt
            | L::UInteger
            | L::UBigInt
            | L::HugeInt
            | L::UHugeInt
            | L::Float
            | L::Double
    )
}

/// The bytes of a string input.
fn string_bytes(input: &Value) -> &[u8] {
    input.as_bytes().unwrap_or_default()
}

/// `TryCast::UnimplementedErrorMessage`: the generic `TryCast::Operation`, for pairs without
/// a cast.
fn unimplemented(input: &Value, target: &LogicalType) -> Error {
    Error::conversion(format!(
        "Unimplemented type for cast ({} -> {})",
        type_id(input.logical_type()).name(),
        type_id(target).name()
    ))
}

/// The error of `Cast::Operation` when `TryCast::Operation` fails (`CastExceptionText`).
fn cast_exception(input: &Value, target: &LogicalType) -> Error {
    let source_id = input.logical_type().id();
    let source = type_id(input.logical_type()).name();
    let target_name = type_id(target).name();
    if source_id == LogicalTypeId::Varchar {
        let text = String::from_utf8_lossy(string_bytes(input));
        return Error::invalid_input(format!("Could not convert string '{text}' to {target_name}"));
    }
    let text = match input.to_varchar() {
        Ok(text) => text,
        Err(error) => return error,
    };
    if is_number(source_id) && is_number(target.id()) {
        Error::invalid_input(format!(
            "Type {source} with value {text} can't be cast because the value is out of range for the \
             destination type {target_name}"
        ))
    } else {
        Error::invalid_input(format!(
            "Type {source} with value {text} can't be cast to the destination type {target_name}"
        ))
    }
}

/// The time of day of a TIMESTAMP input (`TryCast` `timestamp_t` to `dtime_t` and
/// `dtime_tz_t`), which fails for infinite timestamps.
fn time_of_day(input: &Value, target: &LogicalType) -> Result<Time> {
    input
        .as_timestamp()
        .filter(|timestamp| timestamp.is_finite())
        .map(Timestamp::to_time)
        .ok_or_else(|| cast_exception(input, target))
}

/// The error of the string casts to the TIME types (`Time::ConversionError`).
fn time_conversion_error(bytes: &[u8]) -> Error {
    Error::conversion(Time::conversion_error(bytes))
}

/// The value `AppendValueInternal<T>` writes for `input` (a value of the C type `T`; strings
/// are VARCHAR values of raw bytes) into a column of type `target`, or `None` for the column
/// types it leaves to `AppendValue`.
pub(super) fn cast_operation(input: &Value, target: &LogicalType) -> Result<Option<Value>> {
    use LogicalTypeId as L;
    let source = input.logical_type().id();
    let value = match target.id() {
        L::Boolean
        | L::TinyInt
        | L::SmallInt
        | L::Integer
        | L::BigInt
        | L::UTinyInt
        | L::USmallInt
        | L::UInteger
        | L::UBigInt
        | L::HugeInt
        | L::UHugeInt
        | L::Float
        | L::Double => {
            if !is_number(source) && source != L::Varchar {
                return Err(unimplemented(input, target));
            }
            input.try_cast(target).map_err(|_| cast_exception(input, target))?
        }
        // `AppendDecimalValueInternal`: `TryCastToDecimal` throws its conversion error.
        L::Decimal => {
            if !is_number(source) && source != L::Varchar {
                return Err(Error::not_implemented("Unimplemented type for TryCastToDecimal!"));
            }
            input.try_cast(target)?
        }
        L::Date => match source {
            L::Date => input.clone(),
            L::Timestamp => input.try_cast(target).map_err(|_| cast_exception(input, target))?,
            // `Date::FromCString`.
            L::Varchar => {
                let bytes = string_bytes(input);
                Value::date(Date::parse(bytes, false).map_err(|error| Error::conversion(error.message(bytes)))?)
            }
            _ => return Err(unimplemented(input, target)),
        },
        L::Timestamp | L::TimestampTz => match source {
            L::Timestamp => input.clone(),
            L::Date => input
                .try_cast(&LogicalType::TIMESTAMP)
                .map_err(|_| cast_exception(input, target))?,
            // `Timestamp::FromCString` without applying an offset.
            L::Varchar => {
                let bytes = string_bytes(input);
                let (timestamp, _) =
                    Timestamp::parse(bytes, false).map_err(|error| Error::conversion(error.message(bytes)))?;
                Value::timestamp(timestamp)
            }
            _ => return Err(unimplemented(input, target)),
        },
        L::Time => match source {
            L::Time => input.clone(),
            L::Timestamp => Value::time(time_of_day(input, target)?),
            // `Time::FromCString`.
            L::Varchar => {
                let bytes = string_bytes(input);
                Value::time(Time::parse(bytes, false).ok_or_else(|| time_conversion_error(bytes))?)
            }
            _ => return Err(unimplemented(input, target)),
        },
        L::TimeNs => match (source, input.as_time()) {
            (L::Time, Some(time)) => Value::time_ns(time.to_time_ns()?),
            (L::Varchar, _) => {
                let bytes = string_bytes(input);
                Value::time_ns(TimeNs::parse(bytes, false).ok_or_else(|| time_conversion_error(bytes))?)
            }
            _ => return Err(unimplemented(input, target)),
        },
        L::TimeTz => match (source, input.as_time()) {
            (L::Time, Some(time)) => Value::time_tz(TimeTz::from_time(time)),
            (L::Timestamp, _) => Value::time_tz(TimeTz::from_time(time_of_day(input, target)?)),
            (L::Varchar, _) => {
                let bytes = string_bytes(input);
                Value::time_tz(TimeTz::parse(bytes, false).ok_or_else(|| time_conversion_error(bytes))?)
            }
            _ => return Err(unimplemented(input, target)),
        },
        L::Interval => match source {
            L::Interval => input.clone(),
            _ => return Err(unimplemented(input, target)),
        },
        // `StringCast::Operation<T>`; a string is stored as it is.
        L::Varchar => match source {
            L::Varchar => input.clone(),
            _ => input.default_cast_as(&LogicalType::VARCHAR)?,
        },
        _ => return Ok(None),
    };
    Ok(Some(value.reinterpret(target.clone())))
}
