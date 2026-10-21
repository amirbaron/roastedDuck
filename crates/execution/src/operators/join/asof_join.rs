//! The as-of join (DuckDB's `PhysicalAsOfJoin`): each left row joins the right row with
//! the same partition values whose ordering value is the closest one satisfying the
//! inequality.
//!
//! Both inputs are hash-partitioned on the equality conditions and sorted on the
//! partition values and the inequality value ([`SortStrategy`]); the right input is
//! finalized before the left one is sunk (the left pipeline has its own finish event).
//! The source works through the hash groups, largest first, in stages shared by all
//! threads (`AsOfHashGroup`): sort both sides of the group (`SORT`), merge them into
//! spillable sorted runs (`MATERIALIZE`, `GET`), probe ranges of left blocks (`LEFT`) and
//! emit unmatched right rows (`RIGHT`). For every left row in sort order, an exponential
//! plus binary search finds the last right row sorting before it; it is a match if its
//! partition values are equal.

use std::cmp::Ordering as CmpOrdering;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::Expression;
use rustdb_planner::operator::JoinCondition;
use rustdb_row::sort::{
    ChunkRow, ExternalBlockIteratorState, NullOrder, OrderModifiers, OrderType, SortColumn, SortConfig,
    SortKey, SortStrategy, SortedRun, StrategyGlobalState, StrategyLocalState, decode_sort_key_column,
};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, ValidityMask, Vector};

use super::range_join::{key_before, order_modifiers};
use super::{
    OuterJoinMarker, construct_semi_or_anti_join_result, is_left_outer_join, null_columns,
    reorder_conditions, slice_columns,
};
use crate::join_hashtable::flatten_sequences;
use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// How long a thread without a task waits before checking for an interrupt again.
const WAIT_INTERVAL: Duration = Duration::from_millis(10);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An as-of join; see the module docs. Both inputs are sinks of this operator, which
/// produces the result as a source.
#[derive(Debug)]
pub struct PhysicalAsOfJoin {
    types: Vec<LogicalType>,
    join_type: JoinType,
    /// The conditions, equalities first.
    conditions: Vec<JoinCondition>,
    /// The equality conditions (the partitions), by index.
    partitions: Vec<usize>,
    /// The inequality condition.
    inequality: usize,
    /// Whether the inequality sorts descending (`<`, `<=`).
    descending: bool,
    /// Whether the inequality excludes equality.
    strict: bool,
    /// Left key columns whose NULLs never match (DuckDB's `null_sensitive`, which
    /// indexes the keys by the position of the order among the orders).
    null_sensitive: Vec<usize>,
    left_types: Vec<LogicalType>,
    right_types: Vec<LogicalType>,
    estimated_cardinality: [usize; 2],
    children: [PhysicalRef; 2],
}

impl PhysicalAsOfJoin {
    /// An as-of join of `left` and `right` on equality conditions and one inequality;
    /// `estimated_cardinality` are the inputs' estimates.
    pub fn new(
        types: Vec<LogicalType>,
        join_type: JoinType,
        mut conditions: Vec<JoinCondition>,
        left: PhysicalRef,
        right: PhysicalRef,
        estimated_cardinality: [usize; 2],
    ) -> Result<Self> {
        reorder_conditions(&mut conditions);
        let mut partitions = Vec::new();
        let mut inequality = None;
        let mut descending = false;
        let mut strict = false;
        let mut null_sensitive = Vec::new();
        for (index, condition) in conditions.iter().enumerate() {
            let orders = usize::from(inequality.is_some());
            match condition.comparison {
                ExpressionType::CompareGreaterthanorequalto | ExpressionType::CompareGreaterthan => {
                    null_sensitive.push(orders);
                    inequality = Some(index);
                    strict = condition.comparison == ExpressionType::CompareGreaterthan;
                }
                ExpressionType::CompareLessthanorequalto | ExpressionType::CompareLessthan => {
                    // Always put NULLS LAST so they can be ignored.
                    null_sensitive.push(orders);
                    inequality = Some(index);
                    descending = true;
                    strict = condition.comparison == ExpressionType::CompareLessthan;
                }
                ExpressionType::CompareEqual => {
                    null_sensitive.push(orders);
                    partitions.push(index);
                }
                ExpressionType::CompareNotDistinctFrom => partitions.push(index),
                _ => return Err(Error::not_implemented("Unsupported join condition for ASOF join")),
            }
        }
        let inequality = inequality.ok_or_else(|| Error::internal("ASOF join without an inequality"))?;
        Ok(Self {
            types,
            join_type,
            conditions,
            partitions,
            inequality,
            descending,
            strict,
            null_sensitive,
            left_types: left.types().to_vec(),
            right_types: right.types().to_vec(),
            estimated_cardinality,
            children: [left, right],
        })
    }

    fn side_types(&self, child: usize) -> &[LogicalType] {
        if child == 0 {
            &self.left_types
        } else {
            &self.right_types
        }
    }

    /// The partition values and then the ordering value of one side.
    fn key_expressions(&self, child: usize) -> Vec<Expression> {
        self.partitions
            .iter()
            .chain(std::iter::once(&self.inequality))
            .map(|&index| {
                let condition = &self.conditions[index];
                if child == 0 {
                    condition.left.clone()
                } else {
                    condition.right.clone()
                }
            })
            .collect()
    }

    /// The sort strategy of one side (`SortStrategy::Factory`): the input columns
    /// followed by the key values, grouped on the partitions, sorted on the order.
    fn sort_strategy(&self, ctx: &ExecutionContext, child: usize) -> Result<SortStrategy> {
        let input_width = self.side_types(child).len();
        let mut input_types = self.side_types(child).to_vec();
        input_types.extend(
            self.key_expressions(child)
                .iter()
                .map(|key| key.return_type.clone()),
        );
        let partition_columns = (input_width..input_width + self.partitions.len()).collect();
        let orders = vec![SortColumn {
            column: input_width + self.partitions.len(),
            modifiers: order_modifiers(self.descending),
            stats: None,
        }];
        SortStrategy::new(
            ctx.env.buffer_manager.clone(),
            partition_columns,
            &[],
            orders,
            input_types,
            self.estimated_cardinality[child],
            true,
        )
    }
}

impl PhysicalOperator for PhysicalAsOfJoin {
    fn name(&self) -> &'static str {
        "ASOF_JOIN"
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
}

