//! The inequality join (DuckDB's `PhysicalIEJoin`, after Khayyat et al.): joins on two
//! range conditions by sorting both inputs on the first, ranking all rows on the second,
//! and scanning a bit array of the rows that already passed the first condition.
//!
//! Both inputs are sorted into spillable runs ([`SortedTable`]): the right input is
//! finalized and materialized before the left one is sunk (the left pipeline has its own
//! finish event). The source then runs DuckDB's stages as tasks shared by all threads:
//! sort the union of both inputs on the first condition (L1, `SINK_L1` to
//! `MATERIALIZE_L1`), extract the row ids (`EXTRACT_LI`), sort L1 on the second condition
//! (L2, `SINK_L2` to `MATERIALIZE_L2`), extract the permutation (`EXTRACT_P`), join ranges
//! of L2 (`INNER`) and emit unmatched rows of outer joins (`OUTER`). A thread without a
//! task of the current stage waits for the stage to complete.

use rustdb_common::tree_renderer::RenderParams;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::Expression;
use rustdb_planner::operator::JoinCondition;
use rustdb_row::PinProperties;
use rustdb_row::sort::{ExternalBlockIteratorState, SortKey, SortKeyType, SortedRun};
use rustdb_types::{DataChunk, LogicalType, SelectionVector, Value, Vector};

use super::range_join::{
    LocalSortedTable, SortedTable, is_strict_comparison, key_before, order_modifiers,
    reorder_range_conditions, select_join_tail, slice_sorted_payload,
};
use super::{empty_result_if_rhs_is_empty, is_left_outer_join, null_columns, propagates_build_side};
use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// Bits of the bit array summarized by one bloom filter bit (`BLOOM_CHUNK_BITS`).
const BLOOM_CHUNK_BITS: usize = 1024;

/// How long a thread without a task waits before checking for an interrupt again.
const WAIT_INTERVAL: Duration = Duration::from_millis(10);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

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
            // 2. if (op1 ∈ {>, ≥}) sort L1 in descending order, else ascending;
            // 4. if (op2 ∈ {>, ≥}) sort L2 in ascending order, else descending.
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

/// Both sorted inputs (DuckDB's `IEJoinGlobalState`).
#[derive(Debug)]
pub struct IEJoinGlobalState {
    /// The left (0) and right (1) inputs, sorted on the first condition.
    tables: [SortedTable; 2],
    /// The input being sunk: the right one first, then the left one, then 2 (done).
    child: AtomicUsize,
    /// The input whose sorted run the finalize tasks materialize.
    materializing: AtomicUsize,
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
            materializing: AtomicUsize::new(1),
        })
    }

    fn local_sink_state(&self, _: &ExecutionContext, global: &IEJoinGlobalState) -> Result<IEJoinLocalState> {
        let child = global.child.load(Ordering::Acquire);
        if child > 1 {
            return Err(Error::internal("IEJoin sink after both inputs were finalized"));
        }
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
        global.tables[local.child].combine(local.table)
    }

    /// Finalizes the input just sunk, the right one first, and lets the finalize tasks
    /// materialize its sorted run; then moves on to the next input
    /// (`IEJoinGlobalState::Finalize` and `Materialize`).
    fn finalize(&self, _: &ExecutionContext, global: &mut IEJoinGlobalState) -> Result<SinkFinalize> {
        let child = global.child.load(Ordering::Acquire);
        let table = &mut global.tables[child];
        if (child == 1 && propagates_build_side(self.join_type))
            || (child == 0 && is_left_outer_join(self.join_type))
        {
            // FULL/LEFT/RIGHT OUTER joins mark every row's matches.
            table.initialize_matches();
        }
        let result = if child == 1 && table.count() == 0 && empty_result_if_rhs_is_empty(self.join_type) {
            SinkFinalize::NoOutputPossible
        } else {
            SinkFinalize::Ready
        };
        table.finalize()?;
        global.materializing.store(child, Ordering::Release);
        global
            .child
            .store(if child == 1 { 0 } else { 2 }, Ordering::Release);
        Ok(result)
    }

    /// Materializes the finalized input's sorted run (`RangeJoinMaterializeEvent`).
    fn finalize_tasks(&self, _: &ExecutionContext, global: &IEJoinGlobalState) -> Result<usize> {
        Ok(global.tables[global.materializing.load(Ordering::Acquire)].materialize_tasks())
    }

    fn execute_finalize_task(
        &self,
        _: &ExecutionContext,
        global: &IEJoinGlobalState,
        _: usize,
    ) -> Result<()> {
        global.tables[global.materializing.load(Ordering::Acquire)].execute_materialize_task()
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

/// The stages of the source (`IEJoinSourceStage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Init,
    SinkL1,
    FinalizeL1,
    MaterializeL1,
    ExtractLi,
    SinkL2,
    FinalizeL2,
    MaterializeL2,
    ExtractP,
    Inner,
    Outer,
    Done,
}

