//! The piecewise merge join (DuckDB's `PhysicalPiecewiseMergeJoin`): joins on a range
//! condition (plus optional further conditions) by sorting the right side once and each
//! input chunk on the range key, then merging.

use rustdb_common::tree_renderer::RenderParams;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::Expression;
use rustdb_planner::operator::JoinCondition;
use rustdb_row::sort::{ExternalBlockIteratorState, SortKey, SortedRun};
use rustdb_row::{ParallelScanState, PinProperties, ScanState};
use rustdb_types::{DataChunk, LogicalType, SelectionVector, ValidityMask};

use super::range_join::{
    LocalSortedTable, SortedTable, is_strict_comparison, key_before, order_modifiers,
    reorder_range_conditions, select_join_tail, slice_sorted_payload,
};
use super::{
    OuterJoinMarker, construct_empty_join_result, construct_mark_join_result,
    construct_semi_or_anti_join_result, empty_result_if_rhs_is_empty, is_left_outer_join, null_columns,
    propagates_build_side, slice_columns,
};
use crate::operator::{ErasedOperator, ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{
    ExecutionContext, ExpressionExecutor, Operator, OperatorResult, Sink, SinkFinalize, SinkResult, Source,
    SourceResult,
};

/// A range join through sorted inputs; see the module docs.
#[derive(Debug)]
pub struct PhysicalPiecewiseMergeJoin {
    types: Vec<LogicalType>,
    join_type: JoinType,
    /// The conditions, range conditions first.
    conditions: Vec<JoinCondition>,
    /// Whether the first condition sorts descending (`>`/`>=`).
    descending: bool,
    left_types: Vec<LogicalType>,
    right_types: Vec<LogicalType>,
    children: [PhysicalRef; 2],
}

impl PhysicalPiecewiseMergeJoin {
    /// A merge join of `left` and `right` whose first (reordered) condition is a range
    /// comparison.
    pub fn new(
        types: Vec<LogicalType>,
        join_type: JoinType,
        conditions: Vec<JoinCondition>,
        left: PhysicalRef,
        right: PhysicalRef,
    ) -> Result<Self> {
        let conditions = reorder_range_conditions(conditions);
        let mut descending = false;
        for (index, condition) in conditions.iter().enumerate() {
            match condition.comparison {
                ExpressionType::CompareLessthan | ExpressionType::CompareLessthanorequalto => {
                    if index == 0 {
                        descending = false;
                    }
                }
                ExpressionType::CompareGreaterthan | ExpressionType::CompareGreaterthanorequalto => {
                    if index == 0 {
                        descending = true;
                    }
                }
                ExpressionType::CompareNotequal | ExpressionType::CompareDistinctFrom if index > 0 => {}
                _ => return Err(Error::not_implemented("Unimplemented join type for merge join")),
            }
        }
        Ok(Self {
            types,
            join_type,
            conditions,
            descending,
            left_types: left.types().to_vec(),
            right_types: right.types().to_vec(),
            children: [left, right],
        })
    }

    fn left_expressions(&self) -> Vec<Expression> {
        self.conditions
            .iter()
            .map(|condition| condition.left.clone())
            .collect()
    }

    fn right_expressions(&self) -> Vec<Expression> {
        self.conditions
            .iter()
            .map(|condition| condition.right.clone())
            .collect()
    }

    /// A sorted table of one side: key type of the first condition, the side's columns.
    fn sorted_table(&self, ctx: &ExecutionContext, side_types: &[LogicalType]) -> Result<SortedTable> {
        SortedTable::new(
            ctx,
            &[self.conditions[0].left.return_type.clone()],
            &[order_modifiers(self.descending)],
            side_types,
        )
    }

    /// Sorts an input chunk on the first condition, reads the sorted payload back and
    /// recomputes its keys (`PiecewiseMergeJoinState::ResolveJoinKeys`).
    fn resolve_join_keys(
        &self,
        ctx: &ExecutionContext,
        input: &DataChunk,
        state: &mut MergeJoinProbeState,
    ) -> Result<()> {
        let table = self.sorted_table(ctx, &self.left_types)?;
        let mut local = LocalSortedTable::new(&table, self.left_expressions());
        local.sink(&table, input, &self.conditions)?;
        let mut executor = std::mem::replace(&mut local.executor, ExpressionExecutor::new(Vec::new()));
        table.combine(local)?;
        table.finalize()?;
        table.materialize()?;
        // Scan the sorted payload (minus the primary sort column).
        let payload = table.sorted()?.payload_data();
        state.lhs_payload = DataChunk::default();
        if let Some(payload) = payload {
            let mut scan = payload.initialize_scan(PinProperties::UnpinAfterDone);
            payload.scan(&mut scan, &mut state.lhs_payload)?;
        }
        // Recompute the sorted keys from the sorted input.
        executor.execute(&state.lhs_payload, &mut state.lhs_keys)?;
        state.lhs_iterator = Some(table.create_iterator_state()?);
        state.lhs_table = Some(table);
        Ok(())
    }

    /// SEMI, ANTI and MARK joins: whether each sorted input row is before the largest
    /// key of some right block (`ResolveSimpleJoin`). Output comes in sorted order.
    fn resolve_simple_join(
        &self,
        ctx: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        global: &MergeJoinGlobalState,
        state: &mut MergeJoinProbeState,
    ) -> Result<()> {
        self.resolve_join_keys(ctx, input, state)?;
        let lhs_table = state
            .lhs_table
            .as_ref()
            .ok_or_else(|| Error::internal("merge join without sorted input"))?;
        let mut found_match = vec![false; lhs_table.count()];
        let strict = is_strict_comparison(self.conditions[0].comparison)?;
        let rhs = &global.table;
        let sort_key_type = rhs.sort_key_type()?;
        let (Some(lhs_iterator), Some(rhs_iterator)) = (&mut state.lhs_iterator, &mut state.rhs_iterator)
        else {
            return Err(Error::internal("merge join without iterators"));
        };
        rustdb_row::dispatch_sort_key!(sort_key_type, K => merge_join_simple_blocks::<K>(
            lhs_table,
            lhs_iterator,
            rhs,
            rhs_iterator,
            &mut found_match,
            strict,
        ))?;
        // Use the sorted payload.
        let lhs_not_null = lhs_table.not_null();
        let payload = &state.lhs_payload;
        match self.join_type {
            JoinType::Mark => {
                // The only part of the join keys that is used is the validity mask. Since
                // the payload is sorted, the NULL keys are the tail end.
                let count = lhs_table.count();
                let columns = state
                    .lhs_keys
                    .columns()
                    .iter()
                    .map(|key| {
                        if key.validity().is_all_valid() {
                            return key.clone();
                        }
                        let mut key = key.flattened(count);
                        let mut validity = ValidityMask::all_valid();
                        for row in lhs_not_null..count {
                            validity.set_invalid(row, count);
                        }
                        key.set_validity(validity);
                        key
                    })
                    .collect();
                let keys = DataChunk::new(columns, count);
                construct_mark_join_result(
                    &keys,
                    payload,
                    output,
                    Some(&found_match),
                    rhs.has_null.load(Ordering::Relaxed) > 0,
                );
            }
            JoinType::Semi => construct_semi_or_anti_join_result(payload, output, &found_match, true),
            JoinType::Anti => construct_semi_or_anti_join_result(payload, output, &found_match, false),
            _ => return Err(Error::not_implemented("Unimplemented join type for merge join")),
        }
        Ok(())
    }

    /// INNER, LEFT, RIGHT and FULL joins: merges the sorted input chunk with the right
    /// blocks (`ResolveComplexJoin`).
    fn resolve_complex_join(
        &self,
        ctx: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        global: &MergeJoinGlobalState,
        state: &mut MergeJoinProbeState,
    ) -> Result<OperatorResult> {
        let rhs_table = &global.table;
        let strict = is_strict_comparison(self.conditions[0].comparison)?;
        let sort_key_type = rhs_table.sort_key_type()?;
        loop {
            if state.first_fetch {
                self.resolve_join_keys(ctx, input, state)?;
                state.right_chunk_index = 0;
                state.right_base = 0;
                state.left_position = 0;
                state.prev_left_index = 0;
                state.right_position = 0;
                state.first_fetch = false;
                state.finished = false;
            }
            if state.finished {
                if state.left_outer.enabled() {
                    // Before moving to the next chunk, emit the input rows without a match.
                    state.left_outer.construct_left_join_result(
                        &state.lhs_payload,
                        &self.right_types,
                        output,
                    );
                    state.left_outer.reset();
                }
                state.first_fetch = true;
                state.finished = false;
                return Ok(OperatorResult::NeedMoreInput);
            }
            let lhs_table = state
                .lhs_table
                .as_ref()
                .ok_or_else(|| Error::internal("merge join without sorted input"))?;
            let (Some(lhs_iterator), Some(rhs_iterator)) = (&mut state.lhs_iterator, &mut state.rhs_iterator)
            else {
                return Err(Error::internal("merge join without iterators"));
            };
            let mut left = ChunkMergeInfo {
                block_idx: 0,
                not_null: lhs_table.not_null(),
                entry_idx: state.left_position,
            };
            let mut right = ChunkMergeInfo {
                block_idx: state.right_chunk_index,
                not_null: sorted_chunk_not_null(
                    state.right_chunk_index,
                    rhs_table.count(),
                    rhs_table.has_null.load(Ordering::Relaxed),
                ),
                entry_idx: state.right_position,
            };
            // Repin so we don't hang on to data after we have scanned it. Only the right
            // side: the left side is one chunk.
            rhs_iterator.repin();
            let lhs_run = lhs_table.sorted()?;
            let rhs_run = rhs_table.sorted()?;
            let result_count = rustdb_row::dispatch_sort_key!(sort_key_type, K => merge_join_complex_blocks::<K>(
                &mut MergeSide {
                    run: lhs_run,
                    iterator: lhs_iterator,
                    info: &mut left,
                },
                &mut MergeSide {
                    run: rhs_run,
                    iterator: rhs_iterator,
                    info: &mut right,
                },
                strict,
                &mut state.prev_left_index,
                &mut state.lsel,
                &mut state.rsel,
            ))?;
            state.left_position = left.entry_idx;
            state.right_position = right.entry_idx;
            if result_count == 0 {
                // This right chunk is exhausted: move on to the next.
                state.left_position = 0;
                state.right_position = 0;
                state.right_base += STANDARD_VECTOR_SIZE;
                state.right_chunk_index += 1;
                if state.right_chunk_index >= rhs_table.block_count() {
                    state.finished = true;
                }
                continue;
            }
            // Found matches: extract them.
            let absolute: Vec<u32> = state
                .rsel
                .iter()
                .map(|&row| (right.block_idx * STANDARD_VECTOR_SIZE) as u32 + row)
                .collect();
            let mut rhs_input = DataChunk::default();
            slice_sorted_payload(
                &mut rhs_input,
                rhs_table,
                rhs_iterator,
                &absolute,
                &mut state.decode_buffer,
            )?;
            let mut sel: Vec<u32> = (0..result_count as u32).collect();
            if self.conditions.len() > 1 {
                // Resolve the remaining conditions on the matched pairs.
                let mut rhs_keys = DataChunk::default();
                state.rhs_executor.execute(&rhs_input, &mut rhs_keys)?;
                let lsel = SelectionVector::new(state.lsel.clone());
                for index in 1..self.conditions.len() {
                    let tail = SelectionVector::new(sel.clone());
                    let left = state.lhs_keys.column(index).slice(&lsel).slice(&tail);
                    let right = rhs_keys.column(index).slice(&tail);
                    let passing =
                        select_join_tail(self.conditions[index].comparison, &left, &right, sel.len())?;
                    sel = passing.iter().map(|&i| sel[i as usize]).collect();
                }
            }
            let left_rows: Vec<u32> = sel.iter().map(|&i| state.lsel[i as usize]).collect();
            for &row in &left_rows {
                state.left_outer.set_match(row as usize);
            }
            if !rhs_table.found_match.is_empty() {
                // Absolute position of the block plus the position inside that block.
                for &i in &sel {
                    rhs_table.set_match(state.right_base + state.rsel[i as usize] as usize);
                }
            }
            if sel.is_empty() {
                continue;
            }
            let mut columns = slice_columns(&state.lhs_payload, &left_rows);
            if sel.len() == result_count {
                columns.extend(rhs_input.into_columns());
            } else {
                columns.extend(slice_columns(&rhs_input, &sel));
            }
            output.set_columns(columns, sel.len());
            return Ok(OperatorResult::HaveMoreOutput);
        }
    }
}

/// Number of non-NULL rows in right block `block` (`SortedChunkNotNull`).
fn sorted_chunk_not_null(block: usize, count: usize, has_null: usize) -> usize {
    let begin = block * STANDARD_VECTOR_SIZE;
    let end = (begin + STANDARD_VECTOR_SIZE).min(count);
    let not_null = count - has_null;
    end.min(begin.max(not_null)) - begin
}

/// Marks the input rows that sort before the largest key of some right block
/// (`TemplatedMergeJoinSimpleBlocks`).
fn merge_join_simple_blocks<K: SortKey>(
    lhs_table: &SortedTable,
    lhs_iterator: &mut ExternalBlockIteratorState,
    rhs_table: &SortedTable,
    rhs_iterator: &mut ExternalBlockIteratorState,
    found_match: &mut [bool],
    strict: bool,
) -> Result<()> {
    let lhs_run = lhs_table.sorted()?;
    let rhs_run = rhs_table.sorted()?;
    let lhs_not_null = lhs_table.not_null();
    let rhs_not_null = rhs_table.not_null();
    let mut l_entry_idx = 0;
    for r_idx in (0..rhs_not_null).step_by(STANDARD_VECTOR_SIZE) {
        // Repin the right side to release memory; safe because only left values are
        // returned (the left side is only one chunk).
        rhs_iterator.repin();
        // Only the biggest value of the right block matters.
        let r_entry_idx = (r_idx + STANDARD_VECTOR_SIZE).min(rhs_not_null) - 1;
        let rhs_key = rhs_iterator.get::<K>(rhs_run, r_entry_idx)?;
        loop {
            let lhs_key = lhs_iterator.get::<K>(lhs_run, l_entry_idx)?;
            if !key_before(&lhs_key, &rhs_key, strict) {
                // Every later left value is bigger: next right block.
                break;
            }
            found_match[l_entry_idx] = true;
            l_entry_idx += 1;
            if l_entry_idx >= lhs_not_null {
                // Early out: the whole input matches.
                return Ok(());
            }
        }
    }
    Ok(())
}

/// One side of a merge: its current block and position (`ChunkMergeInfo`).
#[derive(Debug)]
struct ChunkMergeInfo {
    /// The block being scanned.
    block_idx: usize,
    /// Non-NULL rows in the block (they come first).
    not_null: usize,
    /// The current position in the block.
    entry_idx: usize,
}

impl ChunkMergeInfo {
    fn index(&self) -> usize {
        self.block_idx * STANDARD_VECTOR_SIZE + self.entry_idx
    }
}

/// A side of a merge with its run and iterator.
struct MergeSide<'a> {
    run: &'a SortedRun,
    iterator: &'a mut ExternalBlockIteratorState,
    info: &'a mut ChunkMergeInfo,
}