/// Both inputs, partitioned and sorted (DuckDB's `AsOfGlobalSinkState`).
#[derive(Debug)]
pub struct AsOfGlobalSinkState {
    strategies: [SortStrategy; 2],
    states: [StrategyGlobalState; 2],
    /// The input being sunk: the right one first.
    child: AtomicUsize,
}

/// One thread's sink of one input (DuckDB's `AsOfLocalSinkState`).
#[derive(Debug)]
pub struct AsOfLocalSinkState {
    child: usize,
    executor: ExpressionExecutor,
    state: StrategyLocalState,
}

impl Sink for PhysicalAsOfJoin {
    type Global = AsOfGlobalSinkState;
    type Local = AsOfLocalSinkState;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<AsOfGlobalSinkState> {
        let config = SortConfig {
            threads: ctx.env.threads(),
            connections: 1,
            ..SortConfig::default()
        };
        let strategies = [self.sort_strategy(ctx, 0)?, self.sort_strategy(ctx, 1)?];
        let states = [
            strategies[0].global_state(config.clone())?,
            strategies[1].global_state(config)?,
        ];
        Ok(AsOfGlobalSinkState {
            strategies,
            states,
            child: AtomicUsize::new(1),
        })
    }

    fn local_sink_state(
        &self,
        _: &ExecutionContext,
        global: &AsOfGlobalSinkState,
    ) -> Result<AsOfLocalSinkState> {
        let child = global.child.load(Ordering::Acquire);
        Ok(AsOfLocalSinkState {
            child,
            executor: ExpressionExecutor::new(self.key_expressions(child)),
            state: global.strategies[child].local_state(),
        })
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &AsOfGlobalSinkState,
        local: &mut AsOfLocalSinkState,
    ) -> Result<SinkResult> {
        let mut keys = DataChunk::default();
        local.executor.execute(chunk, &mut keys)?;
        let mut columns = chunk.columns().to_vec();
        columns.extend(keys.into_columns());
        let mut input = DataChunk::new(columns, chunk.len());
        flatten_sequences(&mut input);
        global.strategies[local.child].sink(&input, &global.states[local.child], &mut local.state)?;
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(
        &self,
        _: &ExecutionContext,
        global: &AsOfGlobalSinkState,
        local: AsOfLocalSinkState,
    ) -> Result<()> {
        let child = local.child;
        global.strategies[child].combine(&global.states[child], local.state)
    }

    /// Finalizes the input just sunk: the right one first, then the left one (the left
    /// pipeline has its own finish event).
    fn finalize(&self, _: &ExecutionContext, global: &mut AsOfGlobalSinkState) -> Result<SinkFinalize> {
        let child = global.child.load(Ordering::Acquire);
        if child == 1 {
            // The data is all in, so the left side can partition like the right side.
            global.strategies[0].synchronize(&global.states[1], &global.states[0])?;
        }
        // Switch sides.
        global.child.store(1 - child, Ordering::Release);
        global.strategies[child].finalize(&mut global.states[child])?;
        Ok(SinkFinalize::Ready)
    }
}

/// The stages of a hash group (`AsOfJoinSourceStage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Init,
    Sort,
    Materialize,
    Get,
    Left,
    Right,
    Done,
}

const STAGES: [Stage; 7] = [
    Stage::Init,
    Stage::Sort,
    Stage::Materialize,
    Stage::Get,
    Stage::Left,
    Stage::Right,
    Stage::Done,
];

/// A unit of source work (`AsOfSourceTask`).
#[derive(Clone, Copy, Debug)]
struct AsOfSourceTask {
    stage: Stage,
    /// The hash group.
    group_idx: usize,
    /// The block range.
    begin_idx: usize,
    end_idx: usize,
}

/// The scheduling of one hash group (the task bookkeeping of DuckDB's `AsOfHashGroup`,
/// under the source's lock).
#[derive(Debug)]
struct GroupSchedule {
    stage: Stage,
    /// The number of tasks per stage, and the first task of each stage.
    stage_tasks: Vec<usize>,
    stage_begin: Vec<usize>,
    per_thread: usize,
    next_task: usize,
}

/// One hash group of both inputs (DuckDB's `AsOfHashGroup`).
#[derive(Debug)]
struct AsOfHashGroup {
    group_idx: usize,
    left_stats: ChunkRow,
    right_stats: ChunkRow,
    /// The sorted sides (from the `GET` stage until their last reader is done).
    left_group: Mutex<Option<Arc<SortedRun>>>,
    right_group: Mutex<Option<Arc<SortedRun>>>,
    /// Which right rows found a match (RIGHT and FULL joins).
    right_outer: OuterJoinMarker,
    /// Completed tasks per stage.
    sorted: AtomicUsize,
    materialized: AtomicUsize,
    gotten: AtomicUsize,
    left_completed: AtomicUsize,
    right_completed: AtomicUsize,
}

impl AsOfHashGroup {
    fn left_chunks(&self) -> usize {
        self.left_stats.chunks
    }

    fn right_chunks(&self) -> usize {
        self.right_stats.chunks
    }

    /// The most chunks any side has (`MaximumChunks`).
    fn maximum_chunks(&self) -> usize {
        self.left_chunks().max(self.right_chunks())
    }

    fn is_right_outer(&self) -> bool {
        self.right_outer.enabled()
    }

    /// The task counts of each stage (`InitTasks`).
    fn init_tasks(&self, per_thread: usize) -> GroupSchedule {
        let bin = |n: usize| n.div_ceil(per_thread);
        let materialize_tasks = bin(self.left_chunks()) + bin(self.right_chunks());
        let right_chunks = if self.is_right_outer() {
            self.right_chunks()
        } else {
            0
        };
        let stage_tasks = vec![
            0,                                   // INIT
            materialize_tasks,                   // SORT
            materialize_tasks,                   // MATERIALIZE
            usize::from(materialize_tasks != 0), // GET
            bin(self.left_chunks()),             // LEFT
            bin(right_chunks),                   // RIGHT
            0,                                   // DONE
        ];
        // Accumulate task counts so we can find boundaries reliably.
        let mut stage_begin = Vec::with_capacity(stage_tasks.len());
        let mut begin = 0;
        for &tasks in &stage_tasks {
            stage_begin.push(begin);
            begin += tasks;
        }
        GroupSchedule {
            stage: Stage::Sort,
            stage_tasks,
            stage_begin,
            per_thread,
            next_task: 0,
        }
    }

