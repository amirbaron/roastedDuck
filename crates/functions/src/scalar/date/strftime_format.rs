//! The `strftime`/`strptime` format engine (DuckDB's `strftime_format.cpp`).

use rustdb_common::{Error, Result};
use rustdb_types::{Date, Interval, Time, Timestamp};

use super::calendar::{self, DAY_NAMES, DAY_NAMES_ABBREVIATED, MONTH_NAMES, MONTH_NAMES_ABBREVIATED};
use crate::temporal::timestamp_from_datetime;

/// A format specifier (DuckDB's `StrTimeSpecifier`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spec {
    AbbreviatedWeekdayName,
    FullWeekdayName,
    WeekdayDecimal,
    DayOfMonthPadded,
    DayOfMonth,
    AbbreviatedMonthName,
    FullMonthName,
    MonthDecimalPadded,
    MonthDecimal,
    YearWithoutCenturyPadded,
    YearWithoutCentury,
    YearDecimal,
    Hour24Padded,
    Hour24Decimal,
    Hour12Padded,
    Hour12Decimal,
    AmPm,
    MinutePadded,
    MinuteDecimal,
    SecondPadded,
    SecondDecimal,
    MicrosecondPadded,
    MillisecondPadded,
    UtcOffset,
    TzName,
    DayOfYearPadded,
    DayOfYearDecimal,
    WeekNumberPaddedSunFirst,
    WeekNumberPaddedMonFirst,
    NanosecondPadded,
    YearIso,
    WeekdayIso,
    WeekNumberIso,
}

impl Spec {
    /// Whether the specifier is computed from the date (DuckDB's `IsDateSpecifier`).
    const fn is_date_specifier(self) -> bool {
        matches!(
            self,
            Self::AbbreviatedWeekdayName
                | Self::FullWeekdayName
                | Self::DayOfYearPadded
                | Self::DayOfYearDecimal
                | Self::WeekNumberPaddedMonFirst
                | Self::WeekNumberPaddedSunFirst
                | Self::WeekNumberIso
                | Self::WeekdayDecimal
                | Self::WeekdayIso
                | Self::YearIso
        )
    }

    /// The digits `strptime` reads (DuckDB's `NumericSpecifierWidth`); -1 for others.
    const fn numeric_width(self) -> i32 {
        match self {
            Self::WeekdayDecimal | Self::WeekdayIso => 1,
            Self::DayOfMonthPadded
            | Self::DayOfMonth
            | Self::MonthDecimalPadded
            | Self::MonthDecimal
            | Self::YearWithoutCenturyPadded
            | Self::YearWithoutCentury
            | Self::Hour24Padded
            | Self::Hour24Decimal
            | Self::Hour12Padded
            | Self::Hour12Decimal
            | Self::MinutePadded
            | Self::MinuteDecimal
            | Self::SecondPadded
            | Self::SecondDecimal
            | Self::WeekNumberPaddedSunFirst
            | Self::WeekNumberPaddedMonFirst
            | Self::WeekNumberIso => 2,
            Self::MillisecondPadded | Self::DayOfYearPadded | Self::DayOfYearDecimal => 3,
            Self::YearDecimal | Self::YearIso => 4,
            Self::MicrosecondPadded => 6,
            Self::NanosecondPadded => 9,
            _ => -1,
        }
    }
}

/// A parsed format string (DuckDB's `StrTimeFormat`): literals around specifiers.
#[derive(Clone, Debug, Default)]
pub struct StrTimeFormat {
    pub format_specifier: String,
    specifiers: Vec<Spec>,
    literals: Vec<Vec<u8>>,
}

