//! The transform functions of DuckDB's `transform_attach.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformAttachStatement`.
    pub(super) fn transform_attach_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformAttachStatement", parse_result))
    }

    /// `TransformAttachAlias`.
    pub(super) fn transform_attach_alias(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformAttachAlias", parse_result))
    }

    /// `TransformAttachOptions`.
    pub(super) fn transform_attach_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<GenericCopyOption>> {
        Err(not_ported("TransformAttachOptions", parse_result))
    }

    /// `TransformGenericCopyOptionList`.
    pub(super) fn transform_generic_copy_option_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<GenericCopyOption>> {
        Err(not_ported("TransformGenericCopyOptionList", parse_result))
    }

    /// `TransformGenericCopyOption`.
    pub(super) fn transform_generic_copy_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        Err(not_ported("TransformGenericCopyOption", parse_result))
    }

    /// `TransformDatabasePath`.
    pub(super) fn transform_database_path(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformDatabasePath", parse_result))
    }
}