    /// Moves to the next stage once the current one completed (`TryPrepareNextStage`).
    fn try_prepare_next_stage(&self, schedule: &mut GroupSchedule) -> bool {
        let done = |counter: &AtomicUsize, stage: Stage| {
            counter.load(Ordering::Acquire) >= schedule.stage_tasks[stage as usize]
        };
        let next = match schedule.stage {
            Stage::Init => Stage::Sort,
            Stage::Sort if done(&self.sorted, Stage::Sort) => Stage::Materialize,
            Stage::Materialize if done(&self.materialized, Stage::Materialize) => Stage::Get,
            Stage::Get if done(&self.gotten, Stage::Get) => Stage::Left,
            Stage::Left if done(&self.left_completed, Stage::Left) => {
                if schedule.stage_tasks[Stage::Right as usize] != 0 {
                    Stage::Right
                } else {
                    Stage::Done
                }
            }
            Stage::Right if done(&self.right_completed, Stage::Right) => Stage::Done,
            Stage::Done => return true,
            _ => return false,
        };
        schedule.stage = next;
        true
    }

    /// The next task of the group's current stage, if any (`TryNextTask`).
    fn try_next_task(&self, schedule: &mut GroupSchedule) -> Option<AsOfSourceTask> {
        let task_count = schedule.stage_begin[Stage::Done as usize];
        if schedule.next_task >= task_count {
            return None;
        }
        // Search for where we are in the task list.
        let mut stage = Stage::Done;
        let mut thread_idx = 0;
        for index in Stage::Init as usize..=Stage::Done as usize {
            if schedule.next_task < schedule.stage_begin[index] {
                stage = STAGES[index - 1];
                thread_idx = schedule.next_task - schedule.stage_begin[stage as usize];
                break;
            }
        }
        if stage != schedule.stage {
            return None;
        }
        let per_thread = schedule.per_thread;
        let range = |max: usize| {
            let begin = thread_idx * per_thread;
            (begin, (begin + per_thread).min(max))
        };
        let (begin_idx, end_idx) = match stage {
            Stage::Sort | Stage::Materialize => range(self.left_chunks() + self.right_chunks()),
            Stage::Get => (0, 1),
            Stage::Left if lock(&self.left_group).is_some() => range(self.left_chunks()),
            Stage::Right if lock(&self.right_group).is_some() => range(self.right_chunks()),
            _ => (0, 0),
        };
        schedule.next_task += 1;
        Some(AsOfSourceTask {
            stage,
            group_idx: self.group_idx,
            begin_idx,
            end_idx,
        })
    }

    /// Releases the sorted sides once their readers are done (`FinishTask`, under the
    /// lock); returns whether every task of the group finished.
    fn finish_task(&self, schedule: &GroupSchedule, task: &AsOfSourceTask) -> bool {
        let stage_done = |counter: &AtomicUsize| {
            counter.load(Ordering::Acquire) >= schedule.stage_tasks[task.stage as usize]
        };
        match task.stage {
            Stage::Left if stage_done(&self.left_completed) => {
                *lock(&self.left_group) = None;
                if !self.is_right_outer() {
                    *lock(&self.right_group) = None;
                }
            }
            Stage::Right if stage_done(&self.right_completed) => *lock(&self.right_group) = None,
            _ => {}
        }
        let completed = self.materialized.load(Ordering::Acquire)
            + self.gotten.load(Ordering::Acquire)
            + self.left_completed.load(Ordering::Acquire)
            + self.right_completed.load(Ordering::Acquire);
        completed >= schedule.stage_begin[Stage::Done as usize]
    }
}

/// What the source hands out, under its lock.
#[derive(Debug)]
struct SourceSchedule {
    groups: Vec<GroupSchedule>,
    /// The groups being worked on, in the order they were started.
    active_groups: Vec<usize>,
    /// The next group to start (an index into `partition_blocks`).
    next_group: usize,
}

/// The shared state of the source (DuckDB's `AsOfGlobalSourceState`).
#[derive(Debug)]
pub struct AsOfSourceState {
    sink: SinkState,
    groups: Vec<AsOfHashGroup>,
    /// The groups, largest first: (maximum chunks, group).
    partition_blocks: Vec<(usize, usize)>,
    schedule: Mutex<SourceSchedule>,
    /// Signalled when a stage completes or a task fails.
    changed: Condvar,
    total_tasks: usize,
    started: AtomicUsize,
    finished: AtomicUsize,
    stopped: AtomicBool,
}

impl AsOfSourceState {
    fn sink_state(&self) -> Result<&AsOfGlobalSinkState> {
        super::sink_state::<AsOfGlobalSinkState>(&self.sink, "ASOF join")
    }

    fn has_more_tasks(&self) -> bool {
        !self.stopped.load(Ordering::Acquire) && self.started.load(Ordering::Acquire) < self.total_tasks
    }

    fn has_unfinished_tasks(&self) -> bool {
        !self.stopped.load(Ordering::Acquire) && self.finished.load(Ordering::Acquire) < self.total_tasks
    }

    /// Counts `task` as finished and frees what its group no longer needs (`FinishTask`,
    /// under the lock).
    fn finish_task(&self, schedule: &mut SourceSchedule, task: &AsOfSourceTask) {
        self.finished.fetch_add(1, Ordering::AcqRel);
        let group = task.group_idx;
        if self.groups[group].finish_task(&schedule.groups[group], task) {
            // Remove it from the active groups.
            schedule.active_groups.retain(|&active| active != group);
        }
    }

    /// Counts the completion of `task` in its group's stage counters (the bookkeeping of
    /// `TryAssignTask`) and finishes it.
    fn complete_task(&self, task: &AsOfSourceTask) {
        let group = &self.groups[task.group_idx];
        let counter = match task.stage {
            Stage::Sort => Some(&group.sorted),
            Stage::Materialize => Some(&group.materialized),
            Stage::Get => Some(&group.gotten),
            Stage::Left => Some(&group.left_completed),
            Stage::Right => Some(&group.right_completed),
            Stage::Init | Stage::Done => None,
        };
        if let Some(counter) = counter {
            counter.fetch_add(1, Ordering::AcqRel);
        }
        let mut schedule = lock(&self.schedule);
        self.finish_task(&mut schedule, task);
        self.changed.notify_all();
    }

