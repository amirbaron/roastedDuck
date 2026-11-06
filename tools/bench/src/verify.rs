//! Result checking: a port of `InterpretedBenchmark::VerifyInternal` and
//! `Value::ValuesAreEqual`, over engine-neutral [`ResultTable`]s.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::format::ExpectedResult;

/// How values of a column compare against expected strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueKind {
    Boolean,
    /// Any integer type (`TINYINT` ... `UHUGEINT`).
    Integer,
    Float,
    Double,
    Decimal {
        scale: u8,
    },
    Varchar,
    /// `TIMESTAMP`, `TIMESTAMP_S/MS/NS`, `TIMESTAMP WITH TIME ZONE`: a date alone means
    /// midnight, trailing fractional zeros do not matter.
    Timestamp,
    /// `TIME`, `TIME_NS`, `TIME WITH TIME ZONE`.
    Time,
    /// Everything else: compared by rendered text.
    Other,
}

/// A result column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultColumn {
    #[serde(with = "column_name_bytes")]
    pub name: Vec<u8>,
    /// DuckDB's `LogicalType::ToString`, e.g. `DECIMAL(15,2)`.
    pub type_name: String,
    pub kind: ValueKind,
}

// Preserve the existing JSON string representation for valid text and exact bytes otherwise.
mod column_name_bytes {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        match std::str::from_utf8(value) {
            Ok(text) => serializer.serialize_str(text),
            Err(_) => value.serialize(serializer),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Name {
            Text(String),
            Bytes(Vec<u8>),
        }
        Ok(match Name::deserialize(deserializer)? {
            Name::Text(text) => text.into_bytes(),
            Name::Bytes(bytes) => bytes,
        })
    }
}

/// A query result rendered to strings (`Value::ToString`); `None` is SQL `NULL`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResultTable {
    pub columns: Vec<ResultColumn>,
    pub rows: Vec<Vec<Option<String>>>,
}

impl ResultTable {
    /// A short rendering for error messages (first rows only).
    pub fn preview(&self) -> String {
        const MAX_ROWS: usize = 10;
        let mut out = String::new();
        let header: Vec<String> = self
            .columns
            .iter()
            .map(|c| format!("{} {}", String::from_utf8_lossy(&c.name), c.type_name))
            .collect();
        out.push_str(&header.join("\t"));
        for row in self.rows.iter().take(MAX_ROWS) {
            out.push('\n');
            let values: Vec<&str> = row.iter().map(|v| v.as_deref().unwrap_or("NULL")).collect();
            out.push_str(&values.join("\t"));
        }
        if self.rows.len() > MAX_ROWS {
            let _ = write!(out, "\n... ({} rows total)", self.rows.len());
        }
        out
    }

    /// A stable 64-bit fingerprint (FNV-1a) of the column types and all values, used to
    /// cross-check results between engines when no expected result exists.
    pub fn fingerprint(&self) -> u64 {
        let mut hash = Fnv1a::default();
        for column in &self.columns {
            hash.write(column.type_name.as_bytes());
            hash.write(&[0x1f]);
        }
        for row in &self.rows {
            hash.write(&[0x1e]);
            for value in row {
                match value {
                    Some(v) => {
                        hash.write(&[0x01]);
                        hash.write(v.as_bytes());
                    }
                    None => hash.write(&[0x00]),
                }
                hash.write(&[0x1f]);
            }
        }
        hash.0
    }
}

/// 64-bit FNV-1a.
struct Fnv1a(u64);

