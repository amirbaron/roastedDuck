//! `TIME`, `TIME_NS` and `TIME WITH TIME ZONE` values (DuckDB's `dtime_t`, `dtime_ns_t`,
//! `dtime_tz_t` and `Time`).

use std::fmt;

use roastedduck_common::{Error, Result};

use crate::date::{digit_at, parse_one_or_two_digits, push_padded, push_time_of_day, skip_spaces};
use crate::timestamp::parse_utc_offset;
use crate::{Interval, NativeType, PhysicalType, Timestamp};

/// Microseconds since midnight (`TIME`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Time(pub i64);

/// Nanoseconds since midnight (`TIME_NS`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct TimeNs(pub i64);

/// A time of day plus a UTC offset in seconds (`TIME WITH TIME ZONE`).
///
/// Packed like DuckDB: the upper 40 bits hold the microseconds, the lower 24 bits the
/// biased offset `MAX_OFFSET - offset`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct TimeTz(pub u64);

// SAFETY: transparent 64-bit integers; every bit pattern is a valid value.
unsafe impl NativeType for Time {
    const PHYSICAL: PhysicalType = PhysicalType::Int64;
}
// SAFETY: as above.
unsafe impl NativeType for TimeNs {
    const PHYSICAL: PhysicalType = PhysicalType::Int64;
}
// SAFETY: as above.
unsafe impl NativeType for TimeTz {
    const PHYSICAL: PhysicalType = PhysicalType::Int64;
}

/// A time of day parsed from the start of a string by [`Time::parse_prefix`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ParsedTime {
    /// The parsed value, truncated to microseconds. Hours are not limited to 24.
    pub time: Time,
    /// Nanoseconds beyond `time` (`0..1000`), from fraction digits 7-9.
    pub nanos: i32,
    /// Number of input bytes consumed.
    pub len: usize,
}

const MICROS_PER_DAY: i64 = Interval::MICROS_PER_DAY;
const NANOS_PER_MICRO: i64 = Interval::NANOS_PER_MICRO;

impl Time {
    /// Midnight, `00:00:00`.
    pub const MIDNIGHT: Self = Self(0);
    /// The end of the day, `24:00:00` (DuckDB accepts it as a TIME).
    pub const END_OF_DAY: Self = Self(MICROS_PER_DAY);

    /// The time `hour:minute:second.micros` (`Time::FromTime`). The fields are not
    /// validated; any `i32` inputs combine without overflow.
    pub const fn from_hms_micros(hour: i32, minute: i32, second: i32, micros: i32) -> Self {
        let minutes = hour as i64 * 60 + minute as i64;
        let seconds = minutes * 60 + second as i64;
        Self(seconds * Interval::MICROS_PER_SEC + micros as i64)
    }

    /// The `(hour, minute, second, microsecond)` of this time (`Time::Convert`).
    pub const fn to_hms_micros(self) -> (i32, i32, i32, i32) {
        let mut rest = self.0;
        let hour = (rest / Interval::MICROS_PER_HOUR) as i32;
        rest -= hour as i64 * Interval::MICROS_PER_HOUR;
        let minute = (rest / Interval::MICROS_PER_MINUTE) as i32;
        rest -= minute as i64 * Interval::MICROS_PER_MINUTE;
        let second = (rest / Interval::MICROS_PER_SEC) as i32;
        rest -= second as i64 * Interval::MICROS_PER_SEC;
        (hour, minute, second, rest as i32)
    }

    /// Whether the fields form a valid time of day, `24:00:00` included (`Time::IsValidTime`).
    pub const fn is_valid_hms(hour: i32, minute: i32, second: i32, micros: i32) -> bool {
        if hour < 0 || hour >= 24 {
            return hour == 24 && minute == 0 && second == 0 && micros == 0;
        }
        minute >= 0 && minute < 60 && second >= 0 && second <= 60 && micros >= 0 && micros <= 1_000_000
    }

    /// Parses a TIME the way `CAST(varchar AS TIME)` does (`Time::TryConvertTime`).
    ///
    /// Accepts `H[H..]:M[M][:S[S]][.fraction]` after optional whitespace, up to `24:00:00`;
    /// fraction digits beyond the sixth are truncated. Outside `strict` mode trailing text
    /// is ignored, and text that is not a time is retried as a TIMESTAMP whose time of day
    /// is returned (`'2021-01-02 12:34:56'` gives `12:34:56`).
    pub fn parse(input: &[u8], strict: bool) -> Option<Self> {
        Self::parse_with_nanos(input, strict).map(|(time, _)| time)
    }

