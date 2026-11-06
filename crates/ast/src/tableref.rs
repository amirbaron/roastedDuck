//! Hand-written parts of table references: the backwards-compatible form of PIVOT entries
//! and the binder's rewritten pivots.

use std::collections::BTreeSet;

use rustdb_common::{Error, Result};
use rustdb_serde::Serializer;
use rustdb_types::{LogicalType, LogicalTypeId, TypeInfo, Value};

use crate::{ParsedExpression, ParsedExpressionKind, PivotColumn, PivotColumnEntry};

/// Ownership attached to an external scan (`TableRef::external_dependency`). Copies
/// share the owner. Like C++, equality and serialization concern the query tree,
/// rather than this process-local lifetime metadata.
#[derive(Clone, Default)]
pub struct ExternalDependency {
    owner: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>,
}
impl ExternalDependency {
    pub fn new<T: std::any::Any + Send + Sync>(owner: std::sync::Arc<T>) -> Self {
        Self { owner: Some(owner) }
    }
    pub fn is_empty(&self) -> bool {
        self.owner.is_none()
    }
}
impl std::fmt::Debug for ExternalDependency {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExternalDependency")
            .field("present", &self.owner.is_some())
            .finish()
    }
}
impl PartialEq for ExternalDependency {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

/// Retain external references injected while binding a copy, leaving the original
/// expressions, sampling and other syntax intact for later rebinding.
pub fn retain_external_replacements(
    original: &mut crate::SelectStatement,
    bound: &mut crate::SelectStatement,
) {
    use crate::TableRef;
    use crate::visit::{VisitorMut, walk_table_ref};
    #[derive(Default)]
    struct Collector(std::collections::HashMap<u64, TableRef>);
    impl VisitorMut for Collector {
        fn visit_table_ref(&mut self, table: &mut TableRef) {
            if !table.external_dependency.is_empty()
                && let Some(location) = table.query_location
            {
                self.0.entry(location).or_insert_with(|| table.clone());
            }
            walk_table_ref(self, table);
        }
    }
    let mut collect = Collector::default();
    collect.visit_query_node(&mut bound.node);
    retain_external_replacement_list(original, collect.0.into_values());
}

/// Apply replacements captured before binding consumes scalar subquery/CTE trees.
pub fn retain_external_replacement_list(
    original: &mut crate::SelectStatement,
    replacements: impl IntoIterator<Item = crate::TableRef>,
) {
    use crate::visit::{VisitorMut, walk_table_ref};
    use crate::{TableRef, TableRefKind};
    struct Retain(std::collections::HashMap<u64, TableRef>);
    impl VisitorMut for Retain {
        fn visit_table_ref(&mut self, table: &mut TableRef) {
            if matches!(table.kind, TableRefKind::BaseTable(_))
                && let Some(replacement) = table.query_location.and_then(|location| self.0.get(&location))
            {
                let sample = table.sample.clone();
                *table = replacement.clone();
                table.sample = sample;
                return;
            }
            walk_table_ref(self, table);
        }
    }
    let mut by_location = std::collections::HashMap::new();
    for replacement in replacements {
        if let Some(location) = replacement.query_location {
            by_location.entry(location).or_insert(replacement);
        }
    }
    Retain(by_location).visit_query_node(&mut original.node);
}

/// One output column group of a pivot: the pivot values it stands for and its name
/// (DuckDB's `PivotValueElement`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PivotValueElement {
    pub values: Vec<Value>,
    pub name: rustdb_common::ByteString,
}

/// What the binder records on a pivot it rewrote into the LIST-based plan, for binding the
/// final PIVOT operator (DuckDB's `PivotRef::bound_*` members).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoundPivotSpec {
    pub pivot_values: Vec<PivotValueElement>,
    pub group_names: Vec<rustdb_common::ByteString>,
    pub aggregate_names: Vec<rustdb_common::ByteString>,
}

/// From this version on, PIVOT entries may hold arbitrary expressions.
const PIVOT_EXPRESSION_VERSION: u64 = 7;

/// The entries of a pivot column as written for the target version
/// (DuckDB's `PivotColumn::GetEntriesForSerialization`): older versions only know constant
/// values, so expressions are folded into values where possible.
pub(crate) fn pivot_entries_for_serialization(
    column: &PivotColumn,
    serializer: &dyn Serializer,
) -> Result<Vec<PivotColumnEntry>> {
    if serializer.options().should_serialize(PIVOT_EXPRESSION_VERSION) {
        return Ok(column.entries.clone());
    }
    let is_unpivot = !column.unpivot_names.is_empty();
    let mut result = Vec::with_capacity(column.entries.len());
    for entry in &column.entries {
        let Some(expr) = &entry.star_expr else {
            result.push(entry.clone());
            continue;
        };
        let mut values = Vec::new();
        if fold_entry(expr, &mut values)? {
            result.push(PivotColumnEntry {
                values,
                star_expr: None,
                alias: entry.alias.clone(),
            });
        } else if is_unpivot {
            // UNPIVOT always supported expressions.
            result.push(entry.clone());
        } else {
            return Err(Error::serialization(format!(
                "Cannot serialize non-constant expression '{expr}' in pivot list when targeting database \
                 storage version '{}'",
                serializer.options().compatibility.duckdb_version
            )));
        }
    }
    Ok(result)
}

