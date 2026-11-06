//! Equality of parsed expressions as DuckDB's `ParsedExpression::Equals` defines it:
//! aliases and query locations are ignored and column names compare case-insensitively.
//! The binder uses it to match select-list, GROUP BY and ORDER BY expressions.

use rustdb_ast::visit::{VisitorMut, walk_expression, walk_table_ref};
use rustdb_ast::{ExpressionType, ParsedExpression, ParsedExpressionKind, TableRef};
use rustdb_common::ByteString;
use rustdb_types::{LogicalType, LogicalTypeId, TypeInfo, TypeParameterValue, UnboundTypeInfo};
use std::collections::HashMap;

/// Clears what equality ignores.
struct Normalize;

impl VisitorMut for Normalize {
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        expression.alias.clear();
        expression.query_location = None;
        match &mut expression.kind {
            ParsedExpressionKind::ColumnRef(column) => {
                for name in &mut column.column_names {
                    name.make_ascii_lowercase();
                }
            }
            // A type written in the query (`x::BIGINT`) is equal wherever it is written.
            ParsedExpressionKind::Cast(cast) => cast.cast_type = without_locations(&cast.cast_type),
            _ => {}
        }
        walk_expression(self, expression);
    }

    fn visit_table_ref(&mut self, table_ref: &mut TableRef) {
        table_ref.query_location = None;
        walk_table_ref(self, table_ref);
    }
}

/// An unbound type without the query locations of its name and parameters (DuckDB's
/// `TypeExpression::Equals` ignores them).
fn without_locations(ty: &LogicalType) -> LogicalType {
    fn strip(info: &mut UnboundTypeInfo) {
        info.query_location = None;
        for parameter in &mut info.parameters {
            parameter.query_location = None;
            if let TypeParameterValue::Type(inner) = &mut parameter.value {
                strip(inner);
            }
        }
    }
    match ty.info() {
        Some(TypeInfo::Unbound(info)) if ty.id() == LogicalTypeId::Unbound => {
            let mut info = info.clone();
            strip(&mut info);
            LogicalType::with_info(LogicalTypeId::Unbound, TypeInfo::Unbound(info))
        }
        _ => ty.clone(),
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
    column_references: HashMap<(ExpressionType, Vec<ByteString>), usize>,
}

impl<V> ParsedExpressionMap<V> {
    /// The value for an expression equal to `key`.
    pub fn get(&self, key: &ParsedExpression) -> Option<&V> {
        let key = normalized(key);
        if let ParsedExpressionKind::ColumnRef(column) = &key.kind {
            let index = self
                .column_references
                .get(&(key.expression_type, column.column_names.clone()))?;
            return (self.entries[*index].0 == key).then_some(&self.entries[*index].1);
        }
        self.entries
            .iter()
            .find(|(entry, _)| entry == &key)
            .map(|(_, value)| value)
    }

    /// Inserts or replaces the value for `key`.
    pub fn insert(&mut self, key: &ParsedExpression, value: V) {
        let key = normalized(key);
        if let ParsedExpressionKind::ColumnRef(column) = &key.kind {
            let names = (key.expression_type, column.column_names.clone());
            if let Some(&index) = self.column_references.get(&names)
                && self.entries[index].0 == key
            {
                self.entries[index].1 = value;
                return;
            }
            self.column_references.insert(names, self.entries.len());
            self.entries.push((key, value));
            return;
        }
        match self.entries.iter_mut().find(|(entry, _)| entry == &key) {
            Some((_, existing)) => *existing = value,
            None => self.entries.push((key, value)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_types::Value;

    #[test]
    fn wide_column_reference_map_preserves_normalized_expression_equality() {
        let mut map = ParsedExpressionMap::default();
        for i in 0..2048 {
            let mut expression = ParsedExpression::column_ref(["Table".to_owned(), format!("Field{i}")]);
            expression.alias = format!("Alias{i}").into();
            expression.query_location = Some(i);
            map.insert(&expression, i);
        }
        let mut key = ParsedExpression::column_ref(["TABLE", "FIELD1024"]);
        key.alias = "different_alias".into();
        key.query_location = Some(9999);
        assert_eq!(map.get(&key), Some(&1024));
        map.insert(&key, 9000);
        assert_eq!(
            map.get(&ParsedExpression::column_ref(["table", "field1024"])),
            Some(&9000)
        );
        assert_eq!(
            map.get(&ParsedExpression::column_ref(["other_table", "field1024"])),
            None
        );
        assert_eq!(map.clone().get(&key), Some(&9000));
        let literal = ParsedExpression::constant(Value::integer(7));
        map.insert(&literal, 77);
        map.insert(&literal, 88);
        assert_eq!(map.get(&literal), Some(&88));
        assert_eq!(map.get(&ParsedExpression::constant(Value::integer(8))), None);
    }
}
