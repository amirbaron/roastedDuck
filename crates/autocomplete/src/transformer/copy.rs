//! The transform functions of DuckDB's `transform_copy.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformCopyStatement`.
    pub(super) fn transform_copy_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformCopyStatement", parse_result))
    }

    /// `TransformCopySelect`.
    pub(super) fn transform_copy_select(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformCopySelect", parse_result))
    }

    /// `TransformCopyFromDatabase`.
    pub(super) fn transform_copy_from_database(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        Err(not_ported("TransformCopyFromDatabase", parse_result))
    }

    /// `TransformCopyDatabaseFlag`.
    pub(super) fn transform_copy_database_flag(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CopyDatabaseType> {
        Err(not_ported("TransformCopyDatabaseFlag", parse_result))
    }

    /// `TransformSchemaOrData`.
    pub(super) fn transform_schema_or_data(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CopyDatabaseType> {
        Err(not_ported("TransformSchemaOrData", parse_result))
    }

    /// `TransformCopyTable`.
    pub(super) fn transform_copy_table(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformCopyTable", parse_result))
    }

    /// `TransformFromOrTo`.
    pub(super) fn transform_from_or_to(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformFromOrTo", parse_result))
    }

    /// `TransformCopyFileName`.
    pub(super) fn transform_copy_file_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformCopyFileName", parse_result))
    }

    /// `TransformIdentifierColId`.
    pub(super) fn transform_identifier_col_id(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformIdentifierColId", parse_result))
    }

    /// `TransformCopyOptions`.
    pub(super) fn transform_copy_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<GenericCopyOption>> {
        Err(not_ported("TransformCopyOptions", parse_result))
    }

    /// `TransformSpecializedOptionList`.
    pub(super) fn transform_specialized_option_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<GenericCopyOption>> {
        Err(not_ported("TransformSpecializedOptionList", parse_result))
    }

    /// `TransformSpecializedOption`.
    pub(super) fn transform_specialized_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        Err(not_ported("TransformSpecializedOption", parse_result))
    }

    /// `TransformSingleOption`.
    pub(super) fn transform_single_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        Err(not_ported("TransformSingleOption", parse_result))
    }

    /// `TransformEncodingOption`.
    pub(super) fn transform_encoding_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        Err(not_ported("TransformEncodingOption", parse_result))
    }

    /// `TransformForceQuoteOption`.
    pub(super) fn transform_force_quote_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        Err(not_ported("TransformForceQuoteOption", parse_result))
    }

    /// `TransformQuoteAsOption`.
    pub(super) fn transform_quote_as_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        Err(not_ported("TransformQuoteAsOption", parse_result))
    }

    /// `TransformForceNullOption`.
    pub(super) fn transform_force_null_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        Err(not_ported("TransformForceNullOption", parse_result))
    }

    /// `TransformPartitionByOption`.
    pub(super) fn transform_partition_by_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        Err(not_ported("TransformPartitionByOption", parse_result))
    }

    /// `TransformNullAsOption`.
    pub(super) fn transform_null_as_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        Err(not_ported("TransformNullAsOption", parse_result))
    }

    /// `TransformDelimiterAsOption`.
    pub(super) fn transform_delimiter_as_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        Err(not_ported("TransformDelimiterAsOption", parse_result))
    }

    /// `TransformEscapeAsOption`.
    pub(super) fn transform_escape_as_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GenericCopyOption> {
        Err(not_ported("TransformEscapeAsOption", parse_result))
    }
}
