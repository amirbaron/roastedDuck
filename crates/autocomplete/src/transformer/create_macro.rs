//! The transform functions of DuckDB's `transform_create_macro.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformCreateMacroStmt`.
    pub(super) fn transform_create_macro_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        Err(not_ported("TransformCreateMacroStmt", parse_result))
    }

    /// `TransformMacroDefinition`.
    pub(super) fn transform_macro_definition(&mut self, parse_result: &ParseResult) -> Result<MacroFunction> {
        Err(not_ported("TransformMacroDefinition", parse_result))
    }

    /// `TransformTableMacroDefinition`.
    pub(super) fn transform_table_macro_definition(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MacroFunction> {
        Err(not_ported("TransformTableMacroDefinition", parse_result))
    }

    /// `TransformScalarMacroDefinition`.
    pub(super) fn transform_scalar_macro_definition(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MacroFunction> {
        Err(not_ported("TransformScalarMacroDefinition", parse_result))
    }

    /// `TransformMacroParameters`.
    pub(super) fn transform_macro_parameters(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<MacroParameter>> {
        Err(not_ported("TransformMacroParameters", parse_result))
    }

    /// `TransformMacroParameter`.
    pub(super) fn transform_macro_parameter(&mut self, parse_result: &ParseResult) -> Result<MacroParameter> {
        Err(not_ported("TransformMacroParameter", parse_result))
    }

    /// `TransformSimpleParameter`.
    pub(super) fn transform_simple_parameter(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<MacroParameter> {
        Err(not_ported("TransformSimpleParameter", parse_result))
    }
}
