//! DuckDB's `icu-table-range.cpp`: the table functions `range` / `generate_series` over
//! TIMESTAMPTZ, stepping on the session's calendar (`ICUTableRange`).

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::{
    FunctionData, InOutResult, TableFunction, TableFunctionBindInput, TableFunctionBindOutput,
    TableFunctionSet, TableScanState,
};
use rustdb_types::{DataChunk, Interval, LogicalType, Timestamp, Value, Vector};

use crate::icu_dateadd::add;
use crate::icu_datefunc::{TzCalendar, create_calendar_with_cutover, session_settings};
use crate::icu4c::calendar::Calendar;
use crate::icu4c::timezone::TimeZone;

/// `ICUTableRange::ICURangeBindData`.
#[derive(Debug)]
struct IcuRangeBindData {
    cal_setting: String,
    calendar: Calendar,
    cardinality: u64,
    /// The constant arguments, the input row of a call that is not lateral.
    inputs: Vec<Value>,
}

impl FunctionData for IcuRangeBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl IcuRangeBindData {
    /// `ICURangeBindData(ClientContext &context, const vector<Value> &inputs)`: the calendar
    /// keeps ICU's default Julian/Gregorian cutover.
    fn new(client: &rustdb_function::FunctionClient, inputs: &[Value]) -> Self {
        let (tz_setting, cal_setting) = session_settings(client);
        let calendar = create_calendar_with_cutover(TimeZone::create(&tz_setting), &cal_setting, false);
        let mut result = Self {
            cal_setting,
            calendar,
            cardinality: 0,
            inputs: inputs.to_vec(),
        };
        let mut bounds = [Timestamp(0); 2];
        let mut step = Interval::default();
        for (i, input) in inputs.iter().enumerate() {
            if input.is_null() {
                return result;
            }
            if i >= 2 {
                step = input.as_interval().unwrap_or_default();
            } else {
                bounds[i] = input.as_timestamp().unwrap_or_default();
            }
        }
        // Estimate cardinality using micros.
        let Some(increment) = step.total_micros().filter(|&increment| increment != 0) else {
            return result;
        };
        let Some(delta) = bounds[1].0.checked_sub(bounds[0].0) else {
            return result;
        };
        #[allow(clippy::cast_sign_loss, reason = "DuckDB converts the quotient to idx_t")]
        {
            result.cardinality = (delta / increment) as u64;
        }
        result
    }
}

/// `ICUTableRange::ICURangeLocalState`.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
struct IcuRangeLocalState {
    initialized_row: bool,
    current_input_row: usize,
    current_state: Timestamp,
    start: Timestamp,
    end: Timestamp,
    increment: Interval,
    inclusive_bound: bool,
    greater_than_check: bool,
    empty_range: bool,
}

impl IcuRangeLocalState {
    /// `ICURangeLocalState::Finished`.
    fn finished(&self, current_value: Timestamp) -> bool {
        match (self.greater_than_check, self.inclusive_bound) {
            (true, true) => current_value > self.end,
            (true, false) => current_value >= self.end,
            (false, true) => current_value < self.end,
            (false, false) => current_value <= self.end,
        }
    }

    /// `ICUTableRange::GenerateRangeDateTimeParameters`. Like DuckDB, an empty range
    /// leaves `empty_range` set for the following rows.
    fn generate_parameters(&mut self, input: &DataChunk, row_id: usize, generate_series: bool) -> Result<()> {
        let values: Vec<Value> = input
            .columns()
            .iter()
            .map(|column| column.get_value(row_id))
            .collect();
        if values.iter().any(Value::is_null) {
            self.start = Timestamp(0);
            self.end = Timestamp(0);
            self.increment = Interval::default();
            self.greater_than_check = true;
            self.inclusive_bound = false;
            return Ok(());
        }
        self.start = values[0].as_timestamp().unwrap_or_default();
        self.end = values[1].as_timestamp().unwrap_or_default();
        self.increment = values[2].as_interval().unwrap_or_default();

        // Infinities either cause errors or infinite loops, so just ban them
        if !self.start.is_finite() || !self.end.is_finite() {
            return Err(Error::binder("RANGE with infinite bounds is not supported"));
        }
        let increment = self.increment;
        if increment.months == 0 && increment.days == 0 && increment.micros == 0 {
            return Err(Error::binder("interval cannot be 0!"));
        }
        // all elements should point in the same direction
        if increment.months > 0 || increment.days > 0 || increment.micros > 0 {
            if increment.months < 0 || increment.days < 0 || increment.micros < 0 {
                return Err(Error::binder(
                    "RANGE with composite interval that has mixed signs is not supported",
                ));
            }
            self.greater_than_check = true;
            if self.start > self.end {
                self.empty_range = true;
            }
        } else {
            self.greater_than_check = false;
            if self.start < self.end {
                self.empty_range = true;
            }
        }
        self.inclusive_bound = generate_series;
        Ok(())
    }
}

fn range_bind_data(bind_data: Option<&dyn FunctionData>) -> Result<&IcuRangeBindData> {
    bind_data
        .and_then(|data| data.as_any().downcast_ref::<IcuRangeBindData>())
        .ok_or_else(|| Error::internal("ICU range without bind data"))
}

