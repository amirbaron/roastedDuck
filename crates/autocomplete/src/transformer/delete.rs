//! The transform functions of DuckDB's `transform_delete.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformDeleteStatement`.
    pub(super) fn transform_delete_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformDeleteStatement", parse_result))
    }

    /// `TransformTargetOptAlias`.
    pub(super) fn transform_target_opt_alias(&mut self, parse_result: &ParseResult) -> Result<BaseTable> {
        Err(not_ported("TransformTargetOptAlias", parse_result))
    }

    /// `TransformDeleteUsingClause`.
    pub(super) fn transform_delete_using_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<TableRef>> {
        Err(not_ported("TransformDeleteUsingClause", parse_result))
    }

    /// `TransformTruncateStatement`.
    pub(super) fn transform_truncate_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        Err(not_ported("TransformTruncateStatement", parse_result))
    }
}
