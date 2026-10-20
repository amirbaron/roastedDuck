//! The transform functions of DuckDB's `transform_vacuum.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformVacuumStatement`.
    pub(super) fn transform_vacuum_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformVacuumStatement", parse_result))
    }

    /// `TransformVacuumOptions`.
    pub(super) fn transform_vacuum_options(&mut self, parse_result: &ParseResult) -> Result<VacuumOptions> {
        Err(not_ported("TransformVacuumOptions", parse_result))
    }

    /// `TransformVacuumLegacyOptions`.
    pub(super) fn transform_vacuum_legacy_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<VacuumOptions> {
        Err(not_ported("TransformVacuumLegacyOptions", parse_result))
    }

    /// `TransformVacuumParensOptions`.
    pub(super) fn transform_vacuum_parens_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<VacuumOptions> {
        Err(not_ported("TransformVacuumParensOptions", parse_result))
    }

    /// `TransformVacuumOption`.
    pub(super) fn transform_vacuum_option(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformVacuumOption", parse_result))
    }

    /// `TransformNameList`.
    pub(super) fn transform_name_list(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        Err(not_ported("TransformNameList", parse_result))
    }
}
