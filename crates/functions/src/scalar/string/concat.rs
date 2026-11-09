//! `concat`, `||`, `list_concat` (DuckDB's `concat.cpp`) and `concat_ws`
//! (`concat_ws.cpp`).

use std::borrow::Cow;
use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_function::{BindInput, FunctionData, NullHandling, ScalarFunction, ScalarFunctionSet};
use roastedduck_types::kernels::scatter;
use roastedduck_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, Vector};

use crate::scalar::executor::{Str, StrOut, addressable, binary, variadic};
use crate::scalar::nested::{ListInput, all_constant, list_child_type};

type BindResult = Result<Option<Arc<dyn FunctionData>>>;

/// `concat(...)` on strings: NULL arguments are skipped (DuckDB's `StringConcatFunction`).
fn string_concat(args: &DataChunk, result: &mut Vector) -> Result<()> {
    variadic::<Str, StrOut>(args, result, true, |values, out| {
        for value in values.iter().flatten() {
            out.extend_from_slice(value);
        }
        Ok(true)
    })
}

/// `a || b` on strings (DuckDB's `ConcatOperator`).
fn concat_operator(args: &DataChunk, result: &mut Vector) -> Result<()> {
    binary::<Str, Str, StrOut>(args, result, |left, right, out| {
        out.extend_from_slice(left);
        out.extend_from_slice(right);
        Ok(true)
    })
}

/// Concatenates lists row by row; a NULL list makes the operator's result NULL and is
/// skipped by the function (DuckDB's `ListConcatFunction`).
fn list_concat(args: &DataChunk, result: &mut Vector, is_operator: bool) -> Result<()> {
    let count = args.len();
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { count };
    let columns: Vec<Cow<'_, Vector>> = args
        .columns()
        .iter()
        .map(|column| addressable(column, count))
        .collect();
    let lists = columns
        .iter()
        .map(|column| ListInput::new(column))
        .collect::<Result<Vec<_>>>()?;
    // For every input, the child rows to copy and the result positions they go to.
    let mut selections: Vec<(Vec<Option<u32>>, Vec<u32>)> = vec![(Vec::new(), Vec::new()); lists.len()];
    let mut entries = Vec::with_capacity(rows);
    let mut nulls = Vec::new();
    let mut offset = 0usize;
    for row in 0..rows {
        let mut length = 0usize;
        for (list, (sources, positions)) in lists.iter().zip(&mut selections) {
            let Some(entry) = list.get(row) else {
                if is_operator {
                    nulls.push(row);
                }
                continue;
            };
            for child in entry.offset..entry.offset + entry.length {
                sources.push(Some(child as u32));
                positions.push((offset + length) as u32);
                length += 1;
            }
        }
        entries.push(ListEntry::new(offset, length));
        offset += length;
    }
    let child_type = list_child_type(result.logical_type())?;
    let mut child = Vector::flat(child_type, offset.max(1));
    for (list, (sources, positions)) in lists.iter().zip(&selections) {
        if !sources.is_empty() {
            scatter(&list.child.select_rows(sources), &mut child, positions)?;
        }
    }
    let mut output = Vector::flat(result.logical_type().clone(), rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries);
    output.set_list_child(child, offset);
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, rows);
    }
    if constant {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

/// DuckDB's `SetArgumentType`.
fn set_argument_type(function: &mut ScalarFunction, ty: &LogicalType, is_operator: bool) {
    if is_operator {
        function.arguments[0] = ty.clone();
        function.arguments[1] = ty.clone();
    } else {
        for argument in &mut function.arguments {
            *argument = ty.clone();
        }
        function.varargs = Some(ty.clone());
    }
    function.return_type = ty.clone();
}

/// Installs the kernel for the bound return type (DuckDB's `ConcatFunction` dispatch).
fn set_kernel(function: &mut ScalarFunction, is_operator: bool) {
    function.kernel = match function.return_type.id() {
        LogicalTypeId::SqlNull => Arc::new(|_, _, result: &mut Vector| {
            *result = Vector::constant_null(LogicalType::SQLNULL);
            Ok(())
        }),
        LogicalTypeId::List => Arc::new(move |args, _, result| list_concat(args, result, is_operator)),
        _ if is_operator => Arc::new(|args, _, result| concat_operator(args, result)),
        _ => Arc::new(|args, _, result| string_concat(args, result)),
    };
}

/// DuckDB's `BindListConcat`.
fn bind_list_concat(function: &mut ScalarFunction, types: &[LogicalType], is_operator: bool) -> Result<()> {
    let mut child_type = LogicalType::SQLNULL;
    let mut all_null = true;
    for ty in types {
        if ty.id() == LogicalTypeId::SqlNull {
            continue;
        }
        all_null = false;
        let next = match ty.id() {
            LogicalTypeId::Unknown => return Err(crate::parameter_not_resolved()),
            LogicalTypeId::List | LogicalTypeId::Array => list_child_type(ty)?,
            _ => {
                let mut list = String::new();
                for (index, ty) in types.iter().enumerate() {
                    if !list.is_empty() {
                        list.push_str(if index + 1 == types.len() { " and " } else { ", " });
                    }
                    list.push_str(&ty.to_string());
                }
                return Err(Error::binder(format!(
                    "Cannot concatenate types {list} - an explicit cast is required"
                )));
            }
        };
        child_type = LogicalType::try_max_logical_type(&child_type, &next).ok_or_else(|| {
            Error::binder(format!(
                "Cannot concatenate lists of types {child_type}[] and {next}[] - an explicit cast is required"
            ))
        })?;
    }
    let ty = if all_null {
        LogicalType::SQLNULL
    } else {
        LogicalType::list(child_type)
    };
    set_argument_type(function, &ty, is_operator);
    Ok(())
}

/// DuckDB's `BindConcatFunctionInternal`.
fn bind_concat(function: &mut ScalarFunction, input: &BindInput<'_>, is_operator: bool) -> BindResult {
    let types = input.argument_types;
    let mut is_list = false;
    let mut all_null = true;
    let mut all_blob = is_operator;
    for ty in types {
        match ty.id() {
            LogicalTypeId::Unknown => return Err(crate::parameter_not_resolved()),
            LogicalTypeId::List | LogicalTypeId::Array => is_list = true,
            _ => {}
        }
        if ty.id() != LogicalTypeId::Blob {
            all_blob = false;
        }
        if ty.id() != LogicalTypeId::SqlNull {
            all_null = false;
        }
    }
    if is_list {
        bind_list_concat(function, types, is_operator)?;
    } else if all_null {
        let list_varargs = function
            .varargs
            .as_ref()
            .is_some_and(|ty| matches!(ty.id(), LogicalTypeId::List | LogicalTypeId::Array));
        let ty = if is_operator || list_varargs {
            LogicalType::SQLNULL
        } else {
            LogicalType::VARCHAR
        };
        set_argument_type(function, &ty, is_operator);
    } else {
        let ty = if all_blob {
            LogicalType::BLOB
        } else {
            LogicalType::VARCHAR
        };
        set_argument_type(function, &ty, is_operator);
    }
    set_kernel(function, is_operator);
    Ok(None)
}

fn unbound_concat() -> impl Fn(&DataChunk, &mut roastedduck_function::FunctionState, &mut Vector) -> Result<()>
{
    |_, _, _| Err(Error::internal("concat used without being bound"))
}

/// `concat(...)` (DuckDB's `ConcatFun`).
pub fn concat_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "concat",
        vec![LogicalType::ANY],
        LogicalType::ANY,
        unbound_concat(),
    )
    .with_bind(|function, input| bind_concat(function, input, false));
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("concat");
    set.add(function);
    set
}

