//! Hand-written parts of table references: the backwards-compatible form of PIVOT entries.

use std::collections::BTreeSet;

use rustdb_common::{Error, Result};
use rustdb_serde::Serializer;
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use crate::{ParsedExpression, ParsedExpressionKind, PivotColumn, PivotColumnEntry};

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
                "Cannot serialize non-constant expression in pivot list when targeting database storage \
                 version '{}'",
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
                values.push(Value::varchar(name));
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
        ParsedExpressionKind::Function(function) => match function.function_name.as_str() {
            "struct_pack" => {
                let mut names = BTreeSet::new();
                let mut fields = Vec::with_capacity(function.children.len());
                for child in &function.children {
                    if !names.insert(child.alias.as_str()) {
                        return Ok(None);
                    }
                    let Some(value) = fold_constant(child)? else {
                        return Ok(None);
                    };
                    fields.push((child.alias.clone(), value));
                }
                Ok(Some(Value::struct_value(fields)))
            }
            "list_value" => {
                let Some(values) = fold_all(&function.children)? else {
                    return Ok(None);
                };
                let child_type = common_type(&values)?;
                Ok(Some(Value::list(child_type, values)))
            }
            "map" => {
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
            if fold_constant(&cast.child)?.is_none() {
                return Ok(None);
            }
            Err(Error::not_implemented(
                "Folding casts in PIVOT entries for older storage versions",
            ))
        }
        _ => Ok(None),
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