const STAGES: [Stage; 12] = [
    Stage::Init,
    Stage::SinkL1,
    Stage::FinalizeL1,
    Stage::MaterializeL1,
    Stage::ExtractLi,
    Stage::SinkL2,
    Stage::FinalizeL2,
    Stage::MaterializeL2,
    Stage::ExtractP,
    Stage::Inner,
    Stage::Outer,
    Stage::Done,
];

impl Stage {
    fn from_index(index: usize) -> Self {
        STAGES[index.min(STAGES.len() - 1)]
    }
}

/// A unit of source work (`IEJoinSourceTask`).
#[derive(Clone, Copy, Debug)]
struct IEJoinSourceTask {
    stage: Stage,
    /// The task's number within its stage.
    thread_idx: usize,
    /// The block range.
    range: (usize, usize),
}

/// The shared state of the source (DuckDB's `IEJoinGlobalSourceState`).
#[derive(Debug)]
pub struct IEJoinSourceState {
    sink: SinkState,
    /// The processing stage.
    stage: AtomicU8,
    /// The number of tasks per stage, and the first task of each stage.
    stage_tasks: Vec<usize>,
    stage_begin: Vec<usize>,
    total_tasks: usize,
    /// The next task to hand out; its lock serializes stage changes.
    next_task: Mutex<usize>,
    /// Signalled when a stage completes or a task fails.
    changed: Condvar,
    started: AtomicUsize,
    finished: AtomicUsize,
    stopped: AtomicBool,
    /// The completed tasks of each stage.
    completed: [AtomicUsize; 11],
    /// L1: both inputs sorted on the first condition (and `from_left`), carrying the second
    /// condition's value and the row id.
    l1: SortedTable,
    /// L2: L1 sorted on the second condition, carrying the L1 position.
    l2: SortedTable,
    /// Row ids in L1 order: `i + 1` for left row `i`, `-(i + 1)` for right row `i`.
    li: Mutex<Option<Arc<Vec<i64>>>>,
    /// For each L2 position, the L1 position of the row (`P`).
    p: Mutex<Option<Arc<Vec<usize>>>>,
    l2_blocks: usize,
    per_thread: usize,
    left_blocks: usize,
    right_blocks: usize,
    left_outers: usize,
}

impl IEJoinSourceState {
    fn sink_state(&self) -> Result<&IEJoinGlobalState> {
        super::sink_state::<IEJoinGlobalState>(&self.sink, "IEJoin")
    }

    fn stage(&self) -> Stage {
        Stage::from_index(usize::from(self.stage.load(Ordering::Acquire)))
    }

    fn task_count(&self) -> usize {
        self.stage_begin[Stage::Done as usize]
    }

    fn has_more_tasks(&self) -> bool {
        !self.stopped.load(Ordering::Acquire) && self.started.load(Ordering::Acquire) < self.total_tasks
    }

    /// Moves to the next stage with tasks once every task of the current one completed
    /// (`TryPrepareNextStage`, under the lock).
    fn try_prepare_next_stage(&self) -> bool {
        let stage = self.stage();
        if stage == Stage::Done {
            return false;
        }
        if self.completed[stage as usize].load(Ordering::Acquire) < self.stage_tasks[stage as usize] {
            return false;
        }
        let mut next = stage as usize;
        while next < Stage::Done as usize {
            next += 1;
            if self.stage_tasks[next] != 0 {
                break;
            }
        }
        self.stage.store(next as u8, Ordering::Release);
        true
    }

    /// Counts a task of `stage` as completed and wakes the threads waiting for the stage.
    fn finish_task(&self, stage: Stage) {
        self.completed[stage as usize].fetch_add(1, Ordering::AcqRel);
        self.finished.fetch_add(1, Ordering::AcqRel);
        let _guard = lock(&self.next_task);
        if self.try_prepare_next_stage() {
            self.changed.notify_all();
        }
    }

