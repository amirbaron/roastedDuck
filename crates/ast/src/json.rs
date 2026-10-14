//! The JSON documents of `json_serialize_sql` and `json_deserialize_sql`.

use rustdb_common::{Error, Result};
use rustdb_serde::json::{JsonDeserializer, JsonOptions, JsonSerializer, JsonValue, parse_json, write_json};
use rustdb_serde::{SerializationCompatibility, SerializationOptions};

use crate::SelectStatement;

/// Options of `json_serialize_sql(sql, skip_null := .., skip_empty := .., skip_default := .., format := ..)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JsonSerializeOptions {
    /// `skip_null`, `skip_empty`, `skip_default`.
    pub json: JsonOptions,
    /// Pretty-print (`format := true`).
    pub format: bool,
    /// The serialization version to target (the database's `serialization_compatibility`).
    pub compatibility: SerializationCompatibility,
}

/// `{"error":false,"statements":[...]}`: the output of `json_serialize_sql` for
/// successfully parsed SELECT statements.
pub fn serialize_statements(
    statements: &[SelectStatement],
    options: &JsonSerializeOptions,
) -> Result<String> {
    let serialization = SerializationOptions {
        compatibility: options.compatibility.clone(),
        ..SerializationOptions::default()
    };
    let documents = statements
        .iter()
        .map(|statement| JsonSerializer::serialize(statement, options.json, serialization.clone()))
        .collect::<Result<Vec<_>>>()?;
    let root = JsonValue::Object(vec![
        ("error".to_owned(), JsonValue::Bool(false)),
        ("statements".to_owned(), JsonValue::Array(documents)),
    ]);
    Ok(write_json(&root, options.format))
}

/// `{"error":true,"error_type":...,"error_message":...}`: the output of
/// `json_serialize_sql` when the SQL cannot be serialized. `extra_info` holds the
/// error's extra fields (such as `position`).
pub fn serialize_error(error: &Error, extra_info: &[(&str, &str)], format: bool) -> String {
    let mut entries = vec![
        ("error".to_owned(), JsonValue::Bool(true)),
        (
            "error_type".to_owned(),
            JsonValue::Str(error.kind().display_name().to_ascii_lowercase()),
        ),
        (
            "error_message".to_owned(),
            JsonValue::Str(error.raw_message().to_owned()),
        ),
    ];
    entries.extend(
        extra_info
            .iter()
            .map(|(key, value)| ((*key).to_owned(), JsonValue::Str((*value).to_owned()))),
    );
    write_json(&JsonValue::Object(entries), format)
}

/// Reads the statements of a `json_serialize_sql` document, with the checks and
/// messages of `json_deserialize_sql`.
pub fn deserialize_statements(json: &str) -> Result<Vec<SelectStatement>> {
    let root = parse_json(json).map_err(|_| Error::parser("Could not parse json"))?;
    if matches!(root.get("error"), Some(JsonValue::Bool(true))) {
        return match (root.get("error_type"), root.get("error_message")) {
            (Some(JsonValue::Str(kind)), Some(JsonValue::Str(message))) => {
                Err(Error::parser(format!("Error parsing json: {kind}: {message}")))
            }
            _ => Err(Error::parser(
                "Error parsing json, expected error property to contain 'error_type' and 'error_message'",
            )),
        };
    }
    let Some(JsonValue::Array(statements)) = root.get("statements") else {
        return Err(Error::parser("Error parsing json: no statements array"));
    };
    if statements.is_empty() {
        return Err(Error::parser("Error parsing json: no statements"));
    }
    statements.iter().map(JsonDeserializer::deserialize).collect()
}