    /// The next task: from the active groups first, else from the next group to start
    /// (`TryNextTask`).
    fn try_next_task(&self) -> Option<AsOfSourceTask> {
        let mut schedule = lock(&self.schedule);
        if !self.has_more_tasks() {
            return None;
        }
        let schedule = &mut *schedule;
        // Run through the active groups looking for one that can assign a task.
        for &group in &schedule.active_groups {
            let group_schedule = &mut schedule.groups[group];
            if self.groups[group].try_prepare_next_stage(group_schedule) {
                self.changed.notify_all();
            }
            if let Some(task) = self.groups[group].try_next_task(group_schedule) {
                self.started.fetch_add(1, Ordering::AcqRel);
                return Some(task);
            }
        }
        // All active groups are busy or blocked, so start the next one (if any).
        while schedule.next_group < self.partition_blocks.len() {
            let group = self.partition_blocks[schedule.next_group].1;
            schedule.next_group += 1;
            schedule.active_groups.push(group);
            let group_schedule = &mut schedule.groups[group];
            if self.groups[group].try_prepare_next_stage(group_schedule) {
                self.changed.notify_all();
            }
            let Some(task) = self.groups[group].try_next_task(group_schedule) else {
                // The group has no tasks (empty?).
                continue;
            };
            self.started.fetch_add(1, Ordering::AcqRel);
            return Some(task);
        }
        None
    }
}

/// The comparison of a column of the partition prefix of sort keys
/// (`SortKeyPrefixComparisonType`).
#[derive(Clone, Copy, Debug)]
enum PrefixColumn {
    /// Its validity byte plus the value's width.
    Fixed(usize),
    /// Up to and including the first NUL byte.
    Varchar,
    /// As wide as decoding says.
    Nested,
}

/// Compares the partition prefix of two sort keys column by column
/// (`SortKeyPrefixComparison`).
#[derive(Debug)]
struct SortKeyPrefixComparison {
    columns: Vec<PrefixColumn>,
    types: Vec<LogicalType>,
    lhs: Vec<u8>,
    rhs: Vec<u8>,
}

/// Zero bytes after a decoded key, so comparisons past its end read zeros.
const PREFIX_PADDING: usize = 64;

impl SortKeyPrefixComparison {
    fn new(types: Vec<LogicalType>) -> Self {
        let columns = types
            .iter()
            .map(|ty| match ty.id() {
                LogicalTypeId::Varchar | LogicalTypeId::Blob => PrefixColumn::Varchar,
                LogicalTypeId::Struct | LogicalTypeId::List | LogicalTypeId::Array => PrefixColumn::Nested,
                _ => PrefixColumn::Fixed(1 + ty.physical_type().size()),
            })
            .collect();
        Self {
            columns,
            types,
            lhs: Vec::new(),
            rhs: Vec::new(),
        }
    }

    /// The index of the first column at which the keys differ (`columns.len()` if the
    /// prefixes are equal).
    fn compare<K: SortKey>(&mut self, lhs: &K, rhs: &K) -> Result<usize> {
        self.lhs.clear();
        self.rhs.clear();
        // SAFETY: the keys were read through iterators that keep their heaps pinned.
        unsafe {
            lhs.decode_into(&mut self.lhs);
            rhs.decode_into(&mut self.rhs);
        }
        self.lhs.resize(self.lhs.len() + PREFIX_PADDING, 0);
        self.rhs.resize(self.rhs.len() + PREFIX_PADDING, 0);
        // Partition keys are always sorted this way.
        let modifiers = OrderModifiers::new(OrderType::Ascending, NullOrder::NullsFirst);
        let (mut lhs_pos, mut rhs_pos) = (0, 0);
        for (index, column) in self.columns.iter().enumerate() {
            let lhs_bytes = self.lhs.get(lhs_pos..).unwrap_or(&[]);
            let rhs_bytes = self.rhs.get(rhs_pos..).unwrap_or(&[]);
            let (lhs_width, rhs_width, cmp) = match *column {
                PrefixColumn::Fixed(width) => {
                    let width = width.min(lhs_bytes.len()).min(rhs_bytes.len());
                    (width, width, lhs_bytes[..width].cmp(&rhs_bytes[..width]))
                }
                PrefixColumn::Varchar => {
                    // Include the first null byte.
                    let strlen =
                        |bytes: &[u8]| bytes.iter().position(|&byte| byte == 0).unwrap_or(bytes.len());
                    let lhs_width = (1 + strlen(lhs_bytes)).min(lhs_bytes.len());
                    let rhs_width = (1 + strlen(rhs_bytes)).min(rhs_bytes.len());
                    let width = lhs_width.min(rhs_width);
                    (lhs_width, rhs_width, lhs_bytes[..width].cmp(&rhs_bytes[..width]))
                }
                PrefixColumn::Nested => {
                    let mut decoded = Vector::flat(self.types[index].clone(), 2);
                    let lhs_width = decode_sort_key_column(lhs_bytes, &mut decoded, 0, modifiers)?;
                    let rhs_width = decode_sort_key_column(rhs_bytes, &mut decoded, 1, modifiers)?;
                    let width = lhs_width.min(rhs_width);
                    let cmp = lhs_bytes[..width]
                        .cmp(&rhs_bytes[..width])
                        .then(lhs_width.cmp(&rhs_width));
                    (lhs_width, rhs_width, cmp)
                }
            };
            if cmp != CmpOrdering::Equal {
                return Ok(index);
            }
            lhs_pos += lhs_width;
            rhs_pos += rhs_width;
        }
        Ok(self.columns.len())
    }
}

/// Reads a sorted run chunk by chunk, decoding its keys and gathering its payload
/// (`AsOfPayloadScanner`).
#[derive(Debug)]
struct AsOfPayloadScanner {
    run: Arc<SortedRun>,
    block_state: ExternalBlockIteratorState,
    /// The input columns among the strategy's payload columns.
    width: usize,
    count: usize,
    base: usize,
    scanned: usize,
    chunk_idx: usize,
    decode_buffer: Vec<u8>,
}

impl AsOfPayloadScanner {
    fn new(run: Arc<SortedRun>, width: usize) -> Self {
        let block_state = ExternalBlockIteratorState::new(&run);
        let count = run.count();
        Self {
            run,
            block_state,
            width,
            count,
            base: 0,
            scanned: 0,
            chunk_idx: 0,
            decode_buffer: Vec::new(),
        }
    }