    /// The next task of the current stage, if any (`TryNextTask`).
    fn try_next_task(&self) -> Option<IEJoinSourceTask> {
        let mut next_task = lock(&self.next_task);
        if !self.has_more_tasks() {
            return None;
        }
        if self.try_prepare_next_stage() {
            self.changed.notify_all();
        }
        if *next_task >= self.task_count() {
            return None;
        }
        // Search for where we are in the task list.
        let mut stage = Stage::Init;
        let mut thread_idx = 0;
        for index in Stage::Init as usize..=Stage::Done as usize {
            if *next_task < self.stage_begin[index] {
                stage = Stage::from_index(index - 1);
                thread_idx = *next_task - self.stage_begin[stage as usize];
                break;
            }
        }
        if stage != self.stage() {
            return None;
        }
        let range = match stage {
            Stage::SinkL1 => {
                let blocks = self.left_blocks + self.right_blocks;
                let first = (thread_idx * self.per_thread).min(blocks);
                (first, (first + self.per_thread).min(blocks))
            }
            Stage::SinkL2 => {
                let blocks = self.l1.block_count();
                let first = (thread_idx * self.per_thread).min(blocks);
                (first, (first + self.per_thread).min(blocks))
            }
            Stage::Inner => {
                let first = thread_idx * self.per_thread;
                (first, (first + self.per_thread).min(self.l2_blocks))
            }
            Stage::Outer => {
                // Left outer blocks first, then right outer blocks.
                let block = if thread_idx < self.left_outers {
                    thread_idx
                } else {
                    thread_idx - self.left_outers
                };
                (block, block + 1)
            }
            _ => (0, 0),
        };
        *next_task += 1;
        self.started.fetch_add(1, Ordering::AcqRel);
        Some(IEJoinSourceTask {
            stage,
            thread_idx,
            range,
        })
    }

    /// `ExecuteLiTask`: the row ids of L1, in L1 order.
    fn execute_li_task(&self) -> Result<()> {
        // We don't need the L1 column, just its sort key, which is in the sort blocks.
        self.l1.get_sorted_run()?;
        *lock(&self.li) = Some(Arc::new(self.l1.extract_column::<i64>(1)?));
        Ok(())
    }

    /// `ExecutePermutationTask`: the permutation array P of L2 with respect to L1.
    fn execute_permutation_task(&self) -> Result<()> {
        self.l2.get_sorted_run()?;
        let p = self
            .l2
            .extract_column::<i64>(0)?
            .into_iter()
            .map(|position| position as usize)
            .collect();
        *lock(&self.p) = Some(Arc::new(p));
        Ok(())
    }
}

/// `(n + val - 1) / val` (`BinValue`).
fn bin_value(n: usize, val: usize) -> usize {
    n.div_ceil(val)
}

/// Sinks the key columns of the non-NULL rows in blocks `range` of the sorted `table` into
/// `marked`, with row ids `rid, rid + increment, ...` (`IEJoinUnion::AppendKey`).
/// `keys` computes the key columns from the payload of a block.
fn append_key(
    table: &SortedTable,
    keys: &mut dyn FnMut(&DataChunk) -> Result<Vec<Vector>>,
    marked: &SortedTable,
    increment: i64,
    mut rid: i64,
    range: (usize, usize),
) -> Result<usize> {
    let (chunk_begin, chunk_end) = range;
    if chunk_begin == chunk_end {
        return Ok(0);
    }
    // Reading.
    let valid = table.not_null();
    let Some(source) = table.sorted()?.payload_data() else {
        return Ok(0);
    };
    let mut scanner = source.initialize_scan(PinProperties::UnpinAfterDone);
    let mut table_idx = source.seek(&mut scanner, chunk_begin);
    // Writing.
    let mut local_sort_state = marked.local_sink_state();
    let mut inserted = 0;
    for _ in chunk_begin..chunk_end {
        let mut scanned = DataChunk::default();
        source.scan(&mut scanner, &mut scanned)?;
        // NULLs are at the end, so stop when we reach them.
        let mut scan_count = scanned.len();
        if table_idx + scan_count > valid {
            scan_count = valid.saturating_sub(table_idx);
            scanned.set_len(scan_count);
        }
        if scan_count == 0 {
            break;
        }
        table_idx += scan_count;
        // Compute the key columns from the payload and mark the rid column.
        let mut columns = keys(&scanned)?;
        columns.push(sequence(rid, increment, scan_count));
        rid += increment * scan_count as i64;
        // Sort on the sort columns (which will no longer be needed).
        marked.sink(&mut local_sort_state, &DataChunk::new(columns, scan_count))?;
        inserted += scan_count;
    }
    marked.combine_sort(local_sort_state)?;
    marked.count.fetch_add(inserted, Ordering::Relaxed);
    Ok(inserted)
}

/// A BIGINT vector `start, start + increment, ...` of `count` rows.
fn sequence(start: i64, increment: i64, count: usize) -> Vector {
    let mut vector = Vector::flat(LogicalType::BIGINT, count.max(1));
    for (row, value) in vector.values_mut::<i64>()[..count].iter_mut().enumerate() {
        *value = start + increment * row as i64;
    }
    vector
}