/// `ICUTableRange::ICUTableRangeFunction`.
fn table_range_function(
    generate_series: bool,
) -> impl Fn(Option<&dyn FunctionData>, &mut TableScanState, &DataChunk, &mut DataChunk) -> Result<InOutResult>
+ Send
+ Sync
+ Clone {
    move |bind_data, scan_state, input, output| {
        let bind_data = range_bind_data(bind_data)?;
        let mut calendar = TzCalendar::new(&bind_data.calendar, &bind_data.cal_setting);
        let state = scan_state
            .local
            .get_or_insert_with(|| Box::new(IcuRangeLocalState::default()))
            .downcast_mut::<IcuRangeLocalState>()
            .ok_or_else(|| Error::internal("ICU range state of another function"))?;
        loop {
            if !state.initialized_row {
                // initialize for the current input row
                if state.current_input_row >= input.len() {
                    // ran out of rows
                    state.current_input_row = 0;
                    state.initialized_row = false;
                    output.set_columns(vec![Vector::flat(LogicalType::TIMESTAMP_TZ, 1)], 0);
                    return Ok(InOutResult::NeedMoreInput);
                }
                state.generate_parameters(input, state.current_input_row, generate_series)?;
                state.initialized_row = true;
                state.current_state = state.start;
            }
            if state.empty_range {
                // empty range
                output.set_columns(vec![Vector::flat(LogicalType::TIMESTAMP_TZ, 1)], 0);
                state.current_input_row += 1;
                state.initialized_row = false;
                return Ok(InOutResult::HaveMoreOutput);
            }
            let mut column = Vector::flat(LogicalType::TIMESTAMP_TZ, STANDARD_VECTOR_SIZE);
            let mut size = 0;
            {
                let data = column.values_mut::<Timestamp>();
                loop {
                    if state.finished(state.current_state) {
                        break;
                    }
                    data[size] = state.current_state;
                    size += 1;
                    state.current_state = add(&mut calendar, state.current_state, state.increment)?;
                    if size >= STANDARD_VECTOR_SIZE {
                        break;
                    }
                }
            }
            if size == 0 {
                // move to next row
                state.current_input_row += 1;
                state.initialized_row = false;
                continue;
            }
            output.set_columns(vec![column], size);
            return Ok(InOutResult::HaveMoreOutput);
        }
    }
}

/// The in-out function run over the call's constant arguments (a call that is not
/// lateral has them as its single input row).
#[derive(Debug)]
struct ConstantScan {
    inner: TableScanState,
    input: DataChunk,
    done: bool,
}

/// `ICUTableRange::Bind`.
fn bind(generate_series: bool) -> impl Fn(&TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    move |input| {
        let context = input
            .binder
            .and_then(|binder| binder.downcast_ref::<rustdb_planner::ClientContext>())
            .ok_or_else(|| Error::internal("ICU range bound without a client context"))?;
        let result = IcuRangeBindData::new(&context.function_client, input.inputs);
        Ok(TableFunctionBindOutput {
            return_types: vec![LogicalType::TIMESTAMP_TZ],
            names: vec![
                if generate_series {
                    "generate_series"
                } else {
                    "range"
                }
                .to_owned().into(),
            ],
            bind_data: Some(Arc::new(result)),
        })
    }
}

/// One `(TIMESTAMPTZ, TIMESTAMPTZ, INTERVAL)` overload.
fn range_function(name: &str, generate_series: bool) -> TableFunction {
    let in_out = table_range_function(generate_series);
    let scan_in_out = in_out.clone();
    let mut function = TableFunction::new(
        name,
        vec![
            LogicalType::TIMESTAMP_TZ,
            LogicalType::TIMESTAMP_TZ,
            LogicalType::INTERVAL,
        ],
    )
    .with_bind(bind(generate_series))
    .with_scan(move |bind_data, _, state, chunk| {
        let data = range_bind_data(bind_data)?;
        if state.local.is_none() {
            let columns = data
                .inputs
                .iter()
                .map(Vector::constant)
                .collect::<Result<Vec<_>>>()?;
            state.local = Some(Box::new(ConstantScan {
                inner: TableScanState::default(),
                input: DataChunk::new(columns, 1),
                done: false,
            }));
        }
        let scan = state
            .local
            .as_mut()
            .and_then(|local| local.downcast_mut::<ConstantScan>())
            .ok_or_else(|| Error::internal("ICU range scan state of another function"))?;
        loop {
            if scan.done {
                chunk.set_columns(vec![Vector::flat(LogicalType::TIMESTAMP_TZ, 1)], 0);
                return Ok(false);
            }
            match scan_in_out(bind_data, &mut scan.inner, &scan.input, chunk)? {
                InOutResult::HaveMoreOutput if !chunk.is_empty() => return Ok(true),
                InOutResult::HaveMoreOutput => {}
                InOutResult::NeedMoreInput | InOutResult::Finished => scan.done = true,
            }
        }
    })
    .with_in_out(in_out)
    .with_cardinality(|bind_data| {
        bind_data
            .and_then(|data| data.as_any().downcast_ref::<IcuRangeBindData>())
            .map(|data| data.cardinality)
    });
    function.in_out = true;
    function
}

/// `ICUTableRange::AddICUTableRangeFunction`: `range` and `generate_series`.
pub(crate) fn table_range_functions() -> Vec<TableFunctionSet> {
    let mut range = TableFunctionSet::new("range");
    range.add(range_function("range", false));
    let mut generate_series = TableFunctionSet::new("generate_series");
    generate_series.add(range_function("generate_series", true));
    vec![range, generate_series]
}
