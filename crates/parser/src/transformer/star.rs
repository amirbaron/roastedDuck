//! Column references and stars (`transform_columnref.cpp`).
//!
//! The `EXCLUDE`, `REPLACE` and `RENAME` lists of a star are `unordered_map`s and
//! `unordered_set`s in DuckDB and are serialized in their iteration order, which
//! [`HashOrder`] reproduces.

use roastedduck_ast::{
    ExpressionClass, ExpressionType, ParsedExpression, ParsedExpressionKind, QualifiedColumnName,
    StarExpression,
};
use roastedduck_common::{CaseInsensitiveMap, Error, OrderedMap, Result};
use roastedduck_pg_parser::nodes::{AStar, ColumnRef, List, Node};

use super::build::{self, set_location};
use super::{Transformer, node_string};
use crate::hash_order::{CiHashMap, HashOrder, ci_hash};

/// `TransformQualifiedColumnName`: `[[[catalog.]schema.]table.]column`.
fn transform_qualified_column_name(list: &List) -> Result<QualifiedColumnName> {
    let parts: Vec<&[u8]> = list.iter().map(node_string).collect::<Result<_>>()?;
    let (catalog, schema, table, column) = match parts.as_slice() {
        [column] => (b"".as_slice(), b"".as_slice(), b"".as_slice(), *column),
        [table, column] => (b"".as_slice(), b"".as_slice(), *table, *column),
        [schema, table, column] => (b"".as_slice(), *schema, *table, *column),
        [catalog, schema, table, column] => (*catalog, *schema, *table, *column),
        _ => {
            return Err(Error::parser(
                "Qualified column name must have between 1 and 4 elements",
            ));
        }
    };
    Ok(QualifiedColumnName {
        catalog: catalog.to_owned().into(),
        schema: schema.to_owned().into(),
        table: table.to_owned().into(),
        column: column.to_owned().into(),
    })
}

/// `QualifiedColumnEquality`: a prefix comparison, so `tbl.i` equals `i` but not `tbl2.i`.
fn qualified_equal(a: &QualifiedColumnName, b: &QualifiedColumnName) -> bool {
    let part_matches = |x: &[u8], y: &[u8]| x.is_empty() || y.is_empty() || x.eq_ignore_ascii_case(y);
    part_matches(&a.catalog, &b.catalog)
        && part_matches(&a.schema, &b.schema)
        && part_matches(&a.table, &b.table)
        && a.column.eq_ignore_ascii_case(&b.column)
}

/// A `qualified_column_map_t` / `qualified_column_set_t` in libc++ iteration order.
#[derive(Debug, Default)]
pub struct QualifiedColumnMap<V> {
    order: HashOrder,
    entries: Vec<(QualifiedColumnName, V)>,
}

impl<V> QualifiedColumnMap<V> {
    fn find(&self, key: &QualifiedColumnName) -> Option<usize> {
        let entries = &self.entries;
        self.order.find(ci_hash(&key.column), |node| {
            qualified_equal(&entries[node - 1].0, key)
        })
    }

    pub fn contains(&self, key: &QualifiedColumnName) -> bool {
        self.find(key).is_some()
    }

    pub fn insert(&mut self, key: QualifiedColumnName, value: V) {
        let entries = &self.entries;
        if self
            .order
            .insert_unique(ci_hash(&key.column), |node| {
                qualified_equal(&entries[node - 1].0, &key)
            })
            .is_ok()
        {
            self.entries.push((key, value));
        }
    }

    pub fn into_ordered(self) -> Vec<(QualifiedColumnName, V)> {
        let mut slots: Vec<Option<(QualifiedColumnName, V)>> = self.entries.into_iter().map(Some).collect();
        self.order
            .order()
            .into_iter()
            .filter_map(|node| slots[node - 1].take())
            .collect()
    }
}

/// The exclude list in the order DuckDB serializes it: the unqualified names (re-inserted
/// into a `case_insensitive_set_t`), then the qualified ones (re-inserted into a fresh
/// `qualified_column_set_t`).
pub fn serialized_exclude_list(exclude: QualifiedColumnMap<()>) -> Vec<QualifiedColumnName> {
    let ordered = exclude.into_ordered();
    let mut plain: CiHashMap<(), roastedduck_common::ByteString> = CiHashMap::new_with_keys();
    let mut qualified = QualifiedColumnMap::default();
    for (name, ()) in ordered {
        if name.is_qualified() {
            qualified.insert(name, ());
        } else {
            plain.insert(name.column, ());
        }
    }
    let mut result: Vec<QualifiedColumnName> = plain
        .into_ordered()
        .into_iter()
        .map(|(column, ())| QualifiedColumnName::column(column))
        .collect();
    result.extend(qualified.into_ordered().into_iter().map(|(name, ())| name));
    result
}

/// `StarExpression::IsStar`: a `*` that is not `COLUMNS(...)`.
pub(crate) fn is_star(expr: &ParsedExpression) -> bool {
    matches!(&expr.kind, ParsedExpressionKind::Star(star) if !star.columns)
}

