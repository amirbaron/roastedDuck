//! Window functions computed in one pass over the input (DuckDB's
//! `PhysicalStreamingWindow`): functions over `OVER ()` whose value for a row only
//! depends on the rows before it (or a bounded number after it, for `LEAD`).

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, PoisonError};

use rustdb_ast::{WindowBoundary, WindowExcludeMode};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::StatePtr;
use rustdb_function::cast::ValueCast;
use rustdb_planner::{Expression, ExpressionType};
use rustdb_types::{DataChunk, LogicalType, SelectionVector, Value, Vector};

use crate::aggregate_hashtable::GroupedAggregateHashTable;
use crate::operator::{ErasedOperator, PhysicalOperator, PhysicalRef};
use crate::window::WindowExpression;
use crate::window::aggregate_states::WindowAggregateStates;
use crate::window::collection::copy_into;
use crate::{ExecutionContext, ExpressionExecutor, Operator, OperatorResult};

/// The streaming window operator.
#[derive(Debug)]
pub struct PhysicalStreamingWindow {
    types: Vec<LogicalType>,
    select_list: Vec<WindowExpression>,
    children: [PhysicalRef; 1],
}

/// Rows `begin..end` of `source` copied into `target` at `offset`.
fn copy_range(source: &Vector, begin: usize, end: usize, target: &mut Vector, offset: usize) -> Result<()> {
    if end <= begin {
        return Ok(());
    }
    let sel = SelectionVector::new((begin as u32..end as u32).collect::<Vec<_>>());
    copy_into(&source.slice(&sel), end - begin, target, offset)
}

/// Rows `begin..end` of every column of `chunk` as a new flat chunk with room for
/// `capacity` rows.
fn chunk_range(chunk: &DataChunk, begin: usize, end: usize, capacity: usize) -> Result<DataChunk> {
    let mut columns = Vec::with_capacity(chunk.column_count());
    for column in chunk.columns() {
        let mut target = Vector::flat(column.logical_type().clone(), capacity.max(1));
        copy_range(column, begin, end, &mut target, 0)?;
        columns.push(target);
    }
    Ok(DataChunk::new(columns, end.saturating_sub(begin)))
}

/// The constant offset and default of a streamable LEAD/LAG (DuckDB's
/// `LeadLagState::ComputeOffset` / `ComputeDefault`).
#[derive(Clone, Debug)]
struct LeadLagParameters {
    /// Rows behind (LAG, positive) or ahead (LEAD, negative).
    offset: i64,
    default: Value,
}

impl LeadLagParameters {
    const MAX_BUFFER: i64 = STANDARD_VECTOR_SIZE as i64;

    fn compute(wexpr: &WindowExpression) -> Result<Option<Self>> {
        let window = &wexpr.window;
        let mut offset = 1;
        if let Some(expression) = &window.offset_expr {
            if !expression.is_foldable() {
                return Ok(None);
            }
            let value = ExpressionExecutor::evaluate_scalar(expression)?;
            if value.is_null() {
                return Ok(None);
            }
            let Ok(value) = value.default_cast_as(&LogicalType::BIGINT) else {
                return Ok(None);
            };
            let Some(value) = value.as_i128().and_then(|value| i64::try_from(value).ok()) else {
                return Ok(None);
            };
            offset = value;
        }
        // Only offsets within one vector can be streamed.
        if wexpr.expression_type == ExpressionType::WindowLead {
            offset = offset.wrapping_neg();
        }
        if offset.unsigned_abs() >= Self::MAX_BUFFER.unsigned_abs() {
            return Ok(None);
        }
        let default = match &window.default_expr {
            None => Value::null(wexpr.return_type.clone()),
            Some(expression) => {
                if !expression.is_foldable() {
                    return Ok(None);
                }
                let value = ExpressionExecutor::evaluate_scalar(expression)?;
                match value.default_cast_as(&wexpr.return_type) {
                    Ok(value) => value,
                    Err(_) => return Ok(None),
                }
            }
        };
        Ok(Some(Self { offset, default }))
    }
}

/// A running aggregate (DuckDB's `StreamingWindowState::AggregateState`).
#[derive(Debug)]
struct AggregateState {
    state: WindowAggregateStates,
    arguments: Option<ExpressionExecutor>,
    filter: Option<ExpressionExecutor>,
    /// Rows counted so far by `COUNT(*)`.
    unfiltered: i64,
    distinct: Option<GroupedAggregateHashTable>,
}

