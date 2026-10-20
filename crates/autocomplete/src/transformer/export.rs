//! The transform functions of DuckDB's `transform_export.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformExportStatement`.
    pub(super) fn transform_export_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformExportStatement", parse_result))
    }

    /// `TransformExportSource`.
    pub(super) fn transform_export_source(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformExportSource", parse_result))
    }
}
