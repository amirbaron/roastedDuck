//! Binding and planning (DuckDB's `src/planner`): parsed statements become typed
//! expressions and logical plans.
//!
//! - [`binder`]: the binder, bind contexts and the binding of query nodes, table refs and
//!   statements;
//! - [`expression_binder`]: the expression binders of each clause;
//! - [`expression`] / [`operator`]: bound expressions and logical operators;
//! - [`function_binder`]: overload resolution and implicit casts;
//! - [`planner`]: the entry point from a statement to a plan;
//! - [`column_binding_resolver`]: turns column bindings into input positions.

pub mod binder;
mod column_binding;
pub mod column_binding_resolver;
pub mod context;
pub mod expression;
pub mod expression_binder;
pub mod function_binder;
mod generated;
pub mod operator;
pub mod parsed_equality;
pub mod planner;
pub mod prepared;
pub mod subquery;

pub use binder::{Binder, BoundStatement};
pub use column_binding::{ColumnBinding, generate_column_bindings};
pub use column_binding_resolver::ColumnBindingResolver;
pub use context::{ClientContext, DefaultNullOrder, ExpressionEvaluator, PlannerConfig};
pub use expression::{Expression, ExpressionKind};
pub use generated::enums::{DistinctType, LogicalOperatorType};
pub use operator::{BoundLimitNode, LogicalKind, LogicalOperator, render_plan};
pub use planner::{PlannedStatement, Planner};
pub use prepared::{PreparedStatementData, StatementProperties, StatementReturnType};
pub use rustdb_ast::{ExpressionClass, ExpressionType, JoinType, SubqueryType};
