//! The transform functions of DuckDB's `transform_update.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformUpdateStatement`.
    pub(super) fn transform_update_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformUpdateStatement", parse_result))
    }

    /// `TransformUpdateTarget`.
    pub(super) fn transform_update_target(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformUpdateTarget", parse_result))
    }

    /// `TransformBaseTableSet`.
    pub(super) fn transform_base_table_set(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformBaseTableSet", parse_result))
    }

    /// `TransformBaseTableAliasSet`.
    pub(super) fn transform_base_table_alias_set(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformBaseTableAliasSet", parse_result))
    }

    /// `TransformUpdateAlias`.
    pub(super) fn transform_update_alias(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformUpdateAlias", parse_result))
    }

    /// `TransformUpdateSetClause`.
    pub(super) fn transform_update_set_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<UpdateSetInfo> {
        Err(not_ported("TransformUpdateSetClause", parse_result))
    }

    /// `TransformUpdateSetTuple`.
    pub(super) fn transform_update_set_tuple(&mut self, parse_result: &ParseResult) -> Result<UpdateSetInfo> {
        Err(not_ported("TransformUpdateSetTuple", parse_result))
    }

    /// `TransformUpdateSetElementList`.
    pub(super) fn transform_update_set_element_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<UpdateSetInfo> {
        Err(not_ported("TransformUpdateSetElementList", parse_result))
    }

    /// `TransformUpdateSetElement`.
    pub(super) fn transform_update_set_element(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, ParsedExpression)> {
        Err(not_ported("TransformUpdateSetElement", parse_result))
    }

    /// `TransformUpdateSetColumnTarget`.
    pub(super) fn transform_update_set_column_target(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<String> {
        Err(not_ported("TransformUpdateSetColumnTarget", parse_result))
    }
}
