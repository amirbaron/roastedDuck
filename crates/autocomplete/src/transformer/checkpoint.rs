//! The transform functions of DuckDB's `transform_checkpoint.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformCheckpointStatement`.
    pub(super) fn transform_checkpoint_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        Err(not_ported("TransformCheckpointStatement", parse_result))
    }
}