    fn scanned(&self) -> usize {
        self.scanned
    }

    fn next_size(&self) -> usize {
        (self.count - self.scanned).min(STANDARD_VECTOR_SIZE)
    }

    fn seek_block(&mut self, block_idx: usize) {
        self.chunk_idx = block_idx;
        self.base = (block_idx * STANDARD_VECTOR_SIZE).min(self.count);
        self.scanned = self.base;
    }

    /// The next chunk's input columns (`Scan`).
    fn scan(&mut self, chunk: &mut DataChunk) -> Result<()> {
        // Free the previous blocks.
        self.block_state.repin();
        self.base = self.scanned;
        let count = self.next_size();
        let run = Arc::clone(&self.run);
        rustdb_row::dispatch_sort_key!(run.key_type(), K => {
            let mut keys: Vec<K> = Vec::with_capacity(count);
            for i in 0..count {
                keys.push(self.block_state.get::<K>(&run, self.chunk_idx * STANDARD_VECTOR_SIZE + i)?);
            }
            let keep_alive = self.block_state.keep_alive();
            // SAFETY: the keys' chunks and payload stay pinned by the block state, whose
            // pins `keep_alive` holds.
            unsafe { run.scan(&keys, &mut self.decode_buffer, Some(&keep_alive), chunk)? };
        });
        let mut columns = chunk.columns().to_vec();
        columns.truncate(self.width);
        chunk.set_columns(columns, count);
        self.scanned += count;
        self.chunk_idx += 1;
        Ok(())
    }
}

/// The probe of a range of left blocks (DuckDB's `AsOfProbeBuffer`).
#[derive(Debug)]
struct AsOfProbeBuffer {
    task: AsOfSourceTask,
    left_group: Option<Arc<SortedRun>>,
    right_group: Option<Arc<SortedRun>>,
    left_itr: Option<ExternalBlockIteratorState>,
    lhs_scanner: Option<AsOfPayloadScanner>,
    lhs_payload: DataChunk,
    lhs_match_sel: Vec<u32>,
    left_outer: OuterJoinMarker,
    right_itr: Option<ExternalBlockIteratorState>,
    /// Where the right search starts (the iteration state does not know this).
    right_pos: usize,
    /// Reads the payload of matched right rows.
    rhs_itr: Option<ExternalBlockIteratorState>,
    matches: Vec<usize>,
    fetch_next_left: bool,
    decode_buffer: Vec<u8>,
}

impl AsOfProbeBuffer {
    /// Whether the task has more output (`HasMoreData`).
    fn has_more_data(&self) -> bool {
        !self.fetch_next_left || self.task.begin_idx < self.task.end_idx
    }
}

/// A thread's state of the source (DuckDB's `AsOfLocalSourceState`).
#[derive(Debug)]
pub struct AsOfLocalSourceState {
    global: Arc<AsOfSourceState>,
    /// The task this thread is working on, until it is counted as completed.
    task: Option<AsOfSourceTask>,
    probe: Option<Box<AsOfProbeBuffer>>,
    /// The right side scan of a `RIGHT` task.
    right_scanner: Option<AsOfPayloadScanner>,
    lhs_executor: ExpressionExecutor,
    prefix: SortKeyPrefixComparison,
}

impl Drop for AsOfLocalSourceState {
    fn drop(&mut self) {
        // A thread that stops early (or fails) still completes its task, so threads
        // waiting for the stage do not wait forever.
        if let Some(task) = self.task.take() {
            self.probe = None;
            self.right_scanner = None;
            self.global.complete_task(&task);
        }
    }
}

impl AsOfLocalSourceState {
    /// Whether the current task is done (`TaskFinished`).
    fn task_finished(&self) -> bool {
        let Some(task) = &self.task else {
            return true;
        };
        if task.stage == Stage::Left {
            return self.probe.as_ref().is_none_or(|probe| !probe.has_more_data());
        }
        task.begin_idx >= task.end_idx
    }
}

impl PhysicalAsOfJoin {
    /// Takes the next task, completing the previous one (`TryAssignTask`).
    fn try_assign_task(&self, gsource: &AsOfSourceState, local: &mut AsOfLocalSourceState) -> bool {
        // Because downstream operators may be using our internal buffers, we can't
        // "finish" a task until we are about to get the next one.
        if let Some(task) = local.task.take() {
            // `EndLeftScan` / `EndRightScan`: drop the readers first.
            local.probe = None;
            local.right_scanner = None;
            gsource.complete_task(&task);
        }
        let Some(task) = gsource.try_next_task() else {
            return false;
        };
        local.task = Some(task);
        match task.stage {
            Stage::Left => local.probe = Some(Box::new(self.begin_left_scan(gsource, task))),
            Stage::Right => local.right_scanner = self.begin_right_scan(gsource, &task),
            _ => {}
        }
        true
    }

    /// Sets up the probe of a left task (`BeginLeftScan`).
    fn begin_left_scan(&self, gsource: &AsOfSourceState, task: AsOfSourceTask) -> AsOfProbeBuffer {
        let group = &gsource.groups[task.group_idx];
        let right_group = lock(&group.right_group).clone();
        let left_group = lock(&group.left_group).clone();
        let mut left_outer = OuterJoinMarker::new(is_left_outer_join(self.join_type));
        left_outer.initialize(STANDARD_VECTOR_SIZE);
        let mut probe = AsOfProbeBuffer {
            task,
            left_group: None,
            right_group: None,
            left_itr: None,
            lhs_scanner: None,
            lhs_payload: DataChunk::default(),
            lhs_match_sel: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            left_outer,
            right_itr: None,
            right_pos: 0,
            rhs_itr: None,
            matches: vec![0; STANDARD_VECTOR_SIZE],
            fetch_next_left: true,
            decode_buffer: Vec::new(),
        };
        let Some(left_group) = left_group.filter(|run| run.count() > 0) else {
            return probe;
        };
        let mut scanner = AsOfPayloadScanner::new(Arc::clone(&left_group), self.left_types.len());
        scanner.seek_block(task.begin_idx);
        probe.lhs_scanner = Some(scanner);
        // Don't pin the payload: only the keys are compared.
        let mut left_itr = ExternalBlockIteratorState::new(&left_group);
        left_itr.set_keep_pinned(true);
        probe.left_itr = Some(left_itr);
        probe.left_group = Some(left_group);
        // Only the corresponding right side bin is probed, which may be empty; then the
        // iterator stays empty, so left matches can be emitted.
        if let Some(right_group) = right_group.filter(|run| run.count() > 0) {
            let mut right_itr = ExternalBlockIteratorState::new(&right_group);
            right_itr.set_keep_pinned(true);
            probe.right_itr = Some(right_itr);
            probe.rhs_itr = Some(ExternalBlockIteratorState::new(&right_group));
            probe.right_group = Some(right_group);
        }
        probe
    }

