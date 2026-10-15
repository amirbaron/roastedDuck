//! `list_extract`/`list_element` and `array_extract` (DuckDB's `list_extract.cpp`).

use rustdb_common::{Error, Result};
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, Vector};

use crate::scalar::executor::{Str, StrOut, addressable, binary};
use crate::scalar::nested::ListInput;
use crate::scalar::string::substring::substring_unicode;
use crate::scalar::structs::struct_extract;
use crate::signature::template;

/// The child row of 1-based (negative: from the end) `offset` in `entry`, or `None`
/// (DuckDB's `TryGetChildOffset`).
pub(crate) fn child_offset(entry: ListEntry, offset: i64) -> Option<usize> {
    if offset == 0 {
        return None;
    }
    let index = if offset > 0 { offset - 1 } else { offset };
    let length = entry.length as i64;
    if index < 0 {
        if length + index < 0 {
            return None;
        }
        return Some((entry.offset as i64 + length + index) as usize);
    }
    (index < length).then(|| entry.offset as usize + index as usize)
}

/// DuckDB's `ExecuteListExtract`.
fn extract_from_list(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let count = args.len();
    let lists = addressable(args.column(0), count);
    let offsets = addressable(args.column(1), count);
    let constant = lists.is_constant() && offsets.is_constant();
    let rows = if constant { 1 } else { count };
    let input = ListInput::new(&lists)?;
    let offsets = offsets.unified::<i64>();
    let selection: Vec<Option<u32>> = (0..rows)
        .map(|row| {
            let entry = input.get(row)?;
            let offset = offsets.get(row)?;
            child_offset(entry, offset).map(|child| child as u32)
        })
        .collect();
    let mut extracted = input.child.select_rows(&selection);
    if constant {
        extracted.make_constant();
    }
    *result = extracted;
    Ok(())
}

/// `string[index]`: the character at `index` (DuckDB's `ExecuteStringExtract`).
fn extract_from_string(args: &DataChunk, result: &mut Vector) -> Result<()> {
    binary::<Str, i64, StrOut>(args, result, |input, index, out| {
        out.extend_from_slice(substring_unicode(input, index, 1)?);
        Ok(true)
    })
}

fn list_extract(args: &DataChunk, result: &mut Vector) -> Result<()> {
    match args.column(0).logical_type().id() {
        LogicalTypeId::List => extract_from_list(args, result),
        LogicalTypeId::Varchar => extract_from_string(args, result),
        LogicalTypeId::SqlNull => {
            *result = Vector::constant_null(result.logical_type().clone());
            Ok(())
        }
        _ => Err(Error::not_implemented("Specifier type not implemented")),
    }
}

fn extract_overloads(name: &str) -> [ScalarFunction; 2] {
    let element = template("T");
    [
        ScalarFunction::new(
            name,
            vec![LogicalType::list(element.clone()), LogicalType::BIGINT],
            element,
            |args, _, result| list_extract(args, result),
        ),
        ScalarFunction::new(
            name,
            vec![LogicalType::VARCHAR, LogicalType::BIGINT],
            LogicalType::VARCHAR,
            |args, _, result| list_extract(args, result),
        ),
    ]
}

/// The `list_extract` set (alias `list_element`).
pub fn list_extract_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("list_extract");
    for function in extract_overloads("list_extract") {
        set.add(function);
    }
    set
}

/// The `array_extract` set: lists, strings and struct fields (by name or index).
pub fn array_extract_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("array_extract");
    for function in extract_overloads("array_extract") {
        set.add(function);
    }
    set.add(struct_extract::key_extract("array_extract"));
    set.add(struct_extract::index_extract("array_extract"));
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_offsets_follow_duckdb() {
        let entry = ListEntry::new(10, 3);
        assert_eq!(child_offset(entry, 1), Some(10));
        assert_eq!(child_offset(entry, 3), Some(12));
        assert_eq!(child_offset(entry, 4), None);
        assert_eq!(child_offset(entry, -1), Some(12));
        assert_eq!(child_offset(entry, -3), Some(10));
        assert_eq!(child_offset(entry, -4), None);
        assert_eq!(child_offset(entry, 0), None);
    }
}