impl StrTimeFormat {
    /// DuckDB's `StrTimeFormat::ParseFormatSpecifier`; the error is DuckDB's message.
    pub fn parse(format: &str) -> std::result::Result<Self, String> {
        if format.is_empty() {
            return Err("Empty format string".to_owned());
        }
        let bytes = format.as_bytes();
        let mut result = Self {
            format_specifier: format.to_owned(),
            ..Self::default()
        };
        let mut pos = 0;
        let mut current: Vec<u8> = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != b'%' {
                i += 1;
                continue;
            }
            if i + 1 == bytes.len() {
                return Err("Trailing format character %".to_owned());
            }
            if i > pos {
                current.extend_from_slice(&bytes[pos..i]);
            }
            i += 1;
            let mut c = bytes[i];
            if c == b'%' {
                pos = i;
                i += 1;
                continue;
            }
            let spec = if c == b'-' && i + 1 < bytes.len() {
                i += 1;
                c = bytes[i];
                match c {
                    b'd' => Spec::DayOfMonth,
                    b'm' => Spec::MonthDecimal,
                    b'y' => Spec::YearWithoutCentury,
                    b'H' => Spec::Hour24Decimal,
                    b'I' => Spec::Hour12Decimal,
                    b'M' => Spec::MinuteDecimal,
                    b'S' => Spec::SecondDecimal,
                    b'j' => Spec::DayOfYearDecimal,
                    _ => {
                        return Err(format!(
                            "Unrecognized format for strftime/strptime: %-{}",
                            char::from(c)
                        ));
                    }
                }
            } else {
                match c {
                    b'a' => Spec::AbbreviatedWeekdayName,
                    b'A' => Spec::FullWeekdayName,
                    b'w' => Spec::WeekdayDecimal,
                    b'u' => Spec::WeekdayIso,
                    b'd' => Spec::DayOfMonthPadded,
                    b'h' | b'b' => Spec::AbbreviatedMonthName,
                    b'B' => Spec::FullMonthName,
                    b'm' => Spec::MonthDecimalPadded,
                    b'y' => Spec::YearWithoutCenturyPadded,
                    b'Y' => Spec::YearDecimal,
                    b'G' => Spec::YearIso,
                    b'H' => Spec::Hour24Padded,
                    b'I' => Spec::Hour12Padded,
                    b'p' => Spec::AmPm,
                    b'M' => Spec::MinutePadded,
                    b'S' => Spec::SecondPadded,
                    b'n' => Spec::NanosecondPadded,
                    b'f' => Spec::MicrosecondPadded,
                    b'g' => Spec::MillisecondPadded,
                    b'z' => Spec::UtcOffset,
                    b'Z' => Spec::TzName,
                    b'j' => Spec::DayOfYearPadded,
                    b'U' => Spec::WeekNumberPaddedSunFirst,
                    b'W' => Spec::WeekNumberPaddedMonFirst,
                    b'V' => Spec::WeekNumberIso,
                    b'c' | b'x' | b'X' | b'T' => {
                        let sub = match c {
                            b'c' => "%Y-%m-%d %H:%M:%S",
                            b'x' => "%Y-%m-%d",
                            _ => "%H:%M:%S",
                        };
                        let mut locale = Self::parse(sub)?;
                        let mut first = std::mem::take(&mut current);
                        first.extend_from_slice(&locale.literals[0]);
                        locale.literals[0] = first;
                        for (literal, spec) in locale.literals.into_iter().zip(locale.specifiers) {
                            result.literals.push(literal);
                            result.specifiers.push(spec);
                        }
                        i += 1;
                        pos = i;
                        continue;
                    }
                    _ => {
                        return Err(format!(
                            "Unrecognized format for strftime/strptime: %{}",
                            char::from(c)
                        ));
                    }
                }
            };
            result.literals.push(std::mem::take(&mut current));
            result.specifiers.push(spec);
            i += 1;
            pos = i;
        }
        if pos < bytes.len() {
            current.extend_from_slice(&bytes[pos..]);
        }
        result.literals.push(current);
        Ok(result)
    }

    pub fn has_specifier(&self, spec: Spec) -> bool {
        self.specifiers.contains(&spec)
    }

    // -----------------------------------------------------------------------------------
    // strftime
    // -----------------------------------------------------------------------------------

    /// Writes `date` with time parts `data` (year, month, day, hour, minute, second,
    /// nanoseconds, UTC offset) (DuckDB's `FormatStringNS`).
    pub fn format(&self, date: Date, data: &[i32; 8], out: &mut Vec<u8>) {
        self.format_with_tz_name(date, data, None, out);
    }

    /// [`StrTimeFormat::format`] with the text `%Z` writes (DuckDB's `tz_name`).
    pub fn format_with_tz_name(&self, date: Date, data: &[i32; 8], tz_name: Option<&str>, out: &mut Vec<u8>) {
        if !self.specifiers.contains(&Spec::NanosecondPadded) {
            for (literal, spec) in self.literals.iter().zip(&self.specifiers) {
                out.extend_from_slice(literal);
                if spec.is_date_specifier() {
                    write_date_specifier(*spec, date, out);
                } else if *spec == Spec::TzName {
                    out.extend_from_slice(tz_name.unwrap_or_default().as_bytes());
                } else {
                    write_standard_specifier(*spec, data, out);
                }
            }
            if let Some(last) = self.literals.last() {
                out.extend_from_slice(last);
            }
            return;
        }
        // DuckDB's `WritePadded` with an odd width writes 9 digits for `%n` but advances only
        // 6, so what follows overwrites the last 3 and the string ends with whatever was in
        // the (zeroed) buffer. Reproduce that layout.
        let mut segments: Vec<(Vec<u8>, usize)> = Vec::new();
        for (literal, spec) in self.literals.iter().zip(&self.specifiers) {
            segments.push((literal.clone(), literal.len()));
            let mut bytes = Vec::new();
            if spec.is_date_specifier() {
                write_date_specifier(*spec, date, &mut bytes);
            } else if *spec == Spec::TzName {
                bytes.extend_from_slice(tz_name.unwrap_or_default().as_bytes());
            } else {
                write_standard_specifier(*spec, data, &mut bytes);
            }
            let advance = if *spec == Spec::NanosecondPadded {
                6
            } else {
                bytes.len()
            };
            segments.push((bytes, advance));
        }
        if let Some(last) = self.literals.last() {
            segments.push((last.clone(), last.len()));
        }
        let total: usize = segments.iter().map(|(bytes, _)| bytes.len()).sum();
        let start = out.len();
        out.resize(start + total, 0);
        let mut cursor = start;
        for (bytes, advance) in segments {
            out[cursor..cursor + bytes.len()].copy_from_slice(&bytes);
            cursor += advance;
        }
    }

    // -----------------------------------------------------------------------------------
    // strptime
    // -----------------------------------------------------------------------------------

    /// DuckDB's `StrpTimeFormat::Parse`.
    pub fn parse_text(&self, input: &[u8], strict: bool) -> ParseResult {
        let mut result = ParseResult {
            data: [1900, 1, 1, 0, 0, 0, 0, 0],
            ..ParseResult::default()
        };
        match self.parse_into(input, strict, &mut result) {
            Ok(()) => result.ok = true,
            Err(()) => result.ok = false,
        }
        result
    }

    fn parse_into(
        &self,
        input: &[u8],
        strict: bool,
        result: &mut ParseResult,
    ) -> std::result::Result<(), ()> {
        let mut start = 0;
        while input.get(start).copied().is_some_and(is_space) {
            start += 1;
        }
        let data = &input[start..];
        let size = data.len();
        let at = |index: usize| data.get(index).copied().unwrap_or(0);
        let mut pos = 0usize;
        if size > 4 {
            if at(0).is_ascii_alphabetic() {
                if special(data, &mut pos, b"infinity", 3) {
                    result.special = Some(Date::INFINITY);
                } else if special(data, &mut pos, b"epoch", 5) {
                    result.special = Some(Date::EPOCH);
                }
            } else if at(0) == b'-' && special(data, &mut pos, b"-infinity", 4) {
                result.special = Some(Date::NEG_INFINITY);
            }
        }
        if result.special.is_some() {
            while pos < size && is_space(at(pos)) {
                pos += 1;
            }
            if pos != size {
                return result.fail("Special timestamp did not match: trailing characters", pos);
            }
            return Ok(());
        }
        let mut ampm: Option<bool> = None;
        let mut offset_specifier = Spec::WeekdayDecimal;
        let mut weekno: u64 = 0;
        let mut weekday: u64 = 0;
        let mut yearday: u64 = 0;
        let mut has_weekday = false;
        let mut iso_year: u64 = 10_000;
        let mut iso_week: u64 = 54;
        let mut iso_weekday: u64 = 8;
        let mut i = 0;
        loop {
            let literal = &self.literals[i];
            let mut l = 0;
            while l < literal.len() {
                if is_space(literal[l]) {
                    if !is_space(at(pos)) {
                        let message = format!(
                            "Space does not match, expected {}",
                            String::from_utf8_lossy(literal)
                        );
                        result.error_message = message;
                        result.error_position = Some(pos);
                        return Err(());
                    }
                    pos += 1;
                    while pos < size && is_space(at(pos)) {
                        pos += 1;
                    }
                    l += 1;
                    while l < literal.len() && is_space(literal[l]) {
                        l += 1;
                    }
                    continue;
                }
                let matched = at(pos) == literal[l];
                pos += 1;
                l += 1;
                if !matched {
                    result.error_message = format!(
                        "Literal does not match, expected {}",
                        String::from_utf8_lossy(literal)
                    );
                    result.error_position = Some(pos);
                    return Err(());
                }
            }
            if i == self.specifiers.len() {
                break;
            }
            let spec = self.specifiers[i];
            let width = spec.numeric_width();
            if width > 0 {
                let mut number: u64 = 0;
                let mut digits = 0;
                let start_pos = pos;
                let end_pos = start_pos + width as usize;
                while pos < size && pos < end_pos && at(pos).is_ascii_digit() {
                    number = number * 10 + u64::from(at(pos) - b'0');
                    pos += 1;
                    digits += 1;
                }
                if pos == start_pos {
                    return result.fail("Expected a number", start_pos);
                }
                let error_at = start_pos;
                macro_rules! fail {
                    ($message:expr) => {{
                        result.error_message = $message.to_string();
                        result.error_position = Some(error_at);
                        return Err(());
                    }};
                }
                match spec {
                    Spec::DayOfMonthPadded | Spec::DayOfMonth => {
                        if !(1..=31).contains(&number) {
                            fail!("Day out of range, expected a value between 1 and 31");
                        }
                        result.data[2] = number as i32;
                        offset_specifier = spec;
                    }
                    Spec::MonthDecimalPadded | Spec::MonthDecimal => {
                        if !(1..=12).contains(&number) {
                            fail!("Month out of range, expected a value between 1 and 12");
                        }
                        result.data[1] = number as i32;
                        offset_specifier = spec;
                    }
                    Spec::YearWithoutCenturyPadded | Spec::YearWithoutCentury => {
                        if matches!(
                            offset_specifier,
                            Spec::YearIso | Spec::WeekNumberIso | Spec::WeekdayDecimal
                        ) {
                            offset_specifier = spec;
                        }
                        if pos - start_pos < 2 && strict {
                            return Err(());
                        }
                        if number >= 100 {
                            fail!("Year without century out of range, expected a value between 0 and 99");
                        }
                        result.data[0] = if number >= 69 {
                            1900 + number as i32
                        } else {
                            2000 + number as i32
                        };
                    }
                    Spec::YearDecimal => {
                        if matches!(
                            offset_specifier,
                            Spec::YearIso | Spec::WeekNumberIso | Spec::WeekdayDecimal
                        ) {
                            offset_specifier = spec;
                        }
                        if pos - start_pos < 2 && strict {
                            return Err(());
                        }
                        result.data[0] = number as i32;
                    }
                    Spec::YearIso => {
                        match offset_specifier {
                            Spec::DayOfMonthPadded
                            | Spec::DayOfMonth
                            | Spec::MonthDecimalPadded
                            | Spec::MonthDecimal
                            | Spec::YearWithoutCenturyPadded
                            | Spec::YearWithoutCentury
                            | Spec::YearDecimal => {}
                            Spec::WeekdayDecimal => offset_specifier = spec,
                            Spec::YearIso | Spec::WeekNumberIso => {
                                if iso_year <= 9999 {
                                    fail!("Multiple ISO year offsets specified");
                                }
                            }
                            _ => fail!("Incompatible ISO year offset specified"),
                        }
                        if number > 9999 {
                            fail!("ISO Year out of range, expected a value between 0000 and 9999");
                        }
                        iso_year = number;
                    }
                    Spec::Hour24Padded | Spec::Hour24Decimal => {
                        if number >= 24 {
                            fail!("Hour out of range, expected a value between 0 and 23");
                        }
                        result.data[3] = number as i32;
                    }
                    Spec::Hour12Padded | Spec::Hour12Decimal => {
                        if !(1..=12).contains(&number) {
                            fail!("Hour12 out of range, expected a value between 1 and 12");
                        }
                        result.data[3] = number as i32;
                    }
                    Spec::MinutePadded | Spec::MinuteDecimal => {
                        if number >= 60 {
                            fail!("Minutes out of range, expected a value between 0 and 59");
                        }
                        result.data[4] = number as i32;
                    }
                    Spec::SecondPadded | Spec::SecondDecimal => {
                        if number >= 60 {
                            fail!("Seconds out of range, expected a value between 0 and 59");
                        }
                        result.data[5] = number as i32;
                    }
                    Spec::NanosecondPadded | Spec::MicrosecondPadded | Spec::MillisecondPadded => {
                        for _ in digits..width {
                            number *= 10;
                        }
                        result.data[6] = match spec {
                            Spec::NanosecondPadded => number as i32,
                            Spec::MicrosecondPadded => (number * 1_000) as i32,
                            _ => (number * 1_000_000) as i32,
                        };
                    }
                    Spec::WeekNumberPaddedSunFirst | Spec::WeekNumberPaddedMonFirst => {
                        match offset_specifier {
                            Spec::DayOfMonthPadded
                            | Spec::DayOfMonth
                            | Spec::MonthDecimalPadded
                            | Spec::MonthDecimal => {}
                            Spec::YearWithoutCenturyPadded
                            | Spec::YearWithoutCentury
                            | Spec::YearDecimal
                            | Spec::WeekdayDecimal => offset_specifier = spec,
                            _ => fail!("Multiple week offsets specified"),
                        }
                        if number > 53 {
                            fail!("Week out of range, expected a value between 0 and 53");
                        }
                        weekno = number;
                    }
                    Spec::WeekdayDecimal => {
                        if number > 6 {
                            fail!("Weekday out of range, expected a value between 0 and 6");
                        }
                        has_weekday = true;
                        weekday = number;
                    }
                    Spec::WeekNumberIso => {
                        match offset_specifier {
                            Spec::YearWithoutCenturyPadded | Spec::YearWithoutCentury | Spec::YearDecimal => {
                                fail!(
                                    "ISO week offsets are incompatible with non-ISO year specifiers. Use '%G' instead"
                                )
                            }
                            Spec::DayOfMonthPadded
                            | Spec::DayOfMonth
                            | Spec::MonthDecimalPadded
                            | Spec::MonthDecimal => {}
                            Spec::WeekdayDecimal => offset_specifier = spec,
                            Spec::WeekNumberIso | Spec::YearIso => {
                                if iso_week <= 53 {
                                    fail!("Multiple ISO week offsets specified");
                                }
                            }
                            _ => fail!("Incompatible ISO week offset specified"),
                        }
                        if !(1..=53).contains(&number) {
                            fail!("ISO week offset out of range, expected a value between 1 and 53");
                        }
                        iso_week = number;
                    }
                    Spec::WeekdayIso => {
                        if iso_weekday <= 7 {
                            fail!("Multiple ISO weekday offsets specified");
                        }
                        if !(1..=7).contains(&number) {
                            fail!("ISO weekday offset out of range, expected a value between 1 and 7");
                        }
                        iso_weekday = number;
                    }
                    Spec::DayOfYearPadded | Spec::DayOfYearDecimal => {
                        match offset_specifier {
                            Spec::DayOfMonthPadded
                            | Spec::DayOfMonth
                            | Spec::MonthDecimalPadded
                            | Spec::MonthDecimal => {}
                            Spec::YearWithoutCenturyPadded
                            | Spec::YearWithoutCentury
                            | Spec::YearDecimal
                            | Spec::WeekdayDecimal => offset_specifier = spec,
                            _ => fail!("Multiple year offsets specified"),
                        }
                        if !(1..=366).contains(&number) {
                            fail!("Year day out of range, expected a value between 1 and 366");
                        }
                        yearday = number;
                    }
                    _ => {
                        result.not_implemented = true;
                        return Err(());
                    }
                }
            } else {
                match spec {
                    Spec::AmPm => {
                        if pos + 2 > size {
                            return result.fail("Expected AM/PM", pos);
                        }
                        let p = at(pos).to_ascii_lowercase();
                        let m = at(pos + 1).to_ascii_lowercase();
                        if m != b'm' || (p != b'p' && p != b'a') {
                            return result.fail("Expected AM/PM", pos);
                        }
                        ampm = Some(p == b'p');
                        pos += 2;
                    }
                    Spec::AbbreviatedWeekdayName => {
                        if parse_collection(data, &mut pos, &DAY_NAMES_ABBREVIATED).is_none() {
                            return result.fail(
                                "Expected an abbreviated day name (Mon, Tue, Wed, Thu, Fri, Sat, Sun)",
                                pos,
                            );
                        }
                    }
                    Spec::FullWeekdayName => {
                        if parse_collection(data, &mut pos, &DAY_NAMES).is_none() {
                            return result.fail("Expected a full day name (Monday, Tuesday, etc...)", pos);
                        }
                    }
                    Spec::AbbreviatedMonthName => {
                        match parse_collection(data, &mut pos, &MONTH_NAMES_ABBREVIATED) {
                            Some(month) => result.data[1] = month as i32 + 1,
                            None => {
                                return result
                                    .fail("Expected an abbreviated month name (Jan, Feb, Mar, etc..)", pos);
                            }
                        }
                    }
                    Spec::FullMonthName => match parse_collection(data, &mut pos, &MONTH_NAMES) {
                        Some(month) => result.data[1] = month as i32 + 1,
                        None => {
                            return result
                                .fail("Expected a full month name (January, February, etc...)", pos);
                        }
                    },
                    Spec::UtcOffset => match parse_utc_offset(data, &mut pos) {
                        Some((hh, mm, ss)) => result.data[7] = (hh * 60 + mm) * 60 + ss,
                        None => return result.fail("Expected ±HH[MM] or -HH[:MM[:SS]]", pos),
                    },
                    Spec::TzName => {
                        while pos < size && is_space(at(pos)) {
                            pos += 1;
                        }
                        let begin = pos;
                        while pos < size && is_time_zone_char(at(pos)) {
                            pos += 1;
                        }
                        if pos == begin {
                            return result.fail("Empty Time Zone name", begin);
                        }
                        result.tz = String::from_utf8_lossy(&data[begin..pos]).into_owned();
                    }
                    _ => {
                        result.not_implemented = true;
                        return Err(());
                    }
                }
            }
            i += 1;
        }
        while pos < size && is_space(at(pos)) {
            pos += 1;
        }
        if pos != size {
            return result.fail("Full specifier did not match: trailing characters", pos);
        }
        let values = &mut result.data;
        if let Some(pm) = ampm {
            if values[3] > 12 {
                result.error_message = format!(
                    "Invalid hour: {} AM/PM, expected an hour within the range [0..12]",
                    values[3]
                );
                return Err(());
            }
            if pm {
                if values[3] != 12 {
                    values[3] += 12;
                }
            } else if values[3] == 12 {
                values[3] = 0;
            }
        }
        match offset_specifier {
            Spec::YearIso | Spec::WeekNumberIso => {
                let iso_year = if iso_year > 9999 { 1900 } else { iso_year as i32 };
                let iso_week = if iso_week > 53 { 1 } else { iso_week as i64 };
                let iso_weekday = if iso_weekday > 7 { 1 } else { iso_weekday as i64 };
                let jan4 = calendar::days_from_ymd(iso_year, 1, 4);
                let week1 = jan4 - (calendar::iso_day_of_week(jan4) - 1);
                let date = Date((week1 + (iso_week - 1) * 7 + (iso_weekday - 1)) as i32);
                let (y, m, d) = date.to_ymd();
                values[0] = y;
                values[1] = m;
                values[2] = d;
            }
            Spec::WeekNumberPaddedSunFirst | Spec::WeekNumberPaddedMonFirst => {
                if has_weekday {
                    weekday =
                        (weekday + 7 - u64::from(offset_specifier == Spec::WeekNumberPaddedMonFirst)) % 7;
                }
                let jan1 = calendar::days_from_ymd(values[0], 1, 1);
                let mut yeardate = jan1 - (calendar::iso_day_of_week(jan1) - 1);
                yeardate -= i64::from(offset_specifier == Spec::WeekNumberPaddedSunFirst);
                yeardate -= 7 * i64::from(yeardate >= jan1);
                yeardate += (weekno * 7 + weekday) as i64;
                let (y, m, d) = Date(yeardate as i32).to_ymd();
                values[0] = y;
                values[1] = m;
                values[2] = d;
            }
            Spec::DayOfYearPadded | Spec::DayOfYearDecimal => {
                let yeardate = calendar::days_from_ymd(values[0], 1, 1) + yearday as i64 - 1;
                let (y, m, d) = Date(yeardate as i32).to_ymd();
                values[0] = y;
                values[1] = m;
                values[2] = d;
            }
            _ => {}
        }
        Ok(())
    }
}

