//! `parse_dirname`, `parse_dirpath`, `parse_filename` and `parse_path` (DuckDB's
//! `parse_path.cpp`).
//!
//! The separator option is `'system'`, `'forward_slash'`, `'backslash'` or anything else
//! for both slashes. Like DuckDB, an optional argument whose first row is NULL is replaced
//! by its default for every row.

use memchr::memchr;
use rustdb_common::{Error, Result};
use rustdb_function::{NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, Value, Vector};

use crate::scalar::executor::{Str, StrOut, addressable, binary, str_at, ternary};
use crate::scalar::nested::all_constant;

/// The separator characters of an option (DuckDB's `GetSeparator`).
fn separator(option: &[u8]) -> &'static [u8] {
    match option {
        // `FileSystem::PathSeparator` of the local file system.
        b"system" => {
            if cfg!(windows) {
                b"\\"
            } else {
                b"/"
            }
        }
        b"forward_slash" => b"/",
        b"backslash" => b"\\",
        _ => b"/\\",
    }
}

/// The leftmost separator position (DuckDB's `Find`).
fn find(input: &[u8], separators: &[u8]) -> Option<usize> {
    separators
        .iter()
        .filter_map(|&separator| memchr(separator, input))
        .min()
}

/// The last separator position (DuckDB's `FindLast`).
fn find_last(input: &[u8], separators: &[u8]) -> Option<usize> {
    input.iter().rposition(|byte| separators.contains(byte))
}

/// DuckDB's `ReadOptionalArgs`: the separator and `trim_extension` columns, replaced by
/// their defaults when absent or NULL in the first row.
fn optional_args(args: &DataChunk, front_trim: bool) -> Result<(Vector, Vector)> {
    let count = args.len();
    let mut separator = Vector::constant(&Value::varchar("default"))?;
    let mut trim = Vector::constant(&Value::boolean(false))?;
    let valid_first = |column: &Vector| !addressable(column, count.max(1)).is_null(0);
    match args.columns() {
        [_] => {}
        [_, second] => {
            if valid_first(second) {
                match second.logical_type().id() {
                    LogicalTypeId::Varchar => separator = second.clone(),
                    LogicalTypeId::Boolean => trim = second.clone(),
                    _ => return Err(Error::invalid_input("Invalid argument type")),
                }
            }
        }
        [_, second, third] if !front_trim => {
            if valid_first(second) {
                trim = second.clone();
            }
            if valid_first(third) {
                separator = third.clone();
            }
        }
        _ => return Err(Error::invalid_input("Invalid number of arguments")),
    }
    Ok((separator, trim))
}

/// DuckDB's `TrimPathFunction`: `parse_dirname` (`front_trim`) or `parse_filename`.
fn trim_path(args: &DataChunk, result: &mut Vector, front_trim: bool) -> Result<()> {
    let (separator_column, trim_column) = optional_args(args, front_trim)?;
    let inputs = DataChunk::new(
        vec![args.column(0).clone(), separator_column, trim_column],
        args.len(),
    );
    ternary::<Str, Str, bool, StrOut>(&inputs, result, |path, option, trim_extension, out| {
        let separators = separator(option);
        let mut begin = 0;
        let mut end = path.len();
        if front_trim {
            end = match find(path, separators) {
                Some(0) => 1,
                Some(position) => position,
                None => 0,
            };
        } else {
            if let Some(last) = find_last(path, separators) {
                begin = last + 1;
            }
            if trim_extension && let Some(dot) = find_last(path, b".").filter(|&dot| begin <= dot) {
                end = dot;
            }
        }
        out.extend_from_slice(&path[begin..end]);
        Ok(true)
    })
}

/// DuckDB's `ParseDirpathFunction`.
fn parse_dirpath(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let (separator_column, _) = optional_args(args, true)?;
    let inputs = DataChunk::new(vec![args.column(0).clone(), separator_column], args.len());
    binary::<Str, Str, StrOut>(&inputs, result, |path, option, out| {
        let end = match find_last(path, separator(option)) {
            Some(0) if path.len() == 1 => 1,
            Some(last) => last,
            None => 0,
        };
        out.extend_from_slice(&path[..end]);
        Ok(true)
    })
}

