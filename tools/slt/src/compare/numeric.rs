//! Engine-independent numeric value semantics for the Rust backend.
//!
//! The C++ oracle compares numbers with DuckDB itself (`Value::TryCastAs` + `ValuesAreEqual`).
//! The Rust engine has no cast machinery yet, so this module mirrors those semantics: strings are
//! parsed like DuckDB's VARCHAR → numeric casts (whitespace, sign, `_` digit separators,
//! decimal points and exponents rounded half away from zero, `0x`/`0b` literals for integers)
//! and floating point values are compared with DuckDB's `ApproxEqual`.

use rustdb::{LogicalType, LogicalTypeId};

/// A parsed numeric value of a known column type.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Number {
    Null,
    /// Signed integers and decimals (unscaled).
    Signed(i128),
    Unsigned(u128),
    Float(f32),
    Double(f64),
}

/// `ApproxEqual(float, float)` from `src/common/types.cpp`.
#[allow(clippy::float_cmp, reason = "non-finite values compare exactly, like DuckDB")]
pub fn approx_equal_f32(left: f32, right: f32) -> bool {
    if left.is_nan() && right.is_nan() {
        return true;
    }
    if !left.is_finite() || !right.is_finite() {
        return left == right;
    }
    let epsilon = (f64::from(right.abs()) * 0.01 + 0.000_000_01) as f32;
    (left - right).abs() <= epsilon
}

/// `ApproxEqual(double, double)` from `src/common/types.cpp`.
#[allow(clippy::float_cmp, reason = "non-finite values compare exactly, like DuckDB")]
pub fn approx_equal_f64(left: f64, right: f64) -> bool {
    if left.is_nan() && right.is_nan() {
        return true;
    }
    if !left.is_finite() || !right.is_finite() {
        return left == right;
    }
    let epsilon = right.abs() * 0.01 + 0.000_000_01;
    (left - right).abs() <= epsilon
}

/// The numeric branch of `TestResultHelper::CompareValues`: casts both strings to `ty` and
/// compares them like `Value::ValuesAreEqual(actual, expected)`. Unparsable input never matches.
pub fn values_equal(ty: &LogicalType, actual: &str, expected: &str) -> bool {
    let (Some(actual), Some(expected)) = (parse(ty, actual), parse(ty, expected)) else {
        return false;
    };
    match (actual, expected) {
        (Number::Null, Number::Null) => true,
        // ValuesAreEqual(result_value, value) computes ApproxEqual(value, result_value): the
        // tolerance is relative to the actual (result) value.
        (Number::Float(actual), Number::Float(expected)) => approx_equal_f32(expected, actual),
        (Number::Double(actual), Number::Double(expected)) => approx_equal_f64(expected, actual),
        (actual, expected) => actual == expected,
    }
}

/// Parses `text` as a value of the numeric type `ty` (`"NULL"` is the NULL value).
fn parse(ty: &LogicalType, text: &str) -> Option<Number> {
    if text == "NULL" {
        return Some(Number::Null);
    }
    let signed = |min: i128, max: i128| {
        parse_scaled(text, 0, true)
            .and_then(|value| i128::try_from(value).ok())
            .filter(|value| (min..=max).contains(value))
            .map(Number::Signed)
    };
    let unsigned = |max: u128| {
        parse_scaled(text, 0, true)
            .and_then(|value| u128::try_from(value).ok())
            .filter(|value| *value <= max)
            .map(Number::Unsigned)
    };
    match ty.id() {
        LogicalTypeId::TinyInt => signed(i8::MIN.into(), i8::MAX.into()),
        LogicalTypeId::SmallInt => signed(i16::MIN.into(), i16::MAX.into()),
        LogicalTypeId::Integer => signed(i32::MIN.into(), i32::MAX.into()),
        LogicalTypeId::BigInt => signed(i64::MIN.into(), i64::MAX.into()),
        LogicalTypeId::HugeInt => signed(i128::MIN, i128::MAX),
        LogicalTypeId::UTinyInt => unsigned(u8::MAX.into()),
        LogicalTypeId::USmallInt => unsigned(u16::MAX.into()),
        LogicalTypeId::UInteger => unsigned(u32::MAX.into()),
        LogicalTypeId::UBigInt => unsigned(u64::MAX.into()),
        LogicalTypeId::UHugeInt => unsigned(u128::MAX),
        LogicalTypeId::Decimal => {
            let (width, scale) = ty.decimal_properties()?;
            let limit = 10i128.checked_pow(u32::from(width))?;
            parse_scaled(text, u32::from(scale), false)
                .and_then(|value| i128::try_from(value).ok())
                .filter(|value| value.abs() < limit)
                .map(Number::Signed)
        }
        LogicalTypeId::Float => parse_float(text).map(|value| Number::Float(value as f32)),
        LogicalTypeId::Double => parse_float(text).map(Number::Double),
        _ => None,
    }
}

