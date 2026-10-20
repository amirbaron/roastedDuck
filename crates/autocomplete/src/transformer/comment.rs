//! The transform functions of DuckDB's `transform_comment.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformCommentStatement`.
    pub(super) fn transform_comment_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformCommentStatement", parse_result))
    }

    /// `TransformCommentOnType`.
    pub(super) fn transform_comment_on_type(&mut self, parse_result: &ParseResult) -> Result<CatalogType> {
        Err(not_ported("TransformCommentOnType", parse_result))
    }

    /// `TransformCommentValue`.
    pub(super) fn transform_comment_value(&mut self, parse_result: &ParseResult) -> Result<Value> {
        Err(not_ported("TransformCommentValue", parse_result))
    }
}
