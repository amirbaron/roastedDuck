//! The nested loop join (DuckDB's `PhysicalNestedLoopJoin`): joins on comparisons
//! without an equality by comparing every left row with every right row, right rows of a
//! right chunk outer and left rows inner.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::JoinType;
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::Expression;
use rustdb_planner::operator::JoinCondition;
use rustdb_row::AppendState;
use rustdb_types::{DataChunk, LogicalType, PhysicalType};

use super::{
    OuterJoinMarker, construct_empty_join_result, construct_mark_join_result, construct_semi_or_anti_join_result,
    empty_result_if_rhs_is_empty, has_null_values, is_left_outer_join, propagates_build_side, reorder_conditions,
    slice_columns,
};
use crate::column_data_collection::ColumnDataCollection;
use crate::join_hashtable::flatten_sequences;
use crate::nested_loop_join::{NestedLoopPosition, perform_inner, perform_mark};
use crate::operator::{ErasedOperator, ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{
    ExecutionContext, ExpressionExecutor, Operator, OperatorResult, Sink, SinkFinalize, SinkResult, Source,
    SourceResult,
};

/// A join on comparisons that compares every pair of rows; see the module docs.
#[derive(Debug)]
pub struct PhysicalNestedLoopJoin {
    types: Vec<LogicalType>,
    join_type: JoinType,
    /// The conditions, equalities first.
    conditions: Vec<JoinCondition>,
    /// An extra predicate over the joined row (left columns, then right columns).
    predicate: Option<Expression>,
    left_types: Vec<LogicalType>,
    right_types: Vec<LogicalType>,
    children: [PhysicalRef; 2],
}

impl PhysicalNestedLoopJoin {
    /// A nested loop join of `left` and `right` on `conditions`.
    pub fn new(
        types: Vec<LogicalType>,
        join_type: JoinType,
        mut conditions: Vec<JoinCondition>,
        predicate: Option<Expression>,
        left: PhysicalRef,
        right: PhysicalRef,
    ) -> Self {
        reorder_conditions(&mut conditions);
        Self {
            types,
            join_type,
            conditions,
            predicate,
            left_types: left.types().to_vec(),
            right_types: right.types().to_vec(),
            children: [left, right],
        }
    }

    /// Whether a nested loop join can evaluate `conditions` for `join_type`
    /// (`PhysicalNestedLoopJoin::IsSupported`); otherwise a blockwise join is used.
    pub fn is_supported(conditions: &[JoinCondition], join_type: JoinType) -> bool {
        if join_type == JoinType::Mark {
            return true;
        }
        let nested = conditions.iter().any(|condition| {
            matches!(
                condition.left.return_type.physical_type(),
                PhysicalType::Struct | PhysicalType::List | PhysicalType::Array
            )
        });
        if nested {
            return false;
        }
        // Several conditions of a SEMI/ANTI join would be resolved one by one like a
        // MARK join (issue 10046): use the blockwise join for those.
        if matches!(join_type, JoinType::Semi | JoinType::Anti) {
            return conditions.len() == 1;
        }
        true
    }

    fn left_expressions(&self) -> Vec<Expression> {
        self.conditions.iter().map(|condition| condition.left.clone()).collect()
    }

    fn right_expressions(&self) -> Vec<Expression> {
        self.conditions
            .iter()
            .map(|condition| condition.right.clone())
            .collect()
    }

    /// Columns of the materialized right side: the condition values, then the input.
    fn condition_columns(&self) -> Vec<usize> {
        (0..self.conditions.len()).collect()
    }

    fn payload_columns(&self) -> Vec<usize> {
        (self.conditions.len()..self.conditions.len() + self.right_types.len()).collect()
    }

    fn sink_state<'a>(&self, state: &'a SinkState) -> Result<&'a NestedLoopJoinGlobalState> {
        state
            .downcast_ref::<NestedLoopJoinGlobalState>()
            .ok_or_else(|| Error::internal("nested loop join sink state has the wrong type"))
    }

    /// SEMI, ANTI and MARK joins: at most one output row per input row
    /// (`ResolveSimpleJoin`).
    fn resolve_simple_join(
        &self,
        input: &DataChunk,
        output: &mut DataChunk,
        global: &NestedLoopJoinGlobalState,
        state: &mut NestedLoopJoinProbeState,
    ) -> Result<()> {
        state.lhs_executor.execute(input, &mut state.left_condition)?;
        flatten_sequences(&mut state.left_condition);
        let mut found_match = vec![false; input.len()];
        let columns = self.condition_columns();
        let mut right = DataChunk::default();
        for chunk in 0..global.data.chunk_count() {
            global.data.fetch_chunk(chunk, &columns, &mut right)?;
            perform_mark(&state.left_condition, &right, &mut found_match, &self.conditions)?;
        }
        match self.join_type {
            JoinType::Mark => construct_mark_join_result(
                &state.left_condition,
                input,
                output,
                Some(&found_match),
                global.has_null.load(Ordering::Relaxed),
            ),
            JoinType::Semi => construct_semi_or_anti_join_result(input, output, &found_match, true),
            JoinType::Anti => construct_semi_or_anti_join_result(input, output, &found_match, false),
            _ => return Err(Error::not_implemented("Unimplemented type for simple nested loop join!")),
        }
        Ok(())
    }

    /// INNER, LEFT, RIGHT and FULL joins: the next matching pairs of the current input
    /// chunk and right chunk (`ResolveComplexJoin`).
    fn resolve_complex_join(
        &self,
        input: &DataChunk,
        output: &mut DataChunk,
        global: &NestedLoopJoinGlobalState,
        state: &mut NestedLoopJoinProbeState,
    ) -> Result<OperatorResult> {
        let condition_columns = self.condition_columns();
        let payload_columns = self.payload_columns();
        loop {
            if state.fetch_next_right {
                // The right chunk is exhausted: move on to the next one.
                state.position = NestedLoopPosition::default();
                state.fetch_next_right = false;
                if state.next_right_chunk < global.data.chunk_count() {
                    state.load_right_chunk(global, &condition_columns, &payload_columns)?;
                } else {
                    // Every right chunk was joined with this input chunk.
                    state.fetch_next_left = true;
                    if state.left_outer.enabled() {
                        state
                            .left_outer
                            .construct_left_join_result(input, &self.right_types, output);
                        state.left_outer.reset();
                    }
                    return Ok(OperatorResult::NeedMoreInput);
                }
            }
            if state.fetch_next_left {
                state.lhs_executor.execute(input, &mut state.left_condition)?;
                flatten_sequences(&mut state.left_condition);
                state.position = NestedLoopPosition::default();
                state.next_right_chunk = 0;
                state.load_right_chunk(global, &condition_columns, &payload_columns)?;
                state.fetch_next_left = false;
            }
            let mut match_count = perform_inner(
                &mut state.position,
                &state.left_condition,
                &state.right_condition,
                &mut state.lvector,
                &mut state.rvector,
                &self.conditions,
            )?;
            if match_count > 0 {
                let mut columns = slice_columns(input, &state.lvector);
                columns.extend(slice_columns(&state.right_payload, &state.rvector));
                output.set_columns(columns, match_count);
                if let Some(predicate) = &mut state.predicate {
                    let mut sel = Vec::new();
                    match_count = predicate.select(output, &mut sel)?;
                    let columns = slice_columns(output, &sel);
                    output.set_columns(columns, match_count);
                    state.lvector = sel.iter().map(|&i| state.lvector[i as usize]).collect();
                    state.rvector = sel.iter().map(|&i| state.rvector[i as usize]).collect();
                }
                state.left_outer.set_matches(&state.lvector, 0);
                global.right_outer.set_matches(&state.rvector, state.right_base);
            }
            if state.position.right >= state.right_condition.len() {
                state.fetch_next_right = true;
            }
            if match_count > 0 {
                return Ok(OperatorResult::HaveMoreOutput);
            }
        }
    }
}

