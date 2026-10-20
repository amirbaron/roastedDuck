//! The transform functions of DuckDB's `transform_create_table.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformCreateStatement`.
    pub(super) fn transform_create_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformCreateStatement", parse_result))
    }

    /// `TransformTemporary`.
    pub(super) fn transform_temporary(&mut self, parse_result: &ParseResult) -> Result<SecretPersistType> {
        Err(not_ported("TransformTemporary", parse_result))
    }

    /// `TransformCreateStatementVariation`.
    pub(super) fn transform_create_statement_variation(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CreateInfo> {
        Err(not_ported("TransformCreateStatementVariation", parse_result))
    }

    /// `TransformCreateTableStmt`.
    pub(super) fn transform_create_table_stmt(&mut self, parse_result: &ParseResult) -> Result<CreateInfo> {
        Err(not_ported("TransformCreateTableStmt", parse_result))
    }

    /// `TransformCreateTableAs`.
    pub(super) fn transform_create_table_as(&mut self, parse_result: &ParseResult) -> Result<CreateTableAs> {
        Err(not_ported("TransformCreateTableAs", parse_result))
    }

    /// `TransformIdentifierList`.
    pub(super) fn transform_identifier_list(&mut self, parse_result: &ParseResult) -> Result<ColumnList> {
        Err(not_ported("TransformIdentifierList", parse_result))
    }

    /// `TransformCreateColumnList`.
    pub(super) fn transform_create_column_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ColumnElements> {
        Err(not_ported("TransformCreateColumnList", parse_result))
    }

    /// `TransformCreateTableColumnList`.
    pub(super) fn transform_create_table_column_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ColumnElements> {
        Err(not_ported("TransformCreateTableColumnList", parse_result))
    }

    /// `TransformIdentifierOrStringLiteral`.
    pub(super) fn transform_identifier_or_string_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Err(not_ported("TransformIdentifierOrStringLiteral", parse_result))
    }

    /// `TransformColIdOrString`.
    pub(super) fn transform_col_id_or_string(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformColIdOrString", parse_result))
    }

    /// `TransformColLabelOrString`.
    pub(super) fn transform_col_label_or_string(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformColLabelOrString", parse_result))
    }

    /// `TransformColId`.
    pub(super) fn transform_col_id(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformColId", parse_result))
    }

    /// `TransformIdentifier`.
    pub(super) fn transform_identifier(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformIdentifier", parse_result))
    }

    /// `TransformDottedIdentifier`.
    pub(super) fn transform_dotted_identifier(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        Err(not_ported("TransformDottedIdentifier", parse_result))
    }

    /// `TransformColumnDefinition`.
    pub(super) fn transform_column_definition(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ConstraintColumnDefinition> {
        Err(not_ported("TransformColumnDefinition", parse_result))
    }

    /// `TransformGeneratedColumn`.
    pub(super) fn transform_generated_column(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GeneratedColumnDefinition> {
        Err(not_ported("TransformGeneratedColumn", parse_result))
    }

    /// `TransformDefaultValue`.
    pub(super) fn transform_default_value(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformDefaultValue", parse_result))
    }

    /// `TransformTopLevelConstraint`.
    pub(super) fn transform_top_level_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Constraint> {
        Err(not_ported("TransformTopLevelConstraint", parse_result))
    }

    /// `TransformTopLevelConstraintList`.
    pub(super) fn transform_top_level_constraint_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Constraint> {
        Err(not_ported("TransformTopLevelConstraintList", parse_result))
    }

    /// `TransformTopPrimaryKeyConstraint`.
    pub(super) fn transform_top_primary_key_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Constraint> {
        Err(not_ported("TransformTopPrimaryKeyConstraint", parse_result))
    }

    /// `TransformTopUniqueConstraint`.
    pub(super) fn transform_top_unique_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Constraint> {
        Err(not_ported("TransformTopUniqueConstraint", parse_result))
    }

    /// `TransformCheckConstraint`.
    pub(super) fn transform_check_constraint(&mut self, parse_result: &ParseResult) -> Result<Constraint> {
        Err(not_ported("TransformCheckConstraint", parse_result))
    }

    /// `TransformTopForeignKeyConstraint`.
    pub(super) fn transform_top_foreign_key_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Constraint> {
        Err(not_ported("TransformTopForeignKeyConstraint", parse_result))
    }

    /// `TransformColumnIdList`.
    pub(super) fn transform_column_id_list(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        Err(not_ported("TransformColumnIdList", parse_result))
    }

    /// `TransformTypeFuncName`.
    pub(super) fn transform_type_func_name(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformTypeFuncName", parse_result))
    }

    /// `TransformColumnCompression`.
    pub(super) fn transform_column_compression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CompressionType> {
        Err(not_ported("TransformColumnCompression", parse_result))
    }

    /// `TransformForeignKeyConstraint`.
    pub(super) fn transform_foreign_key_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ForeignKeyConstraint> {
        Err(not_ported("TransformForeignKeyConstraint", parse_result))
    }

    /// `TransformKeyActions`.
    pub(super) fn transform_key_actions(&mut self, parse_result: &ParseResult) -> Result<KeyActions> {
        Err(not_ported("TransformKeyActions", parse_result))
    }

    /// `TransformUpdateAction`.
    pub(super) fn transform_update_action(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformUpdateAction", parse_result))
    }

    /// `TransformDeleteAction`.
    pub(super) fn transform_delete_action(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformDeleteAction", parse_result))
    }

    /// `TransformKeyAction`.
    pub(super) fn transform_key_action(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformKeyAction", parse_result))
    }

    /// `TransformNoKeyAction`.
    pub(super) fn transform_no_key_action(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformNoKeyAction", parse_result))
    }

    /// `TransformRestrictKeyAction`.
    pub(super) fn transform_restrict_key_action(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformRestrictKeyAction", parse_result))
    }

    /// `TransformCascadeKeyAction`.
    pub(super) fn transform_cascade_key_action(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformCascadeKeyAction", parse_result))
    }

    /// `TransformSetNullKeyAction`.
    pub(super) fn transform_set_null_key_action(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformSetNullKeyAction", parse_result))
    }

    /// `TransformSetDefaultKeyAction`.
    pub(super) fn transform_set_default_key_action(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformSetDefaultKeyAction", parse_result))
    }

    /// `TransformPrimaryKeyConstraint`.
    pub(super) fn transform_primary_key_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(bool, ConstraintType)> {
        Err(not_ported("TransformPrimaryKeyConstraint", parse_result))
    }

    /// `TransformUniqueConstraint`.
    pub(super) fn transform_unique_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(bool, ConstraintType)> {
        Err(not_ported("TransformUniqueConstraint", parse_result))
    }

    /// `TransformNotNullConstraint`.
    pub(super) fn transform_not_null_constraint(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(bool, ConstraintType)> {
        Err(not_ported("TransformNotNullConstraint", parse_result))
    }

    /// `TransformColumnCollation`.
    pub(super) fn transform_column_collation(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformColumnCollation", parse_result))
    }

    /// `TransformWithData`.
    pub(super) fn transform_with_data(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformWithData", parse_result))
    }

    /// `TransformCommitAction`.
    pub(super) fn transform_commit_action(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformCommitAction", parse_result))
    }

    /// `TransformPreserveOrDelete`.
    pub(super) fn transform_preserve_or_delete(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformPreserveOrDelete", parse_result))
    }

    /// `TransformGeneratedColumnType`.
    pub(super) fn transform_generated_column_type(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformGeneratedColumnType", parse_result))
    }

    /// `TransformPartitionOptions`.
    pub(super) fn transform_partition_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformPartitionOptions", parse_result))
    }

    /// `TransformSortedOptions`.
    pub(super) fn transform_sorted_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformSortedOptions", parse_result))
    }

    /// `TransformPartitionOptSortedOptions`.
    pub(super) fn transform_partition_opt_sorted_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<PartitionSortedOptions> {
        Err(not_ported("TransformPartitionOptSortedOptions", parse_result))
    }

    /// `TransformSortedOptPartitionOptions`.
    pub(super) fn transform_sorted_opt_partition_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<PartitionSortedOptions> {
        Err(not_ported("TransformSortedOptPartitionOptions", parse_result))
    }

    /// `TransformPartitionSortedOptions`.
    pub(super) fn transform_partition_sorted_options(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<PartitionSortedOptions> {
        Err(not_ported("TransformPartitionSortedOptions", parse_result))
    }
}