impl Default for Fnv1a {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv1a {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// Checks `actual` against `expected`; the error describes the first difference.
pub fn verify(expected: &ExpectedResult, actual: &ResultTable) -> Result<(), String> {
    if expected.column_count != actual.columns.len() {
        return Err(format!(
            "Error in result: expected {} columns but got {}\nObtained result:\n{}",
            expected.column_count,
            actual.columns.len(),
            actual.preview()
        ));
    }
    if expected.rows.len() != actual.rows.len() {
        return Err(format!(
            "Error in result: expected {} rows but got {}\nObtained result:\n{}",
            expected.rows.len(),
            actual.rows.len(),
            actual.preview()
        ));
    }
    for (r, (expected_row, actual_row)) in expected.rows.iter().zip(&actual.rows).enumerate() {
        for (c, (expected_value, column)) in expected_row.iter().zip(&actual.columns).enumerate() {
            let actual_value = actual_row.get(c).and_then(Option::as_deref);
            if !value_matches(expected_value, actual_value, column.kind) {
                return Err(format!(
                    "Error in result on row {} column {}: expected value \"{expected_value}\" but got value \
                     \"{}\"\nObtained result:\n{}",
                    r + 1,
                    c + 1,
                    actual_value.unwrap_or("NULL"),
                    actual.preview()
                ));
            }
        }
    }
    Ok(())
}

/// One cell of `VerifyInternal`: textual shortcuts first, then a typed comparison of the
/// expected string cast to the actual value's type.
fn value_matches(expected: &str, actual: Option<&str>, kind: ValueKind) -> bool {
    let Some(actual) = actual else {
        return expected == "NULL" || expected == "(empty)";
    };
    if expected == actual || (expected == "(empty)" && actual.is_empty()) {
        return true;
    }
    match kind {
        ValueKind::Double => match (parse_f64(expected), parse_f64(actual)) {
            (Some(e), Some(a)) => approx_equal_f64(a, e),
            _ => false,
        },
        ValueKind::Float => match (parse_f64(expected), parse_f64(actual)) {
            (Some(e), Some(a)) => approx_equal_f32(a as f32, e as f32),
            _ => false,
        },
        ValueKind::Varchar => sanitize(expected) == sanitize(actual),
        ValueKind::Integer => match (parse_integer(expected), parse_integer(actual)) {
            (Some(e), Some(a)) => e == a,
            _ => false,
        },
        ValueKind::Decimal { scale } => {
            match (parse_decimal(expected, scale), parse_decimal(actual, scale)) {
                (Some(e), Some(a)) => e == a,
                _ => false,
            }
        }
        ValueKind::Boolean => match (parse_bool(expected), parse_bool(actual)) {
            (Some(e), Some(a)) => e == a,
            _ => false,
        },
        ValueKind::Timestamp => {
            normalize_timestamp(expected).is_some_and(|e| Some(e) == normalize_timestamp(actual))
        }
        ValueKind::Time => normalize_time(expected).is_some_and(|e| Some(e) == normalize_time(actual)),
        ValueKind::Other => expected.trim() == actual.trim(),
    }
}

/// `ApproxEqual(double, double)`: 1% relative tolerance.
#[allow(clippy::float_cmp)] // infinities compare exactly, as in DuckDB
fn approx_equal_f64(actual: f64, expected: f64) -> bool {
    if actual.is_nan() && expected.is_nan() {
        return true;
    }
    if !actual.is_finite() || !expected.is_finite() {
        return actual == expected;
    }
    let epsilon = expected.abs() * 0.01 + 0.000_000_01;
    (actual - expected).abs() <= epsilon
}

/// `ApproxEqual(float, float)`.
#[allow(clippy::float_cmp)] // infinities compare exactly, as in DuckDB
fn approx_equal_f32(actual: f32, expected: f32) -> bool {
    if actual.is_nan() && expected.is_nan() {
        return true;
    }
    if !actual.is_finite() || !expected.is_finite() {
        return actual == expected;
    }
    let epsilon = (f64::from(expected.abs()) * 0.01 + 0.000_000_01) as f32;
    (actual - expected).abs() <= epsilon
}

/// `SanitizeValue`: ignore trailing whitespace, escape NUL bytes.
fn sanitize(value: &str) -> String {
    value
        .trim_end_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c'])
        .replace('\0', "\\0")
}

fn parse_f64(s: &str) -> Option<f64> {
    let s = s.trim();
    match s.to_ascii_lowercase().as_str() {
        "inf" | "+inf" | "infinity" | "+infinity" => Some(f64::INFINITY),
        "-inf" | "-infinity" => Some(f64::NEG_INFINITY),
        "nan" | "-nan" => Some(f64::NAN),
        _ => s.parse().ok(),
    }
}

/// VARCHAR -> integer cast: accepts decimal notation and rounds half away from zero.
fn parse_integer(s: &str) -> Option<i128> {
    let s = s.trim();
    if let Ok(v) = s.parse::<i128>() {
        return Some(v);
    }
    let unsigned = s.strip_prefix('+').unwrap_or(s);
    if let Ok(v) = unsigned.parse::<u128>() {
        return i128::try_from(v).ok();
    }
    parse_decimal(s, 0)
}

/// VARCHAR -> `DECIMAL(_, scale)` cast: the value scaled by `10^scale`, rounded half
/// away from zero. Accepts an exponent.
fn parse_decimal(s: &str, scale: u8) -> Option<i128> {
    let s = s.trim();
    let (negative, digits) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let (mantissa, exponent) = match digits.find(['e', 'E']) {
        Some(i) => (&digits[..i], digits[i + 1..].parse::<i32>().ok()?),
        None => (digits, 0),
    };
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part
        .bytes()
        .chain(frac_part.bytes())
        .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    // All digits as one integer, with the decimal point `point` digits from the left.
    let all_digits = format!("{int_part}{frac_part}");
    let point = i64::try_from(int_part.len()).ok()? + i64::from(exponent) + i64::from(scale);
    let point = usize::try_from(point.max(0)).ok()?;
    let (kept, dropped) = if point >= all_digits.len() {
        (
            format!("{all_digits}{}", "0".repeat(point - all_digits.len())),
            "",
        )
    } else {
        (all_digits[..point].to_owned(), &all_digits[point..])
    };
    let mut value: i128 = if kept.is_empty() { 0 } else { kept.parse().ok()? };
    if dropped.as_bytes().first().is_some_and(|&d| d >= b'5') {
        value = value.checked_add(1)?;
    }
    Some(if negative { -value } else { value })
}

/// Canonical `YYYY-MM-DD HH:MM:SS[.fff][+HH:MM]` for a VARCHAR -> TIMESTAMP cast.
fn normalize_timestamp(s: &str) -> Option<String> {
    let s = s.trim();
    let (date, time) = match s.find([' ', 'T']) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, "00:00:00"),
    };
    if date.is_empty() || !date.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return None;
    }
    Some(format!("{date} {}", normalize_time(time)?))
}

