//! DuckDB's `icu-dateadd.cpp`: TIMESTAMPTZ `+` / `-` INTERVAL and TIMESTAMPTZ `-`
//! TIMESTAMPTZ on the session's calendar (`ICUCalendarAdd`, `ICUCalendarSub`), and
//! `age` (`ICUCalendarAge`).

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionClient, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_functions::scalar::executor::{binary, unary};
use rustdb_types::{DataChunk, Date, Interval, LogicalType, Timestamp, Vector};

use crate::icu4c::calendar::{Status, field};
use crate::icu_datefunc::{self, BindData, MICROS_PER_MSEC, TzCalendar, get_time, set_time, subtract_field};
use crate::icu_datetrunc::current_midnight;
use crate::icu_helpers::{TimestampComponents, get_components};

/// The bind data of the interval functions: the calendar, and the connection whose
/// transaction start `age(x)` reads.
#[derive(Debug)]
struct DateAddBindData {
    data: BindData,
    client: Arc<FunctionClient>,
}

impl FunctionData for DateAddBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn equals(&self, other: &dyn FunctionData) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self.data.equals(&other.data))
    }
}

/// `ICUBindIntervalMonths`: intervals do not work with 13 month calendars.
fn bind_interval_months(_function: &mut ScalarFunction, input: &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    let data = BindData::from_client(input.client);
    let calendar = TzCalendar::new(&data.calendar, &data.cal_setting);
    if !calendar.supports_intervals {
        return Err(Error::not_implemented(
            "INTERVALs do not work with 13 month calendars. Try using DATE_DIFF instead.",
        ));
    }
    Ok(Some(Arc::new(DateAddBindData {
        data,
        client: Arc::clone(input.client),
    })))
}

/// `CalendarAddHour`: adds hours in steps that fit `int32_t`.
fn calendar_add_hour(calendar: &mut crate::icu4c::calendar::Calendar, interval_hour: i64, status: &mut Status) {
    let mut interval_hour = interval_hour;
    if interval_hour >= 0 {
        while interval_hour > 0 {
            let amount = interval_hour.min(i64::from(i32::MAX)) as i32;
            calendar.add(field::HOUR, amount, status);
            interval_hour -= i64::from(i32::MAX);
        }
    } else {
        while interval_hour < 0 {
            let amount = interval_hour.max(i64::from(i32::MIN)) as i32;
            calendar.add(field::HOUR, amount, status);
            interval_hour -= i64::from(i32::MIN);
        }
    }
}

/// `ICUCalendarAdd::Operation(timestamp_t, interval_t)`.
pub(crate) fn add(calendar_p: &mut TzCalendar, timestamp: Timestamp, interval: Interval) -> Result<Timestamp> {
    if !timestamp.is_finite() {
        return Ok(timestamp);
    }
    let calendar = &mut calendar_p.calendar;

    let mut millis = timestamp.0 / MICROS_PER_MSEC;
    let mut micros = timestamp.0 % MICROS_PER_MSEC;

    // Manually move the µs
    micros += interval.micros % MICROS_PER_MSEC;
    if micros >= MICROS_PER_MSEC {
        micros -= MICROS_PER_MSEC;
        millis += 1;
    } else if micros < 0 {
        micros += MICROS_PER_MSEC;
        millis -= 1;
    }

    // Make sure the value is still in range
    let us = millis.checked_mul(MICROS_PER_MSEC).ok_or_else(|| {
        Error::out_of_range(format!(
            "Overflow in multiplication of INT64 ({millis} * {MICROS_PER_MSEC})!"
        ))
    })?;
    let date = Timestamp(us).to_date();
    if i64::from(date.0).checked_mul(Interval::MICROS_PER_DAY).is_none() {
        return Err(Error::conversion("Date out of range in timestamp conversion"));
    }

    // Now use the calendar to add the other parts
    let mut status = Status::ok();
    calendar.set_time_in_millis(millis as f64, &mut status);

    // Break units apart to avoid overflow
    let mut interval_h = interval.micros / MICROS_PER_MSEC;

    let interval_ms = (interval_h % 1000) as i32;
    interval_h /= 1000;

    let interval_s = (interval_h % 60) as i32;
    interval_h /= 60;

    let interval_m = (interval_h % 60) as i32;
    interval_h /= 60;

    if interval.months < 0 || interval.days < 0 || interval.micros < 0 {
        // Add interval fields from lowest to highest (non-ragged to ragged)
        calendar.add(field::MILLISECOND, interval_ms, &mut status);
        calendar.add(field::SECOND, interval_s, &mut status);
        calendar.add(field::MINUTE, interval_m, &mut status);
        calendar_add_hour(calendar, interval_h, &mut status);

        // PG Adds months before days
        calendar.add(field::MONTH, interval.months, &mut status);
        calendar.add(field::DATE, interval.days, &mut status);
    } else {
        // PG Adds months before days
        calendar.add(field::MONTH, interval.months, &mut status);
        calendar.add(field::DATE, interval.days, &mut status);

        // Add interval fields from highest to lowest (ragged to non-ragged)
        calendar_add_hour(calendar, interval_h, &mut status);
        calendar.add(field::MINUTE, interval_m, &mut status);
        calendar.add(field::SECOND, interval_s, &mut status);
        calendar.add(field::MILLISECOND, interval_ms, &mut status);
    }

    get_time(calendar, micros as u64)
}

