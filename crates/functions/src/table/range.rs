//! `range` and `generate_series` (DuckDB's `function/table/range.cpp`): integer
//! sequences, and timestamp sequences with an interval step.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
    TableScanState,
};
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

/// The constant arguments of one call (the input row of DuckDB's in-out function).
#[derive(Debug)]
struct RangeBindData {
    inputs: Vec<Value>,
    generate_series: bool,
}

impl FunctionData for RangeBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The integer range of one call: `start`, `end` (exclusive) and `increment`, or an
/// empty range when an argument is NULL.
#[derive(Debug)]
struct RangeParameters {
    start: i128,
    end: i128,
    increment: i128,
    empty: bool,
}

impl FunctionData for RangeParameters {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// DuckDB's `GetParameters` and `GenerateRangeParameters` for constant arguments.
fn range_parameters(inputs: &[Value], generate_series: bool) -> Result<RangeParameters> {
    if inputs.iter().any(Value::is_null) {
        return Ok(RangeParameters {
            start: i128::from(generate_series),
            end: 0,
            increment: 1,
            empty: true,
        });
    }
    let values: Vec<i128> = inputs
        .iter()
        .map(|value| {
            value
                .as_i128()
                .ok_or_else(|| Error::internal("range argument is not an integer"))
        })
        .collect::<Result<_>>()?;
    let (start, mut end) = match values.as_slice() {
        [end] => (0, *end),
        [start, end, ..] => (*start, *end),
        [] => return Err(Error::internal("range without arguments")),
    };
    let increment = values.get(2).copied().unwrap_or(1);
    if increment == 0 {
        return Err(Error::binder("interval cannot be 0!"));
    }
    let empty = (start > end && increment > 0) || (start < end && increment < 0);
    if generate_series {
        // generate_series includes its upper bound.
        end = if increment < 0 { end - 1 } else { end + 1 };
    }
    Ok(RangeParameters {
        start,
        end,
        increment,
        empty,
    })
}

/// DuckDB's `RangeFunction`: up to `STANDARD_VECTOR_SIZE` values per call.
fn range_scan(
    bind_data: Option<&dyn FunctionData>,
    _: &[u64],
    state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let bind_data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<RangeBindData>())
        .ok_or_else(|| Error::internal("range scan without bind data"))?;
    let data = range_parameters(&bind_data.inputs, bind_data.generate_series)?;
    let mut output = Vector::flat(LogicalType::BIGINT, STANDARD_VECTOR_SIZE);
    if data.empty {
        chunk.set_columns(vec![output], 0);
        return Ok(false);
    }
    let mut count = 0;
    {
        let values = output.values_mut::<i64>();
        let mut current = data.start + data.increment * i128::from(state.position);
        while count < STANDARD_VECTOR_SIZE {
            let finished = if data.increment > 0 {
                current >= data.end
            } else {
                current <= data.end
            };
            if finished {
                break;
            }
            // Values outside BIGINT end the sequence (DuckDB moves to the next input row).
            let Ok(value) = i64::try_from(current) else {
                break;
            };
            values[count] = value;
            count += 1;
            current += data.increment;
        }
    }
    state.position += count as u64;
    chunk.set_columns(vec![output], count);
    Ok(count == STANDARD_VECTOR_SIZE)
}

/// One integer overload of `range` / `generate_series`.
fn integer_overload(name: &'static str, arguments: usize, generate_series: bool) -> TableFunction {
    let mut function = TableFunction::new(name, vec![LogicalType::BIGINT; arguments])
        .with_bind(move |input: &TableFunctionBindInput<'_>| {
            let bind_data: Arc<dyn FunctionData> = Arc::new(RangeBindData {
                inputs: input.inputs.to_vec(),
                generate_series,
            });
            Ok(TableFunctionBindOutput {
                return_types: vec![LogicalType::BIGINT],
                names: vec![name.to_owned()],
                bind_data: Some(bind_data),
            })
        })
        .with_scan(range_scan);
    function.in_out = true;
    function
}

/// One timestamp overload (`range(TIMESTAMP, TIMESTAMP, INTERVAL)`); its rows are not
/// produced yet.
fn timestamp_overload(name: &'static str, timestamp: LogicalType) -> TableFunction {
    let return_type = timestamp.clone();
    let mut function = TableFunction::new(name, vec![timestamp.clone(), timestamp, LogicalType::INTERVAL])
        .with_bind(move |_: &TableFunctionBindInput<'_>| {
            Ok(TableFunctionBindOutput {
                return_types: vec![return_type.clone()],
                names: vec![name.to_owned()],
                bind_data: None,
            })
        })
        .with_scan(|_, _, _, _| Err(Error::not_implemented("timestamp ranges")));
    function.in_out = true;
    function
}

/// `range` or `generate_series` with all overloads, in DuckDB's order.
fn range_set(name: &'static str, generate_series: bool) -> TableFunctionSet {
    let mut set = TableFunctionSet::new(name);
    for arguments in 1..=3 {
        set.add(integer_overload(name, arguments, generate_series));
    }
    set.add(timestamp_overload(name, LogicalType::TIMESTAMP));
    set.add(timestamp_overload(name, LogicalType::TIMESTAMP_TZ));
    set
}

/// `range` and `generate_series`.
pub fn range_functions() -> Vec<TableFunctionSet> {
    vec![range_set("range", false), range_set("generate_series", true)]
}
