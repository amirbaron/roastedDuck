//! `make_date`, `make_time`, `make_timestamp`, `make_timestamp_ns` (DuckDB's
//! `make_date.cpp`) and `to_timestamp`, `normalized_interval`, `timetz_byte_comparable`
//! (`epoch.cpp`).

use roastedduck_common::{Error, Result};
use roastedduck_function::cast::numeric::TryCastTo;
use roastedduck_function::{ScalarFunction, ScalarFunctionSet};
use roastedduck_types::{DataChunk, Date, Interval, LogicalType, Time, TimeTz, Timestamp, Vector};

use crate::scalar::executor::{addressable, ternary, unary};
use crate::scalar::nested::struct_field;
use crate::temporal::{date_from_ymd, timestamp_from_datetime};
use crate::values::cast_error;

/// `Cast::Operation<int64_t, int32_t>`.
fn to_i32(value: i64) -> Result<i32> {
    value.try_cast_to().ok_or_else(|| cast_error(value, "INT32"))
}

/// DuckDB's `MakeDateOperator`.
fn make_date(year: i64, month: i64, day: i64) -> Result<Date> {
    date_from_ymd(to_i32(year)?, to_i32(month)?, to_i32(day)?)
}

/// DuckDB's `MakeTimeOperator`: fractional seconds are rounded to microseconds.
fn make_time(hour: i64, minute: i64, seconds: f64) -> Result<Time> {
    let hour = to_i32(hour)?;
    let minute = to_i32(minute)?;
    let whole: i32 = if seconds < 0.0 || seconds > 60.0 {
        seconds
            .try_cast_to()
            .ok_or_else(|| cast_error(seconds, "INT32"))?
    } else {
        seconds as i32
    };
    let micros = ((seconds - f64::from(whole)) * Interval::MICROS_PER_SEC as f64).round() as i32;
    if !Time::is_valid_hms(hour, minute, whole, micros) {
        return Err(Error::conversion(format!(
            "Time out of range: {hour}:{minute}:{whole}.{micros}"
        )));
    }
    Ok(Time::from_hms_micros(hour, minute, whole, micros))
}

fn timestamp_micros(value: i64) -> Result<Timestamp> {
    let timestamp = Timestamp(value);
    if !timestamp.is_finite() {
        return Err(Error::conversion(format!(
            "Timestamp microseconds out of range: {value}"
        )));
    }
    Ok(timestamp)
}

/// `make_date(year, month, day)`, `make_date(days)` and `make_date({year, month, day})`.
pub fn make_date_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("make_date");
    set.add(ScalarFunction::new(
        "make_date",
        vec![LogicalType::INTEGER],
        LogicalType::DATE,
        |args, _, result| {
            unary::<i32, Date>(args, result, |days, out| {
                *out = Date(days);
                Ok(true)
            })
        },
    ));
    set.add(ScalarFunction::new(
        "make_date",
        vec![LogicalType::BIGINT; 3],
        LogicalType::DATE,
        |args, _, result| {
            ternary::<i64, i64, i64, Date>(args, result, |year, month, day, out| {
                *out = make_date(year, month, day)?;
                Ok(true)
            })
        },
    ));
    let fields = vec![
        ("year".into(), LogicalType::BIGINT),
        ("month".into(), LogicalType::BIGINT),
        ("day".into(), LogicalType::BIGINT),
    ];
    set.add(ScalarFunction::new(
        "make_date",
        vec![LogicalType::struct_type(fields)],
        LogicalType::DATE,
        |args, _, result| {
            let count = args.len();
            let input = addressable(args.column(0), count);
            let parts = (0..3)
                .map(|index| struct_field(&input, index, count))
                .collect::<Result<Vec<Vector>>>()?;
            let chunk = DataChunk::new(parts, count);
            ternary::<i64, i64, i64, Date>(&chunk, result, |year, month, day, out| {
                *out = make_date(year, month, day)?;
                Ok(true)
            })
        },
    ));
    set
}

/// `make_time(hour, minute, seconds)`.
pub fn make_time_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("make_time");
    set.add(ScalarFunction::new(
        "make_time",
        vec![LogicalType::BIGINT, LogicalType::BIGINT, LogicalType::DOUBLE],
        LogicalType::TIME,
        |args, _, result| {
            ternary::<i64, i64, f64, Time>(args, result, |hour, minute, seconds, out| {
                *out = make_time(hour, minute, seconds)?;
                Ok(true)
            })
        },
    ));
    set
}