    /// Like [`Time::parse`], but also returns the nanoseconds beyond the microsecond
    /// (fraction digits 7-9), i.e. `TryConvertTime` with its `nanos` out-parameter.
    pub fn parse_with_nanos(input: &[u8], strict: bool) -> Option<(Self, i32)> {
        match Self::parse_prefix(input, strict) {
            Some(parsed) => (parsed.time.0 <= MICROS_PER_DAY).then_some((parsed.time, parsed.nanos)),
            None if !strict => {
                let (timestamp, nanos) = Timestamp::parse(input, false).ok()?;
                timestamp.is_finite().then(|| (timestamp.to_time(), nanos))
            }
            None => None,
        }
    }

    /// Parses `H[H..]:M[M][:S[S]][.fraction]` at the start of `input` without limiting the
    /// hours (up to nine digits) and without the timestamp fallback (`Time::TryConvertInterval`).
    ///
    /// In `strict` mode the minutes need two digits unless the seconds are present, and only
    /// whitespace may follow. Used by the TIMESTAMP and INTERVAL parsers.
    pub fn parse_prefix(input: &[u8], strict: bool) -> Option<ParsedTime> {
        let len = input.len();
        let mut pos = skip_spaces(input, 0);
        // The hours are mandatory.
        digit_at(input, pos)?;

        let mut hour: i32 = 0;
        let mut hour_digits = 0;
        while let Some(digit) = digit_at(input, pos) {
            if hour_digits == 9 {
                return None;
            }
            hour_digits += 1;
            hour = hour * 10 + digit;
            pos += 1;
        }

        if input.get(pos) != Some(&b':') {
            return None;
        }
        pos += 1;
        let minutes_start = pos;
        let minute = if pos == len && !strict {
            0
        } else {
            parse_one_or_two_digits(input, &mut pos).filter(|m| *m < 60)?
        };
        let second = if pos == len && (!strict || pos == minutes_start + 2) {
            0
        } else {
            if input.get(pos) != Some(&b':') {
                return None;
            }
            pos += 1;
            if pos == len && !strict {
                0
            } else {
                parse_one_or_two_digits(input, &mut pos).filter(|s| *s < 60)?
            }
        };

        // Fraction in nanoseconds; digits past the ninth are skipped.
        let mut fraction: i32 = 0;
        if input.get(pos) == Some(&b'.') {
            pos += 1;
            let mut scale = 100_000_000;
            while let Some(digit) = digit_at(input, pos) {
                fraction += digit * scale;
                scale /= 10;
                pos += 1;
            }
        }

        if strict && skip_spaces(input, pos) < len {
            return None;
        }
        Some(ParsedTime {
            time: Self::from_hms_micros(hour, minute, second, fraction / 1000),
            nanos: fraction % 1000,
            len: pos,
        })
    }

    /// DuckDB's message for text that is not a TIME (`Time::ConversionError`); also used
    /// for `TIME_NS` and `TIME WITH TIME ZONE`.
    pub fn conversion_error(input: &[u8]) -> String {
        format!(
            "time field value out of range: \"{}\", expected format is ([YYYY-MM-DD ]HH:MM:SS[.MS])",
            String::from_utf8_lossy(input)
        )
    }

    /// The `TIME_NS` with the same value (`TryCast` `dtime_t` to `dtime_ns_t`).
    pub fn to_time_ns(self) -> Result<TimeNs> {
        self.0
            .checked_mul(NANOS_PER_MICRO)
            .map(TimeNs)
            .ok_or_else(|| Error::conversion("Could not convert TIME to TIME_NS"))
    }

    /// Appends DuckDB's text form, `HH:MM:SS[.ffffff]` with trailing fraction zeros removed
    /// (`StringAsTime<false>`). DuckDB only produces values in `0..=24:00:00`; others are
    /// written by magnitude.
    pub fn write_to(self, out: &mut Vec<u8>) {
        push_time_of_day(out, self.0.unsigned_abs(), 6);
    }
}

