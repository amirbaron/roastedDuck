//! Runs a physical plan on the task scheduler (DuckDB's `Executor`, `executor.cpp`).
//!
//! The plan is split into pipelines ([`PipelineBuilder`]); every meta pipeline gets a
//! stack of events (initialize → run the pipelines → prepare finish → finish → complete)
//! linked by the pipelines' dependencies. Events without dependencies are scheduled
//! first; the thread running the query works on the query's tasks until the last meta
//! pipeline completes, while the scheduler's workers help. A [`QueryExecution`] lets the
//! caller drive that work one task at a time (pending query results).

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roastedduck_common::{Error, Result};

use super::event::{Event, EventKind};
use super::interrupt::InterruptState;
use super::meta_pipeline::{MetaPipelineType, PipelineBuilder, PipelineGraph, operator_key};
use super::pipeline::{Pipeline, SharedState};
use super::pipeline_executor::{
    PARTIAL_CHUNK_COUNT, PipelineExecuteResult, PipelineExecutor, PipelineStates,
};
use super::task_scheduler::{ProducerToken, Task, TaskExecutionMode, TaskExecutionResult};
use crate::operator::{OperatorPartitionInfo, PhysicalRef, SinkFinalize, SinkState};
use crate::{CardinalityEstimates, ClientEnv, ExecutionContext, QueryClient, QueryProfiler};

/// Settings of one query run.
#[derive(Clone, Debug)]
pub struct QueryOptions {
    /// Records every operator's rows and time (`EXPLAIN ANALYZE`).
    pub profiler: Option<Arc<QueryProfiler>>,
    /// The `preserve_insertion_order` setting.
    pub preserve_insertion_order: bool,
    /// The client's interrupt flag (DuckDB's `ClientContext::interrupted`): once set, the
    /// query's tasks fail with `INTERRUPT Error: Interrupted!`.
    pub client_interrupted: Option<Arc<AtomicBool>>,
    /// The planner's estimated cardinality of the plan's operators, which weighs the
    /// pipelines' progress.
    pub estimates: CardinalityEstimates,
    /// Whether the plan's root is a streaming result collector, whose sink blocks until
    /// the client fetches (`HasStreamingResultCollector`).
    pub streaming_result: bool,
    /// The `debug_force_external` setting (`ClientConfig::force_external`).
    pub force_external: bool,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            profiler: None,
            preserve_insertion_order: true,
            client_interrupted: None,
            estimates: CardinalityEstimates::default(),
            streaming_result: false,
            force_external: false,
        }
    }
}

/// How far a query got: `done` of `total` rows, the sum of its pipelines' progress
/// weighted by their sources' estimated cardinality (DuckDB's `ProgressData`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProgressData {
    pub done: f64,
    pub total: f64,
}

impl ProgressData {
    /// The progress as a percentage (`ProgressData::ProgressDone`, times 100).
    pub fn percentage(&self) -> f64 {
        if self.total > 0.0 {
            self.done / self.total * 100.0
        } else {
            0.0
        }
    }
}

/// The state of a query after a step of work (DuckDB's `PendingExecutionResult` as the
/// `Executor` reports it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionStep {
    /// Every pipeline completed (`EXECUTION_FINISHED`).
    Finished,
    /// Work was done and more remains (`RESULT_NOT_READY`).
    NotReady,
    /// The remaining tasks are running on other threads (`NO_TASKS_AVAILABLE`).
    NoTasksAvailable,
    /// The streaming result collector blocked with its buffer full: the result can be
    /// fetched (`RESULT_READY`).
    ResultReady,
    /// Every task left is blocked, waiting to be rescheduled (`BLOCKED`).
    Blocked,
}

/// How long a blocked query waits for a task to be rescheduled (`Executor::WAIT_TIME`).
const WAIT_TIME: std::time::Duration = std::time::Duration::from_millis(20);

/// Tasks waiting for an operator's interrupt callback (`to_be_rescheduled_tasks`).
#[derive(Default)]
struct BlockedTasks {
    tasks: HashMap<u64, Box<dyn Task>>,
    /// Tasks whose callback came before they were descheduled: they are rescheduled as
    /// soon as they are (DuckDB's `RescheduleTask` waits for the task to be descheduled).
    rescheduled_early: HashSet<u64>,
    /// Set by `CancelTasks`: blocked tasks are dropped instead of kept.
    cancelled: bool,
}

/// A physical plan being executed (DuckDB's `Executor` after `Initialize`): the pipelines
/// are scheduled, and [`QueryExecution::execute_task`] works on the query's tasks on the
/// calling thread while the scheduler's workers help. Dropping it cancels the query.
pub struct QueryExecution {
    root: PhysicalRef,
    shared: Arc<ExecutorShared>,
    /// The task the calling thread works on (`Executor::task`). Only accessed through
    /// `&mut self`; the mutex makes the execution shareable like the connection owning it.
    current: Mutex<Option<Box<dyn Task>>>,
    /// Whether the query completed or failed.
    done: bool,
}

impl QueryExecution {
    /// Builds the pipelines of the plan rooted at `root` (a sink, e.g. a result collector)
    /// and schedules the events whose dependencies are met (`Executor::Initialize`).
    pub fn new(root: PhysicalRef, env: Arc<ClientEnv>, options: &QueryOptions) -> Result<Self> {
        let graph = PipelineBuilder::build(&root, options.preserve_insertion_order)?;
        let producer = env.scheduler.create_producer();
        let opener = roastedduck_function::file_system::opener();
        let client = Arc::new(QueryClient {
            force_external: options.force_external,
            logger: opener.as_ref().and_then(|opener| opener.logger()),
            estimates: options.estimates.clone(),
        });
        let shared = Arc::new(ExecutorShared {
            env,
            client,
            profiler: options.profiler.clone(),
            opener,
            client_interrupted: options.client_interrupted.clone(),
            producer,
            pipelines: graph.pipelines.clone(),
            interrupted: Arc::default(),
            states: Mutex::default(),
            error: Mutex::new(None),
            completed_pipelines: AtomicUsize::new(0),
            total_pipelines: AtomicUsize::new(0),
            events: Mutex::default(),
            executor_tasks: AtomicUsize::new(0),
            next_task_id: AtomicU64::new(0),
            wake_generation: AtomicU64::new(0),
            blocked: Mutex::default(),
            streaming_result: options.streaming_result,
        });
        shared.schedule(&graph);
        Ok(Self {
            root,
            shared,
            current: Mutex::new(None),
            done: false,
        })
    }

