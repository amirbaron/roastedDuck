//! The inequality join (DuckDB's `PhysicalIEJoin`, after Khayyat et al.): joins on two
//! range conditions by sorting both inputs on the first, ranking all rows on the second,
//! and scanning a bit array of the rows that already passed the first condition.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::Expression;
use rustdb_planner::operator::JoinCondition;
use rustdb_row::sort::SortLocalSinkState;
use rustdb_types::{DataChunk, LogicalType, SelectionVector, Value, Vector};

use super::range_join::{
    LocalSortedTable, SortedTable, is_strict_comparison, order_modifiers, reorder_range_conditions,
    select_join_tail, sorts_before,
};
use super::{empty_result_if_rhs_is_empty, is_left_outer_join, null_columns, propagates_build_side};
use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// Bits of the bit array summarized by one bloom filter bit (`BLOOM_CHUNK_BITS`).
const BLOOM_CHUNK_BITS: usize = 1024;

/// A join on two range conditions; see the module docs. Both inputs are sinks of this
/// operator, which then produces the result as a source.
#[derive(Debug)]
pub struct PhysicalIEJoin {
    types: Vec<LogicalType>,
    join_type: JoinType,
    /// The conditions, range conditions first.
    conditions: Vec<JoinCondition>,
    /// Sort direction of the first two conditions (`lhs_orders`/`rhs_orders`).
    descending: [bool; 2],
    left_types: Vec<LogicalType>,
    right_types: Vec<LogicalType>,
    children: [PhysicalRef; 2],
}

