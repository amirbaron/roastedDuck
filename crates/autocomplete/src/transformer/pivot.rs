//! The transform functions of DuckDB's `transform_pivot.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformPivotOn`.
    pub(super) fn transform_pivot_on(&mut self, parse_result: &ParseResult) -> Result<Vec<PivotColumn>> {
        Err(not_ported("TransformPivotOn", parse_result))
    }

    /// `TransformPivotColumnList`.
    pub(super) fn transform_pivot_column_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<PivotColumn>> {
        Err(not_ported("TransformPivotColumnList", parse_result))
    }

    /// `TransformPivotColumnEntry`.
    pub(super) fn transform_pivot_column_entry(&mut self, parse_result: &ParseResult) -> Result<PivotColumn> {
        Err(not_ported("TransformPivotColumnEntry", parse_result))
    }

    /// `TransformPivotColumnSubquery`.
    pub(super) fn transform_pivot_column_subquery(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<PivotColumn> {
        Err(not_ported("TransformPivotColumnSubquery", parse_result))
    }

    /// `TransformPivotColumnEntryInternal`.
    pub(super) fn transform_pivot_column_entry_internal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<PivotColumn> {
        Err(not_ported("TransformPivotColumnEntryInternal", parse_result))
    }

    /// `TransformPivotStatement`.
    pub(super) fn transform_pivot_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        Err(not_ported("TransformPivotStatement", parse_result))
    }

    /// `TransformPivotUsing`.
    pub(super) fn transform_pivot_using(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformPivotUsing", parse_result))
    }

    /// `TransformUnpivotHeader`.
    pub(super) fn transform_unpivot_header(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        Err(not_ported("TransformUnpivotHeader", parse_result))
    }

    /// `TransformUnpivotHeaderSingle`.
    pub(super) fn transform_unpivot_header_single(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<String>> {
        Err(not_ported("TransformUnpivotHeaderSingle", parse_result))
    }

    /// `TransformUnpivotHeaderList`.
    pub(super) fn transform_unpivot_header_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<String>> {
        Err(not_ported("TransformUnpivotHeaderList", parse_result))
    }

    /// `TransformIncludeOrExcludeNulls`.
    pub(super) fn transform_include_or_exclude_nulls(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformIncludeOrExcludeNulls", parse_result))
    }

    /// `TransformUnpivotStatement`.
    pub(super) fn transform_unpivot_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        Err(not_ported("TransformUnpivotStatement", parse_result))
    }

    /// `TransformIntoNameValues`.
    pub(super) fn transform_into_name_values(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<UnpivotNameValues> {
        Err(not_ported("TransformIntoNameValues", parse_result))
    }

    /// `PEGTransformer::CreatePivotStatement`.
    pub(super) fn create_pivot_statement(&mut self, statement: SqlStatement) -> Result<SqlStatement> {
        let _ = statement;
        Err(Error::not_implemented("PEG transformer CreatePivotStatement"))
    }
}
