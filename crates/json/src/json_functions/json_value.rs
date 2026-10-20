//! `json_value` (DuckDB's `json_value.cpp`): like `json_extract`, but NULL unless the value
//! is a scalar.

use rustdb_common::Result;
use rustdb_function::{FunctionState, ScalarFunctionSet};
use rustdb_types::{DataChunk, LogicalType, Vector};

use super::json_extract::read_overloads;
use crate::json_executors::{ColumnBuilder, StrBuilder, binary_execute, execute_many};
use crate::yyjson::{Doc, ValId, tag, write_val};

/// `JSONCommon::JSONValue`.
#[allow(clippy::unnecessary_wraps, reason = "the signature of a JSON read function")]
fn json_value(doc: &Doc<'_>, val: ValId, out: &mut StrBuilder) -> Result<()> {
    match doc.tag(val) {
        tag::NULL | tag::ARR | tag::OBJ => out.push_null(),
        _ => out.push_with(|buf| write_val(buf, doc, val)),
    }
    Ok(())
}

fn value(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    binary_execute(args, state, result, &mut json_value, None)
}

fn value_many(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    execute_many(args, state, result, &mut json_value, None)
}

/// `JSONFunctions::GetValueFunction`.
pub(crate) fn value_function() -> ScalarFunctionSet {
    read_overloads("json_value", &LogicalType::VARCHAR, value, value_many, true)
}
