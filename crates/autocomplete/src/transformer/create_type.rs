//! The transform functions of DuckDB's `transform_create_type.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformCreateTypeStmt`.
    pub(super) fn transform_create_type_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        Err(not_ported("TransformCreateTypeStmt", parse_result))
    }

    /// `TransformCreateType`.
    pub(super) fn transform_create_type(&mut self, parse_result: &ParseResult) -> Result<CreateTypeInfo> {
        Err(not_ported("TransformCreateType", parse_result))
    }

    /// `TransformEnumSelectType`.
    pub(super) fn transform_enum_select_type(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        Err(not_ported("TransformEnumSelectType", parse_result))
    }

    /// `TransformEnumStringLiteralList`.
    pub(super) fn transform_enum_string_literal_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<LogicalType> {
        Err(not_ported("TransformEnumStringLiteralList", parse_result))
    }
}
