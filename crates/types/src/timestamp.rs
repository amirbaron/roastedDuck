//! TIMESTAMP values (DuckDB's `timestamp_t` and `Timestamp`).
//!
//! `TIMESTAMP` and `TIMESTAMP WITH TIME ZONE` store microseconds since the epoch;
//! `TIMESTAMP_S`, `TIMESTAMP_MS` and `TIMESTAMP_NS` store seconds, milliseconds and
//! nanoseconds as plain `i64`s with the same infinity encoding. The free functions at the
//! bottom of this module are DuckDB's conversions between these units
//! (`CastTimestamp{Us,Ms,Sec,Ns}To*`, `TryCastToTimestamp{NS,MS,Sec}`), with identical
//! rounding and overflow behaviour.

use std::fmt;

use rustdb_common::{Error, Result};

use crate::date::{DateParseError, digit_at, push_time_of_day, skip_spaces};
use crate::{Date, Interval, NativeType, PhysicalType, Time, TimeNs, TimeTz};

/// Microseconds since 1970-01-01 00:00:00 (UTC for `TIMESTAMP WITH TIME ZONE`).
/// `i64::MAX` / `-i64::MAX` encode `infinity` / `-infinity`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Timestamp(pub i64);

// SAFETY: a transparent `i64`; every bit pattern is a valid value.
unsafe impl NativeType for Timestamp {
    const PHYSICAL: PhysicalType = PhysicalType::Int64;
}

/// Why a string could not be parsed as a TIMESTAMP (DuckDB's `TimestampCastResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimestampParseError {
    /// The text is not a timestamp; reported with [`Timestamp::format_error`].
    Format,
    /// The text names a time zone other than UTC, which needs ICU; reported with
    /// [`Timestamp::unsupported_timezone_error`].
    NonUtcTimezone,
    /// The text is a well-formed timestamp outside the supported range; reported with
    /// [`Timestamp::range_error`].
    Range,
}

impl TimestampParseError {
    /// DuckDB's error message for this failure on `input`.
    pub fn message(self, input: &[u8]) -> String {
        match self {
            Self::Format => Timestamp::format_error(input),
            Self::NonUtcTimezone => Timestamp::unsupported_timezone_error(input),
            Self::Range => Timestamp::range_error(input),
        }
    }
}

/// Everything [`Timestamp::parse_parts`] extracts from a timestamp string
/// (`Timestamp::TryConvertTimestampTZ`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ParsedTimestamp<'a> {
    /// The instant, shifted by the `±HH[:MM[:SS]]` offset if one was given and requested.
    pub timestamp: Timestamp,
    /// Nanoseconds beyond the microsecond (fraction digits 7-9).
    pub nanos: i32,
    /// Whether the text carried a `Z` or numeric offset. Like DuckDB, this is also set for
    /// the special values `infinity`, `-infinity` and `epoch`.
    pub has_offset: bool,
    /// The time zone name following the time (empty if none), e.g. `Europe/Amsterdam`.
    pub time_zone: &'a [u8],
}

const MICROS_PER_SEC: i64 = Interval::MICROS_PER_SEC;
const MICROS_PER_MSEC: i64 = Interval::MICROS_PER_MSEC;
const MICROS_PER_DAY: i64 = Interval::MICROS_PER_DAY;
const NANOS_PER_MICRO: i64 = Interval::NANOS_PER_MICRO;
const NANOS_PER_DAY: i64 = MICROS_PER_DAY * NANOS_PER_MICRO;

const S_TO_US: &str = "Could not convert Timestamp(S) to Timestamp(US)";
const MS_TO_US: &str = "Could not convert Timestamp(MS) to Timestamp(US)";
const US_TO_NS: &str = "Could not convert Timestamp(US) to Timestamp(NS)";
const INFINITE_TIME: &str = "Can't get TIME of infinite TIMESTAMP";

impl Timestamp {
    /// `infinity`.
    pub const INFINITY: Self = Self(i64::MAX);
    /// `-infinity`.
    pub const NEG_INFINITY: Self = Self(-i64::MAX);
    /// 1970-01-01 00:00:00.
    pub const EPOCH: Self = Self(0);

    /// Whether the timestamp is neither `infinity` nor `-infinity`.
    pub const fn is_finite(self) -> bool {
        self.0 != Self::INFINITY.0 && self.0 != Self::NEG_INFINITY.0
    }

    /// Whether the raw `i64` of a `TIMESTAMP_S`/`_MS`/`_NS` value is finite.
    pub const fn is_finite_raw(value: i64) -> bool {
        Self(value).is_finite()
    }

    /// The timestamp at `time` on `date`, or `None` if it is out of range
    /// (`Timestamp::TryFromDatetime`).
    ///
    /// The day is scaled to microseconds before the time is added, so the last partial day
    /// before the minimum timestamp is rejected even though its values would fit.
    pub const fn from_datetime(date: Date, time: Time) -> Option<Self> {
        let Some(day_start) = (date.0 as i64).checked_mul(MICROS_PER_DAY) else {
            return None;
        };
        match day_start.checked_add(time.0) {
            Some(value) if Self(value).is_finite() => Some(Self(value)),
            _ => None,
        }
    }

    /// The instant at local `time` on `date` in the time's UTC offset, or `None` if it is out
    /// of range (`Timestamp::TryFromDatetime` with a `dtime_tz_t`).
    pub const fn from_datetime_tz(date: Date, time: TimeTz) -> Option<Self> {
        let Some(local) = Self::from_datetime(date, time.time()) else {
            return None;
        };
        match local.0.checked_sub(time.offset() as i64 * MICROS_PER_SEC) {
            Some(value) if Self(value).is_finite() => Some(Self(value)),
            _ => None,
        }
    }

    /// The timestamp at midnight of `date`, mapping infinities to infinities, or `None` if it
    /// is out of range (`TryCast` `date_t` to `timestamp_t`).
    pub const fn from_date(date: Date) -> Option<Self> {
        if date.0 == Date::INFINITY.0 {
            Some(Self::INFINITY)
        } else if date.0 == Date::NEG_INFINITY.0 {
            Some(Self::NEG_INFINITY)
        } else {
            Self::from_datetime(date, Time::MIDNIGHT)
        }
    }

    /// The date part, rounding towards negative infinity; infinities map to infinities
    /// (`Timestamp::GetDate`).
    pub const fn to_date(self) -> Date {
        if self.0 == Self::INFINITY.0 {
            Date::INFINITY
        } else if self.0 == Self::NEG_INFINITY.0 {
            Date::NEG_INFINITY
        } else {
            // |i64| / MICROS_PER_DAY < 2^27, so the cast is lossless.
            Date(self.0.div_euclid(MICROS_PER_DAY) as i32)
        }
    }

