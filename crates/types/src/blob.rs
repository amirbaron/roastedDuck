//! BLOB values: text escaping and base64 (DuckDB's `Blob`).
//!
//! The text form keeps printable ASCII except `\`, `'` and `"`, and writes every other byte
//! as `\xAA` (upper-case hex). Parsing accepts ASCII text with `\xAA` escapes (either case).
//! Error messages are DuckDB's, byte for byte, except that input that is not valid UTF-8
//! is rendered lossily (DuckDB embeds the raw bytes).

use std::borrow::Cow;

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";
const BASE64_MAP: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const BASE64_PADDING: u8 = b'=';

/// Whether `byte` is written as itself (DuckDB's `IsRegularCharacter`).
const fn is_regular(byte: u8) -> bool {
    matches!(byte, 32..=126) && !matches!(byte, b'\\' | b'\'' | b'"')
}

fn lossy(input: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(input)
}

/// Length of the text form of `bytes` (DuckDB's `Blob::GetStringSize`).
pub fn text_len(bytes: &[u8]) -> usize {
    bytes.iter().map(|&b| if is_regular(b) { 1 } else { 4 }).sum()
}

/// Appends the text form of `bytes` (`CAST(blob AS VARCHAR)`, DuckDB's `Blob::ToString`).
pub fn write_text(bytes: &[u8], out: &mut Vec<u8>) {
    out.reserve(text_len(bytes));
    for &b in bytes {
        if is_regular(b) {
            out.push(b);
        } else {
            out.extend_from_slice(&[
                b'\\',
                b'x',
                HEX_UPPER[usize::from(b >> 4)],
                HEX_UPPER[usize::from(b & 0xF)],
            ]);
        }
    }
}

