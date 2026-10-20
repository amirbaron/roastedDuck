//! The transform functions of DuckDB's `transform_explain.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformExplainStatement`.
    pub(super) fn transform_explain_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformExplainStatement", parse_result))
    }

    /// `TransformExplainableStatements`.
    pub(super) fn transform_explainable_statements(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        Err(not_ported("TransformExplainableStatements", parse_result))
    }
}