    /// Sets up the scan of a right task (`BeginRightScan`).
    fn begin_right_scan(
        &self,
        gsource: &AsOfSourceState,
        task: &AsOfSourceTask,
    ) -> Option<AsOfPayloadScanner> {
        let group = gsource.groups.get(task.group_idx)?;
        let right_group = lock(&group.right_group).clone()?;
        if right_group.count() == 0 {
            return None;
        }
        let mut scanner = AsOfPayloadScanner::new(right_group, self.right_types.len());
        scanner.seek_block(task.begin_idx);
        Some(scanner)
    }

    /// Runs (a step of) the current task (`ExecuteTask`).
    fn execute_task(
        &self,
        gsource: &AsOfSourceState,
        local: &mut AsOfLocalSourceState,
        chunk: &mut DataChunk,
    ) -> Result<()> {
        let Some(task) = &mut local.task else {
            return Ok(());
        };
        let gsink = gsource.sink_state()?;
        let group = &gsource.groups[task.group_idx];
        // Left or right?
        let child = usize::from(task.begin_idx >= group.left_chunks());
        match task.stage {
            Stage::Sort => {
                gsink.strategies[child].sort_column_data(&gsink.states[child], task.group_idx)?;
                task.begin_idx = task.end_idx;
            }
            Stage::Materialize => {
                gsink.strategies[child].materialize_sorted_run(&gsink.states[child], task.group_idx)?;
                task.begin_idx = task.end_idx;
            }
            Stage::Get => {
                for child in 0..2 {
                    // Don't get children that don't exist.
                    let chunks = if child == 0 {
                        group.left_chunks()
                    } else {
                        group.right_chunks()
                    };
                    if chunks == 0 {
                        continue;
                    }
                    let run = gsink.strategies[child].get_sorted_run(&gsink.states[child], task.group_idx)?;
                    if let Some(run) = run {
                        let slot = if child == 0 {
                            &group.left_group
                        } else {
                            &group.right_group
                        };
                        *lock(slot) = Some(Arc::new(run));
                    }
                }
                task.begin_idx = task.end_idx;
            }
            Stage::Left => {
                let Some(probe) = &mut local.probe else {
                    task.begin_idx = task.end_idx;
                    return Ok(());
                };
                while probe.has_more_data() {
                    self.probe_get_data(group, probe, &mut local.lhs_executor, &mut local.prefix, chunk)?;
                    if !chunk.is_empty() {
                        break;
                    }
                }
                task.begin_idx = probe.task.begin_idx;
            }
            Stage::Right => self.execute_right_task(group, task, &mut local.right_scanner, chunk)?,
            Stage::Init | Stage::Done => {
                return Err(Error::internal("Invalid state for AsOf Task"));
            }
        }
        Ok(())
    }

    /// The output of the next left block, or of its unmatched rows
    /// (`AsOfProbeBuffer::GetData`). May leave `chunk` empty.
    fn probe_get_data(
        &self,
        group: &AsOfHashGroup,
        probe: &mut AsOfProbeBuffer,
        executor: &mut ExpressionExecutor,
        prefix: &mut SortKeyPrefixComparison,
        chunk: &mut DataChunk,
    ) -> Result<()> {
        // Handle dangling left join results from the current chunk.
        if !probe.fetch_next_left {
            probe.fetch_next_left = true;
            if probe.left_outer.enabled() {
                // Before moving to the next chunk, output the rows without a match.
                probe
                    .left_outer
                    .construct_left_join_result(&probe.lhs_payload, &self.right_types, chunk);
                probe.left_outer.reset();
            }
            return Ok(());
        }
        // Stop if there is no more data.
        if probe.task.begin_idx >= probe.task.end_idx {
            return Ok(());
        }
        match self.join_type {
            // Simple joins have at most STANDARD_VECTOR_SIZE matches per chunk.
            JoinType::Semi | JoinType::Anti => {
                self.resolve_join(probe, prefix)?;
                self.scan_left(probe, executor)?;
                let mut found = vec![false; probe.lhs_payload.len()];
                for &row in &probe.lhs_match_sel {
                    found[row as usize] = true;
                }
                construct_semi_or_anti_join_result(
                    &probe.lhs_payload,
                    chunk,
                    &found,
                    self.join_type == JoinType::Semi,
                );
            }
            JoinType::Left | JoinType::Inner | JoinType::Right | JoinType::Outer => {
                self.resolve_complex_join(group, probe, executor, prefix, chunk)?;
            }
            _ => return Err(Error::not_implemented("Unimplemented type for as-of join!")),
        }
        Ok(())
    }

    /// Finds the match of every row of the next left block: the last right row sorting
    /// before it, if its partition values are equal (`ResolveJoin`).
    fn resolve_join(&self, probe: &mut AsOfProbeBuffer, prefix: &mut SortKeyPrefixComparison) -> Result<()> {
        probe.lhs_match_sel.clear();
        // If there was no right partition, there are no matches.
        let (Some(left_group), Some(right_group)) = (&probe.left_group, &probe.right_group) else {
            return Ok(());
        };
        let key_type = left_group.key_type();
        if key_type != right_group.key_type() {
            return Err(Error::internal("ASOF join sides with different sort keys"));
        }
        let left_group = Arc::clone(left_group);
        let right_group = Arc::clone(right_group);
        rustdb_row::dispatch_sort_key!(key_type, K => self.resolve_join_typed::<K>(
            probe,
            &left_group,
            &right_group,
            prefix,
        ))
    }

