//! The blockwise nested loop join (DuckDB's `PhysicalBlockwiseNLJoin`): joins on an
//! arbitrary condition by evaluating it over the cross product of each input chunk with
//! each right row (or of each input row with each right chunk).

use rustdb_common::tree_renderer::RenderParams;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::JoinType;
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::Expression;
use rustdb_row::AppendState;
use rustdb_types::{DataChunk, LogicalType, Vector};

use super::nested_loop_join::{RightOuterScanState, scan_right_outer};
use super::{
    OuterJoinMarker, construct_empty_join_result, construct_semi_or_anti_join_result,
    empty_result_if_rhs_is_empty, is_left_outer_join, propagates_build_side, slice_columns,
};
use crate::column_data_collection::ColumnDataCollection;
use crate::operator::{ErasedOperator, ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{
    ExecutionContext, ExpressionExecutor, Operator, OperatorResult, Sink, SinkFinalize, SinkResult, Source,
    SourceResult,
};

/// A join on an arbitrary condition; see the module docs.
#[derive(Debug)]
pub struct PhysicalBlockwiseNLJoin {
    types: Vec<LogicalType>,
    join_type: JoinType,
    /// The condition over the joined row (left columns, then right columns).
    condition: Expression,
    left_types: Vec<LogicalType>,
    right_types: Vec<LogicalType>,
    children: [PhysicalRef; 2],
}

impl PhysicalBlockwiseNLJoin {
    /// A blockwise join of `left` and `right` on `condition`. MARK and SINGLE joins are
    /// not supported (DuckDB plans those differently).
    pub fn new(
        types: Vec<LogicalType>,
        join_type: JoinType,
        condition: Expression,
        left: PhysicalRef,
        right: PhysicalRef,
    ) -> Result<Self> {
        if matches!(join_type, JoinType::Mark | JoinType::Single) {
            return Err(Error::internal(format!(
                "{} join planned as a blockwise nested loop join",
                join_type.name()
            )));
        }
        Ok(Self {
            types,
            join_type,
            condition,
            left_types: left.types().to_vec(),
            right_types: right.types().to_vec(),
            children: [left, right],
        })
    }

    fn right_columns(&self) -> Vec<usize> {
        (0..self.right_types.len()).collect()
    }
}

impl PhysicalOperator for PhysicalBlockwiseNLJoin {
    fn name(&self) -> &'static str {
        "BLOCKWISE_NL_JOIN"
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

    fn params_to_string(&self, _: u64) -> RenderParams {
        let mut result = RenderParams::new();
        result.insert("Join Type".to_owned(), self.join_type.name().to_owned());
        result.insert("Condition".to_owned(), self.condition.name());
        result
    }
}

/// The materialized right side (DuckDB's `BlockwiseNLJoinGlobalState`).
#[derive(Debug)]
pub struct BlockwiseNLJoinGlobalState {
    sink: Mutex<(ColumnDataCollection, AppendState)>,
    right_chunks: ColumnDataCollection,
    right_outer: OuterJoinMarker,
}

impl Sink for PhysicalBlockwiseNLJoin {
    type Global = BlockwiseNLJoinGlobalState;
    type Local = ();

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<BlockwiseNLJoinGlobalState> {
        let mut collection =
            ColumnDataCollection::new(ctx.env.buffer_manager.clone(), self.right_types.clone())?;
        let append = collection.initialize_append();
        Ok(BlockwiseNLJoinGlobalState {
            sink: Mutex::new((collection, append)),
            right_chunks: ColumnDataCollection::new(
                ctx.env.buffer_manager.clone(),
                self.right_types.clone(),
            )?,
            right_outer: OuterJoinMarker::new(propagates_build_side(self.join_type)),
        })
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &BlockwiseNLJoinGlobalState) -> Result<()> {
        Ok(())
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &BlockwiseNLJoinGlobalState,
        (): &mut (),
    ) -> Result<SinkResult> {
        let mut sink = global.sink.lock().unwrap_or_else(PoisonError::into_inner);
        let (collection, append) = &mut *sink;
        collection.append(append, chunk)?;
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, _: &BlockwiseNLJoinGlobalState, (): ()) -> Result<()> {
        Ok(())
    }

    fn finalize(
        &self,
        _: &ExecutionContext,
        global: &mut BlockwiseNLJoinGlobalState,
    ) -> Result<SinkFinalize> {
        let (collection, append) = global.sink.get_mut().unwrap_or_else(PoisonError::into_inner);
        collection.finalize_append(append);
        std::mem::swap(&mut global.right_chunks, collection);
        global.right_outer.initialize(global.right_chunks.count());
        if global.right_chunks.count() == 0 && empty_result_if_rhs_is_empty(self.join_type) {
            return Ok(SinkFinalize::NoOutputPossible);
        }
        Ok(SinkFinalize::Ready)
    }
}

/// Walks the cross product of an input chunk with the right chunks: one side is kept
/// whole while the other contributes one row at a time (DuckDB's `CrossProductExecutor`).
#[derive(Debug, Default)]
struct CrossProductExecutor {
    initialized: bool,
    /// The right chunk being combined with the input.
    scan_chunk: DataChunk,
    /// The next right chunk to load.
    next_chunk: usize,
    /// Index of the first row of `scan_chunk` (`ScanPosition`).
    scan_position: usize,
    position_in_chunk: usize,
    /// Whether the input contributes one row at a time (`ScanLHS`).
    scan_input_chunk: bool,
}

impl CrossProductExecutor {
    /// Moves to the next row of the scanned side; `false` once every right chunk was
    /// combined with the input (`NextValue`).
    fn next_value(
        &mut self,
        rhs: &ColumnDataCollection,
        columns: &[usize],
        input: &DataChunk,
    ) -> Result<bool> {
        if !self.initialized {
            self.initialized = true;
            self.scan_input_chunk = false;
            self.next_chunk = 0;
            self.position_in_chunk = 0;
            self.scan_chunk = DataChunk::default();
        }
        self.position_in_chunk += 1;
        let chunk_size = if self.scan_input_chunk {
            input.len()
        } else {
            self.scan_chunk.len()
        };
        if self.position_in_chunk < chunk_size {
            return Ok(true);
        }
        self.position_in_chunk = 0;
        if self.next_chunk >= rhs.chunk_count() {
            return Ok(false);
        }
        rhs.fetch_chunk(self.next_chunk, columns, &mut self.scan_chunk)?;
        self.scan_position = rhs.chunk_start(self.next_chunk);
        self.next_chunk += 1;
        // Keep the larger chunk whole, so the output chunks are large.
        self.scan_input_chunk = input.len() < self.scan_chunk.len();
        Ok(true)
    }

    /// The next part of the cross product (`CrossProductExecutor::Execute`).
    fn execute(
        &mut self,
        rhs: &ColumnDataCollection,
        columns: &[usize],
        input: &DataChunk,
        output: &mut DataChunk,
    ) -> Result<OperatorResult> {
        if rhs.count() == 0 {
            return Ok(OperatorResult::Finished);
        }
        if !self.next_value(rhs, columns, input)? {
            self.initialized = false;
            return Ok(OperatorResult::NeedMoreInput);
        }
        let position = self.position_in_chunk;
        let constant = |chunk: &DataChunk| -> Result<Vec<Vector>> {
            chunk
                .columns()
                .iter()
                .map(|column| Vector::constant(&column.get_value(position)))
                .collect()
        };
        let (columns, len) = if self.scan_input_chunk {
            let mut columns = constant(input)?;
            columns.extend(self.scan_chunk.columns().iter().cloned());
            (columns, self.scan_chunk.len())
        } else {
            let mut columns = input.columns().to_vec();
            columns.extend(constant(&self.scan_chunk)?);
            (columns, input.len())
        };
        output.set_columns(columns, len);
        Ok(OperatorResult::HaveMoreOutput)
    }
}

/// One thread's probe state (DuckDB's `BlockwiseNLJoinState`).
#[derive(Debug)]
pub struct BlockwiseNLJoinState {
    cross_product: CrossProductExecutor,
    left_outer: OuterJoinMarker,
    executor: ExpressionExecutor,
    match_sel: Vec<u32>,
    found_match: Vec<bool>,
}

impl Operator for PhysicalBlockwiseNLJoin {
    type Global = SinkState;
    type Local = BlockwiseNLJoinState;

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<SinkState> {
        Err(Error::internal(
            "blockwise nested loop join probe requires its sink state",
        ))
    }

    fn global_operator_state_with_sink(
        &self,
        _: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<SinkState> {
        sink.map(Arc::clone)
            .ok_or_else(|| Error::internal("blockwise nested loop join probe requires its sink state"))
    }

    fn local_operator_state(&self, _: &ExecutionContext, _: &SinkState) -> Result<BlockwiseNLJoinState> {
        if matches!(self.join_type, JoinType::RightAnti | JoinType::RightSemi) {
            return Err(Error::not_implemented(
                "physical blockwise RIGHT_SEMI/RIGHT_ANTI join not yet implemented",
            ));
        }
        let mut left_outer = OuterJoinMarker::new(is_left_outer_join(self.join_type));
        left_outer.initialize(STANDARD_VECTOR_SIZE);
        Ok(BlockwiseNLJoinState {
            cross_product: CrossProductExecutor::default(),
            left_outer,
            executor: ExpressionExecutor::new(vec![self.condition.clone()]),
            match_sel: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            found_match: vec![false; STANDARD_VECTOR_SIZE],
        })
    }

    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        sink: &SinkState,
        state: &mut BlockwiseNLJoinState,
    ) -> Result<OperatorResult> {
        let global = super::sink_state::<BlockwiseNLJoinGlobalState>(sink, "blockwise nested loop join")?;
        if global.right_chunks.count() == 0 {
            if empty_result_if_rhs_is_empty(self.join_type) {
                return Ok(OperatorResult::Finished);
            }
            construct_empty_join_result(self.join_type, false, input, &self.right_types, output)?;
            return Ok(OperatorResult::NeedMoreInput);
        }
        let semi_or_anti = matches!(self.join_type, JoinType::Semi | JoinType::Anti);
        let mut intermediate = DataChunk::default();
        let result = state.cross_product.execute(
            &global.right_chunks,
            &self.right_columns(),
            input,
            &mut intermediate,
        )?;
        if result == OperatorResult::NeedMoreInput {
            // The input chunk is done: emit its unmatched rows (LEFT/FULL) or the
            // SEMI/ANTI result.
            if state.left_outer.enabled() {
                state
                    .left_outer
                    .construct_left_join_result(input, &self.right_types, output);
                state.left_outer.reset();
            }
            if semi_or_anti {
                let wanted = self.join_type == JoinType::Semi;
                construct_semi_or_anti_join_result(input, output, &state.found_match, wanted);
                state.found_match.fill(false);
            }
            return Ok(OperatorResult::NeedMoreInput);
        }
        let result_count = state.executor.select(&intermediate, &mut state.match_sel)?;
        if result_count > 0 {
            let cross = &state.cross_product;
            if semi_or_anti {
                // SEMI/ANTI joins never produce more output than input rows: only record
                // the matches.
                if cross.scan_input_chunk {
                    state.found_match[cross.position_in_chunk] = true;
                } else {
                    for &row in &state.match_sel {
                        state.found_match[row as usize] = true;
                    }
                }
            } else {
                if cross.scan_input_chunk {
                    state.left_outer.set_match(cross.position_in_chunk);
                    global
                        .right_outer
                        .set_matches(&state.match_sel, cross.scan_position);
                } else {
                    state.left_outer.set_matches(&state.match_sel, 0);
                    global
                        .right_outer
                        .set_match(cross.scan_position + cross.position_in_chunk);
                }
                output.set_columns(slice_columns(&intermediate, &state.match_sel), result_count);
            }
        }
        Ok(OperatorResult::HaveMoreOutput)
    }
}

impl Source for PhysicalBlockwiseNLJoin {
    type Global = RightOuterScanState;
    type Local = ();

    fn global_source_state(
        &self,
        _: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<RightOuterScanState> {
        let sink =
            sink.ok_or_else(|| Error::internal("blockwise nested loop join source requires its sink state"))?;
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
        let sink =
            super::sink_state::<BlockwiseNLJoinGlobalState>(global.sink(), "blockwise nested loop join")?;
        scan_right_outer(
            &sink.right_chunks,
            &sink.right_outer,
            global.next_chunk(),
            &self.right_columns(),
            &self.left_types,
            chunk,
        )
    }
}
