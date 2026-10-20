//! The transform functions of DuckDB's `transform_load.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformLoadStatement`.
    pub(super) fn transform_load_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformLoadStatement", parse_result))
    }

    /// `TransformInstallStatement`.
    pub(super) fn transform_install_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformInstallStatement", parse_result))
    }

    /// `TransformFromSource`.
    pub(super) fn transform_from_source(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ExtensionRepositoryInfo> {
        Err(not_ported("TransformFromSource", parse_result))
    }

    /// `TransformUpdateExtensionsStatement`.
    pub(super) fn transform_update_extensions_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        Err(not_ported("TransformUpdateExtensionsStatement", parse_result))
    }

    /// `TransformVersionNumber`.
    pub(super) fn transform_version_number(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformVersionNumber", parse_result))
    }
}
