//! `length`, `strlen`, `bit_length`, `octet_length`, `length_grapheme` and
//! `array_length` (DuckDB's `length.cpp`).

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, Vector};

use crate::scalar::executor::{Str, addressable, binary, unary};
use crate::scalar::string::utf8::{char_count, grapheme_count};

type BindResult = Result<Option<Arc<dyn FunctionData>>>;

fn string_length_function(name: &str, argument: LogicalType, length: fn(&[u8]) -> i64) -> ScalarFunction {
    ScalarFunction::new(
        name,
        vec![argument],
        LogicalType::BIGINT,
        move |args, _, result| {
            unary::<Str, i64>(args, result, |input, out| {
                *out = length(input);
                Ok(true)
            })
        },
    )
}

fn bit_string_length(bits: &[u8]) -> i64 {
    rustdb_types::bit::bit_length(bits) as i64
}

/// `length(list)`: the number of elements.
fn list_length(args: &DataChunk, result: &mut Vector) -> Result<()> {
    unary::<ListEntry, i64>(args, result, |entry, out| {
        *out = entry.length as i64;
        Ok(true)
    })
}

/// `length(array)`: the fixed size, NULL for NULL arrays (DuckDB's `ArrayLengthFunction`).
fn array_length(args: &DataChunk, result: &mut Vector, size: i64) -> Result<()> {
    let count = args.len();
    let input = addressable(args.column(0), count);
    let constant = input.is_constant();
    let rows = if constant { 1 } else { count };
    result.reset_flat(rows);
    result.values_mut::<i64>()[..rows].fill(size);
    let nulls: Vec<usize> = (0..rows).filter(|&row| input.is_null(row)).collect();
    let validity = result.validity_mut();
    for row in nulls {
        validity.set_invalid(row, rows);
    }
    if constant {
        result.make_constant();
    }
    Ok(())
}

fn unresolved(ty: &LogicalType) -> bool {
    ty.id() == LogicalTypeId::Unknown
}

/// DuckDB's `ArrayOrListLengthBind`.
fn bind_length(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let ty = input.argument_types[0].clone();
    if unresolved(&ty) {
        return Err(crate::parameter_not_resolved());
    }
    match ty.id() {
        LogicalTypeId::Array => {
            let size = ty.array_child().map_or(0, |(_, size)| size) as i64;
            function.kernel = Arc::new(move |args, _, result| array_length(args, result, size));
        }
        LogicalTypeId::List => function.kernel = Arc::new(|args, _, result| list_length(args, result)),
        _ => return Err(Error::binder("length can only be used on arrays or lists")),
    }
    function.arguments[0] = ty;
    Ok(None)
}

/// The dimension sizes of a nested ARRAY type (DuckDB's `ArrayLengthBinaryFunctionData`).
#[derive(Debug)]
struct ArrayDimensions(Vec<i64>);

impl FunctionData for ArrayDimensions {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// DuckDB's `ArrayOrListLengthBinaryBind`.
fn bind_length_dimension(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let ty = input.argument_types[0].clone();
    if unresolved(&ty) {
        return Err(crate::parameter_not_resolved());
    }
    match ty.id() {
        LogicalTypeId::Array => {
            let mut dimensions = Vec::new();
            let mut current = ty.clone();
            while let Some((child, size)) = current.array_child().map(|(child, size)| (child.clone(), size)) {
                dimensions.push(size as i64);
                current = child;
            }
            function.arguments[0] = ty;
            function.kernel = Arc::new(|args, state: &mut FunctionState, result| {
                let dimensions = state
                    .bind_data
                    .as_ref()
                    .and_then(|data| data.as_any().downcast_ref::<ArrayDimensions>())
                    .map(|data| data.0.clone())
                    .ok_or_else(|| Error::internal("array_length without bind data"))?;
                let max = dimensions.len() as i64;
                // Only the dimension column decides the result (the array itself may be NULL).
                let dimension = DataChunk::new(vec![args.column(1).clone()], args.len());
                unary::<i64, i64>(&dimension, result, |dimension, out| {
                    if dimension < 1 || dimension > max {
                        return Err(Error::out_of_range(format!(
                            "array_length dimension '{dimension}' out of range (min: '1', max: '{max}')"
                        )));
                    }
                    *out = dimensions[(dimension - 1) as usize];
                    Ok(true)
                })
            });
            Ok(Some(Arc::new(ArrayDimensions(dimensions))))
        }
        LogicalTypeId::List => {
            function.arguments[0] = ty;
            function.kernel = Arc::new(|args, _, result| {
                binary::<ListEntry, i64, i64>(args, result, |entry, dimension, out| {
                    if dimension != 1 {
                        return Err(Error::not_implemented(
                            "array_length for lists with dimensions other than 1 not implemented",
                        ));
                    }
                    *out = entry.length as i64;
                    Ok(true)
                })
            });
            Ok(None)
        }
        _ => Err(Error::binder("array_length can only be used on arrays or lists")),
    }
}

fn any_list() -> LogicalType {
    LogicalType::list(LogicalType::ANY)
}

fn unbound(name: &str, arguments: Vec<LogicalType>) -> ScalarFunction {
    ScalarFunction::new(name, arguments, LogicalType::BIGINT, |_, _, _| {
        Err(Error::internal("length used without being bound"))
    })
}

/// `length` (DuckDB's `LengthFun`).
pub fn length_functions(name: &str) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(string_length_function(name, LogicalType::VARCHAR, |input| {
        char_count(input) as i64
    }));
    set.add(string_length_function(name, LogicalType::BIT, bit_string_length));
    set.add(unbound(name, vec![any_list()]).with_bind(bind_length));
    set
}

/// `length_grapheme` (DuckDB's `LengthGraphemeFun`).
pub fn length_grapheme_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("length_grapheme");
    set.add(string_length_function(
        "length_grapheme",
        LogicalType::VARCHAR,
        |input| grapheme_count(input) as i64,
    ));
    set
}

/// `array_length` (DuckDB's `ArrayLengthFun`).
pub fn array_length_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("array_length");
    set.add(unbound("array_length", vec![any_list()]).with_bind(bind_length));
    set.add(unbound("array_length", vec![any_list(), LogicalType::BIGINT]).with_bind(bind_length_dimension));
    set
}

/// `strlen`: the size in bytes.
pub fn strlen_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("strlen");
    set.add(string_length_function("strlen", LogicalType::VARCHAR, |input| {
        input.len() as i64
    }));
    set
}

/// `bit_length` (DuckDB's `BitLengthFun`).
pub fn bit_length_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("bit_length");
    set.add(string_length_function(
        "bit_length",
        LogicalType::VARCHAR,
        |input| 8 * input.len() as i64,
    ));
    set.add(string_length_function(
        "bit_length",
        LogicalType::BIT,
        bit_string_length,
    ));
    set
}

/// `octet_length` (DuckDB's `OctetLengthFun`).
pub fn octet_length_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("octet_length");
    set.add(string_length_function(
        "octet_length",
        LogicalType::BLOB,
        |input| input.len() as i64,
    ));
    set.add(string_length_function("octet_length", LogicalType::BIT, |bits| {
        rustdb_types::bit::octet_length(bits) as i64
    }));
    set
}
