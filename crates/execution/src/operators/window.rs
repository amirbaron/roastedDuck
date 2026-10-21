//! Window functions over partitions and orderings (DuckDB's `PhysicalWindow`).
//!
//! The sink hands its input (plus computed PARTITION BY / ORDER BY keys) to a
//! [`SortStrategy`], which radix-partitions it into hash groups. The source works through
//! the hash groups, largest first, in DuckDB's `WindowHashGroup` stages, as tasks shared
//! by all threads: `SORT` sorts a group, `MATERIALIZE` merges it into a buffer-managed
//! collection of rows (which can spill), `MASK` computes its partition and peer boundary
//! masks, `SINK` materialises the shared argument expressions and feeds the executors,
//! `FINALIZE` lets them build their structures, and `GETDATA` evaluates the functions
//! chunk by chunk. Each stage of a group has one task per block range; the largest group
//! gets as many tasks as there are threads. Sinking into and finalizing the executors'
//! global states runs in the stage's first task (they are built single-threaded); the
//! other stages run in parallel.

use rustdb_common::tree_renderer::RenderParams;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_row::sort::{SortColumn, SortConfig, SortStrategy, StrategyGlobalState, StrategyLocalState};
use rustdb_row::{PinProperties, ScanState, TupleDataCollection};
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::operator::{
    ErasedSink, ErasedSource, OperatorPartitionData, OperatorPartitionInfo, OrderPreservationType,
    PhysicalOperator, PhysicalRef, SinkState,
};
use crate::window::WindowExpression;
use crate::window::collection::{WindowCollection, delta_flags};
use crate::window::executor::{ExecutorGlobalState, ExecutorLocalState, WindowExecutor, WindowGroup};
use crate::window::mask::{BITS_PER_ENTRY, WindowMask};
use crate::window::order_modifiers;
use crate::window::shared_expressions::WindowSharedExpressions;
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// How long a thread without a task waits before checking for an interrupt again.
const WAIT_INTERVAL: Duration = Duration::from_millis(10);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The window operator: computes window functions sharing one partitioning and ordering.
#[derive(Debug)]
pub struct PhysicalWindow {
    types: Vec<LogicalType>,
    /// The window functions, in output order.
    select_list: Vec<WindowExpression>,
    /// The function with the longest ORDER BY: it defines the sort.
    order_idx: usize,
    /// Whether some function has neither PARTITION BY nor ORDER BY (input order matters).
    is_order_dependent: bool,
    estimated_cardinality: usize,
    children: [PhysicalRef; 1],
    /// Columns of the input.
    input_width: usize,
    /// Key expressions that are not plain input columns; computed into extra columns.
    sort_exprs: Vec<Expression>,
    /// The sorted data: input columns, then the computed keys.
    payload_types: Vec<LogicalType>,
    /// Payload columns of the PARTITION BY keys followed by the ORDER BY keys.
    sort_ids: Vec<usize>,
    /// The executors and their shared expressions.
    executors: Vec<WindowExecutor>,
    shared: WindowSharedExpressions,
}

