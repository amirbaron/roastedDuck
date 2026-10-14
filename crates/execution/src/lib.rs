//! Push-based, vectorised execution (DuckDB's physical operators, pipelines and executor).
//!
//! A physical plan is split into pipelines at every sink. Each pipeline pulls chunks from
//! its source, pushes them through streaming operators and into its sink. Operators keep
//! their state in typed global (shared) and local (per-thread) state objects.

mod context;
mod executor;
mod expression_executor;
mod operator;
pub mod operators;
mod pipeline;
mod plan_generator;

pub use context::ExecutionContext;
pub use executor::Executor;
pub use expression_executor::ExpressionExecutor;
pub use operator::{
    Operator, OperatorResult, PhysicalOperator, PhysicalRef, Sink, SinkFinalize, SinkResult, Source,
    SourceResult,
};
pub use plan_generator::create_physical_plan;
