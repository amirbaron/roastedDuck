//! DuckDB's `icu-datetrunc.cpp`: `date_trunc(part, TIMESTAMPTZ)` truncating in the
//! session's calendar and time zone, and `ICUDateFunc::TruncationFactory` /
//! `CurrentMidnight`.

use rustdb_common::{Error, Result};
use rustdb_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_functions::scalar::date::date_part::Part;
use rustdb_functions::scalar::executor::{Str, binary, str_at, unary};
use rustdb_types::{DataChunk, LogicalType, Timestamp, Vector};

use crate::icu_datefunc::{self, BindData, CalendarPtr, extract_field, get_time, get_time_unsafe, set_time};
use crate::icu4c::calendar::{Calendar, field};
use crate::icu4c::grego::MONDAY;

/// `ICUDateFunc::part_trunc_t`: truncates the calendar's time to a part's precision.
pub(crate) type PartTrunc = fn(&mut Calendar, &mut u64) -> Result<()>;

/// `ICUDateTrunc::PreserveOffsets`: we have to extract _everything_ before setting
/// anything, otherwise ICU will clear the stamps; this also means we must call this
/// method first. Forces reuse of offsets when reassembling truncated sub-hour times.
fn preserve_offsets(calendar: &mut Calendar) -> Result<()> {
    let zone_offset = extract_field(calendar, field::ZONE_OFFSET)?;
    let dst_offset = extract_field(calendar, field::DST_OFFSET)?;
    calendar.set(field::ZONE_OFFSET, zone_offset);
    calendar.set(field::DST_OFFSET, dst_offset);
    Ok(())
}

fn trunc_microsecond_internal(_calendar: &mut Calendar, _micros: &mut u64) {}

fn trunc_microsecond(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    preserve_offsets(calendar)?;
    trunc_microsecond_internal(calendar, micros);
    Ok(())
}

fn trunc_millisecond_internal(calendar: &mut Calendar, micros: &mut u64) {
    trunc_microsecond_internal(calendar, micros);
    *micros = 0;
}

fn trunc_millisecond(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    preserve_offsets(calendar)?;
    trunc_millisecond_internal(calendar, micros);
    Ok(())
}

fn trunc_second_internal(calendar: &mut Calendar, micros: &mut u64) {
    trunc_millisecond_internal(calendar, micros);
    calendar.set(field::MILLISECOND, 0);
}

fn trunc_second(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    preserve_offsets(calendar)?;
    trunc_second_internal(calendar, micros);
    Ok(())
}

fn trunc_minute_internal(calendar: &mut Calendar, micros: &mut u64) {
    trunc_second_internal(calendar, micros);
    calendar.set(field::SECOND, 0);
}

fn trunc_minute(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    preserve_offsets(calendar)?;
    trunc_minute_internal(calendar, micros);
    Ok(())
}

#[allow(clippy::unnecessary_wraps)] // the signature of the function pointer it is used as
fn trunc_hour(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    trunc_minute_internal(calendar, micros);
    calendar.set(field::MINUTE, 0);
    Ok(())
}

/// `ICUDateTrunc::TruncDay`.
pub(crate) fn trunc_day(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    trunc_hour(calendar, micros)?;
    calendar.set(field::HOUR_OF_DAY, 0);
    Ok(())
}

fn trunc_week(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    calendar.set_first_day_of_week(MONDAY);
    calendar.set_minimal_days_in_first_week(4);
    trunc_day(calendar, micros)?;
    calendar.set(field::DAY_OF_WEEK, MONDAY);
    Ok(())
}

fn trunc_month(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    trunc_day(calendar, micros)?;
    calendar.set(field::DATE, 1);
    Ok(())
}

fn trunc_quarter(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    trunc_month(calendar, micros)?;
    let mm = extract_field(calendar, field::MONTH)?;
    calendar.set(field::MONTH, (mm / 3) * 3);
    Ok(())
}

fn trunc_year(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    trunc_month(calendar, micros)?;
    calendar.set(field::MONTH, 0);
    Ok(())
}

fn trunc_iso_year(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    trunc_week(calendar, micros)?;
    calendar.set(field::WEEK_OF_YEAR, 1);
    Ok(())
}

fn trunc_decade(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    trunc_year(calendar, micros)?;
    let yyyy = extract_field(calendar, field::YEAR)? / 10;
    calendar.set(field::YEAR, yyyy * 10);
    Ok(())
}