impl PhysicalWindow {
    /// A window operator over `child` computing `select_list` (all with the same
    /// partitioning and prefix-compatible orderings).
    pub fn new(
        types: Vec<LogicalType>,
        select_list: Vec<Expression>,
        estimated_cardinality: usize,
        child: PhysicalRef,
    ) -> Result<Self> {
        let select_list = select_list
            .into_iter()
            .map(WindowExpression::from_expression)
            .collect::<Result<Vec<_>>>()?;
        let mut order_idx = 0;
        let mut max_orders = 0;
        let mut is_order_dependent = false;
        for (index, wexpr) in select_list.iter().enumerate() {
            if wexpr.window.partitions.is_empty() && wexpr.window.orders.is_empty() {
                is_order_dependent = true;
            }
            if wexpr.window.orders.len() > max_orders {
                order_idx = index;
                max_orders = wexpr.window.orders.len();
            }
        }
        let over = select_list
            .get(order_idx)
            .ok_or_else(|| Error::internal("window operator without window functions"))?;
        let input_types = child.types().to_vec();
        let input_width = input_types.len();
        // Key expressions that are not references are materialised as payload columns
        // (DuckDB's `HashedSort` / `FullSort` constructors).
        let mut payload_types = input_types;
        let mut sort_exprs = Vec::new();
        let mut sort_ids = Vec::new();
        let keys = over
            .window
            .partitions
            .iter()
            .chain(over.window.orders.iter().map(|order| &order.expression));
        for key in keys {
            if let ExpressionKind::Reference(reference) = &key.kind {
                sort_ids.push(reference.index as usize);
            } else {
                sort_ids.push(payload_types.len());
                payload_types.push(key.return_type.clone());
                sort_exprs.push(key.clone());
            }
        }
        let mut shared = WindowSharedExpressions::default();
        let executors = select_list
            .iter()
            .map(|wexpr| WindowExecutor::new(wexpr.clone(), &mut shared))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            types,
            select_list,
            order_idx,
            is_order_dependent,
            estimated_cardinality,
            children: [child],
            input_width,
            sort_exprs,
            payload_types,
            sort_ids,
            executors,
            shared,
        })
    }

    /// The window functions computed by this operator.
    pub fn select_list(&self) -> &[WindowExpression] {
        &self.select_list
    }

    /// Whether the result depends on the input order (DuckDB's `is_order_dependent`).
    pub fn is_order_dependent(&self) -> bool {
        self.is_order_dependent
    }

    fn over(&self) -> &WindowExpression {
        &self.select_list[self.order_idx]
    }

    /// The sort strategy for the function with the longest ordering
    /// (DuckDB's `SortStrategy::Factory`).
    fn create_strategy(&self, ctx: &ExecutionContext) -> Result<SortStrategy> {
        let over = &self.over().window;
        let partition_count = over.partitions.len();
        let partition_columns = self.sort_ids[..partition_count].to_vec();
        let orders = over
            .orders
            .iter()
            .zip(&self.sort_ids[partition_count..])
            .map(|(order, &column)| {
                Ok(SortColumn {
                    column,
                    modifiers: order_modifiers(order)?,
                    stats: None,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        SortStrategy::new(
            ctx.env.buffer_manager.clone(),
            partition_columns,
            &[],
            orders,
            self.payload_types.clone(),
            self.estimated_cardinality,
            false,
        )
    }
}

impl PhysicalOperator for PhysicalWindow {
    fn name(&self) -> &'static str {
        "WINDOW"
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
    /// Input order only matters to functions without PARTITION BY and ORDER BY.
    fn parallel_sink(&self) -> bool {
        !self.is_order_dependent
    }
    fn sink_order_dependent(&self) -> bool {
        self.is_order_dependent
    }
    /// Hash groups are evaluated by as many threads.
    fn parallel_source(&self) -> bool {
        true
    }
    /// `PhysicalWindow::SupportsPartitioning`: the order can only be preserved without
    /// PARTITION BY (work stealing between groups breaks batch order).
    fn supports_partitioning(&self, partition_info: &OperatorPartitionInfo) -> bool {
        !partition_info.requires_partition_columns() && self.over().window.partitions.is_empty()
    }
    /// `PhysicalWindow::SourceOrder`: partitions leave the order undefined, an ORDER BY
    /// fixes it.
    fn source_order(&self) -> OrderPreservationType {
        let over = &self.over().window;
        if !over.partitions.is_empty() {
            OrderPreservationType::NoOrder
        } else if over.orders.is_empty() {
            OrderPreservationType::InsertionOrder
        } else {
            OrderPreservationType::FixedOrder
        }
    }

    fn params_to_string(&self, _: u64) -> RenderParams {
        let mut result = RenderParams::new();
        let projections: Vec<&str> = self.select_list.iter().map(|w| w.name.as_str()).collect();
        result.insert("Projections".to_owned(), projections.join("\n"));
        result
    }
}

/// The sink's shared state (DuckDB's `WindowGlobalSinkState`).
#[derive(Debug)]
pub struct WindowGlobalSinkState {
    strategy: SortStrategy,
    sink: StrategyGlobalState,
}

/// A thread's sink state: its part of the strategy and the key expression executor.
#[derive(Debug)]
pub struct WindowLocalSinkState {
    strategy: StrategyLocalState,
    keys: Option<ExpressionExecutor>,
}

impl Sink for PhysicalWindow {
    type Global = WindowGlobalSinkState;
    type Local = WindowLocalSinkState;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<WindowGlobalSinkState> {
        let strategy = self.create_strategy(ctx)?;
        let sink = strategy.global_state(SortConfig {
            threads: ctx.env.threads(),
            connections: 1,
            ..SortConfig::default()
        })?;
        Ok(WindowGlobalSinkState { strategy, sink })
    }

    fn local_sink_state(
        &self,
        _: &ExecutionContext,
        global: &WindowGlobalSinkState,
    ) -> Result<WindowLocalSinkState> {
        Ok(WindowLocalSinkState {
            strategy: global.strategy.local_state(),
            keys: (!self.sort_exprs.is_empty()).then(|| ExpressionExecutor::new(self.sort_exprs.clone())),
        })
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &WindowGlobalSinkState,
        local: &mut WindowLocalSinkState,
    ) -> Result<SinkResult> {
        match &mut local.keys {
            None => global.strategy.sink(chunk, &global.sink, &mut local.strategy)?,
            Some(executor) => {
                let mut keys = DataChunk::default();
                executor.execute(chunk, &mut keys)?;
                let mut columns = chunk.columns().to_vec();
                columns.extend(keys.into_columns());
                let payload = DataChunk::new(columns, chunk.len());
                global
                    .strategy
                    .sink(&payload, &global.sink, &mut local.strategy)?;
            }
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(
        &self,
        _: &ExecutionContext,
        global: &WindowGlobalSinkState,
        local: WindowLocalSinkState,
    ) -> Result<()> {
        global.strategy.combine(&global.sink, local.strategy)
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut WindowGlobalSinkState) -> Result<SinkFinalize> {
        Ok(if global.strategy.finalize(&mut global.sink)? {
            SinkFinalize::Ready
        } else {
            SinkFinalize::NoOutputPossible
        })
    }
}

/// The stages of a hash group (`WindowGroupStage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Sort,
    Materialize,
    Mask,
    Sink,
    Finalize,
    GetData,
    Done,
}

const STAGES: [Stage; 7] = [
    Stage::Sort,
    Stage::Materialize,
    Stage::Mask,
    Stage::Sink,
    Stage::Finalize,
    Stage::GetData,
    Stage::Done,
];

/// A unit of source work (`WindowSourceTask`).
#[derive(Clone, Copy, Debug)]
struct WindowSourceTask {
    stage: Stage,
    /// The hash group (its index among the source's groups).
    group: usize,
    /// The task's number within its stage.
    thread_idx: usize,
    /// The block range.
    begin_idx: usize,
    end_idx: usize,
}

/// The boundary masks of a hash group: partitions, and peers per key width.
#[derive(Debug)]
struct GroupMasks {
    partition: WindowMask,
    orders: BTreeMap<usize, WindowMask>,
}

/// What `SINK` and `FINALIZE` build: the collection and the executors' states.
#[derive(Debug)]
struct GroupBuild {
    collection: WindowCollection,
    global_states: Vec<ExecutorGlobalState>,
    local_states: Vec<ExecutorLocalState>,
}

/// A finalized hash group, read by the `GETDATA` tasks.
#[derive(Debug)]
struct GroupReady {
    masks: GroupMasks,
    collection: WindowCollection,
    global_states: Vec<ExecutorGlobalState>,
}

/// One hash group (DuckDB's `WindowHashGroup`).
#[derive(Debug)]
struct WindowHashGroup {
    /// The strategy's hash bin.
    hash_bin: usize,
    /// Rows and blocks of the group.
    count: usize,
    blocks: usize,
    /// The output batch index the group starts at.
    batch_base: usize,
    /// The sorted rows (from `MATERIALIZE`).
    rows: OnceLock<TupleDataCollection>,
    /// The first row of every chunk of `rows`, and the row count.
    chunk_starts: OnceLock<Vec<usize>>,
    /// The masks while `MASK` tasks fill them in.
    masks: Mutex<Option<GroupMasks>>,
    build: Mutex<Option<GroupBuild>>,
    ready: OnceLock<GroupReady>,
    /// Blocks whose materialization finished (`materialized`).
    materialized: AtomicUsize,
    /// Completed tasks per stage, and of all stages.
    stage_completed: [AtomicUsize; 6],
    completed: AtomicUsize,
}

/// The scheduling of one hash group, under the source's lock.
#[derive(Debug)]
struct GroupSchedule {
    stage: Stage,
    per_thread: usize,
    group_threads: usize,
    next_task: usize,
}

impl GroupSchedule {
    /// Tasks of every stage (`GetTaskCount`).
    fn task_count(&self) -> usize {
        self.group_threads * (Stage::Done as usize)
    }
}

impl WindowHashGroup {
    /// Moves to the next stage once every task of the current one completed
    /// (`TryPrepareNextStage`).
    fn try_prepare_next_stage(&self, schedule: &mut GroupSchedule) -> bool {
        let stage = schedule.stage;
        if stage >= Stage::GetData {
            // Never block in GETDATA.
            return true;
        }
        if self.stage_completed[stage as usize].load(Ordering::Acquire) < schedule.group_threads {
            return false;
        }
        if stage == Stage::Materialize && self.rows.get().is_none() {
            return false;
        }
        schedule.stage = STAGES[stage as usize + 1];
        true
    }

    /// The next task of the group's current stage (`TryNextTask`).
    fn try_next_task(&self, group: usize, schedule: &mut GroupSchedule) -> Option<WindowSourceTask> {
        if schedule.next_task >= schedule.task_count() {
            return None;
        }
        let stage = STAGES[schedule.next_task / schedule.group_threads];
        if stage != schedule.stage {
            return None;
        }
        let thread_idx = schedule.next_task % schedule.group_threads;
        let begin_idx = thread_idx * schedule.per_thread;
        let end_idx = (begin_idx + schedule.per_thread).min(self.blocks);
        schedule.next_task += 1;
        Some(WindowSourceTask {
            stage,
            group,
            thread_idx,
            begin_idx,
            end_idx,
        })
    }

    fn rows(&self) -> Result<&TupleDataCollection> {
        self.rows
            .get()
            .ok_or_else(|| Error::internal("window hash group read before it was materialized"))
    }

    fn chunk_starts(&self) -> Result<&[usize]> {
        self.chunk_starts
            .get()
            .map(Vec::as_slice)
            .ok_or_else(|| Error::internal("window hash group read before it was materialized"))
    }

    fn ready(&self) -> Result<&GroupReady> {
        self.ready
            .get()
            .ok_or_else(|| Error::internal("window hash group read before it was finalized"))
    }
}

/// What the source hands out, under its lock.
#[derive(Debug)]
struct SourceSchedule {
    groups: Vec<GroupSchedule>,
    /// The groups being worked on, in the order they were started.
    active_groups: Vec<usize>,
    /// The next group to start (an index into the groups, largest first).
    next_group: usize,
}

/// The source's shared state (DuckDB's `WindowGlobalSourceState`).
#[derive(Debug)]
pub struct WindowGlobalSourceState {
    sink: SinkState,
    /// The non-empty hash groups, largest first (DuckDB's `partition_blocks`; ties by
    /// descending bin).
    groups: Vec<Mutex<Option<Arc<WindowHashGroup>>>>,
    schedule: Mutex<SourceSchedule>,
    /// Signalled when a stage completes or a task fails.
    changed: Condvar,
    total_blocks: usize,
    total_tasks: usize,
    started: AtomicUsize,
    finished: AtomicUsize,
    stopped: AtomicBool,
}

impl WindowGlobalSourceState {
    /// The finalized sink state.
    fn sink_state(&self) -> Result<&WindowGlobalSinkState> {
        self.sink
            .downcast_ref::<WindowGlobalSinkState>()
            .ok_or_else(|| Error::internal("WINDOW sink state has the wrong type"))
    }

    fn group(&self, group: usize) -> Result<Arc<WindowHashGroup>> {
        lock(&self.groups[group])
            .clone()
            .ok_or_else(|| Error::internal("window hash group used after it finished"))
    }

    fn has_more_tasks(&self) -> bool {
        !self.stopped.load(Ordering::Acquire) && self.started.load(Ordering::Acquire) < self.total_tasks
    }

    fn has_unfinished_tasks(&self) -> bool {
        !self.stopped.load(Ordering::Acquire) && self.finished.load(Ordering::Acquire) < self.total_tasks
    }

    /// Counts `task` as finished; a group whose tasks all finished is released
    /// (`FinishTask`).
    fn finish_task(&self, task: &WindowSourceTask) {
        let mut schedule = lock(&self.schedule);
        if let Ok(group) = self.group(task.group) {
            group.stage_completed[task.stage as usize].fetch_add(1, Ordering::AcqRel);
            let completed = group.completed.fetch_add(1, Ordering::AcqRel) + 1;
            if completed >= schedule.groups[task.group].task_count() {
                *lock(&self.groups[task.group]) = None;
                // Remove it from the active groups.
                schedule.active_groups.retain(|&active| active != task.group);
            }
        }
        self.finished.fetch_add(1, Ordering::AcqRel);
        drop(schedule);
        self.changed.notify_all();
    }

    /// The next task: from the active groups first, else from the next group to start
    /// (`TryNextTask`).
    fn try_next_task(&self) -> Option<WindowSourceTask> {
        let mut schedule = lock(&self.schedule);
        if !self.has_more_tasks() {
            return None;
        }
        let schedule = &mut *schedule;
        // Run through the active groups looking for one that can assign a task.
        for &group in &schedule.active_groups {
            let Ok(hash_group) = self.group(group) else {
                continue;
            };
            let group_schedule = &mut schedule.groups[group];
            if hash_group.try_prepare_next_stage(group_schedule) {
                self.changed.notify_all();
            }
            if let Some(task) = hash_group.try_next_task(group, group_schedule) {
                self.started.fetch_add(1, Ordering::AcqRel);
                return Some(task);
            }
        }
        // All active groups are busy or blocked, so start the next one (if any).
        while schedule.next_group < self.groups.len() {
            let group = schedule.next_group;
            schedule.next_group += 1;
            schedule.active_groups.push(group);
            let Ok(hash_group) = self.group(group) else {
                continue;
            };
            let group_schedule = &mut schedule.groups[group];
            if hash_group.try_prepare_next_stage(group_schedule) {
                self.changed.notify_all();
            }
            let Some(task) = hash_group.try_next_task(group, group_schedule) else {
                // The group has no tasks (empty?).
                continue;
            };
            self.started.fetch_add(1, Ordering::AcqRel);
            return Some(task);
        }
        None
    }
}

/// A thread's source state (DuckDB's `WindowLocalSourceState`).
#[derive(Debug)]
pub struct WindowLocalSourceState {
    global: Arc<WindowGlobalSourceState>,
    /// The task this thread is working on, until it is counted as completed.
    task: Option<WindowSourceTask>,
    /// The `GETDATA` task's group, scan and executor states.
    group: Option<Arc<WindowHashGroup>>,
    scan: Option<ScanState>,
    local_states: Vec<ExecutorLocalState>,
    /// The batch index of the last chunk (output reordering).
    batch_index: u64,
    eval: Option<ExpressionExecutor>,
}

impl Drop for WindowLocalSourceState {
    fn drop(&mut self) {
        // A thread that stops early (or fails) still completes its task, so threads
        // waiting for the stage do not wait forever.
        if let Some(task) = self.task.take() {
            self.scan = None;
            self.group = None;
            self.global.finish_task(&task);
        }
    }
}

impl WindowLocalSourceState {
    /// Whether the current task is done (`TaskFinished`).
    fn task_finished(&self) -> bool {
        self.task
            .as_ref()
            .is_none_or(|task| task.begin_idx == task.end_idx)
    }
}

impl PhysicalWindow {
    /// The widths of the peer masks: each executor's partition plus ORDER BY width.
    fn mask_widths(&self) -> Vec<usize> {
        let mut widths: Vec<usize> = self.executors.iter().map(WindowExecutor::key_count).collect();
        widths.sort_unstable();
        widths.dedup();
        widths
    }

    /// Takes the next task, completing the previous one (`TryAssignTask`).
    fn try_assign_task(&self, gsource: &WindowGlobalSourceState, local: &mut WindowLocalSourceState) -> bool {
        // Because downstream operators may be using our internal buffers, we can't
        // "finish" a task until we are about to get the next one.
        local.scan = None;
        local.group = None;
        if let Some(task) = local.task.take() {
            gsource.finish_task(&task);
        }
        let Some(task) = gsource.try_next_task() else {
            return false;
        };
        local.task = Some(task);
        true
    }

    /// Runs (a step of) the current task (`ExecuteTask`).
    fn execute_task(
        &self,
        ctx: &ExecutionContext,
        gsource: &WindowGlobalSourceState,
        local: &mut WindowLocalSourceState,
        chunk: &mut DataChunk,
    ) -> Result<()> {
        let Some(task) = local.task else {
            return Ok(());
        };
        let group = gsource.group(task.group)?;
        let sink = gsource.sink_state()?;
        match task.stage {
            Stage::Sort => sink.strategy.sort_column_data(&sink.sink, group.hash_bin)?,
            Stage::Materialize => self.materialize(sink, &group, task.end_idx - task.begin_idx)?,
            Stage::Mask => self.compute_masks(&group, task.begin_idx, task.end_idx)?,
            Stage::Sink => {
                if task.thread_idx == 0 {
                    self.sink_group(ctx, &group)?;
                }
            }
            Stage::Finalize => {
                if task.thread_idx == 0 {
                    self.finalize_group(&group)?;
                }
            }
            Stage::GetData => {
                self.get_data(ctx, gsource, &group, local, chunk)?;
                return Ok(());
            }
            Stage::Done => return Err(Error::internal("Invalid window source state.")),
        }
        // Mark this range as done.
        if let Some(task) = &mut local.task {
            task.begin_idx = task.end_idx;
        }
        Ok(())
    }

    /// Merges the group into rows; the task that completes it takes them
    /// (`WindowLocalSourceState::Materialize`).
    fn materialize(
        &self,
        sink: &WindowGlobalSinkState,
        group: &WindowHashGroup,
        blocks: usize,
    ) -> Result<()> {
        sink.strategy
            .materialize_column_data(&sink.sink, group.hash_bin)?;
        // Mark this range as done.
        let materialized = group.materialized.fetch_add(blocks, Ordering::AcqRel) + blocks;
        if materialized < group.blocks {
            return Ok(());
        }
        // There is no good place to read the column data; doing it twice splits it.
        let _guard = lock(&group.masks);
        if group.rows.get().is_some() {
            return Ok(());
        }
        if let Some(rows) = sink.strategy.get_column_data(&sink.sink, group.hash_bin)?
            && rows.count() == group.count
        {
            let mut starts = Vec::with_capacity(rows.chunk_count() + 1);
            let mut total = 0;
            starts.push(0);
            for count in rows.chunk_row_counts() {
                total += count;
                starts.push(total);
            }
            let _ = group.chunk_starts.set(starts);
            let _ = group.rows.set(rows);
            return Ok(());
        }
        Err(Error::internal("window hash group rows went missing"))
    }

    /// The partition and peer boundaries of blocks `block_begin..block_end`
    /// (`WindowHashGroup::ComputeMasks`).
    fn compute_masks(&self, group: &WindowHashGroup, block_begin: usize, block_end: usize) -> Result<()> {
        let rows = group.rows()?;
        let starts = group.chunk_starts()?;
        let chunk_count = starts.len() - 1;
        let block_end = block_end.min(chunk_count);
        if block_begin >= block_end {
            return Ok(());
        }
        let over = &self.over().window;
        let partition_count = over.partitions.len();
        let widths = self.mask_widths();
        // Scan back one block for the boundary comparison.
        let first = block_begin.saturating_sub(1);
        let mut chunks = Vec::with_capacity(block_end - first);
        let sorted = !(over.partitions.is_empty() && over.orders.is_empty());
        if sorted {
            let mut scan = rows.initialize_scan_columns(self.sort_ids.clone(), PinProperties::UnpinAfterDone);
            rows.seek(&mut scan, first);
            for _ in first..block_end {
                let mut chunk = DataChunk::default();
                if !rows.scan(&mut scan, &mut chunk)? {
                    break;
                }
                chunks.push(chunk);
            }
        }
        let offset = starts[first];
        let begin_row = starts[block_begin];
        let end_row = starts[block_end];
        let lens: Vec<usize> = (first..block_end)
            .map(|chunk| starts[chunk + 1] - starts[chunk])
            .collect();
        let mut flags = vec![false; end_row - offset];
        if block_begin == 0
            && let Some(first) = flags.first_mut()
        {
            // The first row starts a group.
            *first = true;
        }
        // The masks of wider keys add the boundaries of further key columns.
        let mut range_masks: BTreeMap<usize, Vec<bool>> = BTreeMap::new();
        let mut width = 0;
        for target in std::iter::once(partition_count).chain(widths.iter().copied()) {
            let target = target.max(partition_count);
            if sorted && target > width {
                let keys: Vec<Vec<&Vector>> = chunks
                    .iter()
                    .map(|chunk| (width..target).map(|column| chunk.column(column)).collect())
                    .collect();
                delta_flags(&keys, &lens, &mut flags)?;
                width = target;
            }
            range_masks
                .entry(target)
                .or_insert_with(|| flags[begin_row - offset..].to_vec());
        }
        // Write the range into the group's masks (whole entries: blocks are multiples of
        // the entry width, except at the end).
        let mut masks = lock(&group.masks);
        let masks = masks.get_or_insert_with(|| GroupMasks {
            partition: WindowMask::all_invalid(group.count),
            orders: widths
                .iter()
                .map(|&width| (width, WindowMask::all_invalid(group.count)))
                .collect(),
        });
        let set = |mask: &mut WindowMask, flags: &[bool]| {
            for (row, _) in flags.iter().enumerate().filter(|&(_, &flag)| flag) {
                mask.set_valid(begin_row + row);
            }
        };
        if let Some(flags) = range_masks.get(&partition_count) {
            set(&mut masks.partition, flags);
        }
        for (&width, mask) in &mut masks.orders {
            if let Some(flags) = range_masks.get(&width.max(partition_count)) {
                set(mask, flags);
            }
        }
        debug_assert!(begin_row % BITS_PER_ENTRY == 0 || block_begin == 0 || !sorted);
        Ok(())
    }

    /// Materialises the shared argument expressions and feeds every chunk of the group to
    /// the executors (DuckDB's `SINK` stage, here in one task).
    fn sink_group(&self, ctx: &ExecutionContext, group: &WindowHashGroup) -> Result<()> {
        let count = group.count;
        let masks = lock(&group.masks);
        let partition_mask = masks
            .as_ref()
            .map_or_else(|| WindowMask::all_invalid(count), |masks| masks.partition.clone());
        drop(masks);
        // These can be large, so they are built only now.
        let mut collection = WindowCollection::new(&self.shared.coll.types(), count);
        let mut global_states = self
            .executors
            .iter()
            .map(|executor| executor.global_state(ctx, count, &partition_mask))
            .collect::<Result<Vec<_>>>()?;
        let mut local_states = self
            .executors
            .iter()
            .zip(&global_states)
            .map(|(executor, global)| executor.local_state(ctx, global, count))
            .collect::<Result<Vec<_>>>()?;
        let mut coll_exec = (!self.shared.coll.is_empty()).then(|| self.shared.coll.executor());
        let mut sink_exec = (!self.shared.sink.is_empty()).then(|| self.shared.sink.executor());
        let rows = group.rows()?;
        let mut scan =
            rows.initialize_scan_columns((0..self.input_width).collect(), PinProperties::UnpinAfterDone);
        let mut input_idx = 0;
        loop {
            let mut input = DataChunk::default();
            if !rows.scan(&mut scan, &mut input)? {
                break;
            }
            // Compute the fully materialised expressions.
            let mut coll_chunk = DataChunk::new(Vec::new(), input.len());
            if let Some(executor) = &mut coll_exec {
                executor.execute(&input, &mut coll_chunk)?;
                collection.append(&coll_chunk, input_idx)?;
            }
            // Compute the sink expressions.
            let mut sink_chunk = DataChunk::new(Vec::new(), input.len());
            if let Some(executor) = &mut sink_exec {
                executor.execute(&input, &mut sink_chunk)?;
            }
            for ((executor, global), local) in self
                .executors
                .iter()
                .zip(&mut global_states)
                .zip(&mut local_states)
            {
                executor.sink(global, local, &sink_chunk, &coll_chunk, input_idx)?;
            }
            input_idx += input.len();
        }
        *lock(&group.build) = Some(GroupBuild {
            collection,
            global_states,
            local_states,
        });
        Ok(())
    }

    /// Completes the collection, then lets the executors build their structures (DuckDB's
    /// `FINALIZE` stage, here in one task).
    fn finalize_group(&self, group: &WindowHashGroup) -> Result<()> {
        let Some(mut build) = lock(&group.build).take() else {
            return Err(Error::internal("window hash group finalized before it was sunk"));
        };
        let masks = lock(&group.masks).take().unwrap_or_else(|| GroupMasks {
            partition: WindowMask::all_invalid(group.count),
            orders: self
                .mask_widths()
                .into_iter()
                .map(|width| (width, WindowMask::all_invalid(group.count)))
                .collect(),
        });
        build.collection.combine(&self.shared.coll_validity);
        for ((executor, global), local) in self
            .executors
            .iter()
            .zip(&mut build.global_states)
            .zip(&mut build.local_states)
        {
            let window_group = WindowGroup {
                count: group.count,
                partition_mask: &masks.partition,
                order_mask: masks
                    .orders
                    .get(&executor.key_count())
                    .ok_or_else(|| Error::internal("window peer mask missing"))?,
                collection: &build.collection,
            };
            executor.finalize(global, local, window_group)?;
        }
        group
            .ready
            .set(GroupReady {
                masks,
                collection: build.collection,
                global_states: build.global_states,
            })
            .map_err(|_| Error::internal("window hash group finalized twice"))
    }

    /// Evaluates the functions for the task's next chunk (DuckDB's `GETDATA` stage).
    fn get_data(
        &self,
        ctx: &ExecutionContext,
        gsource: &WindowGlobalSourceState,
        group: &Arc<WindowHashGroup>,
        local: &mut WindowLocalSourceState,
        output: &mut DataChunk,
    ) -> Result<()> {
        let Some(task) = &mut local.task else {
            return Ok(());
        };
        let ready = group.ready()?;
        let rows = group.rows()?;
        let starts = group.chunk_starts()?;
        if task.begin_idx >= starts.len() - 1 {
            task.begin_idx = task.end_idx;
            return Ok(());
        }
        let same_group = local
            .group
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, group));
        if !same_group || local.scan.is_none() {
            // A new task: scan from its first block, with this thread's executor states.
            let mut scan =
                rows.initialize_scan_columns((0..self.input_width).collect(), PinProperties::UnpinAfterDone);
            rows.seek(&mut scan, task.begin_idx);
            local.scan = Some(scan);
            local.local_states = self
                .executors
                .iter()
                .zip(&ready.global_states)
                .map(|(executor, global)| executor.local_state(ctx, global, group.count))
                .collect::<Result<Vec<_>>>()?;
            local.group = Some(Arc::clone(group));
        }
        local.batch_index = (group.batch_base + task.begin_idx) as u64;
        let position = starts[task.begin_idx];
        let mut input = DataChunk::default();
        if let Some(scan) = &mut local.scan {
            rows.scan(scan, &mut input)?;
        }
        let mut eval_chunk = DataChunk::new(Vec::new(), input.len());
        if let Some(executor) = &mut local.eval {
            executor.execute(&input, &mut eval_chunk)?;
        }
        let mut columns: Vec<Vector> = input.columns().to_vec();
        for ((executor, global), local_state) in self
            .executors
            .iter()
            .zip(&ready.global_states)
            .zip(&mut local.local_states)
        {
            let window_group = WindowGroup {
                count: group.count,
                partition_mask: &ready.masks.partition,
                order_mask: ready
                    .masks
                    .orders
                    .get(&executor.key_count())
                    .ok_or_else(|| Error::internal("window peer mask missing"))?,
                collection: &ready.collection,
            };
            columns.push(executor.evaluate(global, local_state, window_group, position, &eval_chunk)?);
        }
        output.set_columns(columns, input.len());
        // Move to the next chunk.
        task.begin_idx += 1;
        let _ = gsource;
        Ok(())
    }
}

impl Source for PhysicalWindow {
    type Global = Arc<WindowGlobalSourceState>;
    type Local = Mutex<WindowLocalSourceState>;

    /// The hash groups and their tasks, largest group first; the largest group gets as
    /// many tasks as there are threads (`WindowGlobalSourceState`, `CreateTaskList`).
    fn global_source_state(
        &self,
        ctx: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<Arc<WindowGlobalSourceState>> {
        let sink = sink.ok_or_else(|| Error::internal("WINDOW source requires its sink state"))?;
        let state = sink
            .downcast_ref::<WindowGlobalSinkState>()
            .ok_or_else(|| Error::internal("WINDOW sink state has the wrong type"))?;
        let hash_groups = state.strategy.hash_groups(&state.sink);
        // Batch bases follow the bins; the groups are processed largest first.
        let mut total_blocks = 0;
        let mut groups: Vec<(usize, usize, usize, usize)> = Vec::new();
        for (bin, chunk_row) in hash_groups.iter().enumerate() {
            if chunk_row.chunks == 0 {
                continue;
            }
            groups.push((chunk_row.chunks, bin, chunk_row.count, total_blocks));
            total_blocks += chunk_row.chunks;
        }
        // Largest groups first; equal sizes by descending bin (DuckDB sorts (blocks, bin)
        // pairs with std::greater).
        groups.sort_unstable_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)));
        let threads = ctx.env.threads().max(1);
        let max_blocks = groups.first().map_or(0, |group| group.0);
        // Masks are computed in parallel, so each thread's rows must cover whole mask
        // entries (only matters if STANDARD_VECTOR_SIZE < 64).
        let aligned_scale = (BITS_PER_ENTRY / STANDARD_VECTOR_SIZE).max(1);
        let aligned_count = max_blocks.div_ceil(aligned_scale);
        let per_thread = aligned_scale * aligned_count.div_ceil(threads);
        let mut schedules = Vec::with_capacity(groups.len());
        let mut hash_groups_out = Vec::with_capacity(groups.len());
        let mut total_tasks = 0;
        for &(blocks, bin, count, batch_base) in &groups {
            let group_threads = blocks.div_ceil(per_thread.max(1));
            let schedule = GroupSchedule {
                stage: Stage::Sort,
                per_thread: per_thread.max(1),
                group_threads,
                next_task: 0,
            };
            total_tasks += schedule.task_count();
            schedules.push(schedule);
            hash_groups_out.push(Mutex::new(Some(Arc::new(WindowHashGroup {
                hash_bin: bin,
                count,
                blocks,
                batch_base,
                rows: OnceLock::new(),
                chunk_starts: OnceLock::new(),
                masks: Mutex::new(None),
                build: Mutex::new(None),
                ready: OnceLock::new(),
                materialized: AtomicUsize::new(0),
                stage_completed: Default::default(),
                completed: AtomicUsize::new(0),
            }))));
        }
        Ok(Arc::new(WindowGlobalSourceState {
            sink: Arc::clone(sink),
            groups: hash_groups_out,
            schedule: Mutex::new(SourceSchedule {
                groups: schedules,
                active_groups: Vec::new(),
                next_group: 0,
            }),
            changed: Condvar::new(),
            total_blocks,
            total_tasks,
            started: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
        }))
    }

    fn local_source_state(
        &self,
        _: &ExecutionContext,
        global: &Arc<WindowGlobalSourceState>,
    ) -> Result<Self::Local> {
        Ok(Mutex::new(WindowLocalSourceState {
            global: Arc::clone(global),
            task: None,
            group: None,
            scan: None,
            local_states: Vec::new(),
            batch_index: 0,
            eval: (!self.shared.eval.is_empty()).then(|| self.shared.eval.executor()),
        }))
    }

    /// Any call must produce rows unless the operator is done, so this loops until it has
    /// rows, waiting for stages other threads are still working on (`GetDataInternal`).
    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &Arc<WindowGlobalSourceState>,
        local: &mut Self::Local,
    ) -> Result<SourceResult> {
        let local = local.get_mut().unwrap_or_else(PoisonError::into_inner);
        let gsource = global.as_ref();
        chunk.set_columns(Vec::new(), 0);
        while gsource.has_unfinished_tasks() && chunk.is_empty() {
            ctx.check_interrupted()?;
            if !local.task_finished() || self.try_assign_task(gsource, local) {
                if let Err(error) = self.execute_task(ctx, gsource, local, chunk) {
                    gsource.stopped.store(true, Ordering::Release);
                    gsource.changed.notify_all();
                    return Err(error);
                }
                continue;
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

    fn max_threads(&self, global: &Arc<WindowGlobalSourceState>) -> usize {
        global.total_blocks.max(1)
    }

    fn progress(&self, global: &Arc<WindowGlobalSourceState>) -> Option<f64> {
        (global.total_tasks > 0)
            .then(|| global.finished.load(Ordering::Relaxed) as f64 / global.total_tasks as f64)
    }

    /// The batch index of the last chunk: its group's base plus its block
    /// (`GetPartitionData`).
    fn partition_data(
        &self,
        _: &Arc<WindowGlobalSourceState>,
        local: &Self::Local,
        partition_info: &OperatorPartitionInfo,
    ) -> Result<OperatorPartitionData> {
        if partition_info.requires_partition_columns() {
            return Err(Error::internal(
                "PhysicalWindow::GetPartitionData: partition columns not supported",
            ));
        }
        let local = local.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(OperatorPartitionData {
            batch_index: local.batch_index,
            partition_data: Vec::new(),
        })
    }
}
