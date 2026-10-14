//! The serialization check: rustdb must reproduce DuckDB's `json_serialize_sql` output
//! byte for byte, and its binary format must round-trip the same trees.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rustdb_ast::SelectStatement;
use rustdb_ast::json::{JsonSerializeOptions, deserialize_statements, serialize_statements};
use rustdb_serde::binary::{BinaryDeserializer, BinarySerializer};
use rustdb_serde::json::JsonOptions;
use rustdb_serde::{SerializationCompatibility, SerializationOptions};
use serde_json::Value as Json;

use corpus::oracle::SerializedSql;

/// Outcome counts of a check run.
#[derive(Debug, Default)]
pub struct Report {
    pub total: usize,
    /// DuckDB could not serialize the SQL, by `error_type`.
    pub duckdb_errors: BTreeMap<String, usize>,
    /// Serialized by DuckDB and reproduced exactly in every variant.
    pub exact: usize,
    /// Failures by check and node type, with sample ids.
    pub mismatches: BTreeMap<String, Vec<u64>>,
}

impl Report {
    fn fail(&mut self, category: String, id: u64) {
        self.mismatches.entry(category).or_default().push(id);
    }

    /// Statements DuckDB serialized.
    pub fn serialized(&self) -> usize {
        self.total - self.duckdb_errors.values().sum::<usize>()
    }

    pub fn summary(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "statements:                 {}", self.total);
        let _ = writeln!(
            out,
            "DuckDB errors:              {}",
            self.total - self.serialized()
        );
        for (kind, count) in &self.duckdb_errors {
            let _ = writeln!(out, "  {kind:<26}{count}");
        }
        let _ = writeln!(out, "serialized by DuckDB:       {}", self.serialized());
        let _ = writeln!(out, "reproduced exactly:         {}", self.exact);
        let failed: usize = self.serialized() - self.exact;
        let _ = writeln!(out, "mismatched:                 {failed}");
        for (category, ids) in &self.mismatches {
            let _ = writeln!(out, "  {category}: {} (e.g. id {})", ids.len(), ids[0]);
        }
        out
    }
}

/// Where two JSON documents first differ, and the node type around it (the `class` or
/// `type` of the innermost enclosing object).
fn first_difference(expected: &Json, actual: &Json, path: &str, node: &str) -> Option<(String, String)> {
    let node = match expected {
        Json::Object(map) => map
            .get("class")
            .or_else(|| map.get("type"))
            .and_then(Json::as_str)
            .unwrap_or(node),
        _ => node,
    };
    match (expected, actual) {
        (Json::Object(a), Json::Object(b)) => {
            for (key, value) in a {
                let Some(other) = b.get(key) else {
                    return Some((format!("{path}.{key} (missing)"), node.to_owned()));
                };
                if let Some(found) = first_difference(value, other, &format!("{path}.{key}"), node) {
                    return Some(found);
                }
            }
            b.keys()
                .find(|key| !a.contains_key(*key))
                .map(|key| (format!("{path}.{key} (unexpected)"), node.to_owned()))
        }
        (Json::Array(a), Json::Array(b)) => {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                if let Some(found) = first_difference(x, y, &format!("{path}[{i}]"), node) {
                    return Some(found);
                }
            }
            (a.len() != b.len()).then(|| (format!("{path} (length)"), node.to_owned()))
        }
        _ => (expected != actual).then(|| (path.to_owned(), node.to_owned())),
    }
}

fn categorize(check: &str, expected: &str, actual: &str) -> String {
    match (
        serde_json::from_str::<Json>(expected),
        serde_json::from_str::<Json>(actual),
    ) {
        (Ok(a), Ok(b)) => match first_difference(&a, &b, "", "document") {
            Some((path, node)) => {
                let path: String = path
                    .split('.')
                    .next_back()
                    .unwrap_or(&path)
                    .chars()
                    .filter(|c| !c.is_ascii_digit() && *c != '[' && *c != ']')
                    .collect();
                format!("{check}: {node} .{path}")
            }
            None => format!("{check}: formatting"),
        },
        _ => format!("{check}: invalid JSON"),
    }
}

fn binary_round_trip(
    statements: &[SelectStatement],
    options: &SerializationOptions,
) -> Result<Vec<SelectStatement>, String> {
    statements
        .iter()
        .map(|statement| {
            let bytes = BinarySerializer::serialize(statement, options.clone()).map_err(|e| e.to_string())?;
            let read: SelectStatement = BinaryDeserializer::deserialize(&bytes).map_err(|e| e.to_string())?;
            let again = BinarySerializer::serialize(&read, options.clone()).map_err(|e| e.to_string())?;
            if again != bytes {
                return Err("binary output differs after a round trip".to_owned());
            }
            Ok(read)
        })
        .collect()
}

