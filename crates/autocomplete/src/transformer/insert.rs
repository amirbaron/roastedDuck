//! The transform functions of DuckDB's `transform_insert.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformInsertStatement`.
    pub(super) fn transform_insert_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformInsertStatement", parse_result))
    }

    /// `TransformOrAction`.
    pub(super) fn transform_or_action(&mut self, parse_result: &ParseResult) -> Result<OnConflictAction> {
        Err(not_ported("TransformOrAction", parse_result))
    }

    /// `TransformInsertTarget`.
    pub(super) fn transform_insert_target(&mut self, parse_result: &ParseResult) -> Result<BaseTable> {
        Err(not_ported("TransformInsertTarget", parse_result))
    }

    /// `TransformInsertAlias`.
    pub(super) fn transform_insert_alias(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformInsertAlias", parse_result))
    }

    /// `TransformOnConflictClause`.
    pub(super) fn transform_on_conflict_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OnConflictInfo> {
        Err(not_ported("TransformOnConflictClause", parse_result))
    }

    /// `TransformOnConflictTarget`.
    pub(super) fn transform_on_conflict_target(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OnConflictExpressionTarget> {
        Err(not_ported("TransformOnConflictTarget", parse_result))
    }

    /// `TransformOnConflictExpressionTarget`.
    pub(super) fn transform_on_conflict_expression_target(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OnConflictExpressionTarget> {
        Err(not_ported("TransformOnConflictExpressionTarget", parse_result))
    }

    /// `TransformOnConflictIndexTarget`.
    pub(super) fn transform_on_conflict_index_target(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OnConflictExpressionTarget> {
        Err(not_ported("TransformOnConflictIndexTarget", parse_result))
    }

    /// `TransformOnConflictAction`.
    pub(super) fn transform_on_conflict_action(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OnConflictInfo> {
        Err(not_ported("TransformOnConflictAction", parse_result))
    }

    /// `TransformOnConflictUpdate`.
    pub(super) fn transform_on_conflict_update(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OnConflictInfo> {
        Err(not_ported("TransformOnConflictUpdate", parse_result))
    }

    /// `TransformOnConflictNothing`.
    pub(super) fn transform_on_conflict_nothing(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OnConflictInfo> {
        Err(not_ported("TransformOnConflictNothing", parse_result))
    }

    /// `TransformInsertValues`.
    pub(super) fn transform_insert_values(&mut self, parse_result: &ParseResult) -> Result<InsertValues> {
        Err(not_ported("TransformInsertValues", parse_result))
    }

    /// `TransformByNameOrPosition`.
    pub(super) fn transform_by_name_or_position(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<InsertColumnOrder> {
        Err(not_ported("TransformByNameOrPosition", parse_result))
    }

    /// `TransformInsertColumnList`.
    pub(super) fn transform_insert_column_list(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        Err(not_ported("TransformInsertColumnList", parse_result))
    }

    /// `TransformColumnList`.
    pub(super) fn transform_column_list(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        Err(not_ported("TransformColumnList", parse_result))
    }

    /// `TransformReturningClause`.
    pub(super) fn transform_returning_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformReturningClause", parse_result))
    }
}
