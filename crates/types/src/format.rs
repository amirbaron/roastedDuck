//! Text rendering shared by [`Value::to_varchar`](crate::Value::to_varchar) and the
//! vectorised casts to VARCHAR: integers, floating point numbers (fmtlib's `{}`),
//! decimals and the quoting of strings inside nested values.
//!
//! Every writer appends to a `Vec<u8>` so that kernels can reuse one scratch buffer.

use std::io::Write as _;

/// DuckDB's `StringUtil::CharacterIsSpace`: space, `\t`, `\n`, `\v`, `\f` and `\r`.
#[inline]
pub const fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

const DIGIT_PAIRS: &[u8; 200] = b"0001020304050607080910111213141516171819\
2021222324252627282930313233343536373839\
4041424344454647484950515253545556575859\
6061626364656667686970717273747576777879\
8081828384858687888990919293949596979899";

/// Appends the decimal digits of `value`.
pub fn write_u64(mut value: u64, out: &mut Vec<u8>) {
    let mut buf = [0u8; 20];
    let mut pos = buf.len();
    while value >= 100 {
        let pair = (value % 100) as usize * 2;
        value /= 100;
        pos -= 2;
        buf[pos..pos + 2].copy_from_slice(&DIGIT_PAIRS[pair..pair + 2]);
    }
    if value >= 10 {
        let pair = value as usize * 2;
        pos -= 2;
        buf[pos..pos + 2].copy_from_slice(&DIGIT_PAIRS[pair..pair + 2]);
    } else {
        pos -= 1;
        buf[pos] = b'0' + value as u8;
    }
    out.extend_from_slice(&buf[pos..]);
}

/// Appends the decimal digits of `value`, with a leading `-` if negative.
pub fn write_i64(value: i64, out: &mut Vec<u8>) {
    if value < 0 {
        out.push(b'-');
    }
    write_u64(value.unsigned_abs(), out);
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

/// Appends an `f64` exactly like `duckdb_fmt::format("{}", v)` (fmt 6.1.2): the shortest
/// round-trip digits in fmt's "general" presentation.
pub fn write_f64(value: f64, out: &mut Vec<u8>) {
    if value.is_nan() {
        out.extend_from_slice(b"nan");
    } else if value.is_infinite() {
        out.extend_from_slice(if value < 0.0 { b"-inf" } else { b"inf" });
    } else {
        let mut scientific = [0u8; 32];
        let len = scientific_into(&mut scientific, format_args!("{value:e}"));
        prettify_general(&scientific[..len], out);
    }
}

/// Appends an `f32` exactly like `duckdb_fmt::format("{}", v)` (fmt 6.1.2).
pub fn write_f32(value: f32, out: &mut Vec<u8>) {
    if value.is_nan() {
        out.extend_from_slice(b"nan");
    } else if value.is_infinite() {
        out.extend_from_slice(if value < 0.0 { b"-inf" } else { b"inf" });
    } else {
        let mut scientific = [0u8; 32];
        let len = scientific_into(&mut scientific, format_args!("{value:e}"));
        prettify_general(&scientific[..len], out);
    }
}

/// Formats into a stack buffer, returning the number of bytes written.
fn scientific_into(buf: &mut [u8; 32], args: std::fmt::Arguments<'_>) -> usize {
    let capacity = buf.len();
    let mut cursor = &mut buf[..];
    // Rust's shortest `{:e}` output of a finite float is at most 25 bytes.
    let _ = cursor.write_fmt(args);
    capacity - cursor.len()
}

/// Re-renders Rust's shortest round-trip `{:e}` output (`-d.ddde[-]x`) using fmt's
/// `general` presentation with `trailing_zeros` set, as fmt does for an empty format spec.
fn prettify_general(scientific: &[u8], out: &mut Vec<u8>) {
    let (negative, body) = match scientific.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, scientific),
    };
    let e_pos = body.iter().position(|&b| b == b'e').unwrap_or(body.len());
    let (mantissa, exponent) = body.split_at(e_pos);
    let exponent = parse_exponent(exponent.get(1..).unwrap_or_default());
    let mut digits = [0u8; 24];
    let mut num_digits = 0;
    for &b in mantissa.iter().filter(|b| b.is_ascii_digit()) {
        digits[num_digits] = b;
        num_digits += 1;
    }
    let digits = &digits[..num_digits];
    let num_digits = num_digits as i32;

    if negative {
        out.push(b'-');
    }
    if !(-4..16).contains(&exponent) {
        out.push(digits[0]);
        if num_digits > 1 {
            out.push(b'.');
            out.extend_from_slice(&digits[1..]);
        }
        out.push(b'e');
        out.push(if exponent < 0 { b'-' } else { b'+' });
        let magnitude = exponent.unsigned_abs();
        if magnitude < 10 {
            out.push(b'0');
        }
        write_u64(u64::from(magnitude), out);
        return;
    }
    let full_exp = exponent + 1;
    if num_digits <= full_exp {
        out.extend_from_slice(digits);
        out.extend(std::iter::repeat_n(b'0', (full_exp - num_digits) as usize));
        out.extend_from_slice(b".0");
    } else if full_exp > 0 {
        out.extend_from_slice(&digits[..full_exp as usize]);
        out.push(b'.');
        out.extend_from_slice(&digits[full_exp as usize..]);
    } else {
        out.extend_from_slice(b"0.");
        out.extend(std::iter::repeat_n(b'0', (-full_exp) as usize));
        out.extend_from_slice(digits);
    }
}

fn parse_exponent(text: &[u8]) -> i32 {
    let (negative, digits) = match text.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, text),
    };
    let magnitude = digits
        .iter()
        .fold(0i32, |acc, &d| acc * 10 + i32::from(d.wrapping_sub(b'0')));
    if negative { -magnitude } else { magnitude }
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
