//! Push-based, vectorised execution (DuckDB's physical operators, pipelines and executor).
//!
//! A physical plan is split into pipelines at every sink. Each pipeline pulls chunks from
//! its source, pushes them through streaming operators and into its sink. Operators keep
//! their state in typed global (shared) and local (per-thread) state objects.

pub mod aggregate_hashtable;
mod column_data_collection;
mod constraints;
mod context;
mod executor;
mod expression_executor;
pub mod join_hashtable;
mod nested_loop_join;
mod operator;
pub mod operators;
mod pipeline;
mod plan_generator;
pub mod sample;
pub mod window;

pub use constraints::table_constraints;
pub use context::{ClientEnv, ExecutionContext};
pub use executor::Executor;
pub use expression_executor::{ExpressionExecutor, ScalarEvaluator};
pub use operator::{
    Operator, OperatorResult, PhysicalOperator, PhysicalRef, Sink, SinkFinalize, SinkResult, Source,
    SourceResult,
};
pub use plan_generator::{
    ExplainOutput, JoinPlanSettings, PlanContext, create_physical_plan, create_physical_plan_with,
};
