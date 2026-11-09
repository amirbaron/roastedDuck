//! DuckDB's `icu-datesub.cpp`: `date_sub` / `datesub` (whole parts between two
//! instants, `ICUCalendarSub`) and `date_diff` / `datediff` (part boundaries crossed,
//! `ICUCalendarDiff`) over TIMESTAMPTZ.

use roastedduck_common::{Error, Result};
use roastedduck_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use roastedduck_functions::scalar::date::date_part::Part;
use roastedduck_functions::scalar::executor::{Str, binary, str_at, ternary};
use roastedduck_types::{DataChunk, Interval, LogicalType, Timestamp, Vector};

use crate::icu_datefunc::{self, BindData, CalendarPtr, get_time_unsafe, set_time, subtract_field};
use crate::icu_datetrunc::{PartTrunc, truncation_factory};
use crate::icu4c::calendar::{Calendar, field};
use crate::icu4c::grego::MONDAY;

/// `ICUDateFunc::part_sub_t`: the number of parts between two instants.
pub(crate) type PartSub = fn(&mut Calendar, Timestamp, Timestamp) -> Result<i64>;

// ICU only has 32 bit precision for date parts, so it can overflow a high resolution.
// Since there is no difference between ICU and the obvious calculations, we make these
// using the DuckDB internal type.
#[allow(clippy::unnecessary_wraps)] // the signature of the function pointer it is used as
fn subtract_microsecond(_calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    Ok(end_date.0.wrapping_sub(start_date.0))
}

fn subtract_millisecond(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    Ok(subtract_microsecond(calendar, start_date, end_date)? / Interval::MICROS_PER_MSEC)
}

fn subtract_second(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    Ok(subtract_microsecond(calendar, start_date, end_date)? / Interval::MICROS_PER_SEC)
}

fn subtract_minute(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    Ok(subtract_microsecond(calendar, start_date, end_date)? / Interval::MICROS_PER_MINUTE)
}

fn subtract_hour(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    set_time(calendar, start_date);
    Ok(i64::from(subtract_field(calendar, field::HOUR_OF_DAY, end_date)?))
}

fn subtract_day(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    set_time(calendar, start_date);
    Ok(i64::from(subtract_field(calendar, field::DATE, end_date)?))
}

fn subtract_week(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    calendar.set_first_day_of_week(MONDAY);
    calendar.set_minimal_days_in_first_week(4);
    set_time(calendar, start_date);
    Ok(i64::from(subtract_field(
        calendar,
        field::WEEK_OF_YEAR,
        end_date,
    )?))
}

fn subtract_month(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    set_time(calendar, start_date);
    Ok(i64::from(subtract_field(calendar, field::MONTH, end_date)?))
}

fn subtract_quarter(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    // No ICU part for this, so do it manually. This will not work for lunar calendars!
    Ok(subtract_month(calendar, start_date, end_date)? / 3)
}

fn subtract_year(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    set_time(calendar, start_date);
    Ok(i64::from(subtract_field(calendar, field::YEAR, end_date)?))
}

fn subtract_iso_year(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    calendar.set_first_day_of_week(MONDAY);
    calendar.set_minimal_days_in_first_week(4);
    set_time(calendar, start_date);
    Ok(i64::from(subtract_field(calendar, field::YEAR_WOY, end_date)?))
}

fn subtract_decade(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    // No ICU part for this, so do it manually.
    Ok(subtract_year(calendar, start_date, end_date)? / 10)
}

fn subtract_century(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    // No ICU part for this, so do it manually.
    Ok(subtract_year(calendar, start_date, end_date)? / 100)
}

fn subtract_millenium(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    // No ICU part for this, so do it manually.
    Ok(subtract_year(calendar, start_date, end_date)? / 1000)
}

fn subtract_era(calendar: &mut Calendar, start_date: Timestamp, end_date: Timestamp) -> Result<i64> {
    set_time(calendar, start_date);
    Ok(i64::from(subtract_field(calendar, field::ERA, end_date)?))
}

/// `ICUDateFunc::SubtractFactory`.
pub(crate) fn subtract_factory(part: Part) -> Result<PartSub> {
    Ok(match part {
        Part::Millennium => subtract_millenium,
        Part::Century => subtract_century,
        Part::Decade => subtract_decade,
        Part::Year => subtract_year,
        Part::Quarter => subtract_quarter,
        Part::Month => subtract_month,
        Part::Week | Part::YearWeek => subtract_week,
        Part::IsoYear => subtract_iso_year,
        Part::Day | Part::Dow | Part::IsoDow | Part::Doy | Part::JulianDay => subtract_day,
        Part::Hour => subtract_hour,
        Part::Minute => subtract_minute,
        Part::Second | Part::Epoch => subtract_second,
        Part::Milliseconds => subtract_millisecond,
        Part::Microseconds => subtract_microsecond,
        Part::Era => subtract_era,
        _ => {
            return Err(Error::not_implemented(
                "Specifier type not implemented for ICU subtraction",
            ));
        }
    })
}

