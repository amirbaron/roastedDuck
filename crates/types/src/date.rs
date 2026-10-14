//! DATE values (DuckDB's `date_t` and `Date`).
//!
//! Besides the calendar arithmetic this module hosts the small byte-level helpers shared by
//! all temporal parsers and formatters (DuckDB's whitespace class, zero-padded number
//! output), so the time, timestamp and interval modules use identical rules.

use std::fmt;

use crate::{NativeType, PhysicalType};

/// Days since 1970-01-01. `i32::MAX` / `-i32::MAX` encode `infinity` / `-infinity`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Date(pub i32);

// SAFETY: a transparent `i32`; every bit pattern is a valid value.
unsafe impl NativeType for Date {
    const PHYSICAL: PhysicalType = PhysicalType::Int32;
}

/// Why a string could not be parsed as a DATE (DuckDB's `DateCastResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DateParseError {
    /// The text is not a date; reported with [`Date::format_error`].
    Format,
    /// The text is a well-formed date outside the supported range (or an impossible day
    /// such as `2021-02-29`); reported with [`Date::range_error`].
    Range,
}

impl DateParseError {
    /// DuckDB's error message for this failure on `input`.
    pub fn message(self, input: &[u8]) -> String {
        match self {
            Self::Format => Date::format_error(input),
            Self::Range => Date::range_error(input),
        }
    }
}

/// A date parsed from the start of a string by [`Date::parse_prefix`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ParsedDate {
    /// The parsed value.
    pub date: Date,
    /// Number of input bytes consumed.
    pub len: usize,
    /// Whether the input was one of the special values `infinity`, `-infinity` or `epoch`
    /// (these always consume the whole input).
    pub special: bool,
}

const DAYS_PER_YEAR_INTERVAL: i32 = 146_097;
const YEAR_INTERVAL: i32 = 400;
const EPOCH_YEAR: i32 = 1970;

