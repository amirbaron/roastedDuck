//! `range` and `generate_series` (DuckDB's `function/table/range.cpp`): integer
//! sequences, and timestamp sequences with an interval step.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::{
    FunctionData, InOutResult, TableFunction, TableFunctionBindInput, TableFunctionBindOutput,
    TableFunctionSet, TableScanState,
};
use rustdb_types::{DataChunk, Interval, LogicalType, Timestamp, Value, Vector};

use crate::temporal::timestamp_add_interval;

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
        .with_scan(range_scan)
        .with_in_out(range_in_out(generate_series));
    function.in_out = true;
    function
}

/// The timestamp range of one call (DuckDB's `RangeDateTimeLocalState`), or `None` for
/// an empty range.
#[derive(Debug)]
struct TimestampRange {
    start: Timestamp,
    end: Timestamp,
    increment: Interval,
    /// Whether the range ascends (DuckDB's `greater_than_check`).
    ascending: bool,
    inclusive: bool,
}

impl TimestampRange {
    /// DuckDB's `RangeDateTimeLocalState::Finished`.
    fn finished(&self, current: Timestamp) -> bool {
        match (self.ascending, self.inclusive) {
            (true, true) => current > self.end,
            (true, false) => current >= self.end,
            (false, true) => current < self.end,
            (false, false) => current <= self.end,
        }
    }
}

/// DuckDB's `GenerateRangeDateTimeParameters`.
fn timestamp_range(inputs: &[Value], inclusive: bool) -> Result<Option<TimestampRange>> {
    if inputs.iter().any(Value::is_null) {
        return Ok(None);
    }
    let bound = |value: &Value| {
        value
            .as_timestamp()
            .ok_or_else(|| Error::internal("range bound is not a timestamp"))
    };
    let (start, end) = (bound(&inputs[0])?, bound(&inputs[1])?);
    let increment = inputs[2]
        .as_interval()
        .ok_or_else(|| Error::internal("range step is not an interval"))?;
    if !start.is_finite() || !end.is_finite() {
        return Err(Error::binder("RANGE with infinite bounds is not supported"));
    }
    if increment.months == 0 && increment.days == 0 && increment.micros == 0 {
        return Err(Error::binder("interval cannot be 0!"));
    }
    let ascending = increment.months > 0 || increment.days > 0 || increment.micros > 0;
    if ascending && (increment.months < 0 || increment.days < 0 || increment.micros < 0) {
        return Err(Error::binder(
            "RANGE with composite interval that has mixed signs is not supported",
        ));
    }
    let empty = if ascending { start > end } else { start < end };
    Ok((!empty).then_some(TimestampRange {
        start,
        end,
        increment,
        ascending,
        inclusive,
    }))
}

/// DuckDB's `RangeDateTimeFunction`: up to `STANDARD_VECTOR_SIZE` timestamps per call;
/// the scan state keeps the next timestamp.
fn timestamp_range_scan(
    bind_data: Option<&dyn FunctionData>,
    _: &[u64],
    state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let bind_data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<RangeBindData>())
        .ok_or_else(|| Error::internal("range scan without bind data"))?;
    let mut output = Vector::flat(LogicalType::TIMESTAMP, STANDARD_VECTOR_SIZE);
    let Some(range) = timestamp_range(&bind_data.inputs, bind_data.generate_series)? else {
        chunk.set_columns(vec![output], 0);
        return Ok(false);
    };
    let mut current = match state
        .local
        .as_ref()
        .and_then(|local| local.downcast_ref::<Timestamp>())
    {
        Some(&next) => next,
        None => range.start,
    };
    let mut count = 0;
    {
        let values = output.values_mut::<Timestamp>();
        while count < STANDARD_VECTOR_SIZE && !range.finished(current) {
            values[count] = current;
            count += 1;
            current = timestamp_add_interval(current, range.increment)?;
        }
    }
    state.local = Some(Box::new(current));
    state.position += count as u64;
    chunk.set_columns(vec![output], count);
    Ok(count == STANDARD_VECTOR_SIZE)
}