    /// The task the calling thread works on.
    fn current(&mut self) -> &mut Option<Box<dyn Task>> {
        self.current.get_mut().unwrap_or_else(PoisonError::into_inner)
    }

    /// Works on one task of the query for a while (`Executor::ExecuteTask`). A failure of
    /// any of the query's tasks cancels the others and is returned.
    pub fn execute_task(&mut self) -> Result<ExecutionStep> {
        if self.done {
            return Ok(ExecutionStep::Finished);
        }
        if let Some(error) = self.shared.take_error() {
            return Err(self.fail(error));
        }
        if self.shared.is_complete() {
            self.done = true;
            return Ok(ExecutionStep::Finished);
        }
        let shared = Arc::clone(&self.shared);
        let current = self.current();
        if current.is_none() {
            *current = shared.producer.take_task();
        }
        let Some(task) = current.as_mut() else {
            if let Some(error) = self.shared.take_error() {
                return Err(self.fail(error));
            }
            // No task to run: the others run elsewhere or are blocked.
            let blocked = lock(&self.shared.blocked);
            if blocked.tasks.is_empty() {
                return Ok(ExecutionStep::NoTasksAvailable);
            }
            if self.shared.result_collector_is_blocked(&blocked) {
                return Ok(ExecutionStep::ResultReady);
            }
            return Ok(ExecutionStep::Blocked);
        };
        match task.execute(TaskExecutionMode::ProcessPartial) {
            TaskExecutionResult::NotFinished => {}
            TaskExecutionResult::Blocked => {
                if let Some(task) = current.take() {
                    task.deschedule();
                }
            }
            TaskExecutionResult::Finished | TaskExecutionResult::Error => *current = None,
        }
        if let Some(error) = self.shared.take_error() {
            return Err(self.fail(error));
        }
        Ok(ExecutionStep::NotReady)
    }

    /// Waits for a blocked task to be rescheduled, for at most `WAIT_TIME`; returns right
    /// away when none is blocked or the result collector is, which only a fetch unblocks
    /// (`Executor::WaitForTask`).
    pub fn wait_for_blocked_task(&self) {
        let shared = &self.shared;
        {
            let blocked = lock(&shared.blocked);
            if blocked.tasks.is_empty() || shared.result_collector_is_blocked(&blocked) {
                return;
            }
        }
        shared
            .producer
            .wait_timeout(|| shared.is_complete() || shared.is_cancelled(), WAIT_TIME);
    }

    /// Whether every pipeline completed or a task failed (`Executor::ExecutionIsFinished`).
    pub fn execution_is_finished(&self) -> bool {
        self.done || self.shared.is_complete() || lock(&self.shared.error).is_some()
    }

    /// Blocks until a task of the query is queued, a task blocked, a streaming result
    /// has a chunk to read, or the query completed or failed.
    pub fn wait_for_task(&self) {
        let shared = &self.shared;
        let generation = shared.wake_generation.load(Ordering::Acquire);
        shared.producer.wait(|| {
            shared.is_complete()
                || shared.is_cancelled()
                || shared.wake_generation.load(Ordering::Acquire) != generation
                // A sink may have filled the stream buffer and descheduled
                // between the caller's poll and this generation snapshot. No
                // producer can wake us again until the client consumes a chunk.
                || !lock(&shared.blocked).tasks.is_empty()
        });
    }

    /// Runs the query to completion and returns the root's finalized sink state.
    pub fn finish(mut self) -> Result<SinkState> {
        loop {
            match self.execute_task()? {
                ExecutionStep::Finished => break,
                ExecutionStep::NotReady => {}
                ExecutionStep::NoTasksAvailable => self.wait_for_task(),
                ExecutionStep::Blocked => self.wait_for_blocked_task(),
                ExecutionStep::ResultReady => {
                    return Err(Error::internal(
                        "a streaming result's collector blocked while running the query to completion",
                    ));
                }
            }
        }
        self.shared.cancel_tasks();
        self.shared.take_finalized_sink(&self.root)
    }

    /// Runs `f` on the global state of the root sink once its pipeline is initialized: a
    /// streaming result collector hands out its chunks from it while the query runs. `f`
    /// runs under the executor's lock of the states, so the thread that completes the
    /// pipeline cannot finalize the sink meanwhile (which needs the state to itself).
    pub fn with_root_sink_state<R>(&self, f: impl FnOnce(&SinkState) -> R) -> Option<R> {
        self.shared.with_sink_state(&self.root, f)
    }

