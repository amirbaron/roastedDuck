//! `json_serialize_sql`: the JSON form of the SELECT statements of a query, or of the
//! error parsing it.

use rustdb_ast::json::{JsonSerializeOptions, serialize_error, serialize_statements};
use rustdb_ast::{SelectStatement, SqlStatementKind};
use rustdb_common::{Error, Result};

/// DuckDB's `json_serialize_sql(sql, ...)`: `{"error":false,"statements":[...]}`, or
/// `{"error":true,"error_type":...,"error_message":...,...}` when the query does not
/// parse or contains a statement other than SELECT.
pub fn serialize_sql(sql: &str, options: &JsonSerializeOptions) -> String {
    serialize(sql, options).unwrap_or_else(|error| {
        let position = error.position().map(|position| position.to_string());
        let mut extra_info = Vec::new();
        if let Some(subtype) = error.subtype() {
            extra_info.push(("error_subtype", subtype));
        }
        if let Some(position) = &position {
            extra_info.push(("position", position.as_str()));
        }
        serialize_error(&error, &extra_info, options.format)
    })
}

fn serialize(sql: &str, options: &JsonSerializeOptions) -> Result<String> {
    let statements = crate::parse(sql)?;
    let mut selects: Vec<SelectStatement> = Vec::with_capacity(statements.len());
    let mut only_select = true;
    for statement in statements {
        if let SqlStatementKind::Select(select) = statement.kind {
            selects.push(select);
        } else {
            only_select = false;
            break;
        }
    }
    // Statements are serialized in order, so a failure in an earlier SELECT wins.
    let json = serialize_statements(&selects, options)?;
    if !only_select {
        return Err(Error::not_implemented(
            "Only SELECT statements can be serialized to json!",
        ));
    }
    Ok(json)
}