/// LEAD/LAG over a sliding buffer (DuckDB's `StreamingWindowState::LeadLagState`).
#[derive(Debug)]
struct LeadLagState {
    offset: i64,
    buffered: usize,
    executor: ExpressionExecutor,
    /// The previous values (LAG) or the default values (LEAD).
    prev: Vector,
}

/// The state of one function.
#[derive(Debug)]
enum FunctionState {
    /// A value fixed by the first rows (`rank`, `first_value`, ...).
    Constant(Vector),
    Aggregate(Box<AggregateState>),
    LeadLag(LeadLagState),
    /// Computed from the chunk alone (`row_number`, `last_value`).
    Stateless,
}

/// The operator's state (DuckDB keeps a single local state in the global state, since
/// streaming requires one ordered input).
#[derive(Debug)]
pub struct StreamingWindowState {
    functions: Option<Vec<FunctionState>>,
    /// The number of rows LEAD needs to look ahead.
    lead_count: usize,
    /// Rows held back for LEAD.
    delayed: Option<DataChunk>,
}

/// The shared state: the next row number and the function states.
#[derive(Debug)]
pub struct StreamingWindowGlobalState {
    row_number: AtomicI64,
    state: Mutex<StreamingWindowState>,
}

impl PhysicalStreamingWindow {
    pub fn new(types: Vec<LogicalType>, select_list: Vec<Expression>, child: PhysicalRef) -> Result<Self> {
        Ok(Self {
            types,
            select_list: select_list
                .into_iter()
                .map(WindowExpression::from_expression)
                .collect::<Result<_>>()?,
            children: [child],
        })
    }

    /// Whether `expression` can be computed in one pass (DuckDB's `IsStreamingFunction`).
    pub fn is_streaming_function(expression: &Expression) -> Result<bool> {
        let wexpr = WindowExpression::from_expression(expression.clone())?;
        let window = &wexpr.window;
        if !window.partitions.is_empty()
            || !window.orders.is_empty()
            || !window.arg_orders.is_empty()
            || window.exclude_clause != WindowExcludeMode::NoOther
        {
            return Ok(false);
        }
        let running = window.start == WindowBoundary::UnboundedPreceding
            && window.end == WindowBoundary::CurrentRowRows;
        Ok(match wexpr.expression_type {
            ExpressionType::WindowAggregate => {
                // Aggregates with destructors are too slow to update and finalize per row.
                let destructor = window
                    .aggregate
                    .as_ref()
                    .is_some_and(|aggregate| aggregate.ops.needs_destructor());
                !destructor && running
            }
            ExpressionType::WindowPercentRank
            | ExpressionType::WindowRank
            | ExpressionType::WindowRankDense
            | ExpressionType::WindowRowNumber => true,
            // Ignoring NULLs streams only for running frames.
            ExpressionType::WindowFirstValue => !window.ignore_nulls || running,
            ExpressionType::WindowLastValue => running,
            ExpressionType::WindowLag | ExpressionType::WindowLead => {
                !window.ignore_nulls && LeadLagParameters::compute(&wexpr)?.is_some()
            }
            _ => false,
        })
    }

    fn input_width(&self) -> usize {
        self.children[0].types().len()
    }

