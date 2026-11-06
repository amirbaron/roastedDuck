//! The base of the aggregate evaluation strategies (DuckDB's `window_aggregator.cpp`):
//! the aggregate description, the FILTER mask and the frame splitting for `EXCLUDE`.

use std::sync::Arc;

use rustdb_ast::WindowExcludeMode;
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::aggregate::AggregateInput;
use rustdb_function::{AggregateFunction, FunctionData, StatePtr};
use rustdb_types::{LogicalType, SelectionVector, Vector};

use super::WindowExpression;
use super::boundaries_state::{FrameBounds, WindowBound, WindowBounds};
use super::shared_expressions::WindowSharedExpressions;

/// The subframes a frame splits into for an `EXCLUDE` mode (DuckDB's `InitSubFrames`).
pub(crate) fn init_subframes(exclude_mode: WindowExcludeMode) -> Vec<FrameBounds> {
    let count = match exclude_mode {
        WindowExcludeMode::NoOther => 1,
        WindowExcludeMode::CurrentRow | WindowExcludeMode::Group => 2,
        WindowExcludeMode::Ties => 3,
    };
    vec![FrameBounds::default(); count]
}

/// Splits each row's frame into subframes without the excluded rows and calls
/// `operation(i, frames)` for row `i` (DuckDB's `WindowAggregator::EvaluateSubFrames`).
///
/// The subframes are ordered, disjoint, have `start <= end`, and there are always as many
/// as [`init_subframes`] allocated.
pub(crate) fn evaluate_subframes(
    bounds: &WindowBounds,
    exclude_mode: WindowExcludeMode,
    count: usize,
    row_idx: usize,
    frames: &mut [FrameBounds],
    mut operation: impl FnMut(usize, &[FrameBounds]) -> Result<()>,
) -> Result<()> {
    let begins = &bounds[WindowBound::FrameBegin];
    let ends = &bounds[WindowBound::FrameEnd];
    let peer_begins = &bounds[WindowBound::PeerBegin];
    let peer_ends = &bounds[WindowBound::PeerEnd];
    for i in 0..count {
        let cur_row = row_idx + i;
        let (frame_begin, frame_end) = (begins[i], ends[i]);
        if exclude_mode == WindowExcludeMode::NoOther {
            frames[0] = FrameBounds::new(frame_begin, frame_end);
        } else {
            // EXCLUDE CURRENT ROW drops the row, GROUP its peer group, TIES the peers but
            // not the row itself.
            let mut nframes = 0;
            let left_end = if exclude_mode == WindowExcludeMode::CurrentRow {
                cur_row
            } else {
                peer_begins[i]
            };
            frames[nframes] = FrameBounds::new(frame_begin, left_end.min(frame_end).max(frame_begin));
            nframes += 1;
            if exclude_mode == WindowExcludeMode::Ties {
                let begin = cur_row.max(frame_begin).min(frame_end);
                let end = (cur_row + 1).min(frame_end).max(frame_begin);
                frames[nframes] = FrameBounds::new(begin, end);
                nframes += 1;
            }
            let right_begin = if exclude_mode == WindowExcludeMode::CurrentRow {
                cur_row + 1
            } else {
                peer_ends[i]
            };
            frames[nframes] = FrameBounds::new(right_begin.max(frame_begin).min(frame_end), frame_end);
        }
        operation(i, frames)?;
    }
    Ok(())
}

/// What every aggregation strategy knows about its aggregate (DuckDB's
/// `WindowAggregator` fields and `AggregateObject`).
#[derive(Clone, Debug)]
pub(crate) struct WindowAggregator {
    pub function: AggregateFunction,
    pub bind_info: Option<Arc<dyn FunctionData>>,
    /// Whether the aggregate is `DISTINCT`.
    pub distinct: bool,
    /// Whether the aggregate has a `FILTER` clause.
    pub has_filter: bool,
    pub arg_types: Vec<LogicalType>,
    pub result_type: LogicalType,
    pub exclude_mode: WindowExcludeMode,
    /// Collection (or, for the constant aggregator, sink) columns of the arguments.
    pub child_idx: Vec<usize>,
}