/// One timestamp overload (`range(TIMESTAMP, TIMESTAMP, INTERVAL)`).
fn timestamp_overload(name: &'static str, generate_series: bool) -> TableFunction {
    let timestamp = LogicalType::TIMESTAMP;
    let mut function = TableFunction::new(name, vec![timestamp.clone(), timestamp, LogicalType::INTERVAL])
        .with_bind(move |input: &TableFunctionBindInput<'_>| {
            let bind_data: Arc<dyn FunctionData> = Arc::new(RangeBindData {
                inputs: input.inputs.to_vec(),
                generate_series,
            });
            Ok(TableFunctionBindOutput {
                return_types: vec![LogicalType::TIMESTAMP],
                names: vec![name.to_owned()],
                bind_data: Some(bind_data),
            })
        })
        .with_scan(timestamp_range_scan)
        .with_in_out(timestamp_range_in_out(generate_series));
    function.in_out = true;
    function
}

/// The ICU `TIMESTAMP WITH TIME ZONE` overload; its rows are not produced (ICU is not
/// ported).
fn timestamp_tz_overload(name: &'static str) -> TableFunction {
    let timestamp = LogicalType::TIMESTAMP_TZ;
    let mut function = TableFunction::new(name, vec![timestamp.clone(), timestamp, LogicalType::INTERVAL])
        .with_bind(move |_: &TableFunctionBindInput<'_>| {
            Ok(TableFunctionBindOutput {
                return_types: vec![LogicalType::TIMESTAMP_TZ],
                names: vec![name.to_owned()],
                bind_data: None,
            })
        })
        .with_scan(|_, _, _, _| Err(Error::not_implemented("TIMESTAMP WITH TIME ZONE ranges (ICU)")));
    function.in_out = true;
    function
}

/// The progress of the in-out form of the integer `range` (DuckDB's
/// `RangeFunctionLocalState`).
#[derive(Debug, Default)]
struct RangeInOutState {
    initialized_row: bool,
    current_input_row: usize,
    current_idx: i128,
    start: i128,
    end: i128,
    increment: i128,
    empty_range: bool,
}

impl RangeInOutState {
    /// DuckDB's `GenerateRangeParameters` for input row `row`.
    fn generate_parameters(&mut self, input: &DataChunk, row: usize, generate_series: bool) -> Result<()> {
        let values: Vec<Value> = input
            .columns()
            .iter()
            .map(|column| column.get_value(row))
            .collect();
        if values.iter().any(Value::is_null) {
            // Like DuckDB, `empty_range` keeps its value here.
            self.start = i128::from(generate_series);
            self.end = 0;
            self.increment = 1;
            return Ok(());
        }
        let numbers: Vec<i128> = values
            .iter()
            .map(|value| {
                value
                    .as_i128()
                    .ok_or_else(|| Error::internal("range argument is not an integer"))
            })
            .collect::<Result<_>>()?;
        (self.start, self.end) = match numbers.as_slice() {
            [end] => (0, *end),
            [start, end, ..] => (*start, *end),
            [] => return Err(Error::internal("Unsupported parameter count for range function")),
        };
        self.increment = numbers.get(2).copied().unwrap_or(1);
        if self.increment == 0 {
            return Err(Error::binder("interval cannot be 0!"));
        }
        self.empty_range =
            (self.start > self.end && self.increment > 0) || (self.start < self.end && self.increment < 0);
        if generate_series {
            self.end = if self.increment < 0 {
                self.end - 1
            } else {
                self.end + 1
            };
        }
        Ok(())
    }
}

/// DuckDB's in-out `RangeFunction`: each input row's sequence, in sequence vectors of at
/// most `STANDARD_VECTOR_SIZE` values.
fn range_in_out(
    generate_series: bool,
) -> impl Fn(Option<&dyn FunctionData>, &mut TableScanState, &DataChunk, &mut DataChunk) -> Result<InOutResult>
+ Send
+ Sync {
    move |_, scan_state, input, output| {
        let state = scan_state
            .local
            .get_or_insert_with(|| Box::new(RangeInOutState::default()))
            .downcast_mut::<RangeInOutState>()
            .ok_or_else(|| Error::internal("range in-out state of another function"))?;
        loop {
            if !state.initialized_row {
                if state.current_input_row >= input.len() {
                    state.current_input_row = 0;
                    state.initialized_row = false;
                    output.set_columns(vec![Vector::flat(LogicalType::BIGINT, 1)], 0);
                    return Ok(InOutResult::NeedMoreInput);
                }
                state.generate_parameters(input, state.current_input_row, generate_series)?;
                state.initialized_row = true;
                state.current_idx = 0;
            }
            if state.empty_range {
                output.set_columns(vec![Vector::flat(LogicalType::BIGINT, 1)], 0);
                state.current_input_row += 1;
                state.initialized_row = false;
                return Ok(InOutResult::HaveMoreOutput);
            }
            let current = state.start + state.increment * state.current_idx;
            let Ok(first) = i64::try_from(current) else {
                state.current_input_row += 1;
                state.initialized_row = false;
                continue;
            };
            let offset = if state.increment < 0 { 1 } else { -1 };
            let remaining = ((state.end - current + (state.increment + offset)) / state.increment)
                .clamp(0, STANDARD_VECTOR_SIZE as i128);
            if remaining == 0 {
                state.current_input_row += 1;
                state.initialized_row = false;
                continue;
            }
            let increment = i64::try_from(state.increment).map_err(|_| Error::internal("range increment"))?;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "at most STANDARD_VECTOR_SIZE"
            )]
            let count = remaining as usize;
            output.set_columns(
                vec![Vector::sequence(LogicalType::BIGINT, first, increment)],
                count,
            );
            state.current_idx += remaining;
            return Ok(InOutResult::HaveMoreOutput);
        }
    }
}

