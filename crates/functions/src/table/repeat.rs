//! `repeat(value, count)` and `repeat_row(values..., num_rows := n)` (DuckDB's
//! `function/table/repeat.cpp` and `repeat_row.cpp`): a constant row, `count` times.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
    TableScanState,
};
use rustdb_types::{DataChunk, LogicalType, Vector};

/// The repeated row and how often it is produced (`RepeatFunctionData` /
/// `RepeatRowFunctionData`).
#[derive(Debug)]
struct RepeatData {
    values: Vec<rustdb_types::Value>,
    target_count: u64,
}

impl FunctionData for RepeatData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `RepeatFunction` / `RepeatRowFunction`: constant vectors, up to a vector's worth of
/// rows per call.
fn repeat_scan(
    bind_data: Option<&dyn FunctionData>,
    _: &[u64],
    state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<RepeatData>())
        .ok_or_else(|| Error::internal("repeat scan without bind data"))?;
    let remaining = data
        .target_count
        .saturating_sub(state.position)
        .min(STANDARD_VECTOR_SIZE as u64);
    let columns = data
        .values
        .iter()
        .map(Vector::constant)
        .collect::<Result<Vec<_>>>()?;
    #[allow(clippy::cast_possible_truncation, reason = "at most STANDARD_VECTOR_SIZE")]
    chunk.set_columns(columns, remaining as usize);
    state.position += remaining;
    Ok(state.position < data.target_count)
}

/// `RepeatBind`.
fn repeat_bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let [value, count] = input.inputs else {
        return Err(Error::internal("repeat takes two arguments"));
    };
    let name = if value.is_null() {
        "NULL".to_owned()
    } else {
        value.to_varchar()?
    };
    if count.is_null() {
        return Err(Error::binder("Repeat second parameter cannot be NULL"));
    }
    let count = count
        .as_i128()
        .ok_or_else(|| Error::internal("repeat count is not a BIGINT"))?;
    let target_count = u64::try_from(count)
        .map_err(|_| Error::binder("Repeat second parameter cannot be be less than 0"))?;
    Ok(TableFunctionBindOutput {
        return_types: vec![value.logical_type().clone()],
        names: vec![name],
        bind_data: Some(Arc::new(RepeatData {
            values: vec![value.clone()],
            target_count,
        })),
    })
}

/// `RepeatRowBind`.
fn repeat_row_bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let return_types = input
        .inputs
        .iter()
        .map(|value| value.logical_type().clone())
        .collect();
    let names = (0..input.inputs.len())
        .map(|index| format!("column{index}"))
        .collect();
    let num_rows = input
        .named_parameters
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("num_rows"))
        .map(|(_, value)| value)
        .ok_or_else(|| Error::binder("repeat_rows requires num_rows to be specified"))?;
    if input.inputs.is_empty() {
        return Err(Error::binder(
            "repeat_rows requires at least one column to be specified",
        ));
    }
    // `NumericCast<idx_t>(GetValue<int64_t>())`.
    let rows = num_rows
        .as_i128()
        .ok_or_else(|| Error::invalid_input("Attempting to get a value of a NULL"))?;
    let target_count = u64::try_from(rows).map_err(|_| {
        Error::internal(format!(
            "Information loss on integer cast: value {rows} outside of target range [0, {}]",
            u64::MAX
        ))
    })?;
    Ok(TableFunctionBindOutput {
        return_types,
        names,
        bind_data: Some(Arc::new(RepeatData {
            values: input.inputs.to_vec(),
            target_count,
        })),
    })
}

/// `repeat`.
pub fn repeat_functions() -> TableFunctionSet {
    let mut set = TableFunctionSet::new("repeat");
    set.add(
        TableFunction::new("repeat", vec![LogicalType::ANY, LogicalType::BIGINT])
            .with_bind(repeat_bind)
            .with_scan(repeat_scan),
    );
    set
}

/// `repeat_row`.
pub fn repeat_row_functions() -> TableFunctionSet {
    let mut function = TableFunction::new("repeat_row", Vec::new())
        .with_bind(repeat_row_bind)
        .with_scan(repeat_scan);
    function.varargs = Some(LogicalType::ANY);
    function
        .named_parameters
        .push(("num_rows".to_owned(), LogicalType::BIGINT));
    let mut set = TableFunctionSet::new("repeat_row");
    set.add(function);
    set
}
