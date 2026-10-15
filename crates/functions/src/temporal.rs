//! Temporal helpers shared by the date/time functions and aggregates (pieces of DuckDB's
//! `Interval`, `Time` and `Timestamp` classes that the types crate does not cover).

use rustdb_types::{Interval, Time, TimeTz};

/// Microseconds in a day.
pub(crate) const MICROS_PER_DAY: i64 = Interval::MICROS_PER_DAY;

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
