//! `json_array_length` (DuckDB's `json_array_length.cpp`).

use rustdb_common::Result;
use rustdb_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, LogicalType, Vector};

use super::{input_types, read_bind, read_many_bind};
use crate::json_executors::{NativeBuilder, binary_execute, execute_many, unary_execute};
use crate::yyjson::{Doc, ValId};

#[allow(clippy::unnecessary_wraps, reason = "the signature of a JSON read function")]
fn array_length(doc: &Doc<'_>, val: ValId, out: &mut NativeBuilder<u64>) -> Result<()> {
    out.push(doc.arr_size(val) as u64);
    Ok(())
}

fn length_unary(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    unary_execute(args, state, result, &mut array_length)
}

fn length_binary(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    binary_execute(args, state, result, &mut array_length, None)
}

fn length_many(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    execute_many(args, state, result, &mut array_length, None)
}

/// `JSONFunctions::GetArrayLengthFunction`.
pub(crate) fn array_length_function() -> ScalarFunctionSet {
    let name = "json_array_length";
    let mut set = ScalarFunctionSet::new(name);
    for input in input_types() {
        set.add(ScalarFunction::new(
            name,
            vec![input.clone()],
            LogicalType::UBIGINT,
            length_unary,
        ));
        set.add(
            ScalarFunction::new(
                name,
                vec![input.clone(), LogicalType::VARCHAR],
                LogicalType::UBIGINT,
                length_binary,
            )
            .with_bind(read_bind),
        );
        set.add(
            ScalarFunction::new(
                name,
                vec![input, LogicalType::list(LogicalType::VARCHAR)],
                LogicalType::list(LogicalType::UBIGINT),
                length_many,
            )
            .with_bind(read_many_bind),
        );
    }
    set
}
