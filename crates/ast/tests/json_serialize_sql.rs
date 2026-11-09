//! `json_serialize_sql` output of DuckDB 1.5.6 for statements covering most parse-tree
//! node kinds (`tests/fixtures/json_serialize_sql.jsonl`): reading it and writing it back
//! must reproduce DuckDB's text exactly, with every option set, and so must a trip
//! through the binary format.

#![cfg(test)]

use roastedduck_ast::SelectStatement;
use roastedduck_ast::json::{JsonSerializeOptions, deserialize_statements, serialize_statements};
use roastedduck_serde::binary::{BinaryDeserializer, BinarySerializer};
use roastedduck_serde::json::{JsonOptions, JsonValue, parse_json};
use roastedduck_serde::{SerializationCompatibility, SerializationOptions};

struct Fixture {
    sql: String,
    json: String,
    json_skip: String,
    json_pretty: Option<String>,
}

fn string_field(row: &JsonValue, key: &str) -> Option<String> {
    match row.get(key) {
        Some(JsonValue::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn fixtures() -> Vec<Fixture> {
    let text = include_str!("fixtures/json_serialize_sql.jsonl");
    text.lines()
        .map(|line| {
            let row = parse_json(line).expect("fixture line is JSON");
            Fixture {
                sql: string_field(&row, "sql").expect("sql"),
                json: string_field(&row, "json").expect("json"),
                json_skip: string_field(&row, "json_skip").expect("json_skip"),
                json_pretty: string_field(&row, "json_pretty"),
            }
        })
        .collect()
}

fn binary_round_trip(
    statements: &[SelectStatement],
    compatibility: SerializationCompatibility,
) -> Vec<SelectStatement> {
    let options = SerializationOptions {
        compatibility,
        ..SerializationOptions::default()
    };
    statements
        .iter()
        .map(|statement| {
            let bytes = BinarySerializer::serialize(statement, options.clone()).expect("serializes");
            BinaryDeserializer::deserialize(&bytes).expect("deserializes")
        })
        .collect()
}

#[test]
fn reproduces_duckdb_json_exactly() {
    let skip = JsonSerializeOptions {
        json: JsonOptions {
            skip_null: true,
            skip_empty: true,
            skip_default: true,
        },
        ..JsonSerializeOptions::default()
    };
    let pretty = JsonSerializeOptions {
        format: true,
        ..JsonSerializeOptions::default()
    };
    for fixture in fixtures() {
        let statements =
            deserialize_statements(&fixture.json).unwrap_or_else(|e| panic!("{}: {e}", fixture.sql));
        let default =
            serialize_statements(&statements, &JsonSerializeOptions::default()).expect("serializes");
        assert_eq!(default, fixture.json, "{}", fixture.sql);
        let reduced = serialize_statements(&statements, &skip).expect("serializes");
        assert_eq!(reduced, fixture.json_skip, "{}", fixture.sql);
        if let Some(expected) = &fixture.json_pretty {
            let formatted = serialize_statements(&statements, &pretty).expect("serializes");
            assert_eq!(&formatted, expected, "{}", fixture.sql);
        }
        for compatibility in [
            SerializationCompatibility::default(),
            SerializationCompatibility::latest(),
        ] {
            let read = binary_round_trip(&statements, compatibility);
            let again = serialize_statements(&read, &JsonSerializeOptions::default()).expect("serializes");
            assert_eq!(again, fixture.json, "binary round trip of {}", fixture.sql);
        }
    }
}

#[test]
fn reports_errors_like_json_deserialize_sql() {
    let error = deserialize_statements("{").unwrap_err();
    assert_eq!(error.to_string(), "Parser Error: Could not parse json");
    let error =
        deserialize_statements(r#"{"error":true,"error_type":"parser","error_message":"oops"}"#).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Parser Error: Error parsing json: parser: oops"
    );
    let error = deserialize_statements(r#"{"error":false,"statements":[]}"#).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Parser Error: Error parsing json: no statements"
    );
    let error = deserialize_statements(r#"{"error":false,"statements":[{"node":{"type":"SELECT_NODE"}}]}"#)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        r#"Parser Error: Expected but did not find property 'cte_map' in json object: '{"type":"SELECT_NODE"}'"#
    );
}