/// `ICUCalendarSub::Operation(timestamp_t, interval_t)`.
pub(crate) fn sub_interval(calendar: &mut TzCalendar, timestamp: Timestamp, interval: Interval) -> Result<Timestamp> {
    let negated = Interval::new(
        interval.months.wrapping_neg(),
        interval.days.wrapping_neg(),
        interval.micros.wrapping_neg(),
    );
    add(calendar, timestamp, negated)
}

/// The borrow of `ICUCalendarSub` / `ICUCalendarAge`: sets the calendar to `start_date`
/// and returns the adjusted end and the µs below the ms of both ends.
fn set_start(calendar: &mut TzCalendar, start_date: Timestamp, end_date: Timestamp) -> (Timestamp, i64, i64) {
    let start_micros = set_time(&mut calendar.calendar, start_date) as i64;
    let mut end_date = end_date;
    let mut end_micros = end_date.0 % MICROS_PER_MSEC;
    // Borrow 1ms from end_date if we wrap. This works because start_date <= end_date and
    // if the µs are out of order, then there must be an extra ms. (The comparison is
    // unsigned in DuckDB: a negative remainder never borrows.)
    if end_micros >= 0 && start_micros > end_micros {
        end_date.0 -= MICROS_PER_MSEC;
        end_micros += MICROS_PER_MSEC;
    }
    (end_date, start_micros, end_micros)
}

/// The time part of the differences (`Time::FromTime` of the field differences).
fn time_difference(calendar: &mut TzCalendar, end_date: Timestamp, start_micros: i64, end_micros: i64) -> Result<i64> {
    let hour_diff = subtract_field(&mut calendar.calendar, field::HOUR_OF_DAY, end_date)?;
    let min_diff = subtract_field(&mut calendar.calendar, field::MINUTE, end_date)?;
    let sec_diff = subtract_field(&mut calendar.calendar, field::SECOND, end_date)?;
    let ms_diff = subtract_field(&mut calendar.calendar, field::MILLISECOND, end_date)?;
    let micros_diff = (i64::from(ms_diff) * MICROS_PER_MSEC + (end_micros - start_micros)) as i32;
    Ok(((i64::from(hour_diff) * 60 + i64::from(min_diff)) * 60 + i64::from(sec_diff)) * Interval::MICROS_PER_SEC
        + i64::from(micros_diff))
}

