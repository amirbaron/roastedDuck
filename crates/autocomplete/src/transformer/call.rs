//! The transform functions of DuckDB's `transform_call.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformCallStatement`.
    pub(super) fn transform_call_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformCallStatement", parse_result))
    }
}
