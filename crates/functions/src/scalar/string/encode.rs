//! Encodings and formatting: `hex`/`to_hex`, `unhex`/`from_hex`, `bin`/`to_binary`,
//! `unbin`/`from_binary` (DuckDB's `hex.cpp`), `to_base` (`to_base.cpp`), `url_encode`,
//! `url_decode` (`url_encode.cpp`), `format_bytes`, `formatReadableDecimalSize`
//! (`format_bytes.cpp`), `parse_formatted_bytes` (`parse_formatted_bytes.cpp`) and `bar`
//! (`bar.cpp`).

use rustdb_common::{Error, Result};
use rustdb_function::cast::ValueCast;
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::{LogicalType, Value};

use crate::scalar::executor::{ScalarOutput, Str, StrOut, unary, variadic};

fn string_set<A: crate::scalar::executor::ScalarArg>(
    name: &str,
    arguments: &[LogicalType],
    return_type: &LogicalType,
    op: for<'a> fn(A::Item<'a>, &mut Vec<u8>) -> Result<()>,
) -> Vec<ScalarFunction> {
    arguments
        .iter()
        .map(|argument| {
            ScalarFunction::new(name, vec![argument.clone()], return_type.clone(), move |args, _, result| {
                unary::<A, StrOut>(args, result, |input, out| {
                    op(input, out)?;
                    Ok(true)
                })
            })
        })
        .collect()
}

fn set_of(name: &str, functions: Vec<Vec<ScalarFunction>>) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    for function in functions.into_iter().flatten() {
        set.add(function);
    }
    set
}

const HEX_TABLE: &[u8; 16] = b"0123456789ABCDEF";

fn hex_bytes(input: &[u8], out: &mut Vec<u8>) -> Result<()> {
    for &byte in input {
        out.push(HEX_TABLE[usize::from(byte >> 4)]);
        out.push(HEX_TABLE[usize::from(byte & 0xF)]);
    }
    Ok(())
}

fn binary_bytes(input: &[u8], out: &mut Vec<u8>) -> Result<()> {
    for &byte in input {
        for bit in (0..8).rev() {
            out.push(b'0' + ((byte >> bit) & 1));
        }
    }
    Ok(())
}

/// `hex(string|blob|bignum|integer)`.
pub fn hex_functions() -> ScalarFunctionSet {
    let strings = [LogicalType::VARCHAR, LogicalType::BIGNUM, LogicalType::BLOB];
    set_of(
        "hex",
        vec![
            string_set::<Str>("hex", &strings, &LogicalType::VARCHAR, hex_bytes),
            string_set::<i64>("hex", &[LogicalType::BIGINT], &LogicalType::VARCHAR, |v, out| {
                out.extend_from_slice(format!("{:X}", v as u64).as_bytes());
                Ok(())
            }),
            string_set::<u64>("hex", &[LogicalType::UBIGINT], &LogicalType::VARCHAR, |v, out| {
                out.extend_from_slice(format!("{v:X}").as_bytes());
                Ok(())
            }),
            string_set::<i128>("hex", &[LogicalType::HUGEINT], &LogicalType::VARCHAR, |v, out| {
                out.extend_from_slice(format!("{:X}", v as u128).as_bytes());
                Ok(())
            }),
            string_set::<u128>("hex", &[LogicalType::UHUGEINT], &LogicalType::VARCHAR, |v, out| {
                out.extend_from_slice(format!("{v:X}").as_bytes());
                Ok(())
            }),
        ],
    )
}

/// `bin(string|bignum|integer)`.
pub fn bin_functions() -> ScalarFunctionSet {
    let strings = [LogicalType::VARCHAR, LogicalType::BIGNUM];
    set_of(
        "bin",
        vec![
            string_set::<Str>("bin", &strings, &LogicalType::VARCHAR, binary_bytes),
            string_set::<u64>("bin", &[LogicalType::UBIGINT], &LogicalType::VARCHAR, |v, out| {
                out.extend_from_slice(format!("{v:b}").as_bytes());
                Ok(())
            }),
            string_set::<i64>("bin", &[LogicalType::BIGINT], &LogicalType::VARCHAR, |v, out| {
                out.extend_from_slice(format!("{:b}", v as u64).as_bytes());
                Ok(())
            }),
            string_set::<i128>("bin", &[LogicalType::HUGEINT], &LogicalType::VARCHAR, |v, out| {
                out.extend_from_slice(format!("{:b}", v as u128).as_bytes());
                Ok(())
            }),
            string_set::<u128>("bin", &[LogicalType::UHUGEINT], &LogicalType::VARCHAR, |v, out| {
                out.extend_from_slice(format!("{v:b}").as_bytes());
                Ok(())
            }),
        ],
    )
}

