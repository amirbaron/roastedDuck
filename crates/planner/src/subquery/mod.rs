//! Decorrelation of subqueries (DuckDB's `planner/subquery`): the dependent joins the
//! binder plans for correlated subqueries and LATERAL joins are flattened into DELIM
//! joins over duplicate-eliminated scans.

mod flatten_dependent_join;
mod rewrite;

pub use flatten_dependent_join::decorrelate_independent;