/// The next pairs of (input row, right block row) where the input row sorts before the
/// right row: for each right row, every input row before it, resuming at the sides'
/// positions (`TemplatedMergeJoinComplexBlocks`).
fn merge_join_complex_blocks<K: SortKey>(
    l: &mut MergeSide<'_>,
    r: &mut MergeSide<'_>,
    strict: bool,
    prev_left_index: &mut usize,
    lsel: &mut Vec<u32>,
    rsel: &mut Vec<u32>,
) -> Result<usize> {
    lsel.clear();
    rsel.clear();
    if r.info.entry_idx >= r.info.not_null {
        return Ok(0);
    }
    loop {
        if l.info.entry_idx < *prev_left_index
            || (l.info.entry_idx < l.info.not_null
                && key_before(
                    &l.iterator.get::<K>(l.run, l.info.index())?,
                    &r.iterator.get::<K>(r.run, r.info.index())?,
                    strict,
                ))
        {
            // Left side smaller: found a match; move the left side forward.
            lsel.push(l.info.entry_idx as u32);
            rsel.push(r.info.entry_idx as u32);
            l.info.entry_idx += 1;
            if lsel.len() == STANDARD_VECTOR_SIZE {
                // Out of space.
                break;
            }
            continue;
        }
        *prev_left_index = l.info.entry_idx;
        // The right side is smaller or equal, or the left side is exhausted: move the
        // right side forward and reset the left side to its start.
        r.info.entry_idx += 1;
        if r.info.entry_idx >= r.info.not_null {
            break;
        }
        l.info.entry_idx = 0;
    }
    Ok(lsel.len())
}