impl<'a> Transformer<'a> {
    /// `TransformColumnRef`.
    #[inline(never)]
    pub(crate) fn transform_column_ref(&mut self, root: &'a ColumnRef) -> Result<ParsedExpression> {
        let Some(head) = root.fields.first() else {
            return Err(Error::internal("Unexpected field length"));
        };
        match head {
            Node::AStar(star) => self.transform_star_expression(star),
            Node::Value(_) | Node::Str(_) => {
                let names = root
                    .fields
                    .iter()
                    .map(|node| node_string(node).map(roastedduck_common::ByteString::from))
                    .collect::<Result<Vec<_>>>()?;
                let mut column_ref = build::column_ref(names);
                set_location(&mut column_ref, root.location);
                Ok(column_ref)
            }
            _ => Err(Error::not_implemented("ColumnRef not implemented!")),
        }
    }

    /// `TransformStarExpression`: `*`, `t.*`, `COLUMNS(...)` with `EXCLUDE`, `REPLACE`
    /// and `RENAME`.
    #[inline(never)]
    pub(crate) fn transform_star_expression(&mut self, star: &'a AStar) -> Result<ParsedExpression> {
        let mut result = build::star_expression(star.relation.clone().unwrap_or_default());
        let mut exclude: QualifiedColumnMap<()> = QualifiedColumnMap::default();
        for item in &star.except_list {
            let Node::List(parts) = item else {
                return Err(Error::internal("EXCLUDE entry is not a list"));
            };
            let column = transform_qualified_column_name(parts)?;
            if exclude.contains(&column) {
                return Err(Error::parser(roastedduck_common::byte_format!(
                    "Duplicate entry \"{column}\" in EXCLUDE list"
                )));
            }
            exclude.insert(column, ());
        }
        let mut replace: CiHashMap<ParsedExpression, roastedduck_common::ByteString> =
            CiHashMap::new_with_keys();
        for item in &star.replace_list {
            let Node::List(pair) = item else {
                return Err(Error::internal("REPLACE entry is not a list"));
            };
            let (Some(expression), Some(name)) = (pair.first(), pair.last()) else {
                return Err(Error::internal("REPLACE entry is not a pair"));
            };
            let replace_expression = self.transform_expression(expression)?;
            let entry = node_string(name)?.to_owned();
            if replace.contains_key(&entry) {
                return Err(Error::parser(roastedduck_common::byte_format!(
                    "Duplicate entry \"{entry}\" in REPLACE list"
                )));
            }
            if exclude.contains(&QualifiedColumnName::column(entry.clone())) {
                return Err(Error::parser(roastedduck_common::byte_format!(
                    "Column \"{entry}\" cannot occur in both EXCLUDE and REPLACE list"
                )));
            }
            replace.insert(entry, replace_expression);
        }
        let mut rename: QualifiedColumnMap<roastedduck_common::ByteString> = QualifiedColumnMap::default();
        for item in &star.rename_list {
            let Node::List(pair) = item else {
                return Err(Error::internal("RENAME entry is not a list"));
            };
            let (Some(Node::List(column)), Some(new_name)) = (pair.first(), pair.last()) else {
                return Err(Error::internal("RENAME entry is not a pair"));
            };
            let rename_column = transform_qualified_column_name(column)?;
            let new_name = node_string(new_name)?.to_owned();
            if rename.contains(&rename_column) {
                return Err(Error::parser(roastedduck_common::byte_format!(
                    "Duplicate entry \"{rename_column}\" in EXCLUDE list"
                )));
            }
            if exclude.contains(&rename_column) {
                return Err(Error::parser(roastedduck_common::byte_format!(
                    "Column \"{rename_column}\" cannot occur in both EXCLUDE and RENAME list"
                )));
            }
            if replace.contains_key(&rename_column.column) {
                return Err(Error::parser(roastedduck_common::byte_format!(
                    "Column \"{rename_column}\" cannot occur in both REPLACE and RENAME list"
                )));
            }
            rename.insert(rename_column, new_name.into());
        }
        result.exclude_list = serialized_exclude_list(exclude);
        result.replace_list = replace
            .into_ordered()
            .into_iter()
            .collect::<CaseInsensitiveMap<_, roastedduck_common::ByteString>>();
        result.rename_list = rename.into_ordered().into_iter().collect::<OrderedMap<_, _>>();
        if let Some(expr) = &star.expr {
            let child = self.transform_expression(expr)?;
            if is_star(&child) {
                let ParsedExpressionKind::Star(child_star) = child.kind else {
                    return Err(Error::internal("Star expression without a star"));
                };
                let StarExpression {
                    relation_name,
                    exclude_list,
                    replace_list,
                    rename_list,
                    ..
                } = *child_star;
                result.relation_name = relation_name;
                result.exclude_list = exclude_list;
                result.replace_list = replace_list;
                result.rename_list = rename_list;
                result.expr = None;
            } else if child.expression_type == ExpressionType::Lambda {
                result.expr = Some(Box::new(build::function(
                    "list_filter",
                    vec![build::star(), child],
                )));
            } else {
                result.expr = Some(Box::new(child));
            }
        }
        result.columns = star.columns;
        let mut expression =
            ParsedExpression::new(ExpressionType::Star, ParsedExpressionKind::Star(Box::new(result)));
        set_location(&mut expression, star.location);
        Ok(expression)
    }
}

/// `ExpressionIsEmptyStar`: a `*` without `COLUMNS`, `EXCLUDE` or `REPLACE`.
pub(crate) fn is_empty_star(expr: &ParsedExpression) -> bool {
    if expr.expression_class() != ExpressionClass::Star {
        return false;
    }
    matches!(&expr.kind, ParsedExpressionKind::Star(star)
        if !star.columns && star.exclude_list.is_empty() && star.replace_list.is_empty())
}