/// The outcome of `strptime` parsing (DuckDB's `StrpTimeFormat::ParseResult`).
#[derive(Clone, Debug, Default)]
pub struct ParseResult {
    /// year, month, day, hour, minute, second, nanoseconds, UTC offset (seconds).
    pub data: [i32; 8],
    pub tz: String,
    pub error_message: String,
    pub error_position: Option<usize>,
    pub special: Option<Date>,
    pub ok: bool,
    /// An unsupported specifier was used (DuckDB throws).
    pub not_implemented: bool,
}

impl ParseResult {
    fn fail(&mut self, message: &str, position: usize) -> std::result::Result<(), ()> {
        message.clone_into(&mut self.error_message);
        self.error_position = Some(position);
        Err(())
    }

    /// DuckDB's `ParseResult::FormatError`.
    pub fn format_error(&self, input: &[u8], format_specifier: &str) -> String {
        let input = String::from_utf8_lossy(input);
        let position = self
            .error_position
            .map(|position| format!("{input}\n{}^", " ".repeat(position)))
            .unwrap_or_default();
        format!(
            "Could not parse string \"{input}\" according to format specifier \"{format_specifier}\"\n{position}\nError: {}",
            self.error_message
        )
    }

    /// DuckDB's `GetMicros`.
    pub fn micros(&self) -> i32 {
        (self.data[6] + 500) / 1000
    }