/// The state of one join task over a range of L2 blocks (DuckDB's `IEJoinUnion`).
#[derive(Debug)]
struct IEJoinUnion {
    /// `li` and `P` of the source.
    li: Arc<Vec<i64>>,
    p: Arc<Vec<usize>>,
    /// The bit array `B`, indexed by L1 position.
    bits: Vec<u64>,
    bloom: Vec<u64>,
    bloom_count: usize,
    strict: bool,
    sort_key_type: SortKeyType,
    /// End of this task's L2 range.
    n: usize,
    /// The current L2 position.
    i: usize,
    /// Number of rows in L2.
    n_j: usize,
    /// The next L1 position to check in the bit array.
    j: usize,
    /// Iterators over L2 for the current left row (`op2`) and the next L2 position to add
    /// to the bit array (`off2`).
    op2: ExternalBlockIteratorState,
    off2: ExternalBlockIteratorState,
    off2_index: usize,
    /// Row id of the current left row.
    lrid: i64,
}

impl IEJoinUnion {
    fn new(gsource: &IEJoinSourceState, chunks: (usize, usize), strict: bool) -> Result<Self> {
        let l2 = &gsource.l2;
        let run = l2.sorted()?;
        // 7. initialize bit-array B (|B| = n), and set all bits to 0
        let n_j = l2.count();
        let bloom_count = n_j.div_ceil(BLOOM_CHUNK_BITS);
        let li = lock(&gsource.li)
            .clone()
            .ok_or_else(|| Error::internal("IEJoin join task before EXTRACT_LI"))?;
        let p = lock(&gsource.p)
            .clone()
            .ok_or_else(|| Error::internal("IEJoin join task before EXTRACT_P"))?;
        // 11. for(i←1 to n) do
        let i = l2.block_start(chunks.0);
        let mut op2 = ExternalBlockIteratorState::new(run);
        op2.repin();
        let mut off2 = ExternalBlockIteratorState::new(run);
        off2.repin();
        let mut union = Self {
            li,
            p,
            bits: vec![0; n_j.div_ceil(64)],
            bloom: vec![0; bloom_count.div_ceil(64)],
            bloom_count,
            strict,
            sort_key_type: run.key_type(),
            n: l2.block_start(chunks.1),
            i,
            n_j,
            j: i,
            op2,
            off2,
            off2_index: 0,
            lrid: 0,
        };
        union.next_row(run)?;
        Ok(union)
    }

    fn next_row(&mut self, run: &SortedRun) -> Result<bool> {
        rustdb_row::dispatch_sort_key!(self.sort_key_type, K => self.next_row_typed::<K>(run))
    }

    /// Moves to the next left row in L2 order, first marking every right row whose second
    /// value comes before it (`NextRow`).
    fn next_row_typed<K: SortKey>(&mut self, run: &SortedRun) -> Result<bool> {
        let mut pinned_idx = self.off2_index / STANDARD_VECTOR_SIZE;
        while self.i < self.n {
            // 12. pos ← P[i]
            let pos = self.p[self.i];
            self.lrid = self.li[pos];
            if self.lrid < 0 {
                self.i += 1;
                continue;
            }
            // 16. B[pos] ← 1
            let op2_key = self.op2.get::<K>(run, self.i)?;
            while self.off2_index < self.n_j {
                // Prevent buildup of pinned blocks.
                let chunk = self.off2_index / STANDARD_VECTOR_SIZE;
                if chunk != pinned_idx {
                    self.off2.repin();
                    pinned_idx = chunk;
                }
                if !key_before(&self.off2.get::<K>(run, self.off2_index)?, &op2_key, self.strict) {
                    break;
                }
                let p2 = self.p[self.off2_index];
                if self.li[p2] < 0 {
                    // Only mark rhs matches.
                    set_bit(&mut self.bits, p2);
                    set_bit(&mut self.bloom, p2 / BLOOM_CHUNK_BITS);
                }
                self.off2_index += 1;
            }
            // Scan the bit array from the row's L1 position: the rows after it satisfy
            // the first condition.
            self.j = pos;
            return Ok(true);
        }
        Ok(false)
    }

    /// The next (left row, right row) pairs, as row indexes of the sorted inputs
    /// (`JoinComplexBlocks`).
    fn join_complex_blocks(
        &mut self,
        run: &SortedRun,
        lsel: &mut Vec<u32>,
        rsel: &mut Vec<u32>,
    ) -> Result<usize> {
        // Release pinned blocks.
        self.op2.repin();
        self.off2.repin();
        // 8. initialize join result as an empty list for tuple pairs
        lsel.clear();
        rsel.clear();
        // 11. for(i←1 to n) do
        while self.i < self.n {
            // 13. for (j ← pos+eqOff to n) do
            loop {
                // 14. if B[j] = 1 then: the bloom filter finds candidate blocks.
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
                // Filter out tuples with the same sign (they come from the same table).
                let rrid = self.li[self.j];
                self.j += 1;
                // 15. add tuples w.r.t. (L1[j], L1[i]) to join result
                lsel.push((self.lrid - 1) as u32);
                rsel.push((-rrid - 1) as u32);
                if lsel.len() == STANDARD_VECTOR_SIZE {
                    // Out of space!
                    return Ok(lsel.len());
                }
            }
            self.i += 1;
            if !self.next_row(run)? {
                break;
            }
        }
        Ok(lsel.len())
    }
}

