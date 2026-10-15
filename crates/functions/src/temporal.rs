//! Temporal arithmetic shared by the date/time functions and aggregates (pieces of DuckDB's
//! `Interval`, `Date`, `Time` and `Timestamp` classes and the temporal `AddOperator`/
//! `SubtractOperator`/`MultiplyOperator` specialisations).

use rustdb_common::{Error, Result};
use rustdb_types::{Date, Interval, Time, TimeTz, Timestamp};

/// Microseconds in a day.
pub(crate) const MICROS_PER_DAY: i64 = Interval::MICROS_PER_DAY;

fn date_out_of_range() -> Error {
    Error::out_of_range("Date out of range")
}

/// `time + interval` wrapping around midnight; returns the new time and the number of days
/// carried (DuckDB's `Interval::Add(dtime_t, interval_t, date_t &)`). Only the micros of the
/// interval apply.
pub(crate) fn time_add_interval(time: Time, interval: Interval) -> (Time, i32) {
    let diff = interval.micros - (interval.micros / MICROS_PER_DAY) * MICROS_PER_DAY;
    let mut micros = time.0 + diff;
    let mut carry = 0;
    if micros >= MICROS_PER_DAY {
        micros -= MICROS_PER_DAY;
        carry = 1;
    } else if micros < 0 {
        micros += MICROS_PER_DAY;
        carry = -1;
    }
    (Time(micros), carry)
}

/// The UTC time of day of a TIME WITH TIME ZONE in microseconds (DuckDB's
/// `Time::NormalizeTimeTZ`).
pub(crate) fn time_tz_normalized_micros(value: TimeTz) -> i64 {
    let offset = Interval::new(0, 0, -i64::from(value.offset()) * Interval::MICROS_PER_SEC);
    time_add_interval(value.time(), offset).0.0
}

/// `date + days` (DuckDB's `AddOperator<date_t, int32_t>`): infinities stay infinite.
pub(crate) fn date_add_days(date: Date, days: i32) -> Result<Date> {
    if !date.is_finite() {
        return Ok(date);
    }
    let result = date.0.checked_add(days).map(Date).ok_or_else(date_out_of_range)?;
    if result.is_finite() {
        Ok(result)
    } else {
        Err(date_out_of_range())
    }
}

/// `date - days` (DuckDB's `SubtractOperator<date_t, int32_t>`).
pub(crate) fn date_subtract_days(date: Date, days: i32) -> Result<Date> {
    if !date.is_finite() {
        return Ok(date);
    }
    let result = date.0.checked_sub(days).map(Date).ok_or_else(date_out_of_range)?;
    if result.is_finite() {
        Ok(result)
    } else {
        Err(date_out_of_range())
    }
}

/// `Date::FromDate` with its conversion error.
pub(crate) fn date_from_ymd(year: i32, month: i32, day: i32) -> Result<Date> {
    Date::from_ymd(year, month, day)
        .ok_or_else(|| Error::conversion(format!("Date out of range: {year}-{month}-{day}")))
}

/// `date + interval` on the calendar (DuckDB's `Interval::Add(date_t, interval_t)`).
pub(crate) fn date_add_interval(date: Date, interval: Interval) -> Result<Date> {
    if !date.is_finite() {
        return Ok(date);
    }
    let mut result = if interval.months == 0 {
        date
    } else {
        let (mut year, mut month, day) = date.to_ymd();
        let year_diff = interval.months / 12;
        year += year_diff;
        month += interval.months - year_diff * 12;
        if month > 12 {
            year += 1;
            month -= 12;
        } else if month <= 0 {
            year -= 1;
            month += 12;
        }
        let day = day.min(Date::month_days(year, month).unwrap_or(31));
        date_from_ymd(year, month, day)?
    };
    if interval.days != 0 {
        result = Date(
            result
                .0
                .checked_add(interval.days)
                .ok_or_else(date_out_of_range)?,
        );
    }
    if interval.micros != 0 {
        let days = (interval.micros / MICROS_PER_DAY) as i32;
        result = Date(result.0.checked_add(days).ok_or_else(date_out_of_range)?);
    }
    if !result.is_finite() {
        return Err(date_out_of_range());
    }
    Ok(result)
}

/// `Timestamp::FromDatetime` with its conversion error.
pub(crate) fn timestamp_from_datetime(date: Date, time: Time) -> Result<Timestamp> {
    Timestamp::from_datetime(date, time)
        .ok_or_else(|| Error::conversion("Date and time not in timestamp range"))
}

/// `Timestamp::Convert` with its conversion error.
pub(crate) fn timestamp_to_datetime(timestamp: Timestamp) -> Result<(Date, Time)> {
    timestamp
        .to_datetime()
        .ok_or_else(|| Error::conversion("Date out of range in timestamp conversion"))
}

/// `timestamp + interval` (DuckDB's `Interval::Add(timestamp_t, interval_t)`).
pub(crate) fn timestamp_add_interval(timestamp: Timestamp, interval: Interval) -> Result<Timestamp> {
    if !timestamp.is_finite() {
        return Ok(timestamp);
    }
    let (date, time) = timestamp_to_datetime(timestamp)?;
    let date = date_add_interval(date, interval)?;
    let (time, carry) = time_add_interval(time, interval);
    timestamp_from_datetime(Date(date.0 + carry), time)
}

/// `date + interval` as a TIMESTAMP (DuckDB's `AddOperator<date_t, interval_t, timestamp_t>`).
pub(crate) fn date_add_interval_timestamp(date: Date, interval: Interval) -> Result<Timestamp> {
    if date == Date::INFINITY {
        return Ok(Timestamp::INFINITY);
    }
    if date == Date::NEG_INFINITY {
        return Ok(Timestamp::NEG_INFINITY);
    }
    timestamp_add_interval(timestamp_from_datetime(date, Time(0))?, interval)
}

