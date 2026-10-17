//! `json_pretty` (DuckDB's `json_pretty.cpp`).

use rustdb_common::Result;
use rustdb_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::json_executors::{StrBuilder, unary_execute};
use crate::json_type;
use crate::yyjson::{Doc, ValId, write_val_pretty};

fn pretty_print(doc: &Doc<'_>, val: ValId, out: &mut StrBuilder) -> Result<()> {
    out.push_with(|buf| write_val_pretty(buf, doc, val));
    Ok(())
}

fn pretty(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    unary_execute(args, state, result, &mut pretty_print)
}

/// `JSONFunctions::GetPrettyPrintFunction`.
pub(crate) fn pretty_function() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("json_pretty");
    set.add(ScalarFunction::new(
        "json_pretty",
        vec![json_type()],
        LogicalType::VARCHAR,
        pretty,
    ));
    set
}