/// Checks one statement; returns the failed check, if any.
fn check_one(row: &SerializedSql) -> Option<(String, String, String)> {
    let default = JsonSerializeOptions::default();
    let skip = JsonSerializeOptions {
        json: JsonOptions {
            skip_null: true,
            skip_empty: true,
            skip_default: true,
        },
        ..JsonSerializeOptions::default()
    };
    let skip_defaults = JsonSerializeOptions {
        json: JsonOptions {
            skip_null: true,
            skip_empty: false,
            skip_default: true,
        },
        ..JsonSerializeOptions::default()
    };
    let pretty = JsonSerializeOptions {
        format: true,
        ..JsonSerializeOptions::default()
    };
    let statements = match deserialize_statements(&row.json) {
        Ok(statements) => statements,
        Err(error) => return Some(("json deserialize".to_owned(), row.json.clone(), error.to_string())),
    };
    for (name, options, expected) in [
        ("json", &default, &row.json),
        ("json skip", &skip, &row.json_skip),
        ("json skip defaults", &skip_defaults, &row.json_skip_defaults),
        ("json pretty", &pretty, &row.json_pretty),
    ] {
        match serialize_statements(&statements, options) {
            Ok(actual) if actual == *expected => {}
            Ok(actual) => return Some((name.to_owned(), expected.clone(), actual)),
            Err(error) => return Some((format!("{name} serialize"), expected.clone(), error.to_string())),
        }
    }
    // Reading a document without defaults must restore them.
    match deserialize_statements(&row.json_skip_defaults).and_then(|s| serialize_statements(&s, &default)) {
        Ok(actual) if actual == row.json => {}
        Ok(actual) => return Some(("json skip read".to_owned(), row.json.clone(), actual)),
        Err(error) => return Some(("json skip read".to_owned(), row.json.clone(), error.to_string())),
    }
    for (name, compatibility) in [
        ("binary", SerializationCompatibility::default()),
        ("binary latest", SerializationCompatibility::latest()),
    ] {
        let options = SerializationOptions {
            compatibility,
            ..SerializationOptions::default()
        };
        let round_tripped = match binary_round_trip(&statements, &options) {
            Ok(read) => read,
            Err(error) => return Some((name.to_owned(), row.json.clone(), error)),
        };
        match serialize_statements(&round_tripped, &default) {
            Ok(actual) if actual == row.json => {}
            Ok(actual) => return Some((name.to_owned(), row.json.clone(), actual)),
            Err(error) => return Some((name.to_owned(), row.json.clone(), error.to_string())),
        }
    }
    None
}

/// Runs every check; returns the report and a log of the failures.
pub fn check_serialization(rows: &[SerializedSql], sql_by_id: &BTreeMap<u64, &str>) -> (Report, String) {
    let mut report = Report {
        total: sql_by_id.len(),
        ..Report::default()
    };
    let mut log = String::new();
    let mut seen = 0;
    for row in rows {
        seen += 1;
        if row.json.starts_with(r#"{"error":true"#) {
            let kind = serde_json::from_str::<Json>(&row.json)
                .ok()
                .and_then(|json| json.get("error_type").and_then(Json::as_str).map(str::to_owned))
                .unwrap_or_else(|| "unknown".to_owned());
            *report.duckdb_errors.entry(kind).or_default() += 1;
            continue;
        }
        match check_one(row) {
            None => report.exact += 1,
            Some((check, expected, actual)) => {
                let category = categorize(&check, &expected, &actual);
                let samples = report.mismatches.get(&category).map_or(0, Vec::len);
                if samples < 3 {
                    let sql = sql_by_id.get(&row.id).copied().unwrap_or("");
                    let _ = writeln!(
                        log,
                        "=== {category} (id {})\n--- sql\n{sql}\n--- expected\n{expected}\n--- actual\n{actual}\n",
                        row.id
                    );
                }
                report.fail(category, row.id);
            }
        }
    }
    let missing = report.total.saturating_sub(seen);
    if missing > 0 {
        *report
            .duckdb_errors
            .entry("crashed the CLI".to_owned())
            .or_default() += missing;
    }
    (report, log)
}
