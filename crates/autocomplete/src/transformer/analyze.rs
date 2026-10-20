//! The transform functions of DuckDB's `transform_analyze.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformAnalyzeStatement`.
    pub(super) fn transform_analyze_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformAnalyzeStatement", parse_result))
    }

    /// `TransformAnalyzeTarget`.
    pub(super) fn transform_analyze_target(&mut self, parse_result: &ParseResult) -> Result<AnalyzeTarget> {
        Err(not_ported("TransformAnalyzeTarget", parse_result))
    }
}
