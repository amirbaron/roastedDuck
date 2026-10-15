//! `array_value` (DuckDB's `array_value.cpp`).

use rustdb_common::Error;
use rustdb_function::{NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_types::kernels::scatter;
use rustdb_types::{LogicalType, LogicalTypeId, Vector};

use crate::scalar::executor::addressable;
use crate::scalar::nested::all_constant;

/// The largest ARRAY size (DuckDB's `ArrayType::MAX_ARRAY_SIZE`).
pub(crate) const MAX_ARRAY_SIZE: usize = 100_000;

/// `array_value(value, ...)`: a fixed-size array of the common type.
pub fn array_value_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "array_value",
        Vec::new(),
        LogicalType::simple(LogicalTypeId::Array),
        |args, _, result| {
            let (child_type, size) = result
                .logical_type()
                .array_child()
                .map(|(child, size)| (child.clone(), size as usize))
                .ok_or_else(|| Error::internal("array_value result is not an ARRAY"))?;
            let constant = all_constant(args.columns());
            let rows = if constant { 1 } else { args.len() };
            let mut child = Vector::flat(child_type, (rows * size).max(1));
            for (column, input) in args.columns().iter().enumerate() {
                let input = addressable(input, rows);
                let positions: Vec<u32> = (0..rows).map(|row| (row * size + column) as u32).collect();
                scatter(&input, &mut child, &positions)?;
            }
            let mut array = Vector::flat(result.logical_type().clone(), rows.max(1));
            array.set_array_child(child);
            if constant {
                array.make_constant();
            }
            *result = array;
            Ok(())
        },
    )
    .with_bind(|function, input| {
        let Some(first) = input.argument_types.first() else {
            return Err(Error::invalid_input("array_value requires at least one argument"));
        };
        let mut child_type = first.clone();
        for ty in &input.argument_types[1..] {
            child_type = LogicalType::try_max_logical_type(&child_type, ty).ok_or_else(|| {
                Error::not_implemented(format!(
                    "Cannot combine types {child_type} and {ty} - an explicit cast is required"
                ))
            })?;
        }
        if input.argument_types.len() > MAX_ARRAY_SIZE {
            return Err(Error::out_of_range("Array size exceeds maximum allowed size"));
        }
        let child_type = crate::signature::normalize_type(&child_type);
        function.varargs = Some(child_type.clone());
        function.return_type = LogicalType::array(child_type, input.argument_types.len() as u32);
        Ok(None)
    });
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("array_value");
    set.add(function);
    set
}
