//! The piecewise merge join (DuckDB's `PhysicalPiecewiseMergeJoin`): joins on a range
//! condition (plus optional further conditions) by sorting the right side once and each
//! input chunk on the range key, then merging.

use rustdb_common::tree_renderer::RenderParams;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::Expression;
use rustdb_planner::operator::JoinCondition;
use rustdb_types::{DataChunk, LogicalType, SelectionVector, ValidityMask};

use super::range_join::{
    LocalSortedTable, SortedTable, is_strict_comparison, order_modifiers, reorder_range_conditions,
    select_join_tail, sorts_before,
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
            vec![order_modifiers(self.descending)],
            side_types.to_vec(),
        )
    }

    /// Sorts an input chunk on the first condition and recomputes its keys
    /// (`PiecewiseMergeJoinState::ResolveJoinKeys`).
    fn resolve_join_keys(
        &self,
        ctx: &ExecutionContext,
        input: &DataChunk,
        state: &mut MergeJoinProbeState,
    ) -> Result<()> {
        let mut table = self.sorted_table(ctx, &self.left_types)?;
        let mut local = LocalSortedTable::new(&table, self.left_expressions());
        local.sink(&table, input, &self.conditions)?;
        table.combine(local)?;
        table.finalize()?;
        let all: Vec<u32> = (0..table.count() as u32).collect();
        table.gather(&all, &mut state.lhs_payload)?;
        state
            .lhs_executor
            .execute(&state.lhs_payload, &mut state.lhs_keys)?;
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
        let lhs = state
            .lhs_table
            .as_ref()
            .ok_or_else(|| Error::internal("merge join without sorted input"))?;
        let rhs = &global.table;
        let strict = is_strict_comparison(self.conditions[0].comparison)?;
        let mut found_match = vec![false; lhs.count()];
        let lhs_not_null = lhs.not_null();
        let rhs_not_null = rhs.not_null();
        let mut l_entry = 0;
        'blocks: for r_start in (0..rhs_not_null).step_by(STANDARD_VECTOR_SIZE) {
            // Only the largest key of the block matters.
            let r_entry = (r_start + STANDARD_VECTOR_SIZE).min(rhs_not_null) - 1;
            while sorts_before(lhs.key(l_entry), rhs.key(r_entry), strict) {
                found_match[l_entry] = true;
                l_entry += 1;
                if l_entry >= lhs_not_null {
                    break 'blocks;
                }
            }
        }
        let payload = &state.lhs_payload;
        match self.join_type {
            JoinType::Mark => {
                // Keys are only used for their validity: the sorted NULL keys are last.
                let count = lhs.count();
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
        let rhs = &global.table;
        let strict = is_strict_comparison(self.conditions[0].comparison)?;
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
            let lhs = state
                .lhs_table
                .as_ref()
                .ok_or_else(|| Error::internal("merge join without sorted input"))?;
            let rhs_not_null = sorted_chunk_not_null(state.right_chunk_index, rhs.count(), rhs.not_null());
            let block_start = rhs.block_start(state.right_chunk_index);
            let result_count = merge_join_complex_blocks(
                lhs,
                &mut state.left_position,
                rhs,
                block_start,
                rhs_not_null,
                &mut state.right_position,
                strict,
                &mut state.prev_left_index,
                &mut state.lsel,
                &mut state.rsel,
            );
            if result_count == 0 {
                // This right block is done: move on to the next.
                state.left_position = 0;
                state.right_position = 0;
                state.right_base += STANDARD_VECTOR_SIZE;
                state.right_chunk_index += 1;
                if state.right_chunk_index >= rhs.block_count() {
                    state.finished = true;
                }
                continue;
            }
            let absolute: Vec<u32> = state.rsel.iter().map(|&row| (block_start as u32) + row).collect();
            let mut rhs_input = DataChunk::default();
            rhs.gather(&absolute, &mut rhs_input)?;
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
            if sel.is_empty() {
                continue;
            }
            let left_rows: Vec<u32> = sel.iter().map(|&i| state.lsel[i as usize]).collect();
            let mut columns = slice_columns(&state.lhs_payload, &left_rows);
            if sel.len() == result_count {
                columns.extend(rhs_input.into_columns());
            } else {
                columns.extend(slice_columns(&rhs_input, &sel));
            }
            output.set_columns(columns, sel.len());
            for &row in &left_rows {
                state.left_outer.set_match(row as usize);
            }
            if !rhs.found_match.is_empty() {
                for &i in &sel {
                    rhs.set_match(state.right_base + state.rsel[i as usize] as usize);
                }
            }
            return Ok(OperatorResult::HaveMoreOutput);
        }
    }
}

/// Number of non-NULL rows in right block `block` (`SortedChunkNotNull`).
fn sorted_chunk_not_null(block: usize, count: usize, not_null: usize) -> usize {
    let begin = block * STANDARD_VECTOR_SIZE;
    let end = (begin + STANDARD_VECTOR_SIZE).min(count);
    end.min(begin.max(not_null)) - begin
}

