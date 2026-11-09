//! Casts from and to `DATE`, `TIME`, `TIME_NS`, `TIME WITH TIME ZONE`, `TIMESTAMP` (all units and
//! WITH TIME ZONE) and INTERVAL (DuckDB's `time_casts.cpp` and the temporal operators of
//! `cast_operators.cpp`). The calendar arithmetic lives in `roastedduck_types`.

use roastedduck_common::{Error, Result};
use roastedduck_types::interval::IntervalParseError;
use roastedduck_types::timestamp::{self as ts, TimestampParseError};
use roastedduck_types::{
    Date, Interval, LogicalType, LogicalTypeId, NativeType, PhysicalType, Time, TimeNs, TimeTz, Timestamp,
};

use super::BoundCastInfo;
use super::executor::{RowError, RowResult, try_map_strings, try_map_values, try_values_to_string};
use super::string::could_not_convert;

/// `Type INT32 with value 5881580-07-10 can't be cast to the destination type INT64`
/// (DuckDB's `CastExceptionText` for non-numeric values).
fn cannot_cast(source: PhysicalType, text: &str, target: PhysicalType) -> String {
    format!(
        "Type {} with value {text} can't be cast to the destination type {}",
        source.name(),
        target.name()
    )
}

/// The text of a DATE for error messages.
fn date_text(date: Date) -> String {
    let mut out = Vec::new();
    date.write_to(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

/// The text of a TIMESTAMP for error messages.
fn timestamp_text(timestamp: Timestamp) -> String {
    let mut out = Vec::new();
    match timestamp.write_to(&mut out) {
        Ok(()) => String::from_utf8_lossy(&out).into_owned(),
        Err(_) => timestamp.0.to_string(),
    }
}

/// A kernel over fixed-width values with an infallible conversion.
fn map<I: NativeType, O: NativeType>(convert: fn(I) -> O) -> BoundCastInfo {
    BoundCastInfo::new(move |source, result, count, params| {
        try_map_values::<I, O>(source, result, count, params, |value| Ok(convert(value)))
    })
}

/// A kernel whose conversion raises its errors even under `TRY_CAST` (DuckDB throws them).
fn map_fatal<I: NativeType, O: NativeType>(convert: fn(I) -> Result<O>) -> BoundCastInfo {
    BoundCastInfo::new(move |source, result, count, params| {
        try_map_values::<I, O>(source, result, count, params, |value| {
            convert(value).map_err(RowError::Fatal)
        })
    })
}

/// A kernel rendering values as text; rendering errors are raised (DuckDB throws them).
fn to_text<I: NativeType>(write: fn(I, &mut Vec<u8>) -> Result<()>) -> BoundCastInfo {
    BoundCastInfo::new(move |source, result, count, params| {
        try_values_to_string::<I>(source, result, count, params, |value, out| {
            write(value, out).map_err(RowError::Fatal)
        })
    })
}

/// DATE -> TIMESTAMP variants: out of range dates fail with DuckDB's generic message.
fn date_to_timestamp(convert: fn(Date) -> Option<i64>) -> BoundCastInfo {
    BoundCastInfo::new(move |source, result, count, params| {
        try_map_values::<Date, i64>(source, result, count, params, |date| {
            convert(date).ok_or_else(|| {
                RowError::Invalid(cannot_cast(
                    PhysicalType::Int32,
                    &date_text(date),
                    PhysicalType::Int64,
                ))
            })
        })
    })
}

/// TIMESTAMP -> TIME/TIMETZ: infinite timestamps raise `Invalid Input Error` even under
/// `TRY_CAST` (DuckDB's `Cast::Operation`).
fn timestamp_time_of_day<O: NativeType>(convert: fn(Time) -> O) -> BoundCastInfo {
    BoundCastInfo::new(move |source, result, count, params| {
        try_map_values::<Timestamp, O>(source, result, count, params, |timestamp| {
            if timestamp.is_finite() {
                Ok(convert(timestamp.to_time()))
            } else {
                Err(RowError::Fatal(Error::invalid_input(cannot_cast(
                    PhysicalType::Int64,
                    &timestamp_text(timestamp),
                    PhysicalType::Int64,
                ))))
            }
        })
    })
}

fn date_cast(target: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    Some(match target.id() {
        L::Varchar => to_text::<Date>(|date, out| {
            date.write_to(out);
            Ok(())
        }),
        L::Timestamp | L::TimestampTz => date_to_timestamp(|date| Timestamp::from_date(date).map(|t| t.0)),
        L::TimestampNs => date_to_timestamp(ts::date_to_ns),
        L::TimestampSec => date_to_timestamp(ts::date_to_sec),
        L::TimestampMs => date_to_timestamp(ts::date_to_ms),
        _ => return None,
    })
}

fn time_cast(source: &LogicalType, target: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    Some(match (source.id(), target.id()) {
        (L::Time, L::Varchar) => to_text::<Time>(|time, out| {
            time.write_to(out);
            Ok(())
        }),
        (L::Time, L::TimeNs) => map_fatal::<Time, TimeNs>(Time::to_time_ns),
        (L::Time, L::TimeTz) => map::<Time, TimeTz>(TimeTz::from_time),
        (L::TimeNs, L::Varchar) => to_text::<TimeNs>(|time, out| {
            time.write_to(out);
            Ok(())
        }),
        (L::TimeNs, L::Time) => map::<TimeNs, Time>(TimeNs::to_time),
        (L::TimeTz, L::Varchar) => to_text::<TimeTz>(|time, out| {
            time.write_to(out);
            Ok(())
        }),
        (L::TimeTz, L::Time) => map::<TimeTz, Time>(TimeTz::time),
        _ => return None,
    })
}

fn timestamp_cast(source: &LogicalType, target: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    let with_tz = source.id() == L::TimestampTz;
    Some(match target.id() {
        L::Varchar if with_tz => to_text::<Timestamp>(Timestamp::write_tz_to),
        L::Varchar => to_text::<Timestamp>(Timestamp::write_to),
        L::Date if !with_tz => map::<Timestamp, Date>(Timestamp::to_date),
        L::Time if !with_tz => timestamp_time_of_day::<Time>(|time| time),
        L::TimeTz => timestamp_time_of_day::<TimeTz>(TimeTz::from_time),
        L::Timestamp | L::TimestampTz => BoundCastInfo::reinterpret(),
        L::TimestampNs => map_fatal::<Timestamp, i64>(ts::us_to_ns),
        L::TimestampMs => map::<Timestamp, i64>(ts::us_to_ms),
        L::TimestampSec => map::<Timestamp, i64>(ts::us_to_sec),
        _ => return None,
    })
}

fn timestamp_ns_cast(target: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    Some(match target.id() {
        L::Varchar => to_text::<i64>(Timestamp::write_ns_to),
        L::Date => map::<i64, Date>(ts::ns_to_date),
        L::Time => map_fatal::<i64, Time>(ts::ns_to_time),
        L::TimeNs => map_fatal::<i64, TimeNs>(ts::ns_to_time_ns),
        L::Timestamp | L::TimestampTz => map::<i64, Timestamp>(ts::ns_to_us),
        _ => return None,
    })
}

fn timestamp_ms_cast(target: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    Some(match target.id() {
        L::Varchar => to_text::<i64>(ts::write_ms_to),
        L::Date => map_fatal::<i64, Date>(ts::ms_to_date),
        L::Time => map_fatal::<i64, Time>(ts::ms_to_time),
        L::Timestamp | L::TimestampTz => map_fatal::<i64, Timestamp>(ts::ms_to_us),
        L::TimestampNs => map_fatal::<i64, i64>(ts::ms_to_ns),
        _ => return None,
    })
}

fn timestamp_sec_cast(target: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    Some(match target.id() {
        L::Varchar => to_text::<i64>(ts::write_sec_to),
        L::Date => map_fatal::<i64, Date>(ts::sec_to_date),
        L::Time => map_fatal::<i64, Time>(ts::sec_to_time),
        L::TimestampMs => map_fatal::<i64, i64>(ts::sec_to_ms),
        L::Timestamp | L::TimestampTz => map_fatal::<i64, Timestamp>(ts::sec_to_us),
        L::TimestampNs => map_fatal::<i64, i64>(ts::sec_to_ns),
        _ => return None,
    })
}

/// Casts whose source is a temporal type (DuckDB's `DateCastSwitch`, `TimeCastSwitch`,
/// `Timestamp*CastSwitch` and `IntervalCastSwitch`).
pub(super) fn cast_switch(source: &LogicalType, target: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    match source.id() {
        L::Date => date_cast(target),
        L::Time | L::TimeNs | L::TimeTz => time_cast(source, target),
        L::Timestamp | L::TimestampTz => timestamp_cast(source, target),
        L::TimestampNs => timestamp_ns_cast(target),
        L::TimestampMs => timestamp_ms_cast(target),
        L::TimestampSec => timestamp_sec_cast(target),
        L::Interval if target.id() == L::Varchar => Some(to_text::<Interval>(|interval, out| {
            interval.write_to(out);
            Ok(())
        })),
        _ => None,
    }
}

/// A VARCHAR -> temporal kernel failing with a per-row message.
fn parse_kernel<O: NativeType>(parse: fn(&[u8], bool) -> RowResult<O>) -> BoundCastInfo {
    BoundCastInfo::new(move |source, result, count, params| {
        let strict = params.strict;
        try_map_strings::<O>(source, result, count, params, |text| parse(text, strict))
    })
}

fn parse_timestamp(text: &[u8], use_offset: bool) -> RowResult<Timestamp> {
    Timestamp::parse(text, use_offset)
        .map(|(timestamp, _)| timestamp)
        .map_err(|error: TimestampParseError| RowError::Invalid(error.message(text)))
}

/// VARCHAR -> temporal casts.
pub(super) fn from_string(target: &LogicalType) -> Option<BoundCastInfo> {
    use LogicalTypeId as L;
    Some(match target.id() {
        L::Date => parse_kernel::<Date>(|text, strict| {
            Date::parse(text, strict).map_err(|error| RowError::Invalid(error.message(text)))
        }),
        L::Time => parse_kernel::<Time>(|text, strict| {
            Time::parse(text, strict).ok_or_else(|| RowError::Invalid(Time::conversion_error(text)))
        }),
        L::TimeNs => parse_kernel::<TimeNs>(|text, strict| {
            TimeNs::parse(text, strict).ok_or_else(|| RowError::Invalid(Time::conversion_error(text)))
        }),
        L::TimeTz => parse_kernel::<TimeTz>(|text, strict| {
            TimeTz::parse(text, strict).ok_or_else(|| RowError::Invalid(Time::conversion_error(text)))
        }),
        L::Timestamp => parse_kernel::<Timestamp>(|text, _| parse_timestamp(text, false)),
        L::TimestampTz => parse_kernel::<Timestamp>(|text, _| parse_timestamp(text, true)),
        // These report DuckDB's generic message, naming the physical type.
        L::TimestampNs => parse_kernel::<i64>(|text, _| {
            Timestamp::parse_ns(text).map_err(|_| could_not_convert(text, PhysicalType::Int64))
        }),
        L::TimestampMs => parse_kernel::<i64>(|text, _| {
            ts::parse_ms(text).ok_or_else(|| could_not_convert(text, PhysicalType::Int64))
        }),
        L::TimestampSec => parse_kernel::<i64>(|text, _| {
            ts::parse_sec(text).ok_or_else(|| could_not_convert(text, PhysicalType::Int64))
        }),
        L::Interval => parse_kernel::<Interval>(|text, _| {
            Interval::parse_detailed(text).map_err(|error| match error {
                IntervalParseError::Reported(error) => {
                    RowError::InvalidBytes(error.raw_message_bytes().to_vec())
                }
                IntervalParseError::Cast(error) => RowError::Error(error),
                IntervalParseError::Unconditional(error) => RowError::Fatal(error),
            })
        }),
        _ => return None,
    })
}
