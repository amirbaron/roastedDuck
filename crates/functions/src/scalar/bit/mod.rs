//! Bit string functions (DuckDB's `core_functions/scalar/bit/bitstring.cpp`):
//! `bitstring`, `get_bit`, `set_bit` and `bit_position`.

use rustdb_common::{Error, Result};
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::LogicalType;
use rustdb_types::bit::{bit_length, text_stored_len};

use crate::bitstring::{finalize, get_bit, set_bit, zeros};
use crate::scalar::executor::{Str, StrOut, binary, ternary};

/// Validates `n` as a bit index of `bits` (DuckDB's range check in `GetBitOperator`).
fn bit_index(bits: &[u8], n: i32) -> Result<usize> {
    let len = bit_length(bits);
    match usize::try_from(n) {
        Ok(index) if index < len => Ok(index),
        _ => Err(Error::out_of_range(format!(
            "bit index {n} out of valid range (0..{})",
            // DuckDB prints `BitLength - 1` as an unsigned value.
            (len as u64).wrapping_sub(1)
        ))),
    }
}

/// The requested length of `bitstring`, checked against the input length.
fn target_length(n: i32, input_length: usize) -> Result<usize> {
    let n =
        usize::try_from(n).map_err(|_| Error::invalid_input("The bitstring length cannot be negative"))?;
    if n < input_length {
        return Err(Error::invalid_input(
            "Length must be equal or larger than input string",
        ));
    }
    Ok(n)
}

/// `bitstring(text, n)` (DuckDB's `Bit::BitString`): `text` left-padded with zeros to `n`
/// bits.
fn bitstring_from_text(input: &[u8], n: i32, out: &mut Vec<u8>) -> Result<()> {
    let n = target_length(n, input.len())?;
    text_stored_len(input).map_err(Error::conversion)?;
    let mut result = zeros(n);
    let shift = n - input.len();
    for (i, &c) in input.iter().enumerate() {
        set_bit(&mut result, shift + i, c == b'1');
    }
    finalize(&mut result);
    out.extend_from_slice(&result);
    Ok(())
}

/// `bitstring(bits, n)` (DuckDB's `Bit::ExtendBitString`): `bits` left-padded with zeros to
/// `n` bits.
fn bitstring_from_bits(input: &[u8], n: i32, out: &mut Vec<u8>) -> Result<()> {
    let original = bit_length(input);
    let n = target_length(n, original)?;
    let mut result = zeros(n);
    let shift = n - original;
    for i in 0..original {
        set_bit(&mut result, shift + i, get_bit(input, i));
    }
    finalize(&mut result);
    out.extend_from_slice(&result);
    Ok(())
}

/// The 1-based position of `substring` in `bits`, or 0 (DuckDB's `Bit::BitPosition`).
/// Like DuckDB the scan does not backtrack after a partial match.
fn bit_position(substring: &[u8], bits: &[u8]) -> i32 {
    if substring.len() > bits.len() {
        return 0;
    }
    let substring_length = bit_length(substring);
    let mut matched = 0;
    for i in 0..bit_length(bits) {
        if get_bit(bits, i) == get_bit(substring, matched) {
            matched += 1;
            if matched == substring_length {
                return i32::try_from(i + 2 - substring_length).unwrap_or(i32::MAX);
            }
        } else {
            matched = 0;
        }
    }
    0
}

/// `bitstring(VARCHAR|BIT, INTEGER) -> BIT`.
pub fn bitstring_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("bitstring");
    for (ty, from_text) in [(LogicalType::VARCHAR, true), (LogicalType::BIT, false)] {
        set.add(ScalarFunction::new(
            "bitstring",
            vec![ty, LogicalType::INTEGER],
            LogicalType::BIT,
            move |args, _, result| {
                binary::<Str, i32, StrOut>(args, result, |input, n, out| {
                    if from_text {
                        bitstring_from_text(input, n, out)?;
                    } else {
                        bitstring_from_bits(input, n, out)?;
                    }
                    Ok(true)
                })
            },
        ));
    }
    set
}

/// `get_bit(BIT, INTEGER) -> INTEGER`.
pub fn get_bit_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("get_bit");
    set.add(ScalarFunction::new(
        "get_bit",
        vec![LogicalType::BIT, LogicalType::INTEGER],
        LogicalType::INTEGER,
        |args, _, result| {
            binary::<Str, i32, i32>(args, result, |bits, n, out| {
                *out = i32::from(get_bit(bits, bit_index(bits, n)?));
                Ok(true)
            })
        },
    ));
    set
}

/// `set_bit(BIT, INTEGER, INTEGER) -> BIT`.
pub fn set_bit_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("set_bit");
    set.add(ScalarFunction::new(
        "set_bit",
        vec![LogicalType::BIT, LogicalType::INTEGER, LogicalType::INTEGER],
        LogicalType::BIT,
        |args, _, result| {
            ternary::<Str, i32, i32, StrOut>(args, result, |bits, n, value, out| {
                if value != 0 && value != 1 {
                    return Err(Error::invalid_input("The new bit must be 1 or 0"));
                }
                let index = bit_index(bits, n)?;
                let start = out.len();
                out.extend_from_slice(bits);
                set_bit(&mut out[start..], index, value == 1);
                Ok(true)
            })
        },
    ));
    set
}

/// `bit_position(BIT, BIT) -> INTEGER`.
pub fn bit_position_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("bit_position");
    set.add(ScalarFunction::new(
        "bit_position",
        vec![LogicalType::BIT, LogicalType::BIT],
        LogicalType::INTEGER,
        |args, _, result| {
            binary::<Str, Str, i32>(args, result, |substring, bits, out| {
                *out = bit_position(substring, bits);
                Ok(true)
            })
        },
    ));
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        rustdb_types::bit::parse_text(text.as_bytes(), &mut out).unwrap();
        out
    }

    fn render(bits: &[u8]) -> String {
        let mut out = Vec::new();
        rustdb_types::bit::write_text(bits, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn bitstring_pads_with_zeros() {
        let mut out = Vec::new();
        bitstring_from_text(b"101", 10, &mut out).unwrap();
        assert_eq!(render(&out), "0000000101");
        out.clear();
        bitstring_from_bits(&parse("101"), 10, &mut out).unwrap();
        assert_eq!(render(&out), "0000000101");
        assert_eq!(
            bitstring_from_text(b"", 3, &mut out).unwrap_err().to_string(),
            "Conversion Error: Cannot cast empty string to BIT"
        );
    }

    #[test]
    fn bit_position_does_not_backtrack() {
        assert_eq!(bit_position(&parse("0"), &parse("10")), 2);
        assert_eq!(bit_position(&parse("11"), &parse("0111")), 2);
        assert_eq!(bit_position(&parse("011"), &parse("0010110")), 4);
    }
}
