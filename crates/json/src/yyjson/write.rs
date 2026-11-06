//! The yyjson writer with DuckDB's flags (`YYJSON_WRITE_ALLOW_INF_AND_NAN`, optionally
//! `YYJSON_WRITE_PRETTY`): `yyjson_write_single`, `yyjson_write_minify`,
//! `yyjson_write_pretty`, `write_string` (with `enc_table_cpy`: no unicode or slash
//! escaping) and `write_f64_raw` (shortest round-trip digits in ECMAScript layout, but
//! keeping `.0` on integral values and the sign of zero).

use super::{Doc, MutVal, ValId, ValType, tag};

/// Lowercase-free hex digits of `\u00XX` escapes.
const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// Appends `bytes` as a JSON string (with quotes).
pub fn write_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.reserve(bytes.len() + 2);
    out.push(b'"');
    let mut start = 0;
    for (i, &c) in bytes.iter().enumerate() {
        if c >= 0x20 && c != b'"' && c != b'\\' {
            continue;
        }
        out.extend_from_slice(&bytes[start..i]);
        start = i + 1;
        match c {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x09 => out.extend_from_slice(b"\\t"),
            0x0A => out.extend_from_slice(b"\\n"),
            0x0C => out.extend_from_slice(b"\\f"),
            0x0D => out.extend_from_slice(b"\\r"),
            _ => out.extend_from_slice(&[
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX_UPPER[usize::from(c >> 4)],
                HEX_UPPER[usize::from(c & 0xF)],
            ]),
        }
    }
    out.extend_from_slice(&bytes[start..]);
    out.push(b'"');
}

/// Appends a raw value verbatim.
pub fn write_raw(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes);
}

/// Appends the decimal digits of `value`.
pub fn write_u64(out: &mut Vec<u8>, mut value: u64) {
    let mut buf = [0u8; 20];
    let mut pos = buf.len();
    loop {
        pos -= 1;
        buf[pos] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[pos..]);
}

/// Appends a signed integer.
pub fn write_i64(out: &mut Vec<u8>, value: i64) {
    if value < 0 {
        out.push(b'-');
    }
    write_u64(out, value.unsigned_abs());
}

/// The shortest round-trip decimal digits of a finite, non-zero `value` (no sign) and the
/// position of the decimal point relative to the first digit (`f64_bin_to_dec` +
/// `write_u64_len_15_to_17_trim`).
fn shortest_digits(value: f64, digits: &mut [u8; 20]) -> (usize, i32) {
    let mut buffer = zmij::Buffer::new();
    let text = buffer.format_finite(value.abs()).as_bytes();
    let (mantissa, exponent) = match text.iter().position(|&c| c == b'e' || c == b'E') {
        Some(e) => {
            let exp_text = &text[e + 1..];
            let (negative, exp_digits) = match exp_text.first() {
                Some(b'-') => (true, &exp_text[1..]),
                Some(b'+') => (false, &exp_text[1..]),
                _ => (false, exp_text),
            };
            let magnitude = exp_digits
                .iter()
                .fold(0i32, |acc, &d| acc * 10 + i32::from(d - b'0'));
            (&text[..e], if negative { -magnitude } else { magnitude })
        }
        None => (text, 0),
    };
    let int_len = mantissa.iter().position(|&c| c == b'.').unwrap_or(mantissa.len());
    let mut len = 0;
    let mut leading_zeros = 0i32;
    for &c in mantissa {
        if c == b'.' {
            continue;
        }
        if len == 0 && c == b'0' {
            leading_zeros += 1;
            continue;
        }
        if len < digits.len() {
            digits[len] = c;
            len += 1;
        }
    }
    while len > 1 && digits[len - 1] == b'0' {
        len -= 1;
    }
    (len, int_len as i32 + exponent - leading_zeros)
}