impl PhysicalOperator for PhysicalPiecewiseMergeJoin {
    fn name(&self) -> &'static str {
        "PIECEWISE_MERGE_JOIN"
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
    fn parallel_operator(&self) -> bool {
        true
    }
    fn parallel_sink(&self) -> bool {
        true
    }
    fn parallel_source(&self) -> bool {
        true
    }

    fn params_to_string(&self, estimated_cardinality: u64) -> RenderParams {
        super::comparison_join_params(self.join_type, &self.conditions, estimated_cardinality)
    }
}

/// The sorted right side (DuckDB's `MergeJoinGlobalState`).
#[derive(Debug)]
pub struct MergeJoinGlobalState {
    table: SortedTable,
}

impl Sink for PhysicalPiecewiseMergeJoin {
    type Global = MergeJoinGlobalState;
    type Local = LocalSortedTable;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<MergeJoinGlobalState> {
        Ok(MergeJoinGlobalState {
            table: self.sorted_table(ctx, &self.right_types)?,
        })
    }

    fn local_sink_state(&self, _: &ExecutionContext, global: &MergeJoinGlobalState) -> Result<Self::Local> {
        Ok(LocalSortedTable::new(&global.table, self.right_expressions()))
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &MergeJoinGlobalState,
        local: &mut Self::Local,
    ) -> Result<SinkResult> {
        local.sink(&global.table, chunk, &self.conditions)?;
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, global: &MergeJoinGlobalState, local: Self::Local) -> Result<()> {
        global.table.combine(local)
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut MergeJoinGlobalState) -> Result<SinkFinalize> {
        global.table.finalize()?;
        if propagates_build_side(self.join_type) {
            // FULL/RIGHT OUTER joins mark every right row's matches.
            global.table.initialize_matches();
        }
        if global.table.count() == 0 && empty_result_if_rhs_is_empty(self.join_type) {
            return Ok(SinkFinalize::NoOutputPossible);
        }
        // The sorted right side is materialized by the finalize tasks.
        Ok(SinkFinalize::Ready)
    }