/// The unmatched rows of one block of an outer join being emitted.
#[derive(Debug)]
struct OuterTask {
    left: bool,
    base: usize,
    outer_idx: usize,
    outer_count: usize,
}

/// A thread's state of the source (DuckDB's `IEJoinLocalSourceState`).
#[derive(Debug)]
pub struct IEJoinLocalSourceState {
    /// The task this thread is working on, until it is counted as completed.
    task: Option<IEJoinSourceTask>,
    /// The global state, to count the task as completed when the thread stops early.
    global: Arc<IEJoinSourceState>,
    joiner: Option<Box<IEJoinUnion>>,
    outer: Option<OuterTask>,
    left_iterator: Option<ExternalBlockIteratorState>,
    right_iterator: Option<ExternalBlockIteratorState>,
    lsel: Vec<u32>,
    rsel: Vec<u32>,
    left_executor: ExpressionExecutor,
    right_executor: ExpressionExecutor,
    decode_buffer: Vec<u8>,
}

impl Drop for IEJoinLocalSourceState {
    fn drop(&mut self) {
        // A thread that stops early (or fails) still completes its task, so threads
        // waiting for the stage do not wait forever.
        if let Some(task) = self.task.take() {
            self.global.finish_task(task.stage);
        }
    }
}

impl IEJoinLocalSourceState {
    /// Whether the thread is between tasks (`TaskFinished`).
    fn task_finished(&self) -> bool {
        self.joiner.is_none() && self.outer.is_none()
    }
}

/// The iteration state in `slot`, created over `table` on first use.
fn iterator_state<'a>(
    slot: &'a mut Option<ExternalBlockIteratorState>,
    table: &SortedTable,
) -> Result<&'a mut ExternalBlockIteratorState> {
    if slot.is_none() {
        *slot = Some(table.create_iterator_state()?);
    }
    slot.as_mut()
        .ok_or_else(|| Error::internal("IEJoin iterator state missing"))
}

impl PhysicalIEJoin {
    /// Takes the next task, counting the previous one as completed (`TryAssignTask`).
    fn try_assign_task(
        &self,
        gsource: &IEJoinSourceState,
        local: &mut IEJoinLocalSourceState,
    ) -> Result<bool> {
        // Because downstream operators may be using our internal buffers, we can't
        // "finish" a task until we are about to get the next one.
        if let Some(task) = local.task.take() {
            gsource.finish_task(task.stage);
        }
        let Some(task) = gsource.try_next_task() else {
            return Ok(false);
        };
        local.task = Some(task);
        match task.stage {
            Stage::Inner => {
                // The join can hit any block on either side.
                let strict = is_strict_comparison(self.conditions[1].comparison)?;
                local.joiner = Some(Box::new(IEJoinUnion::new(gsource, task.range, strict)?));
            }
            Stage::Outer => {
                let gsink = gsource.sink_state()?;
                let left = task.thread_idx < gsource.left_outers;
                let table = &gsink.tables[usize::from(!left)];
                let block = task.range.0;
                local.outer = Some(OuterTask {
                    left,
                    base: table.block_start(block),
                    outer_idx: 0,
                    outer_count: table.block_size(block),
                });
            }
            _ => {}
        }
        Ok(true)
    }

    /// Runs (a step of) the current task (`ExecuteTask`).
    fn execute_task(
        &self,
        gsource: &IEJoinSourceState,
        local: &mut IEJoinLocalSourceState,
        result: &mut DataChunk,
    ) -> Result<()> {
        let Some(task) = local.task else {
            return Ok(());
        };
        match task.stage {
            Stage::Init | Stage::Done => {}
            Stage::SinkL1 => self.execute_sink_l1_task(gsource, task)?,
            Stage::FinalizeL1 => gsource.l1.finalize()?,
            Stage::MaterializeL1 => gsource.l1.materialize_sorted_run()?,
            Stage::ExtractLi => gsource.execute_li_task()?,
            Stage::SinkL2 => Self::execute_sink_l2_task(gsource, task)?,
            Stage::FinalizeL2 => gsource.l2.finalize()?,
            Stage::MaterializeL2 => gsource.l2.materialize_sorted_run()?,
            Stage::ExtractP => gsource.execute_permutation_task()?,
            Stage::Inner => self.resolve_complex_join(gsource, local, result)?,
            Stage::Outer => self.execute_outer_task(gsource, local, result)?,
        }
        Ok(())
    }

