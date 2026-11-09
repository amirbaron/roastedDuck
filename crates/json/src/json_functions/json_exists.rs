//! `json_exists` (DuckDB's `json_exists.cpp`).

use roastedduck_common::Result;
use roastedduck_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use roastedduck_types::{DataChunk, LogicalType, Vector};

use super::{input_types, read_bind, read_many_bind};
use crate::json_executors::{NativeBuilder, binary_execute, execute_many};
use crate::yyjson::{Doc, ValId};

#[allow(clippy::unnecessary_wraps, reason = "the signature of a JSON read function")]
fn exists(_: &Doc<'_>, _: ValId, out: &mut NativeBuilder<bool>) -> Result<()> {
    out.push(true);
    Ok(())
}

fn not_found(out: &mut NativeBuilder<bool>) {
    out.push(false);
}

fn exists_binary(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    binary_execute(args, state, result, &mut exists, Some(&mut not_found))
}

fn exists_many(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    execute_many(args, state, result, &mut exists, Some(&mut not_found))
}

/// `JSONFunctions::GetExistsFunction`.
pub(crate) fn exists_function() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("json_exists");
    for input in input_types() {
        set.add(
            ScalarFunction::new(
                "json_exists",
                vec![input.clone(), LogicalType::VARCHAR],
                LogicalType::BOOLEAN,
                exists_binary,
            )
            .with_bind(read_bind),
        );
        set.add(
            ScalarFunction::new(
                "json_exists",
                vec![input, LogicalType::list(LogicalType::VARCHAR)],
                LogicalType::list(LogicalType::BOOLEAN),
                exists_many,
            )
            .with_bind(read_many_bind),
        );
    }
    set
}