impl WindowAggregator {
    /// The aggregate of `wexpr`, without registering its arguments.
    pub(crate) fn new(wexpr: &WindowExpression) -> Result<Self> {
        let function = wexpr
            .window
            .aggregate
            .clone()
            .ok_or_else(|| Error::internal("window aggregate without an aggregate function"))?;
        Ok(Self {
            function,
            bind_info: wexpr.window.bind_info.clone(),
            distinct: wexpr.window.distinct,
            has_filter: wexpr.window.filter_expr.is_some(),
            arg_types: wexpr
                .window
                .children
                .iter()
                .map(|child| child.return_type.clone())
                .collect(),
            result_type: wexpr.return_type.clone(),
            exclude_mode: wexpr.window.exclude_clause,
            child_idx: Vec::new(),
        })
    }

    /// Like [`Self::new`], with the arguments materialised in the collection.
    pub(crate) fn with_collection_arguments(
        wexpr: &WindowExpression,
        shared: &mut WindowSharedExpressions,
    ) -> Result<Self> {
        let mut aggregator = Self::new(wexpr)?;
        aggregator.child_idx = wexpr
            .window
            .children
            .iter()
            .filter_map(|child| shared.register_collection(Some(child), false))
            .collect();
        Ok(aggregator)
    }

    /// The input passed to every aggregate callback.
    pub(crate) fn input(&self) -> AggregateInput<'_> {
        AggregateInput {
            bind_data: self.bind_info.as_deref(),
        }
    }
}

/// Pending aggregate work, applied in batches of up to `STANDARD_VECTOR_SIZE` like
/// DuckDB's `statep`/`statel`/`filter_sel` buffers. Operations are applied in the order
/// they were added, so batching never changes results.
#[derive(Debug, Default)]
pub(crate) struct FlushBuffer {
    /// The states to update or combine into.
    targets: Vec<StatePtr>,
    /// For updates: the input rows.
    rows: Vec<u32>,
    /// For combines: the source states.
    sources: Vec<StatePtr>,
}

impl FlushBuffer {
    /// Buffers "add input row `row` to `target`"; flushes when full.
    pub(crate) fn update(
        &mut self,
        aggregator: &WindowAggregator,
        inputs: &[Vector],
        target: StatePtr,
        row: usize,
    ) -> Result<()> {
        self.targets.push(target);
        self.rows.push(row as u32);
        if self.targets.len() >= STANDARD_VECTOR_SIZE {
            self.flush_updates(aggregator, inputs)?;
        }
        Ok(())
    }

    /// Buffers "combine `source` into `target`"; flushes when full.
    pub(crate) fn combine(
        &mut self,
        aggregator: &WindowAggregator,
        source: StatePtr,
        target: StatePtr,
    ) -> Result<()> {
        self.sources.push(source);
        self.targets.push(target);
        if self.targets.len() >= STANDARD_VECTOR_SIZE {
            self.flush_combines(aggregator)?;
        }
        Ok(())
    }

    /// Applies the buffered updates (DuckDB's `FlushStates(false)`).
    pub(crate) fn flush_updates(&mut self, aggregator: &WindowAggregator, inputs: &[Vector]) -> Result<()> {
        if self.targets.is_empty() {
            return Ok(());
        }
        let count = self.targets.len();
        let sel = SelectionVector::new(std::mem::take(&mut self.rows));
        let leaves: Vec<Vector> = inputs.iter().map(|input| input.slice(&sel)).collect();
        // SAFETY: the targets are initialised states of this aggregate owned by the
        // caller, and nothing else accesses them during the call.
        unsafe {
            aggregator
                .function
                .ops
                .update(&leaves, aggregator.input(), &self.targets, count)?;
        }
        self.targets.clear();
        Ok(())
    }

    /// Applies the buffered combines in order (DuckDB's `FlushStates(true)`).
    pub(crate) fn flush_combines(&mut self, aggregator: &WindowAggregator) -> Result<()> {
        if self.targets.is_empty() {
            return Ok(());
        }
        // SAFETY: sources and targets are initialised states of this aggregate; a source
        // is never its own target.
        unsafe {
            aggregator
                .function
                .ops
                .combine(&self.sources, &self.targets, aggregator.input())?;
        }
        self.sources.clear();
        self.targets.clear();
        Ok(())
    }
}