/// `StringUtil::GetHexValue`.
fn hex_value(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(Error::invalid_input(format!(
            "Invalid input for hex digit: {}",
            String::from_utf8_lossy(&[c])
        ))),
    }
}

/// `StringUtil::GetBinaryValue`.
fn binary_value(c: u8) -> Result<u8> {
    match c {
        b'0' | b'1' => Ok(c - b'0'),
        _ => Err(Error::invalid_input(format!(
            "Invalid input for binary digit: {}",
            String::from_utf8_lossy(&[c])
        ))),
    }
}

/// DuckDB's `FromHexOperator`: an odd leading digit becomes its own byte.
fn unhex(input: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let (head, rest) = input.split_at(input.len() % 2);
    for &digit in head {
        out.push(hex_value(digit)?);
    }
    for pair in rest.chunks_exact(2) {
        out.push((hex_value(pair[0])? << 4) | hex_value(pair[1])?);
    }
    Ok(())
}

/// DuckDB's `FromBinaryOperator`: leading digits that do not fill a byte form the first.
fn unbin(input: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let (head, rest) = input.split_at(input.len() % 8);
    let byte = |digits: &[u8]| -> Result<u8> {
        digits
            .iter()
            .try_fold(0u8, |byte, &digit| Ok((byte << 1) | binary_value(digit)?))
    };
    if !head.is_empty() {
        let value = byte(head)?;
        out.push(value);
    }
    for chunk in rest.chunks_exact(8) {
        let value = byte(chunk)?;
        out.push(value);
    }
    Ok(())
}

/// `unhex(string)` and `unbin(string)`.
pub fn unhex_unbin_functions() -> (ScalarFunctionSet, ScalarFunctionSet) {
    (
        set_of(
            "unhex",
            vec![string_set::<Str>("unhex", &[LogicalType::VARCHAR], &LogicalType::BLOB, unhex)],
        ),
        set_of(
            "unbin",
            vec![string_set::<Str>("unbin", &[LogicalType::VARCHAR], &LogicalType::BLOB, unbin)],
        ),
    )
}

/// `to_base(number, radix[, min_length])`.
pub fn to_base_functions() -> ScalarFunctionSet {
    const ALPHABET: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut set = ScalarFunctionSet::new("to_base");
    for arguments in [
        vec![LogicalType::BIGINT, LogicalType::INTEGER],
        vec![LogicalType::BIGINT, LogicalType::INTEGER, LogicalType::INTEGER],
    ] {
        set.add(ScalarFunction::new("to_base", arguments, LogicalType::VARCHAR, |args, _, result| {
            // Columns are BIGINT, INTEGER[, INTEGER]: read them as i64 through the
            // variadic executor on a widened copy.
            let count = args.len();
            let input = crate::scalar::executor::addressable(args.column(0), count);
            let radix = crate::scalar::executor::addressable(args.column(1), count);
            let min_length = args
                .columns()
                .get(2)
                .map(|column| crate::scalar::executor::addressable(column, count));
            let constant = args.columns().iter().all(rustdb_types::Vector::is_constant);
            let rows = if constant { 1 } else { count };
            let inputs = input.unified::<i64>();
            let radixes = radix.unified::<i32>();
            let lengths = min_length.as_ref().map(|column| column.unified::<i32>());
            StrOut::write(result, rows, |row, out| {
                let (Some(mut value), Some(radix)) = (inputs.get(row), radixes.get(row)) else {
                    return Ok(false);
                };
                let min_length = match &lengths {
                    Some(lengths) => match lengths.get(row) {
                        Some(length) => length,
                        None => return Ok(false),
                    },
                    None => 0,
                };
                if value < 0 {
                    return Err(Error::invalid_input("'to_base' number must be greater than or equal to 0"));
                }
                if !(2..=36).contains(&radix) {
                    return Err(Error::invalid_input("'to_base' radix must be between 2 and 36"));
                }
                if !(0..=64).contains(&min_length) {
                    return Err(Error::invalid_input("'to_base' min_length must be between 0 and 64"));
                }
                let radix = i64::from(radix);
                let mut digits = Vec::new();
                loop {
                    digits.push(ALPHABET[(value % radix) as usize]);
                    value /= radix;
                    if value == 0 {
                        break;
                    }
                }
                while digits.len() < min_length as usize {
                    digits.push(b'0');
                }
                out.extend(digits.iter().rev());
                Ok(true)
            })?;
            if constant {
                result.make_constant();
            }
            Ok(())
        }));
    }
    set
}

