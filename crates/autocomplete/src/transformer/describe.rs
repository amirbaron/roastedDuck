//! The transform functions of DuckDB's `transform_describe.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformDescribeStatement`.
    pub(super) fn transform_describe_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        Err(not_ported("TransformDescribeStatement", parse_result))
    }

    /// `TransformShowSelect`.
    pub(super) fn transform_show_select(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        Err(not_ported("TransformShowSelect", parse_result))
    }

    /// `TransformShowTables`.
    pub(super) fn transform_show_tables(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        Err(not_ported("TransformShowTables", parse_result))
    }

    /// `TransformShowAllTables`.
    pub(super) fn transform_show_all_tables(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        Err(not_ported("TransformShowAllTables", parse_result))
    }

    /// `TransformShowQualifiedName`.
    pub(super) fn transform_show_qualified_name(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        Err(not_ported("TransformShowQualifiedName", parse_result))
    }

    /// `TransformShowOrDescribeOrSummarize`.
    pub(super) fn transform_show_or_describe_or_summarize(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ShowType> {
        Err(not_ported("TransformShowOrDescribeOrSummarize", parse_result))
    }

    /// `TransformShowOrDescribe`.
    pub(super) fn transform_show_or_describe(&mut self, parse_result: &ParseResult) -> Result<ShowType> {
        Err(not_ported("TransformShowOrDescribe", parse_result))
    }

    /// `TransformSummarize`.
    pub(super) fn transform_summarize(&mut self, parse_result: &ParseResult) -> Result<ShowType> {
        Err(not_ported("TransformSummarize", parse_result))
    }
}