/// Canonical `HH:MM:SS[.fff][+HH:MM]` for a VARCHAR -> TIME cast.
fn normalize_time(s: &str) -> Option<String> {
    let s = s.trim();
    let (clock, zone) = match s.find(['+', '-', 'Z']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let (whole, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut parts = whole.split(':');
    let (hours, minutes) = (parts.next()?, parts.next()?);
    let seconds = parts.next().unwrap_or("00");
    if parts.next().is_some() || ![hours, minutes, seconds].iter().all(|p| p.parse::<u32>().is_ok()) {
        return None;
    }
    let fraction = fraction.trim_end_matches('0');
    let mut out = format!("{hours:0>2}:{minutes:0>2}:{seconds:0>2}");
    if !fraction.is_empty() {
        out.push('.');
        out.push_str(fraction);
    }
    out.push_str(&normalize_zone(zone)?);
    Some(out)
}

/// `+HH`, `+HHMM`, `+HH:MM`, `Z` -> `+HH:MM`; no zone -> empty.
fn normalize_zone(zone: &str) -> Option<String> {
    if zone.is_empty() {
        return Some(String::new());
    }
    if zone == "Z" {
        return Some("+00:00".to_owned());
    }
    let (sign, digits) = zone.split_at(1);
    let digits = digits.replace(':', "");
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (hours, minutes) = match digits.len() {
        1 | 2 => (digits.as_str(), "00"),
        3 | 4 => digits.split_at(digits.len() - 2),
        _ => return None,
    };
    Some(format!("{sign}{hours:0>2}:{minutes}"))
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "t" | "1" | "y" | "yes" => Some(true),
        "false" | "f" | "0" | "n" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_name_metadata_roundtrips_native_bytes() {
        for name in [b"plain".as_slice(), "雪".as_bytes(), b"a\0b", b"bad\xff"] {
            let column = ResultColumn {
                name: name.to_vec(),
                type_name: "INTEGER".into(),
                kind: ValueKind::Integer,
            };
            let encoded = serde_json::to_string(&column).unwrap();
            let decoded: ResultColumn = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, column);
            if std::str::from_utf8(name).is_ok() {
                assert!(encoded.contains("\"name\":\""));
            }
        }
    }

    fn table(kinds: &[ValueKind], rows: &[&[Option<&str>]]) -> ResultTable {
        ResultTable {
            columns: kinds
                .iter()
                .enumerate()
                .map(|(i, &kind)| ResultColumn {
                    name: format!("c{i}").into_bytes(),
                    type_name: format!("{kind:?}"),
                    kind,
                })
                .collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|v| v.map(str::to_owned)).collect())
                .collect(),
        }
    }

    fn expected(rows: &[&[&str]]) -> ExpectedResult {
        ExpectedResult {
            query: None,
            column_count: rows.first().map_or(0, |r| r.len()),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|v| (*v).to_owned()).collect())
                .collect(),
        }
    }

    #[test]
    fn compares_like_duckdb() {
        assert!(value_matches("NULL", None, ValueKind::Integer));
        assert!(value_matches("(empty)", Some(""), ValueKind::Varchar));
        assert!(value_matches("(empty)", None, ValueKind::Varchar));
        assert!(!value_matches("1", None, ValueKind::Integer));
        assert!(value_matches("abc", Some("abc  "), ValueKind::Varchar));
        assert!(value_matches("1.5", Some("2"), ValueKind::Integer));
        assert!(value_matches("100", Some("100.5"), ValueKind::Double));
        assert!(!value_matches("100", Some("102"), ValueKind::Double));
        assert!(value_matches("nan", Some("NaN"), ValueKind::Double));
        assert!(value_matches(
            "0.0500000",
            Some("0.05"),
            ValueKind::Decimal { scale: 2 }
        ));
        assert!(value_matches(
            "53758257134.8700",
            Some("53758257134.87"),
            ValueKind::Decimal { scale: 2 }
        ));
        assert!(value_matches(
            "1.005",
            Some("1.01"),
            ValueKind::Decimal { scale: 2 }
        ));
        assert!(value_matches(
            "-1.005",
            Some("-1.01"),
            ValueKind::Decimal { scale: 2 }
        ));
        assert!(!value_matches(
            "1.004",
            Some("1.01"),
            ValueKind::Decimal { scale: 2 }
        ));
        assert!(value_matches(
            "1e2",
            Some("100.0"),
            ValueKind::Decimal { scale: 1 }
        ));
        assert!(value_matches("true", Some("t"), ValueKind::Boolean));
        assert!(!value_matches("1992-01-01", Some("1992-01-02"), ValueKind::Other));
        assert!(value_matches(
            "1992-01-01",
            Some("1992-01-01 00:00:00"),
            ValueKind::Timestamp
        ));
        assert!(value_matches(
            "1992-01-01T10:00:00.500",
            Some("1992-01-01 10:00:00.5"),
            ValueKind::Timestamp
        ));
        assert!(value_matches(
            "2020-01-01 00:00:00Z",
            Some("2020-01-01 00:00:00+00"),
            ValueKind::Timestamp
        ));
        assert!(!value_matches(
            "1992-01-01",
            Some("1992-01-01 00:00:01"),
            ValueKind::Timestamp
        ));
        assert!(value_matches("1:02:03", Some("01:02:03"), ValueKind::Time));
        assert!(value_matches(
            "10:00:00+0530",
            Some("10:00:00+05:30"),
            ValueKind::Time
        ));
        assert!(!value_matches("garbage", Some("garbage "), ValueKind::Time));
    }

    #[test]
    fn decimal_parsing() {
        assert_eq!(parse_decimal("12.345", 2), Some(1235));
        assert_eq!(parse_decimal("-0.5", 0), Some(-1));
        assert_eq!(parse_decimal(".5", 1), Some(5));
        assert_eq!(parse_decimal("1.5e-1", 2), Some(15));
        assert_eq!(parse_decimal("abc", 2), None);
        assert_eq!(parse_decimal("", 2), None);
        assert_eq!(
            parse_integer("170141183460469231731687303715884105727"),
            Some(i128::MAX)
        );
    }

    #[test]
    fn verify_reports_shape_and_value_errors() {
        let kinds = [ValueKind::Integer, ValueKind::Varchar];
        let actual = table(&kinds, &[&[Some("1"), Some("a")], &[Some("2"), None]]);
        assert!(verify(&expected(&[&["1", "a"], &["2", "NULL"]]), &actual).is_ok());
        let err = verify(&expected(&[&["1", "a"]]), &actual).unwrap_err();
        assert!(err.contains("expected 1 rows but got 2"), "{err}");
        let err = verify(&expected(&[&["1"], &["2"]]), &actual).unwrap_err();
        assert!(err.contains("expected 1 columns but got 2"), "{err}");
        let err = verify(&expected(&[&["1", "a"], &["3", "NULL"]]), &actual).unwrap_err();
        assert!(err.contains("row 2 column 1"), "{err}");
    }

    #[test]
    fn fingerprint_distinguishes_null_and_text() {
        let kinds = [ValueKind::Varchar];
        let a = table(&kinds, &[&[None]]);
        let b = table(&kinds, &[&[Some("NULL")]]);
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint(), a.clone().fingerprint());
    }
}
