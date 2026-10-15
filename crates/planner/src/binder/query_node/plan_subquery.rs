//! Planning subqueries used as expressions (DuckDB's `plan_subquery.cpp`).

use rustdb_common::{Error, Result};

use crate::binder::BindScope;
use crate::expression::Expression;
use crate::operator::LogicalOperator;

/// Plans the subqueries in `expression` as joins below `root` and replaces them with
/// references to the join's output (DuckDB's `Binder::PlanSubqueries`).
pub fn plan_subqueries(_scope: &mut BindScope<'_>, expression: &mut Expression, _root: &mut LogicalOperator) -> Result<()> {
    if expression.has_subquery() {
        return Err(Error::not_implemented("planning of subqueries"));
    }
    Ok(())
}
