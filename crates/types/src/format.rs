//! Text rendering shared by [`Value::to_varchar`](crate::Value::to_varchar) and the
//! vectorised casts to VARCHAR: integers, floating point numbers (fmtlib's `{}`),
//! decimals and the quoting of strings inside nested values.
//!
//! Every writer appends to a `Vec<u8>` so that kernels can reuse one scratch buffer.

use crate::fmt_float::{DigitBuffer as _, ShortDigits, format_fixed, format_shortest};

/// DuckDB's `StringUtil::CharacterIsSpace`: space, `\t`, `\n`, `\v`, `\f` and `\r`.
#[inline]
pub const fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

pub(crate) const DIGIT_PAIRS: &[u8; 200] = b"0001020304050607080910111213141516171819\
2021222324252627282930313233343536373839\
4041424344454647484950515253545556575859\
6061626364656667686970717273747576777879\
8081828384858687888990919293949596979899";

/// Appends the decimal digits of `value`.
pub fn write_u64(value: u64, out: &mut Vec<u8>) {
    let mut buf = [0u8; 21];
    out.extend_from_slice(integer_text(false, value, &mut buf));
}

/// Appends the decimal digits of `value`, with a leading `-` if negative.
pub fn write_i64(value: i64, out: &mut Vec<u8>) {
    let mut buf = [0u8; 21];
    out.extend_from_slice(integer_text(value < 0, value.unsigned_abs(), &mut buf));
}

/// Writes the decimal text of `±magnitude` at the end of `buf` (two digits at a time,
/// like DuckDB's `NumericHelper::FormatUnsigned`) and returns it.
pub fn integer_text(negative: bool, mut magnitude: u64, buf: &mut [u8; 21]) -> &[u8] {
    let mut pos = buf.len();
    while magnitude >= 100 {
        let pair = (magnitude % 100) as usize * 2;
        magnitude /= 100;
        pos -= 2;
        buf[pos..pos + 2].copy_from_slice(&DIGIT_PAIRS[pair..pair + 2]);
    }
    if magnitude >= 10 {
        let pair = magnitude as usize * 2;
        pos -= 2;
        buf[pos..pos + 2].copy_from_slice(&DIGIT_PAIRS[pair..pair + 2]);
    } else {
        pos -= 1;
        buf[pos] = b'0' + magnitude as u8;
    }
    if negative {
        pos -= 1;
        buf[pos] = b'-';
    }
    &buf[pos..]
}

/// Appends the decimal digits of a 128-bit unsigned value.
pub fn write_u128(value: u128, out: &mut Vec<u8>) {
    const CHUNK: u128 = 10_000_000_000_000_000_000;
    if let Ok(small) = u64::try_from(value) {
        write_u64(small, out);
        return;
    }
    write_u128(value / CHUNK, out);
    let start = out.len();
    write_u64((value % CHUNK) as u64, out);
    // Left-pad the low chunk to 19 digits.
    let written = out.len() - start;
    out.splice(start..start, std::iter::repeat_n(b'0', 19 - written));
}

/// Appends the decimal digits of a 128-bit signed value.
pub fn write_i128(value: i128, out: &mut Vec<u8>) {
    if value < 0 {
        out.push(b'-');
    }
    write_u128(value.unsigned_abs(), out);
}

/// Appends an `f64` exactly like `duckdb_fmt::format("{}", v)` (fmt 6.1.2): fmt's shortest
/// digits in its "general" presentation (`1.0`, `0.001`, `1e+16`, `-nan`, `inf`).
pub fn write_f64(value: f64, out: &mut Vec<u8>) {
    write_general(value, false, out);
}

/// Appends an `f32` exactly like `duckdb_fmt::format("{}", v)` (fmt 6.1.2).
pub fn write_f32(value: f32, out: &mut Vec<u8>) {
    write_general(f64::from(value), true, out);
}

/// Writes the sign (fmt uses the sign bit, so `-0.0` and negative NaNs keep it) and
/// returns the magnitude, or `None` after writing `nan`/`inf`.
fn write_sign(value: f64, out: &mut Vec<u8>) -> Option<f64> {
    if value.is_sign_negative() {
        out.push(b'-');
    }
    if value.is_nan() {
        out.extend_from_slice(b"nan");
        return None;
    }
    if value.is_infinite() {
        out.extend_from_slice(b"inf");
        return None;
    }
    Some(value.abs())
}

