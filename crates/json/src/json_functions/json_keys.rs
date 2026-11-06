//! `json_keys` (DuckDB's `json_keys.cpp`).

use rustdb_common::Result;
use rustdb_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, LogicalType, Vector};

use super::{input_types, read_bind, read_many_bind};
use crate::json_executors::{ListBuilder, StrBuilder, binary_execute, execute_many, unary_execute};
use crate::yyjson::{Doc, ValId};

/// `GetJSONKeys`: the keys of an object (empty for other values).
fn json_keys(doc: &Doc<'_>, val: ValId, out: &mut ListBuilder<StrBuilder>) -> Result<()> {
    out.push_list(|keys| {
        if doc.is_obj(val) {
            for (key, _) in doc.obj_iter(val) {
                keys.push(doc.str(key));
            }
        }
        Ok(())
    })
}

fn keys_unary(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    unary_execute(args, state, result, &mut json_keys)
}

fn keys_binary(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    binary_execute(args, state, result, &mut json_keys, None)
}

fn keys_many(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    execute_many(args, state, result, &mut json_keys, None)
}

/// `JSONFunctions::GetKeysFunction`.
pub(crate) fn keys_function() -> ScalarFunctionSet {
    let name = "json_keys";
    let keys = LogicalType::list(LogicalType::VARCHAR);
    let mut set = ScalarFunctionSet::new(name);
    for input in input_types() {
        set.add(ScalarFunction::new(
            name,
            vec![input.clone()],
            keys.clone(),
            keys_unary,
        ));
        set.add(
            ScalarFunction::new(
                name,
                vec![input.clone(), LogicalType::VARCHAR],
                keys.clone(),
                keys_binary,
            )
            .with_bind(read_bind),
        );
        set.add(
            ScalarFunction::new(
                name,
                vec![input, LogicalType::list(LogicalType::VARCHAR)],
                LogicalType::list(keys.clone()),
                keys_many,
            )
            .with_bind(read_many_bind),
        );
    }
    set
}
