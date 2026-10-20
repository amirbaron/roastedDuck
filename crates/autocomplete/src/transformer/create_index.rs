//! The transform functions of DuckDB's `transform_create_index.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformCreateIndexStmt`.
    pub(super) fn transform_create_index_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        Err(not_ported("TransformCreateIndexStmt", parse_result))
    }

    /// `TransformIndexType`.
    pub(super) fn transform_index_type(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformIndexType", parse_result))
    }

    /// `TransformIndexElement`.
    pub(super) fn transform_index_element(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformIndexElement", parse_result))
    }

    /// `TransformWithList`.
    pub(super) fn transform_with_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression>> {
        Err(not_ported("TransformWithList", parse_result))
    }

    /// `TransformRelOptionOrOids`.
    pub(super) fn transform_rel_option_or_oids(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression>> {
        Err(not_ported("TransformRelOptionOrOids", parse_result))
    }

    /// `TransformRelOptionList`.
    pub(super) fn transform_rel_option_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression>> {
        Err(not_ported("TransformRelOptionList", parse_result))
    }

    /// `TransformOids`.
    pub(super) fn transform_oids(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression>> {
        Err(not_ported("TransformOids", parse_result))
    }

    /// `TransformRelOptionName`.
    pub(super) fn transform_rel_option_name(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformRelOptionName", parse_result))
    }

    /// `TransformRelOptionArgumentOpt`.
    pub(super) fn transform_rel_option_argument_opt(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformRelOptionArgumentOpt", parse_result))
    }

    /// `TransformRelOption`.
    pub(super) fn transform_rel_option(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, ParsedExpression)> {
        Err(not_ported("TransformRelOption", parse_result))
    }

    /// `TransformIndexName`.
    pub(super) fn transform_index_name(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformIndexName", parse_result))
    }
}
