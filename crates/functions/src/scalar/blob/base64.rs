//! `to_base64(blob)`/`base64(blob)` and `from_base64(varchar)` (DuckDB's `base64.cpp`,
//! with the codec of `Blob::ToBase64`/`Blob::FromBase64`).

use rustdb_common::{Error, Result};
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::LogicalType;

use crate::scalar::executor::{Str, StrOut, unary};

const BASE64_MAP: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const BASE64_PADDING: u8 = b'=';

/// The 6-bit value of a base64 character, or `None` (DuckDB's `BASE64_DECODING_TABLE`).
fn decode_char(c: u8) -> Option<u32> {
    let value = match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(u32::from(value))
}

/// Appends the base64 encoding of `input` (DuckDB's `Blob::ToBase64`).
pub(crate) fn to_base64(input: &[u8], out: &mut Vec<u8>) {
    let map = |index: u8| BASE64_MAP[usize::from(index & 0x3F)];
    out.reserve(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);
    for chunk in &mut chunks {
        let (a, b, c) = (chunk[0], chunk[1], chunk[2]);
        out.extend_from_slice(&[
            map(a >> 2),
            map(((a & 0x3) << 4) | (b >> 4)),
            map(((b & 0xF) << 2) | (c >> 6)),
            map(c),
        ]);
    }
    match *chunks.remainder() {
        [a] => out.extend_from_slice(&[map(a >> 2), map((a & 0x3) << 4), BASE64_PADDING, BASE64_PADDING]),
        [a, b] => out.extend_from_slice(&[
            map(a >> 2),
            map(((a & 0x3) << 4) | (b >> 4)),
            map((b & 0xF) << 2),
            BASE64_PADDING,
        ]),
        _ => {}
    }
}

/// The decoded size of a base64 string (DuckDB's `Blob::FromBase64Size`).
fn from_base64_size(input: &[u8]) -> Result<usize> {
    let size = input.len();
    if size % 4 != 0 {
        return Err(Error::conversion(format!(
            "Could not decode string \"{}\" as base64: length must be a multiple of 4",
            String::from_utf8_lossy(input)
        )));
    }
    if size < 4 {
        return Ok(0);
    }
    let base = size / 4 * 3;
    Ok(if input[size - 2] == BASE64_PADDING {
        base - 2
    } else if input[size - 1] == BASE64_PADDING {
        base - 1
    } else {
        base
    })
}

/// Decodes the four characters at `base` (DuckDB's `DecodeBase64Bytes`).
fn decode_quad(input: &[u8], base: usize, allow_padding: bool) -> Result<u32> {
    let mut combined = 0u32;
    for (index, &c) in input[base..base + 4].iter().enumerate() {
        let value = if allow_padding && index >= 2 && c == BASE64_PADDING {
            0
        } else {
            decode_char(c).ok_or_else(|| {
                Error::conversion(format!(
                    "Could not decode string \"{}\" as base64: invalid byte value '{c}' at position {}",
                    String::from_utf8_lossy(input),
                    base + index
                ))
            })?
        };
        combined = (combined << 6) | value;
    }
    Ok(combined)
}

/// Appends the bytes encoded by the base64 string `input` (DuckDB's `Blob::FromBase64`).
pub(crate) fn from_base64(input: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let size = from_base64_size(input)?;
    if input.is_empty() {
        return Ok(());
    }
    let start = out.len();
    let mut i = 0;
    while i + 4 < input.len() {
        let combined = decode_quad(input, i, false)?;
        out.extend_from_slice(&combined.to_be_bytes()[1..]);
        i += 4;
    }
    let combined = decode_quad(input, i, true)?.to_be_bytes();
    out.push(combined[1]);
    for &byte in &combined[2..] {
        if out.len() - start < size {
            out.push(byte);
        }
    }
    Ok(())
}

/// `to_base64(BLOB) -> VARCHAR` (alias `base64`).
pub fn to_base64_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("to_base64");
    set.add(ScalarFunction::new(
        "to_base64",
        vec![LogicalType::BLOB],
        LogicalType::VARCHAR,
        |args, _, result| {
            unary::<Str, StrOut>(args, result, |input, out| {
                to_base64(input, out);
                Ok(true)
            })
        },
    ));
    set
}

/// `from_base64(VARCHAR) -> BLOB`.
pub fn from_base64_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("from_base64");
    set.add(ScalarFunction::new(
        "from_base64",
        vec![LogicalType::VARCHAR],
        LogicalType::BLOB,
        |args, _, result| {
            unary::<Str, StrOut>(args, result, |input, out| {
                from_base64(input, out)?;
                Ok(true)
            })
        },
    ));
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(input: &[u8]) -> String {
        let mut out = Vec::new();
        to_base64(input, &mut out);
        String::from_utf8(out).unwrap()
    }

    fn decode(input: &str) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        from_base64(input.as_bytes(), &mut out).map(|()| out)
    }

    #[test]
    fn round_trips() {
        for input in [&b""[..], b"a", b"ab", b"abc", b"abcd", b"\x00\xFF\x10\x7F"] {
            assert_eq!(decode(&encode(input)).unwrap(), input);
        }
        assert_eq!(encode(b"abcd"), "YWJjZA==");
        assert_eq!(encode(b"\x00\xFF"), "AP8=");
    }

    #[test]
    fn errors_match_duckdb() {
        assert_eq!(
            decode("abc").unwrap_err().to_string(),
            "Conversion Error: Could not decode string \"abc\" as base64: length must be a multiple of 4"
        );
        assert_eq!(
            decode("ab!d").unwrap_err().to_string(),
            "Conversion Error: Could not decode string \"ab!d\" as base64: invalid byte value '33' at position 2"
        );
    }
}
