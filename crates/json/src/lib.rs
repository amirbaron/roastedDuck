//! The JSON extension (DuckDB's `extension/json`): the `JSON` type and its casts, the
//! `json_*` scalar functions and macros, `read_json` / `read_ndjson` /
//! `read_json_objects`, `json_each` / `json_tree`, `json_serialize_sql` and friends,
//! `COPY ... (FORMAT json)` and the replacement scan of `.json` files.
//!
//! JSON text is parsed and written by [`yyjson`], a port of the parts of yyjson DuckDB
//! uses, so that results — number formatting, escapes, error messages and positions —
//! match DuckDB byte for byte.

pub mod json_casts;
pub mod json_common;
pub(crate) mod json_executors;
pub mod json_functions;
mod variant_to_json;
pub mod yyjson;

use rustdb_types::LogicalType;

pub use json_casts::register_json_casts;
pub use json_functions::json_scalar_functions;

/// The name of the JSON type (`LogicalType::JSON_TYPE_NAME`).
pub const JSON_TYPE_NAME: &str = "JSON";

/// The JSON type: VARCHAR with the alias `JSON` (`LogicalType::JSON()`).
pub fn json_type() -> LogicalType {
    LogicalType::VARCHAR.with_alias(JSON_TYPE_NAME)
}

/// A scalar macro of the extension: name, parameters and body (`JSON_MACROS`).
#[derive(Clone, Copy, Debug)]
pub struct JsonMacro {
    pub name: &'static str,
    pub parameters: &'static [&'static str],
    pub body: &'static str,
}

/// `JSON_MACROS`: the aggregates and `json` defined in SQL.
pub const JSON_MACROS: &[JsonMacro] = &[
    JsonMacro {
        name: "json_group_array",
        parameters: &["x"],
        body: "CAST('[' || string_agg(CASE WHEN x IS NULL THEN 'null'::JSON ELSE to_json(x) END, ',') || ']' AS JSON)",
    },
    JsonMacro {
        name: "json_group_object",
        parameters: &["n", "v"],
        body: "CAST('{' || string_agg(CASE WHEN n IS NULL THEN error('json_group_object key cannot be NULL') ELSE to_json(n::VARCHAR) END || ':' || CASE WHEN v IS NULL THEN 'null'::JSON ELSE to_json(v) END, ',') || '}' AS JSON)",
    },
    JsonMacro {
        name: "json_group_structure",
        parameters: &["x"],
        body: "json_structure(json_group_array(x))->0",
    },
    JsonMacro {
        name: "json",
        parameters: &["x"],
        body: "json_extract(x, '$')",
    },
];