impl PhysicalOperator for PhysicalNestedLoopJoin {
    fn name(&self) -> &'static str {
        "NESTED_LOOP_JOIN"
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
    fn as_operator(&self) -> Option<&dyn ErasedOperator> {
        Some(self)
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        propagates_build_side(self.join_type).then_some(self as &dyn ErasedSource)
    }
}

/// The materialized right side (DuckDB's `NestedLoopJoinGlobalState`).
#[derive(Debug)]
pub struct NestedLoopJoinGlobalState {
    /// Rows appended while sinking.
    sink: Mutex<(ColumnDataCollection, AppendState)>,
    /// The right side's condition values and columns, once finalized.
    data: ColumnDataCollection,
    /// Whether a right condition value is NULL (MARK joins).
    has_null: AtomicBool,
    /// Which right rows found a match (RIGHT and FULL joins).
    right_outer: OuterJoinMarker,
}

impl Sink for PhysicalNestedLoopJoin {
    type Global = NestedLoopJoinGlobalState;
    type Local = ExpressionExecutor;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<NestedLoopJoinGlobalState> {
        let mut types: Vec<LogicalType> = self
            .conditions
            .iter()
            .map(|condition| condition.right.return_type.clone())
            .collect();
        types.extend(self.right_types.iter().cloned());
        let mut collection = ColumnDataCollection::new(ctx.env.buffer_manager.clone(), types.clone())?;
        let append = collection.initialize_append();
        Ok(NestedLoopJoinGlobalState {
            sink: Mutex::new((collection, append)),
            data: ColumnDataCollection::new(ctx.env.buffer_manager.clone(), types)?,
            has_null: AtomicBool::new(false),
            right_outer: OuterJoinMarker::new(propagates_build_side(self.join_type)),
        })
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &NestedLoopJoinGlobalState) -> Result<ExpressionExecutor> {
        Ok(ExpressionExecutor::new(self.right_expressions()))
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &NestedLoopJoinGlobalState,
        executor: &mut ExpressionExecutor,
    ) -> Result<SinkResult> {
        let mut right_condition = DataChunk::default();
        executor.execute(chunk, &mut right_condition)?;
        if self.join_type == JoinType::Mark
            && !global.has_null.load(Ordering::Relaxed)
            && has_null_values(&right_condition)
        {
            global.has_null.store(true, Ordering::Relaxed);
        }
        let mut columns = right_condition.into_columns();
        columns.extend(chunk.columns().iter().cloned());
        let rows = DataChunk::new(columns, chunk.len());
        let mut sink = global.sink.lock().unwrap_or_else(PoisonError::into_inner);
        let (collection, append) = &mut *sink;
        collection.append(append, &rows)?;
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, _: &NestedLoopJoinGlobalState, _: ExpressionExecutor) -> Result<()> {
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut NestedLoopJoinGlobalState) -> Result<SinkFinalize> {
        let (collection, append) = global.sink.get_mut().unwrap_or_else(PoisonError::into_inner);
        collection.finalize_append(append)?;
        std::mem::swap(&mut global.data, collection);
        global.right_outer.initialize(global.data.count());
        if global.data.count() == 0 && empty_result_if_rhs_is_empty(self.join_type) {
            return Ok(SinkFinalize::NoOutputPossible);
        }
        Ok(SinkFinalize::Ready)
    }
}

