//! Arbitrary single-column predicates pushed into table scans (DuckDB's
//! `ExpressionFilter`). The planner stores the predicate; the physical planner swaps in
//! an implementation that evaluates it.

use rustdb_common::{Error, Result};
use rustdb_storage::table_filter::ExpressionFilter;
use rustdb_types::Vector;

use crate::expression::{Expression, ExpressionKind};

/// A predicate over one scanned column, which it reads as `BOUND_REF #0` (DuckDB's
/// `ExpressionFilter::expr`).
#[derive(Clone, Debug)]
pub struct ExpressionTableFilter {
    pub expression: Expression,
}

impl ExpressionTableFilter {
    /// The predicate with the column reference named `column_name` (DuckDB's
    /// `ExpressionFilter::ToExpression`).
    pub fn to_expression(&self, column_name: &str) -> Expression {
        fn replace(expression: &mut Expression, column_name: &str) {
            if matches!(expression.kind, ExpressionKind::Reference(_)) {
                column_name.clone_into(&mut expression.alias);
                return;
            }
            expression.for_each_child_mut(|child| replace(child, column_name));
        }
        let mut expression = self.expression.clone();
        replace(&mut expression, column_name);
        expression
    }
}

impl ExpressionFilter for ExpressionTableFilter {
    fn select(&self, _: &Vector, _: usize, _: &mut Vec<u32>) -> Result<()> {
        Err(Error::internal(
            "expression table filter evaluated before physical planning",
        ))
    }

    fn to_sql(&self, column_name: &str) -> String {
        self.to_expression(column_name).to_string()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
