//! The transform functions of DuckDB's `transform_detach.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformDetachStatement`.
    pub(super) fn transform_detach_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformDetachStatement", parse_result))
    }
}
