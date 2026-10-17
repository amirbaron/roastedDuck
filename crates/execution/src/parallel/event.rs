//! Events: the steps of running a pipeline graph and the dependencies between them
//! (DuckDB's `Event` and its pipeline subclasses, `event.cpp`, `pipeline_*_event.cpp`).
//!
//! An event is scheduled once all its dependencies have finished; scheduling queues its
//! tasks, and the event finishes when its last task does (or right away if it has none).
//! Finishing notifies the events that depend on it.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use super::executor::{ExecutorShared, ExecutorTask, TaskKind};
use super::pipeline::Pipeline;
use super::task_scheduler::Task;

/// What an event does when it is scheduled.
#[derive(Debug)]
pub(crate) enum EventKind {
    /// Creates the global state of the pipeline's sink (`PipelineInitializeEvent`).
    PipelineInitialize(Arc<Pipeline>),
    /// Runs the pipeline on as many threads as it allows (`PipelineEvent`).
    Pipeline(Arc<Pipeline>),
    /// All input of the sink has arrived (`PipelinePrepareFinishEvent`); a no-op, as no
    /// sink negotiates memory before finalizing yet.
    PipelinePrepareFinish,
    /// Finalizes the sink (`PipelineFinishEvent`).
    PipelineFinish(Arc<Pipeline>),
    /// One stage of parallel finalize work the sink asked for, with its number of tasks
    /// (DuckDB's sink-specific events, e.g. `HashJoinFinalizeEvent`).
    SinkFinalizeStage { pipeline: Arc<Pipeline>, tasks: usize },
    /// The meta pipeline is complete (`PipelineCompleteEvent`); counts towards the
    /// query's completion if `complete_pipeline`.
    PipelineComplete { complete_pipeline: bool },
}