    /// The progress of the query's pipelines, or `None` while a started pipeline's source
    /// cannot tell (`Executor::GetPipelinesProgress` with `Pipeline::GetProgress`): a
    /// pipeline counts as many rows as its source is estimated to produce.
    pub fn progress(&self) -> Option<ProgressData> {
        if self.done {
            // C++ clears its active pipeline vector when ExecuteTask finishes.
            return Some(ProgressData {
                done: 0.0,
                total: 0.0,
            });
        }
        let mut progress = ProgressData {
            done: 0.0,
            // C++ also counts the root pull pipeline whose source is the result
            // collector. It stays uninitialized until the push pipelines finish.
            // Rust fetches that sink directly, so account for it without a task.
            total: self.shared.client.estimates.get(&self.root).clamp(1, 1 << 48) as f64,
        };
        for pipeline in &self.shared.pipelines {
            let cardinality = self
                .shared
                .client
                .estimates
                .get(&pipeline.source)
                .clamp(1, 1 << 48) as f64;
            if let Some(state) = pipeline.source_state() {
                let done = pipeline.source.as_source()?.progress(state.as_ref()).ok()??;
                progress.done += done.clamp(0.0, 1.0) * cardinality;
            }
            progress.total += cardinality;
        }
        // `ProgressBar::Update`: very large totals are scaled down.
        if progress.total > 1e15 {
            progress.done = progress.done / progress.total * 1e15;
            progress.total = 1e15;
        }
        Some(progress)
    }

    /// Cancels the remaining work after `error` and releases the operators' states now
    /// (a failed COPY removes its files), not when a worker thread drops its last handle
    /// on the query.
    fn fail(&mut self, error: Error) -> Error {
        self.done = true;
        *self.current() = None;
        self.shared.cancel_tasks();
        self.shared.release_runtime_states();
        error
    }
}

impl std::fmt::Debug for QueryExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryExecution")
            .field("root", &self.root.name())
            .field("shared", &self.shared)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl Drop for QueryExecution {
    fn drop(&mut self) {
        // The task this thread held is dropped first: it may hold operator states.
        *self.current() = None;
        self.shared.cancel_tasks();
        self.shared.release_runtime_states();
    }
}

/// Runs physical plans (DuckDB's `Executor`).
#[derive(Debug)]
pub struct Executor;

impl Executor {
    /// Runs the plan rooted at `root` (a sink, e.g. a result collector) to completion and
    /// returns the root's finalized sink state.
    pub fn execute(root: &PhysicalRef, env: Arc<ClientEnv>) -> Result<SinkState> {
        Self::execute_with(root, env, &QueryOptions::default())
    }

    /// [`Self::execute`], recording every operator's rows and time in `profiler`.
    pub fn execute_profiled(
        root: &PhysicalRef,
        env: Arc<ClientEnv>,
        profiler: Option<Arc<QueryProfiler>>,
    ) -> Result<SinkState> {
        Self::execute_with(
            root,
            env,
            &QueryOptions {
                profiler,
                ..QueryOptions::default()
            },
        )
    }

    /// [`Self::execute`] with explicit query settings.
    pub fn execute_with(
        root: &PhysicalRef,
        env: Arc<ClientEnv>,
        options: &QueryOptions,
    ) -> Result<SinkState> {
        QueryExecution::new(Arc::clone(root), env, options)?.finish()
    }
}

/// A sink's global state through the query.
#[derive(Debug)]
enum SinkSlot {
    /// Receiving input from the pipelines' threads.
    Building(SharedState),
    /// Taken out for `finalize`.
    Finalizing,
    /// Finalized: read by the operator's source and operator roles.
    Finalized {
        state: SharedState,
        result: SinkFinalize,
    },
}

/// The global states of the plan's operators, by operator address (DuckDB keeps them on
/// the operators: `sink_state`, `op_state`).
#[derive(Debug, Default)]
struct OperatorStates {
    sinks: HashMap<usize, SinkSlot>,
    operators: HashMap<usize, SharedState>,
}

impl OperatorStates {
    fn finalized(&self, op: &PhysicalRef) -> Option<(&SharedState, SinkFinalize)> {
        match self.sinks.get(&operator_key(op)) {
            Some(SinkSlot::Finalized { state, result }) => Some((state, *result)),
            _ => None,
        }
    }
}

/// The state of a running query shared by its events and tasks.
pub(crate) struct ExecutorShared {
    env: Arc<ClientEnv>,
    /// The client's settings, logger and the plan's estimated cardinalities.
    client: Arc<QueryClient>,
    profiler: Option<Arc<QueryProfiler>>,
    /// The client's interrupt flag, if the client can interrupt the query.
    client_interrupted: Option<Arc<AtomicBool>>,
    /// The connection's function state the query runs with: worker threads see it as
    /// the thread that started the query does, e.g. to log (the tasks' `ClientContext`).
    opener: Option<Arc<roastedduck_function::client::FunctionClient>>,
    producer: ProducerToken,
    pipelines: Vec<Arc<Pipeline>>,
    /// Set on the first error: tasks stop at their next chunk.
    interrupted: Arc<AtomicBool>,
    states: Mutex<OperatorStates>,
    error: Mutex<Option<Error>>,
    completed_pipelines: AtomicUsize,
    total_pipelines: AtomicUsize,
    /// Every event of the query (events only point to each other weakly).
    events: Mutex<Vec<Arc<Event>>>,
    /// Tasks created and not dropped yet (`executor_tasks`).
    executor_tasks: AtomicUsize,
    /// Numbers the tasks, for their interrupt states.
    next_task_id: AtomicU64,
    /// Changes when a task blocks or a streaming result's buffer gets a chunk to read:
    /// wakes the query thread waiting for tasks, as nothing is queued then.
    wake_generation: AtomicU64,
    /// Descheduled tasks (`to_be_rescheduled_tasks`).
    blocked: Mutex<BlockedTasks>,
    /// Whether the root is a streaming result collector (`HasStreamingResultCollector`).
    streaming_result: bool,
}

impl std::fmt::Debug for ExecutorShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutorShared")
            .field("pipelines", &self.pipelines.len())
            .field("completed_pipelines", &self.completed_pipelines)
            .field("total_pipelines", &self.total_pipelines)
            .finish_non_exhaustive()
    }
}

