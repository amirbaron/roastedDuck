//! The transform functions of DuckDB's `transform_create_view.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformCreateViewStmt`.
    pub(super) fn transform_create_view_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        Err(not_ported("TransformCreateViewStmt", parse_result))
    }
}