    /// Creates the function states from the first input chunk (DuckDB's `Initialize`).
    fn initialize(
        &self,
        ctx: &ExecutionContext,
        state: &mut StreamingWindowState,
        input: &DataChunk,
    ) -> Result<()> {
        let mut functions = Vec::with_capacity(self.select_list.len());
        for wexpr in &self.select_list {
            let window = &wexpr.window;
            let function = match wexpr.expression_type {
                ExpressionType::WindowAggregate => {
                    let aggregate = window
                        .aggregate
                        .clone()
                        .ok_or_else(|| Error::internal("window aggregate without a function"))?;
                    let arg_types: Vec<LogicalType> = window
                        .children
                        .iter()
                        .map(|child| child.return_type.clone())
                        .collect();
                    let distinct = if window.distinct {
                        Some(GroupedAggregateHashTable::new(
                            ctx.env.buffer_manager.clone(),
                            arg_types.clone(),
                            &[],
                            STANDARD_VECTOR_SIZE,
                            0,
                        )?)
                    } else {
                        None
                    };
                    FunctionState::Aggregate(Box::new(AggregateState {
                        state: WindowAggregateStates::new(aggregate.ops.clone(), 1)?,
                        arguments: (!window.children.is_empty())
                            .then(|| ExpressionExecutor::new(window.children.clone())),
                        filter: window
                            .filter_expr
                            .as_ref()
                            .map(|filter| ExpressionExecutor::new(vec![filter.clone()])),
                        unfiltered: 0,
                        distinct,
                    }))
                }
                ExpressionType::WindowFirstValue | ExpressionType::WindowLastValue => {
                    // The first row's value.
                    let mut executor =
                        ExpressionExecutor::new(window.children.iter().take(1).cloned().collect());
                    let mut values = DataChunk::default();
                    executor.execute(input, &mut values)?;
                    FunctionState::Constant(Vector::constant(&values.column(0).get_value(0))?)
                }
                ExpressionType::WindowPercentRank => {
                    FunctionState::Constant(Vector::constant(&Value::double(0.0))?)
                }
                ExpressionType::WindowRank | ExpressionType::WindowRankDense => {
                    FunctionState::Constant(Vector::constant(&Value::bigint(1))?)
                }
                ExpressionType::WindowLag | ExpressionType::WindowLead => {
                    let parameters = LeadLagParameters::compute(wexpr)?
                        .ok_or_else(|| Error::internal("non-streamable LEAD/LAG in a streaming window"))?;
                    let buffered = parameters.offset.unsigned_abs() as usize;
                    if parameters.offset < 0 {
                        state.lead_count = state.lead_count.max(buffered);
                    }
                    let prev = Vector::constant(&parameters.default)?.flattened(buffered.max(1));
                    FunctionState::LeadLag(LeadLagState {
                        offset: parameters.offset,
                        buffered,
                        executor: ExpressionExecutor::new(window.children.iter().take(1).cloned().collect()),
                        prev,
                    })
                }
                _ => FunctionState::Stateless,
            };
            functions.push(function);
        }
        state.functions = Some(functions);
        Ok(())
    }

    /// Computes the function columns of `output` (whose first columns are the input);
    /// `delayed` holds the rows after it, for LEAD (DuckDB's `ExecuteFunctions`).
    fn execute_functions(
        &self,
        global: &StreamingWindowGlobalState,
        state: &mut StreamingWindowState,
        output: &mut DataChunk,
        delayed: &DataChunk,
    ) -> Result<()> {
        let count = output.len();
        let input = DataChunk::new(output.columns()[..self.input_width()].to_vec(), count);
        let functions = state
            .functions
            .as_mut()
            .ok_or_else(|| Error::internal("streaming window used before initialization"))?;
        let mut columns = input.columns().to_vec();
        for (wexpr, function) in self.select_list.iter().zip(functions.iter_mut()) {
            let result = match (wexpr.expression_type, function) {
                (ExpressionType::WindowAggregate, FunctionState::Aggregate(aggregate)) => {
                    aggregate.execute(wexpr, &input)?
                }
                (ExpressionType::WindowFirstValue, FunctionState::Constant(prev)) => {
                    Self::first_value(wexpr, prev, &input)?
                }
                (ExpressionType::WindowLastValue, FunctionState::Constant(prev)) => {
                    Self::last_value(wexpr, prev, &input)?
                }
                (_, FunctionState::Constant(value)) => value.clone(),
                (ExpressionType::WindowRowNumber, _) => {
                    let start = global.row_number.load(Ordering::Relaxed);
                    let mut result = Vector::flat(LogicalType::BIGINT, count);
                    for (i, value) in result.values_mut::<i64>()[..count].iter_mut().enumerate() {
                        *value = start + i as i64;
                    }
                    result
                }
                (_, FunctionState::LeadLag(lead_lag)) => lead_lag.execute(wexpr, &input, delayed)?,
                (other, _) => {
                    return Err(Error::not_implemented(format!(
                        "{} for StreamingWindow",
                        other.name()
                    )));
                }
            };
            columns.push(result);
        }
        output.set_columns(columns, count);
        global.row_number.fetch_add(count as i64, Ordering::Relaxed);
        Ok(())
    }

