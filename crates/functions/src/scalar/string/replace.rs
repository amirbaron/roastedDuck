//! `replace` (DuckDB's `replace.cpp`) and `repeat` (`repeat.cpp`).

use rustdb_common::{Error, Result};
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_types::kernels::scatter;
use rustdb_types::{DataChunk, ListEntry, LogicalType, Vector};

use crate::scalar::executor::{Str, StrOut, addressable, binary, ternary};
use crate::scalar::nested::{ListInput, list_child_type};
use crate::signature::template;

/// DuckDB's `string_t::MAX_STRING_SIZE`.
const MAX_STRING_SIZE: u64 = u32::MAX as u64;

/// Replaces every occurrence of `needle` (an empty needle matches nothing).
fn replace(haystack: &[u8], needle: &[u8], replacement: &[u8], out: &mut Vec<u8>) {
    if needle.is_empty() {
        out.extend_from_slice(haystack);
        return;
    }
    let mut rest = haystack;
    while let Some(position) = memchr::memmem::find(rest, needle) {
        out.extend_from_slice(&rest[..position]);
        out.extend_from_slice(replacement);
        rest = &rest[position + needle.len()..];
    }
    out.extend_from_slice(rest);
}

/// `replace(string, search, replacement)`.
pub fn replace_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("replace");
    set.add(ScalarFunction::new(
        "replace",
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR, LogicalType::VARCHAR],
        LogicalType::VARCHAR,
        |args, _, result| {
            ternary::<Str, Str, Str, StrOut>(args, result, |haystack, needle, replacement, out| {
                replace(haystack, needle, replacement, out);
                Ok(true)
            })
        },
    ));
    set
}

/// `repeat(string, count)` for VARCHAR and BLOB.
fn repeat_string(args: &DataChunk, result: &mut Vector) -> Result<()> {
    binary::<Str, i64, StrOut>(args, result, |input, count, out| {
        let copies = if count <= 0 || input.is_empty() {
            0
        } else {
            count as u64
        };
        let size = (input.len() as u64).checked_mul(copies).ok_or_else(|| {
            Error::out_of_range(format!(
                "Cannot create a string of size: '{}' * '{copies}', the maximum supported string size is: \
                 '{MAX_STRING_SIZE}'",
                input.len()
            ))
        })?;
        if size > MAX_STRING_SIZE {
            return Err(crate::scalar::executor::string_too_large(size));
        }
        out.reserve(size as usize);
        for _ in 0..copies {
            out.extend_from_slice(input);
        }
        Ok(true)
    })
}

/// `repeat(list, count)` (DuckDB's `RepeatListFunction`).
fn repeat_list(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let count = args.len();
    let lists = addressable(args.column(0), count);
    let counts = addressable(args.column(1), count);
    let constant = lists.is_constant() && counts.is_constant();
    let rows = if constant { 1 } else { count };
    let input = ListInput::new(&lists)?;
    let counts = counts.unified::<i64>();
    let mut entries = vec![ListEntry::default(); rows];
    let mut nulls = Vec::new();
    let mut sources = Vec::new();
    for (row, entry) in entries.iter_mut().enumerate() {
        let (Some(list), Some(copies)) = (input.get(row), counts.get(row)) else {
            nulls.push(row);
            continue;
        };
        let copies = if copies <= 0 || list.length == 0 {
            0
        } else {
            copies as u64
        };
        let length = list.length.checked_mul(copies).ok_or_else(|| {
            Error::out_of_range(format!(
                "Cannot create a list of size: '{}' * '{copies}', the result is too large",
                list.length
            ))
        })?;
        let start = sources.len();
        for _ in 0..copies {
            sources.extend((list.offset..list.offset + list.length).map(|child| Some(child as u32)));
        }
        *entry = ListEntry::new(start, length as usize);
    }
    let child_type = list_child_type(result.logical_type())?;
    let mut child = Vector::flat(child_type, sources.len().max(1));
    if !sources.is_empty() {
        let positions: Vec<u32> = (0..sources.len() as u32).collect();
        scatter(&input.child.select_rows(&sources), &mut child, &positions)?;
    }
    let mut output = Vector::flat(result.logical_type().clone(), rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries);
    output.set_list_child(child, sources.len());
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, rows);
    }
    if constant {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

/// `repeat(string|blob|list, count)` (DuckDB's `RepeatFun`).
pub fn repeat_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("repeat");
    for ty in [LogicalType::VARCHAR, LogicalType::BLOB] {
        set.add(ScalarFunction::new(
            "repeat",
            vec![ty.clone(), LogicalType::BIGINT],
            ty,
            |args, _, result| repeat_string(args, result),
        ));
    }
    let list = LogicalType::list(template("T"));
    set.add(ScalarFunction::new(
        "repeat",
        vec![list.clone(), LogicalType::BIGINT],
        list,
        |args, _, result| repeat_list(args, result),
    ));
    set
}
