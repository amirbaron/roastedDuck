//! `json_serialize_sql` output of DuckDB 1.5.6 for a sample of the sqllogictest corpus
//! (`fixtures/json_serialize_sql.jsonl`: SELECTs, other statements, and one or two
//! statements per distinct parse error): `rustdb_parser::json::serialize_sql` must
//! reproduce it byte for byte. The full comparison is `examples/json_check`.

#![cfg(test)]

use rustdb_ast::json::JsonSerializeOptions;
use rustdb_parser::json::serialize_sql;
use rustdb_serde::json::{JsonValue, parse_json};

fn string_field(row: &JsonValue, key: &str) -> String {
    match row.get(key) {
        Some(JsonValue::Str(text)) => text.clone(),
        _ => panic!("fixture row without {key}"),
    }
}

#[test]
fn reproduces_duckdb_json_serialize_sql() {
    let fixture = include_str!("fixtures/json_serialize_sql.jsonl");
    let mut checked = 0;
    for line in fixture.lines() {
        let row = parse_json(line).expect("fixture line is JSON");
        let sql = string_field(&row, "sql");
        let expected = string_field(&row, "json");
        assert_eq!(
            serialize_sql(&sql, &JsonSerializeOptions::default()),
            expected,
            "{sql}"
        );
        checked += 1;
    }
    assert!(checked > 300);
}
