use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_planner::{BoundLimitNode, Expression};
use rustdb_types::{DataChunk, LogicalType, SelectionVector, Value};

use crate::operator::{ErasedOperator, ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{
    ExecutionContext, ExpressionExecutor, Operator, OperatorResult, Sink, SinkFinalize, SinkResult, Source,
    SourceResult,
};

/// The largest LIMIT/OFFSET (`PhysicalLimit::MAX_LIMIT_VALUE`).
const MAX_LIMIT_VALUE: u64 = 1 << 62;

/// Evaluates a LIMIT/OFFSET expression on the first row of `input`
/// (`PhysicalLimit::GetDelimiter`).
fn delimiter(input: &DataChunk, expression: &Expression) -> Result<Value> {
    let first_row = DataChunk::new(input.columns().to_vec(), 1);
    let mut result = DataChunk::default();
    ExpressionExecutor::new(vec![expression.clone()]).execute(&first_row, &mut result)?;
    Ok(result.columns()[0].get_value(0))
}

/// `Value::GetValue<T>`: `value` cast to `target`, failures as Invalid Input errors.
fn get_value_as(value: &Value, target: &LogicalType) -> Result<Value> {
    use rustdb_function::cast::ValueCast;
    // `GetValueInternal` reads a DECIMAL as a DOUBLE first.
    let value = if value.logical_type().id() == rustdb_types::LogicalTypeId::Decimal {
        value.try_cast(&LogicalType::DOUBLE)?
    } else {
        value.clone()
    };
    value.try_cast(target).map_err(|error| {
        if error.kind() == rustdb_common::ErrorKind::Conversion {
            Error::invalid_input(error.raw_message().to_owned())
        } else {
            error
        }
    })
}

/// A LIMIT or OFFSET value: the BIGINT result as an unsigned count (`GetValue<idx_t>`),
/// `null` when NULL.
fn count_value(input: &DataChunk, expression: &Expression, null: u64) -> Result<u64> {
    let value = delimiter(input, expression)?;
    if value.is_null() {
        return Ok(null);
    }
    // `Value::GetValue<idx_t>`: the value cast to UBIGINT (a LIMIT subquery keeps its own
    // type).
    let count = get_value_as(&value, &LogicalType::UBIGINT)?
        .as_i128()
        .and_then(|count| u64::try_from(count).ok())
        .unwrap_or_default();
    if count > MAX_LIMIT_VALUE {
        return Err(Error::binder(format!(
            "Max value {count} for LIMIT/OFFSET is {MAX_LIMIT_VALUE}"
        )));
    }
    Ok(count)
}

/// The initial limit or offset: known when constant or unset (`SetInitialLimits`).
fn initial(bound: &BoundLimitNode, unset: u64) -> Option<u64> {
    match bound {
        BoundLimitNode::Unset => Some(unset),
        BoundLimitNode::ConstantValue(value) => Some(*value),
        _ => None,
    }
}

/// Resolves a limit or offset from the first input chunk if it is an expression.
fn resolve(known: &mut Option<u64>, bound: &BoundLimitNode, input: &DataChunk, null: u64) -> Result<u64> {
    if let Some(value) = *known {
        return Ok(value);
    }
    let BoundLimitNode::ExpressionValue(expression) = bound else {
        return Err(Error::internal("unexpected LIMIT/OFFSET kind"));
    };
    let value = count_value(input, expression, null)?;
    *known = Some(value);
    Ok(value)
}

/// Rows `[first, last)` of `input` as `output`.
fn slice_into(input: &DataChunk, output: &mut DataChunk, first: u64, last: u64) {
    let count = input.len() as u64;
    if first == 0 && last == count {
        output.set_columns(input.columns().to_vec(), input.len());
    } else if first < last {
        let sel = SelectionVector::new((first as u32..last as u32).collect::<Vec<_>>());
        output.set_columns(
            input.columns().iter().map(|c| c.slice(&sel)).collect(),
            (last - first) as usize,
        );
    }
}

/// LIMIT/OFFSET over an ordered stream (DuckDB's `PhysicalStreamingLimit`). Expression
/// bounds are evaluated on the first input chunk.
#[derive(Debug)]
pub struct PhysicalStreamingLimit {
    types: Vec<LogicalType>,
    limit: BoundLimitNode,
    offset: BoundLimitNode,
    children: [PhysicalRef; 1],
}

impl PhysicalStreamingLimit {
    pub fn new(
        types: Vec<LogicalType>,
        limit: BoundLimitNode,
        offset: BoundLimitNode,
        child: PhysicalRef,
    ) -> Self {
        Self {
            types,
            limit,
            offset,
            children: [child],
        }
    }
}

impl PhysicalOperator for PhysicalStreamingLimit {
    fn name(&self) -> &'static str {
        "STREAMING_LIMIT"
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

/// The limit and offset of one thread, once known (`StreamingLimitOperatorState`).
#[derive(Debug)]
pub struct LimitBounds {
    limit: Option<u64>,
    offset: Option<u64>,
}

impl Operator for PhysicalStreamingLimit {
    /// Rows seen so far across all threads.
    type Global = AtomicU64;
    type Local = LimitBounds;

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<AtomicU64> {
        Ok(AtomicU64::new(0))
    }

    fn local_operator_state(&self, _: &ExecutionContext, _: &AtomicU64) -> Result<LimitBounds> {
        Ok(LimitBounds {
            limit: initial(&self.limit, MAX_LIMIT_VALUE),
            offset: initial(&self.offset, 0),
        })
    }

    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        seen: &AtomicU64,
        bounds: &mut LimitBounds,
    ) -> Result<OperatorResult> {
        let count = input.len() as u64;
        let start = seen.fetch_add(count, Ordering::Relaxed);
        let limit = resolve(&mut bounds.limit, &self.limit, input, MAX_LIMIT_VALUE)?;
        let offset = resolve(&mut bounds.offset, &self.offset, input, 0)?;
        let end = offset.saturating_add(limit);
        if limit == 0 || start >= end {
            return Ok(OperatorResult::Finished);
        }
        // Rows [first, last) of this chunk fall inside [offset, offset + limit).
        let first = offset.saturating_sub(start).min(count);
        let last = (end - start).min(count);
        slice_into(input, output, first, last);
        Ok(if start + count >= end {
            OperatorResult::Finished
        } else {
            OperatorResult::NeedMoreInput
        })
    }
}

/// `LIMIT n%` (DuckDB's `PhysicalLimitPercent`): buffers the rows after the offset, then
/// emits the requested share of them.
#[derive(Debug)]
pub struct PhysicalLimitPercent {
    types: Vec<LogicalType>,
    limit: BoundLimitNode,
    offset: BoundLimitNode,
    children: [PhysicalRef; 1],
}

impl PhysicalLimitPercent {
    pub fn new(
        types: Vec<LogicalType>,
        limit: BoundLimitNode,
        offset: BoundLimitNode,
        child: PhysicalRef,
    ) -> Self {
        Self {
            types,
            limit,
            offset,
            children: [child],
        }
    }
}

impl PhysicalOperator for PhysicalLimitPercent {
    fn name(&self) -> &'static str {
        "LIMIT_PERCENT"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn as_sink(&self) -> Option<&dyn ErasedSink> {
        Some(self)
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
}

/// The buffered rows and the bounds, once known (`LimitPercentGlobalState`).
#[derive(Debug, Default)]
pub struct LimitPercentGlobal {
    /// Rows seen, including those skipped by the offset.
    seen: u64,
    percent: Option<f64>,
    offset: Option<u64>,
    chunks: Vec<DataChunk>,
}

impl Sink for PhysicalLimitPercent {
    type Global = Mutex<LimitPercentGlobal>;
    type Local = ();

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        let percent = match &self.limit {
            BoundLimitNode::ConstantPercentage(percent) => Some(*percent),
            BoundLimitNode::ExpressionPercentage(_) => None,
            _ => {
                return Err(Error::internal(
                    "Unsupported type for limit value in PhysicalLimitPercent",
                ));
            }
        };
        Ok(Mutex::new(LimitPercentGlobal {
            percent,
            offset: initial(&self.offset, 0),
            ..LimitPercentGlobal::default()
        }))
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<()> {
        Ok(())
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &Self::Global,
        (): &mut (),
    ) -> Result<SinkResult> {
        let mut state = global.lock().unwrap_or_else(PoisonError::into_inner);
        if state.percent.is_none() {
            let BoundLimitNode::ExpressionPercentage(expression) = &self.limit else {
                return Err(Error::internal("unexpected LIMIT kind"));
            };
            let value = delimiter(chunk, expression)?;
            let percent = if value.is_null() {
                100.0
            } else {
                // `Value::GetValue<double>`.
                get_value_as(&value, &LogicalType::DOUBLE)?
                    .as_f64()
                    .unwrap_or_default()
            };
            if percent < 0.0 {
                return Err(Error::binder(format!(
                    "Percentage value({percent:.6}) can't be negative"
                )));
            }
            state.percent = Some(percent);
        }
        let mut offset = state.offset;
        let offset = resolve(&mut offset, &self.offset, chunk, 0)?;
        state.offset = Some(offset);
        // `HandleOffset` without a limit: keep the rows past the offset.
        let count = chunk.len() as u64;
        let start = state.seen;
        state.seen += count;
        let first = offset.saturating_sub(start);
        if first < count {
            let mut kept = DataChunk::default();
            slice_into(chunk, &mut kept, first, count);
            state.chunks.push(kept);
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, _: &Self::Global, (): ()) -> Result<()> {
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, _: &mut Self::Global) -> Result<SinkFinalize> {
        Ok(SinkFinalize::Ready)
    }
}

/// The rows to emit and the next chunk.
#[derive(Debug)]
pub struct LimitPercentSource {
    chunks: Vec<DataChunk>,
    next: AtomicUsize,
}

impl Source for PhysicalLimitPercent {
    type Global = LimitPercentSource;
    type Local = ();

    fn global_source_state(
        &self,
        _: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<LimitPercentSource> {
        let sink = sink
            .and_then(|state| state.downcast_ref::<Mutex<LimitPercentGlobal>>())
            .ok_or_else(|| Error::internal("LIMIT % source without its sink state"))?;
        let mut state = sink.lock().unwrap_or_else(PoisonError::into_inner);
        let chunks = std::mem::take(&mut state.chunks);
        let Some(percent) = state.percent else {
            // LIMIT % with an expression over an empty input.
            return Ok(LimitPercentSource {
                chunks: Vec::new(),
                next: AtomicUsize::new(0),
            });
        };
        let mut count: u64 = chunks.iter().map(|chunk| chunk.len() as u64).sum();
        if count > 0 {
            count += state.offset.unwrap_or_default();
        }
        if percent.is_nan() || !(0.0..=100.0).contains(&percent) {
            return Err(Error::out_of_range(
                "Limit percent out of range, should be between 0% and 100%",
            ));
        }
        // Truncating like C++'s `idx_t(double)`.
        let mut remaining = ((percent / 100.0 * count as f64) as u64).min(count);
        let mut emitted = Vec::new();
        for source in chunks {
            if remaining == 0 {
                break;
            }
            let take = remaining.min(source.len() as u64);
            remaining -= take;
            let mut kept = DataChunk::default();
            slice_into(&source, &mut kept, 0, take);
            emitted.push(kept);
        }
        Ok(LimitPercentSource {
            chunks: emitted,
            next: AtomicUsize::new(0),
        })
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &LimitPercentSource) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        state: &LimitPercentSource,
        (): &mut (),
    ) -> Result<SourceResult> {
        let index = state.next.fetch_add(1, Ordering::Relaxed);
        if let Some(source) = state.chunks.get(index) {
            chunk.set_columns(source.columns().to_vec(), source.len());
        }
        Ok(if index + 1 < state.chunks.len() {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }
}