/// `StringUtil::URLEncode` (slashes encoded).
fn url_encode(input: &[u8], out: &mut Vec<u8>) -> Result<()> {
    for &c in input {
        if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'~' | b'.') {
            out.push(c);
        } else {
            out.push(b'%');
            out.push(HEX_TABLE[usize::from(c >> 4)]);
            out.push(HEX_TABLE[usize::from(c & 15)]);
        }
    }
    Ok(())
}

/// `StringUtil::URLDecode` (without `+` to space); the result must be valid UTF-8.
fn url_decode(input: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let start = out.len();
    let mut i = 0;
    while i < input.len() {
        let c = input[i];
        if c == b'%' && i + 2 < input.len() && input[i + 1].is_ascii_hexdigit() && input[i + 2].is_ascii_hexdigit() {
            out.push((hex_value(input[i + 1])? << 4) + hex_value(input[i + 2])?);
            i += 3;
        } else {
            out.push(c);
            i += 1;
        }
    }
    if std::str::from_utf8(&out[start..]).is_err() {
        return Err(Error::invalid_input(format!(
            "Failed to decode string \"{}\" using URL decoding - decoded value is invalid UTF8",
            String::from_utf8_lossy(input)
        )));
    }
    Ok(())
}

/// `url_encode` and `url_decode`.
pub fn url_functions() -> Vec<ScalarFunctionSet> {
    vec![
        set_of(
            "url_encode",
            vec![string_set::<Str>("url_encode", &[LogicalType::VARCHAR], &LogicalType::VARCHAR, url_encode)],
        ),
        set_of(
            "url_decode",
            vec![string_set::<Str>("url_decode", &[LogicalType::VARCHAR], &LogicalType::VARCHAR, url_decode)],
        ),
    ]
}

/// `StringUtil::BytesToHumanReadableString`.
fn bytes_to_human_readable(bytes: u64, multiplier: u64) -> String {
    const UNITS: [[&str; 6]; 2] = [
        ["bytes", "KiB", "MiB", "GiB", "TiB", "PiB"],
        ["bytes", "kB", "MB", "GB", "TB", "PB"],
    ];
    let units = &UNITS[usize::from(multiplier == 1000)];
    let mut array = [0u64; 6];
    array[0] = bytes;
    for i in 1..6 {
        array[i] = array[i - 1] / multiplier;
        array[i - 1] %= multiplier;
    }
    for i in (1..6).rev() {
        if array[i] != 0 {
            let fractional = (array[i - 1] * 10) / multiplier;
            return format!("{}.{} {}", array[i], fractional, units[i]);
        }
    }
    format!("{}{}", array[0], if bytes == 1 { " byte" } else { " bytes" })
}

fn format_bytes_set(name: &str, multiplier: u64) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(ScalarFunction::new(
        name,
        vec![LogicalType::BIGINT],
        LogicalType::VARCHAR,
        move |args, _, result| {
            unary::<i64, StrOut>(args, result, |bytes, out| {
                if bytes < 0 {
                    out.push(b'-');
                }
                out.extend_from_slice(bytes_to_human_readable(bytes.unsigned_abs(), multiplier).as_bytes());
                Ok(true)
            })
        },
    ));
    set
}

/// `format_bytes` (alias `formatReadableSize`) and `formatReadableDecimalSize`.
pub fn format_bytes_functions() -> (ScalarFunctionSet, ScalarFunctionSet) {
    (
        format_bytes_set("format_bytes", 1024),
        format_bytes_set("formatReadableDecimalSize", 1000),
    )
}