/// Value of an ASCII hex digit (either case).
fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Appends the bytes of the BLOB written as `input` (`CAST(text AS BLOB)`, DuckDB's
/// `Blob::TryGetBlobSize` + `Blob::ToBlob`).
///
/// On failure `out` is left unchanged and the error is DuckDB's conversion message, e.g.
/// `Invalid hex escape code encountered in string -> blob conversion of string "\xZZ": \xZZ`.
pub fn parse_text(input: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
    let start_len = out.len();
    let result = parse_text_into(input, out);
    if result.is_err() {
        out.truncate(start_len);
    }
    result
}

fn parse_text_into(input: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
    out.reserve(input.len());
    let mut rest = input;
    while let Some((&b, tail)) = rest.split_first() {
        if b == b'\\' {
            let [marker, hi, lo, after @ ..] = tail else {
                return Err(format!(
                    "Invalid hex escape code encountered in string -> blob conversion of string \
                     \"{}\": unterminated escape code at end of blob",
                    lossy(input)
                ));
            };
            match (marker, hex_value(*hi), hex_value(*lo)) {
                (b'x', Some(hi), Some(lo)) => out.push((hi << 4) | lo),
                _ => {
                    return Err(format!(
                        "Invalid hex escape code encountered in string -> blob conversion of \
                         string \"{}\": {}",
                        lossy(input),
                        lossy(&[b'\\', *marker, *hi, *lo])
                    ));
                }
            }
            rest = after;
        } else if b.is_ascii() {
            out.push(b);
            rest = tail;
        } else {
            return Err(format!(
                "Invalid byte encountered in STRING -> BLOB conversion of string \"{}\". All \
                 non-ascii characters must be escaped with hex codes (e.g. \\xAA)",
                lossy(input)
            ));
        }
    }
    Ok(())
}

/// Length of the base64 encoding of `len` bytes (DuckDB's `Blob::ToBase64Size`).
pub const fn base64_len(len: usize) -> usize {
    len.div_ceil(3) * 4
}

/// Appends the padded base64 encoding of `bytes` (`to_base64`, DuckDB's `Blob::ToBase64`).
pub fn write_base64(bytes: &[u8], out: &mut Vec<u8>) {
    let sextet = |v: u32, shift: u32| BASE64_MAP[((v >> shift) & 0x3F) as usize];
    out.reserve(base64_len(bytes.len()));
    let (chunks, remainder) = bytes.as_chunks::<3>();
    for &[a, b, c] in chunks {
        let v = u32::from(a) << 16 | u32::from(b) << 8 | u32::from(c);
        out.extend_from_slice(&[sextet(v, 18), sextet(v, 12), sextet(v, 6), sextet(v, 0)]);
    }
    match *remainder {
        [a] => {
            let v = u32::from(a) << 16;
            out.extend_from_slice(&[sextet(v, 18), sextet(v, 12), BASE64_PADDING, BASE64_PADDING]);
        }
        [a, b] => {
            let v = u32::from(a) << 16 | u32::from(b) << 8;
            out.extend_from_slice(&[sextet(v, 18), sextet(v, 12), sextet(v, 6), BASE64_PADDING]);
        }
        _ => {}
    }
}

/// Value of a base64 digit.
fn base64_value(c: u8) -> Option<u32> {
    let v = match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(u32::from(v))
}

/// Appends the bytes encoded by the base64 text `input` (`from_base64`, DuckDB's
/// `Blob::FromBase64`). Padding (`=`) is only accepted in the last two positions.
///
/// On failure `out` is left unchanged and the error is DuckDB's conversion message.
pub fn parse_base64(input: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
    if !input.len().is_multiple_of(4) {
        return Err(format!(
            "Could not decode string \"{}\" as base64: length must be a multiple of 4",
            lossy(input)
        ));
    }
    let decoded_len = match input {
        [] => return Ok(()),
        [.., BASE64_PADDING, _] => input.len() / 4 * 3 - 2,
        [.., BASE64_PADDING] => input.len() / 4 * 3 - 1,
        _ => input.len() / 4 * 3,
    };
    let start_len = out.len();
    out.reserve(decoded_len);
    let (groups, _) = input.as_chunks::<4>();
    for (group_idx, group) in groups.iter().enumerate() {
        let is_last = group_idx + 1 == groups.len();
        let mut combined = 0u32;
        for (i, &c) in group.iter().enumerate() {
            let value = if is_last && i >= 2 && c == BASE64_PADDING {
                Some(0)
            } else {
                base64_value(c)
            };
            let Some(value) = value else {
                out.truncate(start_len);
                return Err(format!(
                    "Could not decode string \"{}\" as base64: invalid byte value '{c}' at \
                     position {}",
                    lossy(input),
                    group_idx * 4 + i
                ));
            };
            combined = combined << 6 | value;
        }
        let [_, b0, b1, b2] = combined.to_be_bytes();
        out.extend_from_slice(&[b0, b1, b2]);
    }
    out.truncate(start_len + decoded_len);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(bytes: &[u8]) -> String {
        let mut out = Vec::new();
        write_text(bytes, &mut out);
        assert_eq!(out.len(), text_len(bytes));
        String::from_utf8(out).unwrap()
    }

    fn parse(input: &str) -> Result<Vec<u8>, String> {
        let mut out = b"keep".to_vec();
        let result = parse_text(input.as_bytes(), &mut out);
        match result {
            Ok(()) => Ok(out.split_off(4)),
            Err(e) => {
                assert_eq!(out, b"keep");
                Err(e)
            }
        }
    }

    #[test]
    fn text_escapes_non_printable_and_quotes() {
        assert_eq!(text(b"\xAA"), r"\xAA");
        assert_eq!(text(b"'\"\\"), r"\x27\x22\x5C");
        assert_eq!(text(b"\n\x7F\x80\xFF a~"), r"\x0A\x7F\x80\xFF a~");
        assert_eq!(text(&[0; 2]), r"\x00\x00");
        assert_eq!(text(b""), "");
    }

    #[test]
    fn parse_accepts_ascii_and_hex_escapes() {
        assert_eq!(parse(r"a\x41\x4a\x4A'").unwrap(), b"aAJJ'");
        assert_eq!(parse(r"\x00\xff").unwrap(), [0, 0xFF]);
        assert_eq!(parse("").unwrap(), b"");
    }

    #[test]
    fn parse_errors_match_duckdb() {
        let prefix = "Invalid hex escape code encountered in string -> blob conversion of string";
        assert_eq!(
            parse(r#"a\x41\\x42'""#).unwrap_err(),
            format!(r#"{prefix} "a\x41\\x42'"": \\x4"#)
        );
        assert_eq!(
            parse(r"\x4").unwrap_err(),
            format!(r#"{prefix} "\x4": unterminated escape code at end of blob"#)
        );
        assert_eq!(
            parse(r"ab\").unwrap_err(),
            format!(r#"{prefix} "ab\": unterminated escape code at end of blob"#)
        );
        assert_eq!(
            parse(r"\xZZab").unwrap_err(),
            format!(r#"{prefix} "\xZZab": \xZZ"#)
        );
        assert_eq!(parse(r"\y123").unwrap_err(), format!(r#"{prefix} "\y123": \y12"#));
        assert_eq!(
            parse("é").unwrap_err(),
            "Invalid byte encountered in STRING -> BLOB conversion of string \"é\". All \
             non-ascii characters must be escaped with hex codes (e.g. \\xAA)"
        );
    }

    fn base64(bytes: &[u8]) -> String {
        let mut out = Vec::new();
        write_base64(bytes, &mut out);
        assert_eq!(out.len(), base64_len(bytes.len()));
        String::from_utf8(out).unwrap()
    }

    fn from_base64(input: &str) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        parse_base64(input.as_bytes(), &mut out).map(|()| out)
    }

    #[test]
    fn base64_round_trip() {
        assert_eq!(base64(b"\x00\xff\x10abc"), "AP8QYWJj");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b""), "");
        assert_eq!(from_base64("YWJj").unwrap(), b"abc");
        assert_eq!(from_base64("YQ==").unwrap(), b"a");
        assert_eq!(from_base64("YWI=").unwrap(), b"ab");
        assert_eq!(from_base64("AP8QYWJj").unwrap(), b"\x00\xff\x10abc");
        assert_eq!(from_base64("").unwrap(), b"");
    }

    #[test]
    fn base64_errors_match_duckdb() {
        let err = |s: &str| from_base64(s).unwrap_err();
        assert_eq!(
            err("YWJ"),
            "Could not decode string \"YWJ\" as base64: length must be a multiple of 4"
        );
        for (input, byte, pos) in [
            ("YW!j", 33, 2),
            ("YW=jYWJj", 61, 2),
            ("YWJjY===", 61, 5),
            ("YWJj=WJj", 61, 4),
            ("Y===", 61, 1),
            ("====", 61, 0),
        ] {
            assert_eq!(
                err(input),
                format!(
                    "Could not decode string \"{input}\" as base64: invalid byte value \
                     '{byte}' at position {pos}"
                )
            );
        }
    }
}
