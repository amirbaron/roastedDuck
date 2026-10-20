//! The transform functions of DuckDB's `transform_merge_into.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformMergeIntoStatement`.
    pub(super) fn transform_merge_into_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SqlStatement> {
        Err(not_ported("TransformMergeIntoStatement", parse_result))
    }

    /// `TransformMergeIntoUsingClause`.
    pub(super) fn transform_merge_into_using_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TableRef> {
        Err(not_ported("TransformMergeIntoUsingClause", parse_result))
    }

    /// `TransformMergeMatch`.
    pub(super) fn transform_merge_match(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(MergeActionCondition, MergeIntoAction)> {
        Err(not_ported("TransformMergeMatch", parse_result))
    }

    /// `TransformMatchedClause`.
    pub(super) fn transform_matched_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(MergeActionCondition, MergeIntoAction)> {
        Err(not_ported("TransformMatchedClause", parse_result))
    }

    /// `TransformMatchedClauseAction`.
    pub(super) fn transform_matched_clause_action(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Err(not_ported("TransformMatchedClauseAction", parse_result))
    }

    /// `TransformUpdateMatchClause`.
    pub(super) fn transform_update_match_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Err(not_ported("TransformUpdateMatchClause", parse_result))
    }

    /// `TransformUpdateMatchInfo`.
    pub(super) fn transform_update_match_info(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Err(not_ported("TransformUpdateMatchInfo", parse_result))
    }

    /// `TransformUpdateMatchSetClause`.
    pub(super) fn transform_update_match_set_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<UpdateSetInfo> {
        Err(not_ported("TransformUpdateMatchSetClause", parse_result))
    }

    /// `TransformDeleteMatchClause`.
    pub(super) fn transform_delete_match_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Err(not_ported("TransformDeleteMatchClause", parse_result))
    }

    /// `TransformInsertMatchClause`.
    pub(super) fn transform_insert_match_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Err(not_ported("TransformInsertMatchClause", parse_result))
    }

    /// `TransformInsertMatchInfo`.
    pub(super) fn transform_insert_match_info(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Err(not_ported("TransformInsertMatchInfo", parse_result))
    }

    /// `TransformInsertDefaultValues`.
    pub(super) fn transform_insert_default_values(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Err(not_ported("TransformInsertDefaultValues", parse_result))
    }

    /// `TransformInsertByNameOrPosition`.
    pub(super) fn transform_insert_by_name_or_position(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Err(not_ported("TransformInsertByNameOrPosition", parse_result))
    }

    /// `TransformInsertValuesList`.
    pub(super) fn transform_insert_values_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Err(not_ported("TransformInsertValuesList", parse_result))
    }

    /// `TransformDoNothingMatchClause`.
    pub(super) fn transform_do_nothing_match_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Err(not_ported("TransformDoNothingMatchClause", parse_result))
    }

    /// `TransformErrorMatchClause`.
    pub(super) fn transform_error_match_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeIntoAction> {
        Err(not_ported("TransformErrorMatchClause", parse_result))
    }

    /// `TransformAndExpression`.
    pub(super) fn transform_and_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformAndExpression", parse_result))
    }

    /// `TransformNotMatchedClause`.
    pub(super) fn transform_not_matched_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(MergeActionCondition, MergeIntoAction)> {
        Err(not_ported("TransformNotMatchedClause", parse_result))
    }

    /// `TransformBySourceOrTarget`.
    pub(super) fn transform_by_source_or_target(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MergeActionCondition> {
        Err(not_ported("TransformBySourceOrTarget", parse_result))
    }
}
