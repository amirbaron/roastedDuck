//! INTERVAL values (DuckDB's `interval_t` and `Interval`).

use std::cmp::Ordering;
use std::fmt;

use roastedduck_common::Error;

use crate::date::{push_fraction, push_padded};
use crate::{Interval, NativeType, PhysicalType, Time};

// `Interval` itself (DuckDB's `interval_t`: months, days and microseconds) is generated
// from DuckDB's serialization specs; this module adds its parsing, formatting and
// DuckDB's normalised comparison.
//
// The derived `PartialEq`/`Eq`/`Hash` compare the three fields bitwise, so
// `1 month != 30 days` for them. DuckDB compares intervals after normalising them
// (carrying 24 hours into a day and 30 days into a month): use `Interval::normalized_eq`
// for SQL equality. The `Ord` implementation follows DuckDB's normalised ordering, so `cmp`
// may report `Equal` for bitwise-different values.

// Three plain integer fields of 16 bytes in total: no padding, whatever the field order.
const _: () = assert!(size_of::<Interval>() == 16);

// SAFETY: `Interval` consists of two `i32`s and an `i64` with no padding (checked above),
// so every bit pattern of its 16 bytes is a valid value and it has no drop glue.
unsafe impl NativeType for Interval {
    const PHYSICAL: PhysicalType = PhysicalType::Interval;
}

/// Why a string could not be parsed as an INTERVAL.
///
/// DuckDB raises some of these failures as `Invalid Input` or `Out of Range` errors rather
/// than conversion errors, and one of them even under `TRY_CAST`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntervalParseError {
    /// Parsing returned false without throwing; the vector cast reports this
    /// conversion failure with the cast source and query location.
    Reported(Error),
    /// The cast fails: `CAST` raises the error, `TRY_CAST` yields NULL.
    Cast(Error),
    /// DuckDB raises this error unconditionally, even under `TRY_CAST`
    /// (`AGO interval value is out of range`).
    Unconditional(Error),
}

impl IntervalParseError {
    /// The underlying error (kind and message).
    pub const fn error(&self) -> &Error {
        match self {
            Self::Reported(error) | Self::Cast(error) | Self::Unconditional(error) => error,
        }
    }

    /// Consumes the error, returning the underlying [`Error`].
    pub fn into_error(self) -> Error {
        match self {
            Self::Reported(error) | Self::Cast(error) | Self::Unconditional(error) => error,
        }
    }
}

impl fmt::Display for IntervalParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error().fmt(f)
    }
}

impl std::error::Error for IntervalParseError {}

/// The date parts an interval string may use (`DatePartSpecifier`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Millennium,
    Century,
    Decade,
    Year,
    Quarter,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    /// A known date part that intervals do not support (`epoch`, `dow`, ...).
    Unsupported,
}

/// Every spelling DuckDB's `TryGetDatePartSpecifier` recognises. Intervals parse only ASCII
/// letters as units, so the names containing `_` can never match but are kept for fidelity.
const UNIT_NAMES: &[(&str, Unit)] = &[
    ("year", Unit::Year),
    ("yr", Unit::Year),
    ("y", Unit::Year),
    ("years", Unit::Year),
    ("yrs", Unit::Year),
    ("month", Unit::Month),
    ("mon", Unit::Month),
    ("months", Unit::Month),
    ("mons", Unit::Month),
    ("day", Unit::Day),
    ("days", Unit::Day),
    ("d", Unit::Day),
    ("dayofmonth", Unit::Day),
    ("decade", Unit::Decade),
    ("dec", Unit::Decade),
    ("decades", Unit::Decade),
    ("decs", Unit::Decade),
    ("century", Unit::Century),
    ("cent", Unit::Century),
    ("centuries", Unit::Century),
    ("c", Unit::Century),
    ("millennium", Unit::Millennium),
    ("mil", Unit::Millennium),
    ("millenniums", Unit::Millennium),
    ("millennia", Unit::Millennium),
    ("mils", Unit::Millennium),
    ("millenium", Unit::Millennium),
    ("microseconds", Unit::Microsecond),
    ("microsecond", Unit::Microsecond),
    ("us", Unit::Microsecond),
    ("usec", Unit::Microsecond),
    ("usecs", Unit::Microsecond),
    ("usecond", Unit::Microsecond),
    ("useconds", Unit::Microsecond),
    ("milliseconds", Unit::Millisecond),
    ("millisecond", Unit::Millisecond),
    ("ms", Unit::Millisecond),
    ("msec", Unit::Millisecond),
    ("msecs", Unit::Millisecond),
    ("msecond", Unit::Millisecond),
    ("mseconds", Unit::Millisecond),
    ("second", Unit::Second),
    ("sec", Unit::Second),
    ("seconds", Unit::Second),
    ("secs", Unit::Second),
    ("s", Unit::Second),
    ("minute", Unit::Minute),
    ("min", Unit::Minute),
    ("minutes", Unit::Minute),
    ("mins", Unit::Minute),
    ("m", Unit::Minute),
    ("hour", Unit::Hour),
    ("hr", Unit::Hour),
    ("hours", Unit::Hour),
    ("hrs", Unit::Hour),
    ("h", Unit::Hour),
    ("week", Unit::Week),
    ("weeks", Unit::Week),
    ("w", Unit::Week),
    ("weekofyear", Unit::Week),
    ("quarter", Unit::Quarter),
    ("quarters", Unit::Quarter),
    ("epoch", Unit::Unsupported),
    ("dow", Unit::Unsupported),
    ("dayofweek", Unit::Unsupported),
    ("weekday", Unit::Unsupported),
    ("isodow", Unit::Unsupported),
    ("doy", Unit::Unsupported),
    ("dayofyear", Unit::Unsupported),
    ("yearweek", Unit::Unsupported),
    ("isoyear", Unit::Unsupported),
    ("era", Unit::Unsupported),
    ("timezone", Unit::Unsupported),
    ("timezone_hour", Unit::Unsupported),
    ("timezone_minute", Unit::Unsupported),
    ("julian", Unit::Unsupported),
    ("jd", Unit::Unsupported),
];