/// The progress of the in-out form of the timestamp `range` (DuckDB's
/// `RangeDateTimeLocalState`).
#[derive(Debug, Default)]
struct TimestampInOutState {
    initialized_row: bool,
    current_input_row: usize,
    current: Timestamp,
    range: Option<TimestampRange>,
    empty_range: bool,
}

/// DuckDB's in-out `RangeDateTimeFunction`.
fn timestamp_range_in_out(
    generate_series: bool,
) -> impl Fn(Option<&dyn FunctionData>, &mut TableScanState, &DataChunk, &mut DataChunk) -> Result<InOutResult>
+ Send
+ Sync {
    move |_, scan_state, input, output| {
        let state = scan_state
            .local
            .get_or_insert_with(|| Box::new(TimestampInOutState::default()))
            .downcast_mut::<TimestampInOutState>()
            .ok_or_else(|| Error::internal("range in-out state of another function"))?;
        loop {
            if !state.initialized_row {
                if state.current_input_row >= input.len() {
                    state.current_input_row = 0;
                    state.initialized_row = false;
                    output.set_columns(vec![Vector::flat(LogicalType::TIMESTAMP, 1)], 0);
                    return Ok(InOutResult::NeedMoreInput);
                }
                let row = state.current_input_row;
                let values: Vec<Value> = input
                    .columns()
                    .iter()
                    .map(|column| column.get_value(row))
                    .collect();
                // `GenerateRangeDateTimeParameters`: a NULL argument gives an empty range.
                state.empty_range = false;
                state.range = if values.iter().any(Value::is_null) {
                    None
                } else {
                    let range = timestamp_range(&values, generate_series)?;
                    state.empty_range = range.is_none();
                    range
                };
                state.initialized_row = true;
                state.current = state.range.as_ref().map_or(Timestamp(0), |range| range.start);
            }
            let Some(range) = state.range.as_ref().filter(|_| !state.empty_range) else {
                output.set_columns(vec![Vector::flat(LogicalType::TIMESTAMP, 1)], 0);
                state.current_input_row += 1;
                state.initialized_row = false;
                if state.empty_range {
                    return Ok(InOutResult::HaveMoreOutput);
                }
                continue;
            };
            let mut column = Vector::flat(LogicalType::TIMESTAMP, STANDARD_VECTOR_SIZE);
            let mut size = 0;
            {
                let values = column.values_mut::<Timestamp>();
                while size < STANDARD_VECTOR_SIZE && !range.finished(state.current) {
                    values[size] = state.current;
                    size += 1;
                    state.current = timestamp_add_interval(state.current, range.increment)?;
                }
            }
            if size == 0 {
                state.current_input_row += 1;
                state.initialized_row = false;
                continue;
            }
            output.set_columns(vec![column], size);
            return Ok(InOutResult::HaveMoreOutput);
        }
    }
}

/// `range` or `generate_series` with all overloads, in DuckDB's order.
fn range_set(name: &'static str, generate_series: bool) -> TableFunctionSet {
    let mut set = TableFunctionSet::new(name);
    for arguments in 1..=3 {
        set.add(integer_overload(name, arguments, generate_series));
    }
    set.add(timestamp_overload(name, generate_series));
    set.add(timestamp_tz_overload(name));
    set
}

/// `range` and `generate_series`.
pub fn range_functions() -> Vec<TableFunctionSet> {
    vec![range_set("range", false), range_set("generate_series", true)]
}
