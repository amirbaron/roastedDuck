//! `unnest(list)` in the FROM clause (DuckDB's `function/table/unnest.cpp`): one row per
//! element of the list.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
    TableScanState,
};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value, Vector};

/// The list of one call and its element type (`UnnestBindData`, plus the constant input
/// row of the in-out function).
#[derive(Debug)]
struct UnnestData {
    list: Value,
    child_type: LogicalType,
}

impl FunctionData for UnnestData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `UnnestBind`.
fn unnest_bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let ([input_type], [list]) = (input.input_table_types, input.inputs) else {
        return Err(Error::binder("UNNEST requires a single list as input"));
    };
    if input_type.id() != LogicalTypeId::List {
        return Err(Error::binder("UNNEST requires a single list as input"));
    }
    let child_type = input_type
        .child_type()
        .cloned()
        .ok_or_else(|| Error::internal("LIST type without a child type"))?;
    Ok(TableFunctionBindOutput {
        return_types: vec![child_type.clone()],
        names: vec!["unnest".to_owned()],
        bind_data: Some(Arc::new(UnnestData {
            list: list.clone(),
            child_type,
        })),
    })
}

/// `UnnestFunction`: the elements of the list, a vector's worth per call.
fn unnest_scan(
    bind_data: Option<&dyn FunctionData>,
    _: &[u64],
    state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<UnnestData>())
        .ok_or_else(|| Error::internal("unnest scan without bind data"))?;
    let elements = data.list.children().unwrap_or_default();
    let start = usize::try_from(state.position)
        .unwrap_or(usize::MAX)
        .min(elements.len());
    let end = (start + STANDARD_VECTOR_SIZE).min(elements.len());
    let mut output = Vector::flat(data.child_type.clone(), STANDARD_VECTOR_SIZE);
    for (row, element) in elements[start..end].iter().enumerate() {
        output.set_value(row, element)?;
    }
    chunk.set_columns(vec![output], end - start);
    state.position = end as u64;
    Ok(end < elements.len())
}

/// `unnest`.
pub fn unnest_functions() -> TableFunctionSet {
    let mut function = TableFunction::new("unnest", vec![LogicalType::ANY])
        .with_bind(unnest_bind)
        .with_scan(unnest_scan);
    function.in_out = true;
    let mut set = TableFunctionSet::new("unnest");
    set.add(function);
    set
}
