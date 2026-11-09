//! `encode(VARCHAR) -> BLOB` and `decode(BLOB[, error_behavior]) -> VARCHAR` (DuckDB's
//! `encode.cpp`).

use roastedduck_common::{Error, Result};
use roastedduck_function::{ScalarFunction, ScalarFunctionSet};
use roastedduck_types::LogicalType;

use crate::scalar::executor::{Str, StrOut, binary, unary};

const DECODE_ERROR: &str = "Failure in decode: could not convert blob to UTF8 string, the blob contained invalid \
     UTF8 characters. \nUse try(decode(BLOB)) to return NULL and continue instead of returning an error. Specify \
     decode(BLOB, 'replace') to replace invalid characters with '?'. Specify decode(BLOB, 'ignore') to remove \
     invalid characters when encountered.";

/// What `decode` does with invalid UTF-8 (DuckDB's `DecodeErrorBehavior`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DecodeErrorBehavior {
    Strict,
    Replace,
    Ignore,
}

impl DecodeErrorBehavior {
    fn parse(specifier: &[u8]) -> Result<Self> {
        if specifier.eq_ignore_ascii_case(b"strict") {
            Ok(Self::Strict)
        } else if specifier.eq_ignore_ascii_case(b"replace") {
            Ok(Self::Replace)
        } else if specifier.eq_ignore_ascii_case(b"ignore") {
            Ok(Self::Ignore)
        } else {
            Err(Error::conversion(format!(
                "decode error behavior specifier \"{}\" not recognized",
                String::from_utf8_lossy(specifier)
            )))
        }
    }
}

/// The outcome of DuckDB's `UTF8ExtraByteLoop` for the sequence starting at `start`:
/// whether it is valid, and the index of the last byte it consumed.
fn extra_byte_loop(input: &[u8], start: usize) -> Option<(bool, usize)> {
    let c = input[start];
    let (extra, mut code, mask) = if c & 0xE0 == 0xC0 {
        (1, u32::from(c & 0x1F), 0x0780)
    } else if c & 0xF0 == 0xE0 {
        (2, u32::from(c & 0x0F), 0xF800)
    } else if c & 0xF8 == 0xF0 {
        (3, u32::from(c & 0x07), 0x001F_0000)
    } else {
        return None;
    };
    if input.len() - start < extra + 1 {
        // An incomplete sequence: nothing is consumed beyond the start byte.
        return Some((false, start));
    }
    let mut i = start;
    for _ in 0..extra {
        i += 1;
        let byte = input[i];
        if byte & 0xC0 != 0x80 {
            return Some((false, i));
        }
        code = (code << 6) | u32::from(byte & 0x3F);
    }
    let valid = code & mask != 0 && code <= 0x0010_FFFF && code & 0x01FF_F800 != 0xD800;
    Some((valid, i))
}

/// Replaces every invalid byte sequence by `?` bytes (DuckDB's `Utf8Proc::MakeValid`).
fn make_valid(input: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(input);
    let mut i = 0;
    while i < input.len() {
        if input[i] & 0x80 != 0 {
            match extra_byte_loop(input, i) {
                None => out[i] = b'?',
                Some((true, end)) => i = end,
                Some((false, end)) => {
                    out[i..=end].fill(b'?');
                    i = end;
                }
            }
        }
        i += 1;
    }
}

/// Drops invalid start bytes (DuckDB's `Utf8Proc::RemoveInvalid`). Like DuckDB, the bytes
/// of a multi-byte sequence are kept even when the sequence turns out to be invalid.
fn remove_invalid(input: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < input.len() {
        if input[i] & 0x80 == 0 {
            out.push(input[i]);
        } else if let Some((_, end)) = extra_byte_loop(input, i) {
            out.extend_from_slice(&input[i..=end]);
            i = end;
        }
        i += 1;
    }
}

/// `encode(VARCHAR) -> BLOB`: reinterprets the string bytes.
pub fn encode_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("encode");
    set.add(ScalarFunction::new(
        "encode",
        vec![LogicalType::VARCHAR],
        LogicalType::BLOB,
        |args, _, result| {
            *result = args.column(0).reinterpret(LogicalType::BLOB)?;
            Ok(())
        },
    ));
    set
}

/// `decode(BLOB) -> VARCHAR` and `decode(BLOB, VARCHAR) -> VARCHAR`.
pub fn decode_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("decode");
    set.add(ScalarFunction::new(
        "decode",
        vec![LogicalType::BLOB],
        LogicalType::VARCHAR,
        |args, _, result| {
            unary::<Str, StrOut>(args, result, |input, out| {
                if std::str::from_utf8(input).is_err() {
                    return Err(Error::conversion(DECODE_ERROR));
                }
                out.extend_from_slice(input);
                Ok(true)
            })
        },
    ));
    set.add(ScalarFunction::new(
        "decode",
        vec![LogicalType::BLOB, LogicalType::VARCHAR],
        LogicalType::VARCHAR,
        |args, _, result| {
            binary::<Str, Str, StrOut>(args, result, |input, behavior, out| {
                if std::str::from_utf8(input).is_ok() {
                    out.extend_from_slice(input);
                    return Ok(true);
                }
                match DecodeErrorBehavior::parse(behavior)? {
                    DecodeErrorBehavior::Replace => make_valid(input, out),
                    DecodeErrorBehavior::Ignore => remove_invalid(input, out),
                    DecodeErrorBehavior::Strict => return Err(Error::conversion(DECODE_ERROR)),
                }
                Ok(true)
            })
        },
    ));
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_matches_duckdb() {
        let mut out = Vec::new();
        make_valid(b"\xC3\x28a\xE2\x82\xFF", &mut out);
        assert_eq!(out, b"??a???");
        out.clear();
        remove_invalid(b"\xC3\x28a\xE2\x82", &mut out);
        assert_eq!(out, b"\xC3\x28a\xE2");
        out.clear();
        make_valid("é\u{10000}".as_bytes(), &mut out);
        assert_eq!(out, "é\u{10000}".as_bytes());
    }
}