    fn first_value(wexpr: &WindowExpression, prev: &mut Vector, input: &DataChunk) -> Result<Vector> {
        if !wexpr.ignore_nulls() || !prev.is_null(0) {
            return Ok(prev.clone());
        }
        // Ignoring NULLs after a NULL start: look for the first non-NULL value.
        let count = input.len();
        let mut executor = ExpressionExecutor::new(wexpr.window.children.iter().take(1).cloned().collect());
        let mut values = DataChunk::default();
        executor.execute(input, &mut values)?;
        let argument = values.column(0);
        let Some(first) = (0..count).find(|&row| !argument.is_null(row)) else {
            return Ok(prev.clone());
        };
        let value = argument.get_value(first);
        *prev = Vector::constant(&value)?;
        // NULL before the first non-NULL row, its value from there on.
        let mut result = prev.flattened(count);
        let null = Value::null(argument.logical_type().clone());
        for row in 0..first {
            result.set_value(row, &null)?;
        }
        Ok(result)
    }

    fn last_value(wexpr: &WindowExpression, prev: &mut Vector, input: &DataChunk) -> Result<Vector> {
        let count = input.len();
        let mut executor = ExpressionExecutor::new(wexpr.window.children.iter().take(1).cloned().collect());
        let mut values = DataChunk::default();
        executor.execute(input, &mut values)?;
        let argument = values.column(0).flattened(count);
        if !wexpr.ignore_nulls() {
            return Ok(argument);
        }
        // Each row takes the last non-NULL value so far (or the previous chunk's).
        let mut rows = Vec::with_capacity(count);
        let mut last: Option<u32> = None;
        for row in 0..count {
            if !argument.is_null(row) {
                last = Some(row as u32);
            }
            rows.push(last);
        }
        let mut result = argument.select_rows(&rows);
        if !prev.is_null(0) {
            let previous = prev.get_value(0);
            for (row, _) in rows.iter().enumerate().take_while(|(_, source)| source.is_none()) {
                result.set_value(row, &previous)?;
            }
        }
        if count > 0 {
            *prev = Vector::constant(&result.get_value(count - 1))?;
        }
        Ok(result)
    }
}

impl AggregateState {
    /// Updates and finalizes the running aggregate row by row (DuckDB's
    /// `AggregateState::Execute`).
    fn execute(&mut self, wexpr: &WindowExpression, input: &DataChunk) -> Result<Vector> {
        let count = input.len();
        let aggregate = wexpr
            .window
            .aggregate
            .as_ref()
            .ok_or_else(|| Error::internal("window aggregate without a function"))?;
        let bind = rustdb_function::aggregate::AggregateInput {
            bind_data: wexpr.window.bind_info.as_deref(),
        };
        // The FILTER mask.
        let mut passes = vec![true; count];
        let mut filtered: Vec<u32> = (0..count as u32).collect();
        if let Some(filter) = &mut self.filter {
            filter.select(input, &mut filtered)?;
            passes.fill(false);
            for &row in &filtered {
                passes[row as usize] = true;
            }
        }
        // COUNT(*)
        let Some(arguments) = &mut self.arguments else {
            let mut result = Vector::flat(LogicalType::BIGINT, count);
            for (row, value) in result.values_mut::<i64>()[..count].iter_mut().enumerate() {
                self.unfiltered += i64::from(passes[row]);
                *value = self.unfiltered;
            }
            return Ok(result);
        };
        let mut values = DataChunk::default();
        arguments.execute(input, &mut values)?;
        let columns: Vec<Vector> = values
            .columns()
            .iter()
            .map(|column| column.flattened(count))
            .collect();
        // DISTINCT: only rows that create a new group count.
        if let Some(table) = &mut self.distinct {
            let sel = SelectionVector::new(filtered.clone());
            let candidates = DataChunk::new(
                columns.iter().map(|column| column.slice(&sel)).collect(),
                filtered.len(),
            );
            let mut new_rows = Vec::new();
            if !candidates.is_empty() {
                let column_refs: Vec<&Vector> = candidates.columns().iter().collect();
                let hashes = rustdb_row::hash::hash_columns(&column_refs, candidates.len())?;
                table.find_or_create_groups(&candidates, &hashes)?;
                new_rows.extend(table.new_group_rows().iter().map(|&row| filtered[row as usize]));
            }
            passes.fill(false);
            for row in new_rows {
                passes[row as usize] = true;
            }
        }
        let state: StatePtr = self.state.state(0);
        let mut result = Vector::flat(wexpr.return_type.clone(), count);
        for (row, &pass) in passes.iter().enumerate() {
            if pass {
                let sel = SelectionVector::new(vec![row as u32]);
                let leaves: Vec<Vector> = columns.iter().map(|column| column.slice(&sel)).collect();
                // SAFETY: the state is initialised and owned by this operator.
                unsafe { aggregate.ops.update(&leaves, bind, &[state], 1)? };
            }
            // SAFETY: as above.
            unsafe { aggregate.ops.finalize(&[state], bind, &mut result, row)? };
        }
        Ok(result)
    }
}

