//! The transform functions of DuckDB's `transform_drop.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformDropStatement`.
    pub(super) fn transform_drop_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformDropStatement", parse_result))
    }

    /// `TransformDropEntries`.
    pub(super) fn transform_drop_entries(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        Err(not_ported("TransformDropEntries", parse_result))
    }

    /// `TransformDropTable`.
    pub(super) fn transform_drop_table(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        Err(not_ported("TransformDropTable", parse_result))
    }

    /// `TransformTableOrView`.
    pub(super) fn transform_table_or_view(&mut self, parse_result: &ParseResult) -> Result<CatalogType> {
        Err(not_ported("TransformTableOrView", parse_result))
    }

    /// `TransformDropTableFunction`.
    pub(super) fn transform_drop_table_function(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        Err(not_ported("TransformDropTableFunction", parse_result))
    }

    /// `TransformDropFunction`.
    pub(super) fn transform_drop_function(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        Err(not_ported("TransformDropFunction", parse_result))
    }

    /// `TransformDropSchema`.
    pub(super) fn transform_drop_schema(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        Err(not_ported("TransformDropSchema", parse_result))
    }

    /// `TransformQualifiedSchemaName`.
    pub(super) fn transform_qualified_schema_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Err(not_ported("TransformQualifiedSchemaName", parse_result))
    }

    /// `TransformDropIndex`.
    pub(super) fn transform_drop_index(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        Err(not_ported("TransformDropIndex", parse_result))
    }

    /// `TransformQualifiedIndexName`.
    pub(super) fn transform_qualified_index_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Err(not_ported("TransformQualifiedIndexName", parse_result))
    }

    /// `TransformDropSequence`.
    pub(super) fn transform_drop_sequence(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        Err(not_ported("TransformDropSequence", parse_result))
    }

    /// `TransformDropCollation`.
    pub(super) fn transform_drop_collation(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        Err(not_ported("TransformDropCollation", parse_result))
    }

    /// `TransformDropType`.
    pub(super) fn transform_drop_type(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        Err(not_ported("TransformDropType", parse_result))
    }

    /// `TransformDropBehavior`.
    pub(super) fn transform_drop_behavior(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformDropBehavior", parse_result))
    }

    /// `TransformDropSecret`.
    pub(super) fn transform_drop_secret(&mut self, parse_result: &ParseResult) -> Result<DropInfo> {
        Err(not_ported("TransformDropSecret", parse_result))
    }

    /// `TransformDropSecretStorage`.
    pub(super) fn transform_drop_secret_storage(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformDropSecretStorage", parse_result))
    }
}