    /// Sorts the task's blocks of both inputs into L1 (`ExecuteSinkL1Task`).
    fn execute_sink_l1_task(&self, gsource: &IEJoinSourceState, task: IEJoinSourceTask) -> Result<()> {
        let gsink = gsource.sink_state()?;
        let [left_table, right_table] = &gsink.tables;
        let left_blocks = gsource.left_blocks;
        // Process the LHS sub-range: LHS has positive rids.
        if task.range.0 < left_blocks {
            let range = (task.range.0, task.range.1.min(left_blocks));
            let rid = left_table.block_start(range.0) as i64 + 1;
            self.append_l1_keys(left_table, 0, &gsource.l1, 1, rid, range)?;
        }
        // Process the RHS sub-range: RHS has negative rids.
        if task.range.1 > left_blocks {
            let range = (
                task.range.0.max(left_blocks) - left_blocks,
                task.range.1 - left_blocks,
            );
            let rid = right_table.block_start(range.0) as i64 + 1;
            self.append_l1_keys(right_table, 1, &gsource.l1, -1, -rid, range)?;
        }
        Ok(())
    }

    /// `AppendKey` into L1: `[first condition, from_left, second condition, rid]`.
    fn append_l1_keys(
        &self,
        table: &SortedTable,
        child: usize,
        l1: &SortedTable,
        increment: i64,
        rid: i64,
        range: (usize, usize),
    ) -> Result<()> {
        let expressions = self.side_expressions(child);
        let mut executor = ExpressionExecutor::new(vec![expressions[0].clone(), expressions[1].clone()]);
        let from_left = Vector::constant(&Value::boolean(child == 0))?;
        let mut keys = |scanned: &DataChunk| -> Result<Vec<Vector>> {
            let mut keys = DataChunk::default();
            executor.execute(scanned, &mut keys)?;
            let mut columns = keys.into_columns();
            let second = columns
                .pop()
                .ok_or_else(|| Error::internal("IEJoin key missing"))?;
            columns.push(from_left.clone());
            columns.push(second);
            Ok(columns)
        };
        append_key(table, &mut keys, l1, increment, rid, range)?;
        Ok(())
    }

    /// Sorts the task's blocks of L1 into L2: `[second condition, L1 position]`
    /// (`ExecuteSinkL2Task`).
    fn execute_sink_l2_task(gsource: &IEJoinSourceState, task: IEJoinSourceTask) -> Result<()> {
        let l1 = &gsource.l1;
        let rid = l1.block_start(task.range.0) as i64;
        let mut keys = |scanned: &DataChunk| -> Result<Vec<Vector>> { Ok(vec![scanned.column(0).clone()]) };
        append_key(l1, &mut keys, &gsource.l2, 1, rid, task.range)?;
        Ok(())
    }

    /// The next output of an inner task (`ResolveComplexJoin`); drops the joiner once the
    /// task is exhausted.
    fn resolve_complex_join(
        &self,
        gsource: &IEJoinSourceState,
        local: &mut IEJoinLocalSourceState,
        result: &mut DataChunk,
    ) -> Result<()> {
        let gsink = gsource.sink_state()?;
        let [left_table, right_table] = &gsink.tables;
        let l2_run = gsource.l2.sorted()?;
        loop {
            let Some(joiner) = &mut local.joiner else {
                return Ok(());
            };
            let result_count = joiner.join_complex_blocks(l2_run, &mut local.lsel, &mut local.rsel)?;
            if result_count == 0 {
                // Exhausted this pair.
                local.joiner = None;
                return Ok(());
            }
            // Found matches: extract them.
            let left_iterator = iterator_state(&mut local.left_iterator, left_table)?;
            left_iterator.repin();
            let mut lpayload = DataChunk::default();
            slice_sorted_payload(
                &mut lpayload,
                left_table,
                left_iterator,
                &local.lsel,
                &mut local.decode_buffer,
            )?;
            let right_iterator = iterator_state(&mut local.right_iterator, right_table)?;
            right_iterator.repin();
            let mut rpayload = DataChunk::default();
            slice_sorted_payload(
                &mut rpayload,
                right_table,
                right_iterator,
                &local.rsel,
                &mut local.decode_buffer,
            )?;
            let mut sel: Vec<u32> = (0..result_count as u32).collect();
            if self.conditions.len() > 2 {
                // More conditions to compute: evaluate them on the payloads.
                let mut left_keys = DataChunk::default();
                let mut right_keys = DataChunk::default();
                local.left_executor.execute(&lpayload, &mut left_keys)?;
                local.right_executor.execute(&rpayload, &mut right_keys)?;
                for (index, condition) in self.conditions.iter().enumerate().skip(2) {
                    let tail = SelectionVector::new(sel.clone());
                    let left = left_keys.column(index - 2).slice(&tail);
                    let right = right_keys.column(index - 2).slice(&tail);
                    let passing = select_join_tail(condition.comparison, &left, &right, sel.len())?;
                    sel = passing.iter().map(|&i| sel[i as usize]).collect();
                }
            }
            let count = sel.len();
            // Merge the payloads.
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
            // Found matches: mark them if required.
            if !left_table.found_match.is_empty() {
                for &i in &sel {
                    left_table.set_match(local.lsel[i as usize] as usize);
                }
            }
            if !right_table.found_match.is_empty() {
                for &i in &sel {
                    right_table.set_match(local.rsel[i as usize] as usize);
                }
            }
            if count > 0 {
                result.set_columns(columns, count);
                return Ok(());
            }
        }
    }