/// A signed magnitude wide enough for every integer type (`u128` plus sign).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scaled {
    negative: bool,
    magnitude: u128,
}

impl TryFrom<Scaled> for i128 {
    type Error = ();

    fn try_from(value: Scaled) -> Result<Self, ()> {
        if value.negative {
            0i128.checked_sub_unsigned(value.magnitude).ok_or(())
        } else {
            i128::try_from(value.magnitude).map_err(|_| ())
        }
    }
}

impl TryFrom<Scaled> for u128 {
    type Error = ();

    fn try_from(value: Scaled) -> Result<Self, ()> {
        if value.negative && value.magnitude != 0 {
            Err(())
        } else {
            Ok(value.magnitude)
        }
    }
}

fn trim_spaces(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_ascii() && crate::text::is_space(c as u8))
}

/// Parses a decimal number and returns it multiplied by `10^scale`, rounded half away from zero.
fn parse_scaled(text: &str, scale: u32, allow_radix_literals: bool) -> Option<Scaled> {
    let text = trim_spaces(text);
    let (negative, body) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    if allow_radix_literals {
        let lower = body.get(..2).map(str::to_ascii_lowercase);
        let radix = match lower.as_deref() {
            Some("0x") => Some(16),
            Some("0b") => Some(2),
            _ => None,
        };
        if let Some(radix) = radix {
            let magnitude = u128::from_str_radix(&body[2..].replace('_', ""), radix).ok()?;
            return Some(Scaled { negative, magnitude });
        }
    }
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(position) => (&body[..position], parse_exponent(&body[position + 1..])?),
        None => (body, 0),
    };
    let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let integer = digits_without_separators(integer)?;
    let fraction = digits_without_separators(fraction)?;
    if integer.is_empty() && fraction.is_empty() {
        return None;
    }
    let digits: Vec<u8> = integer
        .bytes()
        .chain(fraction.bytes())
        .map(|b| b - b'0')
        .collect();
    // The value is 0.d1d2d3... * 10^point; keep `point + scale` digits before rounding.
    let point = i64::try_from(integer.len()).ok()? + i64::from(exponent);
    let keep = point + i64::from(scale);
    let mut magnitude: u128 = 0;
    for position in 0..keep.max(0) {
        let digit = usize::try_from(position)
            .ok()
            .and_then(|index| digits.get(index))
            .copied()
            .unwrap_or(0);
        magnitude = magnitude.checked_mul(10)?.checked_add(u128::from(digit))?;
    }
    let round_digit = usize::try_from(keep)
        .ok()
        .and_then(|index| digits.get(index))
        .copied();
    if round_digit.is_some_and(|digit| digit >= 5) {
        magnitude = magnitude.checked_add(1)?;
    }
    Some(Scaled { negative, magnitude })
}

/// Validates a digit run with optional single `_` separators between digits and removes them.
fn digits_without_separators(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut digits = String::with_capacity(text.len());
    for (index, &byte) in bytes.iter().enumerate() {
        if byte.is_ascii_digit() {
            digits.push(char::from(byte));
        } else if byte == b'_' {
            let between_digits = index > 0
                && bytes[index - 1].is_ascii_digit()
                && bytes.get(index + 1).is_some_and(u8::is_ascii_digit);
            if !between_digits {
                return None;
            }
        } else {
            return None;
        }
    }
    Some(digits)
}

fn parse_exponent(text: &str) -> Option<i32> {
    let (negative, digits) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: i32 = digits.parse().ok()?;
    Some(if negative { -value } else { value })
}

/// VARCHAR → DOUBLE: surrounding whitespace allowed, `inf`/`infinity`/`nan` in any case.
fn parse_float(text: &str) -> Option<f64> {
    trim_spaces(text).parse::<f64>().ok()
}

