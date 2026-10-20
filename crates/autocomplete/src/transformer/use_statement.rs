//! The transform functions of DuckDB's `transform_use.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformUseStatement`.
    pub(super) fn transform_use_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformUseStatement", parse_result))
    }

    /// `TransformUseTarget`.
    pub(super) fn transform_use_target(&mut self, parse_result: &ParseResult) -> Result<QualifiedName> {
        Err(not_ported("TransformUseTarget", parse_result))
    }

    /// `TransformUseTargetCatalogSchema`.
    pub(super) fn transform_use_target_catalog_schema(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Err(not_ported("TransformUseTargetCatalogSchema", parse_result))
    }
}