/// `TryFoldForBackwardsCompatibility`: column names and `row(...)` lists of constants.
fn fold_entry(expr: &ParsedExpression, values: &mut Vec<Value>) -> Result<bool> {
    match &expr.kind {
        ParsedExpressionKind::ColumnRef(column) => match column.column_names.as_slice() {
            [name] => {
                values.push(Value::try_varchar(name.as_bytes())?);
                Ok(true)
            }
            _ => Ok(false),
        },
        ParsedExpressionKind::Function(function) if function.function_name == "row" => {
            for child in &function.children {
                if !fold_entry(child, values)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        ParsedExpressionKind::Function(_) => Ok(false),
        _ => match fold_constant(expr)? {
            Some(value) => {
                values.push(value);
                Ok(true)
            }
            None => Ok(false),
        },
    }
}

/// `TryFoldConstantForBackwardsCompatability`: constants, and `struct_pack` /
/// `list_value` / `map` over constants. Folding casts needs the cast functions, which
/// are not ported yet.
fn fold_constant(expr: &ParsedExpression) -> Result<Option<Value>> {
    match &expr.kind {
        ParsedExpressionKind::Constant(constant) => Ok(Some(constant.value.clone())),
        ParsedExpressionKind::Function(function) => match function.function_name.as_bytes() {
            b"struct_pack" => {
                let mut names = BTreeSet::new();
                let mut fields = Vec::with_capacity(function.children.len());
                for child in &function.children {
                    if !names.insert(child.alias.as_bytes()) {
                        return Ok(None);
                    }
                    let Some(value) = fold_constant(child)? else {
                        return Ok(None);
                    };
                    fields.push((child.alias.clone(), value));
                }
                Ok(Some(Value::struct_value(fields)))
            }
            b"list_value" => {
                let Some(values) = fold_all(&function.children)? else {
                    return Ok(None);
                };
                let child_type = common_type(&values)?;
                Ok(Some(Value::list(child_type, values)))
            }
            b"map" => {
                let [keys, values] = function.children.as_slice() else {
                    return Ok(None);
                };
                let (Some(keys), Some(values)) = (fold_constant(keys)?, fold_constant(values)?) else {
                    return Ok(None);
                };
                Ok(map_value(&keys, &values))
            }
            _ => Ok(None),
        },
        ParsedExpressionKind::Cast(cast) => {
            let Some(value) = fold_constant(&cast.child)? else {
                return Ok(None);
            };
            let target = match cast.cast_type.info() {
                Some(TypeInfo::Unbound(unbound)) => match unbound.default_bind() {
                    Ok(target) => target,
                    Err(_) => return Ok(None),
                },
                _ => cast.cast_type.clone(),
            };
            if matches!(target.id(), LogicalTypeId::Invalid | LogicalTypeId::Unbound) {
                return Ok(None);
            }
            cast_constant(value, &target)
        }
        _ => Ok(None),
    }
}

/// `Value::DefaultTryCastAs` for the casts the parser puts in constant lists (`true` and
/// `false` are `'t'::BOOLEAN` and `'f'::BOOLEAN`); `None` when the cast fails. Other casts
/// need the cast functions, which are not ported yet.
fn cast_constant(value: Value, target: &LogicalType) -> Result<Option<Value>> {
    if value.logical_type() == target {
        return Ok(Some(value));
    }
    if value.is_null() {
        return Ok(Some(Value::null(target.clone())));
    }
    match (value.logical_type().id(), target.id()) {
        (LogicalTypeId::Varchar, LogicalTypeId::Boolean) => {
            Ok(value.as_bytes().and_then(parse_boolean).map(Value::boolean))
        }
        _ => Err(Error::not_implemented(format!(
            "Folding a cast from {} to {target} in PIVOT entries for older storage versions",
            value.logical_type()
        ))),
    }
}

/// DuckDB's lenient string-to-boolean cast (`TryCastStringBool` without `strict`).
fn parse_boolean(text: &[u8]) -> Option<bool> {
    let lower = text.to_ascii_lowercase();
    match lower.as_slice() {
        b"t" | b"y" | b"1" | b"yes" | b"true" => Some(true),
        b"f" | b"n" | b"0" | b"no" | b"false" => Some(false),
        _ => None,
    }
}

fn fold_all(children: &[ParsedExpression]) -> Result<Option<Vec<Value>>> {
    let mut values = Vec::with_capacity(children.len());
    for child in children {
        let Some(value) = fold_constant(child)? else {
            return Ok(None);
        };
        values.push(value);
    }
    Ok(Some(values))
}

/// The element type of a folded list (`ForceMaxLogicalType` over the elements), for the
/// cases that need no casts.
fn common_type(values: &[Value]) -> Result<LogicalType> {
    let mut result = LogicalType::SQLNULL;
    for value in values {
        let ty = value.logical_type();
        if ty.id() == LogicalTypeId::SqlNull || *ty == result {
            continue;
        }
        if result.id() != LogicalTypeId::SqlNull {
            return Err(Error::not_implemented(
                "Folding lists of mixed types in PIVOT entries for older storage versions",
            ));
        }
        result = ty.clone();
    }
    Ok(result)
}

/// `Value::MAP` from folded key and value lists.
fn map_value(keys: &Value, values: &Value) -> Option<Value> {
    let (key_list, value_list) = (keys.children()?, values.children()?);
    let key_type = keys.logical_type().child_type()?.clone();
    let value_type = values.logical_type().child_type()?.clone();
    let map_type = LogicalType::map(key_type, value_type);
    let entry_type = map_type.child_type()?.clone();
    let entries = key_list
        .iter()
        .zip(value_list)
        .map(|(key, value)| Value::from_children(entry_type.clone(), vec![key.clone(), value.clone()]))
        .collect();
    Some(Value::from_children(map_type, entries))
}