    /// Materializes the sorted right side in parallel (`RangeJoinMaterializeEvent`).
    fn finalize_tasks(&self, _: &ExecutionContext, global: &MergeJoinGlobalState) -> Result<usize> {
        Ok(global.table.materialize_tasks())
    }

    fn execute_finalize_task(
        &self,
        _: &ExecutionContext,
        global: &MergeJoinGlobalState,
        _: usize,
    ) -> Result<()> {
        global.table.execute_materialize_task()
    }
}

/// One thread's probe state (DuckDB's `PiecewiseMergeJoinState`).
#[derive(Debug)]
pub struct MergeJoinProbeState {
    left_outer: OuterJoinMarker,
    /// The sorted input chunk and its table.
    lhs_table: Option<SortedTable>,
    lhs_payload: DataChunk,
    /// The condition keys of the sorted input chunk.
    lhs_keys: DataChunk,
    lhs_iterator: Option<ExternalBlockIteratorState>,
    rhs_iterator: Option<ExternalBlockIteratorState>,
    first_fetch: bool,
    finished: bool,
    left_position: usize,
    right_position: usize,
    right_chunk_index: usize,
    right_base: usize,
    prev_left_index: usize,
    lsel: Vec<u32>,
    rsel: Vec<u32>,
    rhs_executor: ExpressionExecutor,
    decode_buffer: Vec<u8>,
}