    /// The time of day (`Timestamp::GetTime`).
    ///
    /// DuckDB raises an error for infinite timestamps (see [`Timestamp::try_to_time`]);
    /// callers must check [`Timestamp::is_finite`] first, as this returns the time of day of
    /// the raw value for them.
    pub const fn to_time(self) -> Time {
        Time(self.0.rem_euclid(MICROS_PER_DAY))
    }

    /// The time of day, or DuckDB's error for infinite timestamps.
    pub fn try_to_time(self) -> Result<Time> {
        if self.is_finite() {
            Ok(self.to_time())
        } else {
            Err(Error::conversion(INFINITE_TIME))
        }
    }

    /// The date and time parts, or `None` for infinities and the few values whose day does
    /// not fit in microseconds (`Timestamp::Convert`, which raises
    /// `Date out of range in timestamp conversion`).
    pub const fn to_datetime(self) -> Option<(Date, Time)> {
        let date = self.to_date();
        match (date.0 as i64).checked_mul(MICROS_PER_DAY) {
            // `day_start <= self.0 < day_start + MICROS_PER_DAY`, so this cannot overflow.
            Some(day_start) => Some((date, Time(self.0 - day_start))),
            None => None,
        }
    }

    /// Parses a timestamp string into its parts (`Timestamp::TryConvertTimestampTZ`).
    ///
    /// The grammar is a [`Date`] (or a special value), then optionally a space or `T`, a
    /// [`Time`] up to `24:00:00`, and finally `Z`, a `±HH[[:]MM[:SS]]` offset or a space and a
    /// time zone name, followed by whitespace only. The offset is subtracted when
    /// `use_offset` is set; the time zone name is returned for the caller to resolve.
    pub fn parse_parts(input: &[u8], use_offset: bool) -> Result<ParsedTimestamp<'_>, TimestampParseError> {
        use TimestampParseError::{Format, NonUtcTimezone, Range};

        let date = Date::parse_prefix(input, false).map_err(|error| match error {
            DateParseError::Format => Format,
            DateParseError::Range => Range,
        })?;
        let mut parsed = ParsedTimestamp {
            timestamp: Self::EPOCH,
            nanos: 0,
            has_offset: date.special,
            time_zone: &[],
        };
        let mut pos = date.len;
        if pos == input.len() {
            parsed.timestamp = Self::from_date(date.date).ok_or(Range)?;
            return Ok(parsed);
        }

        if matches!(input.get(pos), Some(b' ' | b'T')) {
            pos += 1;
        }
        let time = Time::parse_prefix(input.get(pos..).unwrap_or_default(), false).ok_or(Format)?;
        if time.time.0 > MICROS_PER_DAY {
            return Err(Range);
        }
        pos += time.len;
        parsed.nanos = time.nanos;
        parsed.timestamp = Self::from_datetime(date.date, time.time).ok_or(Range)?;

