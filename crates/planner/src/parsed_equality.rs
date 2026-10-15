//! Equality of parsed expressions as DuckDB's `ParsedExpression::Equals` defines it:
//! aliases and query locations are ignored and column names compare case-insensitively.
//! The binder uses it to match select-list, GROUP BY and ORDER BY expressions.

use rustdb_ast::visit::{VisitorMut, walk_expression, walk_table_ref};
use rustdb_ast::{ParsedExpression, ParsedExpressionKind, TableRef};

/// Clears what equality ignores.
struct Normalize;

impl VisitorMut for Normalize {
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        expression.alias.clear();
        expression.query_location = None;
        if let ParsedExpressionKind::ColumnRef(column) = &mut expression.kind {
            for name in &mut column.column_names {
                name.make_ascii_lowercase();
            }
        }
        walk_expression(self, expression);
    }

    fn visit_table_ref(&mut self, table_ref: &mut TableRef) {
        table_ref.query_location = None;
        walk_table_ref(self, table_ref);
    }
}

/// `expression` with aliases and locations removed, for comparisons.
pub fn normalized(expression: &ParsedExpression) -> ParsedExpression {
    let mut copy = expression.clone();
    Normalize.visit_expression(&mut copy);
    copy
}

/// Whether two parsed expressions are equal (DuckDB's `ParsedExpression::Equals`).
pub fn parsed_equals(left: &ParsedExpression, right: &ParsedExpression) -> bool {
    left.expression_type == right.expression_type && normalized(left) == normalized(right)
}

/// A map from parsed expressions to values (DuckDB's `parsed_expression_map_t`), keyed by
/// normalized expressions.
#[derive(Clone, Debug, Default)]
pub struct ParsedExpressionMap<V> {
    entries: Vec<(ParsedExpression, V)>,
}

impl<V> ParsedExpressionMap<V> {
    /// The value for an expression equal to `key`.
    pub fn get(&self, key: &ParsedExpression) -> Option<&V> {
        let key = normalized(key);
        self.entries.iter().find(|(entry, _)| entry == &key).map(|(_, value)| value)
    }

    /// Inserts or replaces the value for `key`.
    pub fn insert(&mut self, key: &ParsedExpression, value: V) {
        let key = normalized(key);
        match self.entries.iter_mut().find(|(entry, _)| entry == &key) {
            Some((_, existing)) => *existing = value,
            None => self.entries.push((key, value)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