impl Unit {
    fn from_name(name: &[u8]) -> Option<Self> {
        UNIT_NAMES
            .iter()
            .find(|(candidate, _)| candidate.as_bytes().eq_ignore_ascii_case(name))
            .map(|&(_, unit)| unit)
    }
}

/// Interval whitespace: only space, tab and newline (unlike the other temporal parsers).
const fn is_interval_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n')
}

/// An interval field that string parts are added to (`IntervalTryAddition<T>`).
trait Field: Copy {
    const TYPE_NAME: &'static str;
    fn from_i64(value: i64) -> Option<Self>;
    fn checked_add(self, other: Self) -> Option<Self>;
}

impl Field for i32 {
    const TYPE_NAME: &'static str = "INT32";
    fn from_i64(value: i64) -> Option<Self> {
        Self::try_from(value).ok()
    }
    fn checked_add(self, other: Self) -> Option<Self> {
        self.checked_add(other)
    }
}

impl Field for i64 {
    const TYPE_NAME: &'static str = "INT64";
    fn from_i64(value: i64) -> Option<Self> {
        Some(value)
    }
    fn checked_add(self, other: Self) -> Option<Self> {
        self.checked_add(other)
    }
}

/// Adds `number * multiplier` plus `fraction * multiplier / 10^6` (the fraction is in
/// millionths) to `target`, with DuckDB's overflow errors.
fn add_to<T: Field>(
    target: &mut T,
    number: i64,
    multiplier: i64,
    fraction: i64,
) -> Result<(), IntervalParseError> {
    let narrow = |value: i64| {
        T::from_i64(value).ok_or_else(|| {
            IntervalParseError::Cast(Error::invalid_input(format!(
                "Type INT64 with value {value} can't be cast because the value is out of range for the \
                 destination type {}",
                T::TYPE_NAME
            )))
        })
    };
    let out_of_range = || IntervalParseError::Cast(Error::out_of_range("interval value is out of range"));

    let addition = number.checked_mul(multiplier).ok_or_else(out_of_range)?;
    *target = target.checked_add(narrow(addition)?).ok_or_else(out_of_range)?;
    if fraction != 0 {
        // |fraction| < 10^6 and multiplier <= MICROS_PER_DAY, so this cannot overflow.
        let addition = fraction * multiplier / Interval::MICROS_PER_SEC;
        *target = target.checked_add(narrow(addition)?).ok_or_else(|| {
            IntervalParseError::Cast(Error::out_of_range("interval fraction is out of range"))
        })?;
    }
    Ok(())
}