    fn resolve_join_typed<K: SortKey>(
        &self,
        probe: &mut AsOfProbeBuffer,
        left_group: &SortedRun,
        right_group: &SortedRun,
        prefix: &mut SortKeyPrefixComparison,
    ) -> Result<()> {
        let (Some(left_itr), Some(right_itr), Some(scanner)) =
            (&mut probe.left_itr, &mut probe.right_itr, &probe.lhs_scanner)
        else {
            return Ok(());
        };
        left_itr.set_keep_pinned(true);
        right_itr.set_keep_pinned(true);
        let count = scanner.next_size();
        let left_base = scanner.scanned();
        let right_count = right_group.count();
        let strict = self.strict;
        // Searching for right <= left.
        for i in 0..count {
            // If right > left, there is no match.
            let left_pos = left_base + i;
            let left_key = left_itr.get::<K>(left_group, left_pos)?;
            if !key_before(
                &right_itr.get::<K>(right_group, probe.right_pos)?,
                &left_key,
                strict,
            ) {
                continue;
            }
            // Exponential search forward for a non-matching value (to avoid thrashing the
            // block manager on large probes).
            let mut bound = 1;
            let begin = probe.right_pos;
            while begin + bound < right_count {
                if key_before(
                    &right_itr.get::<K>(right_group, begin + bound)?,
                    &left_key,
                    strict,
                ) {
                    // If right <= left, jump ahead.
                    bound *= 2;
                } else {
                    break;
                }
            }
            // Binary search for the first non-matching value; the one before it (which
            // we know exists) is the match.
            let mut first = begin + bound / 2;
            let mut last = (begin + bound).min(right_count);
            while first < last {
                let mid = first + (last - first) / 2;
                if key_before(&right_itr.get::<K>(right_group, mid)?, &left_key, strict) {
                    // If right <= left, new lower bound.
                    first = mid + 1;
                } else {
                    last = mid;
                }
            }
            first -= 1;
            probe.right_pos = first;
            // Check the partitions for strict equality.
            if !prefix.columns.is_empty() {
                let right_key = right_itr.get::<K>(right_group, probe.right_pos)?;
                if prefix.compare(&left_key, &right_key)? < prefix.columns.len() {
                    continue;
                }
            }
            // Emit the match.
            probe.matches[i] = first;
            probe.lhs_match_sel.push(i as u32);
        }
        Ok(())
    }

    /// Reads the next left block and drops matches of rows with NULL keys (`ScanLeft`).
    fn scan_left(&self, probe: &mut AsOfProbeBuffer, executor: &mut ExpressionExecutor) -> Result<()> {
        let scanner = probe
            .lhs_scanner
            .as_mut()
            .ok_or_else(|| Error::internal("ASOF probe without left rows"))?;
        // Scan the next sorted chunk.
        scanner.scan(&mut probe.lhs_payload)?;
        probe.task.begin_idx += 1;
        // Compute the join keys and combine their NULLs.
        let mut keys = DataChunk::default();
        executor.execute(&probe.lhs_payload, &mut keys)?;
        let count = probe.lhs_payload.len();
        let mut valid = ValidityMask::all_valid();
        for &column in &self.null_sensitive {
            let key = keys.column(column);
            let validity = key.validity();
            if validity.is_all_valid() {
                continue;
            }
            let sel = key.sel();
            for row in 0..count {
                if !validity.is_valid(sel.slot(row)) {
                    valid.set_invalid(row, count);
                }
            }
        }
        // Filter out NULL matches.
        if !valid.is_all_valid() {
            probe.lhs_match_sel.retain(|&row| valid.is_valid(row as usize));
        }
        Ok(())
    }

    /// Joins the next left block with its matches (`ResolveComplexJoin`).
    fn resolve_complex_join(
        &self,
        group: &AsOfHashGroup,
        probe: &mut AsOfProbeBuffer,
        executor: &mut ExpressionExecutor,
        prefix: &mut SortKeyPrefixComparison,
        chunk: &mut DataChunk,
    ) -> Result<()> {
        // Perform the actual join.
        self.resolve_join(probe, prefix)?;
        // Scan the left values (after comparing keys) and filter out the left NULLs.
        self.scan_left(probe, executor)?;
        // Extract the right input columns of the matches.
        let matched: Vec<u32> = probe
            .lhs_match_sel
            .iter()
            .map(|&row| probe.matches[row as usize] as u32)
            .collect();
        let mut rhs_input = DataChunk::default();
        match (&probe.right_group, &mut probe.rhs_itr) {
            (Some(right_group), Some(rhs_itr)) if !matched.is_empty() => {
                rhs_itr.repin();
                rustdb_row::dispatch_sort_key!(right_group.key_type(), K => {
                    let mut keys: Vec<K> = Vec::with_capacity(matched.len());
                    for &row in &matched {
                        keys.push(rhs_itr.get::<K>(right_group, row as usize)?);
                    }
                    let keep_alive = rhs_itr.keep_alive();
                    // SAFETY: the keys' chunks and payload stay pinned by the iteration
                    // state, whose pins `keep_alive` holds.
                    unsafe { right_group.scan(&keys, &mut probe.decode_buffer, Some(&keep_alive), &mut rhs_input)? };
                });
                let mut columns = rhs_input.columns().to_vec();
                columns.truncate(self.right_types.len());
                rhs_input.set_columns(columns, matched.len());
            }
            _ => rhs_input.set_columns(null_columns(&self.right_types), 0),
        }
        // Slice the left payload into the result and reference the right payload.
        let mut columns = slice_columns(&probe.lhs_payload, &probe.lhs_match_sel);
        columns.extend(rhs_input.into_columns());
        chunk.set_columns(columns, matched.len());
        // Update the match masks for the rows we ended up with.
        probe.left_outer.reset();
        for &row in &probe.lhs_match_sel {
            probe.left_outer.set_match(row as usize);
        }
        if group.right_outer.enabled() {
            for &row in &matched {
                group.right_outer.set_match(row as usize);
            }
        }
        // For a left join, come back for the unmatched rows.
        probe.fetch_next_left = !probe.left_outer.enabled();
        Ok(())
    }

    /// The unmatched right rows of the task's next block (`ExecuteRightTask`).
    fn execute_right_task(
        &self,
        group: &AsOfHashGroup,
        task: &mut AsOfSourceTask,
        scanner: &mut Option<AsOfPayloadScanner>,
        chunk: &mut DataChunk,
    ) -> Result<()> {
        let Some(right_scanner) = scanner else {
            task.begin_idx = task.end_idx;
            return Ok(());
        };
        while task.begin_idx < task.end_idx {
            let rhs_position = right_scanner.scanned();
            let mut rhs_chunk = DataChunk::default();
            right_scanner.scan(&mut rhs_chunk)?;
            task.begin_idx += 1;
            // Which rows didn't find a match?
            group
                .right_outer
                .construct_right_join_result(&rhs_chunk, rhs_position, &self.left_types, chunk);
            if !chunk.is_empty() {
                return Ok(());
            }
        }
        // Exhausted the task data.
        *scanner = None;
        Ok(())
    }
}