impl Operator for PhysicalPiecewiseMergeJoin {
    type Global = SinkState;
    type Local = MergeJoinProbeState;

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<SinkState> {
        Err(Error::internal("merge join probe requires its sink state"))
    }

    fn global_operator_state_with_sink(
        &self,
        _: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<SinkState> {
        sink.map(Arc::clone)
            .ok_or_else(|| Error::internal("merge join probe requires its sink state"))
    }

    fn local_operator_state(&self, _: &ExecutionContext, sink: &SinkState) -> Result<MergeJoinProbeState> {
        let global = super::sink_state::<MergeJoinGlobalState>(sink, "merge join")?;
        let mut left_outer = OuterJoinMarker::new(is_left_outer_join(self.join_type));
        left_outer.initialize(STANDARD_VECTOR_SIZE);
        let rhs_iterator = if global.table.count() > 0 {
            Some(global.table.create_iterator_state()?)
        } else {
            None
        };
        Ok(MergeJoinProbeState {
            left_outer,
            lhs_table: None,
            lhs_payload: DataChunk::default(),
            lhs_keys: DataChunk::default(),
            lhs_iterator: None,
            rhs_iterator,
            first_fetch: true,
            finished: true,
            left_position: 0,
            right_position: 0,
            right_chunk_index: 0,
            right_base: 0,
            prev_left_index: 0,
            lsel: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            rsel: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            rhs_executor: ExpressionExecutor::new(self.right_expressions()),
            decode_buffer: Vec::new(),
        })
    }

    fn execute(
        &self,
        ctx: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        sink: &SinkState,
        state: &mut MergeJoinProbeState,
    ) -> Result<OperatorResult> {
        let global = super::sink_state::<MergeJoinGlobalState>(sink, "merge join")?;
        if global.table.count() == 0 {
            // Empty right side.
            if empty_result_if_rhs_is_empty(self.join_type) {
                return Ok(OperatorResult::Finished);
            }
            let has_null = global.table.has_null.load(Ordering::Relaxed) > 0;
            construct_empty_join_result(self.join_type, has_null, input, &self.right_types, output)?;
            return Ok(OperatorResult::NeedMoreInput);
        }
        match self.join_type {
            JoinType::Semi | JoinType::Anti | JoinType::Mark => {
                // Simple joins have at most STANDARD_VECTOR_SIZE matches per chunk.
                self.resolve_simple_join(ctx, input, output, global, state)?;
                Ok(OperatorResult::NeedMoreInput)
            }
            JoinType::Left | JoinType::Inner | JoinType::Right | JoinType::Outer => {
                self.resolve_complex_join(ctx, input, output, global, state)
            }
            _ => Err(Error::not_implemented(
                "Unimplemented type for piecewise merge loop join!",
            )),
        }
    }
}

/// The scan of the sorted right rows for unmatched ones (`PiecewiseJoinGlobalScanState`).
#[derive(Debug)]
pub struct MergeJoinSourceState {
    sink: SinkState,
    parallel_scan: Option<ParallelScanState>,
    /// The position of the next chunk scanned (`right_outer_position`).
    position: Mutex<usize>,
}

impl Source for PhysicalPiecewiseMergeJoin {
    type Global = MergeJoinSourceState;
    type Local = Option<ScanState>;

