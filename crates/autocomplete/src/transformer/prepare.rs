//! The transform functions of DuckDB's `transform_prepare.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformPrepareStatement`.
    pub(super) fn transform_prepare_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformPrepareStatement", parse_result))
    }
}
