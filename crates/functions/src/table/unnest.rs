//! `unnest(list)` in the FROM clause (DuckDB's `function/table/unnest.cpp`): one row per
//! element of the list.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::{
    FunctionData, InOutResult, TableFunction, TableFunctionBindInput, TableFunctionBindOutput,
    TableFunctionSet, TableScanState,
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
    // The in-out form (non-constant argument) has no constant input row.
    let [input_type] = input.input_table_types else {
        return Err(Error::binder("UNNEST requires a single list as input"));
    };
    let list = match input.inputs {
        [list] => list.clone(),
        [] => Value::null(input_type.clone()),
        _ => return Err(Error::binder("UNNEST requires a single list as input")),
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
        names: vec!["unnest".to_owned().into()],
        bind_data: Some(Arc::new(UnnestData { list, child_type })),
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

/// Where the in-out `unnest` is in its input (DuckDB's `UnnestOperatorState`).
#[derive(Debug, Default)]
struct UnnestInOutState {
    row: usize,
    position: usize,
}

/// `UnnestFunction` as an in-out function: the elements of each input row's list.
fn unnest_in_out(
    bind_data: Option<&dyn FunctionData>,
    scan_state: &mut TableScanState,
    input: &DataChunk,
    output: &mut DataChunk,
) -> Result<InOutResult> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<UnnestData>())
        .ok_or_else(|| Error::internal("unnest without bind data"))?;
    let state = scan_state
        .local
        .get_or_insert_with(|| Box::new(UnnestInOutState::default()))
        .downcast_mut::<UnnestInOutState>()
        .ok_or_else(|| Error::internal("unnest in-out state of another function"))?;
    let mut column = Vector::flat(data.child_type.clone(), STANDARD_VECTOR_SIZE);
    if state.row >= input.len() {
        state.row = 0;
        state.position = 0;
        output.set_columns(vec![column], 0);
        return Ok(InOutResult::NeedMoreInput);
    }
    let mut count = 0;
    while count < STANDARD_VECTOR_SIZE && state.row < input.len() {
        let list = input.column(0).get_value(state.row);
        let elements = list.children().unwrap_or_default();
        while count < STANDARD_VECTOR_SIZE && state.position < elements.len() {
            column.set_value(count, &elements[state.position])?;
            count += 1;
            state.position += 1;
        }
        if state.position >= elements.len() {
            state.row += 1;
            state.position = 0;
        }
    }
    output.set_columns(vec![column], count);
    Ok(InOutResult::HaveMoreOutput)
}

/// `unnest`.
pub fn unnest_functions() -> TableFunctionSet {
    let mut function = TableFunction::new("unnest", vec![LogicalType::ANY])
        .with_bind(unnest_bind)
        .with_scan(unnest_scan)
        .with_in_out(unnest_in_out);
    function.in_out = true;
    let mut set = TableFunctionSet::new("unnest");
    set.add(function);
    set
}