impl TimeNs {
    /// Parses a `TIME_NS` the way `CAST(varchar AS TIME_NS)` does: [`Time::parse_with_nanos`]
    /// keeping nine fraction digits.
    pub fn parse(input: &[u8], strict: bool) -> Option<Self> {
        let (time, nanos) = Time::parse_with_nanos(input, strict)?;
        time.0
            .checked_mul(NANOS_PER_MICRO)?
            .checked_add(i64::from(nanos))
            .map(Self)
    }

    /// The `TIME` rounded to the nearest microsecond, halves up (`TryCast` `dtime_ns_t` to
    /// `dtime_t`); `23:59:59.9999995` becomes `24:00:00`.
    pub const fn to_time(self) -> Time {
        Time(self.0.wrapping_add(NANOS_PER_MICRO / 2) / NANOS_PER_MICRO)
    }

    /// Appends DuckDB's text form, `HH:MM:SS[.fffffffff]` with trailing fraction zeros removed
    /// (`StringAsTime<true>`).
    pub fn write_to(self, out: &mut Vec<u8>) {
        push_time_of_day(out, self.0.unsigned_abs(), 9);
    }
}

impl TimeTz {
    const OFFSET_BITS: u32 = 24;
    const OFFSET_MASK: u64 = (1 << Self::OFFSET_BITS) - 1;
    /// Largest supported offset: +15:59:59.
    pub const MAX_OFFSET: i32 = 16 * 60 * 60 - 1;
    /// Smallest supported offset: -15:59:59.
    pub const MIN_OFFSET: i32 = -Self::MAX_OFFSET;

    /// Packs a time and an offset (in seconds east of UTC).
    pub const fn new(time: Time, offset: i32) -> Self {
        Self(((time.0 as u64) << Self::OFFSET_BITS) | (Self::MAX_OFFSET - offset) as u64)
    }

    /// The time at offset `+00` (`TryCast` `dtime_t` to `dtime_tz_t`).
    pub const fn from_time(time: Time) -> Self {
        Self::new(time, 0)
    }

    /// The local time of day.
    pub const fn time(self) -> Time {
        Time((self.0 >> Self::OFFSET_BITS) as i64)
    }

    /// The UTC offset in seconds.
    pub const fn offset(self) -> i32 {
        Self::MAX_OFFSET - (self.0 & Self::OFFSET_MASK) as i32
    }

    /// The key DuckDB orders by: the time normalised to UTC, biased to stay unsigned.
    pub const fn sort_key(self) -> u64 {
        self.0 + (((self.0 & Self::OFFSET_MASK) * 1_000_000) << Self::OFFSET_BITS)
    }

    /// Parses a `TIME WITH TIME ZONE` the way `CAST(varchar AS TIMETZ)` does
    /// (`Time::TryConvertTimeTZ`): a time up to `24:00:00`, optional whitespace and an optional
    /// `±HH[[:]MM[:SS]]` offset inside ±15:59:59. A missing offset means `+00`. Outside
    /// `strict` mode trailing text is ignored, and text that is not a time is retried as a
    /// TIMESTAMP (with its offset applied) whose UTC time of day is returned at `+00`.
    pub fn parse(input: &[u8], strict: bool) -> Option<Self> {
        Self::parse_with_offset_flag(input, strict).map(|(value, _)| value)
    }

    /// Like [`TimeTz::parse`], but also reports whether the text carried an explicit offset
    /// (`has_offset`; the ICU cast replaces missing offsets by the session time zone's).
    pub fn parse_with_offset_flag(input: &[u8], strict: bool) -> Option<(Self, bool)> {
        let Some(parsed) = Time::parse_prefix(input, false) else {
            if strict {
                return None;
            }
            let (timestamp, _) = Timestamp::parse(input, true).ok()?;
            return timestamp
                .is_finite()
                .then(|| (Self::from_time(timestamp.to_time()), false));
        };
        if parsed.time.0 > MICROS_PER_DAY {
            return None;
        }

        let mut pos = skip_spaces(input, parsed.len);
        let has_offset = pos < input.len();
        let mut offset = 0;
        if has_offset {
            let (hours, minutes, seconds, end) = parse_utc_offset(input, pos)?;
            offset = (hours * 60 + minutes) * 60 + seconds;
            pos = end;
        }
        if !(Self::MIN_OFFSET..=Self::MAX_OFFSET).contains(&offset) {
            return None;
        }
        if strict && skip_spaces(input, pos) < input.len() {
            return None;
        }
        Some((Self::new(parsed.time, offset), has_offset))
    }

