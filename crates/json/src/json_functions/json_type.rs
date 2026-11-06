//! `json_type` (DuckDB's `json_type.cpp`).

use rustdb_common::Result;
use rustdb_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, LogicalType, Vector};

use super::{input_types, read_bind, read_many_bind};
use crate::json_common::val_type_to_string;
use crate::json_executors::{StrBuilder, binary_execute, execute_many, unary_execute};
use crate::yyjson::{Doc, ValId};

#[allow(clippy::unnecessary_wraps, reason = "the signature of a JSON read function")]
fn get_type(doc: &Doc<'_>, val: ValId, out: &mut StrBuilder) -> Result<()> {
    out.push(val_type_to_string(doc, val).as_bytes());
    Ok(())
}

fn type_unary(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    unary_execute(args, state, result, &mut get_type)
}

fn type_binary(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    binary_execute(args, state, result, &mut get_type, None)
}

fn type_many(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    execute_many(args, state, result, &mut get_type, None)
}

/// `JSONFunctions::GetTypeFunction`.
pub(crate) fn type_function() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("json_type");
    for input in input_types() {
        set.add(ScalarFunction::new(
            "json_type",
            vec![input.clone()],
            LogicalType::VARCHAR,
            type_unary,
        ));
        set.add(
            ScalarFunction::new(
                "json_type",
                vec![input.clone(), LogicalType::VARCHAR],
                LogicalType::VARCHAR,
                type_binary,
            )
            .with_bind(read_bind),
        );
        set.add(
            ScalarFunction::new(
                "json_type",
                vec![input, LogicalType::list(LogicalType::VARCHAR)],
                LogicalType::list(LogicalType::VARCHAR),
                type_many,
            )
            .with_bind(read_many_bind),
        );
    }
    set
}