/// `ICUCalendarDiff::DiffTruncationFactory`: weeks are computed without anchors.
fn diff_truncation_factory(part: Part) -> Result<PartTrunc> {
    match part {
        Part::Week => truncation_factory(Part::Day),
        _ => truncation_factory(part),
    }
}

/// `ICUCalendarDiff::DifferenceFunc`: truncates both arguments, then subtracts.
fn difference(
    calendar: &mut Calendar,
    start_date: Timestamp,
    end_date: Timestamp,
    trunc_func: PartTrunc,
    sub_func: PartSub,
) -> Result<i64> {
    // Truncate the two arguments. This is safe because we will stay in range
    let mut micros = set_time(calendar, start_date);
    trunc_func(calendar, &mut micros)?;
    let start_date = get_time_unsafe(calendar, micros)?;

    let mut micros = set_time(calendar, end_date);
    trunc_func(calendar, &mut micros)?;
    let end_date = get_time_unsafe(calendar, micros)?;

    // Now use ICU difference
    sub_func(calendar, start_date, end_date)
}

/// The shared row loop of `date_sub` and `date_diff`: `op(calendar, part, start, end)`.
fn part_rows(
    args: &DataChunk,
    state: &mut FunctionState,
    result: &mut Vector,
    op: fn(&mut Calendar, Part, Timestamp, Timestamp) -> Result<i64>,
) -> Result<()> {
    let info: &BindData = icu_datefunc::bind_data(state.bind_data.as_ref())?;
    let mut calendar: CalendarPtr = info.calendar.clone();
    let part_arg = args.column(0);
    if part_arg.is_constant() {
        // Common case of constant part.
        let Some(specifier) = str_at(part_arg, 0) else {
            *result = Vector::constant_null(LogicalType::BIGINT);
            return Ok(());
        };
        let part = Part::parse(&String::from_utf8_lossy(specifier))?;
        let dates = DataChunk::new(vec![args.column(1).clone(), args.column(2).clone()], args.len());
        binary::<Timestamp, Timestamp, i64>(&dates, result, |start_date, end_date, out| {
            if start_date.is_finite() && end_date.is_finite() {
                *out = op(&mut calendar, part, start_date, end_date)?;
                Ok(true)
            } else {
                Ok(false)
            }
        })
    } else {
        ternary::<Str, Timestamp, Timestamp, i64>(args, result, |specifier, start_date, end_date, out| {
            if start_date.is_finite() && end_date.is_finite() {
                let part = Part::parse(&String::from_utf8_lossy(specifier))?;
                *out = op(&mut calendar, part, start_date, end_date)?;
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }
}

/// `ICUCalendarSub::ICUDateSubFunction`.
fn date_sub_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    part_rows(args, state, result, |calendar, part, start_date, end_date| {
        let part_func = subtract_factory(part)?;
        part_func(calendar, start_date, end_date)
    })
}

/// `ICUCalendarDiff::ICUDateDiffFunction`.
fn date_diff_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    part_rows(args, state, result, |calendar, part, start_date, end_date| {
        let trunc_func = diff_truncation_factory(part)?;
        let sub_func = subtract_factory(part)?;
        difference(calendar, start_date, end_date, trunc_func, sub_func)
    })
}

/// One `(VARCHAR, TIMESTAMPTZ, TIMESTAMPTZ) -> BIGINT` set.
fn part_set(
    name: &str,
    kernel: fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()>,
) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(
        ScalarFunction::new(
            name,
            vec![
                LogicalType::VARCHAR,
                LogicalType::TIMESTAMP_TZ,
                LogicalType::TIMESTAMP_TZ,
            ],
            LogicalType::BIGINT,
            kernel,
        )
        .with_bind(icu_datefunc::bind),
    );
    set
}

/// `RegisterICUDateSubFunctions`.
pub(crate) fn date_sub_functions() -> Vec<ScalarFunctionSet> {
    vec![
        part_set("date_sub", date_sub_rows),
        part_set("datesub", date_sub_rows),
        part_set("date_diff", date_diff_rows),
        part_set("datediff", date_diff_rows),
    ]
}