impl LeadLagState {
    fn execute(
        &mut self,
        wexpr: &WindowExpression,
        input: &DataChunk,
        delayed: &DataChunk,
    ) -> Result<Vector> {
        let count = input.len();
        let mut result = Vector::flat(wexpr.return_type.clone(), count.max(1));
        if self.offset >= 0 {
            self.execute_lag(input, &mut result)?;
        } else {
            self.execute_lead(input, delayed, &mut result)?;
        }
        Ok(result)
    }

    fn evaluate(&mut self, chunk: &DataChunk) -> Result<Vector> {
        let mut values = DataChunk::default();
        self.executor.execute(chunk, &mut values)?;
        Ok(values.column(0).flattened(chunk.len()))
    }

    fn execute_lag(&mut self, input: &DataChunk, result: &mut Vector) -> Result<()> {
        let curr = self.evaluate(input)?;
        let count = input.len();
        let buffered = self.buffered;
        // prev[0, buffered) => result[0, ..)
        copy_range(&self.prev, 0, buffered.min(count), result, 0)?;
        let mut prev = Vector::flat(curr.logical_type().clone(), buffered.max(1));
        if count < buffered {
            // Shift the buffer down and append the input.
            copy_range(&self.prev, count, buffered, &mut prev, 0)?;
            copy_range(&curr, 0, count, &mut prev, buffered - count)?;
        } else {
            // curr[0, count - buffered) => result[buffered, count), keep the tail.
            copy_range(&curr, 0, count - buffered, result, buffered)?;
            copy_range(&curr, count - buffered, count, &mut prev, 0)?;
        }
        self.prev = prev;
        Ok(())
    }

    fn execute_lead(&mut self, input: &DataChunk, delayed: &DataChunk, result: &mut Vector) -> Result<()> {
        // input || delayed is one logical buffer.
        let count = input.len();
        let mut pos = 0;
        let mut unified_offset = self.buffered;
        if unified_offset < count {
            let curr = self.evaluate(input)?;
            copy_range(&curr, unified_offset, count, result, pos)?;
            pos += count - unified_offset;
            unified_offset = count;
        }
        let unified_count = count + delayed.len();
        if unified_offset < unified_count {
            let curr = self.evaluate(delayed)?;
            let delayed_offset = unified_offset - count;
            // Only copy as many values as we need.
            let delayed_count = delayed.len().min(delayed_offset + (count - pos));
            copy_range(&curr, delayed_offset, delayed_count, result, pos)?;
            pos += delayed_count - delayed_offset;
        }
        // The default for the rest.
        if pos < count {
            copy_range(&self.prev, 0, count - pos, result, pos)?;
        }
        Ok(())
    }
}

impl PhysicalOperator for PhysicalStreamingWindow {
    fn name(&self) -> &'static str {
        "STREAMING_WINDOW"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn as_operator(&self) -> Option<&dyn ErasedOperator> {
        Some(self)
    }
}

impl PhysicalStreamingWindow {
    /// Emits `delayed[0, out)` and keeps `delayed[out, ..) ++ input` (DuckDB's
    /// `ExecuteShifted`).
    fn execute_shifted(
        &self,
        global: &StreamingWindowGlobalState,
        state: &mut StreamingWindowState,
        input: &DataChunk,
        output: &mut DataChunk,
        out: usize,
    ) -> Result<()> {
        let shifted = state.delayed.take().unwrap_or_default();
        let delay = shifted.len();
        let capacity = delay - out + input.len() + STANDARD_VECTOR_SIZE;
        let mut delayed = chunk_range(&shifted, out, delay, capacity)?;
        for column in 0..input.column_count() {
            copy_range(
                input.column(column),
                0,
                input.len(),
                delayed.column_mut(column),
                delay - out,
            )?;
        }
        delayed.set_len(delay - out + input.len());
        let mut emitted = chunk_range(&shifted, 0, out, out)?;
        self.execute_functions(global, state, &mut emitted, &delayed)?;
        *output = emitted;
        state.delayed = Some(delayed);
        Ok(())
    }