/// Splits a path into its components (DuckDB's `SplitPath`): a leading separator is its
/// own component and empty components are dropped.
fn split_path<'a>(mut input: &'a [u8], separators: &[u8], out: &mut Vec<&'a [u8]>) {
    let mut first = true;
    while !input.is_empty() {
        let Some(position) = find(input, separators) else {
            break;
        };
        if position == 0 {
            if first {
                out.push(&input[..1]);
                first = false;
                if input.len() == 1 {
                    return;
                }
            }
        } else {
            out.push(&input[..position]);
            first = false;
        }
        input = &input[position + 1..];
    }
    if !input.is_empty() {
        out.push(input);
    }
}

/// DuckDB's `ParsePathFunction`: the separator comes from the first row.
fn parse_path(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let count = args.len();
    let mut option: &[u8] = b"default";
    let separator_column;
    if let [_, second] = args.columns() {
        separator_column = addressable(second, count.max(1));
        if !separator_column.is_null(0) {
            option = str_at(&separator_column, 0).unwrap_or(b"default");
        }
    }
    let separators = separator(option);
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { count };
    let paths = addressable(args.column(0), count);
    let mut entries = Vec::with_capacity(rows);
    let mut parts: Vec<&[u8]> = Vec::new();
    let mut nulls = Vec::new();
    for row in 0..rows {
        let Some(path) = str_at(&paths, row) else {
            nulls.push(row);
            entries.push(ListEntry::new(parts.len(), 0));
            continue;
        };
        let offset = parts.len();
        split_path(path, separators, &mut parts);
        entries.push(ListEntry::new(offset, parts.len() - offset));
    }
    let mut child = Vector::flat(LogicalType::VARCHAR, parts.len().max(1));
    for (index, part) in parts.iter().enumerate() {
        child.set_value(index, &Value::bytes(LogicalType::VARCHAR, part))?;
    }
    let mut output = Vector::flat(LogicalType::list(LogicalType::VARCHAR), rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries);
    output.set_list_child(child, parts.len());
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

fn path_function(
    name: &str,
    arguments: Vec<LogicalType>,
    return_type: LogicalType,
    kernel: fn(&DataChunk, &mut Vector) -> Result<()>,
) -> ScalarFunction {
    let mut function = ScalarFunction::new(name, arguments, return_type, move |args, _, result| {
        kernel(args, result)
    });
    function.null_handling = NullHandling::Special;
    function
}

/// `parse_dirname`, `parse_dirpath`, `parse_filename` and `parse_path`.
pub fn parse_path_functions() -> Vec<ScalarFunctionSet> {
    let varchar = LogicalType::VARCHAR;
    let mut dirname = ScalarFunctionSet::new("parse_dirname");
    let mut dirpath = ScalarFunctionSet::new("parse_dirpath");
    let mut path = ScalarFunctionSet::new("parse_path");
    for arguments in [vec![varchar.clone()], vec![varchar.clone(), varchar.clone()]] {
        dirname.add(path_function(
            "parse_dirname",
            arguments.clone(),
            varchar.clone(),
            |args, result| trim_path(args, result, true),
        ));
        dirpath.add(path_function(
            "parse_dirpath",
            arguments.clone(),
            varchar.clone(),
            parse_dirpath,
        ));
        path.add(path_function(
            "parse_path",
            arguments,
            LogicalType::list(varchar.clone()),
            parse_path,
        ));
    }
    let mut filename = ScalarFunctionSet::new("parse_filename");
    for arguments in [
        vec![varchar.clone()],
        vec![varchar.clone(), varchar.clone()],
        vec![varchar.clone(), LogicalType::BOOLEAN],
        vec![varchar.clone(), LogicalType::BOOLEAN, varchar.clone()],
    ] {
        filename.add(path_function(
            "parse_filename",
            arguments,
            varchar.clone(),
            |args, result| trim_path(args, result, false),
        ));
    }
    vec![dirname, dirpath, filename, path]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(path: &str) -> Vec<String> {
        let mut parts = Vec::new();
        split_path(path.as_bytes(), b"/\\", &mut parts);
        parts
            .iter()
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect()
    }

    #[test]
    fn split_path_matches_duckdb() {
        assert_eq!(split("/path/to//file.csv"), ["/", "path", "to", "file.csv"]);
        assert_eq!(split("/"), ["/"]);
        assert_eq!(split("a\\b/"), ["a", "b"]);
        assert!(split("").is_empty());
    }
}
