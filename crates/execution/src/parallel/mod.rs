//! Parallel query execution (DuckDB's `src/parallel`): the task scheduler, pipelines and
//! meta pipelines, the event graph that orders them, and the per-thread pipeline
//! executor.

mod event;
mod executor;
mod meta_pipeline;
mod pipeline;
mod pipeline_executor;
mod task_scheduler;

pub use executor::{ExecutionStep, Executor, ProgressData, QueryExecution, QueryOptions};
pub use task_scheduler::TaskScheduler;
