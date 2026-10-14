//! Text output identical to yyjson's writer with DuckDB's flags
//! (`YYJSON_WRITE_ALLOW_INF_AND_NAN`, optionally `YYJSON_WRITE_PRETTY`).

use std::fmt::Write as _;

use super::JsonValue;

/// Renders `value` compactly (`{"a":1}`) or pretty-printed with four-space indentation.
pub fn write_json(value: &JsonValue, pretty: bool) -> String {
    let mut out = String::new();
    write_value(&mut out, value, pretty, 0);
    out
}

fn write_value(out: &mut String, value: &JsonValue, pretty: bool, depth: usize) {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(true) => out.push_str("true"),
        JsonValue::Bool(false) => out.push_str("false"),
        JsonValue::UInt(v) => {
            let _ = write!(out, "{v}");
        }
        JsonValue::SInt(v) => {
            let _ = write!(out, "{v}");
        }
        JsonValue::Real(v) => write_f64(out, *v),
        JsonValue::Raw(raw) => out.push_str(raw),
        JsonValue::Str(s) => write_string(out, s),
        JsonValue::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline_indent(out, pretty, depth + 1);
                write_value(out, item, pretty, depth + 1);
            }
            newline_indent(out, pretty, depth);
            out.push(']');
        }
        JsonValue::Object(entries) => {
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (key, item)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline_indent(out, pretty, depth + 1);
                write_string(out, key);
                out.push(':');
                if pretty {
                    out.push(' ');
                }
                write_value(out, item, pretty, depth + 1);
            }
            newline_indent(out, pretty, depth);
            out.push('}');
        }
    }
}

fn newline_indent(out: &mut String, pretty: bool, depth: usize) {
    if pretty {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', depth * 4));
    }
}

/// Escapes like yyjson without `ESCAPE_UNICODE`/`ESCAPE_SLASHES`: `"`, `\` and control
/// characters are escaped (`\b \t \n \f \r`, otherwise `\u00XX`); everything else is copied.
fn write_string(out: &mut String, s: &str) {
    out.push('"');
    let mut start = 0;
    for (i, byte) in s.bytes().enumerate() {
        let escape = match byte {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            0x08 => "\\b",
            b'\t' => "\\t",
            b'\n' => "\\n",
            0x0C => "\\f",
            b'\r' => "\\r",
            0..0x20 => "",
            _ => continue,
        };
        out.push_str(&s[start..i]);
        if escape.is_empty() {
            let _ = write!(out, "\\u{byte:04X}");
        } else {
            out.push_str(escape);
        }
        start = i + 1;
    }
    out.push_str(&s[start..]);
    out.push('"');
}

/// Formats a double like yyjson: the shortest round-trip digits, a mandatory decimal
/// point for integral values (`1.0`), plain notation for decimal exponents in `-7..21`
/// and `1.5e300` / `1e-7` style exponents (no `+`) otherwise.
pub fn write_f64(out: &mut String, value: f64) {
    if value.is_nan() {
        out.push_str("NaN");
        return;
    }
    if value.is_infinite() {
        out.push_str(if value < 0.0 { "-Infinity" } else { "Infinity" });
        return;
    }
    if value.is_sign_negative() {
        out.push('-');
    }
    if value == 0.0 {
        out.push_str("0.0");
        return;
    }
    // Rust's `{:e}` yields the shortest round-trip digits as `d.ddde<exp>`.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let dot_pos = exponent + 1;
    let num_digits = digits.len() as i32;

    if -6 < dot_pos && dot_pos <= 21 {
        if dot_pos <= 0 {
            out.push_str("0.");
            out.extend(std::iter::repeat_n('0', (-dot_pos) as usize));
            out.push_str(&digits);
        } else if num_digits <= dot_pos {
            out.push_str(&digits);
            out.extend(std::iter::repeat_n('0', (dot_pos - num_digits) as usize));
            out.push_str(".0");
        } else {
            let (int_part, frac_part) = digits.split_at(dot_pos as usize);
            out.push_str(int_part);
            out.push('.');
            out.push_str(frac_part);
        }
    } else {
        out.push_str(&digits[..1]);
        if num_digits > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{exponent}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: f64) -> String {
        let mut out = String::new();
        write_f64(&mut out, v);
        out
    }

    #[test]
    fn doubles_match_yyjson() {
        assert_eq!(f(1.0), "1.0");
        assert_eq!(f(1.5), "1.5");
        assert_eq!(f(-0.0), "-0.0");
        assert_eq!(f(0.1), "0.1");
        assert_eq!(f(1e300), "1e300");
        assert_eq!(f(1.5e300), "1.5e300");
        assert_eq!(f(1e20), "100000000000000000000.0");
        assert_eq!(f(1e21), "1e21");
        assert_eq!(f(1e-6), "0.000001");
        assert_eq!(f(1e-7), "1e-7");
        assert_eq!(f(123.456), "123.456");
        assert_eq!(f(5e-324), "5e-324");
        assert_eq!(f(f64::NEG_INFINITY), "-Infinity");
        assert_eq!(f(f64::from(0.1f32)), "0.10000000149011612");
    }

    #[test]
    fn strings_escape_like_yyjson() {
        let mut out = String::new();
        write_string(&mut out, "a\"b\\c\n\u{1}\u{7f}é/");
        assert_eq!(out, "\"a\\\"b\\\\c\\n\\u0001\u{7f}é/\"");
    }

    #[test]
    fn pretty_layout() {
        let value = JsonValue::Object(vec![
            (
                "a".into(),
                JsonValue::Array(vec![JsonValue::UInt(1), JsonValue::SInt(-2)]),
            ),
            ("b".into(), JsonValue::Array(vec![])),
            ("c".into(), JsonValue::Object(vec![])),
        ]);
        assert_eq!(write_json(&value, false), r#"{"a":[1,-2],"b":[],"c":{}}"#);
        assert_eq!(
            write_json(&value, true),
            "{\n    \"a\": [\n        1,\n        -2\n    ],\n    \"b\": [],\n    \"c\": {}\n}"
        );
    }
}