/// One thread's probe state (DuckDB's `PhysicalNestedLoopJoinState`).
#[derive(Debug)]
pub struct NestedLoopJoinProbeState {
    fetch_next_left: bool,
    fetch_next_right: bool,
    lhs_executor: ExpressionExecutor,
    left_condition: DataChunk,
    /// The next right chunk to load.
    next_right_chunk: usize,
    right_condition: DataChunk,
    right_payload: DataChunk,
    /// Index of the first row of the current right chunk.
    right_base: usize,
    position: NestedLoopPosition,
    left_outer: OuterJoinMarker,
    predicate: Option<ExpressionExecutor>,
    lvector: Vec<u32>,
    rvector: Vec<u32>,
}

impl NestedLoopJoinProbeState {
    /// Loads the next right chunk's conditions and columns.
    fn load_right_chunk(
        &mut self,
        global: &NestedLoopJoinGlobalState,
        condition_columns: &[usize],
        payload_columns: &[usize],
    ) -> Result<()> {
        let chunk = self.next_right_chunk;
        global.data.fetch_chunk(chunk, condition_columns, &mut self.right_condition)?;
        global.data.fetch_chunk(chunk, payload_columns, &mut self.right_payload)?;
        self.right_base = global.data.chunk_start(chunk);
        self.next_right_chunk += 1;
        Ok(())
    }
}

impl Operator for PhysicalNestedLoopJoin {
    type Global = SinkState;
    type Local = NestedLoopJoinProbeState;

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<SinkState> {
        Err(Error::internal("nested loop join probe requires its sink state"))
    }