    /// The local time minus the UTC offset, unchecked (DuckDB's `ToTime`).
    fn time(&self) -> Time {
        let offset = self.data[7];
        let (hh, mm, ss) = (offset / 3600, (offset % 3600) / 60, offset % 60);
        let micros = ((i64::from(self.data[3] - hh) * 60 + i64::from(self.data[4] - mm)) * 60
            + i64::from(self.data[5] - ss))
            * Interval::MICROS_PER_SEC
            + i64::from(self.micros());
        Time(micros)
    }

    fn time_nanos(&self) -> i64 {
        let offset = self.data[7];
        let (hh, mm, ss) = (offset / 3600, (offset % 3600) / 60, offset % 60);
        ((i64::from(self.data[3] - hh) * 60 + i64::from(self.data[4] - mm)) * 60
            + i64::from(self.data[5] - ss))
            * Interval::NANOS_PER_SEC
            + i64::from(self.data[6])
    }

    fn date(&self) -> Result<Date> {
        crate::temporal::date_from_ymd(self.data[0], self.data[1], self.data[2])
    }

    /// DuckDB's `ToTimestamp`.
    pub fn to_timestamp(&self) -> Result<Timestamp> {
        if let Some(special) = self.special {
            return Ok(if special == Date::INFINITY {
                Timestamp::INFINITY
            } else if special == Date::NEG_INFINITY {
                Timestamp::NEG_INFINITY
            } else {
                timestamp_from_datetime(special, Time(0))?
            });
        }
        let date = self.date()?;
        from_datetime(date, self.time())
            .ok_or_else(|| Error::conversion("Date and time not in timestamp range"))
    }