    /// Appends DuckDB's text form, the time followed by `±HH[:MM][:SS]` (`StringCastTZ`).
    ///
    /// Like DuckDB, minutes are omitted when zero even if seconds follow, so an offset of
    /// 30 seconds prints as `+00:30`.
    pub fn write_to(self, out: &mut Vec<u8>) {
        self.time().write_to(out);
        let offset = self.offset();
        out.push(if offset < 0 { b'-' } else { b'+' });
        let seconds = u64::from(offset.unsigned_abs());
        push_padded(out, seconds / 3600, 2);
        let minutes = seconds % 3600 / 60;
        if minutes != 0 {
            out.push(b':');
            push_padded(out, minutes, 2);
        }
        if seconds % 60 != 0 {
            out.push(b':');
            push_padded(out, seconds % 60, 2);
        }
    }
}

macro_rules! display_via_write_to {
    ($($ty:ty),*) => {$(
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let mut buf = Vec::with_capacity(32);
                self.write_to(&mut buf);
                f.write_str(&String::from_utf8_lossy(&buf))
            }
        }
    )*};
}

display_via_write_to!(Time, TimeNs, TimeTz);

#[cfg(test)]
mod tests {
    use super::*;

    fn cast_time(input: &str) -> Option<String> {
        Time::parse(input.as_bytes(), false).map(|t| t.to_string())
    }

    fn cast_time_ns(input: &str) -> Option<String> {
        TimeNs::parse(input.as_bytes(), false).map(|t| t.to_string())
    }

    fn cast_time_tz(input: &str) -> Option<String> {
        TimeTz::parse(input.as_bytes(), false).map(|t| t.to_string())
    }

    #[test]
    fn fields_round_trip() {
        let time = Time::from_hms_micros(12, 34, 56, 789);
        assert_eq!(time.0, 45_296_000_789);
        assert_eq!(time.to_hms_micros(), (12, 34, 56, 789));
        assert_eq!(Time::END_OF_DAY.to_hms_micros(), (24, 0, 0, 0));
        assert!(Time::is_valid_hms(24, 0, 0, 0));
        assert!(!Time::is_valid_hms(24, 0, 0, 1));
        assert!(!Time::is_valid_hms(12, 60, 0, 0));
        for v in [i32::MAX, i32::MIN] {
            let w = i128::from(v);
            let expected = ((w * 60 + w) * 60 + w) * 1_000_000 + w;
            assert_eq!(i128::from(Time::from_hms_micros(v, v, v, v).0), expected);
        }
    }

    /// Expected values from DuckDB 1.5.6: `SELECT CAST(<input> AS TIME)::VARCHAR`.
    #[test]
    fn cast_time_matches_duckdb() {
        let ok = [
            ("12:34:56", "12:34:56"),
            ("12:34:56.1", "12:34:56.1"),
            ("12:34:56.123456", "12:34:56.123456"),
            ("12:34:56.1234567", "12:34:56.123456"),
            ("12:34:56.1234569", "12:34:56.123456"),
            ("12:34:56.999999999", "12:34:56.999999"),
            ("12:34:56.", "12:34:56"),
            ("12:34", "12:34:00"),
            ("12:3", "12:03:00"),
            ("12:", "12:00:00"),
            ("1:2:3", "01:02:03"),
            ("24:00:00", "24:00:00"),
            ("23:59:59.999999", "23:59:59.999999"),
            ("  12:00:00  ", "12:00:00"),
            ("12:00:00abc", "12:00:00"),
            ("12:00:00 abc", "12:00:00"),
            ("12:00:00+05", "12:00:00"),
            ("12:00:00.5Z", "12:00:00.5"),
            ("12:00:00 (BC)", "12:00:00"),
            ("00:00:00", "00:00:00"),
            ("2021-01-02 12:34:56", "12:34:56"),
            ("2021-01-02T12:34:56.5", "12:34:56.5"),
            ("2021-01-02", "00:00:00"),
            ("2021-01-02 24:00:00", "00:00:00"),
            ("2021-01-02 12:00:00 utc", "12:00:00"),
            ("epoch", "00:00:00"),
        ];
        for (input, expected) in ok {
            assert_eq!(cast_time(input).as_deref(), Some(expected), "{input:?}");
        }
        let errors = [
            "12",
            "24:00:00.000001",
            "24:00:01",
            "25:00:00",
            "12:60:00",
            "12:00:60",
            "infinity",
            "",
            "x",
            "123:00:00",
            "1234567890:00:00",
            "-12:00:00",
            "2021-01-02 12:00:00 cet",
        ];
        for input in errors {
            assert_eq!(cast_time(input), None, "{input:?}");
        }
        assert_eq!(
            Time::conversion_error(b"x"),
            "time field value out of range: \"x\", expected format is ([YYYY-MM-DD ]HH:MM:SS[.MS])"
        );
    }