impl Source for PhysicalAsOfJoin {
    type Global = Arc<AsOfSourceState>;
    type Local = AsOfLocalSourceState;

    /// Pivots both sides' hash groups into `AsOfHashGroup`s and builds the task list,
    /// largest groups first (`AsOfGlobalSourceState`, `CreateTaskList`).
    fn global_source_state(
        &self,
        ctx: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<Arc<AsOfSourceState>> {
        let sink = sink.ok_or_else(|| Error::internal("ASOF join source requires its sink state"))?;
        let gsink = super::sink_state::<AsOfGlobalSinkState>(sink, "ASOF join")?;
        let lhs_groups = gsink.strategies[0].hash_groups(&gsink.states[0]);
        let rhs_groups = gsink.strategies[1].hash_groups(&gsink.states[1]);
        let group_count = lhs_groups.len().max(rhs_groups.len());
        let right_outer = matches!(self.join_type, JoinType::Right | JoinType::Outer);
        let groups: Vec<AsOfHashGroup> = (0..group_count)
            .map(|group_idx| {
                let right_stats = rhs_groups.get(group_idx).copied().unwrap_or_default();
                let mut marker = OuterJoinMarker::new(right_outer);
                marker.initialize(right_stats.count);
                AsOfHashGroup {
                    group_idx,
                    left_stats: lhs_groups.get(group_idx).copied().unwrap_or_default(),
                    right_stats,
                    left_group: Mutex::new(None),
                    right_group: Mutex::new(None),
                    right_outer: marker,
                    sorted: AtomicUsize::new(0),
                    materialized: AtomicUsize::new(0),
                    gotten: AtomicUsize::new(0),
                    left_completed: AtomicUsize::new(0),
                    right_completed: AtomicUsize::new(0),
                }
            })
            .collect();
        // Sort the groups from largest to smallest, counting chunks, not rows (otherwise
        // left and right raggedness could give the wrong answer).
        let mut partition_blocks: Vec<(usize, usize)> = groups
            .iter()
            .map(|group| (group.maximum_chunks(), group.group_idx))
            .collect();
        partition_blocks.sort_by(|a, b| b.cmp(a));
        // Schedule the largest group on as many threads as possible.
        let threads = ctx.env.threads().max(1);
        let per_thread = partition_blocks
            .first()
            .map_or(0, |&(blocks, _)| blocks.div_ceil(threads));
        let mut schedules: Vec<Option<GroupSchedule>> = (0..group_count).map(|_| None).collect();
        let mut total_tasks = 0;
        if per_thread > 0 {
            for &(_, group) in &partition_blocks {
                let schedule = groups[group].init_tasks(per_thread);
                total_tasks += schedule.stage_begin[Stage::Done as usize];
                schedules[group] = Some(schedule);
            }
        }
        let schedules = schedules
            .into_iter()
            .map(|schedule| {
                schedule.unwrap_or(GroupSchedule {
                    stage: Stage::Done,
                    stage_tasks: vec![0; STAGES.len()],
                    stage_begin: vec![0; STAGES.len()],
                    per_thread: 1,
                    next_task: 0,
                })
            })
            .collect();
        Ok(Arc::new(AsOfSourceState {
            sink: Arc::clone(sink),
            groups,
            partition_blocks,
            schedule: Mutex::new(SourceSchedule {
                groups: schedules,
                active_groups: Vec::new(),
                next_group: 0,
            }),
            changed: Condvar::new(),
            total_tasks,
            started: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
        }))
    }

    fn local_source_state(
        &self,
        _: &ExecutionContext,
        global: &Arc<AsOfSourceState>,
    ) -> Result<AsOfLocalSourceState> {
        let lhs_expressions = self
            .conditions
            .iter()
            .map(|condition| condition.left.clone())
            .collect();
        // If there are equality predicates, set up the prefix comparison.
        let prefix_types = self.conditions[..self.conditions.len() - 1]
            .iter()
            .map(|condition| condition.left.return_type.clone())
            .collect();
        Ok(AsOfLocalSourceState {
            global: Arc::clone(global),
            task: None,
            probe: None,
            right_scanner: None,
            lhs_executor: ExpressionExecutor::new(lhs_expressions),
            prefix: SortKeyPrefixComparison::new(prefix_types),
        })
    }

    /// Any call must produce rows unless the operator is done, so this loops until it has
    /// rows, waiting for stages other threads are still working on (`GetDataInternal`).
    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &Arc<AsOfSourceState>,
        local: &mut AsOfLocalSourceState,
    ) -> Result<SourceResult> {
        let gsource = global.as_ref();
        chunk.set_columns(Vec::new(), 0);
        while gsource.has_unfinished_tasks() && chunk.is_empty() {
            ctx.check_interrupted()?;
            let assigned = !local.task_finished() || self.try_assign_task(gsource, local);
            let step = if assigned {
                self.execute_task(gsource, local, chunk).map(|()| true)
            } else {
                Ok(false)
            };
            match step {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    gsource.stopped.store(true, Ordering::Release);
                    gsource.changed.notify_all();
                    return Err(error);
                }
            }
            let schedule = lock(&gsource.schedule);
            if !gsource.has_more_tasks() {
                // No more tasks: exit.
                gsource.changed.notify_all();
                break;
            }
            // There are more tasks, but we can't execute them yet.
            drop(
                gsource
                    .changed
                    .wait_timeout(schedule, WAIT_INTERVAL)
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
        Ok(if chunk.is_empty() {
            SourceResult::Finished
        } else {
            SourceResult::HaveMoreOutput
        })
    }

    fn max_threads(&self, global: &Arc<AsOfSourceState>) -> usize {
        global.total_tasks.max(1)
    }

    fn progress(&self, global: &Arc<AsOfSourceState>) -> Option<f64> {
        (global.total_tasks > 0)
            .then(|| global.finished.load(Ordering::Relaxed) as f64 / global.total_tasks as f64)
    }
}