/// Unit conversion factors (DuckDB's `Interval::*_PER_*`); the names are self-describing.
impl Interval {
    pub const MONTHS_PER_YEAR: i64 = 12;
    /// A month counts as 30 days when intervals are compared or normalised.
    pub const DAYS_PER_MONTH: i64 = 30;
    pub const MICROS_PER_MSEC: i64 = 1_000;
    pub const MICROS_PER_SEC: i64 = 1_000_000;
    pub const MICROS_PER_MINUTE: i64 = 60 * Self::MICROS_PER_SEC;
    pub const MICROS_PER_HOUR: i64 = 60 * Self::MICROS_PER_MINUTE;
    pub const MICROS_PER_DAY: i64 = 24 * Self::MICROS_PER_HOUR;
    pub const NANOS_PER_MICRO: i64 = 1_000;
    pub const NANOS_PER_MSEC: i64 = 1_000_000;
    pub const NANOS_PER_SEC: i64 = 1_000_000_000;
    pub const SECS_PER_MINUTE: i64 = 60;
    pub const SECS_PER_HOUR: i64 = 3_600;
    pub const SECS_PER_DAY: i64 = 86_400;
}

impl Interval {
    /// The interval of `months` months, `days` days and `micros` microseconds.
    pub const fn new(months: i32, days: i32, micros: i64) -> Self {
        Self { months, days, micros }
    }

    /// Parses an INTERVAL the way `CAST(varchar AS INTERVAL)` does (`Interval::FromCString`),
    /// returning DuckDB's error message (without the `"<Kind> Error: "` prefix) on failure.
    ///
    /// `strict` is accepted for symmetry with the other parsers; DuckDB ignores it. Use
    /// [`Interval::parse_detailed`] to learn the error kind and whether `TRY_CAST` may
    /// suppress the error.
    pub fn parse(input: &[u8], strict: bool) -> Result<Self, String> {
        let _ = strict;
        Self::parse_detailed(input).map_err(|error| error.into_error().raw_message().into_owned())
    }

    /// Parses an INTERVAL the way `CAST(varchar AS INTERVAL)` does, with DuckDB's error kinds.
    ///
    /// The text is an optional `@`, then a sequence of `[-]N[.F] <unit>` terms (units such as
    /// `year`, `mons`, `d`, `h`, `us`, case-insensitive), optionally ending in a
    /// `[-]H..:MM[:SS[.F]]` time (anything after it is ignored) or in `ago`, which negates
    /// everything. A lone number means seconds. ISO 8601 `P...` forms are rejected.
    pub fn parse_detailed(input: &[u8]) -> Result<Self, IntervalParseError> {
        let generic = || {
            IntervalParseError::Reported(Error::conversion(format!(
                "Could not convert string '{}' to INTERVAL",
                String::from_utf8_lossy(input)
            )))
        };
        let skip = |mut pos: usize| {
            while input.get(pos).copied().is_some_and(is_interval_space) {
                pos += 1;
            }
            pos
        };

        let mut result = Self::default();
        let mut pos = match input.first() {
            None | Some(b'P' | b'p') => return Err(generic()),
            Some(b'@') => 1,
            Some(_) => 0,
        };
        let mut found_any = false;
        loop {
            pos = skip(pos);
            let Some(&c) = input.get(pos) else { break };
            let negative = match c {
                b'0'..=b'9' => false,
                b'-' => {
                    pos += 1;
                    true
                }
                b'a' | b'A' => {
                    result.apply_ago(input, pos).ok_or_else(generic)??;
                    break;
                }
                _ => return Err(generic()),
            };

            let number_start = pos;
            while input.get(pos).is_some_and(u8::is_ascii_digit) {
                pos += 1;
            }
            if input.get(pos) == Some(&b':') {
                let time = Time::parse_prefix(input.get(number_start..).unwrap_or_default(), false)
                    .ok_or_else(generic)?;
                result.micros = result.micros.wrapping_add(time.time.0);
                if negative {
                    result.micros = result.micros.wrapping_neg();
                }
                found_any = true;
                break;
            }
            let digits = input.get(number_start..pos).unwrap_or_default();
            if digits.is_empty() {
                return Err(generic());
            }
            let mut number = parse_digits(digits).ok_or_else(|| {
                IntervalParseError::Cast(Error::invalid_input(format!(
                    "Could not convert string '{}' to INT64",
                    String::from_utf8_lossy(digits)
                )))
            })?;
            // The fraction in millionths; digits past the sixth are dropped.
            let mut fraction: i64 = 0;
            if input.get(pos) == Some(&b'.') {
                pos += 1;
                let mut scale = 100_000;
                while let Some(&c) = input.get(pos)
                    && c.is_ascii_digit()
                {
                    fraction += i64::from(c - b'0') * scale;
                    scale /= 10;
                    pos += 1;
                }
            }
            if negative {
                number = -number;
                fraction = -fraction;
            }

            pos = skip(pos);
            let unit_start = pos;
            while input.get(pos).is_some_and(u8::is_ascii_alphabetic) {
                pos += 1;
            }
            let unit_name = input.get(unit_start..pos).unwrap_or_default();
            if unit_name.is_empty() && !found_any {
                // A bare number is seconds, and must end the string.
                add_to(&mut result.micros, number, Self::MICROS_PER_SEC, 0)?;
                add_to(&mut result.micros, fraction, 1, 0)?;
                if skip(pos) < input.len() {
                    return Err(generic());
                }
                return Ok(result);
            }
            let unit = Unit::from_name(unit_name).ok_or_else(|| {
                IntervalParseError::Cast(Error::conversion(format!(
                    "extract specifier \"{}\" not recognized",
                    String::from_utf8_lossy(unit_name)
                )))
            })?;
            result
                .add_unit(unit, number, fraction)
                .map_err(|error| match error {
                    Some(error) => error,
                    None => IntervalParseError::Cast(Error::conversion(format!(
                        "extract specifier \"{}\" not supported for interval",
                        String::from_utf8_lossy(unit_name)
                    ))),
                })?;
            found_any = true;
        }
        if found_any { Ok(result) } else { Err(generic()) }
    }