    /// DuckDB's `TryToTimestamp` (which ignores special values).
    pub fn try_to_timestamp(&self) -> Option<Timestamp> {
        let date = Date::from_ymd(self.data[0], self.data[1], self.data[2])?;
        from_datetime(date, self.time())
    }

    /// DuckDB's `ToTimestampNS`.
    pub(crate) fn to_timestamp_ns(&self) -> Result<i64> {
        if let Some(special) = self.special {
            return Ok(if special == Date::INFINITY {
                Timestamp::INFINITY.0
            } else if special == Date::NEG_INFINITY {
                Timestamp::NEG_INFINITY.0
            } else {
                i64::from(special.0) * Interval::NANOS_PER_SEC * Interval::SECS_PER_DAY
            });
        }
        let date = self.date()?;
        let days = i64::from(date.0)
            .checked_mul(Interval::NANOS_PER_SEC * Interval::SECS_PER_DAY)
            .ok_or_else(|| {
                Error::conversion(format!(
                    "Date out of nanosecond range: {}-{}-{}",
                    self.data[0], self.data[1], self.data[2]
                ))
            })?;
        days.checked_add(self.time_nanos())
            .ok_or_else(|| Error::conversion("Overflow exception in date/time -> timestamp_ns conversion"))
    }

    /// DuckDB's `TryToTimestampNS`.
    pub(crate) fn try_to_timestamp_ns(&self) -> Option<i64> {
        let date = Date::from_ymd(self.data[0], self.data[1], self.data[2])?;
        let value = i64::from(date.0)
            .checked_mul(Interval::NANOS_PER_SEC * Interval::SECS_PER_DAY)?
            .checked_add(self.time_nanos())?;
        Timestamp(value).is_finite().then_some(value)
    }
}

