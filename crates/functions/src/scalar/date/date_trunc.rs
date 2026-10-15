//! `date_trunc`/`datetrunc` (DuckDB's `date_trunc.cpp`).

use std::sync::Arc;

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::{BindInput, FunctionData, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, Date, Interval, LogicalType, LogicalTypeId, Time, Timestamp, Vector};

use super::calendar;
use super::date_part::Part;
use crate::scalar::executor::{Str, binary, unary};
use crate::temporal::timestamp_from_datetime;

/// What a date is truncated to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Millennium,
    Century,
    Decade,
    Year,
    Quarter,
    Month,
    Week,
    IsoYear,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
}

impl Unit {
    /// DuckDB's `TruncateElement` dispatch.
    fn from_part(part: Part) -> Result<Self> {
        Ok(match part {
            Part::Millennium => Self::Millennium,
            Part::Century => Self::Century,
            Part::Decade => Self::Decade,
            Part::Year => Self::Year,
            Part::Quarter => Self::Quarter,
            Part::Month => Self::Month,
            Part::Week | Part::YearWeek => Self::Week,
            Part::IsoYear => Self::IsoYear,
            Part::Day | Part::Dow | Part::IsoDow | Part::Doy | Part::JulianDay => Self::Day,
            Part::Hour => Self::Hour,
            Part::Minute => Self::Minute,
            Part::Second | Part::Epoch => Self::Second,
            Part::Milliseconds => Self::Millisecond,
            Part::Microseconds => Self::Microsecond,
            _ => {
                return Err(Error::new(
                    ErrorKind::NotImplemented,
                    "Specifier type not implemented for DATETRUNC",
                ));
            }
        })
    }

    fn parse(specifier: &[u8]) -> Result<Self> {
        Self::from_part(Part::parse(&String::from_utf8_lossy(specifier))?)
    }
}

fn date(year: i32, month: i32, day: i32) -> Date {
    Date(calendar::days_from_ymd(year, month, day) as i32)
}

/// The Monday of the week of `date` (DuckDB's `Date::GetMondayOfCurrentWeek`).
fn monday(date: Date) -> Date {
    let days = i64::from(date.0);
    Date((days - (calendar::iso_day_of_week(days) - 1)) as i32)
}

/// Truncates a date to a date-level unit.
fn truncate_date(input: Date, unit: Unit) -> Date {
    let (year, month, _) = input.to_ymd();
    match unit {
        Unit::Millennium => date((year / 1000) * 1000, 1, 1),
        Unit::Century => date((year / 100) * 100, 1, 1),
        Unit::Decade => date((year / 10) * 10, 1, 1),
        Unit::Year => date(year, 1, 1),
        Unit::Quarter => date(year, 1 + ((month - 1) / 3) * 3, 1),
        Unit::Month => date(year, month, 1),
        Unit::Week => monday(input),
        Unit::IsoYear => {
            let monday = monday(input);
            let week = calendar::iso_year_week(monday).1;
            Date(monday.0 - (week - 1) * 7)
        }
        _ => input,
    }
}

/// `date_trunc(unit, date)`: a TIMESTAMP (infinite dates stay infinite).
fn truncate_date_to_timestamp(input: Date, unit: Unit) -> Result<Timestamp> {
    if !input.is_finite() {
        return Ok(if input == Date::INFINITY {
            Timestamp::INFINITY
        } else {
            Timestamp::NEG_INFINITY
        });
    }
    timestamp_from_datetime(truncate_date(input, unit), Time(0))
}

/// `date_trunc(unit, timestamp)`.
fn truncate_timestamp(input: Timestamp, unit: Unit) -> Result<Timestamp> {
    if !input.is_finite() {
        return Ok(input);
    }
    let date = input.to_date();
    let time = input.to_time().0;
    let truncated = |unit_micros: i64| Time(time - time % unit_micros);
    match unit {
        Unit::Hour => timestamp_from_datetime(date, truncated(Interval::MICROS_PER_HOUR)),
        Unit::Minute => timestamp_from_datetime(date, truncated(Interval::MICROS_PER_MINUTE)),
        Unit::Second => timestamp_from_datetime(date, truncated(Interval::MICROS_PER_SEC)),
        Unit::Millisecond => timestamp_from_datetime(date, truncated(Interval::MICROS_PER_MSEC)),
        Unit::Microsecond => Ok(input),
        _ => timestamp_from_datetime(truncate_date(date, unit), Time(0)),
    }
}

