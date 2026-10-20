//! The transform functions of DuckDB's `transform_import.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformImportStatement`.
    pub(super) fn transform_import_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformImportStatement", parse_result))
    }
}