/// `date + time` (DuckDB's `AddOperator<date_t, dtime_t, timestamp_t>`).
pub(crate) fn date_add_time(date: Date, time: Time) -> Result<Timestamp> {
    if date == Date::INFINITY {
        return Ok(Timestamp::INFINITY);
    }
    if date == Date::NEG_INFINITY {
        return Ok(Timestamp::NEG_INFINITY);
    }
    Timestamp::from_datetime(date, time).ok_or_else(|| Error::out_of_range("Timestamp out of range"))
}

/// `date + timetz` (DuckDB's `AddOperator<date_t, dtime_tz_t, timestamp_t>`).
pub(crate) fn date_add_time_tz(date: Date, time: TimeTz) -> Result<Timestamp> {
    if date == Date::INFINITY {
        return Ok(Timestamp::INFINITY);
    }
    if date == Date::NEG_INFINITY {
        return Ok(Timestamp::NEG_INFINITY);
    }
    Timestamp::from_datetime_tz(date, time)
        .ok_or_else(|| Error::out_of_range("Timestamp with time zone out of range"))
}

/// `timestamp - timestamp` (DuckDB's `Interval::GetDifference`).
pub(crate) fn timestamp_difference(left: Timestamp, right: Timestamp) -> Result<Interval> {
    if !left.is_finite() || !right.is_finite() {
        return Err(Error::invalid_input("Cannot subtract infinite timestamps"));
    }
    let delta = left
        .0
        .checked_sub(right.0)
        .ok_or_else(|| Error::conversion("Timestamp difference is out of bounds"))?;
    Ok(interval_from_micros(delta))
}

/// DuckDB's `Interval::FromMicro`.
pub(crate) fn interval_from_micros(micros: i64) -> Interval {
    Interval {
        months: 0,
        days: (micros / MICROS_PER_DAY) as i32,
        micros: micros % MICROS_PER_DAY,
    }
}

/// DuckDB's `Interval::Invert`.
pub(crate) fn interval_invert(interval: Interval) -> Interval {
    Interval {
        months: interval.months.wrapping_neg(),
        days: interval.days.wrapping_neg(),
        micros: interval.micros.wrapping_neg(),
    }
}

/// `-interval` (DuckDB's `NegateOperator` on intervals).
pub(crate) fn interval_negate(interval: Interval) -> Result<Interval> {
    let overflow = || Error::out_of_range("Overflow in negation of numeric value!");
    Ok(Interval {
        months: interval.months.checked_neg().ok_or_else(overflow)?,
        days: interval.days.checked_neg().ok_or_else(overflow)?,
        micros: interval.micros.checked_neg().ok_or_else(overflow)?,
    })
}

/// PostgreSQL's `TSROUND`: rounds to whole microseconds.
fn round_to_micros(value: f64) -> f64 {
    (value * Interval::MICROS_PER_SEC as f64).round_ties_even() / Interval::MICROS_PER_SEC as f64
}

/// Whether `value` is a NaN or outside the INT32 range.
fn outside_i32(value: f64) -> bool {
    value.is_nan() || value < f64::from(i32::MIN) || value > f64::from(i32::MAX)
}

/// `interval * factor` (DuckDB's `TryMultiplyOperator<interval_t, double>`, from PostgreSQL's
/// `interval_mul`), with the reference build's fused multiply-adds.
pub(crate) fn interval_multiply_double(interval: Interval, factor: f64) -> Option<Interval> {
    let months_product = f64::from(interval.months) * factor;
    if outside_i32(months_product) {
        return None;
    }
    let months = months_product as i32;
    let days_product = f64::from(interval.days) * factor;
    if outside_i32(days_product) {
        return None;
    }
    let mut days = days_product as i32;
    let month_remainder = round_to_micros(
        f64::from(interval.months).mul_add(factor, -f64::from(months)) * Interval::DAYS_PER_MONTH as f64,
    );
    let day_remainder = month_remainder as i32;
    let mut sec_remainder = round_to_micros(
        (f64::from(interval.days).mul_add(factor, -f64::from(days)) + month_remainder
            - f64::from(day_remainder))
            * Interval::SECS_PER_DAY as f64,
    );
    if sec_remainder.abs() >= Interval::SECS_PER_DAY as f64 {
        let whole_days = (sec_remainder / Interval::SECS_PER_DAY as f64) as i32;
        days = days.wrapping_add(whole_days);
        sec_remainder -= (i64::from(whole_days) * Interval::SECS_PER_DAY) as f64;
    }
    days = days.wrapping_add(day_remainder);
    let micros = (interval.micros as f64)
        .mul_add(factor, sec_remainder * Interval::MICROS_PER_SEC as f64)
        .round_ties_even();
    if micros.is_nan() || !(-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&micros) {
        return None;
    }
    Some(Interval {
        months,
        days,
        micros: micros as i64,
    })
}

/// `interval * factor` raising DuckDB's error.
pub(crate) fn interval_multiply_double_checked(interval: Interval, factor: f64) -> Result<Interval> {
    interval_multiply_double(interval, factor)
        .ok_or_else(|| Error::out_of_range("Overflow in multiplication of INTERVAL."))
}

/// `interval * factor` for a BIGINT factor.
pub(crate) fn interval_multiply_bigint(interval: Interval, factor: i64) -> Result<Interval> {
    crate::operator::interval_multiply(interval, factor)
}