/// `StringUtil::ParseFormattedBytes`.
fn parse_formatted_bytes(input: &[u8]) -> Result<u64> {
    let at = |index: usize| input.get(index).copied().unwrap_or(0);
    let is_space = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r');
    let mut index = 0;
    while is_space(at(index)) {
        index += 1;
    }
    let number_start = index;
    while matches!(at(index), b'0'..=b'9' | b'.' | b'e' | b'E' | b'-') {
        index += 1;
    }
    if index == number_start {
        return Err(Error::invalid_input("Memory must have a number (e.g. 1GB)"));
    }
    let number = String::from_utf8_lossy(&input[number_start..index]).into_owned();
    let limit = Value::varchar(number.clone())
        .default_cast_as(&LogicalType::DOUBLE)
        .ok()
        .and_then(|value| value.as_f64())
        .ok_or_else(|| Error::invalid_input(format!("Invalid memory limit: '{number}'")))?;
    while is_space(at(index)) {
        index += 1;
    }
    let unit_start = index;
    while index < input.len() && !is_space(input[index]) {
        index += 1;
    }
    if limit < 0.0 {
        return Err(Error::invalid_input("Memory cannot be negative"));
    }
    let unit = String::from_utf8_lossy(&input[unit_start..index]).to_ascii_lowercase();
    let multiplier: u64 = match unit.as_str() {
        "byte" | "bytes" | "b" => 1,
        "kilobyte" | "kilobytes" | "kb" | "k" => 1000,
        "megabyte" | "megabytes" | "mb" | "m" => 1_000_000,
        "gigabyte" | "gigabytes" | "gb" | "g" => 1_000_000_000,
        "terabyte" | "terabytes" | "tb" | "t" => 1_000_000_000_000,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        "tib" => 1 << 40,
        _ => {
            return Err(Error::invalid_input(format!(
                "Unknown unit for memory: '{unit}' (expected: KB, MB, GB, TB for 1000^i units or KiB, MiB, GiB, \
                 TiB for 1024^i units)"
            )));
        }
    };
    let max_value = u64::MAX as f64;
    if limit > max_value / multiplier as f64 {
        return Err(Error::invalid_input("Memory value out of range: value is too large"));
    }
    Ok((multiplier as f64 * limit) as u64)
}

/// `parse_formatted_bytes(string)`.
pub fn parse_formatted_bytes_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("parse_formatted_bytes");
    set.add(ScalarFunction::new(
        "parse_formatted_bytes",
        vec![LogicalType::VARCHAR],
        LogicalType::UBIGINT,
        |args, _, result| {
            unary::<Str, u64>(args, result, |input, out| {
                *out = parse_formatted_bytes(input)?;
                Ok(true)
            })
        },
    ));
    set
}

/// DuckDB's `BarScalarFunction`.
fn bar(x: f64, min: f64, max: f64, max_width: f64, out: &mut Vec<u8>) -> Result<()> {
    const FULL_BLOCK: &str = "\u{2588}";
    const PARTIAL_BLOCKS: [&str; 8] = [
        " ", "\u{258F}", "\u{258E}", "\u{258D}", "\u{258C}", "\u{258B}", "\u{258A}", "\u{2589}",
    ];
    if !max_width.is_finite() {
        return Err(Error::out_of_range("Max bar width must not be NaN or infinity"));
    }
    if max_width < 1.0 {
        return Err(Error::out_of_range("Max bar width must be >= 1"));
    }
    if max_width > 1000.0 {
        return Err(Error::out_of_range("Max bar width must be <= 1000"));
    }
    let width = if x.is_nan() || min.is_nan() || max.is_nan() || x <= min {
        0.0
    } else if x >= max {
        max_width
    } else {
        max_width * (x - min) / (max - min)
    };
    if !width.is_finite() {
        return Err(Error::out_of_range("Bar width must not be NaN or infinity"));
    }
    let width_as_int = (width * PARTIAL_BLOCKS.len() as f64) as u32 as usize;
    let full_blocks = width_as_int / PARTIAL_BLOCKS.len();
    let mut used_blocks = full_blocks;
    for _ in 0..full_blocks {
        out.extend_from_slice(FULL_BLOCK.as_bytes());
    }
    let remaining = width_as_int % PARTIAL_BLOCKS.len();
    if remaining != 0 {
        used_blocks += 1;
        out.extend_from_slice(PARTIAL_BLOCKS[remaining].as_bytes());
    }
    let integer_max_width = max_width as usize;
    if used_blocks < integer_max_width {
        out.resize(out.len() + integer_max_width - used_blocks, b' ');
    }
    Ok(())
}

/// `bar(x, min, max[, width])`.
pub fn bar_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("bar");
    for arguments in [vec![LogicalType::DOUBLE; 4], vec![LogicalType::DOUBLE; 3]] {
        set.add(ScalarFunction::new("bar", arguments, LogicalType::VARCHAR, |args, _, result| {
            variadic::<f64, StrOut>(args, result, false, |values, out| {
                let value = |index: usize| values.get(index).copied().flatten().unwrap_or(80.0);
                bar(value(0), value(1), value(2), value(3), out)?;
                Ok(true)
            })
        }));
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_readable_bytes() {
        assert_eq!(bytes_to_human_readable(1, 1024), "1 byte");
        assert_eq!(bytes_to_human_readable(1536, 1024), "1.5 KiB");
        assert_eq!(bytes_to_human_readable(1_000_000, 1000), "1.0 MB");
    }
}
