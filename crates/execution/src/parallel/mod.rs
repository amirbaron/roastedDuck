//! Parallel query execution (DuckDB's `src/parallel`): the task scheduler, pipelines and
//! meta pipelines, the event graph that orders them, and the per-thread pipeline
//! executor.

mod event;
mod executor;
mod interrupt;
mod meta_pipeline;
mod pipeline;
mod pipeline_executor;
mod task_scheduler;

pub use executor::{ExecutionStep, Executor, ProgressData, QueryExecution, QueryOptions};
pub use interrupt::InterruptState;
pub use task_scheduler::TaskScheduler;