    /// Adds `number[.fraction] unit`; `Err(None)` marks a unit intervals do not support.
    fn add_unit(&mut self, unit: Unit, number: i64, fraction: i64) -> Result<(), Option<IntervalParseError>> {
        const MONTHS_PER_QUARTER: i64 = 3;
        const DAYS_PER_WEEK: i64 = 7;
        let (months, days, micros) = (&mut self.months, &mut self.days, &mut self.micros);
        match unit {
            Unit::Millennium => add_to(months, number, 12_000, fraction)?,
            Unit::Century => add_to(months, number, 1_200, fraction)?,
            Unit::Decade => add_to(months, number, 120, fraction)?,
            Unit::Year => add_to(months, number, Self::MONTHS_PER_YEAR, fraction)?,
            Unit::Quarter => {
                add_to(months, number, MONTHS_PER_QUARTER, fraction)?;
                // The part of the fraction below a whole month becomes days.
                let fraction = fraction * MONTHS_PER_QUARTER % Self::MICROS_PER_SEC;
                add_to(days, 0, Self::DAYS_PER_MONTH, fraction)?;
            }
            Unit::Month => {
                add_to(months, number, 1, 0)?;
                add_to(days, 0, Self::DAYS_PER_MONTH, fraction)?;
            }
            Unit::Week => {
                add_to(days, number, DAYS_PER_WEEK, fraction)?;
                // The part of the fraction below a whole day becomes microseconds.
                let fraction = fraction * DAYS_PER_WEEK % Self::MICROS_PER_SEC;
                add_to(micros, 0, Self::MICROS_PER_DAY, fraction)?;
            }
            Unit::Day => {
                add_to(days, number, 1, 0)?;
                add_to(micros, 0, Self::MICROS_PER_DAY, fraction)?;
            }
            Unit::Hour => add_to(micros, number, Self::MICROS_PER_HOUR, fraction)?,
            Unit::Minute => add_to(micros, number, Self::MICROS_PER_MINUTE, fraction)?,
            Unit::Second => add_to(micros, number, Self::MICROS_PER_SEC, fraction)?,
            Unit::Millisecond => add_to(micros, number, Self::MICROS_PER_MSEC, fraction)?,
            Unit::Microsecond => {
                // Round the fraction half away from zero.
                let rounded = number.wrapping_add(fraction * 2 / Self::MICROS_PER_SEC);
                add_to(micros, rounded, 1, 0)?;
            }
            Unit::Unsupported => return Err(None),
        }
        Ok(())
    }

    /// Handles a trailing `ago` at `pos`: `None` if the text is not `ago` plus whitespace,
    /// otherwise negates the interval (or fails if a field cannot be negated).
    fn apply_ago(&mut self, input: &[u8], pos: usize) -> Option<Result<(), IntervalParseError>> {
        let word = input.get(pos..pos + 3)?;
        let rest = input.get(pos + 3..)?;
        if !word.eq_ignore_ascii_case(b"ago") || !rest.iter().copied().all(is_interval_space) {
            return None;
        }
        if self.months == i32::MIN || self.days == i32::MIN {
            return Some(Err(IntervalParseError::Unconditional(Error::out_of_range(
                "AGO interval value is out of range",
            ))));
        }
        *self = self.invert();
        Some(Ok(()))
    }