/// Appends a double like yyjson's `write_f64_raw` with `ALLOW_INF_AND_NAN`.
pub fn write_f64(out: &mut Vec<u8>, value: f64) {
    if value.is_nan() {
        out.extend_from_slice(b"NaN");
        return;
    }
    if value.is_sign_negative() {
        out.push(b'-');
    }
    if value.is_infinite() {
        out.extend_from_slice(b"Infinity");
        return;
    }
    if value == 0.0 {
        out.extend_from_slice(b"0.0");
        return;
    }
    let mut digits = [0u8; 20];
    let (len, dot_pos) = shortest_digits(value, &mut digits);
    let digits = &digits[..len];
    if -6 < dot_pos && dot_pos <= 21 {
        if dot_pos <= 0 {
            out.extend_from_slice(b"0.");
            out.extend(std::iter::repeat_n(b'0', dot_pos.unsigned_abs() as usize));
            out.extend_from_slice(digits);
        } else {
            let dot = dot_pos as usize;
            if len <= dot {
                out.extend_from_slice(digits);
                out.extend(std::iter::repeat_n(b'0', dot - len));
                out.extend_from_slice(b".0");
            } else {
                out.extend_from_slice(&digits[..dot]);
                out.push(b'.');
                out.extend_from_slice(&digits[dot..]);
            }
        }
    } else {
        out.push(digits[0]);
        if len > 1 {
            out.push(b'.');
            out.extend_from_slice(&digits[1..]);
        }
        out.push(b'e');
        let exp = dot_pos - 1;
        if exp < 0 {
            out.push(b'-');
        }
        write_u64(out, u64::from(exp.unsigned_abs()));
    }
}

/// Appends a number value of `doc` (`write_number`).
fn write_number(out: &mut Vec<u8>, doc: &Doc<'_>, v: ValId) {
    match doc.tag(v) {
        tag::REAL => write_f64(out, doc.get_real(v)),
        tag::SINT => write_i64(out, doc.get_sint(v)),
        _ => write_u64(out, doc.get_uint(v)),
    }
}

/// Appends a non-container value, or an empty container.
fn write_scalar(out: &mut Vec<u8>, doc: &Doc<'_>, v: ValId) {
    match doc.get_type(v) {
        ValType::Raw => write_raw(out, doc.str(v)),
        ValType::Str => write_string(out, doc.str(v)),
        ValType::Num => write_number(out, doc, v),
        ValType::Bool => out.extend_from_slice(if doc.get_bool(v) { b"true" } else { b"false" }),
        ValType::Null | ValType::None => out.extend_from_slice(b"null"),
        ValType::Arr => out.extend_from_slice(b"[]"),
        ValType::Obj => out.extend_from_slice(b"{}"),
    }
}

/// Appends value `v` of `doc` as minified JSON (`yyjson_val_write_opts`).
pub fn write_val(out: &mut Vec<u8>, doc: &Doc<'_>, v: ValId) {
    if !doc.is_ctn(v) || doc.len(v) == 0 {
        write_scalar(out, doc, v);
        return;
    }
    // Pre-order walk of the slots: (remaining children, is object, children so far).
    let mut stack: Vec<(usize, bool, usize)> = Vec::new();
    let mut cur = v;
    let end = doc.next(v);
    while cur < end {
        let is_ctn = doc.is_ctn(cur) && doc.len(cur) > 0;
        if let Some(top) = stack.last_mut() {
            if top.2 > 0 {
                out.push(if top.1 && top.2 % 2 == 1 { b':' } else { b',' });
            }
            top.2 += 1;
        }
        if is_ctn {
            let is_obj = doc.is_obj(cur);
            out.push(if is_obj { b'{' } else { b'[' });
            let children = doc.len(cur) << usize::from(is_obj);
            stack.push((children, is_obj, 0));
            cur += 1;
            continue;
        }
        write_scalar(out, doc, cur);
        cur += 1;
        // Close every container whose children are all written.
        while let Some(&(children, is_obj, written)) = stack.last() {
            if written < children {
                break;
            }
            out.push(if is_obj { b'}' } else { b']' });
            stack.pop();
        }
    }
}

/// Appends `level` indents of four spaces.
fn write_indent(out: &mut Vec<u8>, level: usize) {
    out.extend(std::iter::repeat_n(b' ', level * 4));
}