        let Some(&next) = input.get(pos) else {
            return Ok(parsed);
        };
        if next == b'Z' {
            pos += 1;
            parsed.has_offset = true;
        } else if let Some((hours, minutes, seconds, end)) = parse_utc_offset(input, pos) {
            if use_offset {
                let delta = i64::from(hours) * Interval::MICROS_PER_HOUR
                    + i64::from(minutes) * Interval::MICROS_PER_MINUTE
                    + i64::from(seconds) * MICROS_PER_SEC;
                parsed.timestamp.0 = parsed.timestamp.0.checked_sub(delta).ok_or(Range)?;
            }
            parsed.has_offset = true;
            pos = end;
        } else {
            if next != b' ' {
                return Err(NonUtcTimezone);
            }
            pos += 1;
            let name = input.get(pos..).unwrap_or_default();
            let name_len = name.iter().take_while(|&&c| is_time_zone_char(c)).count();
            parsed.time_zone = name.get(..name_len).unwrap_or_default();
            pos += name_len;
        }
        if skip_spaces(input, pos) < input.len() {
            return Err(Format);
        }
        Ok(parsed)
    }

    /// Parses a timestamp the way `CAST(varchar AS TIMESTAMP)` (`use_offset == false`) and
    /// `CAST(varchar AS TIMESTAMPTZ)` without ICU (`use_offset == true`) do
    /// (`Timestamp::TryConvertTimestamp` with its `nanos` out-parameter).
    ///
    /// Returns the timestamp and the nanoseconds beyond its microsecond. Only the time zone
    /// name `UTC` (any case) is accepted.
    pub fn parse(input: &[u8], use_offset: bool) -> Result<(Self, i32), TimestampParseError> {
        let parsed = Self::parse_parts(input, use_offset)?;
        if parsed.time_zone.is_empty() || parsed.time_zone.eq_ignore_ascii_case(b"utc") {
            Ok((parsed.timestamp, parsed.nanos))
        } else {
            Err(TimestampParseError::NonUtcTimezone)
        }
    }

    /// Parses a `TIMESTAMP_NS` the way `CAST(varchar AS TIMESTAMP_NS)` does: offsets are
    /// applied, and values that do not fit in nanoseconds are a [`TimestampParseError::Format`]
    /// error (`Timestamp::TryConvertTimestamp` for `timestamp_ns_t`).
    pub fn parse_ns(input: &[u8]) -> Result<i64, TimestampParseError> {
        let (timestamp, nanos) = Self::parse(input, true)?;
        timestamp
            .to_timestamp_ns(nanos)
            .ok_or(TimestampParseError::Format)
    }

    /// DuckDB's message for unparseable TIMESTAMP text (`Timestamp::FormatError`).
    pub fn format_error(input: &[u8]) -> String {
        format!(
            "invalid timestamp field format: \"{}\", expected format is \
             (YYYY-MM-DD HH:MM:SS[.US][±HH[:MM[:SS]]| ZONE])",
            String::from_utf8_lossy(input)
        )
    }

    /// DuckDB's message for TIMESTAMP text with a non-UTC time zone
    /// (`Timestamp::UnsupportedTimezoneError`).
    pub fn unsupported_timezone_error(input: &[u8]) -> String {
        format!(
            "timestamp field value \"{}\" has a timestamp that is not UTC.\nUse the TIMESTAMPTZ type \
             with the ICU extension loaded to handle non-UTC timestamps.",
            String::from_utf8_lossy(input)
        )
    }

    /// DuckDB's message for TIMESTAMP text outside the supported range (`Timestamp::RangeError`).
    pub fn range_error(input: &[u8]) -> String {
        format!(
            "timestamp field value out of range: \"{}\"",
            String::from_utf8_lossy(input)
        )
    }

    /// Appends DuckDB's text form, `<date> HH:MM:SS[.ffffff]` or `infinity`/`-infinity`
    /// (`StringFromTimestamp<false>`).
    ///
    /// Fails like DuckDB for the values in the last partial day before the minimum
    /// timestamp, whose day does not fit in microseconds.
    pub fn write_to(self, out: &mut Vec<u8>) -> Result<()> {
        if self.write_infinity(out) {
            return Ok(());
        }
        let (date, time) = self
            .to_datetime()
            .ok_or_else(|| Error::conversion("Date out of range in timestamp conversion"))?;
        date.write_ymd(out);
        out.push(b' ');
        time.write_to(out);
        Ok(())
    }

    /// Appends DuckDB's text form of a `TIMESTAMP WITH TIME ZONE` in UTC,
    /// `<date> HH:MM:SS[.ffffff]+00` (`StringCastTZ` for `timestamp_t`).
    pub fn write_tz_to(self, out: &mut Vec<u8>) -> Result<()> {
        if self.write_infinity(out) {
            return Ok(());
        }
        self.write_to(out)?;
        out.extend_from_slice(b"+00");
        Ok(())
    }

    /// Appends DuckDB's text form of a `TIMESTAMP_NS` value,
    /// `<date> HH:MM:SS[.fffffffff]` or `infinity`/`-infinity` (`StringFromTimestamp<true>`).
    ///
    /// Fails like DuckDB for the values in the last partial day before the minimum, whose
    /// day does not fit in nanoseconds.
    pub fn write_ns_to(nanos: i64, out: &mut Vec<u8>) -> Result<()> {
        if Self(nanos).write_infinity(out) {
            return Ok(());
        }
        let date = Self(nanos.div_euclid(NANOS_PER_MICRO)).to_date();
        let day_start = i64::from(date.0)
            .checked_mul(NANOS_PER_DAY)
            .ok_or_else(|| Error::conversion("Date out of range in timestamp_ns conversion"))?;
        date.write_ymd(out);
        out.push(b' ');
        // `day_start <= nanos < day_start + NANOS_PER_DAY`, so this cannot overflow.
        push_time_of_day(out, (nanos - day_start).unsigned_abs(), 9);
        Ok(())
    }

    /// Writes `infinity`/`-infinity` and returns `true` for infinite values.
    fn write_infinity(self, out: &mut Vec<u8>) -> bool {
        if self == Self::INFINITY {
            out.extend_from_slice(b"infinity");
        } else if self == Self::NEG_INFINITY {
            out.extend_from_slice(b"-infinity");
        } else {
            return false;
        }
        true
    }

    /// The timestamp `micros` microseconds after the epoch (`Timestamp::FromEpochMicroSeconds`).
    pub const fn from_epoch_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// The timestamp `nanos` nanoseconds after the epoch, truncated towards zero
    /// (`Timestamp::FromEpochNanoSeconds`).
    pub const fn from_epoch_nanoseconds(nanos: i64) -> Self {
        Self(nanos / NANOS_PER_MICRO)
    }

    /// The timestamp `seconds` seconds after the epoch, or DuckDB's error on overflow
    /// (`Timestamp::FromEpochSeconds`).
    pub fn try_from_epoch_seconds(seconds: i64) -> Result<Self> {
        seconds
            .checked_mul(MICROS_PER_SEC)
            .map(Self)
            .ok_or_else(|| Error::conversion(S_TO_US))
    }

    /// The timestamp `millis` milliseconds after the epoch, or DuckDB's error on overflow
    /// (`Timestamp::FromEpochMs`).
    pub fn try_from_epoch_ms(millis: i64) -> Result<Self> {
        millis
            .checked_mul(MICROS_PER_MSEC)
            .map(Self)
            .ok_or_else(|| Error::conversion(MS_TO_US))
    }

    /// Whole seconds since the epoch, truncated towards zero (`Timestamp::GetEpochSeconds`).
    pub const fn epoch_seconds(self) -> i64 {
        self.0 / MICROS_PER_SEC
    }

    /// Whole milliseconds since the epoch, truncated towards zero (`Timestamp::GetEpochMs`).
    pub const fn epoch_ms(self) -> i64 {
        self.0 / MICROS_PER_MSEC
    }

    /// Microseconds since the epoch (`Timestamp::GetEpochMicroSeconds`).
    pub const fn epoch_micros(self) -> i64 {
        self.0
    }

    /// Nanoseconds since the epoch, or DuckDB's error on overflow
    /// (`Timestamp::GetEpochNanoSeconds`).
    pub fn epoch_nanoseconds(self) -> Result<i64> {
        self.0
            .checked_mul(NANOS_PER_MICRO)
            .ok_or_else(|| Error::conversion(US_TO_NS))
    }

    /// The value in units of `power_of_ten` microseconds, rounded half away from the epoch
    /// (`Timestamp::GetEpochRounded`; `1000` gives milliseconds, `1_000_000` seconds).
    ///
    /// Like DuckDB, this scales before rounding, so a negative value with a remainder below
    /// half a unit still rounds down (`-0.6` units gives `-1`, `-0.4` gives `0`).
    pub const fn epoch_rounded(self, power_of_ten: i64) -> i64 {
        let scaling = if power_of_ten >= 4 { power_of_ten / 2 } else { 1 };
        let halves = self.0 / scaling;
        let halves = if halves < 0 {
            halves.wrapping_sub(1)
        } else {
            halves.wrapping_add(1)
        };
        halves / 2
    }

    /// The `TIMESTAMP_NS` value of this timestamp plus `nanos`, passing infinities through,
    /// or `None` if it does not fit (`Timestamp::TryFromTimestampNanos`).
    pub const fn to_timestamp_ns(self, nanos: i32) -> Option<i64> {
        if !self.is_finite() {
            return Some(self.0);
        }
        let Some(scaled) = self.0.checked_mul(NANOS_PER_MICRO) else {
            return None;
        };
        match scaled.checked_add(nanos as i64) {
            Some(value) if Self(value).is_finite() => Some(value),
            _ => None,
        }
    }

    /// The time of day of a `TIMESTAMP_NS` value at nanosecond precision, or DuckDB's error
    /// for infinities and for days that do not fit in nanoseconds (`Timestamp::GetTimeNs`).
    pub fn time_ns_of_ns(nanos: i64) -> Result<TimeNs> {
        if !Self::is_finite_raw(nanos) {
            return Err(Error::conversion("Can't get TIME_NS of infinite TIMESTAMP"));
        }
        let days = nanos.div_euclid(NANOS_PER_DAY);
        let day_start = days
            .checked_mul(NANOS_PER_DAY)
            .ok_or_else(|| Error::conversion("Overflow extracting TIME_NS of TIMESTAMP"))?;
        Ok(TimeNs(nanos - day_start))
    }
}

