//! Bundled Excel SQL functions and Office Open XML workbook I/O.

mod read;
mod write;

use roastedduck_catalog::Catalog;
use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use roastedduck_functions::scalar::executor::{Str, StrOut, binary};
use roastedduck_types::{DataChunk, LogicalType, Vector};

const MAX_ROWS: usize = 1_048_576;
const MAX_COLUMNS: usize = 16_384;
const EXCEL_EPOCH: f64 = 25_569.0;
const MICROS_PER_DAY: i64 = 86_400_000_000;

fn column_name(mut column: usize) -> String {
    let mut letters = Vec::new();
    loop {
        letters.push(char::from(b'A' + (column % 26) as u8));
        if column < 26 {
            break;
        }
        column = column / 26 - 1;
    }
    letters.into_iter().rev().collect()
}

fn escaped_code_unit(text: &str) -> Option<u16> {
    let escape = text.get(..7)?;
    let digits = escape.strip_prefix("_x")?.strip_suffix('_')?;
    u16::from_str_radix(digits, 16).ok()
}

fn decode_excel_string(text: &str) -> String {
    let mut remaining = text;
    let mut units = Vec::new();
    while let Some(index) = remaining.find("_x") {
        units.extend(remaining[..index].encode_utf16());
        remaining = &remaining[index..];
        if let Some(unit) = escaped_code_unit(remaining) {
            units.push(unit);
            remaining = &remaining[7..];
        } else {
            units.push(u16::from(b'_'));
            remaining = &remaining[1..];
        }
    }
    units.extend(remaining.encode_utf16());
    String::from_utf16_lossy(&units)
}

fn io_error(error: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Io, format!("XLSX: {error}"))
}

fn excel_text(input: &DataChunk, _: &mut FunctionState, output: &mut Vector) -> Result<()> {
    binary::<f64, Str, StrOut>(input, output, |number, format, out| {
        if format.len() > 4096 {
            return Err(Error::invalid_input("Excel number format exceeds 4096 bytes"));
        }
        let format = std::str::from_utf8(format).map_err(|error| Error::invalid_input(error.to_string()))?;
        let result = ssfmt::format_default(number, format)
            .map_err(|error| Error::invalid_input(format!("Invalid Excel format: {error}")))?;
        out.extend_from_slice(result.as_bytes());
        Ok(true)
    })
}

pub(crate) fn register(catalog: &Catalog) -> Result<()> {
    for name in ["excel_text", "text"] {
        let mut functions = ScalarFunctionSet::new(name);
        functions.add(ScalarFunction::new(
            name,
            vec![LogicalType::DOUBLE, LogicalType::VARCHAR],
            LogicalType::VARCHAR,
            excel_text,
        ));
        catalog.register_scalar_functions(functions)?;
    }
    catalog.register_table_functions(read::table_function())?;
    catalog.register_copy_function(write::copy_function())
}