    /// The next unmatched rows of an outer block (`ExecuteLeftTask` /
    /// `ExecuteRightTask`); drops the task once the block is done.
    fn execute_outer_task(
        &self,
        gsource: &IEJoinSourceState,
        local: &mut IEJoinLocalSourceState,
        result: &mut DataChunk,
    ) -> Result<()> {
        let gsink = gsource.sink_state()?;
        let Some(outer) = &mut local.outer else {
            return Ok(());
        };
        let left = outer.left;
        let table = &gsink.tables[usize::from(!left)];
        // `SelectOuterRows`.
        let mut outer_sel = Vec::new();
        while outer.outer_idx < outer.outer_count {
            let row = outer.base + outer.outer_idx;
            outer.outer_idx += 1;
            if !table.found_match[row].load(Ordering::Relaxed) {
                outer_sel.push(row as u32);
                if outer_sel.len() >= STANDARD_VECTOR_SIZE {
                    break;
                }
            }
        }
        if outer_sel.is_empty() {
            local.outer = None;
            return Ok(());
        }
        let slot = if left {
            &mut local.left_iterator
        } else {
            &mut local.right_iterator
        };
        let iterator = iterator_state(slot, table)?;
        iterator.repin();
        let mut payload = DataChunk::default();
        slice_sorted_payload(
            &mut payload,
            table,
            iterator,
            &outer_sel,
            &mut local.decode_buffer,
        )?;
        let count = outer_sel.len();
        let columns = if left {
            // Fill in NULLs to the right.
            let mut columns = payload.into_columns();
            columns.extend(null_columns(&self.right_types));
            columns
        } else {
            // Fill in NULLs to the left.
            let mut columns = null_columns(&self.left_types);
            columns.extend(payload.into_columns());
            columns
        };
        result.set_columns(columns, count);
        Ok(())
    }
}

impl Source for PhysicalIEJoin {
    type Global = Arc<IEJoinSourceState>;
    type Local = IEJoinLocalSourceState;

