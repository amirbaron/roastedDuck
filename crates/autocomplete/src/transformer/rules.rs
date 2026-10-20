//! The transform functions of the grammar rules, by result type (DuckDB's
//! `PEGTransformerFactory::Register*` with `REGISTER_TRANSFORM`), and the values of the
//! enum rules (`RegisterEnums`).
//!
//! The rule list is generated from DuckDB's `peg_transformer_factory.cpp` and the return
//! types of `peg_transformer.hpp`.

use rustdb_ast::{
    AlterInfo, AtClause, CaseCheck, CatalogType, ColumnList, ColumnRefExpression, CommonTableExpressionInfo,
    CommonTableExpressionMap, CompressionType, Constraint, ConstraintType, CopyDatabaseType, CreateInfo,
    CreateTypeInfo, DropInfo, ExpressionType, ForeignKeyConstraint, InsertColumnOrder, JoinType,
    MacroFunction, MergeActionCondition, MergeIntoAction, OnConflictAction, OnConflictInfo, OrderByNode,
    OrderByNullType, OrderType, ParsedExpression, PivotColumn, PivotColumnEntry, QualifiedColumnName,
    QueryNode, ResultModifier, SampleMethod, SampleOptions, SecretPersistType, SelectStatement,
    SetOperationType, SetScope, SetStatement, ShowType, SqlStatement, TableRef, TransactionInfo,
    TransactionModifierType, UpdateSetInfo, VacuumOptions, WindowExcludeMode,
};
use rustdb_common::{CaseInsensitiveMap, OrderedMap, Result};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use super::nodes::{
    AddColumnEntry, AnalyzeTarget, BaseTable, ColumnElements, ConstraintColumnDefinition, CreateTableAs,
    DatePartSpecifier, DistinctClause, ExtensionRepositoryInfo, GeneratedColumnDefinition, GenericCopyOption,
    GroupByNode, InsertValues, JoinPrefix, JoinQualifier, KeyActions, LimitPercentResult, MacroParameter,
    OnConflictExpressionTarget, PartitionSortedOptions, QualifiedName, SequenceOption, SettingInfo,
    TableAlias, UnpivotNameValues, WindowBoundaryExpression, WindowExpr, WindowFrame,
};
use super::parse_result::ParseResult;
use super::{
    PegTransformer, TransformEnum, TransformResult, set_expression_location, set_table_ref_location,
};

/// Implements [`TransformResult`] for each type from its rules, and lists every rule.
macro_rules! transform_rules {
    ($($ty:ty [$($locate:path)?] { $($rule:literal => $method:ident,)* })*) => {
        $(
            impl TransformResult for $ty {
                fn dispatch(transformer: &mut PegTransformer, parse_result: &ParseResult) -> Option<Result<Self>> {
                    Some(match parse_result.name {
                        $($rule => transformer.$method(parse_result),)*
                        _ => return None,
                    })
                }

                $(
                    fn set_location(&mut self, offset: Option<usize>) {
                        $locate(self, offset);
                    }
                )?
            }
        )*

        /// Whether `rule` has a transform function (of any result type).
        pub(super) fn is_transform_rule(rule: &str) -> bool {
            matches!(rule, $($($rule)|*)|*)
        }
    };
}

/// Implements [`TransformEnum`] for each type from its rules, and lists every rule.
macro_rules! enum_rules {
    ($($ty:ty { $($rule:literal => $value:expr,)* })*) => {
        $(
            impl TransformEnum for $ty {
                fn from_rule(rule: &str) -> Option<Self> {
                    Some(match rule {
                        $($rule => $value,)*
                        _ => return None,
                    })
                }
            }
        )*

        /// Whether `rule` is an enum rule (of any type).
        pub(super) fn is_enum_rule(rule: &str) -> bool {
            matches!(rule, $($($rule)|*)|*)
        }
    };
}