/// The events of one pipeline (DuckDB's `PipelineEventStack`).
#[derive(Clone)]
struct PipelineEventStack {
    initialize: Arc<Event>,
    pipeline: Arc<Event>,
    prepare_finish: Arc<Event>,
    finish: Arc<Event>,
    complete: Arc<Event>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ExecutorShared {
    pub fn producer(&self) -> &ProducerToken {
        &self.producer
    }

    pub fn is_cancelled(&self) -> bool {
        self.interrupted.load(Ordering::Acquire)
    }

    fn is_complete(&self) -> bool {
        self.completed_pipelines.load(Ordering::Acquire) >= self.total_pipelines.load(Ordering::Acquire)
    }

    /// Records the query's first error and interrupts its other tasks (`PushError`).
    pub fn push_error(&self, error: Error) {
        {
            let mut slot = lock(&self.error);
            if slot.is_none() {
                *slot = Some(error);
            }
        }
        self.interrupted.store(true, Ordering::Release);
        self.producer.notify();
    }

    pub fn add_event(&self, event: Arc<Event>) {
        lock(&self.events).push(event);
    }

    /// A meta pipeline completed (`CompletePipeline`).
    pub fn complete_pipeline(&self) {
        self.completed_pipelines.fetch_add(1, Ordering::AcqRel);
        if self.is_complete() {
            self.producer.notify();
        }
    }

    /// A context for a task of this query.
    fn context(&self, thread_index: usize) -> ExecutionContext {
        ExecutionContext::for_task(
            Arc::clone(&self.env),
            Arc::clone(&self.client),
            self.profiler.clone(),
            Arc::clone(&self.interrupted),
            self.client_interrupted.clone(),
            thread_index,
        )
    }

    /// The query's first error, if one was pushed (taken, so it is reported once).
    fn take_error(&self) -> Option<Error> {
        lock(&self.error).take()
    }

    /// Creates the event graph and schedules the events without dependencies
    /// (`InitializeInternal`).
    fn schedule(self: &Arc<Self>, graph: &PipelineGraph) {
        for event in &self.schedule_events(graph) {
            if !event.has_dependencies() {
                event.schedule_and_finish_if_empty();
            }
        }
    }

    /// Runs `f` on the global state of `sink` while it receives input or once it is
    /// finalized, holding the lock of the states.
    fn with_sink_state<R>(&self, sink: &PhysicalRef, f: impl FnOnce(&SharedState) -> R) -> Option<R> {
        match lock(&self.states).sinks.get(&operator_key(sink)) {
            Some(SinkSlot::Building(state) | SinkSlot::Finalized { state, .. }) => Some(f(state)),
            _ => None,
        }
    }

    /// Parks a task that blocked until its interrupt callback (`AddToBeRescheduled`); a
    /// callback that came first reschedules it right away.
    fn add_to_be_rescheduled(&self, task_id: u64, task: Box<dyn Task>) {
        let mut blocked = lock(&self.blocked);
        if blocked.cancelled {
            drop(blocked);
            drop(task);
            return;
        }
        if blocked.rescheduled_early.remove(&task_id) {
            drop(blocked);
            self.producer.schedule_tasks(vec![task]);
            return;
        }
        blocked.tasks.insert(task_id, task);
        drop(blocked);
        self.wake_query_thread();
    }

    /// Wakes the query thread waiting for tasks to look at the query again.
    pub(crate) fn wake_query_thread(&self) {
        self.wake_generation.fetch_add(1, Ordering::AcqRel);
        self.producer.notify();
    }

    /// Queues a descheduled task again (`RescheduleTask`).
    pub(crate) fn reschedule_task(&self, task_id: u64) {
        let mut blocked = lock(&self.blocked);
        if blocked.cancelled {
            return;
        }
        match blocked.tasks.remove(&task_id) {
            Some(task) => {
                drop(blocked);
                self.producer.schedule_tasks(vec![task]);
            }
            None => {
                blocked.rescheduled_early.insert(task_id);
            }
        }
    }

    /// Whether a blocked task waits for the streaming result collector, which only a
    /// fetch unblocks (`ResultCollectorIsBlocked`).
    fn result_collector_is_blocked(&self, blocked: &BlockedTasks) -> bool {
        if !self.streaming_result {
            return false;
        }
        // The result collector is always in the last pipeline.
        if self.completed_pipelines.load(Ordering::Acquire) + 1
            != self.total_pipelines.load(Ordering::Acquire)
        {
            return false;
        }
        blocked.tasks.values().any(|task| task.task_blocked_on_result())
    }

    /// Stops the query's remaining work and waits for its running tasks
    /// (`CancelTasks`): queued and blocked tasks are dropped, running ones stop at their
    /// next chunk.
    fn cancel_tasks(&self) {
        self.interrupted.store(true, Ordering::Release);
        let blocked_tasks = {
            let mut blocked = lock(&self.blocked);
            blocked.cancelled = true;
            std::mem::take(&mut blocked.tasks)
        };
        drop(blocked_tasks);
        while let Some(task) = self.producer.take_task() {
            drop(task);
        }
        self.producer
            .wait(|| self.executor_tasks.load(Ordering::Acquire) == 0);
        lock(&self.events).clear();
    }

    /// Match the executor's reset lifetime even if the last worker still owns
    /// shared task metadata after decrementing `executor_tasks` in its destructor.
    fn release_runtime_states(&self) {
        let states = std::mem::take(&mut *lock(&self.states));
        drop(states);
        for pipeline in &self.pipelines {
            pipeline.clear_source();
        }
    }

    /// Creates the event graph (`ScheduleEventsInternal` + `SchedulePipeline`) and
    /// returns its events.
    fn schedule_events(self: &Arc<Self>, graph: &PipelineGraph) -> Vec<Arc<Event>> {
        let mut events = Vec::new();
        let mut event_map: HashMap<usize, PipelineEventStack> = HashMap::new();
        let order = graph.meta_pipelines_in_order();
        self.total_pipelines.store(order.len(), Ordering::Release);
        for &meta in &order {
            let meta_pipeline = &graph.meta_pipelines[meta];
            let base_pipeline = &graph.pipelines[meta_pipeline.base()];
            let base = PipelineEventStack {
                initialize: Event::new(self, EventKind::PipelineInitialize(Arc::clone(base_pipeline))),
                pipeline: Event::new(self, EventKind::Pipeline(Arc::clone(base_pipeline))),
                prepare_finish: Event::new(self, EventKind::PipelinePrepareFinish),
                finish: Event::new(
                    self,
                    EventKind::PipelineFinish {
                        pipeline: Arc::clone(base_pipeline),
                        finalizes_again: meta_pipeline.finalizes_again_after(meta_pipeline.base()),
                    },
                ),
                complete: Event::new(
                    self,
                    EventKind::PipelineComplete {
                        complete_pipeline: true,
                    },
                ),
            };
            base.pipeline.add_dependency(&base.initialize);
            base.prepare_finish.add_dependency(&base.pipeline);
            base.finish.add_dependency(&base.prepare_finish);
            base.complete.add_dependency(&base.finish);
            events.extend([
                Arc::clone(&base.initialize),
                Arc::clone(&base.pipeline),
                Arc::clone(&base.prepare_finish),
                Arc::clone(&base.finish),
                Arc::clone(&base.complete),
            ]);
            for &id in &meta_pipeline.pipelines[1..] {
                let pipeline = &graph.pipelines[id];
                let pipeline_event = Event::new(self, EventKind::Pipeline(Arc::clone(pipeline)));
                let stack = if let Some(group) = meta_pipeline.finish_group(id) {
                    // Part of a finish group: runs after the base's finish, finishes with the
                    // group's pipeline.
                    let group_stack = event_map.get(&group).cloned().unwrap_or_else(|| base.clone());
                    pipeline_event.add_dependency(&base.finish);
                    group_stack.prepare_finish.add_dependency(&pipeline_event);
                    PipelineEventStack {
                        pipeline: Arc::clone(&pipeline_event),
                        ..group_stack
                    }
                } else if meta_pipeline.has_finish_event(id) {
                    // Its own finish event, despite going into the same sink: the sink is
                    // finalized after the base pipeline and again after this one.
                    let prepare_finish = Event::new(self, EventKind::PipelinePrepareFinish);
                    let finish = Event::new(
                        self,
                        EventKind::PipelineFinish {
                            pipeline: Arc::clone(pipeline),
                            finalizes_again: meta_pipeline.finalizes_again_after(id),
                        },
                    );
                    pipeline_event.add_dependency(&base.finish);
                    prepare_finish.add_dependency(&pipeline_event);
                    finish.add_dependency(&prepare_finish);
                    base.complete.add_dependency(&finish);
                    events.extend([Arc::clone(&prepare_finish), Arc::clone(&finish)]);
                    PipelineEventStack {
                        pipeline: Arc::clone(&pipeline_event),
                        prepare_finish,
                        finish,
                        ..base.clone()
                    }
                } else {
                    // Union branches and outer join scans feed the same sink: they run
                    // between its initialize and prepare-finish events.
                    pipeline_event.add_dependency(&base.initialize);
                    base.prepare_finish.add_dependency(&pipeline_event);
                    PipelineEventStack {
                        pipeline: Arc::clone(&pipeline_event),
                        ..base.clone()
                    }
                };
                event_map.insert(id, stack);
                events.push(pipeline_event);
            }
            event_map.insert(meta_pipeline.base(), base);
        }

        for (&id, stack) in &event_map {
            let pipeline = &graph.pipelines[id];
            for dependency in &pipeline.dependencies {
                if let Some(dependency) = event_map.get(dependency) {
                    stack.pipeline.add_dependency(&dependency.complete);
                }
            }
            for dependency in &pipeline.intra_dependencies {
                if let Some(dependency) = event_map.get(dependency) {
                    stack.pipeline.add_dependency(&dependency.pipeline);
                }
            }
        }

        // Sibling join builds finalize together: every build's prepare-finish waits for
        // all siblings to have combined, and every finalize for all prepare-finishes.
        for &meta in &order {
            let children = &graph.meta_pipelines[meta].children;
            for &child1 in children {
                let child1_meta = &graph.meta_pipelines[child1];
                if child1_meta.kind != MetaPipelineType::JoinBuild {
                    continue;
                }
                let first = &event_map[&child1_meta.base()];
                for &child2 in children {
                    let child2_meta = &graph.meta_pipelines[child2];
                    if child2_meta.kind != MetaPipelineType::JoinBuild
                        || child1 == child2
                        || child1_meta.parent != child2_meta.parent
                    {
                        continue;
                    }
                    let second = &event_map[&child2_meta.base()];
                    first.prepare_finish.add_dependency(&second.pipeline);
                    first.finish.add_dependency(&second.prepare_finish);
                }
            }
        }
        debug_assert!(events_are_acyclic(&events));
        lock(&self.events).extend(events.iter().cloned());
        events
    }

    /// Creates the global state of the pipeline's sink unless it exists (`ResetSink`).
    fn reset_sink(&self, pipeline: &Pipeline) -> Result<()> {
        let key = operator_key(&pipeline.sink);
        if lock(&self.states).sinks.contains_key(&key) {
            return Ok(());
        }
        let sink = pipeline
            .sink
            .as_sink()
            .ok_or_else(|| Error::internal("Sink of pipeline does not have IsSink set"))?;
        let state: SharedState = Arc::from(sink.global_sink_state(&self.context(0))?);
        lock(&self.states)
            .sinks
            .entry(key)
            .or_insert(SinkSlot::Building(state));
        Ok(())
    }

    /// Creates the pipeline's operator and source states (`Pipeline::Reset`) and decides
    /// on how many threads it runs (`ScheduleParallel`).
    pub fn pipeline_task_count(&self, pipeline: &Pipeline) -> Result<usize> {
        self.reset_sink(pipeline)?;
        let ctx = self.context(0);
        for op in &pipeline.operators {
            let key = operator_key(op);
            let sink_state = {
                let states = lock(&self.states);
                if states.operators.contains_key(&key) {
                    continue;
                }
                states.finalized(op).map(|(state, _)| Arc::clone(state))
            };
            let operator = op
                .as_operator()
                .ok_or_else(|| Error::internal(format!("operator {} lacks its pipeline role", op.name())))?;
            let state: SharedState = Arc::from(operator.global_operator_state(&ctx, sink_state.as_ref())?);
            lock(&self.states).operators.entry(key).or_insert(state);
        }
        let source = pipeline
            .source
            .as_source()
            .ok_or_else(|| Error::internal("Source of pipeline does not have IsSource set"))?;
        let source_state = pipeline.reset_source(|| {
            let sink_state = lock(&self.states)
                .finalized(&pipeline.source)
                .map(|(state, _)| Arc::clone(state));
            Ok(Arc::from(source.global_source_state(&ctx, sink_state.as_ref())?))
        })?;

        if !pipeline.sink.parallel_sink()
            || !pipeline.source.parallel_source()
            || !pipeline.operators.iter().all(|op| op.parallel_operator())
        {
            return Ok(1);
        }
        if pipeline.sink.required_partition_info().batch_index
            && !pipeline
                .source
                .supports_partitioning(&OperatorPartitionInfo::batch_index())
        {
            return Err(Error::internal(
                "Attempting to schedule a pipeline where the sink requires batch index but source does not support it",
            ));
        }
        let active_threads = self.env.threads();
        let mut max_threads = source.max_threads(source_state.as_ref())?.min(active_threads);
        let sink_state = self.building_sink_state(pipeline)?;
        let sink = pipeline
            .sink
            .as_sink()
            .ok_or_else(|| Error::internal("Sink of pipeline does not have IsSink set"))?;
        max_threads = sink
            .max_threads(sink_state.as_ref(), max_threads)?
            .min(active_threads);
        Ok(max_threads.max(1))
    }

    /// The global state of the pipeline's sink while it receives input.
    fn building_sink_state(&self, pipeline: &Pipeline) -> Result<SharedState> {
        match lock(&self.states).sinks.get(&operator_key(&pipeline.sink)) {
            Some(SinkSlot::Building(state)) => Ok(Arc::clone(state)),
            _ => Err(Error::internal(format!(
                "the sink {} has no state to receive input",
                pipeline.sink.name()
            ))),
        }
    }

    /// The global states one thread of `pipeline` works with.
    fn pipeline_states(&self, pipeline: &Pipeline) -> Result<PipelineStates> {
        let source = pipeline
            .source_state()
            .ok_or_else(|| Error::internal("pipeline started before it was scheduled"))?;
        let sink = self.building_sink_state(pipeline)?;
        let states = lock(&self.states);
        let mut operators = Vec::with_capacity(pipeline.operators.len());
        let mut no_output_possible = Vec::with_capacity(pipeline.operators.len());
        for op in &pipeline.operators {
            let state = states
                .operators
                .get(&operator_key(op))
                .ok_or_else(|| Error::internal("operator state missing"))?;
            operators.push(Arc::clone(state));
            no_output_possible.push(matches!(
                states.finalized(op),
                Some((_, SinkFinalize::NoOutputPossible))
            ));
        }
        Ok(PipelineStates {
            source,
            operators,
            sink,
            no_output_possible,
        })
    }

    /// Finalizes the pipeline's sink (`PipelineFinishTask`) and schedules its first
    /// parallel finalize stage, if it has one.
    /// With `finalizes_again`, more input follows (`AddFinishEvent`): the state goes back to
    /// receiving input once finalized.
    fn finalize_sink(
        self: &Arc<Self>,
        pipeline: &Arc<Pipeline>,
        event: &Arc<Event>,
        finalizes_again: bool,
    ) -> Result<()> {
        let key = operator_key(&pipeline.sink);
        let slot = lock(&self.states).sinks.insert(key, SinkSlot::Finalizing);
        let Some(SinkSlot::Building(mut state)) = slot else {
            return Err(Error::internal("Sink of pipeline does not have sink state"));
        };
        let sink = pipeline
            .sink
            .as_sink()
            .ok_or_else(|| Error::internal("Sink of pipeline does not have IsSink set"))?;
        let ctx = self.context(0);
        let global = Arc::get_mut(&mut state)
            .ok_or_else(|| Error::internal("sink state still in use when finalizing"))?;
        let result = sink.finalize(&ctx, global)?;
        let tasks = sink.finalize_tasks(&ctx, state.as_ref())?;
        let slot = if finalizes_again {
            SinkSlot::Building(state)
        } else {
            SinkSlot::Finalized { state, result }
        };
        lock(&self.states).sinks.insert(key, slot);
        if tasks > 0 {
            let stage = Event::new(
                self,
                EventKind::SinkFinalizeStage {
                    pipeline: Arc::clone(pipeline),
                    tasks,
                },
            );
            event.insert_event(self, stage);
        }
        Ok(())
    }

    /// The finalized state of the pipeline's sink (a sink finalized before more input,
    /// `AddFinishEvent`, is receiving input again).
    fn finalized_sink_state(&self, pipeline: &Pipeline) -> Result<SharedState> {
        match lock(&self.states).sinks.get(&operator_key(&pipeline.sink)) {
            Some(SinkSlot::Finalized { state, .. } | SinkSlot::Building(state)) => Ok(Arc::clone(state)),
            _ => Err(Error::internal(
                "sink finalize stage before the sink was finalized",
            )),
        }
    }

    /// The sink asks for its next finalize stage once a stage finished (the
    /// `FinishEvent` of DuckDB's sink-specific events).
    pub fn schedule_next_finalize_stage(self: &Arc<Self>, event: &Arc<Event>, pipeline: &Arc<Pipeline>) {
        let next = (|| {
            let sink = pipeline
                .sink
                .as_sink()
                .ok_or_else(|| Error::internal("Sink of pipeline does not have IsSink set"))?;
            let state = self.finalized_sink_state(pipeline)?;
            sink.finalize_tasks(&self.context(0), state.as_ref())
        })();
        match next {
            Ok(0) => {}
            Ok(tasks) => {
                let stage = Event::new(
                    self,
                    EventKind::SinkFinalizeStage {
                        pipeline: Arc::clone(pipeline),
                        tasks,
                    },
                );
                event.insert_event(self, stage);
            }
            Err(error) => self.push_error(error),
        }
    }

    fn execute_finalize_task(&self, pipeline: &Pipeline, task: usize, thread_index: usize) -> Result<()> {
        let sink = pipeline
            .sink
            .as_sink()
            .ok_or_else(|| Error::internal("Sink of pipeline does not have IsSink set"))?;
        let state = self.finalized_sink_state(pipeline)?;
        sink.execute_finalize_task(&self.context(thread_index), state.as_ref(), task)
    }

    /// The root sink's finalized state once the query completed.
    fn take_finalized_sink(&self, root: &PhysicalRef) -> Result<SinkState> {
        match lock(&self.states).sinks.remove(&operator_key(root)) {
            Some(SinkSlot::Finalized { state, .. }) => Ok(state),
            _ => Err(Error::internal("the root pipeline produced no sink state")),
        }
    }
}

/// Whether the event graph has no cycles (`VerifyScheduledEvents`).
fn events_are_acyclic(events: &[Arc<Event>]) -> bool {
    fn visit(event: &Arc<Event>, index: &HashMap<usize, usize>, state: &mut [u8]) -> bool {
        let Some(&i) = index.get(&Arc::as_ptr(event).addr()) else {
            return true;
        };
        match state[i] {
            1 => return false,
            2 => return true,
            _ => {}
        }
        state[i] = 1;
        for parent in event.parents() {
            if !visit(&parent, index, state) {
                return false;
            }
        }
        state[i] = 2;
        true
    }
    let index: HashMap<usize, usize> = events
        .iter()
        .enumerate()
        .map(|(i, event)| (Arc::as_ptr(event).addr(), i))
        .collect();
    let mut state = vec![0u8; events.len()];
    events.iter().all(|event| visit(event, &index, &mut state))
}

/// What a task does (the `ExecutorTask` subclasses of DuckDB).
pub(crate) enum TaskKind {
    /// `PipelineInitializeTask`.
    PipelineInitialize(Arc<Pipeline>),
    /// `PipelineTask`: one thread's share of a pipeline; the executor is created on the
    /// first call.
    Pipeline {
        pipeline: Arc<Pipeline>,
        executor: Option<Box<PipelineExecutor>>,
    },
    /// `PipelineFinishTask`.
    PipelineFinish {
        pipeline: Arc<Pipeline>,
        finalizes_again: bool,
    },
    /// One task of a sink's parallel finalize stage.
    SinkFinalizeStage { pipeline: Arc<Pipeline>, task: usize },
}

/// A task of a query (DuckDB's `ExecutorTask`).
pub(crate) struct ExecutorTask {
    executor: Arc<ExecutorShared>,
    event: Arc<Event>,
    kind: TaskKind,
    thread_index: usize,
    /// Identifies the task to its interrupt state.
    task_id: u64,
}

impl ExecutorTask {
    pub fn new(executor: &Arc<ExecutorShared>, event: Arc<Event>, kind: TaskKind) -> Self {
        let thread_index = executor.executor_tasks.fetch_add(1, Ordering::AcqRel);
        let task_id = executor.next_task_id.fetch_add(1, Ordering::Relaxed);
        Self {
            executor: Arc::clone(executor),
            event,
            kind,
            thread_index,
            task_id,
        }
    }

