//! The transform functions of DuckDB's `transform_alter.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformAlterStatement`.
    pub(super) fn transform_alter_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformAlterStatement", parse_result))
    }

    /// `TransformAlterOptions`.
    pub(super) fn transform_alter_options(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAlterOptions", parse_result))
    }

    /// `TransformAlterTableStmt`.
    pub(super) fn transform_alter_table_stmt(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAlterTableStmt", parse_result))
    }

    /// `TransformAlterDatabaseStmt`.
    pub(super) fn transform_alter_database_stmt(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAlterDatabaseStmt", parse_result))
    }

    /// `TransformAlterViewStmt`.
    pub(super) fn transform_alter_view_stmt(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAlterViewStmt", parse_result))
    }

    /// `TransformAlterSchemaStmt`.
    pub(super) fn transform_alter_schema_stmt(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAlterSchemaStmt", parse_result))
    }

    /// `TransformAlterSequenceStmt`.
    pub(super) fn transform_alter_sequence_stmt(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAlterSequenceStmt", parse_result))
    }

    /// `TransformQualifiedSequenceName`.
    pub(super) fn transform_qualified_sequence_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Err(not_ported("TransformQualifiedSequenceName", parse_result))
    }

    /// `TransformAlterSequenceOptions`.
    pub(super) fn transform_alter_sequence_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<AlterInfo> {
        Err(not_ported("TransformAlterSequenceOptions", parse_result))
    }

    /// `TransformSetSequenceOption`.
    pub(super) fn transform_set_sequence_option(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformSetSequenceOption", parse_result))
    }

    /// `TransformAlterTableOptions`.
    pub(super) fn transform_alter_table_options(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAlterTableOptions", parse_result))
    }

    /// `TransformAddColumn`.
    pub(super) fn transform_add_column(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAddColumn", parse_result))
    }

    /// `TransformAddColumnEntry`.
    pub(super) fn transform_add_column_entry(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<AddColumnEntry> {
        Err(not_ported("TransformAddColumnEntry", parse_result))
    }

    /// `TransformDropColumn`.
    pub(super) fn transform_drop_column(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformDropColumn", parse_result))
    }

    /// `TransformAlterColumn`.
    pub(super) fn transform_alter_column(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAlterColumn", parse_result))
    }

    /// `TransformAlterColumnEntry`.
    pub(super) fn transform_alter_column_entry(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAlterColumnEntry", parse_result))
    }

    /// `TransformDropDefault`.
    pub(super) fn transform_drop_default(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformDropDefault", parse_result))
    }

    /// `TransformChangeNullability`.
    pub(super) fn transform_change_nullability(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformChangeNullability", parse_result))
    }

    /// `TransformAlterType`.
    pub(super) fn transform_alter_type(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAlterType", parse_result))
    }

    /// `TransformUsingExpression`.
    pub(super) fn transform_using_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformUsingExpression", parse_result))
    }

    /// `TransformDropOrSet`.
    pub(super) fn transform_drop_or_set(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformDropOrSet", parse_result))
    }

    /// `TransformAddOrDropDefault`.
    pub(super) fn transform_add_or_drop_default(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAddOrDropDefault", parse_result))
    }

    /// `TransformAddDefault`.
    pub(super) fn transform_add_default(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAddDefault", parse_result))
    }

    /// `TransformRenameColumn`.
    pub(super) fn transform_rename_column(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformRenameColumn", parse_result))
    }

    /// `TransformRenameAlter`.
    pub(super) fn transform_rename_alter(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformRenameAlter", parse_result))
    }

    /// `TransformSetPartitionedBy`.
    pub(super) fn transform_set_partitioned_by(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformSetPartitionedBy", parse_result))
    }

    /// `TransformResetPartitionedBy`.
    pub(super) fn transform_reset_partitioned_by(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformResetPartitionedBy", parse_result))
    }

    /// `TransformAddConstraint`.
    pub(super) fn transform_add_constraint(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformAddConstraint", parse_result))
    }

    /// `TransformSequenceName`.
    pub(super) fn transform_sequence_name(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformSequenceName", parse_result))
    }

    /// `TransformSetSortedBy`.
    pub(super) fn transform_set_sorted_by(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformSetSortedBy", parse_result))
    }

    /// `TransformResetSortedBy`.
    pub(super) fn transform_reset_sorted_by(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformResetSortedBy", parse_result))
    }

    /// `TransformSetOptions`.
    pub(super) fn transform_set_options(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformSetOptions", parse_result))
    }

    /// `TransformResetOptions`.
    pub(super) fn transform_reset_options(&mut self, parse_result: &ParseResult) -> Result<AlterInfo> {
        Err(not_ported("TransformResetOptions", parse_result))
    }
}
