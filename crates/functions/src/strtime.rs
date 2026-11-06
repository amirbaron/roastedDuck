//! Public access to DuckDB's `StrpTimeFormat` / `StrfTimeFormat` for readers and writers
//! outside the function library (the CSV reader's `dateformat`, the CSV writer's
//! `timestampformat`).

use rustdb_types::{Date, Timestamp};

use crate::scalar::date::strftime_format::StrTimeFormat;

/// A parsed `strptime`/`strftime` format string (DuckDB's `StrTimeFormat`).
#[derive(Clone, Debug, Default)]
pub struct StrpTimeFormat {
    format: Option<StrTimeFormat>,
}

/// Formats compare by their format string (`StrTimeFormat::operator!=`).
impl PartialEq for StrpTimeFormat {
    fn eq(&self, other: &Self) -> bool {
        self.format_specifier() == other.format_specifier()
    }
}

impl StrpTimeFormat {
    /// `StrTimeFormat::ParseFormatSpecifier`; the error is DuckDB's message.
    pub fn parse(format: &str) -> Result<Self, String> {
        Ok(Self {
            format: Some(StrTimeFormat::parse(format)?),
        })
    }

    /// The format string; empty when no format is set.
    pub fn format_specifier(&self) -> &str {
        self.format
            .as_ref()
            .map_or("", |format| format.format_specifier.as_str())
    }

    /// Whether no format is set (`StrpTimeFormat::Empty`).
    pub fn is_empty(&self) -> bool {
        self.format_specifier().is_empty()
    }

    /// Whether the input parses (`StrpTimeFormat::Parse`); `strict` requires two-digit
    /// fields where the format has them.
    pub fn matches(&self, input: &[u8], strict: bool) -> bool {
        self.format
            .as_ref()
            .is_some_and(|format| format.parse_text(input, strict).ok)
    }

    /// `StrpTimeFormat::TryParseDate`.
    pub fn try_parse_date(&self, input: &[u8]) -> Option<Date> {
        let format = self.format.as_ref()?;
        let result = format.parse_text(input, false);
        if !result.ok {
            return None;
        }
        Date::from_ymd(result.data[0], result.data[1], result.data[2])
    }

    /// `StrpTimeFormat::TryParseTimestamp`.
    pub fn try_parse_timestamp(&self, input: &[u8]) -> Option<Timestamp> {
        let format = self.format.as_ref()?;
        let result = format.parse_text(input, false);
        if !result.ok {
            return None;
        }
        result.try_to_timestamp()
    }

    /// `StrfTimeFormat::ConvertDateVector` for one date.
    pub fn format_date(&self, date: Date, out: &mut Vec<u8>) {
        let Some(format) = &self.format else {
            return;
        };
        if !date.is_finite() {
            out.extend_from_slice(date.to_string().as_bytes());
            return;
        }
        let (year, month, day) = date.to_ymd();
        format.format(date, &[year, month, day, 0, 0, 0, 0, 0], out);
    }

    /// `StrfTimeFormat::ConvertTimestampVector` for one timestamp.
    pub fn format_timestamp(&self, timestamp: Timestamp, out: &mut Vec<u8>) {
        let Some(format) = &self.format else {
            return;
        };
        if !timestamp.is_finite() {
            out.extend_from_slice(timestamp.to_string().as_bytes());
            return;
        }
        let date = timestamp.to_date();
        let micros = timestamp.to_time().0;
        let (year, month, day) = date.to_ymd();
        let hour = micros / rustdb_types::Interval::MICROS_PER_HOUR;
        let minute =
            micros % rustdb_types::Interval::MICROS_PER_HOUR / rustdb_types::Interval::MICROS_PER_MINUTE;
        let second =
            micros % rustdb_types::Interval::MICROS_PER_MINUTE / rustdb_types::Interval::MICROS_PER_SEC;
        let fraction = micros % rustdb_types::Interval::MICROS_PER_SEC;
        let data = [
            year,
            month,
            day,
            hour as i32,
            minute as i32,
            second as i32,
            (fraction * 1000) as i32,
            0,
        ];
        format.format(date, &data, out);
    }
}
