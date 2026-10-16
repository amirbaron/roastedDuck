//! Binding and planning (DuckDB's `src/planner`): parsed statements become typed
//! expressions and logical plans.
//!
//! - [`binder`]: the binder, bind contexts and the binding of query nodes, table refs and
//!   statements;
//! - [`expression_binder`]: the expression binders of each clause;
//! - [`expression`] / [`operator`]: bound expressions and logical operators;
//! - [`function_binder`]: overload resolution and implicit casts;
//! - [`planner`]: the entry point from a statement to a plan;
//! - [`subquery`]: flattening of the dependent joins of correlated subqueries;
//! - [`column_binding_resolver`]: turns column bindings into input positions.
//!
//! # Ownership model
//!
//! DuckDB's binders point at their parents and at a stack of "active" expression
//! binders, all mutable at once. Here a [`binder::Binder`] holds the state of one
//! query level only; the enclosing levels are passed down as a chain of
//! [`binder::Scope`]s (`&mut dyn Scope`). A scope is either a plain
//! [`binder::BindScope`] (a binder binding a FROM item) or an expression binder (which
//! implements `Scope` too). Correlated columns are resolved by walking the chain
//! outward and asking each expression binder in turn, one depth level further out per
//! expression binder — exactly DuckDB's active binder stack. LATERAL joins insert a
//! [`expression_binder::LateralBinder`] over the left side into the right side's chain.
//!
//! Binding happens in place: DuckDB replaces bound sub-trees of a parsed expression by a
//! `BoundExpression` node so that a failed attempt can be retried in an enclosing query
//! without losing what did bind. The parse tree has a matching
//! `ParsedExpressionKind::Bound` variant whose payload is an [`Expression`].
//!
//! Operators that DuckDB identifies by address in its analyses carry a
//! [`LogicalOperator::node_id`].

pub mod binder;
pub mod collation_binding;
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

pub use binder::expression::bind_logical_type;
pub use binder::{Binder, BoundStatement};
pub use column_binding::{ColumnBinding, generate_column_bindings};
pub use column_binding_resolver::ColumnBindingResolver;
pub use context::{ClientContext, DatabaseState, DefaultNullOrder, ExpressionEvaluator, PlannerConfig};
pub use expression::{Expression, ExpressionKind};
pub use generated::enums::{DistinctType, LogicalOperatorType};
pub use operator::{BoundLimitNode, LogicalKind, LogicalOperator, render_plan};
pub use planner::{PlannedStatement, Planner};
pub use prepared::{PreparedStatementData, StatementProperties, StatementReturnType};
pub use rustdb_ast::{ExpressionClass, ExpressionType, JoinType, SubqueryType};