    fn execute_task(&mut self, mode: TaskExecutionMode) -> Result<TaskExecutionResult> {
        match &mut self.kind {
            TaskKind::PipelineInitialize(pipeline) => {
                self.executor.reset_sink(pipeline)?;
            }
            TaskKind::Pipeline { pipeline, executor } => {
                if executor.is_none() {
                    let states = self.executor.pipeline_states(pipeline)?;
                    // `SetTaskForInterrupts`: a blocked sink reschedules this task.
                    let mut ctx = self.executor.context(self.thread_index);
                    ctx.interrupt_state = InterruptState::task(Arc::downgrade(&self.executor), self.task_id);
                    *executor = Some(Box::new(PipelineExecutor::new(
                        ctx,
                        Arc::clone(pipeline),
                        states,
                    )?));
                }
                let pipeline_executor = executor
                    .as_mut()
                    .ok_or_else(|| Error::internal("pipeline task without an executor"))?;
                let max_chunks = match mode {
                    TaskExecutionMode::ProcessAll => usize::MAX,
                    TaskExecutionMode::ProcessPartial => PARTIAL_CHUNK_COUNT,
                };
                match pipeline_executor.execute(max_chunks)? {
                    PipelineExecuteResult::NotFinished => return Ok(TaskExecutionResult::NotFinished),
                    PipelineExecuteResult::Interrupted => return Ok(TaskExecutionResult::Blocked),
                    PipelineExecuteResult::Finished => {}
                }
                // The executor holds the sink state: release it before the event can
                // finalize the sink.
                *executor = None;
            }
            TaskKind::PipelineFinish {
                pipeline,
                finalizes_again,
            } => {
                let pipeline = Arc::clone(pipeline);
                let finalizes_again = *finalizes_again;
                self.executor
                    .finalize_sink(&pipeline, &self.event, finalizes_again)?;
            }
            TaskKind::SinkFinalizeStage { pipeline, task } => {
                self.executor
                    .execute_finalize_task(pipeline, *task, self.thread_index)?;
            }
        }
        self.event.finish_task();
        Ok(TaskExecutionResult::Finished)
    }
}

impl Task for ExecutorTask {
    fn deschedule(self: Box<Self>) {
        let executor = Arc::clone(&self.executor);
        let task_id = self.task_id;
        executor.add_to_be_rescheduled(task_id, self);
    }

