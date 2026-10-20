//! The transform functions of DuckDB's `transform_pragma.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformPragmaStatement`.
    pub(super) fn transform_pragma_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformPragmaStatement", parse_result))
    }

    /// `TransformPragmaAssign`.
    pub(super) fn transform_pragma_assign(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformPragmaAssign", parse_result))
    }

    /// `TransformPragmaFunction`.
    pub(super) fn transform_pragma_function(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformPragmaFunction", parse_result))
    }

    /// `TransformPragmaParameters`.
    pub(super) fn transform_pragma_parameters(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformPragmaParameters", parse_result))
    }
}
