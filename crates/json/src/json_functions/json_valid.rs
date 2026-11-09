//! `json_valid` (DuckDB's `json_valid.cpp`).

use roastedduck_common::Result;
use roastedduck_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use roastedduck_types::{DataChunk, LogicalType, Vector};

use super::input_types;
use crate::json_executors::{ColumnBuilder, NativeBuilder, StrInput, addressable, all_constant, local_arena};

fn valid(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let constant = all_constant(args);
    let rows = if constant { 1 } else { args.len() };
    let column = addressable(args.column(0), rows);
    let input = StrInput::new(&column);
    let arena = local_arena(state);
    let mut builder = NativeBuilder::<bool>::default();
    for row in 0..rows {
        match input.get(row) {
            Some(json) => {
                arena.reset();
                builder.push(arena.read(json, false).is_ok());
            }
            None => builder.push_null(),
        }
    }
    *result = builder.finish(&LogicalType::BOOLEAN)?;
    if constant {
        result.make_constant();
    }
    Ok(())
}

/// `JSONFunctions::GetValidFunction`.
pub(crate) fn valid_function() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("json_valid");
    for input in input_types() {
        set.add(ScalarFunction::new(
            "json_valid",
            vec![input],
            LogicalType::BOOLEAN,
            valid,
        ));
    }
    set
}