transform_rules! {
    SqlStatement [] {
        "AlterStatement" => transform_alter_statement,
        "AttachStatement" => transform_attach_statement,
        "AnalyzeStatement" => transform_analyze_statement,
        "CallStatement" => transform_call_statement,
        "CheckpointStatement" => transform_checkpoint_statement,
        "CommentStatement" => transform_comment_statement,
        "CopyStatement" => transform_copy_statement,
        "CopySelect" => transform_copy_select,
        "CopyFromDatabase" => transform_copy_from_database,
        "CopyTable" => transform_copy_table,
        "CreateStatement" => transform_create_statement,
        "DeallocateStatement" => transform_deallocate_statement,
        "DeleteStatement" => transform_delete_statement,
        "TruncateStatement" => transform_truncate_statement,
        "DetachStatement" => transform_detach_statement,
        "DropStatement" => transform_drop_statement,
        "ExecuteStatement" => transform_execute_statement,
        "ExplainStatement" => transform_explain_statement,
        "ExplainableStatements" => transform_explainable_statements,
        "ExportStatement" => transform_export_statement,
        "ImportStatement" => transform_import_statement,
        "InsertStatement" => transform_insert_statement,
        "LoadStatement" => transform_load_statement,
        "InstallStatement" => transform_install_statement,
        "UpdateExtensionsStatement" => transform_update_extensions_statement,
        "MergeIntoStatement" => transform_merge_into_statement,
        "PragmaStatement" => transform_pragma_statement,
        "PragmaAssign" => transform_pragma_assign,
        "PragmaFunction" => transform_pragma_function,
        "PrepareStatement" => transform_prepare_statement,
        "SelectStatement" => transform_select_statement,
        "UseStatement" => transform_use_statement,
        "ResetStatement" => transform_reset_statement,
        "SetStatement" => transform_set_statement,
        "TransactionStatement" => transform_transaction_statement,
        "UpdateStatement" => transform_update_statement,
        "VacuumStatement" => transform_vacuum_statement,
    }
    AlterInfo [] {
        "AlterOptions" => transform_alter_options,
        "AlterTableStmt" => transform_alter_table_stmt,
        "AlterViewStmt" => transform_alter_view_stmt,
        "AlterSchemaStmt" => transform_alter_schema_stmt,
        "AlterDatabaseStmt" => transform_alter_database_stmt,
        "AlterSequenceStmt" => transform_alter_sequence_stmt,
        "AlterSequenceOptions" => transform_alter_sequence_options,
        "SetSequenceOption" => transform_set_sequence_option,
        "AlterTableOptions" => transform_alter_table_options,
        "AddColumn" => transform_add_column,
        "DropColumn" => transform_drop_column,
        "SetPartitionedBy" => transform_set_partitioned_by,
        "ResetPartitionedBy" => transform_reset_partitioned_by,
        "AlterColumn" => transform_alter_column,
        "AlterColumnEntry" => transform_alter_column_entry,
        "DropDefault" => transform_drop_default,
        "ChangeNullability" => transform_change_nullability,
        "AlterType" => transform_alter_type,
        "AddOrDropDefault" => transform_add_or_drop_default,
        "AddDefault" => transform_add_default,
        "RenameColumn" => transform_rename_column,
        "RenameAlter" => transform_rename_alter,
        "AddConstraint" => transform_add_constraint,
        "SetSortedBy" => transform_set_sorted_by,
        "ResetSortedBy" => transform_reset_sorted_by,
        "SetOptions" => transform_set_options,
        "ResetOptions" => transform_reset_options,
    }
    AddColumnEntry [] {
        "AddColumnEntry" => transform_add_column_entry,
    }
    ParsedExpression [set_expression_location] {
        "UsingExpression" => transform_using_expression,
        "NumberLiteral" => transform_number_literal,
        "TimeType" => transform_time_type,
        "NumericType" => transform_numeric_type,
        "SimpleNumericType" => transform_simple_numeric_type,
        "DecimalNumericType" => transform_decimal_numeric_type,
        "FloatType" => transform_float_type,
        "DecimalType" => transform_decimal_type,
        "SimpleType" => transform_simple_type,
        "CharacterType" => transform_character_type,
        "MapType" => transform_map_type,
        "RowType" => transform_row_type,
        "GeometryType" => transform_geometry_type,
        "VariantType" => transform_variant_type,
        "UnionType" => transform_union_type,
        "BitType" => transform_bit_type,
        "IntervalType" => transform_interval_type,
        "IntervalInterval" => transform_interval_interval,
        "SetofType" => transform_setof_type,
        "CopyFileName" => transform_copy_file_name,
        "IndexElement" => transform_index_element,
        "RelOptionArgumentOpt" => transform_rel_option_argument_opt,
        "DefaultValue" => transform_default_value,
        "ColumnCollation" => transform_column_collation,
        "BaseExpression" => transform_base_expression,
        "Expression" => transform_expression,
        "LambdaArrowExpression" => transform_lambda_arrow_expression,
        "LogicalOrExpression" => transform_logical_or_expression,
        "LogicalAndExpression" => transform_logical_and_expression,
        "LogicalNotExpression" => transform_logical_not_expression,
        "IsExpression" => transform_is_expression,
        "IsTest" => transform_is_test,
        "IsLiteral" => transform_is_literal,
        "NotNull" => transform_not_null,
        "IsNull" => transform_is_null,
        "IsDistinctFromExpression" => transform_is_distinct_from_expression,
        "BetweenInLikeExpression" => transform_between_in_like_expression,
        "BetweenInLikeOp" => transform_between_in_like_op,
        "InClause" => transform_in_clause,
        "InExpression" => transform_in_expression,
        "InExpressionList" => transform_in_expression_list,
        "InSelectStatement" => transform_in_select_statement,
        "BetweenClause" => transform_between_clause,
        "LikeClause" => transform_like_clause,
        "EscapeClause" => transform_escape_clause,
        "ComparisonExpression" => transform_comparison_expression,
        "OtherOperatorExpression" => transform_other_operator_expression,
        "BitwiseExpression" => transform_bitwise_expression,
        "AdditiveExpression" => transform_additive_expression,
        "MultiplicativeExpression" => transform_multiplicative_expression,
        "ExponentiationExpression" => transform_exponentiation_expression,
        "PostfixOperator" => transform_postfix_operator,
        "CollateExpression" => transform_collate_expression,
        "AtTimeZoneExpression" => transform_at_time_zone_expression,
        "PrefixExpression" => transform_prefix_expression,
        "ColumnReference" => transform_column_reference,
        "Parameter" => transform_parameter,
        "QuestionMarkNumberedParameter" => transform_question_mark_numbered_parameter,
        "AnonymousParameter" => transform_anonymous_parameter,
        "ColLabelParameter" => transform_col_label_parameter,
        "NumberedParameter" => transform_numbered_parameter,
        "PositionalExpression" => transform_positional_expression,
        "LiteralExpression" => transform_literal_expression,
        "ParensExpression" => transform_parens_expression,
        "SingleExpression" => transform_single_expression,
        "ConstantLiteral" => transform_constant_literal,
        "ListExpression" => transform_list_expression,
        "StructExpression" => transform_struct_expression,
        "StructField" => transform_struct_field,
        "ArrayBoundedListExpression" => transform_array_bounded_list_expression,
        "ArrayParensSelect" => transform_array_parens_select,
        "FunctionExpression" => transform_function_expression,
        "FilterClause" => transform_filter_clause,
        "ParenthesisExpression" => transform_parenthesis_expression,
        "Indirection" => transform_indirection,
        "CastOperator" => transform_cast_operator,
        "DotOperator" => transform_dot_operator,
        "SliceExpression" => transform_slice_expression,
        "EndSliceBound" => transform_end_slice_bound,
        "StepSliceBound" => transform_step_slice_bound,
        "StarExpression" => transform_star_expression,
        "SpecialFunctionExpression" => transform_special_function_expression,
        "CoalesceExpression" => transform_coalesce_expression,
        "UnpackExpression" => transform_unpack_expression,
        "TryExpression" => transform_try_expression,
        "ColumnsExpression" => transform_columns_expression,
        "ExtractExpression" => transform_extract_expression,
        "ExtractArgument" => transform_extract_argument,
        "LambdaExpression" => transform_lambda_expression,
        "NullIfExpression" => transform_null_if_expression,
        "RowExpression" => transform_row_expression,
        "SubstringExpression" => transform_substring_expression,
        "TrimExpression" => transform_trim_expression,
        "TrimSource" => transform_trim_source,
        "PositionExpression" => transform_position_expression,
        "CastExpression" => transform_cast_expression,
        "CaseExpression" => transform_case_expression,
        "CaseElse" => transform_case_else,
        "TypeLiteral" => transform_type_literal,
        "DefaultExpression" => transform_default_expression,
        "IntervalLiteral" => transform_interval_literal,
        "IntervalParameter" => transform_interval_parameter,
        "SubqueryExpression" => transform_subquery_expression,
        "MapExpression" => transform_map_expression,
        "ListComprehensionExpression" => transform_list_comprehension_expression,
        "ListComprehensionFilter" => transform_list_comprehension_filter,
        "GroupingExpression" => transform_grouping_expression,
        "MethodExpression" => transform_method_expression,
        "AndExpression" => transform_and_expression,
        "FunctionArgument" => transform_function_argument,
        "WhereClause" => transform_where_clause,
        "AliasedExpression" => transform_aliased_expression,
        "ExpressionAsCollabel" => transform_expression_as_collabel,
        "ColIdExpression" => transform_col_id_expression,
        "ExpressionOptIdentifier" => transform_expression_opt_identifier,
        "PivotHeader" => transform_pivot_header,
        "GroupByExpression" => transform_group_by_expression,
        "EmptyGroupingItem" => transform_empty_grouping_item,
        "CubeOrRollupClause" => transform_cube_or_rollup_clause,
        "GroupingSetsClause" => transform_grouping_sets_clause,
        "HavingClause" => transform_having_clause,
        "QualifyClause" => transform_qualify_clause,
        "WindowDefinition" => transform_window_definition,
        "SampleValue" => transform_sample_value,
        "NumericModType" => transform_decimal_type,
        "DecType" => transform_decimal_type,
    }
    String [] {
        "DropOrSet" => transform_drop_or_set,
        "SequenceName" => transform_sequence_name,
        "AttachAlias" => transform_attach_alias,
        "DatabasePath" => transform_database_path,
        "StringLiteral" => transform_string_literal,
        "IdentifierColId" => transform_identifier_col_id,
        "IndexType" => transform_index_type,
        "RelOptionName" => transform_rel_option_name,
        "IndexName" => transform_index_name,
        "SecretStorageSpecifier" => transform_secret_storage_specifier,
        "SecretName" => transform_secret_name,
        "SeqMinOrMax" => transform_seq_min_or_max,
        "ColIdOrString" => transform_col_id_or_string,
        "ColLabelOrString" => transform_col_label_or_string,
        "ColId" => transform_col_id,
        "TypeFuncName" => transform_type_func_name,
        "Identifier" => transform_identifier,
        "KeyAction" => transform_key_action,
        "NoKeyAction" => transform_no_key_action,
        "RestrictKeyAction" => transform_restrict_key_action,
        "CascadeKeyAction" => transform_cascade_key_action,
        "SetNullKeyAction" => transform_set_null_key_action,
        "SetDefaultKeyAction" => transform_set_default_key_action,
        "CubeOrRollup" => transform_cube_or_rollup,
        "UpdateAction" => transform_update_action,
        "DeleteAction" => transform_delete_action,
        "DropSecretStorage" => transform_drop_secret_storage,
        "ExportSource" => transform_export_source,
        "LikeVariations" => transform_like_variations,
        "OtherOperator" => transform_other_operator,
        "QualifiedOperator" => transform_qualified_operator,
        "AnyOp" => transform_any_op,
        "StringOperator" => transform_string_operator,
        "JsonOperator" => transform_json_operator,
        "InetOperator" => transform_inet_operator,
        "ListOperator" => transform_list_operator,
        "BitOperator" => transform_bit_operator,
        "Term" => transform_term,
        "Factor" => transform_factor,
        "ExponentOperator" => transform_exponent_operator,
        "ReservedTableQualification" => transform_reserved_table_qualification,
        "PrefixOperator" => transform_prefix_operator,
        "TableQualification" => transform_table_qualification,
        "BaseWindowName" => transform_base_window_name,
        "Framing" => transform_framing,
        "TrimDirection" => transform_trim_direction,
        "InsertAlias" => transform_insert_alias,
        "VersionNumber" => transform_version_number,
        "SchemaQualification" => transform_schema_qualification,
        "CatalogQualification" => transform_catalog_qualification,
        "ReservedIdentifierOrStringLiteral" => transform_reserved_identifier_or_string_literal,
        "TableAliasColon" => transform_table_alias_colon,
        "AtUnit" => transform_at_unit,
        "UpdateAlias" => transform_update_alias,
        "UpdateSetColumnTarget" => transform_update_set_column_target,
        "VacuumOption" => transform_vacuum_option,
        "PragmaName" => transform_identifier_or_keyword,
        "TypeName" => transform_identifier_or_keyword,
        "ColLabel" => transform_identifier_or_keyword,
        "PlainIdentifier" => transform_identifier_or_keyword,
        "QuotedIdentifier" => transform_identifier_or_keyword,
        "ReservedKeyword" => transform_identifier_or_keyword,
        "UnreservedKeyword" => transform_identifier_or_keyword,
        "ColumnNameKeyword" => transform_identifier_or_keyword,
        "FuncNameKeyword" => transform_identifier_or_keyword,
        "TypeNameKeyword" => transform_identifier_or_keyword,
        "SettingName" => transform_identifier_or_keyword,
        "ReservedSchemaQualification" => transform_schema_qualification,
    }
    QualifiedName [] {
        "QualifiedSequenceName" => transform_qualified_sequence_name,
        "QualifiedTypeName" => transform_qualified_type_name,
        "IdentifierOrStringLiteral" => transform_identifier_or_string_literal,
        "QualifiedSchemaName" => transform_qualified_schema_name,
        "QualifiedIndexName" => transform_qualified_index_name,
        "FunctionIdentifier" => transform_function_identifier,
        "SchemaReservedFunctionName" => transform_schema_reserved_function_name,
        "CatalogReservedSchemaFunctionName" => transform_catalog_reserved_schema_function_name,
        "QualifiedName" => transform_qualified_name,
        "CatalogReservedSchemaIdentifierOrStringLiteral" => transform_catalog_reserved_schema_identifier_or_string_literal,
        "CatalogReservedSchemaIdentifier" => transform_catalog_reserved_schema_identifier,
        "SchemaReservedIdentifierOrStringLiteral" => transform_schema_reserved_identifier_or_string_literal,
        "TableNameIdentifierOrStringLiteral" => transform_table_name_identifier_or_string_literal,
        "QualifiedTableFunction" => transform_qualified_table_function,
        "UseTarget" => transform_use_target,
        "UseTargetCatalogSchema" => transform_use_target_catalog_schema,
    }
    Vec<GenericCopyOption> [] {
        "AttachOptions" => transform_attach_options,
        "GenericCopyOptionList" => transform_generic_copy_option_list,
        "CopyOptions" => transform_copy_options,
        "SpecializedOptionList" => transform_specialized_option_list,
    }
    GenericCopyOption [] {
        "GenericCopyOption" => transform_generic_copy_option,
        "SpecializedOption" => transform_specialized_option,
        "SingleOption" => transform_single_option,
        "EncodingOption" => transform_encoding_option,
        "ForceQuoteOption" => transform_force_quote_option,
        "QuoteAsOption" => transform_quote_as_option,
        "ForceNullOption" => transform_force_null_option,
        "PartitionByOption" => transform_partition_by_option,
        "NullAsOption" => transform_null_as_option,
        "DelimiterAsOption" => transform_delimiter_as_option,
        "EscapeAsOption" => transform_escape_as_option,
    }
    AnalyzeTarget [] {
        "AnalyzeTarget" => transform_analyze_target,
    }
    Vec<ParsedExpression> [] {
        "TableFunctionArguments" => transform_table_function_arguments,
        "TypeModifiers" => transform_type_modifiers,
        "PartitionOptions" => transform_partition_options,
        "SortedOptions" => transform_sorted_options,
        "BoundedListExpression" => transform_bounded_list_expression,
        "SliceBound" => transform_slice_bound,
        "WindowPartition" => transform_window_partition,
        "SubstringArguments" => transform_substring_arguments,
        "SubstringExpressionList" => transform_substring_expression_list,
        "SubstringParameters" => transform_substring_parameters,
        "MapStructExpression" => transform_map_struct_expression,
        "MapStructField" => transform_map_struct_field,
        "ReturningClause" => transform_returning_clause,
        "PivotUsing" => transform_pivot_using,
        "PragmaParameters" => transform_pragma_parameters,
        "DistinctOnTargets" => transform_distinct_on_targets,
        "TargetList" => transform_target_list,
        "ValuesExpressions" => transform_values_expressions,
        "WindowClause" => transform_window_clause,
        "UsingKey" => transform_using_key,
        "SetAssignment" => transform_set_assignment,
        "VariableList" => transform_variable_list,
    }
    CatalogType [] {
        "CommentOnType" => transform_comment_on_type,
        "TableOrView" => transform_table_or_view,
    }
    Value [] {
        "CommentValue" => transform_comment_value,
    }
    LogicalType [] {
        "Type" => transform_type,
        "EnumStringLiteralList" => transform_enum_string_literal_list,
    }
    i64 [] {
        "ArrayBounds" => transform_array_bounds,
        "ArrayKeyword" => transform_array_keyword,
        "SquareBracketsArray" => transform_square_brackets_array,
    }
    bool [] {
        "TimeZone" => transform_time_zone,
        "WithOrWithout" => transform_with_or_without,
        "FromOrTo" => transform_from_or_to,
        "WithData" => transform_with_data,
        "CommitAction" => transform_commit_action,
        "PreserveOrDelete" => transform_preserve_or_delete,
        "GeneratedColumnType" => transform_generated_column_type,
        "DropBehavior" => transform_drop_behavior,
        "AnyOrAll" => transform_any_or_all,
        "PrecedingOrFollowing" => transform_preceding_or_following,
        "CastOrTryCast" => transform_cast_or_try_cast,
        "IgnoreOrRespectNulls" => transform_ignore_or_respect_nulls,
        "IncludeOrExcludeNulls" => transform_include_or_exclude_nulls,
        "DistinctOrAll" => transform_distinct_or_all,
        "Materialized" => transform_materialized,
        "SampleUnit" => transform_sample_unit,
    }
    LogicalTypeId [] {
        "TimeOrTimestamp" => transform_time_or_timestamp,
    }
    Vec<(String, LogicalType)> [] {
        "ColIdTypeList" => transform_col_id_type_list,
    }
    (String, LogicalType) [] {
        "ColIdType" => transform_col_id_type,
    }
    DatePartSpecifier [] {
        "Interval" => transform_interval,
        "IntervalToInterval" => transform_interval_to_interval,
    }
    CopyDatabaseType [] {
        "CopyDatabaseFlag" => transform_copy_database_flag,
        "SchemaOrData" => transform_schema_or_data,
    }
    CreateInfo [] {
        "CreateIndexStmt" => transform_create_index_stmt,
        "CreateMacroStmt" => transform_create_macro_stmt,
        "CreateSchemaStmt" => transform_create_schema_stmt,
        "CreateSecretStmt" => transform_create_secret_stmt,
        "CreateSequenceStmt" => transform_create_sequence_stmt,
        "CreateStatementVariation" => transform_create_statement_variation,
        "CreateTableStmt" => transform_create_table_stmt,
        "CreateTypeStmt" => transform_create_type_stmt,
        "CreateViewStmt" => transform_create_view_stmt,
    }
    CaseInsensitiveMap<ParsedExpression> [] {
        "WithList" => transform_with_list,
        "RelOptionOrOids" => transform_rel_option_or_oids,
        "RelOptionList" => transform_rel_option_list,
        "Oids" => transform_oids,
        "ReplaceList" => transform_replace_list,
        "ReplaceEntries" => transform_replace_entries,
        "ReplaceEntrySingle" => transform_replace_entry_single,
        "ReplaceEntryList" => transform_replace_entry_list,
    }
    (String, ParsedExpression) [] {
        "RelOption" => transform_rel_option,
        "ReplaceEntry" => transform_replace_entry,
        "UpdateSetElement" => transform_update_set_element,
    }
    MacroFunction [] {
        "MacroDefinition" => transform_macro_definition,
        "TableMacroDefinition" => transform_table_macro_definition,
        "ScalarMacroDefinition" => transform_scalar_macro_definition,
    }
    Vec<MacroParameter> [] {
        "MacroParameters" => transform_macro_parameters,
    }
    MacroParameter [] {
        "MacroParameter" => transform_macro_parameter,
        "SimpleParameter" => transform_simple_parameter,
        "NamedParameter" => transform_named_parameter,
    }
    (String, SequenceOption) [] {
        "SequenceOption" => transform_sequence_option,
        "SeqSetCycle" => transform_seq_set_cycle,
        "SeqSetIncrement" => transform_seq_set_increment,
        "SeqSetMinMax" => transform_seq_set_min_max,
        "SeqNoMinMax" => transform_seq_no_min_max,
        "SeqStartWith" => transform_seq_start_with,
        "SeqOwnedBy" => transform_seq_owned_by,
    }
    SecretPersistType [] {
        "Temporary" => transform_temporary,
    }
    CreateTableAs [] {
        "CreateTableAs" => transform_create_table_as,
    }
    ColumnList [] {
        "IdentifierList" => transform_identifier_list,
    }
    ColumnElements [] {
        "CreateColumnList" => transform_create_column_list,
        "CreateTableColumnList" => transform_create_table_column_list,
    }
    PartitionSortedOptions [] {
        "PartitionSortedOptions" => transform_partition_sorted_options,
        "PartitionOptSortedOptions" => transform_partition_opt_sorted_options,
        "SortedOptPartitionOptions" => transform_sorted_opt_partition_options,
    }
    Vec<String> [] {
        "ColumnIdList" => transform_column_id_list,
        "DottedIdentifier" => transform_dotted_identifier,
        "InsertColumnList" => transform_insert_column_list,
        "ColumnList" => transform_column_list,
        "UnpivotHeader" => transform_unpivot_header,
        "UnpivotHeaderSingle" => transform_unpivot_header_single,
        "UnpivotHeaderList" => transform_unpivot_header_list,
        "ColumnAliases" => transform_column_aliases,
        "PivotGroupByList" => transform_pivot_group_by_list,
        "NameList" => transform_name_list,
    }
    ConstraintColumnDefinition [] {
        "ColumnDefinition" => transform_column_definition,
    }
    Constraint [] {
        "TopLevelConstraint" => transform_top_level_constraint,
        "TopLevelConstraintList" => transform_top_level_constraint_list,
        "TopPrimaryKeyConstraint" => transform_top_primary_key_constraint,
        "TopUniqueConstraint" => transform_top_unique_constraint,
        "CheckConstraint" => transform_check_constraint,
        "TopForeignKeyConstraint" => transform_top_foreign_key_constraint,
    }
    ForeignKeyConstraint [] {
        "ForeignKeyConstraint" => transform_foreign_key_constraint,
    }
    GeneratedColumnDefinition [] {
        "GeneratedColumn" => transform_generated_column,
    }
    CompressionType [] {
        "ColumnCompression" => transform_column_compression,
    }
    (bool, ConstraintType) [] {
        "PrimaryKeyConstraint" => transform_primary_key_constraint,
        "UniqueConstraint" => transform_unique_constraint,
        "NotNullConstraint" => transform_not_null_constraint,
    }
    KeyActions [] {
        "KeyActions" => transform_key_actions,
    }
    CreateTypeInfo [] {
        "CreateType" => transform_create_type,
    }
    SelectStatement [] {
        "EnumSelectType" => transform_enum_select_type,
        "DescribeStatement" => transform_describe_statement,
        "PivotStatement" => transform_pivot_statement,
        "UnpivotStatement" => transform_unpivot_statement,
        "SelectStatementInternal" => transform_select_statement_internal,
        "SelectSetOpChain" => transform_select_set_op_chain,
        "IntersectChain" => transform_intersect_chain,
        "SelectAtom" => transform_select_atom,
        "SelectParens" => transform_select_parens,
        "SelectStatementType" => transform_select_statement_type,
        "OptionalParensSimpleSelect" => transform_optional_parens_simple_select,
        "SimpleSelectParens" => transform_simple_select_parens,
        "SimpleSelect" => transform_simple_select,
        "ValuesClause" => transform_values_clause,
        "TableStatement" => transform_table_statement,
    }
    BaseTable [] {
        "TargetOptAlias" => transform_target_opt_alias,
        "InsertTarget" => transform_insert_target,
        "BaseTableName" => transform_base_table_name,
        "SchemaReservedTable" => transform_schema_reserved_table,
        "CatalogReservedSchemaTable" => transform_catalog_reserved_schema_table,
    }
    Vec<TableRef> [] {
        "DeleteUsingClause" => transform_delete_using_clause,
    }
    QueryNode [] {
        "ShowSelect" => transform_show_select,
        "ShowTables" => transform_show_tables,
        "ShowAllTables" => transform_show_all_tables,
        "ShowQualifiedName" => transform_show_qualified_name,
        "SetopClause" => transform_setop_clause,
        "SetIntersectClause" => transform_set_intersect_clause,
        "SelectFrom" => transform_select_from,
        "SelectFromClause" => transform_select_from_clause,
        "FromSelectClause" => transform_from_select_clause,
        "SelectClause" => transform_select_clause,
    }
    ShowType [] {
        "ShowOrDescribeOrSummarize" => transform_show_or_describe_or_summarize,
        "ShowOrDescribe" => transform_show_or_describe,
        "Summarize" => transform_summarize,
    }
    DropInfo [] {
        "DropEntries" => transform_drop_entries,
        "DropTable" => transform_drop_table,
        "DropTableFunction" => transform_drop_table_function,
        "DropFunction" => transform_drop_function,
        "DropSchema" => transform_drop_schema,
        "DropIndex" => transform_drop_index,
        "DropSequence" => transform_drop_sequence,
        "DropCollation" => transform_drop_collation,
        "DropType" => transform_drop_type,
        "DropSecret" => transform_drop_secret,
    }
    ExpressionType [] {
        "ComparisonOperator" => transform_comparison_operator,
        "LambdaOperator" => transform_lambda_operator,
        "Operator" => transform_operator,
        "ConjunctionOperator" => transform_conjunction_operator,
        "IsOperator" => transform_is_operator,
        "InOperator" => transform_in_operator,
        "BetweenOperator" => transform_between_operator,
        "IsDistinctFromOp" => transform_is_distinct_from_op,
    }
    (String, bool) [] {
        "AnyAllOperator" => transform_any_all_operator,
    }
    ColumnRefExpression [] {
        "NestedColumnName" => transform_nested_column_name,
        "CatalogReservedSchemaTableColumnName" => transform_catalog_reserved_schema_table_column_name,
        "SchemaReservedTableColumnName" => transform_schema_reserved_table_column_name,
        "TableReservedColumnName" => transform_table_reserved_column_name,
    }
    Vec<OrderByNode> [] {
        "WithinGroupClause" => transform_within_group_clause,
        "OrderByClause" => transform_order_by_clause,
        "OrderByExpressions" => transform_order_by_expressions,
        "OrderByExpressionList" => transform_order_by_expression_list,
        "OrderByAll" => transform_order_by_all,
    }
    Vec<QualifiedColumnName> [] {
        "ExcludeList" => transform_exclude_list,
        "ExcludeNameList" => transform_exclude_name_list,
        "ExcludeNameSingle" => transform_exclude_name_single,
    }
    QualifiedColumnName [] {
        "ExcludeName" => transform_exclude_name,
    }
    WindowExpr [] {
        "OverClause" => transform_over_clause,
        "WindowFrame" => transform_window_frame,
        "ParensIdentifier" => transform_parens_identifier,
        "WindowFrameDefinition" => transform_window_frame_definition,
        "WindowFrameContentsParens" => transform_window_frame_contents_parens,
        "WindowFrameNameContentsParens" => transform_window_frame_name_contents_parens,
        "WindowFrameContents" => transform_window_frame_contents,
    }
    Vec<WindowBoundaryExpression> [] {
        "FrameExtent" => transform_frame_extent,
        "BetweenFrameExtent" => transform_between_frame_extent,
        "SingleFrameExtent" => transform_single_frame_extent,
    }
    WindowBoundaryExpression [] {
        "FrameBound" => transform_frame_bound,
        "FrameUnbounded" => transform_frame_unbounded,
        "FrameCurrentRow" => transform_frame_current_row,
        "FrameExpression" => transform_frame_expression,
    }
    WindowFrame [] {
        "FrameClause" => transform_frame_clause,
    }
    WindowExcludeMode [] {
        "WindowExcludeClause" => transform_window_exclude_clause,
        "WindowExcludeElement" => transform_window_exclude_element,
    }
    CaseCheck [] {
        "CaseWhenThen" => transform_case_when_then,
    }
    OrderedMap<QualifiedColumnName, String> [] {
        "RenameList" => transform_rename_list,
        "RenameEntryList" => transform_rename_entry_list,
        "SingleRenameEntry" => transform_single_rename_entry,
    }
    (QualifiedColumnName, String) [] {
        "RenameEntry" => transform_rename_entry,
    }
    OnConflictAction [] {
        "OrAction" => transform_or_action,
    }
    OnConflictInfo [] {
        "OnConflictClause" => transform_on_conflict_clause,
        "OnConflictAction" => transform_on_conflict_action,
        "OnConflictUpdate" => transform_on_conflict_update,
        "OnConflictNothing" => transform_on_conflict_nothing,
    }
    OnConflictExpressionTarget [] {
        "OnConflictTarget" => transform_on_conflict_target,
        "OnConflictExpressionTarget" => transform_on_conflict_expression_target,
        "OnConflictIndexTarget" => transform_on_conflict_index_target,
    }
    InsertValues [] {
        "InsertValues" => transform_insert_values,
    }
    InsertColumnOrder [] {
        "ByNameOrPosition" => transform_by_name_or_position,
    }
    ExtensionRepositoryInfo [] {
        "FromSource" => transform_from_source,
    }
    TableRef [set_table_ref_location] {
        "MergeIntoUsingClause" => transform_merge_into_using_clause,
        "FromClause" => transform_from_clause,
        "TableRef" => transform_table_ref,
        "JoinOrPivot" => transform_join_or_pivot,
        "JoinClause" => transform_join_clause,
        "RegularJoinClause" => transform_regular_join_clause,
        "JoinWithoutOnClause" => transform_join_without_on_clause,
        "TableUnpivotClause" => transform_table_unpivot_clause,
        "TablePivotClause" => transform_table_pivot_clause,
        "InnerTableRef" => transform_inner_table_ref,
        "TableFunction" => transform_table_function,
        "TableFunctionLateralOpt" => transform_table_function_lateral_opt,
        "TableFunctionAliasColon" => transform_table_function_alias_colon,
        "TableSubquery" => transform_table_subquery,
        "SubqueryReference" => transform_subquery_reference,
        "BaseTableRef" => transform_base_table_ref,
        "ValuesRef" => transform_values_ref,
        "ParensTableRef" => transform_parens_table_ref,
        "CTEBody" => transform_cte_body,
        "UpdateTarget" => transform_update_target,
        "BaseTableSet" => transform_base_table_set,
        "BaseTableAliasSet" => transform_base_table_alias_set,
    }
    (MergeActionCondition, MergeIntoAction) [] {
        "MergeMatch" => transform_merge_match,
        "MatchedClause" => transform_matched_clause,
        "NotMatchedClause" => transform_not_matched_clause,
    }
    MergeIntoAction [] {
        "MatchedClauseAction" => transform_matched_clause_action,
        "UpdateMatchClause" => transform_update_match_clause,
        "UpdateMatchInfo" => transform_update_match_info,
        "DeleteMatchClause" => transform_delete_match_clause,
        "InsertMatchClause" => transform_insert_match_clause,
        "DoNothingMatchClause" => transform_do_nothing_match_clause,
        "ErrorMatchClause" => transform_error_match_clause,
        "InsertMatchInfo" => transform_insert_match_info,
        "InsertDefaultValues" => transform_insert_default_values,
        "InsertByNameOrPosition" => transform_insert_by_name_or_position,
        "InsertValuesList" => transform_insert_values_list,
    }
    UpdateSetInfo [] {
        "UpdateMatchSetClause" => transform_update_match_set_clause,
        "UpdateSetClause" => transform_update_set_clause,
        "UpdateSetTuple" => transform_update_set_tuple,
        "UpdateSetElementList" => transform_update_set_element_list,
    }
    MergeActionCondition [] {
        "BySourceOrTarget" => transform_by_source_or_target,
    }
    Vec<PivotColumn> [] {
        "PivotOn" => transform_pivot_on,
        "PivotColumnList" => transform_pivot_column_list,
    }
    PivotColumn [] {
        "PivotColumnEntry" => transform_pivot_column_entry,
        "PivotColumnSubquery" => transform_pivot_column_subquery,
        "PivotColumnEntryInternal" => transform_pivot_column_entry_internal,
        "UnpivotValueList" => transform_unpivot_value_list,
        "PivotValueList" => transform_pivot_value_list,
    }
    UnpivotNameValues [] {
        "IntoNameValues" => transform_into_name_values,
    }
    SetOperationType [] {
        "SetopType" => transform_setop_type,
    }
    DistinctClause [] {
        "DistinctClause" => transform_distinct_clause,
        "DistinctOn" => transform_distinct_on,
        "DistinctAll" => transform_distinct_all,
    }
    TableAlias [] {
        "TableAlias" => transform_table_alias,
        "TableAliasAs" => transform_table_alias_as,
        "TableAliasWithoutAs" => transform_table_alias_without_as,
    }
    OrderByNode [] {
        "OrderByExpression" => transform_order_by_expression,
    }
    OrderType [] {
        "DescOrAsc" => transform_desc_or_asc,
    }
    OrderByNullType [] {
        "NullsFirstOrLast" => transform_nulls_first_or_last,
    }
    JoinType [] {
        "JoinType" => transform_join_type,
    }
    JoinQualifier [] {
        "JoinQualifier" => transform_join_qualifier,
        "OnClause" => transform_on_clause,
        "UsingClause" => transform_using_clause,
    }
    JoinPrefix [] {
        "JoinPrefix" => transform_join_prefix,
        "CrossJoinPrefix" => transform_cross_join_prefix,
        "NaturalJoinPrefix" => transform_natural_join_prefix,
        "PositionalJoinPrefix" => transform_positional_join_prefix,
    }
    Vec<PivotColumnEntry> [] {
        "UnpivotTargetList" => transform_unpivot_target_list,
        "PivotTargetList" => transform_pivot_target_list,
    }
    AtClause [] {
        "AtClause" => transform_at_clause,
        "AtSpecifier" => transform_at_specifier,
    }
    Vec<ResultModifier> [] {
        "ResultModifiers" => transform_result_modifiers,
    }
    ResultModifier [] {
        "LimitOffset" => transform_limit_offset,
        "LimitOffsetClause" => transform_limit_offset_clause,
        "OffsetLimitClause" => transform_offset_limit_clause,
    }
    LimitPercentResult [] {
        "LimitClause" => transform_limit_clause,
        "LimitValue" => transform_limit_value,
        "LimitAll" => transform_limit_all,
        "LimitLiteralPercent" => transform_limit_literal_percent,
        "LimitExpression" => transform_limit_expression,
        "OffsetClause" => transform_offset_clause,
        "OffsetValue" => transform_offset_value,
    }
    GroupByNode [] {
        "GroupByClause" => transform_group_by_clause,
        "GroupByExpressions" => transform_group_by_expressions,
        "GroupByAll" => transform_group_by_all,
        "GroupByList" => transform_group_by_list,
    }
    CommonTableExpressionMap [] {
        "WithClause" => transform_with_clause,
    }
    (String, CommonTableExpressionInfo) [] {
        "WithStatement" => transform_with_statement,
    }
    SampleOptions [] {
        "SampleClause" => transform_sample_clause,
        "SampleEntry" => transform_sample_entry,
        "SampleEntryFunction" => transform_sample_entry_function,
        "SampleEntryCount" => transform_sample_entry_count,
        "SampleCount" => transform_sample_count,
    }
    (SampleMethod, Option<u64>) [] {
        "SampleProperties" => transform_sample_properties,
    }
    Option<u64> [] {
        "SampleSeed" => transform_sample_seed,
        "RepeatableSample" => transform_repeatable_sample,
    }
    SampleMethod [] {
        "SampleFunction" => transform_sample_function,
    }
    SettingInfo [] {
        "SetSetting" => transform_set_setting,
        "SetVariable" => transform_set_variable,
    }
    SetStatement [] {
        "SetTimeZone" => transform_set_time_zone,
        "StandardAssignment" => transform_standard_assignment,
    }
    TransactionInfo [] {
        "BeginTransaction" => transform_begin_transaction,
        "CommitTransaction" => transform_commit_transaction,
        "RollbackTransaction" => transform_rollback_transaction,
    }
    TransactionModifierType [] {
        "ReadOrWrite" => transform_read_or_write,
        "ReadOnlyOrReadWrite" => transform_read_only_or_read_write,
    }
    VacuumOptions [] {
        "VacuumOptions" => transform_vacuum_options,
        "VacuumLegacyOptions" => transform_vacuum_legacy_options,
        "VacuumParensOptions" => transform_vacuum_parens_options,
    }
}