/// Appends value `v` of `doc` as pretty JSON (`yyjson_write_pretty`, four spaces).
pub fn write_val_pretty(out: &mut Vec<u8>, doc: &Doc<'_>, v: ValId) {
    if !doc.is_ctn(v) || doc.len(v) == 0 {
        write_scalar(out, doc, v);
        return;
    }
    let mut stack: Vec<(usize, bool, usize)> = Vec::new();
    let mut cur = v;
    let end = doc.next(v);
    while cur < end {
        // Whether this slot is the value of a key (written right after `": "`).
        let mut no_indent = false;
        if let Some(top) = stack.last_mut() {
            if top.2 > 0 {
                if top.1 && top.2 % 2 == 1 {
                    out.extend_from_slice(b": ");
                    no_indent = true;
                } else {
                    out.extend_from_slice(b",\n");
                }
            }
            top.2 += 1;
        }
        let level = stack.len();
        let is_ctn = doc.is_ctn(cur) && doc.len(cur) > 0;
        if is_ctn {
            if !no_indent {
                write_indent(out, level);
            }
            let is_obj = doc.is_obj(cur);
            out.extend_from_slice(if is_obj { b"{\n" } else { b"[\n" });
            let children = doc.len(cur) << usize::from(is_obj);
            stack.push((children, is_obj, 0));
            cur += 1;
            continue;
        }
        // yyjson writes raw values without indentation.
        if !no_indent && doc.get_type(cur) != ValType::Raw {
            write_indent(out, level);
        }
        write_scalar(out, doc, cur);
        cur += 1;
        while let Some(&(children, is_obj, written)) = stack.last() {
            if written < children {
                break;
            }
            stack.pop();
            out.push(b'\n');
            write_indent(out, stack.len());
            out.push(if is_obj { b'}' } else { b']' });
        }
    }
}

/// Appends a mutable value as minified JSON (`yyjson_mut_val_write_opts`).
pub(crate) fn write_mut(out: &mut Vec<u8>, val: &MutVal) {
    match val {
        MutVal::Null => out.extend_from_slice(b"null"),
        MutVal::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        MutVal::UInt(u) => write_u64(out, *u),
        MutVal::SInt(i) => write_i64(out, *i),
        MutVal::Real(r) => write_f64(out, *r),
        MutVal::Raw(raw) => write_raw(out, raw),
        MutVal::Str(s) => write_string(out, s),
        MutVal::Arr(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_mut(out, item);
            }
            out.push(b']');
        }
        MutVal::Obj(pairs) => {
            out.push(b'{');
            for (i, (key, value)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_string(out, key);
                out.push(b':');
                write_mut(out, value);
            }
            out.push(b'}');
        }
    }
}

/// Appends a mutable value as pretty JSON (`yyjson_mut_write_pretty`).
pub fn write_mut_pretty(out: &mut Vec<u8>, val: &MutVal) {
    write_mut_pretty_at(out, val, 0, false);
}

fn write_mut_pretty_at(out: &mut Vec<u8>, val: &MutVal, level: usize, no_indent: bool) {
    let indent = |out: &mut Vec<u8>| {
        if !no_indent {
            write_indent(out, level);
        }
    };
    match val {
        MutVal::Arr(items) if !items.is_empty() => {
            indent(out);
            out.extend_from_slice(b"[\n");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.extend_from_slice(b",\n");
                }
                write_mut_pretty_at(out, item, level + 1, false);
            }
            out.push(b'\n');
            write_indent(out, level);
            out.push(b']');
        }
        MutVal::Obj(pairs) if !pairs.is_empty() => {
            indent(out);
            out.extend_from_slice(b"{\n");
            for (i, (key, value)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.extend_from_slice(b",\n");
                }
                write_indent(out, level + 1);
                write_string(out, key);
                out.extend_from_slice(b": ");
                write_mut_pretty_at(out, value, level + 1, true);
            }
            out.push(b'\n');
            write_indent(out, level);
            out.push(b'}');
        }
        MutVal::Raw(raw) => write_raw(out, raw),
        _ => {
            indent(out);
            write_mut(out, val);
        }
    }
}