impl fmt::Display for Timestamp {
    /// DuckDB's text form (see [`Timestamp::write_to`]). The values DuckDB refuses to
    /// print are rendered from their floor date and time of day instead of failing.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = Vec::with_capacity(40);
        if self.write_to(&mut buf).is_err() {
            buf.clear();
            self.to_date().write_ymd(&mut buf);
            buf.push(b' ');
            self.to_time().write_to(&mut buf);
        }
        f.write_str(&String::from_utf8_lossy(&buf))
    }
}

/// Whether `c` may appear in a time zone name (`Timestamp::CharacterIsTimeZone`).
const fn is_time_zone_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'/' | b'+' | b'-' | b':')
}

/// Parses a `±HH[[:]MM[:SS]]` UTC offset at `pos` (`Timestamp::TryParseUTCOffset`).
///
/// Returns the signed hours, minutes and seconds and the end position. Minutes are optional
/// (an incomplete `:M` still consumes the colon) and seconds need the colon form.
pub(crate) fn parse_utc_offset(input: &[u8], pos: usize) -> Option<(i32, i32, i32, usize)> {
    let sign = match input.get(pos)? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let two_digits = |at: usize| Some(digit_at(input, at)? * 10 + digit_at(input, at + 1)?);
    let hours = sign * two_digits(pos + 1)?;
    let mut cur = pos + 3;
    if cur >= input.len() {
        return Some((hours, 0, 0, cur));
    }
    let colons = input.get(cur) == Some(&b':');
    if colons {
        cur += 1;
    }
    let Some(minutes) = two_digits(cur) else {
        return Some((hours, 0, 0, cur));
    };
    let minutes = sign * minutes;
    cur += 2;
    if !colons || input.get(cur) != Some(&b':') {
        return Some((hours, minutes, 0, cur));
    }
    cur += 1;
    match two_digits(cur) {
        Some(seconds) => Some((hours, minutes, sign * seconds, cur + 2)),
        None => Some((hours, minutes, 0, cur)),
    }
}

// ---------------------------------------------------------------------------------------
// Conversions between TIMESTAMP (µs), TIMESTAMP_S, TIMESTAMP_MS and TIMESTAMP_NS
// (`CastTimestamp*` in DuckDB's cast_operators.cpp). The non-µs units are raw `i64`s.
// ---------------------------------------------------------------------------------------

/// TIMESTAMP to `TIMESTAMP_NS` (`CastTimestampUsToNs`); infinities pass through.
pub fn us_to_ns(timestamp: Timestamp) -> Result<i64> {
    if timestamp.is_finite() {
        timestamp.epoch_nanoseconds()
    } else {
        Ok(timestamp.0)
    }
}

/// TIMESTAMP to `TIMESTAMP_MS`, rounded half away from the epoch (`CastTimestampUsToMs`);
/// infinities pass through.
pub const fn us_to_ms(timestamp: Timestamp) -> i64 {
    if timestamp.is_finite() {
        timestamp.epoch_rounded(MICROS_PER_MSEC)
    } else {
        timestamp.0
    }
}

/// TIMESTAMP to `TIMESTAMP_S`, rounded half away from the epoch (`CastTimestampUsToSec`);
/// infinities pass through.
pub const fn us_to_sec(timestamp: Timestamp) -> i64 {
    if timestamp.is_finite() {
        timestamp.epoch_rounded(MICROS_PER_SEC)
    } else {
        timestamp.0
    }
}

/// `TIMESTAMP_MS` to TIMESTAMP (`CastTimestampMsToUs`); infinities pass through.
pub fn ms_to_us(millis: i64) -> Result<Timestamp> {
    if Timestamp::is_finite_raw(millis) {
        Timestamp::try_from_epoch_ms(millis)
    } else {
        Ok(Timestamp(millis))
    }
}

/// `TIMESTAMP_S` to TIMESTAMP (`CastTimestampSecToUs`); infinities pass through.
pub fn sec_to_us(seconds: i64) -> Result<Timestamp> {
    if Timestamp::is_finite_raw(seconds) {
        Timestamp::try_from_epoch_seconds(seconds)
    } else {
        Ok(Timestamp(seconds))
    }
}

/// `TIMESTAMP_NS` to TIMESTAMP, truncated towards zero (`CastTimestampNsToUs`); infinities
/// pass through. Note that `1969-12-31 23:59:59.999999999` becomes the epoch.
pub const fn ns_to_us(nanos: i64) -> Timestamp {
    if Timestamp::is_finite_raw(nanos) {
        Timestamp::from_epoch_nanoseconds(nanos)
    } else {
        Timestamp(nanos)
    }
}

/// `TIMESTAMP_MS` to `TIMESTAMP_NS` (`CastTimestampMsToNs`); infinities pass through.
pub fn ms_to_ns(millis: i64) -> Result<i64> {
    us_to_ns(ms_to_us(millis)?)
}

/// `TIMESTAMP_S` to `TIMESTAMP_MS` (`CastTimestampSecToMs`); infinities pass through.
pub fn sec_to_ms(seconds: i64) -> Result<i64> {
    sec_to_us(seconds).map(us_to_ms)
}

/// `TIMESTAMP_S` to `TIMESTAMP_NS` (`CastTimestampSecToNs`); infinities pass through.
pub fn sec_to_ns(seconds: i64) -> Result<i64> {
    us_to_ns(sec_to_us(seconds)?)
}

/// `TIMESTAMP_MS` to DATE (`CastTimestampMsToDate`). Like DuckDB, infinities are not special
/// cased and fail with the millisecond overflow error.
pub fn ms_to_date(millis: i64) -> Result<Date> {
    Timestamp::try_from_epoch_ms(millis).map(Timestamp::to_date)
}

/// `TIMESTAMP_MS` to TIME (`CastTimestampMsToTime`); infinities fail like [`ms_to_date`].
pub fn ms_to_time(millis: i64) -> Result<Time> {
    Timestamp::try_from_epoch_ms(millis)?.try_to_time()
}

/// `TIMESTAMP_S` to DATE (`CastTimestampSecToDate`); infinities map to infinities.
pub fn sec_to_date(seconds: i64) -> Result<Date> {
    sec_to_us(seconds).map(Timestamp::to_date)
}

/// `TIMESTAMP_S` to TIME (`CastTimestampSecToTime`); infinities are an error.
pub fn sec_to_time(seconds: i64) -> Result<Time> {
    sec_to_us(seconds)?.try_to_time()
}

