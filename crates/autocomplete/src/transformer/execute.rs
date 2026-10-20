//! The transform functions of DuckDB's `transform_execute.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformExecuteStatement`.
    pub(super) fn transform_execute_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformExecuteStatement", parse_result))
    }
}