    /// The interval with every field negated (`Interval::Invert`; the microseconds wrap like
    /// DuckDB's for `i64::MIN`).
    #[must_use]
    pub const fn invert(self) -> Self {
        Self::new(
            self.months.wrapping_neg(),
            self.days.wrapping_neg(),
            self.micros.wrapping_neg(),
        )
    }

    /// An interval of `micros` microseconds split into whole days and the remainder
    /// (`Interval::FromMicro`).
    pub const fn from_micros(micros: i64) -> Self {
        Self::new(
            0,
            (micros / Self::MICROS_PER_DAY) as i32,
            micros % Self::MICROS_PER_DAY,
        )
    }

    /// The total length in microseconds counting a month as 30 days, or `None` on overflow
    /// (`Interval::TryGetMicro`).
    pub const fn total_micros(self) -> Option<i64> {
        let Some(months) = (self.months as i64).checked_mul(Self::DAYS_PER_MONTH * Self::MICROS_PER_DAY)
        else {
            return None;
        };
        let Some(days) = (self.days as i64).checked_mul(Self::MICROS_PER_DAY) else {
            return None;
        };
        match self.micros.checked_add(months) {
            Some(total) => total.checked_add(days),
            None => None,
        }
    }

    /// The `(months, days, micros)` DuckDB compares intervals by: microseconds carried into
    /// days, then days into 30-day months, each truncating towards zero
    /// (`interval_t::Normalize`).
    pub const fn normalized_parts(self) -> (i64, i64, i64) {
        let carry_days = self.micros / Self::MICROS_PER_DAY;
        let micros = self.micros - carry_days * Self::MICROS_PER_DAY;
        let days = self.days as i64 + carry_days;
        let carry_months = days / Self::DAYS_PER_MONTH;
        let days = days - carry_months * Self::DAYS_PER_MONTH;
        (self.months as i64 + carry_months, days, micros)
    }

    /// The normalised interval, borrowing back into smaller fields where a carried value
    /// does not fit in 32 bits (`interval_t::Normalize()`).
    #[must_use]
    pub const fn normalize(self) -> Self {
        let (months, days, micros) = self.normalized_parts();
        let (months, days) = borrow(months, days, Self::DAYS_PER_MONTH);
        let (days, micros) = borrow(days, micros, Self::MICROS_PER_DAY);
        Self::new(months, days, micros)
    }

    /// SQL equality: whether both intervals normalise to the same value (`Interval::Equals`),
    /// e.g. `1 month = 30 days` and `1 day = 24 hours`.
    pub const fn normalized_eq(self, other: Self) -> bool {
        let (lm, ld, lu) = self.normalized_parts();
        let (rm, rd, ru) = other.normalized_parts();
        lm == rm && ld == rd && lu == ru
    }

    /// Appends DuckDB's text form, e.g. `1 year 2 months -3 days 04:05:06.7`
    /// (`IntervalToStringCast::Format`).
    pub fn write_to(self, out: &mut Vec<u8>) {
        let start = out.len();
        let push_part = |out: &mut Vec<u8>, value: i32, name: &[u8]| {
            if value == 0 {
                return;
            }
            if out.len() != start {
                out.push(b' ');
            }
            if value < 0 {
                out.push(b'-');
            }
            push_padded(out, value.unsigned_abs().into(), 1);
            out.extend_from_slice(name);
            if value != 1 && value != -1 {
                out.push(b's');
            }
        };
        if self.months != 0 {
            let years = self.months / 12;
            push_part(out, years, b" year");
            push_part(out, self.months - years * 12, b" month");
        }
        push_part(out, self.days, b" day");

        if self.micros == 0 {
            if out.len() == start {
                out.extend_from_slice(b"00:00:00");
            }
            return;
        }
        if out.len() != start {
            out.push(b' ');
        }
        if self.micros < 0 {
            out.push(b'-');
        }
        let micros = self.micros.unsigned_abs();
        let seconds = micros / Self::MICROS_PER_SEC.unsigned_abs();
        push_padded(out, seconds / 3600, 2);
        out.push(b':');
        push_padded(out, seconds / 60 % 60, 2);
        out.push(b':');
        push_padded(out, seconds % 60, 2);
        push_fraction(out, micros % Self::MICROS_PER_SEC.unsigned_abs(), 6);
    }
}