/// `TIMESTAMP_NS` to DATE via the truncated microseconds (`CastTimestampNsToDate`);
/// infinities map to infinities.
pub const fn ns_to_date(nanos: i64) -> Date {
    ns_to_us(nanos).to_date()
}

/// `TIMESTAMP_NS` to TIME via the truncated microseconds (`CastTimestampNsToTime`);
/// infinities are an error.
pub fn ns_to_time(nanos: i64) -> Result<Time> {
    ns_to_us(nanos).try_to_time()
}

/// `TIMESTAMP_NS` to `TIME_NS` (`CastTimestampNsToTimeNs`); see [`Timestamp::time_ns_of_ns`].
pub fn ns_to_time_ns(nanos: i64) -> Result<TimeNs> {
    Timestamp::time_ns_of_ns(nanos)
}

/// DATE to `TIMESTAMP_NS`, or `None` on overflow (`TryCastToTimestampNS` for `date_t`);
/// infinities map to infinities.
pub fn date_to_ns(date: Date) -> Option<i64> {
    let timestamp = Timestamp::from_date(date)?;
    if timestamp.is_finite() {
        timestamp.0.checked_mul(NANOS_PER_MICRO)
    } else {
        Some(timestamp.0)
    }
}

/// DATE to `TIMESTAMP_MS`, or `None` on overflow (`TryCastToTimestampMS` for `date_t`);
/// infinities map to infinities.
pub fn date_to_ms(date: Date) -> Option<i64> {
    Timestamp::from_date(date).map(|t| if t.is_finite() { t.0 / MICROS_PER_MSEC } else { t.0 })
}

/// DATE to `TIMESTAMP_S`, or `None` on overflow (`TryCastToTimestampSec` for `date_t`);
/// infinities map to infinities.
pub fn date_to_sec(date: Date) -> Option<i64> {
    Timestamp::from_date(date).map(|t| if t.is_finite() { t.0 / MICROS_PER_SEC } else { t.0 })
}

/// VARCHAR to `TIMESTAMP_MS` (`TryCastToTimestampMS`): [`Timestamp::parse`] without offsets,
/// then [`us_to_ms`]. DuckDB reports failures with its generic cast error.
pub fn parse_ms(input: &[u8]) -> Option<i64> {
    Timestamp::parse(input, false)
        .ok()
        .map(|(timestamp, _)| us_to_ms(timestamp))
}

/// VARCHAR to `TIMESTAMP_S` (`TryCastToTimestampSec`): [`Timestamp::parse`] without offsets,
/// then [`us_to_sec`]. DuckDB reports failures with its generic cast error.
pub fn parse_sec(input: &[u8]) -> Option<i64> {
    Timestamp::parse(input, false)
        .ok()
        .map(|(timestamp, _)| us_to_sec(timestamp))
}

/// Appends the text form of a `TIMESTAMP_MS` value (`CastFromTimestampMS`).
pub fn write_ms_to(millis: i64, out: &mut Vec<u8>) -> Result<()> {
    ms_to_us(millis)?.write_to(out)
}

