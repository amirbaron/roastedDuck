//! DuckDB's JSON format (the json extension's `JsonSerializer` / `JsonDeserializer`), as
//! printed by `json_serialize_sql` and read by `json_deserialize_sql`.
//!
//! Output is byte-identical to DuckDB's: documents are built as a [`JsonValue`] tree with
//! the same structure yyjson would hold, and [`write_json`] renders it with yyjson's
//! number formatting and string escaping.

mod deserializer;
mod reader;
mod serializer;
mod value;
mod writer;

pub use deserializer::JsonDeserializer;
pub use reader::parse_json;
pub use serializer::{JsonOptions, JsonSerializer};
pub use value::JsonValue;
pub use writer::{try_write_json, write_json};