/// fmt's `{}` presentation: shortest digits, fixed notation for decimal exponents in
/// `[-4, 16)`, otherwise exponent notation; whole numbers keep a `.0`.
fn write_general(value: f64, binary32: bool, out: &mut Vec<u8>) {
    let Some(magnitude) = write_sign(value, out) else {
        return;
    };
    let mut digits = ShortDigits::new();
    let exp = format_shortest(magnitude, binary32, &mut digits);
    let digits = digits.digits();
    let num_digits = digits.len() as i32;
    // The value is `0.d1d2... * 10^full_exp`.
    let full_exp = num_digits + exp;
    if !(-4..16).contains(&(full_exp - 1)) {
        out.push(digits[0]);
        if num_digits > 1 {
            out.push(b'.');
            out.extend_from_slice(&digits[1..]);
        }
        out.push(b'e');
        write_exponent(full_exp - 1, out);
    } else if num_digits <= full_exp {
        out.extend_from_slice(digits);
        out.extend(std::iter::repeat_n(b'0', (full_exp - num_digits) as usize));
        out.extend_from_slice(b".0");
    } else if full_exp > 0 {
        let (integral, fraction) = digits.split_at(full_exp as usize);
        out.extend_from_slice(integral);
        out.push(b'.');
        out.extend_from_slice(fraction);
    } else {
        out.extend_from_slice(b"0.");
        out.extend(std::iter::repeat_n(b'0', (-full_exp) as usize));
        out.extend_from_slice(digits);
    }
}

/// fmt's `write_exponent`: a sign and at least two digits.
fn write_exponent(exp: i32, out: &mut Vec<u8>) {
    out.push(if exp < 0 { b'-' } else { b'+' });
    let magnitude = exp.unsigned_abs();
    if magnitude < 10 {
        out.push(b'0');
    }
    write_u64(u64::from(magnitude), out);
}

/// Appends a double like C's `printf("%f")` as implemented by `duckdb_fmt::sprintf`, used in
/// DuckDB's error messages: six decimals, with Grisu's digits for very large values.
pub fn write_printf_fixed(value: f64, out: &mut Vec<u8>) {
    const PRECISION: i32 = 6;
    let Some(magnitude) = write_sign(value, out) else {
        return;
    };
    let mut digits = Vec::with_capacity(32);
    let exp = format_fixed(magnitude, PRECISION, &mut digits);
    let num_digits = digits.len() as i32;
    let full_exp = num_digits + exp;
    if num_digits <= full_exp {
        out.extend_from_slice(&digits);
        out.extend(std::iter::repeat_n(b'0', (full_exp - num_digits) as usize));
        out.push(b'.');
        out.extend(std::iter::repeat_n(b'0', (PRECISION - full_exp).max(0) as usize));
    } else if full_exp > 0 {
        let (integral, fraction) = digits.split_at(full_exp as usize);
        out.extend_from_slice(integral);
        out.push(b'.');
        out.extend_from_slice(fraction);
        if PRECISION > num_digits {
            out.extend(std::iter::repeat_n(b'0', (PRECISION - num_digits) as usize));
        }
    } else {
        out.push(b'0');
        let mut zeros = -full_exp;
        if num_digits == 0 && PRECISION < zeros {
            zeros = PRECISION;
        }
        if zeros != 0 || num_digits != 0 {
            out.push(b'.');
            out.extend(std::iter::repeat_n(b'0', zeros as usize));
            out.extend_from_slice(&digits);
        }
    }
}

/// Appends an unscaled DECIMAL(`width`, `scale`) value like DuckDB's `DecimalToString`:
/// a value of a DECIMAL whose width equals its scale has no leading `0` (`.5`).
pub fn write_decimal(unscaled: i128, width: u8, scale: u8, out: &mut Vec<u8>) {
    if unscaled < 0 {
        out.push(b'-');
    }
    let magnitude = unscaled.unsigned_abs();
    if scale == 0 {
        write_u128(magnitude, out);
        return;
    }
    let divisor = 10u128.pow(u32::from(scale));
    let (major, minor) = (magnitude / divisor, magnitude % divisor);
    if width > scale {
        write_u128(major, out);
    }
    out.push(b'.');
    let start = out.len();
    write_u128(minor, out);
    let written = out.len() - start;
    out.splice(
        start..start,
        std::iter::repeat_n(b'0', usize::from(scale) - written),
    );
}

