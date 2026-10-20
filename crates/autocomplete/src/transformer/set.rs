//! The transform functions of DuckDB's `transform_set.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformResetStatement`.
    pub(super) fn transform_reset_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformResetStatement", parse_result))
    }

    /// `TransformSetAssignment`.
    pub(super) fn transform_set_assignment(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformSetAssignment", parse_result))
    }

    /// `TransformSetSetting`.
    pub(super) fn transform_set_setting(&mut self, parse_result: &ParseResult) -> Result<SettingInfo> {
        Err(not_ported("TransformSetSetting", parse_result))
    }

    /// `TransformSetStatement`.
    pub(super) fn transform_set_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformSetStatement", parse_result))
    }

    /// `TransformSetTimeZone`.
    pub(super) fn transform_set_time_zone(&mut self, parse_result: &ParseResult) -> Result<SetStatement> {
        Err(not_ported("TransformSetTimeZone", parse_result))
    }

    /// `TransformSetVariable`.
    pub(super) fn transform_set_variable(&mut self, parse_result: &ParseResult) -> Result<SettingInfo> {
        Err(not_ported("TransformSetVariable", parse_result))
    }

    /// `TransformStandardAssignment`.
    pub(super) fn transform_standard_assignment(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SetStatement> {
        Err(not_ported("TransformStandardAssignment", parse_result))
    }

    /// `TransformVariableList`.
    pub(super) fn transform_variable_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformVariableList", parse_result))
    }
}
