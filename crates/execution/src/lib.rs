//! Push-based, vectorised execution (DuckDB's physical operators, pipelines and executor).
//!
//! A physical plan is split into pipelines at every sink. Each pipeline pulls chunks from
//! its source, pushes them through streaming operators and into its sink. Operators keep
//! their state in typed global (shared) and local (per-thread) state objects.

pub mod aggregate_hashtable;
mod column_data_collection;
mod constraints;
mod context;
mod expression_executor;
pub mod join_hashtable;
mod nested_loop_join;
mod operator;
pub mod operators;
pub mod parallel;
mod plan_generator;
mod profiler;
pub mod sample;
pub mod window;

pub use constraints::table_constraints;
pub use context::{ClientEnv, ExecutionContext, default_threads};
pub use expression_executor::{ExpressionExecutor, ScalarEvaluator};
pub use operator::{
    Operator, OperatorResult, OrderPreservationType, PartitionInfo, PhysicalOperator, PhysicalRef, Sink,
    SinkFinalize, SinkResult, Source, SourceResult,
};
pub use parallel::{Executor, QueryOptions, TaskScheduler};
pub use plan_generator::{
    ExplainOutput, JoinPlanSettings, PlanContext, create_physical_plan, create_physical_plan_with,
};
pub use profiler::{OperatorProfile, QueryProfiler};