/// `make_timestamp(year, month, day, hour, minute, seconds)` and
/// `make_timestamp(micros)`.
pub fn make_timestamp_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("make_timestamp");
    let mut arguments = vec![LogicalType::BIGINT; 5];
    arguments.push(LogicalType::DOUBLE);
    set.add(ScalarFunction::new(
        "make_timestamp",
        arguments,
        LogicalType::TIMESTAMP,
        |args, _, result| {
            // The parts are BIGINT except the DOUBLE seconds; read them all as DOUBLE-free
            // values through the matching unified accessors.
            let count = args.len();
            let columns: Vec<_> = args
                .columns()
                .iter()
                .map(|column| addressable(column, count))
                .collect();
            let constant = columns.iter().all(|column| column.is_constant());
            let rows = if constant { 1 } else { count };
            let parts: Vec<_> = columns[..5]
                .iter()
                .map(|column| column.unified::<i64>())
                .collect();
            let seconds = columns[5].unified::<f64>();
            let mut values = Vec::with_capacity(rows);
            for row in 0..rows {
                let fields: Option<Vec<i64>> = parts.iter().map(|part| part.get(row)).collect();
                let (Some(fields), Some(seconds)) = (fields, seconds.get(row)) else {
                    values.push(None);
                    continue;
                };
                let date = make_date(fields[0], fields[1], fields[2])?;
                let time = make_time(fields[3], fields[4], seconds)?;
                values.push(Some(timestamp_from_datetime(date, time)?));
            }
            let mut output = Vector::flat(LogicalType::TIMESTAMP, rows.max(1));
            output.reset_flat(rows);
            let mut nulls = Vec::new();
            {
                let data = output.values_mut::<Timestamp>();
                for (row, value) in values.iter().enumerate() {
                    match value {
                        Some(value) => data[row] = *value,
                        None => nulls.push(row),
                    }
                }
            }
            let validity = output.validity_mut();
            for row in nulls {
                validity.set_invalid(row, rows);
            }
            if constant {
                output.make_constant();
            }
            *result = output;
            Ok(())
        },
    ));
    set.add(ScalarFunction::new(
        "make_timestamp",
        vec![LogicalType::BIGINT],
        LogicalType::TIMESTAMP,
        |args, _, result| {
            unary::<i64, Timestamp>(args, result, |micros, out| {
                *out = timestamp_micros(micros)?;
                Ok(true)
            })
        },
    ));
    set
}

/// `make_timestamp_ns(nanos)`.
pub fn make_timestamp_ns_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("make_timestamp_ns");
    set.add(ScalarFunction::new(
        "make_timestamp_ns",
        vec![LogicalType::BIGINT],
        LogicalType::TIMESTAMP_NS,
        |args, _, result| {
            unary::<i64, i64>(args, result, |nanos, out| {
                *out = timestamp_micros(nanos)?.0;
                Ok(true)
            })
        },
    ));
    set
}

/// `to_timestamp(seconds)`: a TIMESTAMP WITH TIME ZONE.
pub fn to_timestamp_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("to_timestamp");
    set.add(ScalarFunction::new(
        "to_timestamp",
        vec![LogicalType::DOUBLE],
        LogicalType::TIMESTAMP_TZ,
        |args, _, result| {
            unary::<f64, Timestamp>(args, result, |seconds, out| {
                let micros: i64 = (seconds * Interval::MICROS_PER_SEC as f64)
                    .try_cast_to()
                    .ok_or_else(|| {
                        Error::conversion("Epoch seconds out of range for TIMESTAMP WITH TIME ZONE")
                    })?;
                *out = Timestamp(micros);
                Ok(true)
            })
        },
    ));
    set
}

/// `normalized_interval(interval)` and `timetz_byte_comparable(timetz)`.
pub fn interval_helper_functions() -> Vec<ScalarFunctionSet> {
    let mut normalized = ScalarFunctionSet::new("normalized_interval");
    normalized.add(ScalarFunction::new(
        "normalized_interval",
        vec![LogicalType::INTERVAL],
        LogicalType::INTERVAL,
        |args, _, result| {
            unary::<Interval, Interval>(args, result, |interval, out| {
                *out = interval.normalize();
                Ok(true)
            })
        },
    ));
    let mut sort_key = ScalarFunctionSet::new("timetz_byte_comparable");
    sort_key.add(ScalarFunction::new(
        "timetz_byte_comparable",
        vec![LogicalType::TIME_TZ],
        LogicalType::UBIGINT,
        |args, _, result| {
            unary::<TimeTz, u64>(args, result, |time, out| {
                *out = time.sort_key();
                Ok(true)
            })
        },
    ));
    vec![normalized, sort_key]
}
