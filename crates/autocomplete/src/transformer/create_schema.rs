//! The transform functions of DuckDB's `transform_create_schema.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformCreateSchemaStmt`.
    pub(super) fn transform_create_schema_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        Err(not_ported("TransformCreateSchemaStmt", parse_result))
    }
}
