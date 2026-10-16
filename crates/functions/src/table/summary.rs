//! `summary(TABLE)` (DuckDB's `function/table/summary.cpp`): a demo of table parameters;
//! each input row with a text rendering of its values in front.

use rustdb_common::Result;
use rustdb_function::{
    FunctionData, InOutResult, TableFunction, TableFunctionBindInput, TableFunctionBindOutput,
    TableFunctionSet, TableScanState,
};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value, Vector};

/// `SummaryFunctionBind`: a `summary` VARCHAR column, then the input columns.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the signature of a table function bind callback"
)]
fn summary_bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let mut return_types = vec![LogicalType::VARCHAR];
    let mut names = vec!["summary".to_owned()];
    return_types.extend(input.input_table_types.iter().cloned());
    names.extend(input.input_table_names.iter().cloned());
    Ok(TableFunctionBindOutput {
        return_types,
        names,
        bind_data: None,
    })
}

/// `Value::ToString`.
fn value_text(value: &Value) -> Result<String> {
    if value.is_null() {
        Ok("NULL".to_owned())
    } else {
        value.to_varchar()
    }
}

/// `SummaryFunction`.
fn summary_in_out(
    _: Option<&dyn FunctionData>,
    _: &mut TableScanState,
    input: &DataChunk,
    output: &mut DataChunk,
) -> Result<InOutResult> {
    let mut summary = Vector::flat(LogicalType::VARCHAR, input.len().max(1));
    for row in 0..input.len() {
        let values = input
            .columns()
            .iter()
            .map(|column| value_text(&column.get_value(row)))
            .collect::<Result<Vec<_>>>()?;
        summary.set_value(row, &Value::varchar(format!("[{}]", values.join(", "))))?;
    }
    let mut columns = vec![summary];
    columns.extend(input.columns().iter().cloned());
    output.set_columns(columns, input.len());
    Ok(InOutResult::NeedMoreInput)
}

/// `summary`.
pub fn summary_functions() -> TableFunctionSet {
    let function = TableFunction::new("summary", vec![LogicalType::simple(LogicalTypeId::Table)])
        .with_bind(summary_bind)
        .with_in_out(summary_in_out);
    let mut set = TableFunctionSet::new("summary");
    set.add(function);
    set
}
