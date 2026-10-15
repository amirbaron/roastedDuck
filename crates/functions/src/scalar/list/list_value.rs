//! `list_value`/`list_pack` and `unpivot_list` (DuckDB's `list_value.cpp`).

use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_types::kernels::scatter;
use rustdb_types::{DataChunk, ListEntry, LogicalType, Vector};

use crate::scalar::executor::addressable;
use crate::scalar::nested::{all_constant, list_child_type};
use crate::signature::template;

/// Builds one list per row from the argument columns (DuckDB's `ListValueFunction`).
pub(crate) fn list_value(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let child_type = list_child_type(result.logical_type())?;
    let columns = args.column_count();
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { args.len() };
    let mut child = Vector::flat(child_type, (rows * columns).max(1));
    for (column, input) in args.columns().iter().enumerate() {
        let input = addressable(input, rows);
        let positions: Vec<u32> = (0..rows).map(|row| (row * columns + column) as u32).collect();
        scatter(&input, &mut child, &positions)?;
    }
    let mut list = Vector::flat(result.logical_type().clone(), rows.max(1));
    {
        let entries = list.values_mut::<ListEntry>();
        for (row, entry) in entries.iter_mut().take(rows.max(1)).enumerate() {
            *entry = ListEntry::new(row * columns, columns);
        }
    }
    list.set_list_child(child, rows * columns);
    if constant {
        list.make_constant();
    }
    *result = list;
    Ok(())
}

/// The `list_value` set: `list_value()` and `list_value(T, ...)`.
pub fn list_value_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("list_value");
    set.add(ScalarFunction::new(
        "list_value",
        Vec::new(),
        LogicalType::list(LogicalType::SQLNULL),
        |args, _, result| list_value(args, result),
    ));
    let element = template("T");
    let mut function = ScalarFunction::new(
        "list_value",
        vec![element.clone()],
        LogicalType::list(element.clone()),
        |args, _, result| list_value(args, result),
    );
    function.varargs = Some(element);
    function.null_handling = NullHandling::Special;
    set.add(function);
    set
}

/// DuckDB's `UnpivotBind`: the element type is the maximum of the argument types.
fn bind_unpivot(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let mut child_type = input
        .argument_types
        .first()
        .cloned()
        .unwrap_or(LogicalType::SQLNULL);
    for ty in input.argument_types.iter().skip(1) {
        child_type = LogicalType::try_max_logical_type(&child_type, ty).ok_or_else(|| {
            Error::binder(format!(
                "Cannot unpivot columns of types {child_type} and {ty} - an explicit cast is required"
            ))
        })?;
    }
    let child_type = crate::signature::normalize_type(&child_type);
    function.varargs = Some(child_type.clone());
    function.return_type = LogicalType::list(child_type);
    Ok(None)
}

/// `unpivot_list(ANY...)`, used by `UNPIVOT`.
pub fn unpivot_list_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "unpivot_list",
        Vec::new(),
        LogicalType::simple(rustdb_types::LogicalTypeId::List),
        |args, _, result| list_value(args, result),
    )
    .with_bind(bind_unpivot);
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("unpivot_list");
    set.add(function);
    set
}