const NORMAL_DAYS: [i32; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const LEAP_DAYS: [i32; 13] = [0, 31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const CUMULATIVE_DAYS: [i32; 13] = cumulative(&NORMAL_DAYS);
const CUMULATIVE_LEAP_DAYS: [i32; 13] = cumulative(&LEAP_DAYS);

/// `CUMULATIVE_YEAR_DAYS[i]`: days from 1970-01-01 to January 1st of year `1970 + i`,
/// covering one full 400-year Gregorian cycle.
const CUMULATIVE_YEAR_DAYS: [i32; 401] = {
    let mut table = [0; 401];
    let mut i = 1;
    while i < table.len() {
        table[i] = table[i - 1]
            + if is_leap(EPOCH_YEAR + i as i32 - 1) {
                366
            } else {
                365
            };
        i += 1;
    }
    table
};

/// Month (1-12) of every zero-based day of a common year.
const MONTH_PER_DAY_OF_YEAR: [u8; 365] = month_per_day(&CUMULATIVE_DAYS);
/// Month (1-12) of every zero-based day of a leap year.
const LEAP_MONTH_PER_DAY_OF_YEAR: [u8; 366] = month_per_day(&CUMULATIVE_LEAP_DAYS);

const fn cumulative(days: &[i32; 13]) -> [i32; 13] {
    let mut table = [0; 13];
    let mut month = 1;
    while month < 13 {
        table[month] = table[month - 1] + days[month];
        month += 1;
    }
    table
}

const fn month_per_day<const N: usize>(cumulative: &[i32; 13]) -> [u8; N] {
    let mut table = [0; N];
    let mut month = 1;
    let mut day = 0;
    while day < N {
        while day as i32 >= cumulative[month] {
            month += 1;
        }
        table[day] = month as u8;
        day += 1;
    }
    table
}

const fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

impl Date {
    /// `infinity`.
    pub const INFINITY: Self = Self(i32::MAX);
    /// `-infinity`.
    pub const NEG_INFINITY: Self = Self(-i32::MAX);
    /// 1970-01-01.
    pub const EPOCH: Self = Self(0);
    /// Smallest valid year (`-5877641`, i.e. 5877642 BC).
    pub const MIN_YEAR: i32 = -5_877_641;
    /// Largest valid year.
    pub const MAX_YEAR: i32 = 5_881_580;
    /// The earliest finite date, `5877642-06-25 (BC)`.
    pub const MIN: Self = Self(-i32::MAX + 1);
    /// The latest finite date, `5881580-07-10`.
    pub const MAX: Self = Self(i32::MAX - 1);

    /// Whether the date is neither `infinity` nor `-infinity`.
    pub const fn is_finite(self) -> bool {
        self.0 != Self::INFINITY.0 && self.0 != Self::NEG_INFINITY.0
    }

    /// Whether `year` is a leap year in the proleptic Gregorian calendar.
    pub const fn is_leap_year(year: i32) -> bool {
        is_leap(year)
    }

    /// Number of days in `month` (1-12) of `year`, or `None` for an invalid month.
    pub const fn month_days(year: i32, month: i32) -> Option<i32> {
        if month < 1 || month > 12 {
            return None;
        }
        let table = if is_leap(year) { &LEAP_DAYS } else { &NORMAL_DAYS };
        Some(table[month as usize])
    }

    /// Whether `year-month-day` names a day inside DuckDB's DATE range (`Date::IsValid`).
    pub const fn is_valid(year: i32, month: i32, day: i32) -> bool {
        let Some(month_days) = Self::month_days(year, month) else {
            return false;
        };
        if day < 1 || day > month_days {
            return false;
        }
        if year < Self::MIN_YEAR || (year == Self::MIN_YEAR && (month < 6 || (month == 6 && day < 25))) {
            return false;
        }
        !(year > Self::MAX_YEAR || (year == Self::MAX_YEAR && (month > 7 || (month == 7 && day > 10))))
    }

    /// The date `year-month-day`, or `None` if it does not exist or is out of range
    /// (`Date::TryFromDate`). Year 0 is 1 BC.
    pub fn from_ymd(year: i32, month: i32, day: i32) -> Option<Self> {
        if !Self::is_valid(year, month, day) {
            return None;
        }
        let leading = if is_leap(year) {
            &CUMULATIVE_LEAP_DAYS
        } else {
            &CUMULATIVE_DAYS
        };
        let years = i64::from(year) - i64::from(EPOCH_YEAR);
        let cycles = years.div_euclid(i64::from(YEAR_INTERVAL));
        // In 0..400, so the index is in bounds.
        let year_in_cycle = years.rem_euclid(i64::from(YEAR_INTERVAL)) as usize;
        let days = cycles * i64::from(DAYS_PER_YEAR_INTERVAL)
            + i64::from(CUMULATIVE_YEAR_DAYS[year_in_cycle])
            + i64::from(leading[month as usize - 1])
            + i64::from(day - 1);
        i32::try_from(days).ok().map(Self)
    }

    /// Splits the day number into `(year, offset of the year within its 400-year cycle,
    /// day within the cycle)` (`Date::ExtractYearOffset`).
    fn year_offset(self) -> (i32, usize, i32) {
        let cycles = self.0.div_euclid(DAYS_PER_YEAR_INTERVAL);
        let n = self.0.rem_euclid(DAYS_PER_YEAR_INTERVAL);
        // Assuming 365-day years overestimates the year by at most one or two; step back.
        // `n / 365 <= 400`, so every index stays in bounds.
        let mut offset = (n / 365) as usize;
        while n < CUMULATIVE_YEAR_DAYS[offset] {
            offset -= 1;
        }
        (EPOCH_YEAR + cycles * YEAR_INTERVAL + offset as i32, offset, n)
    }

    /// The `(year, month, day)` of this date (`Date::Convert`). Year 0 is 1 BC.
    ///
    /// Defined for every `i32`, including the infinity encodings.
    pub fn to_ymd(self) -> (i32, i32, i32) {
        let (year, offset, n) = self.year_offset();
        let day_of_year = (n - CUMULATIVE_YEAR_DAYS[offset]) as usize;
        let leap = CUMULATIVE_YEAR_DAYS[offset + 1] - CUMULATIVE_YEAR_DAYS[offset] == 366;
        let (months, cumulative) = if leap {
            (&LEAP_MONTH_PER_DAY_OF_YEAR[..], &CUMULATIVE_LEAP_DAYS)
        } else {
            (&MONTH_PER_DAY_OF_YEAR[..], &CUMULATIVE_DAYS)
        };
        let month = i32::from(months[day_of_year]);
        let day = day_of_year as i32 - cumulative[month as usize - 1] + 1;
        (year, month, day)
    }

    /// The year of this date (`Date::ExtractYear`). Year 0 is 1 BC.
    pub fn year(self) -> i32 {
        self.year_offset().0
    }

    /// Seconds since the epoch (`Date::Epoch`).
    pub const fn epoch_seconds(self) -> i64 {
        self.0 as i64 * 86_400
    }

    /// Milliseconds since the epoch, or `None` on overflow (`Date::EpochMilliseconds`).
    pub const fn epoch_millis(self) -> Option<i64> {
        (self.0 as i64).checked_mul(86_400_000)
    }

    /// Microseconds since the epoch, or `None` on overflow (`Date::EpochMicroseconds`).
    pub const fn epoch_micros(self) -> Option<i64> {
        (self.0 as i64).checked_mul(86_400_000_000)
    }

    /// Nanoseconds since the epoch, or `None` on overflow (`Date::EpochNanoseconds`).
    pub const fn epoch_nanos(self) -> Option<i64> {
        (self.0 as i64).checked_mul(86_400_000_000_000)
    }

    /// Parses a DATE the way `CAST(varchar AS DATE)` does (`Date::TryConvertDate`).
    ///
    /// Accepts `[-]Y..Y<sep>M[M]<sep>D[D][ (BC)]` with `<sep>` one of `-`, `/`, `\` or space,
    /// plus `infinity`/`inf`, `-infinity`/`-inf` and `epoch` (case-insensitive). Leading
    /// whitespace is skipped. In `strict` mode the year needs at least two digits and only
    /// whitespace may follow; otherwise anything may follow except another digit.
    pub fn parse(input: &[u8], strict: bool) -> Result<Self, DateParseError> {
        Self::parse_prefix(input, strict).map(|parsed| parsed.date)
    }

    /// Like [`Date::parse`], but also reports how many bytes were consumed, so that the
    /// TIMESTAMP parser can continue after the date.
    pub fn parse_prefix(input: &[u8], strict: bool) -> Result<ParsedDate, DateParseError> {
        use DateParseError::{Format, Range};

        let mut pos = skip_spaces(input, 0);
        let negative = input.get(pos) == Some(&b'-');
        if negative {
            pos += 1;
        }
        match input.get(pos) {
            None => return Err(Format),
            Some(c) if !c.is_ascii_digit() => return parse_special(input, pos, negative),
            Some(_) => {}
        }

        let year_start = pos;
        let mut year: i32 = 0;
        while let Some(&c) = input.get(pos)
            && c.is_ascii_digit()
        {
            if year >= 100_000_000 {
                return Err(Range);
            }
            year = year * 10 + i32::from(c - b'0');
            pos += 1;
        }
        if strict && pos - year_start < 2 {
            return Err(Format);
        }
        if negative {
            year = -year;
        }

        let sep = *input.get(pos).ok_or(Format)?;
        if !matches!(sep, b' ' | b'-' | b'/' | b'\\') {
            return Err(Format);
        }
        pos += 1;
        let month = parse_one_or_two_digits(input, &mut pos).ok_or(Format)?;
        if input.get(pos) != Some(&sep) {
            return Err(Format);
        }
        pos += 1;
        let day = parse_one_or_two_digits(input, &mut pos).ok_or(Format)?;

        if let Some(&[space, b'(', b, c, b')']) = input.get(pos..pos + 5)
            && is_space(space)
            && b.eq_ignore_ascii_case(&b'b')
            && c.eq_ignore_ascii_case(&b'c')
        {
            if negative || year == 0 {
                return Err(Format);
            }
            year = 1 - year;
            pos += 5;
        }

        if strict {
            if skip_spaces(input, pos) < input.len() {
                return Err(Format);
            }
        } else if input.get(pos).is_some_and(u8::is_ascii_digit) {
            return Err(Format);
        }

        let date = Self::from_ymd(year, month, day).ok_or(Range)?;
        Ok(ParsedDate {
            date,
            len: pos,
            special: false,
        })
    }

    /// DuckDB's message for unparseable DATE text (`Date::FormatError`).
    pub fn format_error(input: &[u8]) -> String {
        format!(
            "invalid date field format: \"{}\", expected format is (YYYY-MM-DD)",
            String::from_utf8_lossy(input)
        )
    }

    /// DuckDB's message for DATE text outside the supported range (`Date::RangeError`).
    pub fn range_error(input: &[u8]) -> String {
        format!(
            "date field value out of range: \"{}\"",
            String::from_utf8_lossy(input)
        )
    }

    /// Appends DuckDB's text form (`YYYY-MM-DD`, years beyond 9999 in full, ` (BC)` for
    /// years before 1 AD, `infinity`/`-infinity`).
    pub fn write_to(self, out: &mut Vec<u8>) {
        if self == Self::INFINITY {
            out.extend_from_slice(b"infinity");
        } else if self == Self::NEG_INFINITY {
            out.extend_from_slice(b"-infinity");
        } else {
            self.write_ymd(out);
        }
    }

    /// Appends the calendar form of the date, ignoring the infinity encodings
    /// (`DateToStringCast::Format`).
    pub(crate) fn write_ymd(self, out: &mut Vec<u8>) {
        let (year, month, day) = self.to_ymd();
        let bc = year <= 0;
        let year = if bc { 1 - i64::from(year) } else { i64::from(year) };
        push_padded(out, year.unsigned_abs(), 4);
        out.push(b'-');
        push_padded(out, month.unsigned_abs().into(), 2);
        out.push(b'-');
        push_padded(out, day.unsigned_abs().into(), 2);
        if bc {
            out.extend_from_slice(b" (BC)");
        }
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = Vec::with_capacity(24);
        self.write_to(&mut buf);
        f.write_str(&String::from_utf8_lossy(&buf))
    }
}

/// Parses `infinity`, `-infinity` or `epoch` at `pos` (after an optional `-`), which must be
/// followed by nothing but whitespace.
fn parse_special(input: &[u8], pos: usize, negative: bool) -> Result<ParsedDate, DateParseError> {
    let (date, end) = if let Some(end) = match_special(input, pos, b"infinity", 3) {
        (
            if negative {
                Date::NEG_INFINITY
            } else {
                Date::INFINITY
            },
            end,
        )
    } else if let Some(end) = match_special(input, pos, b"epoch", 5) {
        (Date::EPOCH, end)
    } else {
        return Err(DateParseError::Format);
    };
    let len = skip_spaces(input, end);
    if len != input.len() {
        return Err(DateParseError::Format);
    }
    Ok(ParsedDate {
        date,
        len,
        special: true,
    })
}

/// Case-insensitively matches `word` at `pos` (`Date::TryConvertDateSpecial`). The input may
/// end early only after exactly `abbreviation` characters (so `inf` is accepted, `infi` is
/// not). Returns the end position of the match.
fn match_special(input: &[u8], pos: usize, word: &[u8], abbreviation: usize) -> Option<usize> {
    let rest = input.get(pos..)?;
    let n = rest.len().min(word.len());
    let matches = rest.iter().zip(word).all(|(c, w)| c.to_ascii_lowercase() == *w);
    (matches && (n == word.len() || n == abbreviation)).then_some(pos + n)
}

// ---------------------------------------------------------------------------------------
// Byte-level helpers shared by the temporal parsers and formatters.
// ---------------------------------------------------------------------------------------

/// DuckDB's whitespace class (`StringUtil::CharacterIsSpace`): space, `\t`, `\n`, `\v`,
/// `\f` and `\r`.
pub(crate) const fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// The position of the first non-whitespace byte at or after `pos` (or `input.len()`).
pub(crate) fn skip_spaces(input: &[u8], pos: usize) -> usize {
    input.get(pos..).map_or(input.len(), |rest| {
        pos + rest.iter().take_while(|&&c| is_space(c)).count()
    })
}

/// Parses one or two digits at `pos`, advancing it (`Date::ParseDoubleDigit`).
pub(crate) fn parse_one_or_two_digits(input: &[u8], pos: &mut usize) -> Option<i32> {
    let first = digit_at(input, *pos)?;
    *pos += 1;
    Some(match digit_at(input, *pos) {
        Some(second) => {
            *pos += 1;
            first * 10 + second
        }
        None => first,
    })
}

/// The value of the ASCII digit at `pos`, if there is one.
pub(crate) fn digit_at(input: &[u8], pos: usize) -> Option<i32> {
    input
        .get(pos)
        .filter(|c| c.is_ascii_digit())
        .map(|&c| i32::from(c - b'0'))
}

/// Appends `value` in decimal, left-padded with zeros to at least `width` digits.
pub(crate) fn push_padded(out: &mut Vec<u8>, mut value: u64, width: usize) {
    // `u64::MAX` has 20 digits; pre-filling with zeros provides the padding.
    let mut buf = [b'0'; 20];
    let mut start = buf.len();
    loop {
        start -= 1;
        buf[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let start = start.min(buf.len().saturating_sub(width));
    out.extend_from_slice(&buf[start..]);
}

/// Appends a fraction of a second: `.` plus `value` zero-padded to `digits` digits with
/// trailing zeros removed; nothing at all if `value` is zero (`TimeToStringCast::FormatMicros`).
pub(crate) fn push_fraction(out: &mut Vec<u8>, mut value: u64, mut digits: usize) {
    if value == 0 {
        return;
    }
    while value.is_multiple_of(10) {
        value /= 10;
        digits = digits.saturating_sub(1);
    }
    out.push(b'.');
    push_padded(out, value, digits);
}

/// Appends `HH:MM:SS[.fraction]` for a non-negative time of day expressed in units of
/// `10^-digits` seconds (`TimeToStringCast::Format`). Hours above 99 are written in full.
pub(crate) fn push_time_of_day(out: &mut Vec<u8>, units: u64, digits: usize) {
    let per_second = 10_u64.pow(digits as u32);
    let seconds = units / per_second;
    push_padded(out, seconds / 3600, 2);
    out.push(b':');
    push_padded(out, seconds / 60 % 60, 2);
    out.push(b':');
    push_padded(out, seconds % 60, 2);
    push_fraction(out, units % per_second, digits);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(date: Date) -> String {
        date.to_string()
    }

    fn cast(input: &str) -> Result<String, String> {
        Date::parse(input.as_bytes(), false)
            .map(text)
            .map_err(|e| e.message(input.as_bytes()))
    }

    #[test]
    fn tables_match_duckdb() {
        assert_eq!(&CUMULATIVE_YEAR_DAYS[..5], &[0, 365, 730, 1096, 1461]);
        assert_eq!(CUMULATIVE_YEAR_DAYS[400], DAYS_PER_YEAR_INTERVAL);
        assert_eq!(CUMULATIVE_DAYS[12], 365);
        assert_eq!(CUMULATIVE_LEAP_DAYS[12], 366);
        assert_eq!(MONTH_PER_DAY_OF_YEAR[30], 1);
        assert_eq!(MONTH_PER_DAY_OF_YEAR[31], 2);
        assert_eq!(MONTH_PER_DAY_OF_YEAR[364], 12);
        assert_eq!(LEAP_MONTH_PER_DAY_OF_YEAR[59], 2);
        assert_eq!(LEAP_MONTH_PER_DAY_OF_YEAR[60], 3);
    }

    #[test]
    fn ymd_round_trips() {
        for days in (-2_147_483_646..=2_147_483_646).step_by(9_973) {
            let date = Date(days);
            let (y, m, d) = date.to_ymd();
            assert_eq!(Date::from_ymd(y, m, d), Some(date), "{days}");
        }
        for days in -800_000..800_000 {
            let date = Date(days);
            let (y, m, d) = date.to_ymd();
            assert_eq!(Date::from_ymd(y, m, d), Some(date), "{days}");
        }
        assert_eq!(Date::from_ymd(1970, 1, 1), Some(Date::EPOCH));
        assert_eq!(Date::from_ymd(5_881_580, 7, 10), Some(Date::MAX));
        assert_eq!(Date::from_ymd(-5_877_641, 6, 25), Some(Date::MIN));
        assert_eq!(Date::from_ymd(5_881_580, 7, 11), None);
        assert_eq!(Date::from_ymd(-5_877_641, 6, 24), None);
        assert_eq!(Date::from_ymd(2021, 2, 29), None);
        assert_eq!(Date::from_ymd(2021, 13, 1), None);
        assert_eq!(Date::from_ymd(2021, 0, 1), None);
        assert_eq!(Date::INFINITY.to_ymd(), (5_881_580, 7, 11));
        assert_eq!(Date::NEG_INFINITY.to_ymd(), (-5_877_641, 6, 24));
        assert_eq!(Date(i32::MIN).to_ymd(), (-5_877_641, 6, 23));
        assert_eq!(Date(-1).year(), 1969);
    }

    #[test]
    fn leap_years() {
        assert!(Date::is_leap_year(2000));
        assert!(Date::is_leap_year(2020));
        assert!(Date::is_leap_year(0));
        assert!(Date::is_leap_year(-4));
        assert!(!Date::is_leap_year(1900));
        assert!(!Date::is_leap_year(2021));
        assert_eq!(Date::month_days(2020, 2), Some(29));
        assert_eq!(Date::month_days(2021, 2), Some(28));
        assert_eq!(Date::month_days(2021, 13), None);
    }

    /// Expected values from DuckDB 1.5.6: `SELECT CAST(<input> AS DATE)::VARCHAR`.
    #[test]
    fn cast_from_varchar_matches_duckdb() {
        let ok = [
            ("2020-02-29", "2020-02-29"),
            ("2000-02-29", "2000-02-29"),
            ("0001-01-01", "0001-01-01"),
            ("0044-03-15 (BC)", "0044-03-15 (BC)"),
            ("0044-03-15 (bc)", "0044-03-15 (BC)"),
            ("0044-03-15 (BC)  ", "0044-03-15 (BC)"),
            ("0001-01-01 (BC)", "0001-01-01 (BC)"),
            ("-0044-03-15", "0045-03-15 (BC)"),
            ("-0001-01-01", "0002-01-01 (BC)"),
            ("0000-01-01", "0001-01-01 (BC)"),
            ("5881580-07-10", "5881580-07-10"),
            ("-5877641-06-25", "5877642-06-25 (BC)"),
            ("5877642-06-25 (BC)", "5877642-06-25 (BC)"),
            ("12345-01-01", "12345-01-01"),
            ("100000-01-01", "100000-01-01"),
            ("1000000-01-01", "1000000-01-01"),
            ("9999-12-31", "9999-12-31"),
            ("1969-12-31", "1969-12-31"),
            ("1582-10-10", "1582-10-10"),
            ("infinity", "infinity"),
            ("-infinity", "-infinity"),
            ("inf", "infinity"),
            ("-inf", "-infinity"),
            ("INFINITY", "infinity"),
            (" infinity ", "infinity"),
            ("epoch", "1970-01-01"),
            ("-epoch", "1970-01-01"),
            ("EPOCH  ", "1970-01-01"),
            ("  2021-01-02  ", "2021-01-02"),
            ("\t2021-01-02\n", "2021-01-02"),
            ("2021-01-02x", "2021-01-02"),
            ("2021-01-02 12:00:00", "2021-01-02"),
            ("2021/01/02", "2021-01-02"),
            ("2021\\01\\02", "2021-01-02"),
            ("2021 01 02", "2021-01-02"),
            ("2021-1-2", "2021-01-02"),
            ("1-01-02", "0001-01-02"),
            ("21-01-02", "0021-01-02"),
            ("2021-01-02 (BC", "2021-01-02"),
            ("2021-01-02(BC)", "2021-01-02"),
        ];
        for (input, expected) in ok {
            assert_eq!(cast(input).as_deref(), Ok(expected), "{input:?}");
        }

        let format = [
            "0000-03-15 (BC)",
            "-0044-03-15 (BC)",
            "infi",
            "inf ",
            "infinityx",
            "epo",
            "2021-01-021",
            "2021-01/02",
            "2021-01-",
            "2021-01",
            "2021-001-01",
            "",
            "   ",
            "-",
            "x",
        ];
        for input in format {
            assert_eq!(
                cast(input),
                Err(format!(
                    "invalid date field format: \"{input}\", expected format is (YYYY-MM-DD)"
                )),
                "{input:?}"
            );
        }

        let range = [
            "2021-02-29",
            "1900-02-29",
            "5881580-07-11",
            "-5877641-06-24",
            "5877642-06-24 (BC)",
            "99999999-01-01",
            "999999999-01-01",
            "1000000000-01-01",
            "2021-13-01",
            "2021-00-01",
            "2021-01-00",
            "2021-01-32",
        ];
        for input in range {
            assert_eq!(
                cast(input),
                Err(format!("date field value out of range: \"{input}\"")),
                "{input:?}"
            );
        }
    }

    #[test]
    fn strict_parsing() {
        assert_eq!(
            Date::parse(b"2021-01-02  ", true),
            Date::from_ymd(2021, 1, 2).ok_or(DateParseError::Format)
        );
        assert_eq!(Date::parse(b"2021-01-02x", true), Err(DateParseError::Format));
        assert_eq!(
            Date::parse(b"2021-01-02 12:00", true),
            Err(DateParseError::Format)
        );
        assert_eq!(Date::parse(b"1-01-02", true), Err(DateParseError::Format));
        assert_eq!(
            Date::parse(b"21-01-02", true),
            Date::from_ymd(21, 1, 2).ok_or(DateParseError::Format)
        );
    }

    #[test]
    fn parse_prefix_reports_consumed_length() {
        let parsed = Date::parse_prefix(b"2021-01-02 12:00:00", false).unwrap();
        assert_eq!(parsed.len, 10);
        assert!(!parsed.special);
        let parsed = Date::parse_prefix(b"0044-03-15 (BC) 12:00", false).unwrap();
        assert_eq!(parsed.len, 15);
        let parsed = Date::parse_prefix(b" infinity ", false).unwrap();
        assert_eq!(
            (parsed.date, parsed.len, parsed.special),
            (Date::INFINITY, 10, true)
        );
    }

    #[test]
    fn non_utf8_input_is_rejected_without_panicking() {
        let input = b"2021-\xff-01";
        assert_eq!(Date::parse(input, false), Err(DateParseError::Format));
        assert_eq!(
            Date::format_error(input),
            "invalid date field format: \"2021-\u{fffd}-01\", expected format is (YYYY-MM-DD)"
        );
    }

    #[test]
    fn write_to_appends() {
        let mut out = b"x=".to_vec();
        Date::from_ymd(-43, 3, 15).unwrap().write_to(&mut out);
        out.push(b' ');
        Date::NEG_INFINITY.write_to(&mut out);
        assert_eq!(out, b"x=0044-03-15 (BC) -infinity");
        assert_eq!(text(Date::MIN), "5877642-06-25 (BC)");
        assert_eq!(text(Date::MAX), "5881580-07-10");
        assert_eq!(text(Date::from_ymd(10_000, 1, 1).unwrap()), "10000-01-01");
    }

    #[test]
    fn epoch_conversions() {
        let date = Date::from_ymd(2021, 1, 2).unwrap();
        assert_eq!(date.epoch_seconds(), 1_609_545_600);
        assert_eq!(date.epoch_millis(), Some(1_609_545_600_000));
        assert_eq!(date.epoch_micros(), Some(1_609_545_600_000_000));
        assert_eq!(date.epoch_nanos(), Some(1_609_545_600_000_000_000));
        assert_eq!(Date::from_ymd(2300, 1, 2).unwrap().epoch_nanos(), None);
        assert_eq!(Date::MAX.epoch_micros(), None);
    }

    #[test]
    fn text_helpers() {
        let mut out = Vec::new();
        push_padded(&mut out, 7, 4);
        push_padded(&mut out, 123_456, 2);
        push_padded(&mut out, u64::MAX, 25);
        assert_eq!(out, b"000712345618446744073709551615");
        out.clear();
        push_fraction(&mut out, 500_000, 6);
        push_fraction(&mut out, 0, 6);
        push_fraction(&mut out, 1, 9);
        assert_eq!(out, b".5.000000001");
        assert_eq!(skip_spaces(b" \t\x0b\x0c\r\nx", 0), 6);
        assert_eq!(skip_spaces(b"ab", 5), 2);
    }
}