/// `CAST(<floating point> AS BIGINT)`: rounds half to even and fails outside the `BIGINT` range.
pub fn float_to_bigint(value: f64) -> Result<i64, String> {
    if value.is_finite() && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&value) {
        Ok(value.round_ties_even() as i64)
    } else {
        Err(format!(
            "Conversion Error: Type DOUBLE with value {value} can't be cast because the value is out of \
             range for the destination type INT64"
        ))
    }
}

/// `CAST(<decimal> AS BIGINT)`: rounds half away from zero and fails outside the `BIGINT` range.
pub fn decimal_to_bigint(unscaled: i128, scale: u8) -> Result<i64, String> {
    let divisor = 10i128.pow(u32::from(scale));
    let half = divisor / 2;
    let rounded = if scale == 0 {
        unscaled
    } else if unscaled < 0 {
        (unscaled - half) / divisor
    } else {
        (unscaled + half) / divisor
    };
    i64::try_from(rounded).map_err(|_| {
        format!(
            "Conversion Error: Type DECIMAL with value {unscaled}e-{scale} can't be cast because the value is \
             out of range for the destination type INT64"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approx_equal_matches_duckdb() {
        assert!(approx_equal_f64(100.0, 100.9));
        assert!(!approx_equal_f64(100.0, 98.0));
        assert!(approx_equal_f64(f64::NAN, f64::NAN));
        assert!(approx_equal_f64(f64::INFINITY, f64::INFINITY));
        assert!(!approx_equal_f64(f64::INFINITY, 1e308));
        assert!(approx_equal_f64(0.0, 0.000_000_001));
        assert!(approx_equal_f32(1.0, 1.005));
    }

    #[test]
    fn integers_parse_like_duckdb_casts() {
        let int = LogicalType::INTEGER;
        assert!(values_equal(&int, "42", " 42 "));
        assert!(values_equal(&int, "42", "+42"));
        assert!(values_equal(&int, "1000", "1_000"));
        assert!(values_equal(&int, "2", "1.5"));
        assert!(values_equal(&int, "-2", "-1.5"));
        assert!(values_equal(&int, "1", "1.4"));
        assert!(values_equal(&int, "1200", "1.2e3"));
        assert!(values_equal(&int, "255", "0xFF"));
        assert!(!values_equal(&int, "1", "abc"));
        assert!(!values_equal(&int, "1", "1__0"));
        assert!(!values_equal(&int, "1", "2147483648"));
        assert!(values_equal(&int, "NULL", "NULL"));
        assert!(!values_equal(&int, "NULL", "0"));
        assert!(!values_equal(&LogicalType::UTINYINT, "0", "-1"));
        assert!(values_equal(
            &LogicalType::UHUGEINT,
            "340282366920938463463374607431768211455",
            "340282366920938463463374607431768211455"
        ));
    }

    #[test]
    fn decimals_round_to_scale() {
        let ty = LogicalType::decimal(4, 2);
        assert!(values_equal(&ty, "1.24", "1.235"));
        assert!(values_equal(&ty, "1.20", "1.2"));
        assert!(!values_equal(&ty, "1.24", "100.0"));
    }

    #[test]
    fn floats_compare_approximately() {
        assert!(values_equal(&LogicalType::DOUBLE, "0.3333333333", "0.333"));
        assert!(values_equal(&LogicalType::DOUBLE, "inf", "Infinity"));
        assert!(values_equal(&LogicalType::DOUBLE, "nan", "NaN"));
        assert!(!values_equal(&LogicalType::DOUBLE, "1.0", "1.1"));
        assert!(values_equal(&LogicalType::FLOAT, "3.14159", "3.14"));
    }

    #[test]
    fn bigint_casts() {
        assert_eq!(float_to_bigint(2.5), Ok(2));
        assert_eq!(float_to_bigint(3.5), Ok(4));
        assert_eq!(float_to_bigint(-0.5), Ok(0));
        assert!(float_to_bigint(1e300).is_err());
        assert!(float_to_bigint(f64::NAN).is_err());
        assert_eq!(decimal_to_bigint(125, 2), Ok(1));
        assert_eq!(decimal_to_bigint(150, 2), Ok(2));
        assert_eq!(decimal_to_bigint(-150, 2), Ok(-2));
    }
}