/// `Timestamp::TryFromDatetime`: the timestamp, unless it overflows or is infinite.
fn from_datetime(date: Date, time: Time) -> Option<Timestamp> {
    let value = i64::from(date.0)
        .checked_mul(Interval::MICROS_PER_DAY)?
        .checked_add(time.0)?;
    let timestamp = Timestamp(value);
    timestamp.is_finite().then_some(timestamp)
}

/// `StringUtil::CharacterIsSpace`.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// `Timestamp::CharacterIsTimeZone`.
fn is_time_zone_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'/' | b'+' | b'-' | b':')
}

/// DuckDB's `Date::TryConvertDateSpecial`: the full word or exactly its abbreviation.
fn special(data: &[u8], pos: &mut usize, word: &[u8], abbreviation: usize) -> bool {
    let mut p = *pos;
    let mut k = 0;
    while p < data.len() && k < word.len() {
        if data[p].to_ascii_lowercase() != word[k] {
            return false;
        }
        p += 1;
        k += 1;
    }
    if k < word.len() && p - *pos != abbreviation {
        return false;
    }
    *pos = p;
    true
}

/// DuckDB's `TryParseCollection`: a case-insensitive match of one of the names.
fn parse_collection(data: &[u8], pos: &mut usize, names: &[&str]) -> Option<usize> {
    for (index, name) in names.iter().enumerate() {
        let name = name.as_bytes();
        if *pos + name.len() > data.len() {
            continue;
        }
        if data[*pos..*pos + name.len()].eq_ignore_ascii_case(name) {
            *pos += name.len();
            return Some(index);
        }
    }
    None
}

