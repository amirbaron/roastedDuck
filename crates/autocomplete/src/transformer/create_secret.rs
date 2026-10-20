//! The transform functions of DuckDB's `transform_create_secret.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformCreateSecretStmt`.
    pub(super) fn transform_create_secret_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        Err(not_ported("TransformCreateSecretStmt", parse_result))
    }

    /// `TransformSecretName`.
    pub(super) fn transform_secret_name(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformSecretName", parse_result))
    }

    /// `TransformSecretStorageSpecifier`.
    pub(super) fn transform_secret_storage_specifier(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<String> {
        Err(not_ported("TransformSecretStorageSpecifier", parse_result))
    }
}