/// Appends the text form of a `TIMESTAMP_S` value (`CastFromTimestampSec`).
pub fn write_sec_to(seconds: i64, out: &mut Vec<u8>) -> Result<()> {
    sec_to_us(seconds)?.write_to(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(input: &str) -> Timestamp {
        Timestamp::parse(input.as_bytes(), false).unwrap().0
    }

    fn text(timestamp: Timestamp) -> String {
        let mut out = Vec::new();
        timestamp.write_to(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn ns_text(nanos: i64) -> Result<String> {
        let mut out = Vec::new();
        Timestamp::write_ns_to(nanos, &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    fn cast(input: &str, use_offset: bool) -> std::result::Result<String, String> {
        Timestamp::parse(input.as_bytes(), use_offset)
            .map(|(t, _)| text(t))
            .map_err(|e| e.message(input.as_bytes()))
    }

    fn format_error(input: &str) -> std::result::Result<String, String> {
        Err(format!(
            "invalid timestamp field format: \"{input}\", expected format is \
             (YYYY-MM-DD HH:MM:SS[.US][±HH[:MM[:SS]]| ZONE])"
        ))
    }

    fn range_error(input: &str) -> std::result::Result<String, String> {
        Err(format!("timestamp field value out of range: \"{input}\""))
    }

    fn tz_error(input: &str) -> std::result::Result<String, String> {
        Err(format!(
            "timestamp field value \"{input}\" has a timestamp that is not UTC.\nUse the TIMESTAMPTZ \
             type with the ICU extension loaded to handle non-UTC timestamps."
        ))
    }

    /// Expected values from DuckDB 1.5.6: `SELECT CAST(<input> AS TIMESTAMP)::VARCHAR`.
    #[test]
    fn cast_timestamp_matches_duckdb() {
        let ok = [
            ("2021-01-02 12:34:56", "2021-01-02 12:34:56"),
            ("2021-01-02T12:34:56", "2021-01-02 12:34:56"),
            ("2021-01-02T12:34:56Z", "2021-01-02 12:34:56"),
            ("2021-01-02 12:34:56.123456789", "2021-01-02 12:34:56.123456"),
            ("2021-01-02 12:34:56.5", "2021-01-02 12:34:56.5"),
            ("2021-01-02 12:34:56+05", "2021-01-02 12:34:56"),
            ("2021-01-02 12:34:56+05:30", "2021-01-02 12:34:56"),
            ("2021-01-02 12:34:56-15:59:59", "2021-01-02 12:34:56"),
            ("2021-01-02 12:34:56 UTC", "2021-01-02 12:34:56"),
            ("2021-01-02 12:34:56 utc", "2021-01-02 12:34:56"),
            ("2021-01-02 12:34:56 ", "2021-01-02 12:34:56"),
            ("2021-01-02 12:34:56 UTC  ", "2021-01-02 12:34:56"),
            ("2021-01-02 12:34:56Z ", "2021-01-02 12:34:56"),
            ("2021-01-02", "2021-01-02 00:00:00"),
            ("2021-01-02  12:00:00", "2021-01-02 12:00:00"),
            ("2021-01-02 12:00", "2021-01-02 12:00:00"),
            ("2021-01-02T12:34", "2021-01-02 12:34:00"),
            ("2021-01-02 12:34:56.", "2021-01-02 12:34:56"),
            ("2021-01-02 1:2:3", "2021-01-02 01:02:03"),
            ("2021-01-02 12:34:5", "2021-01-02 12:34:05"),
            ("2021-1-2 3:4:5", "2021-01-02 03:04:05"),
            (" 2021-01-02 12:34:56", "2021-01-02 12:34:56"),
            ("2021-01-02 24:00:00", "2021-01-03 00:00:00"),
            ("infinity", "infinity"),
            ("-infinity", "-infinity"),
            ("inf", "infinity"),
            ("epoch", "1970-01-01 00:00:00"),
            ("294247-01-10 04:00:54.775806", "294247-01-10 04:00:54.775806"),
            ("294247-01-10 04:00:54.775806+00", "294247-01-10 04:00:54.775806"),
            ("290309-12-22 (BC) 00:00:00", "290309-12-22 (BC) 00:00:00"),
            ("-290308-12-22 00:00:00", "290309-12-22 (BC) 00:00:00"),
            ("0044-03-15 (BC) 12:00:00", "0044-03-15 (BC) 12:00:00"),
            ("-2021-01-02 12:34:56", "2022-01-02 (BC) 12:34:56"),
            ("1969-12-31 23:59:59.999999", "1969-12-31 23:59:59.999999"),
        ];
        for (input, expected) in ok {
            assert_eq!(cast(input, false).as_deref(), Ok(expected), "{input:?}");
        }
        for input in [
            "2021-01-02 12:34:56 UTC x",
            "2021-01-02 12:34:56Zx",
            "2021-01-02 ",
            "2021-01-02x",
            "2021-01-02 12",
            "2021-01-02T",
            "2021-01-0212:34:56",
            "2021-01-02 12:34:56 !",
            "2021-01-02 12:34:56.123456 (BC)",
            "infinity 12:00",
            "",
            "x",
        ] {
            assert_eq!(cast(input, false), format_error(input), "{input:?}");
        }
        for input in [
            "2021-01-02 24:00:01",
            "2021-01-02 25:00:00",
            "2021-02-29 00:00:00",
            "294247-01-10 04:00:54.775807",
            "290309-12-21 (BC) 23:59:59.999999",
            "290309-12-21 (BC) 00:00:00",
            "-290308-12-21 23:59:59",
            "5881580-07-10",
        ] {
            assert_eq!(cast(input, false), range_error(input), "{input:?}");
        }
        for input in [
            "2021-01-02 12:34:56 Europe/Amsterdam",
            "2021-01-02 12:34:56 GMT",
            "2021-01-02 12:34:56X",
            "2021-01-02 12:34:56 +05",
            "2021-01-02 12:34:56+5",
            "2021-01-02 12:34:56 UTC+1",
            "2021-01-02 12:34:56 U_T",
            "2021-01-02 12:34:56 +",
        ] {
            assert_eq!(cast(input, false), tz_error(input), "{input:?}");
        }
    }

    /// `CAST(<input> AS TIMESTAMPTZ)::VARCHAR` in DuckDB 1.5.6 with `TimeZone='UTC'`.
    #[test]
    fn cast_timestamptz_applies_offsets() {
        let tz = |input: &str| {
            let (t, _) = Timestamp::parse(input.as_bytes(), true).unwrap();
            let mut out = Vec::new();
            t.write_tz_to(&mut out).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert_eq!(tz("2021-01-02 12:34:56.123456"), "2021-01-02 12:34:56.123456+00");
        assert_eq!(tz("2021-01-02 12:34:56.000001"), "2021-01-02 12:34:56.000001+00");
        assert_eq!(tz("2021-01-02 12:34:56+05"), "2021-01-02 07:34:56+00");
        assert_eq!(tz("2021-01-02 12:34:56-15:59:59"), "2021-01-03 04:34:55+00");
        assert_eq!(tz("2021-01-02 12:34:56 utc"), "2021-01-02 12:34:56+00");
        assert_eq!(tz("0044-03-15 (BC) 12:00:00"), "0044-03-15 (BC) 12:00:00+00");
        // The instant parses, but core DuckDB cannot print it (ICU's TIMESTAMPTZ cast can).
        let (low, _) = Timestamp::parse(b"290309-12-22 (BC) 00:00:00+01", true).unwrap();
        assert_eq!(low.to_string(), "290309-12-21 (BC) 23:00:00");
        assert!(low.write_tz_to(&mut Vec::new()).is_err());
        assert_eq!(tz("infinity"), "infinity");
        assert_eq!(
            cast("294247-01-10 04:00:54.775806-01", true),
            range_error("294247-01-10 04:00:54.775806-01")
        );
    }

    #[test]
    fn parse_parts_exposes_zone_and_offset() {
        let parsed = Timestamp::parse_parts(b"2021-01-02 12:34:56.1234567 Europe/Amsterdam ", true).unwrap();
        assert_eq!(parsed.time_zone, b"Europe/Amsterdam");
        assert_eq!(parsed.nanos, 700);
        assert!(!parsed.has_offset);
        assert_eq!(text(parsed.timestamp), "2021-01-02 12:34:56.123456");
        let parsed = Timestamp::parse_parts(b"2021-01-02 12:34:56Z", true).unwrap();
        assert!(parsed.has_offset);
        assert!(Timestamp::parse_parts(b"epoch", true).unwrap().has_offset);
    }

    /// Expected values from DuckDB 1.5.6: `SELECT CAST(<input> AS TIMESTAMP_NS)::VARCHAR`.
    #[test]
    fn cast_timestamp_ns_matches_duckdb() {
        let cast_ns = |input: &str| Timestamp::parse_ns(input.as_bytes()).map(|n| ns_text(n).unwrap());
        let ok = [
            ("2021-01-02 12:34:56.123456789", "2021-01-02 12:34:56.123456789"),
            ("2021-01-02 12:34:56.1234567891", "2021-01-02 12:34:56.123456789"),
            ("2021-01-02 12:34:56.0000005", "2021-01-02 12:34:56.0000005"),
            ("2021-01-02 12:34:56.100000001", "2021-01-02 12:34:56.100000001"),
            ("2021-01-02 12:34:56+05", "2021-01-02 07:34:56"),
            ("2021-01-02 12:34:56+0530", "2021-01-02 07:04:56"),
            ("2021-01-02 12:34:56+05:30:15", "2021-01-02 07:04:41"),
            ("2021-01-02 12:34:56+99", "2020-12-29 09:34:56"),
            ("2021-01-02 12:34:56 UTC", "2021-01-02 12:34:56"),
            ("infinity", "infinity"),
            ("-infinity", "-infinity"),
            ("epoch", "1970-01-01 00:00:00"),
            ("2262-04-11 23:47:16.854775806", "2262-04-11 23:47:16.854775806"),
            ("1960-01-01 00:00:00.000000001", "1960-01-01 00:00:00.000000001"),
            ("1700-01-01 00:00:00.000000001", "1700-01-01 00:00:00.000000001"),
            ("1969-12-31 23:59:59.999999999", "1969-12-31 23:59:59.999999999"),
        ];
        for (input, expected) in ok {
            assert_eq!(cast_ns(input).as_deref(), Ok(expected), "{input:?}");
        }
        for input in [
            "x",
            "2021-01-02 12:34:56 CET",
            "2021-01-02 12:34:56 +05",
            "2021-01-02 12:34:56+05:3",
            "2021-01-02 12:34:56+5",
            "2262-04-11 23:47:16.854775807",
            "2262-04-11 23:47:16.854775808",
            "1677-09-21 00:12:43.145224193",
            "1677-09-21 00:12:43.145224192",
            "0044-03-15 (BC) 12:34:56.100000001",
            "294247-01-10 04:00:54.775806-01",
        ] {
            assert!(cast_ns(input).is_err(), "{input:?}");
        }
    }

    /// `make_timestamp_ns(<n>)::VARCHAR` in DuckDB 1.5.6.
    #[test]
    fn write_ns_matches_duckdb() {
        assert_eq!(ns_text(-1).unwrap(), "1969-12-31 23:59:59.999999999");
        assert_eq!(ns_text(1).unwrap(), "1970-01-01 00:00:00.000000001");
        assert_eq!(ns_text(1000).unwrap(), "1970-01-01 00:00:00.000001");
        assert_eq!(
            ns_text(9_223_372_036_854_775_806).unwrap(),
            "2262-04-11 23:47:16.854775806"
        );
        assert_eq!(
            ns_text(-9_223_286_400_000_000_000).unwrap(),
            "1677-09-22 00:00:00"
        );
        for nanos in [-9_223_286_400_000_000_001, -9_223_372_036_854_775_806, i64::MIN] {
            assert_eq!(
                ns_text(nanos).unwrap_err().to_string(),
                "Conversion Error: Date out of range in timestamp_ns conversion"
            );
        }
    }

    /// `make_timestamp(<n>)` in DuckDB 1.5.6.
    #[test]
    fn write_extremes_match_duckdb() {
        assert_eq!(
            text(Timestamp(9_223_372_036_854_775_806)),
            "294247-01-10 04:00:54.775806"
        );
        let low = Timestamp(-9_223_372_036_854_775_806);
        let mut out = Vec::new();
        assert_eq!(
            low.write_to(&mut out).unwrap_err().to_string(),
            "Conversion Error: Date out of range in timestamp conversion"
        );
        assert_eq!(low.to_date().to_string(), "290309-12-21 (BC)");
        assert_eq!(low.to_time().to_string(), "19:59:05.224194");
        assert_eq!(low.to_string(), "290309-12-21 (BC) 19:59:05.224194");
        assert_eq!(Timestamp::INFINITY.to_string(), "infinity");
    }

    #[test]
    fn date_and_time_parts() {
        let t = ts("1969-12-31 23:59:59.5");
        assert_eq!(t.to_date().to_string(), "1969-12-31");
        assert_eq!(t.to_time().to_string(), "23:59:59.5");
        assert_eq!(t.to_datetime(), Some((t.to_date(), t.to_time())));
        assert_eq!(Timestamp::INFINITY.to_date(), Date::INFINITY);
        assert_eq!(Timestamp::NEG_INFINITY.to_date(), Date::NEG_INFINITY);
        assert_eq!(Timestamp::INFINITY.to_datetime(), None);
        assert_eq!(
            Timestamp::INFINITY.try_to_time().unwrap_err().to_string(),
            "Conversion Error: Can't get TIME of infinite TIMESTAMP"
        );
        let date = Date::from_ymd(2021, 1, 2).unwrap();
        assert_eq!(text(Timestamp::from_date(date).unwrap()), "2021-01-02 00:00:00");
        assert_eq!(Timestamp::from_date(Date::INFINITY), Some(Timestamp::INFINITY));
        assert_eq!(Timestamp::from_date(Date::MAX), None);
        let tz = TimeTz::new(Time::from_hms_micros(12, 0, 0, 0), 3600);
        assert_eq!(
            text(Timestamp::from_datetime_tz(date, tz).unwrap()),
            "2021-01-02 11:00:00"
        );
    }

    #[test]
    fn epoch_helpers() {
        assert_eq!(
            text(Timestamp::try_from_epoch_ms(9_223_372_036_854_775).unwrap()),
            "294247-01-10 04:00:54.775"
        );
        assert_eq!(
            Timestamp::try_from_epoch_ms(9_223_372_036_854_776)
                .unwrap_err()
                .to_string(),
            "Conversion Error: Could not convert Timestamp(MS) to Timestamp(US)"
        );
        assert!(Timestamp::try_from_epoch_seconds(9_223_372_036_855).is_err());
        assert_eq!(Timestamp::from_epoch_nanoseconds(-1), Timestamp::EPOCH);
        assert_eq!(Timestamp(-1_500_000).epoch_seconds(), -1);
        assert_eq!(Timestamp(-1_500).epoch_ms(), -1);
        // Rounding half away from zero after scaling, as DuckDB does.
        let rounded = |micros: i64, unit: i64| Timestamp(micros).epoch_rounded(unit);
        assert_eq!(rounded(500_000, 1_000_000), 1);
        assert_eq!(rounded(499_999, 1_000_000), 0);
        assert_eq!(rounded(-500_000, 1_000_000), -1);
        assert_eq!(rounded(-499_999, 1_000_000), 0);
        assert_eq!(rounded(-600_000, 1_000_000), -1);
        assert_eq!(rounded(-1, 1_000_000), 0);
        assert_eq!(rounded(1_500_000, 1_000_000), 2);
        assert_eq!(rounded(-1_500_000, 1_000_000), -2);
        assert_eq!(rounded(-500, 1000), -1);
        assert_eq!(rounded(-499, 1000), 0);
        assert_eq!(Timestamp(i64::MAX - 1).epoch_rounded(1), i64::MAX / 2);
        assert_eq!(Timestamp(1).to_timestamp_ns(5), Some(1005));
        assert_eq!(Timestamp::INFINITY.to_timestamp_ns(5), Some(i64::MAX));
        assert_eq!(Timestamp(9_223_372_036_854_775).to_timestamp_ns(807), None);
    }

    /// Conversions between the TIMESTAMP units, checked against DuckDB 1.5.6 casts.
    #[test]
    fn unit_casts_match_duckdb() {
        // TIMESTAMP -> TIMESTAMP_MS / TIMESTAMP_S (rounded) -> VARCHAR.
        let ms = |s: &str| {
            let mut out = Vec::new();
            write_ms_to(us_to_ms(ts(s)), &mut out).unwrap();
            String::from_utf8(out).unwrap()
        };
        let sec = |s: &str| {
            let mut out = Vec::new();
            write_sec_to(us_to_sec(ts(s)), &mut out).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert_eq!(ms("2021-01-02 12:34:56.123500"), "2021-01-02 12:34:56.124");
        assert_eq!(ms("1969-12-31 23:59:59.999500"), "1969-12-31 23:59:59.999");
        assert_eq!(ms("1969-12-31 23:59:59.999499"), "1969-12-31 23:59:59.999");
        assert_eq!(ms("infinity"), "infinity");
        assert_eq!(sec("1969-12-31 23:59:59.5"), "1969-12-31 23:59:59");
        assert_eq!(sec("1969-12-31 23:59:59.499999"), "1969-12-31 23:59:59");
        assert_eq!(sec("2021-01-02 12:34:56.5"), "2021-01-02 12:34:57");
        let max = ts("294247-01-10 04:00:54.775806");
        assert_eq!(
            write_sec_to(us_to_sec(max), &mut Vec::new())
                .unwrap_err()
                .to_string(),
            "Conversion Error: Could not convert Timestamp(S) to Timestamp(US)"
        );
        assert_eq!(
            write_ms_to(us_to_ms(max), &mut Vec::new())
                .unwrap_err()
                .to_string(),
            "Conversion Error: Could not convert Timestamp(MS) to Timestamp(US)"
        );
        // VARCHAR -> TIMESTAMP_MS / TIMESTAMP_S.
        assert_eq!(parse_ms(b"2021-01-02 12:34:56.1235").map(|v| v % 1000), Some(124));
        assert_eq!(parse_ms(b"2021-01-02 12:34:56.1225").map(|v| v % 1000), Some(123));
        assert_eq!(
            parse_sec(b"2021-01-02 12:34:56+05"),
            parse_sec(b"2021-01-02 12:34:56")
        );
        assert_eq!(parse_sec(b"infinity"), Some(i64::MAX));
        assert_eq!(parse_sec(b"2021-01-02 12:34:56 CET"), None);
        assert_eq!(parse_ms(b"x"), None);

        // TIMESTAMP -> TIMESTAMP_NS.
        assert_eq!(us_to_ns(Timestamp::INFINITY).unwrap(), i64::MAX);
        assert_eq!(
            us_to_ns(ts("2300-01-02 12:34:56")).unwrap_err().to_string(),
            "Conversion Error: Could not convert Timestamp(US) to Timestamp(NS)"
        );
        assert!(ms_to_ns(ts("2300-01-01").epoch_ms()).is_err());
        assert!(sec_to_ns(ts("2300-01-01").epoch_seconds()).is_err());

        // TIMESTAMP_NS -> TIMESTAMP / DATE / TIME / TIME_NS.
        let ns = Timestamp::parse_ns(b"1969-12-31 23:59:59.999999999").unwrap();
        assert_eq!(ns_to_us(ns), Timestamp::EPOCH);
        assert_eq!(ns_to_date(ns).to_string(), "1970-01-01");
        assert_eq!(ns_to_time(ns).unwrap().to_string(), "00:00:00");
        assert_eq!(ns_to_time_ns(ns).unwrap().to_string(), "23:59:59.999999999");
        assert_eq!(ns_to_date(i64::MAX), Date::INFINITY);
        assert_eq!(ns_to_us(-i64::MAX), Timestamp::NEG_INFINITY);
        assert_eq!(
            ns_to_time(i64::MAX).unwrap_err().to_string(),
            "Conversion Error: Can't get TIME of infinite TIMESTAMP"
        );
        assert_eq!(
            ns_to_time_ns(i64::MAX).unwrap_err().to_string(),
            "Conversion Error: Can't get TIME_NS of infinite TIMESTAMP"
        );
        assert_eq!(
            ns_to_time_ns(i64::MIN).unwrap_err().to_string(),
            "Conversion Error: Overflow extracting TIME_NS of TIMESTAMP"
        );

        // TIMESTAMP_MS -> DATE / TIME / TIMESTAMP_NS.
        let millis = ts("1969-12-31 23:59:59.999").epoch_ms();
        assert_eq!(ms_to_date(millis).unwrap().to_string(), "1969-12-31");
        assert_eq!(ms_to_time(millis).unwrap().to_string(), "23:59:59.999");
        assert_eq!(ms_to_ns(millis).unwrap(), -1_000_000);
        assert_eq!(
            ms_to_date(i64::MAX).unwrap_err().to_string(),
            "Conversion Error: Could not convert Timestamp(MS) to Timestamp(US)"
        );
        assert!(ms_to_time(i64::MAX).is_err());
        assert_eq!(ms_to_us(i64::MAX).unwrap(), Timestamp::INFINITY);

        // TIMESTAMP_S -> DATE / TIME / TIMESTAMP_MS.
        assert_eq!(sec_to_date(-1).unwrap().to_string(), "1969-12-31");
        assert_eq!(sec_to_date(i64::MAX).unwrap(), Date::INFINITY);
        assert_eq!(
            sec_to_time(i64::MAX).unwrap_err().to_string(),
            "Conversion Error: Can't get TIME of infinite TIMESTAMP"
        );
        assert_eq!(sec_to_ms(i64::MAX).unwrap(), i64::MAX);
        assert_eq!(sec_to_ms(-1).unwrap(), -1000);

        // DATE -> TIMESTAMP_{NS,MS,S}.
        let date = Date::from_ymd(1500, 1, 2).unwrap();
        assert_eq!(date_to_sec(date), Some(date.epoch_seconds()));
        assert_eq!(date_to_ms(date), date.epoch_millis());
        assert_eq!(date_to_ns(date), None);
        assert_eq!(
            date_to_ns(Date::from_ymd(2021, 1, 2).unwrap()),
            Some(1_609_545_600_000_000_000)
        );
        assert_eq!(date_to_ms(Date::NEG_INFINITY), Some(-i64::MAX));
        assert_eq!(date_to_sec(Date::MAX), None);
    }

    #[test]
    fn utc_offsets() {
        let offset = |s: &str| parse_utc_offset(s.as_bytes(), 0);
        assert_eq!(offset("+05"), Some((5, 0, 0, 3)));
        assert_eq!(offset("-05:30"), Some((-5, -30, 0, 6)));
        assert_eq!(offset("-0530"), Some((-5, -30, 0, 5)));
        assert_eq!(offset("+05:30:15x"), Some((5, 30, 15, 9)));
        assert_eq!(offset("+053015"), Some((5, 30, 0, 5)));
        assert_eq!(offset("+05:3"), Some((5, 0, 0, 4)));
        assert_eq!(offset("+05:30:1"), Some((5, 30, 0, 7)));
        assert_eq!(offset("+5"), None);
        assert_eq!(offset("05"), None);
        assert_eq!(offset(""), None);
    }
}