/// `date_trunc(unit, interval)`.
fn truncate_interval(mut input: Interval, unit: Unit) -> Interval {
    let truncate_months = |input: &mut Interval, months: i32| {
        input.days = 0;
        input.micros = 0;
        input.months = (input.months / months) * months;
    };
    match unit {
        Unit::Millennium => truncate_months(&mut input, 12_000),
        Unit::Century => truncate_months(&mut input, 1_200),
        Unit::Decade => truncate_months(&mut input, 120),
        Unit::Year | Unit::IsoYear => truncate_months(&mut input, 12),
        Unit::Quarter => truncate_months(&mut input, 3),
        Unit::Month => {
            input.days = 0;
            input.micros = 0;
        }
        Unit::Week => {
            input.micros = 0;
            input.days = (input.days / 7) * 7;
        }
        Unit::Day => input.micros = 0,
        Unit::Hour => input.micros = (input.micros / Interval::MICROS_PER_HOUR) * Interval::MICROS_PER_HOUR,
        Unit::Minute => {
            input.micros = (input.micros / Interval::MICROS_PER_MINUTE) * Interval::MICROS_PER_MINUTE
        }
        Unit::Second => input.micros = (input.micros / Interval::MICROS_PER_SEC) * Interval::MICROS_PER_SEC,
        Unit::Millisecond => {
            input.micros = (input.micros / Interval::MICROS_PER_MSEC) * Interval::MICROS_PER_MSEC
        }
        Unit::Microsecond => {}
    }
    input
}

/// DuckDB's `DateTruncFunction`: a constant unit is parsed once, before any row.
fn truncate_rows<T, R>(args: &DataChunk, result: &mut Vector, op: fn(T, Unit) -> Result<R>) -> Result<()>
where
    T: rustdb_types::NativeType,
    R: rustdb_types::NativeType,
{
    let units = args.column(0);
    if units.is_constant() {
        if units.is_null(0) {
            *result = Vector::constant_null(result.logical_type().clone());
            return Ok(());
        }
        let specifier = crate::values::as_text(&units.get_value(0))?;
        let unit = Unit::parse(specifier.as_bytes())?;
        let values = DataChunk::new(vec![args.column(1).clone()], args.len());
        return unary::<T, R>(&values, result, |value, out| {
            *out = op(value, unit)?;
            Ok(true)
        });
    }
    binary::<Str, T, R>(args, result, |specifier, value, out| {
        *out = op(value, Unit::parse(specifier)?)?;
        Ok(true)
    })
}

/// DuckDB's `DateTruncBind`: a constant unit is validated at bind time.
fn bind_date_trunc(_: &mut ScalarFunction, input: &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    if let Some(Some(value)) = input.constants.first()
        && !value.is_null()
        && input.argument_types[0].id() != LogicalTypeId::Unknown
    {
        // DuckDB picks the statistics callback here, failing on unsupported units.
        let part = Part::parse(&crate::values::as_text(value)?)?;
        if Unit::from_part(part).is_err() {
            return Err(Error::new(
                ErrorKind::NotImplemented,
                "Specifier type not implemented for DATETRUNC statistics",
            ));
        }
    }
    Ok(None)
}

/// `date_trunc`.
pub fn date_trunc_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("date_trunc");
    set.add(
        ScalarFunction::new(
            "date_trunc",
            vec![LogicalType::VARCHAR, LogicalType::TIMESTAMP],
            LogicalType::TIMESTAMP,
            |args, _, result| truncate_rows::<Timestamp, Timestamp>(args, result, truncate_timestamp),
        )
        .with_bind(bind_date_trunc),
    );
    set.add(
        ScalarFunction::new(
            "date_trunc",
            vec![LogicalType::VARCHAR, LogicalType::DATE],
            LogicalType::TIMESTAMP,
            |args, _, result| truncate_rows::<Date, Timestamp>(args, result, truncate_date_to_timestamp),
        )
        .with_bind(bind_date_trunc),
    );
    set.add(ScalarFunction::new(
        "date_trunc",
        vec![LogicalType::VARCHAR, LogicalType::INTERVAL],
        LogicalType::INTERVAL,
        |args, _, result| {
            truncate_rows::<Interval, Interval>(args, result, |value, unit| {
                Ok(truncate_interval(value, unit))
            })
        },
    ));
    set
}