/// Characters that force quoting of a string inside a nested value's text
/// (DuckDB's `NestedToVarcharCast::LOOKUP_TABLE`).
const fn needs_quotes_char(byte: u8) -> bool {
    matches!(
        byte,
        b'"' | b'\'' | b'(' | b')' | b',' | b':' | b'=' | b'[' | b']' | b'{' | b'}'
    )
}

/// How a string is embedded in the text of a nested value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NestedQuoting {
    /// Elements of LISTs/MAPs and STRUCT values: quoted only when needed.
    Value,
    /// STRUCT keys: always quoted.
    Key,
}

/// Appends `text` as an element of a nested value's text form, quoting and escaping it
/// like DuckDB's `VectorCastHelpers::WriteEscapedString`.
pub fn write_nested_string(text: &[u8], quoting: NestedQuoting, out: &mut Vec<u8>) {
    let needs_quotes = match text {
        [] => true,
        _ if quoting == NestedQuoting::Key => true,
        [first, ..] if is_space(*first) => true,
        [.., last] if text.len() >= 2 && is_space(*last) => true,
        _ if text.eq_ignore_ascii_case(b"null") => true,
        _ => text.iter().any(|&b| needs_quotes_char(b)),
    };
    if !needs_quotes {
        out.extend_from_slice(text);
        return;
    }
    out.reserve(text.len() + 2);
    out.push(b'\'');
    for &byte in text {
        if byte == b'\\' || byte == b'\'' {
            out.push(b'\\');
        }
        out.push(byte);
    }
    out.push(b'\'');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(f: impl FnOnce(&mut Vec<u8>)) -> String {
        let mut out = Vec::new();
        f(&mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn doubles_format_like_fmt() {
        let cases = [
            (1.0, "1.0"),
            (0.1, "0.1"),
            (-0.0, "-0.0"),
            (0.0, "0.0"),
            (1e20, "1e+20"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (123.456, "123.456"),
            (1.0 / 3.0, "0.3333333333333333"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-300, "1.5e-300"),
            (f64::MAX, "1.7976931348623157e+308"),
            (f64::NAN, "nan"),
            (f64::NEG_INFINITY, "-inf"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                render(|out| write_f64(input, out)),
                expected,
                "formatting {input:e}"
            );
        }
        assert_eq!(render(|out| write_f32(0.1, out)), "0.1");
    }

    #[test]
    fn integers_and_decimals() {
        assert_eq!(render(|out| write_i64(i64::MIN, out)), "-9223372036854775808");
        assert_eq!(render(|out| write_u64(0, out)), "0");
        assert_eq!(render(|out| write_i128(i128::MIN, out)), i128::MIN.to_string());
        assert_eq!(render(|out| write_u128(u128::MAX, out)), u128::MAX.to_string());
        assert_eq!(
            render(|out| write_u128(10u128.pow(19), out)),
            "10000000000000000000"
        );
        assert_eq!(render(|out| write_decimal(15, 2, 1, out)), "1.5");
        assert_eq!(render(|out| write_decimal(-5, 2, 1, out)), "-0.5");
        assert_eq!(render(|out| write_decimal(5, 1, 1, out)), ".5");
        assert_eq!(render(|out| write_decimal(-5, 1, 1, out)), "-.5");
        assert_eq!(render(|out| write_decimal(0, 3, 3, out)), ".000");
        assert_eq!(render(|out| write_decimal(-5, 4, 0, out)), "-5");
        assert_eq!(render(|out| write_decimal(1234, 9, 3, out)), "1.234");
    }

    #[test]
    fn nested_strings_quote_like_duckdb() {
        let value = |s: &str| render(|out| write_nested_string(s.as_bytes(), NestedQuoting::Value, out));
        assert_eq!(value("a b"), "a b");
        assert_eq!(value("x,y"), "'x,y'");
        assert_eq!(value(""), "''");
        assert_eq!(value("NuLL"), "'NuLL'");
        assert_eq!(value(" s"), "' s'");
        assert_eq!(value("a'b"), "'a\\'b'");
        assert_eq!(value("c\\d"), "c\\d");
        let key = render(|out| write_nested_string(b"it's", NestedQuoting::Key, out));
        assert_eq!(key, "'it\\'s'");
    }
}