/// A node of the event graph (DuckDB's `Event`).
pub(crate) struct Event {
    executor: Weak<ExecutorShared>,
    pub kind: EventKind,
    finished_tasks: AtomicUsize,
    total_tasks: AtomicUsize,
    finished_dependencies: AtomicUsize,
    total_dependencies: AtomicUsize,
    /// The events waiting for this one.
    parents: Mutex<Vec<Weak<Event>>>,
    finished: AtomicBool,
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Event")
            .field("kind", &self.kind)
            .field("finished", &self.finished.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Event {
    pub fn new(executor: &Arc<ExecutorShared>, kind: EventKind) -> Arc<Self> {
        Arc::new(Self {
            executor: Arc::downgrade(executor),
            kind,
            finished_tasks: AtomicUsize::new(0),
            total_tasks: AtomicUsize::new(0),
            finished_dependencies: AtomicUsize::new(0),
            total_dependencies: AtomicUsize::new(0),
            parents: Mutex::default(),
            finished: AtomicBool::new(false),
        })
    }

    /// Makes `self` wait for `dependency` (`AddDependency`).
    pub fn add_dependency(self: &Arc<Self>, dependency: &Arc<Event>) {
        self.total_dependencies.fetch_add(1, Ordering::AcqRel);
        dependency
            .parents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::downgrade(self));
    }

    pub fn has_dependencies(&self) -> bool {
        self.total_dependencies.load(Ordering::Acquire) != 0
    }

    /// The events that depend on this one (for verifying the graph).
    pub fn parents(&self) -> Vec<Arc<Event>> {
        self.parents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }

    /// One dependency finished; schedules the event after the last one
    /// (`CompleteDependency`).
    pub fn complete_dependency(self: &Arc<Self>) {
        let finished = self.finished_dependencies.fetch_add(1, Ordering::AcqRel) + 1;
        if finished == self.total_dependencies.load(Ordering::Acquire) {
            self.schedule_and_finish_if_empty();
        }
    }

    /// Schedules the event's tasks; an event without tasks finishes right away. Nothing
    /// happens once the query was cancelled.
    pub fn schedule_and_finish_if_empty(self: &Arc<Self>) {
        if self.schedule() && self.total_tasks.load(Ordering::Acquire) == 0 {
            self.finish();
        }
    }

    /// Queues the event's tasks (`Schedule`); returns `false` if the query is gone or
    /// cancelled.
    fn schedule(self: &Arc<Self>) -> bool {
        let Some(executor) = self.executor.upgrade() else {
            return false;
        };
        if executor.is_cancelled() {
            return false;
        }
        let tasks = match &self.kind {
            EventKind::PipelineInitialize(pipeline) => {
                vec![TaskKind::PipelineInitialize(Arc::clone(pipeline))]
            }
            EventKind::Pipeline(pipeline) => match executor.pipeline_task_count(pipeline) {
                Ok(count) => (0..count)
                    .map(|_| TaskKind::Pipeline {
                        pipeline: Arc::clone(pipeline),
                        executor: None,
                    })
                    .collect(),
                Err(error) => {
                    executor.push_error(error);
                    return false;
                }
            },
            EventKind::PipelineFinish(pipeline) => vec![TaskKind::PipelineFinish(Arc::clone(pipeline))],
            EventKind::SinkFinalizeStage { pipeline, tasks } => (0..*tasks)
                .map(|task| TaskKind::SinkFinalizeStage {
                    pipeline: Arc::clone(pipeline),
                    task,
                })
                .collect(),
            EventKind::PipelinePrepareFinish | EventKind::PipelineComplete { .. } => Vec::new(),
        };
        self.set_tasks(&executor, tasks);
        true
    }

    /// Queues `tasks` for this event (`SetTasks`).
    fn set_tasks(self: &Arc<Self>, executor: &Arc<ExecutorShared>, tasks: Vec<TaskKind>) {
        debug_assert_eq!(self.total_tasks.load(Ordering::Acquire), 0);
        if tasks.is_empty() {
            return;
        }
        self.total_tasks.store(tasks.len(), Ordering::Release);
        let tasks: Vec<Box<dyn Task>> = tasks
            .into_iter()
            .map(|kind| Box::new(ExecutorTask::new(executor, Arc::clone(self), kind)) as Box<dyn Task>)
            .collect();
        executor.producer().schedule_tasks(tasks);
    }

    /// One of the event's tasks finished; the last one finishes the event (`FinishTask`).
    pub fn finish_task(self: &Arc<Self>) {
        let total = self.total_tasks.load(Ordering::Acquire);
        let finished = self.finished_tasks.fetch_add(1, Ordering::AcqRel) + 1;
        debug_assert!(finished <= total);
        if finished == total {
            self.finish();
        }
    }

    /// Marks the event finished and notifies the events waiting for it (`Finish`).
    fn finish(self: &Arc<Self>) {
        debug_assert!(!self.finished.load(Ordering::Acquire));
        let executor = self.executor.upgrade();
        if let (EventKind::SinkFinalizeStage { pipeline, .. }, Some(executor)) = (&self.kind, &executor) {
            // `FinishEvent`: the sink may ask for another stage.
            executor.schedule_next_finalize_stage(self, pipeline);
        }
        self.finished.store(true, Ordering::Release);
        let parents = std::mem::take(&mut *self.parents.lock().unwrap_or_else(PoisonError::into_inner));
        for parent in parents.iter().filter_map(Weak::upgrade) {
            parent.complete_dependency();
        }
        // `FinalizeFinish`.
        if let (
            EventKind::PipelineComplete {
                complete_pipeline: true,
            },
            Some(executor),
        ) = (&self.kind, executor)
        {
            executor.complete_pipeline();
        }
    }

    /// Puts `replacement` between this event and the events waiting for it
    /// (`InsertEvent`): it runs once this event finished, and they wait for it instead.
    pub fn insert_event(self: &Arc<Self>, executor: &ExecutorShared, replacement: Arc<Event>) {
        let parents = std::mem::take(&mut *self.parents.lock().unwrap_or_else(PoisonError::into_inner));
        *replacement.parents.lock().unwrap_or_else(PoisonError::into_inner) = parents;
        replacement.add_dependency(self);
        executor.add_event(replacement);
    }
}