/// `Timestamp::TryParseUTCOffset`: `±HH[[:]MM[:SS]]`.
fn parse_utc_offset(data: &[u8], pos: &mut usize) -> Option<(i32, i32, i32)> {
    let at = |index: usize| data.get(index).copied().unwrap_or(0);
    let len = data.len();
    let mut cur = *pos;
    if cur + 3 > len {
        return None;
    }
    let sign = at(cur);
    if sign != b'+' && sign != b'-' {
        return None;
    }
    cur += 1;
    if !at(cur).is_ascii_digit() || !at(cur + 1).is_ascii_digit() {
        return None;
    }
    let negate = |value: i32| if sign == b'-' { -value } else { value };
    let hh = negate(i32::from(at(cur) - b'0') * 10 + i32::from(at(cur + 1) - b'0'));
    cur += 2;
    if cur >= len {
        *pos = cur;
        return Some((hh, 0, 0));
    }
    let colons = at(cur) == b':';
    if colons {
        cur += 1;
    }
    if cur + 2 > len || !at(cur).is_ascii_digit() || !at(cur + 1).is_ascii_digit() {
        *pos = cur;
        return Some((hh, 0, 0));
    }
    let mm = negate(i32::from(at(cur) - b'0') * 10 + i32::from(at(cur + 1) - b'0'));
    cur += 2;
    if cur >= len || !colons || at(cur) != b':' {
        *pos = cur;
        return Some((hh, mm, 0));
    }
    cur += 1;
    if cur + 2 > len || !at(cur).is_ascii_digit() || !at(cur + 1).is_ascii_digit() {
        *pos = cur;
        return Some((hh, mm, 0));
    }
    let ss = negate(i32::from(at(cur) - b'0') * 10 + i32::from(at(cur + 1) - b'0'));
    *pos = cur + 2;
    Some((hh, mm, ss))
}

/// `WritePadded`: the last `width` decimal digits of `value`, zero padded.
fn write_padded(out: &mut Vec<u8>, value: u32, width: usize) {
    let modulus = 10u64.pow(width as u32);
    let mut value = u64::from(value) % modulus;
    let start = out.len();
    out.resize(start + width, b'0');
    for slot in out[start..].iter_mut().rev() {
        *slot = b'0' + (value % 10) as u8;
        value /= 10;
    }
}

/// `Write2`: the decimal digits of `value` (one or two for the date parts).
fn write2(out: &mut Vec<u8>, value: u32) {
    if value < 10 {
        out.push(b'0' + value as u8);
    } else if value < 100 {
        out.extend_from_slice(&[b'0' + (value / 10) as u8, b'0' + (value % 10) as u8]);
    } else {
        out.extend_from_slice(value.to_string().as_bytes());
    }
}

/// DuckDB's `ExtractWeekNumberRegular`.
fn week_number_regular(date: Date, monday_first: bool) -> u32 {
    let doy = calendar::day_of_year(date) - 1;
    let (year, _, _) = date.to_ymd();
    let jan1 = calendar::iso_day_of_week(calendar::days_from_ymd(year, 1, 1));
    let first_week_start = if monday_first {
        if jan1 == 1 { 0 } else { 8 - jan1 }
    } else {
        7 - jan1
    };
    if doy < first_week_start {
        0
    } else {
        ((doy - first_week_start) / 7 + 1) as u32
    }
}