/// `ICUCalendarSub::Operation(timestamp_t, timestamp_t)`.
pub(crate) fn sub_timestamps(calendar: &mut TzCalendar, end_date: Timestamp, start_date: Timestamp) -> Result<Interval> {
    if !end_date.is_finite() || !start_date.is_finite() {
        return Err(Error::invalid_input("Cannot subtract infinite timestamps"));
    }
    if start_date > end_date {
        let negated = sub_timestamps(calendar, start_date, end_date)?;
        return Ok(negated.invert());
    }
    let (end_date, start_micros, end_micros) = set_start(calendar, start_date, end_date);

    // Timestamp differences do not use months, so start with days
    let days = subtract_field(&mut calendar.calendar, field::DATE, end_date)?;
    let micros = time_difference(calendar, end_date, start_micros, end_micros)?;
    Ok(Interval::new(0, days, micros))
}

/// `Interval::GetAge(TimestampComponents, TimestampComponents, bool)`.
fn get_age(ts1: TimestampComponents, ts2: TimestampComponents, is_negative: bool) -> Interval {
    const NORMAL_DAYS: [i32; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    const LEAP_DAYS: [i32; 13] = [0, 31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let days_in = |year: i32, month: i32| {
        let table = if Date::is_leap_year(year) { &LEAP_DAYS } else { &NORMAL_DAYS };
        table.get(month as usize).copied().unwrap_or(0)
    };
    // perform the differences
    let mut year_diff = ts1.year.wrapping_sub(ts2.year);
    let mut month_diff = ts1.month - ts2.month;
    let mut day_diff = ts1.day - ts2.day;
    let mut hour_diff = ts1.hour - ts2.hour;
    let mut min_diff = ts1.minute - ts2.minute;
    let mut sec_diff = ts1.second - ts2.second;
    let mut micros_diff = ts1.microsecond - ts2.microsecond;

    // flip sign if necessary
    if is_negative {
        year_diff = year_diff.wrapping_neg();
        month_diff = -month_diff;
        day_diff = -day_diff;
        hour_diff = -hour_diff;
        min_diff = -min_diff;
        sec_diff = -sec_diff;
        micros_diff = -micros_diff;
    }
    // now propagate any negative field into the next higher field
    while micros_diff < 0 {
        micros_diff += 1_000_000;
        sec_diff -= 1;
    }
    while sec_diff < 0 {
        sec_diff += 60;
        min_diff -= 1;
    }
    while min_diff < 0 {
        min_diff += 60;
        hour_diff -= 1;
    }
    while hour_diff < 0 {
        hour_diff += 24;
        day_diff -= 1;
    }
    while day_diff < 0 {
        if is_negative {
            day_diff += days_in(ts1.year, ts1.month);
        } else {
            day_diff += days_in(ts2.year, ts2.month);
        }
        month_diff -= 1;
    }
    while month_diff < 0 {
        month_diff += 12;
        year_diff = year_diff.wrapping_sub(1);
    }

    // recover sign if necessary
    if is_negative {
        year_diff = year_diff.wrapping_neg();
        month_diff = -month_diff;
        day_diff = -day_diff;
        hour_diff = -hour_diff;
        min_diff = -min_diff;
        sec_diff = -sec_diff;
        micros_diff = -micros_diff;
    }
    Interval::new(
        year_diff.wrapping_mul(12).wrapping_add(month_diff),
        day_diff,
        ((i64::from(hour_diff) * 60 + i64::from(min_diff)) * 60 + i64::from(sec_diff)) * Interval::MICROS_PER_SEC
            + i64::from(micros_diff),
    )
}

/// `ICUCalendarAge::Operation`.
fn age(calendar: &mut TzCalendar, end_date: Timestamp, start_date: Timestamp) -> Result<Interval> {
    if calendar.is_gregorian {
        let start_data = get_components(start_date, &mut calendar.calendar)?;
        let end_data = get_components(end_date, &mut calendar.calendar)?;
        return Ok(get_age(end_data, start_data, start_date > end_date));
    }
    // fallback for non-gregorian calendars, since Interval::GetAge does not handle them
    if start_date > end_date {
        let negated = age(calendar, start_date, end_date)?;
        return Ok(negated.invert());
    }
    let (end_date, start_micros, end_micros) = set_start(calendar, start_date, end_date);

    // Lunar calendars have uneven numbers of months, so we just diff months, not years
    let months = subtract_field(&mut calendar.calendar, field::MONTH, end_date)?;
    let days = subtract_field(&mut calendar.calendar, field::DATE, end_date)?;
    let micros = time_difference(calendar, end_date, start_micros, end_micros)?;
    Ok(Interval::new(months, days, micros))
}

/// The calendar of a call (`TZCalendar calendar(*info.calendar, info.cal_setting)`).
fn tz_calendar(state: &FunctionState) -> Result<(TzCalendar, Arc<FunctionClient>)> {
    let info: &DateAddBindData = icu_datefunc::bind_data(state.bind_data.as_ref())?;
    Ok((
        TzCalendar::new(&info.data.calendar, &info.data.cal_setting),
        Arc::clone(&info.client),
    ))
}

/// One `(TA, TB) -> TR` overload with `ICUBindIntervalMonths`.
fn binary_function(
    name: &str,
    left: LogicalType,
    right: LogicalType,
    result_type: LogicalType,
    kernel: fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()>,
) -> ScalarFunction {
    ScalarFunction::new(name, vec![left, right], result_type, kernel).with_bind(bind_interval_months)
}

fn add_timestamp_interval(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let (mut calendar, _) = tz_calendar(state)?;
    binary::<Timestamp, Interval, Timestamp>(args, result, |left, right, out| {
        *out = add(&mut calendar, left, right)?;
        Ok(true)
    })
}

fn add_interval_timestamp(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let (mut calendar, _) = tz_calendar(state)?;
    binary::<Interval, Timestamp, Timestamp>(args, result, |left, right, out| {
        *out = add(&mut calendar, right, left)?;
        Ok(true)
    })
}

fn sub_timestamp_interval(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let (mut calendar, _) = tz_calendar(state)?;
    binary::<Timestamp, Interval, Timestamp>(args, result, |left, right, out| {
        *out = sub_interval(&mut calendar, left, right)?;
        Ok(true)
    })
}

fn sub_timestamp_timestamp(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let (mut calendar, _) = tz_calendar(state)?;
    binary::<Timestamp, Timestamp, Interval>(args, result, |left, right, out| {
        *out = sub_timestamps(&mut calendar, left, right)?;
        Ok(true)
    })
}

fn age_binary(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let (mut calendar, _) = tz_calendar(state)?;
    binary::<Timestamp, Timestamp, Interval>(args, result, |left, right, out| {
        *out = age(&mut calendar, left, right)?;
        Ok(true)
    })
}

/// `ICUDateAdd::ExecuteUnary`: subtracts the argument from the current date (at
/// midnight).
fn age_unary(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let (mut calendar, client) = tz_calendar(state)?;
    let now = Timestamp(client.transaction_start_micros());
    let end_date = current_midnight(&mut calendar.calendar, now)?;
    unary::<Timestamp, Interval>(args, result, |start_date, out| {
        *out = age(&mut calendar, end_date, start_date)?;
        Ok(true)
    })
}

/// `RegisterICUDateAddFunctions`: `+`, `-` and `age`.
pub(crate) fn date_add_functions() -> Vec<ScalarFunctionSet> {
    let tstz = || LogicalType::TIMESTAMP_TZ;
    let mut plus = ScalarFunctionSet::new("+");
    plus.add(binary_function("+", tstz(), LogicalType::INTERVAL, tstz(), add_timestamp_interval));
    plus.add(binary_function("+", LogicalType::INTERVAL, tstz(), tstz(), add_interval_timestamp));

    let mut minus = ScalarFunctionSet::new("-");
    minus.add(binary_function("-", tstz(), LogicalType::INTERVAL, tstz(), sub_timestamp_interval));
    minus.add(binary_function("-", tstz(), tstz(), LogicalType::INTERVAL, sub_timestamp_timestamp));

    let mut age_set = ScalarFunctionSet::new("age");
    age_set.add(binary_function("age", tstz(), tstz(), LogicalType::INTERVAL, age_binary));
    age_set.add(
        ScalarFunction::new("age", vec![tstz()], LogicalType::INTERVAL, age_unary).with_bind(bind_interval_months),
    );
    vec![plus, minus, age_set]
}