impl PhysicalIEJoin {
    /// An `IEJoin` of `left` and `right`; the first two (reordered) conditions must be range
    /// comparisons.
    pub fn new(
        types: Vec<LogicalType>,
        join_type: JoinType,
        conditions: Vec<JoinCondition>,
        left: PhysicalRef,
        right: PhysicalRef,
    ) -> Result<Self> {
        let conditions = reorder_range_conditions(conditions);
        if conditions.len() < 2 {
            return Err(Error::internal("IEJoin needs two range conditions"));
        }
        let mut descending = [false; 2];
        for (index, condition) in conditions.iter().take(2).enumerate() {
            // L1 sorts descending for `>`, L2 ascending for `>` (and vice versa for `<`).
            descending[index] = match condition.comparison {
                ExpressionType::CompareGreaterthan | ExpressionType::CompareGreaterthanorequalto => {
                    index == 0
                }
                ExpressionType::CompareLessthan | ExpressionType::CompareLessthanorequalto => index != 0,
                _ => return Err(Error::not_implemented("Unimplemented join type for IEJoin")),
            };
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

    fn side_expressions(&self, child: usize) -> Vec<Expression> {
        self.conditions
            .iter()
            .map(|condition| {
                if child == 0 {
                    condition.left.clone()
                } else {
                    condition.right.clone()
                }
            })
            .collect()
    }
}

impl PhysicalOperator for PhysicalIEJoin {
    fn name(&self) -> &'static str {
        "IE_JOIN"
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
    fn sinks_all_children(&self) -> bool {
        true
    }
}

/// Both sorted inputs (DuckDB's `IEJoinGlobalState`).
#[derive(Debug)]
pub struct IEJoinGlobalState {
    /// The left (0) and right (1) inputs, sorted on the first condition.
    tables: [SortedTable; 2],
    /// The input being sunk: the right one first.
    child: AtomicUsize,
}

/// One thread's sink of one input (DuckDB's `IEJoinLocalState`).
#[derive(Debug)]
pub struct IEJoinLocalState {
    child: usize,
    table: LocalSortedTable,
}

impl Sink for PhysicalIEJoin {
    type Global = IEJoinGlobalState;
    type Local = IEJoinLocalState;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<IEJoinGlobalState> {
        let key_type = [self.conditions[0].left.return_type.clone()];
        let modifiers = vec![order_modifiers(self.descending[0])];
        Ok(IEJoinGlobalState {
            tables: [
                SortedTable::new(ctx, &key_type, modifiers.clone(), self.left_types.clone())?,
                SortedTable::new(ctx, &key_type, modifiers, self.right_types.clone())?,
            ],
            child: AtomicUsize::new(1),
        })
    }

    fn local_sink_state(&self, _: &ExecutionContext, global: &IEJoinGlobalState) -> Result<IEJoinLocalState> {
        let child = global.child.load(Ordering::Acquire);
        Ok(IEJoinLocalState {
            child,
            table: LocalSortedTable::new(&global.tables[child], self.side_expressions(child)),
        })
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &IEJoinGlobalState,
        local: &mut IEJoinLocalState,
    ) -> Result<SinkResult> {
        if local.child == 0 && global.tables[1].count() == 0 && empty_result_if_rhs_is_empty(self.join_type) {
            return Ok(SinkResult::Finished);
        }
        local
            .table
            .sink(&global.tables[local.child], chunk, &self.conditions)?;
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(
        &self,
        _: &ExecutionContext,
        global: &IEJoinGlobalState,
        local: IEJoinLocalState,
    ) -> Result<()> {
        global.tables[local.child].combine(local.table)?;
        // The right input is sunk completely before the left one.
        global.child.store(0, Ordering::Release);
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut IEJoinGlobalState) -> Result<SinkFinalize> {
        let [left, right] = &mut global.tables;
        right.finalize()?;
        left.finalize()?;
        if propagates_build_side(self.join_type) {
            right.initialize_matches();
        }
        if is_left_outer_join(self.join_type) {
            left.initialize_matches();
        }
        if right.count() == 0 && empty_result_if_rhs_is_empty(self.join_type) {
            return Ok(SinkFinalize::NoOutputPossible);
        }
        Ok(SinkFinalize::Ready)
    }
}

/// Sets bit `index`.
#[inline]
fn set_bit(bits: &mut [u64], index: usize) {
    bits[index / 64] |= 1 << (index % 64);
}

/// The first set bit in `j..n`, or `n` (`NextValid`).
fn next_valid(bits: &[u64], j: usize, n: usize) -> usize {
    if j >= n {
        return n;
    }
    let mut entry = j / 64;
    let mut word = bits[entry] & (u64::MAX << (j % 64));
    loop {
        if word != 0 {
            return (entry * 64 + word.trailing_zeros() as usize).min(n);
        }
        entry += 1;
        if entry * 64 >= n {
            return n;
        }
        word = bits[entry];
    }
}

/// The rankings shared by all join tasks: L2's sort keys and the permutations between
/// L1 and L2.
#[derive(Debug)]
struct IEJoinRanking {
    /// The second condition's values of both inputs, sorted.
    l2: SortedTable,
    /// Row ids in L1 order: `i + 1` for left row `i`, `-(i + 1)` for right row `i`.
    li: Vec<i64>,
    /// For each L2 position, the L1 position of the row (`P`).
    p: Vec<usize>,
}

/// The state of one join task over a range of L2 blocks (DuckDB's `IEJoinUnion`).
#[derive(Debug)]
struct IEJoinUnion {
    /// The bit array `B`, indexed by L1 position.
    bits: Vec<u64>,
    bloom: Vec<u64>,
    bloom_count: usize,
    strict2: bool,
    /// End of this task's L2 range.
    n: usize,
    /// The current L2 position.
    i: usize,
    /// Number of rows in L2.
    n_j: usize,
    /// The next L1 position to check in the bit array.
    j: usize,
    /// The L2 position of the current left row (`op2`).
    op2: usize,
    /// The next L2 position to add to the bit array (`off2`).
    off2: usize,
    /// Row id of the current left row.
    lrid: i64,
}

impl IEJoinUnion {
    fn new(ranking: &IEJoinRanking, blocks: (usize, usize), strict2: bool) -> Self {
        let n_j = ranking.l2.count();
        let bloom_count = n_j.div_ceil(BLOOM_CHUNK_BITS);
        let i = ranking.l2.block_start(blocks.0);
        let mut union = Self {
            bits: vec![0; n_j.div_ceil(64)],
            bloom: vec![0; bloom_count.div_ceil(64)],
            bloom_count,
            strict2,
            n: ranking.l2.block_start(blocks.1),
            i,
            n_j,
            j: i,
            op2: 0,
            off2: 0,
            lrid: 0,
        };
        union.next_row(ranking);
        union
    }

    /// Moves to the next left row in L2 order, first marking every right row whose second
    /// value comes before it (`NextRow`).
    fn next_row(&mut self, ranking: &IEJoinRanking) -> bool {
        while self.i < self.n {
            let pos = ranking.p[self.i];
            self.lrid = ranking.li[pos];
            if self.lrid < 0 {
                self.i += 1;
                continue;
            }
            self.op2 = self.i;
            while self.off2 < self.n_j {
                if !sorts_before(ranking.l2.key(self.off2), ranking.l2.key(self.op2), self.strict2) {
                    break;
                }
                let p2 = ranking.p[self.off2];
                if ranking.li[p2] < 0 {
                    // Only right rows are marked.
                    set_bit(&mut self.bits, p2);
                    set_bit(&mut self.bloom, p2 / BLOOM_CHUNK_BITS);
                }
                self.off2 += 1;
            }
            // Scan the bit array from the row's L1 position: the rows after it satisfy
            // the first condition.
            self.j = pos;
            return true;
        }
        false
    }

    /// The next (left row, right row) pairs, as row indexes of the sorted inputs
    /// (`JoinComplexBlocks`).
    fn join_complex_blocks(
        &mut self,
        ranking: &IEJoinRanking,
        lsel: &mut Vec<u32>,
        rsel: &mut Vec<u32>,
    ) -> usize {
        lsel.clear();
        rsel.clear();
        while self.i < self.n {
            loop {
                // The bloom filter skips empty stretches of the bit array.
                while self.j < self.n_j {
                    let bloom_begin = next_valid(&self.bloom, self.j / BLOOM_CHUNK_BITS, self.bloom_count)
                        * BLOOM_CHUNK_BITS;
                    let bloom_end = self.n_j.min(bloom_begin + BLOOM_CHUNK_BITS);
                    self.j = self.j.max(bloom_begin);
                    self.j = next_valid(&self.bits, self.j, bloom_end);
                    if self.j < bloom_end {
                        break;
                    }
                }
                if self.j >= self.n_j {
                    break;
                }
                let rrid = ranking.li[self.j];
                self.j += 1;
                lsel.push((self.lrid - 1) as u32);
                rsel.push((-rrid - 1) as u32);
                if lsel.len() == STANDARD_VECTOR_SIZE {
                    return lsel.len();
                }
            }
            self.i += 1;
            if !self.next_row(ranking) {
                break;
            }
        }
        lsel.len()
    }
}

/// A unit of source work (`IEJoinSourceTask`).
#[derive(Debug)]
enum IEJoinTask {
    /// Join the L2 blocks in the range.
    Inner(usize, usize),
    /// Emit the unmatched rows of a block of the left (`true`) or right input.
    Outer { left: bool, block: usize },
}

/// The task in progress.
#[derive(Debug)]
enum ActiveTask {
    Inner(Box<IEJoinUnion>),
    Outer {
        left: bool,
        block: usize,
        position: usize,
    },
}

/// The source's progress (DuckDB's `IEJoinGlobalSourceState`, run as one sequence of
/// tasks).
#[derive(Debug)]
struct IEJoinSourceInner {
    ranking: Option<IEJoinRanking>,
    tasks: VecDeque<IEJoinTask>,
    active: Option<ActiveTask>,
    left_executor: ExpressionExecutor,
    right_executor: ExpressionExecutor,
    lsel: Vec<u32>,
    rsel: Vec<u32>,
}

/// The source state: the sink's tables and the task queue.
#[derive(Debug)]
pub struct IEJoinSourceState {
    sink: SinkState,
    inner: Mutex<IEJoinSourceInner>,
}

impl PhysicalIEJoin {
    /// Sorts the union of both inputs on the first condition (L1) and then on the second
    /// (L2), extracting the row ids and the permutation (stages `SINK_L1` to `EXTRACT_P`).
    fn rank(&self, ctx: &ExecutionContext, tables: &[SortedTable; 2]) -> Result<IEJoinRanking> {
        let order1_type = self.conditions[0].left.return_type.clone();
        let order2_type = self.conditions[1].left.return_type.clone();
        let strict1 = is_strict_comparison(self.conditions[0].comparison)?;
        // Ties on the first value are ordered by side, so that `i < j` in L1 means the
        // first condition holds between left row `i` and right row `j`.
        let mut l1 = SortedTable::new(
            ctx,
            &[order1_type, LogicalType::BOOLEAN],
            vec![order_modifiers(self.descending[0]), order_modifiers(!strict1)],
            vec![order2_type.clone(), LogicalType::BIGINT],
        )?;
        let mut sort_state = l1.local_sink_state();
        for (child, table) in tables.iter().enumerate() {
            let expressions = vec![
                self.side_expressions(child)[0].clone(),
                self.side_expressions(child)[1].clone(),
            ];
            let (from_left, increment) = if child == 0 { (true, 1) } else { (false, -1) };
            append_keys(&l1, &mut sort_state, table, expressions, from_left, increment)?;
        }
        l1.combine_sort(sort_state)?;
        l1.finalize()?;
        let li = extract_column::<i64>(&l1, 1)?;

        let mut l2 = SortedTable::new(
            ctx,
            &[order2_type],
            vec![order_modifiers(self.descending[1])],
            vec![LogicalType::BIGINT],
        )?;
        let mut sort_state = l2.local_sink_state();
        let mut position = 0i64;
        for block in 0..l1.block_count() {
            let rows: Vec<u32> = (l1.block_start(block)..l1.block_start(block) + l1.block_size(block))
                .map(|row| row as u32)
                .collect();
            let mut payload = DataChunk::default();
            l1.gather(&rows, &mut payload)?;
            let count = payload.len();
            let positions = sequence(position, 1, count);
            position += count as i64;
            let chunk = DataChunk::new(vec![payload.column(0).clone(), positions], count);
            l2.sink(&mut sort_state, &chunk)?;
        }
        l2.combine_sort(sort_state)?;
        l2.finalize()?;
        let p = extract_column::<i64>(&l2, 0)?
            .into_iter()
            .map(|position| position as usize)
            .collect();
        Ok(IEJoinRanking { l2, li, p })
    }

    /// The next output of an inner task (`ResolveComplexJoin`); `false` when the task
    /// is exhausted.
    fn resolve_complex_join(
        &self,
        sink: &IEJoinGlobalState,
        ranking: &IEJoinRanking,
        union: &mut IEJoinUnion,
        inner: &mut IEJoinSourceExecutors<'_>,
        output: &mut DataChunk,
    ) -> Result<bool> {
        let [left_table, right_table] = &sink.tables;
        loop {
            let result_count = union.join_complex_blocks(ranking, inner.lsel, inner.rsel);
            if result_count == 0 {
                return Ok(false);
            }
            let mut lpayload = DataChunk::default();
            let mut rpayload = DataChunk::default();
            left_table.gather(inner.lsel, &mut lpayload)?;
            right_table.gather(inner.rsel, &mut rpayload)?;
            let mut sel: Vec<u32> = (0..result_count as u32).collect();
            if self.conditions.len() > 2 {
                let mut left_keys = DataChunk::default();
                let mut right_keys = DataChunk::default();
                inner.left_executor.execute(&lpayload, &mut left_keys)?;
                inner.right_executor.execute(&rpayload, &mut right_keys)?;
                for (index, condition) in self.conditions.iter().enumerate().skip(2) {
                    let tail = SelectionVector::new(sel.clone());
                    let left = left_keys.column(index - 2).slice(&tail);
                    let right = right_keys.column(index - 2).slice(&tail);
                    let passing = select_join_tail(condition.comparison, &left, &right, sel.len())?;
                    sel = passing.iter().map(|&i| sel[i as usize]).collect();
                }
            }
            let count = sel.len();
            let mut columns: Vec<Vector>;
            if count < result_count {
                let tail = SelectionVector::new(sel.clone());
                columns = lpayload
                    .columns()
                    .iter()
                    .map(|column| column.slice(&tail))
                    .collect();
                columns.extend(rpayload.columns().iter().map(|column| column.slice(&tail)));
            } else {
                columns = lpayload.into_columns();
                columns.extend(rpayload.into_columns());
            }
            if !left_table.found_match.is_empty() {
                for &i in &sel {
                    left_table.set_match(inner.lsel[i as usize] as usize);
                }
            }
            if !right_table.found_match.is_empty() {
                for &i in &sel {
                    right_table.set_match(inner.rsel[i as usize] as usize);
                }
            }
            if count > 0 {
                output.set_columns(columns, count);
                return Ok(true);
            }
        }
    }

    /// The next unmatched rows of an outer block (`ExecuteLeftTask` /
    /// `ExecuteRightTask`); `false` when the block is done.
    fn resolve_outer(
        &self,
        sink: &IEJoinGlobalState,
        left: bool,
        block: usize,
        position: &mut usize,
        output: &mut DataChunk,
    ) -> Result<bool> {
        let table = &sink.tables[usize::from(!left)];
        let start = table.block_start(block);
        let count = table.block_size(block);
        let mut rows = Vec::new();
        while *position < count && rows.len() < STANDARD_VECTOR_SIZE {
            let row = start + *position;
            *position += 1;
            if !table.found_match[row].load(Ordering::Relaxed) {
                rows.push(row as u32);
            }
        }
        if rows.is_empty() {
            return Ok(false);
        }
        let mut payload = DataChunk::default();
        table.gather(&rows, &mut payload)?;
        let len = rows.len();
        let columns = if left {
            let mut columns = payload.into_columns();
            columns.extend(null_columns(&self.right_types));
            columns
        } else {
            let mut columns = null_columns(&self.left_types);
            columns.extend(payload.into_columns());
            columns
        };
        output.set_columns(columns, len);
        Ok(true)
    }
}

/// The per-task buffers and executors of the source.
struct IEJoinSourceExecutors<'a> {
    left_executor: &'a mut ExpressionExecutor,
    right_executor: &'a mut ExpressionExecutor,
    lsel: &'a mut Vec<u32>,
    rsel: &'a mut Vec<u32>,
}

/// A BIGINT vector `start, start + increment, ...` of `count` rows.
fn sequence(start: i64, increment: i64, count: usize) -> Vector {
    let mut vector = Vector::flat(LogicalType::BIGINT, count.max(1));
    for (row, value) in vector.values_mut::<i64>()[..count].iter_mut().enumerate() {
        *value = start + increment * row as i64;
    }
    vector
}

/// Sinks `[first value, from_left, second value, row id]` for the non-NULL rows of a
/// sorted input into L1 (`IEJoinUnion::AppendKey`). Row ids count from 1, negated for
/// the right input.
fn append_keys(
    l1: &SortedTable,
    sort_state: &mut SortLocalSinkState,
    table: &SortedTable,
    expressions: Vec<Expression>,
    from_left: bool,
    increment: i64,
) -> Result<()> {
    let valid = table.not_null();
    let mut executor = ExpressionExecutor::new(expressions);
    let mut rid = increment;
    let mut row = 0;
    while row < valid {
        let end = (row + STANDARD_VECTOR_SIZE).min(valid);
        let rows: Vec<u32> = (row as u32..end as u32).collect();
        let mut scanned = DataChunk::default();
        table.gather(&rows, &mut scanned)?;
        let count = scanned.len();
        let mut keys = DataChunk::default();
        executor.execute(&scanned, &mut keys)?;
        let mut columns = keys.into_columns();
        let second = columns
            .pop()
            .ok_or_else(|| Error::internal("IEJoin key missing"))?;
        columns.push(Vector::constant(&Value::boolean(from_left))?);
        columns.push(second);
        columns.push(sequence(rid, increment, count));
        rid += increment * count as i64;
        l1.sink(sort_state, &DataChunk::new(columns, count))?;
        row = end;
    }
    Ok(())
}

/// Payload column `column` of every row of a sorted table, in order (`ExtractColumn`).
fn extract_column<T: rustdb_types::NativeType>(table: &SortedTable, column: usize) -> Result<Vec<T>> {
    let mut values = Vec::with_capacity(table.count());
    for block in 0..table.block_count() {
        let start = table.block_start(block);
        let rows: Vec<u32> = (start..start + table.block_size(block))
            .map(|row| row as u32)
            .collect();
        let mut chunk = DataChunk::default();
        table.gather(&rows, &mut chunk)?;
        let unified = chunk.column(column).unified::<T>();
        values.extend((0..chunk.len()).map(|row| unified.values[unified.slot(row)]));
    }
    Ok(values)
}

impl Source for PhysicalIEJoin {
    type Global = IEJoinSourceState;
    type Local = ();

    fn global_source_state(
        &self,
        ctx: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<IEJoinSourceState> {
        let sink = sink.ok_or_else(|| Error::internal("IEJoin source requires its sink state"))?;
        let global = super::sink_state::<IEJoinGlobalState>(sink, "IEJoin")?;
        let [left, right] = &global.tables;
        let mut tasks = VecDeque::new();
        let ranking = if left.count() > 0 && right.count() > 0 {
            let ranking = self.rank(ctx, &global.tables)?;
            // The L2 blocks, split evenly over the threads.
            let l2_blocks = (left.count() + right.count()).div_ceil(STANDARD_VECTOR_SIZE);
            let per_thread = l2_blocks.div_ceil(ctx.env.threads.max(1)).max(1);
            let mut first = 0;
            while first < l2_blocks && ranking.l2.count() > 0 {
                tasks.push_back(IEJoinTask::Inner(first, (first + per_thread).min(l2_blocks)));
                first += per_thread;
            }
            Some(ranking)
        } else {
            None
        };
        if !left.found_match.is_empty() {
            tasks.extend((0..left.block_count()).map(|block| IEJoinTask::Outer { left: true, block }));
        }
        if !right.found_match.is_empty() {
            tasks.extend((0..right.block_count()).map(|block| IEJoinTask::Outer { left: false, block }));
        }
        let tail_expressions = |child: usize| {
            self.side_expressions(child)
                .split_off(2.min(self.conditions.len()))
        };
        Ok(IEJoinSourceState {
            sink: Arc::clone(sink),
            inner: Mutex::new(IEJoinSourceInner {
                ranking,
                tasks,
                active: None,
                left_executor: ExpressionExecutor::new(tail_expressions(0)),
                right_executor: ExpressionExecutor::new(tail_expressions(1)),
                lsel: Vec::with_capacity(STANDARD_VECTOR_SIZE),
                rsel: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            }),
        })
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &IEJoinSourceState) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &IEJoinSourceState,
        (): &mut (),
    ) -> Result<SourceResult> {
        let sink = super::sink_state::<IEJoinGlobalState>(&global.sink, "IEJoin")?;
        let mut guard = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let state = &mut *guard;
        let strict2 = is_strict_comparison(self.conditions[1].comparison)?;
        loop {
            if state.active.is_none() {
                state.active = match state.tasks.pop_front() {
                    None => return Ok(SourceResult::Finished),
                    Some(IEJoinTask::Inner(first, last)) => {
                        let ranking = state
                            .ranking
                            .as_ref()
                            .ok_or_else(|| Error::internal("IEJoin task without ranking"))?;
                        Some(ActiveTask::Inner(Box::new(IEJoinUnion::new(
                            ranking,
                            (first, last),
                            strict2,
                        ))))
                    }
                    Some(IEJoinTask::Outer { left, block }) => Some(ActiveTask::Outer {
                        left,
                        block,
                        position: 0,
                    }),
                };
            }
            let produced = match &mut state.active {
                Some(ActiveTask::Inner(union)) => {
                    let ranking = state
                        .ranking
                        .as_ref()
                        .ok_or_else(|| Error::internal("IEJoin task without ranking"))?;
                    let mut executors = IEJoinSourceExecutors {
                        left_executor: &mut state.left_executor,
                        right_executor: &mut state.right_executor,
                        lsel: &mut state.lsel,
                        rsel: &mut state.rsel,
                    };
                    self.resolve_complex_join(sink, ranking, union, &mut executors, chunk)?
                }
                Some(ActiveTask::Outer {
                    left,
                    block,
                    position,
                }) => self.resolve_outer(sink, *left, *block, position, chunk)?,
                None => false,
            };
            if produced {
                return Ok(SourceResult::HaveMoreOutput);
            }
            state.active = None;
        }
    }
}
