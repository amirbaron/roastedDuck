//! `ICUHelpers` (DuckDB's `icu-helpers.hpp`, implemented in `icu_extension.cpp` and
//! `icu-strptime.cpp`): time zone lookup with DuckDB's normalisation and suggestions,
//! and the decomposition of an instant in a calendar.

use roastedduck_common::string_util::{candidates_message, top_n_jaro_winkler};
use roastedduck_common::{Error, Result};
use roastedduck_types::Timestamp;

use crate::icu_datefunc::{MICROS_PER_MSEC, extract_field, set_time};
use crate::icu4c::calendar::{Calendar, field};
use crate::icu4c::timezone::TimeZone;
use crate::icu4c::zone_names::ZONE_NAMES;

/// `GetKnownTimeZone`: the zone `tz_str` names, if ICU knows it.
fn get_known_time_zone(tz_str: &str) -> Option<TimeZone> {
    let tz = TimeZone::create(tz_str);
    (!tz.is_unknown()).then_some(tz)
}

/// `GetNormalizedTimeZone`: a known zone, or `UTC±NN00` mapped to `Etc/GMT∓N` (which
/// replaces `tz_str`).
fn get_normalized_time_zone(tz_str: &mut String) -> Option<TimeZone> {
    if let Some(tz) = get_known_time_zone(tz_str) {
        return Some(tz);
    }

    // Map UTC±NN00 to Etc/UTC±N
    let bytes = tz_str.as_bytes();
    if bytes.len() <= 4 || !bytes.starts_with(b"UTC") {
        return None;
    }
    let mut pos = 3;
    let utc = bytes[pos];
    pos += 1;
    // Invert the sign (UTC and Etc use opposite sign conventions)
    let sign = match utc {
        b'+' => '-',
        b'-' => '+',
        _ => return None,
    };

    // Collect remaining characters (digits and colons)
    let mut remainder = String::new();
    while pos < bytes.len() {
        let ch = bytes[pos];
        if ch != b':' && !ch.is_ascii_digit() {
            break;
        }
        remainder.push(char::from(ch));
        pos += 1;
    }
    if pos < bytes.len() {
        return None;
    }

    // Step 1: Strip leading zeros
    let remainder = remainder.trim_start_matches('0');

    // Step 2: Parse hours based on whether colon is present
    let hours_str = if let Some(colon_idx) = remainder.find(':') {
        // Has colon: split by colon, part before colon is hours
        &remainder[..colon_idx]
    } else if remainder.len() <= 2 {
        // 1-2 digits: entire string is hours
        remainder
    } else {
        // No colon, 3+ digits: HHMM format, last 2 are minutes, rest are hours
        &remainder[..remainder.len() - 2]
    };

    // Build the mapped timezone string
    let mut mapped = String::from("Etc/GMT");
    if hours_str.is_empty() {
        // Zero offset
        mapped.push_str("+0");
    } else {
        mapped.push(sign);
        mapped.push_str(hours_str);
    }
    // Final sanity check
    let tz = get_known_time_zone(&mapped)?;
    *tz_str = mapped;
    Some(tz)
}

/// `GetNormalizedTimeZone` for the default time zone.
pub(crate) fn try_get_normalized_time_zone(tz_str: &mut String) -> Option<TimeZone> {
    get_normalized_time_zone(tz_str)
}

/// `GetTimeZoneInternal`: a normalised zone, else a case-insensitive match of a zone id
/// (which replaces `tz_str`); the zone ids are collected in `candidates` otherwise.
fn get_time_zone_internal(tz_str: &mut String, candidates: &mut Vec<String>) -> Option<TimeZone> {
    if let Some(tz) = get_normalized_time_zone(tz_str) {
        return Some(tz);
    }

    // Try to be friendlier: go through all the zone names and look for a case insensitive
    // match. If we don't find one, make a suggestion.
    for zone in ZONE_NAMES {
        if zone.name.eq_ignore_ascii_case(tz_str) {
            // case insensitive match - return this timezone instead
            zone.name.clone_into(tz_str);
            return Some(TimeZone::create(zone.name));
        }
        candidates.push(zone.name.to_owned());
    }
    None
}

/// `ICUHelpers::TryGetTimeZone`: the zone, or `None` if it is not found.
pub(crate) fn try_get_time_zone(tz_str: &mut String) -> Option<TimeZone> {
    let mut candidates = Vec::new();
    get_time_zone_internal(tz_str, &mut candidates)
}

/// `ICUHelpers::GetTimeZone` with an error message: the zone, or DuckDB's message.
pub(crate) fn get_time_zone_or_message(tz_str: &mut String) -> std::result::Result<TimeZone, String> {
    let mut candidates = Vec::new();
    if let Some(tz) = get_time_zone_internal(tz_str, &mut candidates) {
        return Ok(tz);
    }
    let candidate_str =
        candidates_message(&top_n_jaro_winkler(&candidates, &*tz_str), "Candidate time zones");
    Err(format!("Unknown TimeZone '{tz_str}'!\n{candidate_str}"))
}

/// `ICUHelpers::GetTimeZone`: the zone, or DuckDB's `Unknown TimeZone` error.
pub(crate) fn get_time_zone(tz_str: &mut String) -> Result<TimeZone> {
    get_time_zone_or_message(tz_str).map_err(Error::not_implemented)
}

/// DuckDB's `TimestampComponents`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TimestampComponents {
    pub year: i32,
    pub month: i32,
    pub day: i32,
    pub hour: i32,
    pub minute: i32,
    pub second: i32,
    pub microsecond: i32,
}

/// `ICUHelpers::GetComponents`: the parts of `ts` in the calendar's time zone.
pub(crate) fn get_components(ts: Timestamp, calendar: &mut Calendar) -> Result<TimestampComponents> {
    // Get the parts in the given time zone
    let micros = set_time(calendar, ts);
    Ok(TimestampComponents {
        year: extract_field(calendar, field::EXTENDED_YEAR)?,
        month: extract_field(calendar, field::MONTH)? + 1,
        day: extract_field(calendar, field::DATE)?,
        hour: extract_field(calendar, field::HOUR_OF_DAY)?,
        minute: extract_field(calendar, field::MINUTE)?,
        second: extract_field(calendar, field::SECOND)?,
        microsecond: (i64::from(extract_field(calendar, field::MILLISECOND)?) * MICROS_PER_MSEC
            + micros as i64) as i32,
    })
}