    #[test]
    fn strict_time() {
        let parse = |s: &str| Time::parse(s.as_bytes(), true).map(|t| t.to_string());
        assert_eq!(parse("12:34").as_deref(), Some("12:34:00"));
        assert_eq!(parse(" 12:34:56.5 ").as_deref(), Some("12:34:56.5"));
        assert_eq!(parse("12:3"), None);
        assert_eq!(parse("12:"), None);
        assert_eq!(parse("12:34:"), None);
        assert_eq!(parse("12:34:56x"), None);
        assert_eq!(parse("2021-01-02 12:34:56"), None);
    }

    #[test]
    fn parse_prefix_allows_long_hours() {
        let parsed = Time::parse_prefix(b"999999999:59:59.123456789 rest", false).unwrap();
        assert_eq!(parsed.time.0, 999_999_999 * 3_600_000_000 + 3_599_123_456);
        assert_eq!(parsed.nanos, 789);
        assert_eq!(parsed.len, 25);
    }

    /// Expected values from DuckDB 1.5.6: `SELECT CAST(<input> AS TIME_NS)::VARCHAR`.
    #[test]
    fn cast_time_ns_matches_duckdb() {
        let ok = [
            ("12:00:00", "12:00:00"),
            ("12:00", "12:00:00"),
            ("12:00:00.123456789", "12:00:00.123456789"),
            ("12:00:00.1234567891", "12:00:00.123456789"),
            ("12:00:00.000000001", "12:00:00.000000001"),
            ("12:00:00.00000001", "12:00:00.00000001"),
            ("12:00:00.1", "12:00:00.1"),
            ("12:00:00.000001", "12:00:00.000001"),
            ("12:00:00.0000015", "12:00:00.0000015"),
            ("12:00:00.0000005", "12:00:00.0000005"),
            ("12:00:00.000000500", "12:00:00.0000005"),
            ("12:00:00.100000001", "12:00:00.100000001"),
            ("00:00:00.000000001", "00:00:00.000000001"),
            ("24:00:00", "24:00:00"),
            ("2021-01-02 12:34:56.123456789", "12:34:56.123456789"),
            ("2021-01-02 23:59:59.9999999", "23:59:59.9999999"),
        ];
        for (input, expected) in ok {
            assert_eq!(cast_time_ns(input).as_deref(), Some(expected), "{input:?}");
        }
        assert_eq!(cast_time_ns("bad"), None);
    }

    /// `CAST(CAST(<input> AS TIME_NS) AS TIME)` and back, from DuckDB 1.5.6.
    #[test]
    fn time_ns_conversions_match_duckdb() {
        let round = |s: &str| TimeNs::parse(s.as_bytes(), false).unwrap().to_time().to_string();
        assert_eq!(round("12:00:00.123456789"), "12:00:00.123457");
        assert_eq!(round("12:00:00.1234565"), "12:00:00.123457");
        assert_eq!(round("12:00:00.1234564"), "12:00:00.123456");
        assert_eq!(round("23:59:59.9999995"), "24:00:00");
        let time = Time::parse(b"12:00:00.123456", false).unwrap();
        assert_eq!(time.to_time_ns().unwrap().to_string(), "12:00:00.123456");
        assert_eq!(Time::END_OF_DAY.to_time_ns().unwrap().to_string(), "24:00:00");
        assert_eq!(
            Time(i64::MAX).to_time_ns().unwrap_err().to_string(),
            "Conversion Error: Could not convert TIME to TIME_NS"
        );
    }