fn trunc_century(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    trunc_year(calendar, micros)?;
    let yyyy = extract_field(calendar, field::YEAR)? / 100;
    calendar.set(field::YEAR, yyyy * 100);
    Ok(())
}

fn trunc_millenium(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    trunc_year(calendar, micros)?;
    let yyyy = extract_field(calendar, field::YEAR)? / 1000;
    calendar.set(field::YEAR, yyyy * 1000);
    Ok(())
}

fn trunc_era(calendar: &mut Calendar, micros: &mut u64) -> Result<()> {
    trunc_year(calendar, micros)?;
    let era = extract_field(calendar, field::ERA)?;
    calendar.set(field::YEAR, 0);
    calendar.set(field::ERA, era);
    Ok(())
}

/// `ICUDateFunc::TruncationFactory`.
pub(crate) fn truncation_factory(part: Part) -> Result<PartTrunc> {
    Ok(match part {
        Part::Era => trunc_era,
        Part::Millennium => trunc_millenium,
        Part::Century => trunc_century,
        Part::Decade => trunc_decade,
        Part::Year => trunc_year,
        Part::Quarter => trunc_quarter,
        Part::Month => trunc_month,
        Part::Week | Part::YearWeek => trunc_week,
        Part::IsoYear => trunc_iso_year,
        Part::Day | Part::Dow | Part::IsoDow | Part::Doy | Part::JulianDay => trunc_day,
        Part::Hour => trunc_hour,
        Part::Minute => trunc_minute,
        Part::Second | Part::Epoch => trunc_second,
        Part::Milliseconds => trunc_millisecond,
        Part::Microseconds => trunc_microsecond,
        _ => {
            return Err(Error::not_implemented(
                "Specifier type not implemented for ICU DATETRUNC",
            ));
        }
    })
}

/// `ICUDateFunc::CurrentMidnight`: the start of the current transaction's day.
pub(crate) fn current_midnight(calendar: &mut Calendar, current_timestamp: Timestamp) -> Result<Timestamp> {
    let mut current_micros = set_time(calendar, current_timestamp);
    trunc_day(calendar, &mut current_micros)?;
    get_time(calendar, 0)
}

/// Truncates one finite value.
fn truncate(calendar: &mut Calendar, truncator: PartTrunc, input: Timestamp) -> Result<Timestamp> {
    if !input.is_finite() {
        return Ok(input);
    }
    let mut micros = set_time(calendar, input);
    truncator(calendar, &mut micros)?;
    get_time_unsafe(calendar, micros)
}

/// `ICUDateTrunc::ICUDateTruncFunction`.
fn date_trunc_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let info: &BindData = icu_datefunc::bind_data(state.bind_data.as_ref())?;
    let mut calendar: CalendarPtr = info.calendar.clone();
    let part_arg = args.column(0);
    if part_arg.is_constant() {
        // Common case of constant part.
        let Some(specifier) = str_at(part_arg, 0) else {
            *result = Vector::constant_null(LogicalType::TIMESTAMP_TZ);
            return Ok(());
        };
        let truncator = truncation_factory(Part::parse(&String::from_utf8_lossy(specifier))?)?;
        let values = DataChunk::new(vec![args.column(1).clone()], args.len());
        unary::<Timestamp, Timestamp>(&values, result, |input, out| {
            *out = truncate(&mut calendar, truncator, input)?;
            Ok(true)
        })
    } else {
        binary::<Str, Timestamp, Timestamp>(args, result, |specifier, input, out| {
            if input.is_finite() {
                let truncator = truncation_factory(Part::parse(&String::from_utf8_lossy(specifier))?)?;
                *out = truncate(&mut calendar, truncator, input)?;
            } else {
                *out = input;
            }
            Ok(true)
        })
    }
}

/// `date_trunc` / `datetrunc` (`ICUDateTrunc::AddBinaryTimestampFunction`).
pub(crate) fn date_trunc_functions() -> Vec<ScalarFunctionSet> {
    ["date_trunc", "datetrunc"]
        .into_iter()
        .map(|name| {
            let mut set = ScalarFunctionSet::new(name);
            set.add(
                ScalarFunction::new(
                    name,
                    vec![LogicalType::VARCHAR, LogicalType::TIMESTAMP_TZ],
                    LogicalType::TIMESTAMP_TZ,
                    date_trunc_rows,
                )
                .with_bind(icu_datefunc::bind),
            );
            set
        })
        .collect()
}