/// `a || b` (DuckDB's `ConcatOperatorFun`).
pub fn concat_operator_functions() -> ScalarFunctionSet {
    let function = ScalarFunction::new(
        "||",
        vec![LogicalType::ANY, LogicalType::ANY],
        LogicalType::ANY,
        unbound_concat(),
    )
    .with_bind(|function, input| bind_concat(function, input, true));
    let mut set = ScalarFunctionSet::new("||");
    set.add(function);
    set
}

/// `list_concat(...)` (DuckDB's `ListConcatFun`).
pub fn list_concat_functions() -> ScalarFunctionSet {
    let any_list = LogicalType::list(LogicalType::ANY);
    let mut function = ScalarFunction::new("list_concat", vec![], any_list.clone(), unbound_concat())
        .with_bind(|function, input| bind_concat(function, input, false));
    function.varargs = Some(any_list);
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("list_concat");
    set.add(function);
    set
}

/// `concat_ws(separator, ...)` (DuckDB's `ConcatWsFun`): NULL arguments are skipped, a
/// NULL separator gives NULL.
pub fn concat_ws_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "concat_ws",
        vec![LogicalType::VARCHAR, LogicalType::ANY],
        LogicalType::VARCHAR,
        |args, _, result| {
            variadic::<Str, StrOut>(args, result, true, |values, out| {
                let Some(Some(separator)) = values.first() else {
                    return Ok(false);
                };
                let mut first = true;
                for value in values[1..].iter().flatten() {
                    if !first {
                        out.extend_from_slice(separator);
                    }
                    out.extend_from_slice(value);
                    first = false;
                }
                Ok(true)
            })
        },
    )
    .with_bind(|function, _| {
        function.arguments.fill(LogicalType::VARCHAR);
        function.varargs = Some(LogicalType::VARCHAR);
        Ok(None)
    });
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("concat_ws");
    set.add(function);
    set
}