    /// Expected values from DuckDB 1.5.6: `SELECT CAST(<input> AS TIMETZ)::VARCHAR`.
    #[test]
    fn cast_time_tz_matches_duckdb() {
        let ok = [
            ("12:00:00", "12:00:00+00"),
            ("12:00:00+05", "12:00:00+05"),
            ("12:00:00+05:30", "12:00:00+05:30"),
            ("12:00:00-05:30:15", "12:00:00-05:30:15"),
            ("12:00:00 +0530", "12:00:00+05:30"),
            ("12:00:00+0530", "12:00:00+05:30"),
            ("12:00:00+053015", "12:00:00+05:30"),
            ("12:00:00+05:30:15", "12:00:00+05:30:15"),
            ("12:00:00+15:59:59", "12:00:00+15:59:59"),
            ("12:00:00-15:59:59", "12:00:00-15:59:59"),
            ("12:00:00+05:3", "12:00:00+05"),
            ("12:00:00+05:30:1", "12:00:00+05:30"),
            ("12:00:00+05 ", "12:00:00+05"),
            ("12:00:00+05x", "12:00:00+05"),
            ("12:00:00 +05 ", "12:00:00+05"),
            ("12:00:00.123+01", "12:00:00.123+01"),
            ("12:00:00.123456789+05", "12:00:00.123456+05"),
            ("24:00:00+01", "24:00:00+01"),
            ("12:00:00-00:30", "12:00:00-00:30"),
            ("12:00:00-00:00:30", "12:00:00-00:30"),
            ("12:00:00+00:00:30", "12:00:00+00:30"),
            ("12:00:00-00:30:00", "12:00:00-00:30"),
            ("12:00:00.5-00", "12:00:00.5+00"),
            ("12:00:00.", "12:00:00+00"),
            ("12:00:00.+05", "12:00:00+05"),
            ("1:2:3+04", "01:02:03+04"),
            ("12:3", "12:03:00+00"),
            ("2021-01-02 12:34:56+05", "07:34:56+00"),
            ("2021-01-02 12:34:56", "12:34:56+00"),
            ("2021-01-02 12:00:00 utc", "12:00:00+00"),
            ("2021-01-02", "00:00:00+00"),
        ];
        for (input, expected) in ok {
            assert_eq!(cast_time_tz(input).as_deref(), Some(expected), "{input:?}");
        }
        let errors = [
            "12:00:00+16",
            "12:00:00+1",
            "12:00:00Z",
            "12:00:00 UTC",
            "24:00:01+01",
            "12",
            "12:00:00 - 05",
            "12:00:00 + 05",
            "2021-01-02 12:00:00 abc",
            "infinity",
        ];
        for input in errors {
            assert_eq!(cast_time_tz(input), None, "{input:?}");
        }
    }

    #[test]
    fn time_tz_offset_flag_and_strictness() {
        assert_eq!(
            TimeTz::parse_with_offset_flag(b"12:00:00", false).map(|(_, flag)| flag),
            Some(false)
        );
        assert_eq!(
            TimeTz::parse_with_offset_flag(b"12:00:00 -03", false).map(|(_, flag)| flag),
            Some(true)
        );
        assert_eq!(
            TimeTz::parse_with_offset_flag(b"2021-01-02 12:00:00+03", false).map(|(_, flag)| flag),
            Some(false)
        );
        assert!(TimeTz::parse(b"12:00:00+05 ", true).is_some());
        assert!(TimeTz::parse(b"12:00:00+05x", true).is_none());
        assert!(TimeTz::parse(b"2021-01-02 12:00:00", true).is_none());
    }

    #[test]
    fn time_tz_packing() {
        let value = TimeTz::new(Time::from_hms_micros(12, 0, 0, 0), -3600);
        assert_eq!(value.time(), Time::from_hms_micros(12, 0, 0, 0));
        assert_eq!(value.offset(), -3600);
        assert_eq!(TimeTz::from_time(Time::END_OF_DAY).to_string(), "24:00:00+00");
        // 13:00+01 and 12:00+00 are the same instant; the offset only breaks the tie.
        let a = TimeTz::new(Time::from_hms_micros(13, 0, 0, 0), 3600);
        let b = TimeTz::new(Time::from_hms_micros(12, 0, 0, 0), 0);
        assert_eq!(a.sort_key() >> 24, b.sort_key() >> 24);
        assert!(a.sort_key() < b.sort_key());
    }
}