    fn global_operator_state_with_sink(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<SinkState> {
        sink.map(Arc::clone)
            .ok_or_else(|| Error::internal("nested loop join probe requires its sink state"))
    }

    fn local_operator_state(&self, _: &ExecutionContext, _: &SinkState) -> Result<NestedLoopJoinProbeState> {
        let mut left_outer = OuterJoinMarker::new(is_left_outer_join(self.join_type));
        left_outer.initialize(STANDARD_VECTOR_SIZE);
        Ok(NestedLoopJoinProbeState {
            fetch_next_left: true,
            fetch_next_right: false,
            lhs_executor: ExpressionExecutor::new(self.left_expressions()),
            left_condition: DataChunk::default(),
            next_right_chunk: 0,
            right_condition: DataChunk::default(),
            right_payload: DataChunk::default(),
            right_base: 0,
            position: NestedLoopPosition::default(),
            left_outer,
            predicate: self
                .predicate
                .as_ref()
                .map(|predicate| ExpressionExecutor::new(vec![predicate.clone()])),
            lvector: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            rvector: Vec::with_capacity(STANDARD_VECTOR_SIZE),
        })
    }

    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        sink: &SinkState,
        state: &mut NestedLoopJoinProbeState,
    ) -> Result<OperatorResult> {
        let global = self.sink_state(sink)?;
        if global.data.count() == 0 {
            if empty_result_if_rhs_is_empty(self.join_type) {
                return Ok(OperatorResult::Finished);
            }
            construct_empty_join_result(
                self.join_type,
                global.has_null.load(Ordering::Relaxed),
                input,
                &self.right_types,
                output,
            )?;
            return Ok(OperatorResult::NeedMoreInput);
        }
        match self.join_type {
            JoinType::Semi | JoinType::Anti | JoinType::Mark => {
                self.resolve_simple_join(input, output, global, state)?;
                Ok(OperatorResult::NeedMoreInput)
            }
            JoinType::Left | JoinType::Inner | JoinType::Outer | JoinType::Right => {
                self.resolve_complex_join(input, output, global, state)
            }
            other => Err(Error::not_implemented(format!(
                "Unimplemented type {} for nested loop join!",
                other.name()
            ))),
        }
    }
}

/// Hands out right chunks to scan for unmatched rows (`OuterJoinGlobalScanState`).
#[derive(Debug)]
pub struct RightOuterScanState {
    sink: SinkState,
    next_chunk: AtomicUsize,
}

impl RightOuterScanState {
    pub(super) fn new(sink: SinkState) -> Self {
        Self {
            sink,
            next_chunk: AtomicUsize::new(0),
        }
    }

    pub(super) fn sink(&self) -> &SinkState {
        &self.sink
    }

    pub(super) fn next_chunk(&self) -> &AtomicUsize {
        &self.next_chunk
    }
}

/// Emits the unmatched rows of chunks of `data` handed out by `next_chunk`, with NULL
/// columns of `left_types` (`OuterJoinMarker::Scan`).
pub(super) fn scan_right_outer(
    data: &ColumnDataCollection,
    marker: &OuterJoinMarker,
    next_chunk: &AtomicUsize,
    columns: &[usize],
    left_types: &[LogicalType],
    chunk: &mut DataChunk,
) -> Result<SourceResult> {
    let mut scanned = DataChunk::default();
    loop {
        let index = next_chunk.fetch_add(1, Ordering::Relaxed);
        if index >= data.chunk_count() {
            return Ok(SourceResult::Finished);
        }
        data.fetch_chunk(index, columns, &mut scanned)?;
        marker.construct_right_join_result(&scanned, data.chunk_start(index), left_types, chunk);
        if !chunk.is_empty() {
            return Ok(SourceResult::HaveMoreOutput);
        }
    }
}

impl Source for PhysicalNestedLoopJoin {
    type Global = RightOuterScanState;
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<RightOuterScanState> {
        let sink = sink.ok_or_else(|| Error::internal("nested loop join source requires its sink state"))?;
        Ok(RightOuterScanState::new(Arc::clone(sink)))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &RightOuterScanState) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &RightOuterScanState,
        (): &mut (),
    ) -> Result<SourceResult> {
        let sink = self.sink_state(&global.sink)?;
        scan_right_outer(
            &sink.data,
            &sink.right_outer,
            &global.next_chunk,
            &self.payload_columns(),
            &self.left_types,
            chunk,
        )
    }
}
