//! `json_extract` / `->` and `json_extract_string` / `->>` (DuckDB's `json_extract.cpp`).

use roastedduck_common::Result;
use roastedduck_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use roastedduck_types::{DataChunk, LogicalType, Vector};

use super::{input_types, read_bind, read_many_bind};
use crate::json_executors::{ColumnBuilder, StrBuilder, binary_execute, execute_many};
use crate::json_type;
use crate::yyjson::{Doc, ValId, tag, write_val};

/// `ExtractFromVal`: the value as JSON.
#[allow(clippy::unnecessary_wraps, reason = "the signature of a JSON read function")]
fn extract_from_val(doc: &Doc<'_>, val: ValId, out: &mut StrBuilder) -> Result<()> {
    out.push_with(|buf| write_val(buf, doc, val));
    Ok(())
}

/// `ExtractStringFromVal`: strings without quotes, NULL for JSON null, JSON otherwise.
#[allow(clippy::unnecessary_wraps, reason = "the signature of a JSON read function")]
fn extract_string_from_val(doc: &Doc<'_>, val: ValId, out: &mut StrBuilder) -> Result<()> {
    match doc.tag(val) {
        tag::NULL => out.push_null(),
        tag::STR | tag::STR_NOESC => out.push(doc.str(val)),
        _ => out.push_with(|buf| write_val(buf, doc, val)),
    }
    Ok(())
}

fn extract(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    binary_execute(args, state, result, &mut extract_from_val, None)
}

fn extract_many(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    execute_many(args, state, result, &mut extract_from_val, None)
}

fn extract_string(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    binary_execute(args, state, result, &mut extract_string_from_val, None)
}

fn extract_string_many(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    execute_many(args, state, result, &mut extract_string_from_val, None)
}

/// The three overloads (integer path, text path, list of paths) per input type.
pub(crate) fn read_overloads(
    name: &str,
    return_type: &LogicalType,
    single: fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()>,
    many: fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()>,
    with_integer_path: bool,
) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    for input in input_types() {
        if with_integer_path {
            set.add(
                ScalarFunction::new(
                    name,
                    vec![input.clone(), LogicalType::BIGINT],
                    return_type.clone(),
                    single,
                )
                .with_bind(read_bind),
            );
        }
        set.add(
            ScalarFunction::new(
                name,
                vec![input.clone(), LogicalType::VARCHAR],
                return_type.clone(),
                single,
            )
            .with_bind(read_bind),
        );
        set.add(
            ScalarFunction::new(
                name,
                vec![input, LogicalType::list(LogicalType::VARCHAR)],
                LogicalType::list(return_type.clone()),
                many,
            )
            .with_bind(read_many_bind),
        );
    }
    set
}

/// `JSONFunctions::GetExtractFunction`.
pub(crate) fn extract_functions() -> ScalarFunctionSet {
    read_overloads("json_extract", &json_type(), extract, extract_many, true)
}

/// `JSONFunctions::GetExtractStringFunction`.
pub(crate) fn extract_string_functions() -> ScalarFunctionSet {
    read_overloads(
        "json_extract_string",
        &LogicalType::VARCHAR,
        extract_string,
        extract_string_many,
        true,
    )
}