enum_rules! {
    SetScope {
        "LocalScope" => SetScope::Local,
        "GlobalScope" => SetScope::Global,
        "SessionScope" => SetScope::Session,
        "VariableScope" => SetScope::Variable,
    }
    Value {
        "FalseLiteral" => Value::boolean(false),
        "TrueLiteral" => Value::boolean(true),
        "NullLiteral" => Value::default(),
        "UnknownLiteral" => Value::default(),
    }
    TransactionModifierType {
        "ReadOnly" => TransactionModifierType::TransactionReadOnly,
        "ReadWrite" => TransactionModifierType::TransactionReadWrite,
    }
    CopyDatabaseType {
        "CopySchema" => CopyDatabaseType::CopySchema,
        "CopyData" => CopyDatabaseType::CopyData,
    }
    String {
        "IntType" => LogicalTypeId::Integer.name().to_owned(),
        "IntegerType" => LogicalTypeId::Integer.name().to_owned(),
        "SmallintType" => LogicalTypeId::SmallInt.name().to_owned(),
        "BigintType" => LogicalTypeId::BigInt.name().to_owned(),
        "RealType" => LogicalTypeId::Float.name().to_owned(),
        "DoubleType" => LogicalTypeId::Double.name().to_owned(),
        "BooleanType" => LogicalTypeId::Boolean.name().to_owned(),
        "MinValue" => "minvalue".to_owned(),
        "MaxValue" => "maxvalue".to_owned(),
        "MinusPrefixOperator" => "-".to_owned(),
        "PlusPrefixOperator" => "+".to_owned(),
        "TildePrefixOperator" => "~".to_owned(),
        "TrimBoth" => "trim".to_owned(),
        "TrimLeading" => "ltrim".to_owned(),
        "TrimTrailing" => "rtrim".to_owned(),
        "LikeToken" => "~~".to_owned(),
        "ILikeToken" => "~~*".to_owned(),
        "GlobToken" => "~~~".to_owned(),
        "SimilarToToken" => "regexp_full_match".to_owned(),
        "NotILikeOp" => "!~~*".to_owned(),
        "NotLikeOp" => "!~~".to_owned(),
        "NotSimilarToOp" => "!~".to_owned(),
        "OptAnalyze" => "analyze".to_owned(),
        "OptFreeze" => "freeze".to_owned(),
        "OptFull" => "full".to_owned(),
        "OptVerbose" => "verbose".to_owned(),
    }
    DatePartSpecifier {
        "YearKeyword" => DatePartSpecifier::Year,
        "MonthKeyword" => DatePartSpecifier::Month,
        "DayKeyword" => DatePartSpecifier::Day,
        "HourKeyword" => DatePartSpecifier::Hour,
        "MinuteKeyword" => DatePartSpecifier::Minute,
        "SecondKeyword" => DatePartSpecifier::Second,
        "MillisecondKeyword" => DatePartSpecifier::Milliseconds,
        "MicrosecondKeyword" => DatePartSpecifier::Microseconds,
        "WeekKeyword" => DatePartSpecifier::Week,
        "QuarterKeyword" => DatePartSpecifier::Quarter,
        "DecadeKeyword" => DatePartSpecifier::Decade,
        "CenturyKeyword" => DatePartSpecifier::Century,
        "MillenniumKeyword" => DatePartSpecifier::Millennium,
    }
    LogicalTypeId {
        "TimeTypeId" => LogicalTypeId::Time,
        "TimestampTypeId" => LogicalTypeId::Timestamp,
    }
    bool {
        "WithRule" => true,
        "WithoutRule" => false,
        "SamplePercentage" => true,
        "SampleRows" => false,
        "SubqueryAny" => true,
        "SubqueryAll" => false,
        "IgnoreNulls" => true,
        "RespectNulls" => false,
        "IncludeNulls" => true,
        "ExcludeNulls" => false,
    }
    SecretPersistType {
        "TempPersistent" => SecretPersistType::Temporary,
        "TemporaryPersistent" => SecretPersistType::Temporary,
        "Persistent" => SecretPersistType::Persistent,
    }
    CatalogType {
        "CommentTable" => CatalogType::TableEntry,
        "CommentSequence" => CatalogType::SequenceEntry,
        "CommentFunction" => CatalogType::MacroEntry,
        "CommentMacroTable" => CatalogType::TableMacroEntry,
        "CommentMacro" => CatalogType::MacroEntry,
        "CommentView" => CatalogType::ViewEntry,
        "MaterializedViewEntry" => CatalogType::ViewEntry,
        "CommentDatabase" => CatalogType::DatabaseEntry,
        "CommentIndex" => CatalogType::IndexEntry,
        "CommentSchema" => CatalogType::SchemaEntry,
        "CommentType" => CatalogType::TypeEntry,
        "CommentColumn" => CatalogType::Invalid,
    }
    ShowType {
        "SummarizeRule" => ShowType::Summary,
        "ShowRule" => ShowType::Describe,
        "DescribeRule" => ShowType::Describe,
    }
    InsertColumnOrder {
        "InsertByName" => InsertColumnOrder::InsertByName,
        "InsertByPosition" => InsertColumnOrder::InsertByPosition,
    }
    OrderType {
        "DescendingOrder" => OrderType::Descending,
        "AscendingOrder" => OrderType::Ascending,
    }
    OrderByNullType {
        "NullsFirst" => OrderByNullType::NullsFirst,
        "NullsLast" => OrderByNullType::NullsLast,
    }
    JoinType {
        "FullJoin" => JoinType::Outer,
        "LeftJoin" => JoinType::Left,
        "RightJoin" => JoinType::Right,
        "SemiJoin" => JoinType::Semi,
        "AntiJoin" => JoinType::Anti,
        "InnerJoin" => JoinType::Inner,
    }
    ExpressionType {
        "OperatorEqual" => ExpressionType::CompareEqual,
        "OperatorNotEqual" => ExpressionType::CompareNotequal,
        "OperatorLessThan" => ExpressionType::CompareLessthan,
        "OperatorGreaterThan" => ExpressionType::CompareGreaterthan,
        "OperatorLessThanEquals" => ExpressionType::CompareLessthanorequalto,
        "OperatorGreaterThanEquals" => ExpressionType::CompareGreaterthanorequalto,
    }
    SetOperationType {
        "SetopUnion" => SetOperationType::Union,
        "SetopExcept" => SetOperationType::Except,
    }
    MergeActionCondition {
        "BySource" => MergeActionCondition::WhenNotMatchedBySource,
        "ByTarget" => MergeActionCondition::WhenNotMatchedByTarget,
    }
    WindowExcludeMode {
        "ExcludeCurrentRow" => WindowExcludeMode::CurrentRow,
        "ExcludeGroup" => WindowExcludeMode::Group,
        "ExcludeTies" => WindowExcludeMode::Ties,
        "ExcludeNoOthers" => WindowExcludeMode::NoOther,
    }
    GenericCopyOption {
        "BinaryOption" => GenericCopyOption::new("format", Value::varchar("binary")),
        "FreezeOption" => GenericCopyOption::new("freeze", Value::default()),
        "OidsOption" => GenericCopyOption::new("oids", Value::default()),
        "CsvOption" => GenericCopyOption::new("format", Value::varchar("csv")),
        "HeaderOption" => GenericCopyOption::new("header", Value::boolean(true)),
    }
}