    fn global_source_state(
        &self,
        _: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<MergeJoinSourceState> {
        let sink = sink.ok_or_else(|| Error::internal("merge join source requires its sink state"))?;
        let global = super::sink_state::<MergeJoinGlobalState>(sink, "merge join")?;
        let parallel_scan = global
            .table
            .sorted()?
            .payload_data()
            .map(|payload| payload.initialize_parallel_scan(None, PinProperties::UnpinAfterDone));
        Ok(MergeJoinSourceState {
            sink: Arc::clone(sink),
            parallel_scan,
            position: Mutex::new(0),
        })
    }

    fn local_source_state(&self, _: &ExecutionContext, global: &MergeJoinSourceState) -> Result<Self::Local> {
        let table = &super::sink_state::<MergeJoinGlobalState>(&global.sink, "merge join")?.table;
        Ok(match (table.sorted()?.payload_data(), &global.parallel_scan) {
            (Some(payload), Some(scan)) => Some(payload.initialize_local_scan(scan)),
            _ => None,
        })
    }

    /// `PiecewiseJoinGlobalScanState::MaxThreads`: one thread per chunk of the right side.
    fn max_threads(&self, global: &MergeJoinSourceState) -> usize {
        super::sink_state::<MergeJoinGlobalState>(&global.sink, "merge join")
            .ok()
            .and_then(|sink| sink.table.sorted().ok())
            .and_then(SortedRun::payload_data)
            .map_or(1, rustdb_row::TupleDataCollection::chunk_count)
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &MergeJoinSourceState,
        local: &mut Self::Local,
    ) -> Result<SourceResult> {
        let table = &super::sink_state::<MergeJoinGlobalState>(&global.sink, "merge join")?.table;
        // The right side was empty: nothing to do.
        let (Some(scan), Some(parallel_scan), Some(payload)) =
            (local, &global.parallel_scan, table.sorted()?.payload_data())
        else {
            return Ok(SourceResult::Finished);
        };
        if table.count() == 0 {
            return Ok(SourceResult::Finished);
        }
        loop {
            // Read the next sorted chunk.
            let mut rhs_chunk = DataChunk::default();
            let rhs_position = {
                let mut position = global.position.lock().unwrap_or_else(PoisonError::into_inner);
                let result = *position;
                payload.parallel_scan(parallel_scan, scan, &mut rhs_chunk)?;
                *position += rhs_chunk.len();
                result
            };
            let count = rhs_chunk.len();
            if count == 0 {
                return Ok(SourceResult::Finished);
            }
            // The rows that did not find a match.
            let rsel: Vec<u32> = (0..count as u32)
                .filter(|&row| !table.found_match[rhs_position + row as usize].load(Ordering::Relaxed))
                .collect();
            if rsel.is_empty() {
                continue;
            }
            let len = rsel.len();
            let mut columns = null_columns(&self.left_types);
            columns.extend(slice_columns(&rhs_chunk, &rsel));
            chunk.set_columns(columns, len);
            return Ok(SourceResult::HaveMoreOutput);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sorted_chunk_not_null;

    #[test]
    fn not_null_rows_per_block() {
        // 5000 rows, the last 100 NULL: blocks of 2048, 2048, 904 (804 not NULL).
        assert_eq!(sorted_chunk_not_null(0, 5000, 100), 2048);
        assert_eq!(sorted_chunk_not_null(2, 5000, 100), 804);
        assert_eq!(sorted_chunk_not_null(1, 5000, 4000), 0);
    }
}
