//! `query(sql)` and `query_table(tables[, by_name])` (DuckDB's
//! `function/table/query_function.cpp`): calls replaced by the subquery they describe.

use std::fmt::Write as _;

use rustdb_common::{Error, Result};
use rustdb_function::{TableFunction, TableFunctionBindInput, TableFunctionReplacement, TableFunctionSet};
use rustdb_types::identifier::sql_identifier;
use rustdb_types::{LogicalType, LogicalTypeId, Value};

/// `QualifiedName::ParseComponents`: the dot-separated parts of a name, double quotes
/// grouping (and removed from) a part.
fn parse_components(input: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut entry = String::new();
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => loop {
                match chars.next() {
                    Some('"') => break,
                    Some(c) => entry.push(c),
                    None => {
                        return Err(Error::parser(format!(
                            "Unterminated quote in qualified name! (input: {input})"
                        )));
                    }
                }
            },
            '.' => result.push(std::mem::take(&mut entry)),
            c => entry.push(c),
        }
    }
    if !entry.is_empty() {
        result.push(entry);
    }
    Ok(result)
}

/// `QualifiedName::Parse(...).ToString()`: the name re-rendered with optional quotes
/// (`ParseInfo::QualifierToString`).
fn qualified_name(input: &str) -> Result<String> {
    let entries = parse_components(input)?;
    let (catalog, schema, name) = match entries.as_slice() {
        [] => ("", "", ""),
        [name] => ("", "", name.as_str()),
        [schema, name] => ("", schema.as_str(), name.as_str()),
        [catalog, schema, name] => (catalog.as_str(), schema.as_str(), name.as_str()),
        _ => {
            return Err(Error::parser(format!(
                "Expected catalog.entry, schema.entry or entry: too many entries found (input: {input})"
            )));
        }
    };
    let mut result = String::new();
    if !catalog.is_empty() {
        result.push_str(&sql_identifier(catalog));
        result.push('.');
        if !schema.is_empty() {
            result.push_str(&sql_identifier(schema));
            result.push('.');
        }
    } else if !schema.is_empty() && schema != "main" {
        result.push_str(&sql_identifier(schema));
        result.push('.');
    }
    result.push_str(&sql_identifier(name));
    Ok(result)
}

/// `UnionTablesQuery`: `FROM t1 UNION ALL [BY NAME] FROM t2 ...`.
fn union_tables_query(inputs: &[Value]) -> Result<String> {
    if inputs.iter().any(Value::is_null) {
        return Err(Error::binder("Cannot use NULL as function argument"));
    }
    let by_name = match inputs {
        [_, flag] if flag.logical_type().id() == LogicalTypeId::Boolean && flag.as_bool() == Some(true) => {
            "BY NAME "
        }
        _ => "",
    };
    let first = &inputs[0];
    if first.logical_type().id() == LogicalTypeId::Varchar {
        return Ok(format!("FROM {}", qualified_name(&first.to_varchar()?)?));
    }
    if first.logical_type() == &LogicalType::list(LogicalType::VARCHAR) {
        let children = first.children().unwrap_or_default();
        let Some((head, rest)) = children.split_first() else {
            return Err(Error::invalid_input("Input list is empty"));
        };
        let mut result = format!("FROM {}", qualified_name(&head.to_varchar()?)?);
        for child in rest {
            let _ = write!(
                result,
                " UNION ALL {by_name}FROM {}",
                qualified_name(&child.to_varchar()?)?
            );
        }
        return Ok(result);
    }
    Err(Error::invalid_input(
        "Expected a table or a list with tables as input",
    ))
}

/// `query`.
pub fn query_functions() -> TableFunctionSet {
    let mut function = TableFunction::new("query", vec![LogicalType::VARCHAR]);
    function.bind_replace = Some(std::sync::Arc::new(|input: &TableFunctionBindInput<'_>| {
        let query = input
            .inputs
            .first()
            .map(Value::to_varchar)
            .transpose()?
            .unwrap_or_default();
        Ok(Some(TableFunctionReplacement {
            query,
            error: "Expected a single SELECT statement".to_owned(),
        }))
    }));
    let mut set = TableFunctionSet::new("query");
    set.add(function);
    set
}

/// `query_table`.
pub fn query_table_functions() -> TableFunctionSet {
    let mut set = TableFunctionSet::new("query_table");
    for arguments in [
        vec![LogicalType::VARCHAR],
        vec![LogicalType::list(LogicalType::VARCHAR)],
        vec![LogicalType::list(LogicalType::VARCHAR), LogicalType::BOOLEAN],
    ] {
        let mut function = TableFunction::new("query_table", arguments);
        function.bind_replace = Some(std::sync::Arc::new(|input: &TableFunctionBindInput<'_>| {
            Ok(Some(TableFunctionReplacement {
                query: union_tables_query(input.inputs)?,
                error: "Expected a table or a list with tables as input".to_owned(),
            }))
        }));
        set.add(function);
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_names_are_requoted() {
        assert_eq!(qualified_name("tbl").unwrap(), "tbl");
        assert_eq!(qualified_name("main.tbl").unwrap(), "tbl");
        assert_eq!(qualified_name("s.\"my table\"").unwrap(), "s.\"my table\"");
        assert_eq!(qualified_name("db.main.t").unwrap(), "db.main.t");
    }
}