    fn task_blocked_on_result(&self) -> bool {
        // A pipeline task whose sink blocked keeps the chunk it could not sink.
        matches!(&self.kind, TaskKind::Pipeline { executor: Some(executor), .. } if executor.remaining_sink_chunk())
    }

    fn execute(&mut self, mode: TaskExecutionMode) -> TaskExecutionResult {
        if self.executor.is_cancelled() {
            return TaskExecutionResult::Error;
        }
        // The thread records into the query's metrics while it works on the query.
        let _metrics = roastedduck_common::profiling::activate(
            self.executor
                .profiler
                .as_ref()
                .and_then(|profiler| profiler.query_metrics()),
        );
        let _opener = self
            .executor
            .opener
            .clone()
            .map(roastedduck_function::file_system::activate_opener);
        match catch_unwind(AssertUnwindSafe(|| self.execute_task(mode))) {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                self.executor.push_error(error);
                TaskExecutionResult::Error
            }
            Err(panic) => {
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|message| (*message).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_owned());
                self.executor
                    .push_error(Error::internal(format!("task panicked: {message}")));
                TaskExecutionResult::Error
            }
        }
    }
}

impl Drop for ExecutorTask {
    fn drop(&mut self) {
        // Drop the pipeline executor (and the states it holds) before signalling.
        if let TaskKind::Pipeline { executor, .. } = &mut self.kind {
            *executor = None;
        }
        if self.executor.executor_tasks.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.executor.producer.notify();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod streaming_wait_tests {
    use super::*;
    use crate::operators::PhysicalResultCollector;
    use roastedduck_buffer::{BufferManager, BufferManagerConfig};
    use roastedduck_planner::{LogicalKind, LogicalOperator};
    use roastedduck_types::{DataChunk, LogicalType, Vector};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn query_drop_releases_execution_data_before_last_task_metadata() -> Result<()> {
        struct Released(Arc<AtomicUsize>);
        impl Drop for Released {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let mut logical = LogicalOperator::new(LogicalKind::ChunkGet {
            table_index: 0,
            chunk_types: vec![LogicalType::INTEGER],
            collection: Arc::from(vec![DataChunk::with_types(&[LogicalType::INTEGER], 0)]),
        });
        logical.resolve_operator_types()?;
        let physical = crate::create_physical_plan(logical)?;
        let root: PhysicalRef = Arc::new(PhysicalResultCollector::streaming(physical, 1, false, 1));
        let env = Arc::new(ClientEnv {
            buffer_manager: BufferManager::new(BufferManagerConfig::default())?,
            scheduler: super::super::TaskScheduler::new(1),
        });
        let execution = QueryExecution::new(root, env, &QueryOptions::default())?;
        // Model a completed worker's remaining Arc field, without relying on a
        // scheduler race or sleeping for a spill-file destructor to catch up.
        let metadata = Arc::clone(&execution.shared);
        let released = Arc::new(AtomicUsize::new(0));
        lock(&metadata.states)
            .operators
            .insert(usize::MAX, Arc::new(Released(Arc::clone(&released))));
        let state = metadata.pipelines[0].reset_source(|| Ok(Arc::new(Released(Arc::clone(&released)))))?;
        drop(state);
        drop(execution);
        assert_eq!(released.load(Ordering::SeqCst), 2);
        assert!(lock(&metadata.states).operators.is_empty());
        assert!(metadata.pipelines[0].source_state().is_none());
        Ok(())
    }

    #[test]
    fn already_ready_stream_does_not_wait_for_another_notification() -> Result<()> {
        let mut vector = Vector::flat(LogicalType::INTEGER, 2048);
        vector.values_mut::<i32>().fill(7);
        let chunk = DataChunk::new(vec![vector], 2048);
        let mut logical = LogicalOperator::new(LogicalKind::ChunkGet {
            table_index: 0,
            chunk_types: vec![LogicalType::INTEGER],
            collection: Arc::from(vec![chunk; 32]),
        });
        logical.resolve_operator_types()?;
        let physical = crate::create_physical_plan(logical)?;
        // One chunk fills the buffer, so no consumer can finish the producer yet.
        let root: PhysicalRef = Arc::new(PhysicalResultCollector::streaming(physical, 2, false, 1));
        let env = Arc::new(ClientEnv {
            buffer_manager: BufferManager::new(BufferManagerConfig::default())?,
            scheduler: super::super::TaskScheduler::new(2),
        });
        let options = QueryOptions {
            streaming_result: true,
            preserve_insertion_order: false,
            ..QueryOptions::default()
        };
        let mut execution = QueryExecution::new(root, env, &options)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if execution.execute_task()? == ExecutionStep::ResultReady {
                break;
            }
            assert!(Instant::now() < deadline, "stream did not become ready");
            std::thread::yield_now();
        }
        let shared = Arc::clone(&execution.shared);
        let (sender, receiver) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            execution.wait_for_task();
            sender.send(()).unwrap();
        });
        let returned_without_notification = receiver.recv_timeout(Duration::from_secs(1)).is_ok();
        // Release the old implementation on failure, then join and cancel its
        // blocked producer normally. A regression must not hang the test suite.
        if !returned_without_notification {
            shared.wake_query_thread();
        }
        waiter.join().unwrap();
        assert!(
            returned_without_notification,
            "an already-ready streaming result waited for a later notification"
        );
        Ok(())
    }
}