    /// The L1 and L2 tables and the task list (`IEJoinGlobalSourceState` constructor and
    /// `Initialize`).
    fn global_source_state(
        &self,
        ctx: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<Arc<IEJoinSourceState>> {
        let sink = sink.ok_or_else(|| Error::internal("IEJoin source requires its sink state"))?;
        let gsink = super::sink_state::<IEJoinGlobalState>(sink, "IEJoin")?;
        let [left_table, right_table] = &gsink.tables;
        let left_blocks = left_table.block_count();
        let left_outers = if left_table.found_match.is_empty() {
            0
        } else {
            left_blocks
        };
        let right_blocks = right_table.block_count();
        let right_outers = if right_table.found_match.is_empty() {
            0
        } else {
            right_blocks
        };
        // For the union algorithm, a unified table sorted on the first condition (and on
        // `from_left`, to order equal values by side) carrying the second condition's value
        // and the rids. If `i < j` in L1, left row `i` and right row `j` satisfy the first
        // condition: for `<=`/`>=` left rows sort before equal right rows, for `<`/`>` after.
        let order1_type = self.conditions[0].left.return_type.clone();
        let order2_type = self.conditions[1].left.return_type.clone();
        let strict1 = is_strict_comparison(self.conditions[0].comparison)?;
        let l1 = SortedTable::new(
            ctx,
            &[order1_type, LogicalType::BOOLEAN],
            vec![order_modifiers(self.descending[0]), order_modifiers(!strict1)],
            vec![order2_type.clone(), LogicalType::BIGINT],
        )?;
        // Sorting on the second condition gives the permutation array.
        let l2 = SortedTable::new(
            ctx,
            &[order2_type],
            vec![order_modifiers(self.descending[1])],
            vec![LogicalType::BIGINT],
        )?;
        // The number of blocks in L2 is not quite the sum of the blocks of both tables.
        let join_count = left_table.count() + right_table.count();
        let l2_blocks = bin_value(join_count, STANDARD_VECTOR_SIZE);
        // Schedule the largest group on as many threads as possible.
        let threads = ctx.env.threads().max(1);
        let per_thread = bin_value(l2_blocks, threads);
        let l1_tasks = if per_thread == 0 {
            0
        } else {
            bin_value(left_blocks + right_blocks, per_thread)
        };
        let l2_tasks = if per_thread == 0 {
            0
        } else {
            bin_value(l2_blocks, per_thread)
        };
        let stage_tasks = vec![
            0,                          // INIT
            l1_tasks,                   // SINK_L1
            1,                          // FINALIZE_L1
            l1_tasks.max(1),            // MATERIALIZE_L1
            1,                          // EXTRACT_LI
            l2_tasks,                   // SINK_L2
            1,                          // FINALIZE_L2
            l2_tasks.max(1),            // MATERIALIZE_L2
            1,                          // EXTRACT_P
            l2_tasks,                   // INNER
            left_outers + right_outers, // OUTER
            0,                          // DONE
        ];
        // Accumulate task counts so we can find boundaries reliably.
        let mut stage_begin = Vec::with_capacity(stage_tasks.len());
        let mut begin = 0;
        for &tasks in &stage_tasks {
            stage_begin.push(begin);
            begin += tasks;
        }
        Ok(Arc::new(IEJoinSourceState {
            sink: Arc::clone(sink),
            // Ready for action.
            stage: AtomicU8::new(Stage::SinkL1 as u8),
            stage_tasks,
            stage_begin,
            total_tasks: begin,
            next_task: Mutex::new(0),
            changed: Condvar::new(),
            started: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
            completed: Default::default(),
            l1,
            l2,
            li: Mutex::new(None),
            p: Mutex::new(None),
            l2_blocks,
            per_thread,
            left_blocks,
            right_blocks,
            left_outers,
        }))
    }

    fn local_source_state(
        &self,
        _: &ExecutionContext,
        global: &Arc<IEJoinSourceState>,
    ) -> Result<IEJoinLocalSourceState> {
        let tail_expressions = |child: usize| {
            self.side_expressions(child)
                .split_off(2.min(self.conditions.len()))
        };
        Ok(IEJoinLocalSourceState {
            task: None,
            global: Arc::clone(global),
            joiner: None,
            outer: None,
            left_iterator: None,
            right_iterator: None,
            lsel: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            rsel: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            left_executor: ExpressionExecutor::new(tail_expressions(0)),
            right_executor: ExpressionExecutor::new(tail_expressions(1)),
            decode_buffer: Vec::new(),
        })
    }

    /// Any call must produce rows unless the operator is done, so this loops until it has
    /// rows, waiting for stages other threads are still working on (`GetDataInternal`).
    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &Arc<IEJoinSourceState>,
        local: &mut IEJoinLocalSourceState,
    ) -> Result<SourceResult> {
        let gsource = global.as_ref();
        chunk.set_columns(Vec::new(), 0);
        while gsource.stage() != Stage::Done && chunk.is_empty() {
            ctx.check_interrupted()?;
            if gsource.stopped.load(Ordering::Acquire) {
                return Ok(SourceResult::Finished);
            }
            let step = if local.task_finished() {
                self.try_assign_task(gsource, local)
            } else {
                Ok(true)
            }
            .and_then(|assigned| {
                if assigned {
                    self.execute_task(gsource, local, chunk)?;
                }
                Ok(assigned)
            });
            match step {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    gsource.stopped.store(true, Ordering::Release);
                    gsource.changed.notify_all();
                    return Err(error);
                }
            }
            let guard = lock(&gsource.next_task);
            if gsource.try_prepare_next_stage() || gsource.stage() == Stage::Done {
                gsource.changed.notify_all();
                continue;
            }
            // Other threads still work on this stage: wait for them.
            drop(
                gsource
                    .changed
                    .wait_timeout(guard, WAIT_INTERVAL)
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
        Ok(if chunk.is_empty() {
            SourceResult::Finished
        } else {
            SourceResult::HaveMoreOutput
        })
    }

    /// We can't leverage any more threads than tasks (`MaxThreads`).
    fn max_threads(&self, global: &Arc<IEJoinSourceState>) -> usize {
        global.stage_tasks.iter().copied().max().unwrap_or(1).max(1)
    }

    fn progress(&self, global: &Arc<IEJoinSourceState>) -> Option<f64> {
        let count = global.task_count();
        (count > 0).then(|| global.finished.load(Ordering::Relaxed) as f64 / count as f64)
    }
}