fn write_date_specifier(spec: Spec, date: Date, out: &mut Vec<u8>) {
    let days = i64::from(date.0);
    match spec {
        Spec::AbbreviatedWeekdayName => {
            out.extend_from_slice(
                DAY_NAMES_ABBREVIATED[(calendar::iso_day_of_week(days) % 7) as usize].as_bytes(),
            );
        }
        Spec::FullWeekdayName => {
            out.extend_from_slice(DAY_NAMES[(calendar::iso_day_of_week(days) % 7) as usize].as_bytes());
        }
        Spec::WeekdayDecimal => out.push(b'0' + (calendar::iso_day_of_week(days) % 7) as u8),
        Spec::DayOfYearPadded => write_padded(out, calendar::day_of_year(date) as u32, 3),
        Spec::WeekNumberPaddedMonFirst => write_padded(out, week_number_regular(date, true), 2),
        Spec::WeekNumberPaddedSunFirst => write_padded(out, week_number_regular(date, false), 2),
        Spec::WeekNumberIso => write_padded(out, calendar::iso_year_week(date).1 as u32, 2),
        Spec::DayOfYearDecimal => out.extend_from_slice(calendar::day_of_year(date).to_string().as_bytes()),
        Spec::YearIso => write_padded(out, calendar::iso_year_week(date).0 as u32, 4),
        Spec::WeekdayIso => out.push(b'0' + calendar::iso_day_of_week(days) as u8),
        _ => {}
    }
}

fn write_standard_specifier(spec: Spec, data: &[i32; 8], out: &mut Vec<u8>) {
    let hour12 = || {
        let hour = data[3] % 12;
        if hour == 0 { 12 } else { hour as u32 }
    };
    match spec {
        Spec::DayOfMonthPadded => write_padded(out, data[2] as u32, 2),
        Spec::AbbreviatedMonthName => {
            out.extend_from_slice(MONTH_NAMES_ABBREVIATED[(data[1] - 1) as usize].as_bytes());
        }
        Spec::FullMonthName => out.extend_from_slice(MONTH_NAMES[(data[1] - 1) as usize].as_bytes()),
        Spec::MonthDecimalPadded => write_padded(out, data[1] as u32, 2),
        Spec::YearWithoutCenturyPadded => write_padded(out, data[0].unsigned_abs() % 100, 2),
        Spec::YearDecimal => {
            if (0..=9999).contains(&data[0]) {
                write_padded(out, data[0] as u32, 4);
            } else {
                out.extend_from_slice(data[0].to_string().as_bytes());
            }
        }
        Spec::Hour24Padded => write_padded(out, data[3] as u32, 2),
        Spec::Hour12Padded => write_padded(out, hour12(), 2),
        Spec::AmPm => out.extend_from_slice(if data[3] >= 12 { b"PM" } else { b"AM" }),
        Spec::MinutePadded => write_padded(out, data[4] as u32, 2),
        Spec::SecondPadded => write_padded(out, data[5] as u32, 2),
        Spec::NanosecondPadded => write_padded(out, data[6] as u32, 9),
        Spec::MicrosecondPadded => write_padded(out, (data[6] / 1000) as u32, 6),
        Spec::MillisecondPadded => write_padded(out, (data[6] / 1_000_000) as u32, 3),
        Spec::UtcOffset => {
            out.push(if data[7] < 0 { b'-' } else { b'+' });
            let offset = data[7].unsigned_abs();
            let (hh, mm, ss) = (offset / 3600, (offset % 3600) / 60, offset % 60);
            write_padded(out, hh, 2);
            if mm != 0 || ss != 0 {
                out.push(b':');
                write_padded(out, mm, 2);
            }
            if ss != 0 {
                out.push(b':');
                write_padded(out, ss, 2);
            }
        }
        Spec::DayOfMonth => write2(out, (data[2] % 100) as u32),
        Spec::MonthDecimal => write2(out, data[1] as u32),
        Spec::YearWithoutCentury => write2(out, data[0].unsigned_abs() % 100),
        Spec::Hour24Decimal => write2(out, data[3] as u32),
        Spec::Hour12Decimal => write2(out, hour12()),
        Spec::MinuteDecimal => write2(out, data[4] as u32),
        Spec::SecondDecimal => write2(out, data[5] as u32),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_parse() {
        let format = StrTimeFormat::parse("%Y-%m-%d %H:%M:%S.%f %a %B %j %%").unwrap();
        let date = Date(calendar::days_from_ymd(2021, 3, 15) as i32);
        let mut out = Vec::new();
        format.format(date, &[2021, 3, 15, 10, 20, 30, 123_456_000, 0], &mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "2021-03-15 10:20:30.123456 Mon March 074 %"
        );
        let parsed = StrTimeFormat::parse("%d/%m/%Y")
            .unwrap()
            .parse_text(b"15/03/2021", false);
        assert!(parsed.ok);
        assert_eq!(&parsed.data[..3], &[2021, 3, 15]);
        assert_eq!(
            StrTimeFormat::parse("%q").unwrap_err(),
            "Unrecognized format for strftime/strptime: %q"
        );
    }
}