    /// Emits all delayed rows; `input` follows them (DuckDB's `ExecuteDelayed`).
    fn execute_delayed(
        &self,
        global: &StreamingWindowGlobalState,
        state: &mut StreamingWindowState,
        input: &DataChunk,
        output: &mut DataChunk,
    ) -> Result<()> {
        let mut emitted = state.delayed.take().unwrap_or_default();
        self.execute_functions(global, state, &mut emitted, input)?;
        *output = emitted;
        Ok(())
    }
}

impl Operator for PhysicalStreamingWindow {
    type Global = StreamingWindowGlobalState;
    type Local = ();

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<StreamingWindowGlobalState> {
        Ok(StreamingWindowGlobalState {
            row_number: AtomicI64::new(1),
            state: Mutex::new(StreamingWindowState {
                functions: None,
                lead_count: 0,
                delayed: None,
            }),
        })
    }

    fn local_operator_state(&self, _: &ExecutionContext, _: &StreamingWindowGlobalState) -> Result<()> {
        Ok(())
    }

    fn execute(
        &self,
        ctx: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        global: &StreamingWindowGlobalState,
        (): &mut (),
    ) -> Result<OperatorResult> {
        let mut state = global.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.functions.is_none() {
            self.initialize(ctx, &mut state, input)?;
        }
        let delayed_len = state.delayed.as_ref().map_or(0, DataChunk::len);
        if delayed_len < state.lead_count {
            // Not enough rows to produce one: delay the input and ask for more.
            let capacity = delayed_len + input.len() + STANDARD_VECTOR_SIZE;
            let previous = state.delayed.take().unwrap_or_default();
            let mut delayed = if previous.column_count() == 0 {
                chunk_range(input, 0, 0, capacity)?
            } else {
                chunk_range(&previous, 0, delayed_len, capacity)?
            };
            for column in 0..input.column_count() {
                copy_range(
                    input.column(column),
                    0,
                    input.len(),
                    delayed.column_mut(column),
                    delayed_len,
                )?;
            }
            delayed.set_len(delayed_len + input.len());
            state.delayed = Some(delayed);
            output.set_len(0);
            return Ok(OperatorResult::NeedMoreInput);
        }
        if input.len() < delayed_len {
            // Emit as many delayed rows as there are new ones, delay the rest.
            self.execute_shifted(global, &mut state, input, output, input.len())?;
            return Ok(OperatorResult::NeedMoreInput);
        }
        if delayed_len > 0 {
            // Flush the delayed rows, then come back for the input.
            self.execute_delayed(global, &mut state, input, output)?;
            return Ok(OperatorResult::HaveMoreOutput);
        }
        // Nothing delayed: emit the input.
        let mut emitted = input.clone();
        self.execute_functions(global, &mut state, &mut emitted, &DataChunk::default())?;
        *output = emitted;
        Ok(OperatorResult::NeedMoreInput)
    }

    fn requires_final_execute(&self) -> bool {
        true
    }

    fn final_execute(
        &self,
        _: &ExecutionContext,
        output: &mut DataChunk,
        global: &StreamingWindowGlobalState,
        (): &mut (),
    ) -> Result<bool> {
        let mut state = global.state.lock().unwrap_or_else(PoisonError::into_inner);
        let delayed_len = state.delayed.as_ref().map_or(0, DataChunk::len);
        if state.functions.is_none() || state.lead_count == 0 || delayed_len == 0 {
            return Ok(false);
        }
        // No more input: the rows after the last delayed ones are missing.
        let empty = chunk_range(state.delayed.as_ref().unwrap_or(&DataChunk::default()), 0, 0, 1)?;
        if delayed_len > STANDARD_VECTOR_SIZE {
            self.execute_shifted(global, &mut state, &empty, output, STANDARD_VECTOR_SIZE)?;
            return Ok(true);
        }
        self.execute_delayed(global, &mut state, &empty, output)?;
        Ok(false)
    }
}