/// The next pairs of (sorted input row, right block row) where the input row sorts
/// before the right row: for each right row, every input row before it, resuming at the
/// given positions (`MergeJoinComplexBlocks`).
#[allow(clippy::too_many_arguments, reason = "mirrors DuckDB's merge state")]
fn merge_join_complex_blocks(
    lhs: &SortedTable,
    l_entry: &mut usize,
    rhs: &SortedTable,
    block_start: usize,
    r_not_null: usize,
    r_entry: &mut usize,
    strict: bool,
    prev_left_index: &mut usize,
    lsel: &mut Vec<u32>,
    rsel: &mut Vec<u32>,
) -> usize {
    lsel.clear();
    rsel.clear();
    if *r_entry >= r_not_null {
        return 0;
    }
    let l_not_null = lhs.not_null();
    loop {
        let matches = *l_entry < *prev_left_index
            || (*l_entry < l_not_null
                && sorts_before(lhs.key(*l_entry), rhs.key(block_start + *r_entry), strict));
        if matches {
            lsel.push(*l_entry as u32);
            rsel.push(*r_entry as u32);
            *l_entry += 1;
            if lsel.len() == STANDARD_VECTOR_SIZE {
                break;
            }
            continue;
        }
        *prev_left_index = *l_entry;
        // The right row is smaller, or the input is exhausted: next right row.
        *r_entry += 1;
        if *r_entry >= r_not_null {
            break;
        }
        *l_entry = 0;
    }
    lsel.len()
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
            global.table.initialize_matches();
        }
        if global.table.count() == 0 && empty_result_if_rhs_is_empty(self.join_type) {
            return Ok(SinkFinalize::NoOutputPossible);
        }
        Ok(SinkFinalize::Ready)
    }
}

/// One thread's probe state (DuckDB's `PiecewiseMergeJoinState`).
#[derive(Debug)]
pub struct MergeJoinProbeState {
    left_outer: OuterJoinMarker,
    lhs_executor: ExpressionExecutor,
    lhs_table: Option<SortedTable>,
    /// The sorted input chunk.
    lhs_payload: DataChunk,
    /// The condition keys of the sorted input chunk.
    lhs_keys: DataChunk,
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

    fn local_operator_state(&self, _: &ExecutionContext, _: &SinkState) -> Result<MergeJoinProbeState> {
        let mut left_outer = OuterJoinMarker::new(is_left_outer_join(self.join_type));
        left_outer.initialize(STANDARD_VECTOR_SIZE);
        Ok(MergeJoinProbeState {
            left_outer,
            lhs_executor: ExpressionExecutor::new(self.left_expressions()),
            lhs_table: None,
            lhs_payload: DataChunk::default(),
            lhs_keys: DataChunk::default(),
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
            if empty_result_if_rhs_is_empty(self.join_type) {
                return Ok(OperatorResult::Finished);
            }
            let has_null = global.table.has_null.load(Ordering::Relaxed) > 0;
            construct_empty_join_result(self.join_type, has_null, input, &self.right_types, output)?;
            return Ok(OperatorResult::NeedMoreInput);
        }
        match self.join_type {
            JoinType::Semi | JoinType::Anti | JoinType::Mark => {
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

/// The scan of unmatched right rows, in sorted order (`PiecewiseJoinGlobalScanState`).
#[derive(Debug)]
pub struct MergeJoinSourceState {
    sink: SinkState,
    next_row: AtomicUsize,
}

impl Source for PhysicalPiecewiseMergeJoin {
    type Global = MergeJoinSourceState;
    type Local = ();

    fn global_source_state(
        &self,
        _: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<MergeJoinSourceState> {
        let sink = sink.ok_or_else(|| Error::internal("merge join source requires its sink state"))?;
        Ok(MergeJoinSourceState {
            sink: Arc::clone(sink),
            next_row: AtomicUsize::new(0),
        })
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &MergeJoinSourceState) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &MergeJoinSourceState,
        (): &mut (),
    ) -> Result<SourceResult> {
        let table = &super::sink_state::<MergeJoinGlobalState>(&global.sink, "merge join")?.table;
        loop {
            let start = global.next_row.fetch_add(STANDARD_VECTOR_SIZE, Ordering::Relaxed);
            if start >= table.count() {
                return Ok(SourceResult::Finished);
            }
            let end = (start + STANDARD_VECTOR_SIZE).min(table.count());
            let unmatched: Vec<u32> = (start..end)
                .filter(|&row| !table.found_match[row].load(Ordering::Relaxed))
                .map(|row| row as u32)
                .collect();
            if unmatched.is_empty() {
                continue;
            }
            let mut right = DataChunk::default();
            table.gather(&unmatched, &mut right)?;
            let mut columns = null_columns(&self.left_types);
            columns.extend(right.into_columns());
            chunk.set_columns(columns, unmatched.len());
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
        assert_eq!(sorted_chunk_not_null(0, 5000, 4900), 2048);
        assert_eq!(sorted_chunk_not_null(2, 5000, 4900), 804);
        assert_eq!(sorted_chunk_not_null(1, 5000, 1000), 0);
    }
}