/// Clamps `most` into an `i32`, moving the excess into `least` scaled by `scale`
/// (`interval_t::Borrow`).
const fn borrow(most: i64, least: i64, scale: i64) -> (i32, i64) {
    let clamped = if most > i32::MAX as i64 {
        i32::MAX
    } else if most < i32::MIN as i64 {
        i32::MIN
    } else {
        most as i32
    };
    let excess = most.wrapping_sub(clamped as i64);
    (clamped, least.wrapping_add(excess.wrapping_mul(scale)))
}

/// Parses a non-empty run of ASCII digits as an `i64` (`TryCast` to `int64_t`).
fn parse_digits(digits: &[u8]) -> Option<i64> {
    digits.iter().try_fold(0_i64, |value, &c| {
        value.checked_mul(10)?.checked_add(i64::from(c - b'0'))
    })
}

impl Ord for Interval {
    /// DuckDB's interval order (`Interval::GreaterThan`): lexicographic on
    /// [`Interval::normalized_parts`].
    fn cmp(&self, other: &Self) -> Ordering {
        self.normalized_parts().cmp(&other.normalized_parts())
    }
}

impl PartialOrd for Interval {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = Vec::with_capacity(70);
        self.write_to(&mut buf);
        f.write_str(&String::from_utf8_lossy(&buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roastedduck_common::ErrorKind;

    fn cast(input: &str) -> Result<String, String> {
        Interval::parse(input.as_bytes(), false).map(|i| i.to_string())
    }

    fn parse(input: &str) -> Interval {
        Interval::parse(input.as_bytes(), false).unwrap()
    }

    /// Expected values from DuckDB 1.5.6: `SELECT CAST(<input> AS INTERVAL)::VARCHAR`.
    #[test]
    fn cast_interval_matches_duckdb() {
        let ok = [
            ("1 year 2 months", "1 year 2 months"),
            ("1 day 02:03:04.5", "1 day 02:03:04.5"),
            ("-1 day", "-1 day"),
            ("@ 1 day", "1 day"),
            ("@1 day", "1 day"),
            (" 1 day", "1 day"),
            ("1 day ago", "-1 day"),
            ("1 day 2 hours ago", "-1 day -02:00:00"),
            ("1 day AGO  ", "-1 day"),
            ("42", "00:00:42"),
            ("42.5", "00:00:42.5"),
            ("-42.5", "-00:00:42.5"),
            ("42 ", "00:00:42"),
            ("1.5", "00:00:01.5"),
            ("02:03:04", "02:03:04"),
            ("-02:03:04", "-02:03:04"),
            ("100:00:00", "100:00:00"),
            ("1 day -02:03:04", "1 day -02:03:04"),
            ("1 day 02:03:04 junk", "1 day 02:03:04"),
            ("02:03:04 1 day", "02:03:04"),
            ("1:2", "01:02:00"),
            ("1 DaYs", "1 day"),
            ("1.5 years", "1 year 6 months"),
            ("1.5 months", "1 month 15 days"),
            ("1.5 days", "1 day 12:00:00"),
            ("1.5 weeks", "10 days 12:00:00"),
            ("1.5 week", "10 days 12:00:00"),
            ("1.33 weeks", "9 days 07:26:24"),
            ("1.5 quarters", "4 months 15 days"),
            ("1.33 quarters", "3 months 29 days"),
            ("-1.33 quarters", "-3 months -29 days"),
            ("1.5 hours", "01:30:00"),
            ("1.5 minutes", "00:01:30"),
            ("1.5 seconds", "00:00:01.5"),
            ("1.5 ms", "00:00:00.0015"),
            ("1.5 us", "00:00:00.000002"),
            ("1.4 us", "00:00:00.000001"),
            ("-1.5 us", "-00:00:00.000002"),
            ("1.999999 us", "00:00:00.000002"),
            ("1.5 decades", "15 years"),
            ("1.5 centuries", "150 years"),
            ("1.5 millennium", "1500 years"),
            ("1 Millennia 1 mils", "2000 years"),
            ("0.1234567 seconds", "00:00:00.123456"),
            ("1.99999999 seconds", "00:00:01.999999"),
            ("1.9999999 ms", "00:00:00.001999"),
            ("1.0000001 days", "1 day"),
            ("0.0000001 days", "00:00:00"),
            ("0.5 years", "6 months"),
            ("0.001 years", "00:00:00"),
            ("1.5 mons ago", "-1 month -15 days"),
            ("2 weeks", "14 days"),
            ("1 y 1 mon 1 d 1 h 1 m 1 s", "1 year 1 month 1 day 01:01:01"),
            ("3 quarters", "9 months"),
            ("1 year 1 year", "2 years"),
            ("1 year -1 month", "11 months"),
            ("-1 year -1 month", "-1 year -1 month"),
            ("-13 months", "-1 year -1 month"),
            ("13 months", "1 year 1 month"),
            ("-1 month", "-1 month"),
            ("5 DAYS 3 MONS", "3 months 5 days"),
            ("1 dayofmonth", "1 day"),
            ("2147483647 months", "178956970 years 7 months"),
            ("-2147483648 months", "-178956970 years -8 months"),
            ("178956970 years", "178956970 years"),
            ("306783378 weeks", "2147483646 days"),
            ("9223372036854775807 us", "2562047788:00:54.775807"),
            ("9223372036854 seconds", "2562047788:00:54"),
            ("9223372036 ms", "2562:02:52.036"),
            ("1 day1 hour", "1 day 01:00:00"),
            ("1day", "1 day"),
            ("1\tday\n", "1 day"),
            ("1. day", "1 day"),
            ("1 day 2:3", "1 day 02:03:00"),
            ("1 day 2:3:4.123456789", "1 day 02:03:04.123456"),
            ("1 day 2:3:4.12345678 hours", "1 day 02:03:04.123456"),
            ("1 day 2:3:4 5:6:7", "1 day 02:03:04"),
            ("2:3:4 ago", "02:03:04"),
            ("999999999:00:00", "999999999:00:00"),
            ("-999999999:59:59.999999", "-999999999:59:59.999999"),
            ("-1 day -2:00:00", "-1 day -02:00:00"),
        ];
        for (input, expected) in ok {
            assert_eq!(cast(input).as_deref(), Ok(expected), "{input:?}");
        }

        let generic = [
            "P1Y2M",
            "p1d",
            "X",
            "",
            "   ",
            "1 day ago x",
            "1 day ag",
            "ago",
            "-",
            "- 1 day",
            "1 day,",
            "1 day\r",
            ".5 day",
            "1234567890:00:00",
            "12:60:00",
            "1 day --2:00:00",
        ];
        for input in generic {
            assert_eq!(
                cast(input),
                Err(format!("Could not convert string '{input}' to INTERVAL")),
                "{input:?}"
            );
        }

        let other = [
            (
                "42 x",
                ErrorKind::Conversion,
                "extract specifier \"x\" not recognized",
            ),
            (
                "1 day 42",
                ErrorKind::Conversion,
                "extract specifier \"\" not recognized",
            ),
            (
                "1 day 1",
                ErrorKind::Conversion,
                "extract specifier \"\" not recognized",
            ),
            (
                "1e5 days",
                ErrorKind::Conversion,
                "extract specifier \"e\" not recognized",
            ),
            (
                "1 fortnight",
                ErrorKind::Conversion,
                "extract specifier \"fortnight\" not recognized",
            ),
            (
                "1 dow",
                ErrorKind::Conversion,
                "extract specifier \"dow\" not supported for interval",
            ),
            (
                "1 epoch",
                ErrorKind::Conversion,
                "extract specifier \"epoch\" not supported for interval",
            ),
            (
                "1 doy",
                ErrorKind::Conversion,
                "extract specifier \"doy\" not supported for interval",
            ),
            (
                "1 julian",
                ErrorKind::Conversion,
                "extract specifier \"julian\" not supported for interval",
            ),
            (
                "2147483648 months",
                ErrorKind::InvalidInput,
                "Type INT64 with value 2147483648 can't be cast because the value is out of range for the \
                 destination type INT32",
            ),
            (
                "200000000 years",
                ErrorKind::InvalidInput,
                "Type INT64 with value 2400000000 can't be cast because the value is out of range for the \
                 destination type INT32",
            ),
            (
                "178956971 years",
                ErrorKind::InvalidInput,
                "Type INT64 with value 2147483652 can't be cast because the value is out of range for the \
                 destination type INT32",
            ),
            (
                "306783379 weeks",
                ErrorKind::InvalidInput,
                "Type INT64 with value 2147483653 can't be cast because the value is out of range for the \
                 destination type INT32",
            ),
            (
                "99999999999999999999 days",
                ErrorKind::InvalidInput,
                "Could not convert string '99999999999999999999' to INT64",
            ),
            (
                "2147483647 months 1 month",
                ErrorKind::OutOfRange,
                "interval value is out of range",
            ),
            (
                "9223372036854775807 seconds",
                ErrorKind::OutOfRange,
                "interval value is out of range",
            ),
            (
                "9223372036854775807 us 1 us",
                ErrorKind::OutOfRange,
                "interval value is out of range",
            ),
            (
                "1 year 2147483647 days 1 day",
                ErrorKind::OutOfRange,
                "interval value is out of range",
            ),
            (
                "0.999999 months 2147483647 days",
                ErrorKind::OutOfRange,
                "interval value is out of range",
            ),
            (
                "9223372036854.9 seconds",
                ErrorKind::OutOfRange,
                "interval fraction is out of range",
            ),
            (
                "2147483647 days 0.999999 months",
                ErrorKind::OutOfRange,
                "interval fraction is out of range",
            ),
            (
                "9223372036854775807 us 0.5 seconds",
                ErrorKind::OutOfRange,
                "interval fraction is out of range",
            ),
        ];
        for (input, kind, message) in other {
            match Interval::parse_detailed(input.as_bytes()) {
                Err(IntervalParseError::Cast(error)) => {
                    assert_eq!(
                        (error.kind(), error.raw_message().as_ref()),
                        (kind, message),
                        "{input:?}"
                    );
                }
                other => panic!("{input:?}: {other:?}"),
            }
        }

        // Raised by DuckDB even under TRY_CAST.
        assert_eq!(
            Interval::parse_detailed(b"-2147483648 months ago"),
            Err(IntervalParseError::Unconditional(Error::out_of_range(
                "AGO interval value is out of range"
            )))
        );
    }

    #[test]
    fn non_utf8_input() {
        assert_eq!(
            Interval::parse(b"1 \xffday", false),
            Err("Could not convert string '1 \u{fffd}day' to INTERVAL".to_owned())
        );
    }

    /// `to_months`/`to_days`/`to_microseconds(...)::VARCHAR` in DuckDB 1.5.6.
    #[test]
    fn write_to_extremes() {
        let text = |i: Interval| i.to_string();
        assert_eq!(
            text(Interval::new(-2_147_483_647, 0, 0)),
            "-178956970 years -7 months"
        );
        assert_eq!(text(Interval::new(0, -2_147_483_647, 0)), "-2147483647 days");
        assert_eq!(
            text(Interval::new(0, 0, -9_223_372_036_854_775_807)),
            "-2562047788:00:54.775807"
        );
        assert_eq!(text(Interval::new(0, 0, i64::MIN)), "-2562047788:00:54.775808");
        assert_eq!(
            text(Interval::new(0, 0, 2_562_047_788 * Interval::MICROS_PER_HOUR)),
            "2562047788:00:00"
        );
        assert_eq!(text(Interval::default()), "00:00:00");
        let mut out = b"x".to_vec();
        Interval::new(1, 1, 1).write_to(&mut out);
        assert_eq!(out, b"x1 month 1 day 00:00:00.000001");
    }

    /// Interval comparisons in DuckDB 1.5.6.
    #[test]
    fn normalized_comparison_matches_duckdb() {
        assert!(parse("1 month").normalized_eq(parse("30 days")));
        assert_ne!(parse("1 month"), parse("30 days"));
        assert_eq!(parse("1 month").cmp(&parse("30 days")), Ordering::Equal);
        assert!(parse("1 day").normalized_eq(parse("24 hours")));
        assert!(parse("1 month") > parse("29 days"));
        assert!(parse("-1 month") < parse("-29 days"));
        assert!(parse("1 month 1 day") <= parse("31 days"));
        assert!(parse("25 hours") > parse("1 day"));
        // Mixed signs are not reconciled: 1 month -1 day sorts after 29 days.
        assert!(Interval::new(1, -1, 0) > Interval::new(0, 29, 0));
    }

    #[test]
    fn normalize_and_helpers() {
        assert_eq!(
            Interval::new(0, 31, 25 * Interval::MICROS_PER_HOUR).normalize(),
            Interval::new(1, 2, Interval::MICROS_PER_HOUR)
        );
        assert_eq!(
            Interval::new(i32::MAX, 30, 0).normalized_parts(),
            (i64::from(i32::MAX) + 1, 0, 0)
        );
        assert_eq!(
            Interval::new(i32::MAX, 30, 0).normalize(),
            Interval::new(i32::MAX, 30, 0)
        );
        assert_eq!(
            Interval::from_micros(-Interval::MICROS_PER_DAY - 1),
            Interval::new(0, -1, -1)
        );
        assert_eq!(Interval::new(1, 1, 1).invert(), Interval::new(-1, -1, -1));
        assert_eq!(
            Interval::new(1, 1, 1).total_micros(),
            Some(31 * Interval::MICROS_PER_DAY + 1)
        );
        assert_eq!(Interval::new(i32::MAX, 0, 0).total_micros(), None);
    }
}
