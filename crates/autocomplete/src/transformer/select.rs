//! The transform functions of DuckDB's `transform_select.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformSelectStatement`.
    pub(super) fn transform_select_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        Err(not_ported("TransformSelectStatement", parse_result))
    }

    /// `TransformSelectStatementInternal`.
    pub(super) fn transform_select_statement_internal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        Err(not_ported("TransformSelectStatementInternal", parse_result))
    }

    /// `TransformSelectSetOpChain`.
    pub(super) fn transform_select_set_op_chain(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        Err(not_ported("TransformSelectSetOpChain", parse_result))
    }

    /// `TransformIntersectChain`.
    pub(super) fn transform_intersect_chain(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        Err(not_ported("TransformIntersectChain", parse_result))
    }

    /// `TransformSetIntersectClause`.
    pub(super) fn transform_set_intersect_clause(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        Err(not_ported("TransformSetIntersectClause", parse_result))
    }

    /// `TransformSelectAtom`.
    pub(super) fn transform_select_atom(&mut self, parse_result: &ParseResult) -> Result<SelectStatement> {
        Err(not_ported("TransformSelectAtom", parse_result))
    }

    /// `TransformSetopClause`.
    pub(super) fn transform_setop_clause(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        Err(not_ported("TransformSetopClause", parse_result))
    }

    /// `TransformDistinctOrAll`.
    pub(super) fn transform_distinct_or_all(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformDistinctOrAll", parse_result))
    }

    /// `TransformSetopType`.
    pub(super) fn transform_setop_type(&mut self, parse_result: &ParseResult) -> Result<SetOperationType> {
        Err(not_ported("TransformSetopType", parse_result))
    }

    /// `TransformSelectParens`.
    pub(super) fn transform_select_parens(&mut self, parse_result: &ParseResult) -> Result<SelectStatement> {
        Err(not_ported("TransformSelectParens", parse_result))
    }

    /// `TransformSelectStatementType`.
    pub(super) fn transform_select_statement_type(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        Err(not_ported("TransformSelectStatementType", parse_result))
    }

    /// `TransformOptionalParensSimpleSelect`.
    pub(super) fn transform_optional_parens_simple_select(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        Err(not_ported("TransformOptionalParensSimpleSelect", parse_result))
    }

    /// `TransformSimpleSelectParens`.
    pub(super) fn transform_simple_select_parens(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        Err(not_ported("TransformSimpleSelectParens", parse_result))
    }

    /// `TransformSimpleSelect`.
    pub(super) fn transform_simple_select(&mut self, parse_result: &ParseResult) -> Result<SelectStatement> {
        Err(not_ported("TransformSimpleSelect", parse_result))
    }

    /// `TransformSelectFrom`.
    pub(super) fn transform_select_from(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        Err(not_ported("TransformSelectFrom", parse_result))
    }

    /// `TransformSelectFromClause`.
    pub(super) fn transform_select_from_clause(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        Err(not_ported("TransformSelectFromClause", parse_result))
    }

    /// `TransformFromSelectClause`.
    pub(super) fn transform_from_select_clause(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        Err(not_ported("TransformFromSelectClause", parse_result))
    }

    /// `TransformFromClause`.
    pub(super) fn transform_from_clause(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformFromClause", parse_result))
    }

    /// `TransformSelectClause`.
    pub(super) fn transform_select_clause(&mut self, parse_result: &ParseResult) -> Result<QueryNode> {
        Err(not_ported("TransformSelectClause", parse_result))
    }

    /// `TransformDistinctClause`.
    pub(super) fn transform_distinct_clause(&mut self, parse_result: &ParseResult) -> Result<DistinctClause> {
        Err(not_ported("TransformDistinctClause", parse_result))
    }

    /// `TransformDistinctOn`.
    pub(super) fn transform_distinct_on(&mut self, parse_result: &ParseResult) -> Result<DistinctClause> {
        Err(not_ported("TransformDistinctOn", parse_result))
    }

    /// `TransformDistinctAll`.
    pub(super) fn transform_distinct_all(&mut self, parse_result: &ParseResult) -> Result<DistinctClause> {
        Err(not_ported("TransformDistinctAll", parse_result))
    }

    /// `TransformDistinctOnTargets`.
    pub(super) fn transform_distinct_on_targets(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformDistinctOnTargets", parse_result))
    }

    /// `TransformFunctionArgument`.
    pub(super) fn transform_function_argument(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformFunctionArgument", parse_result))
    }

    /// `TransformNamedParameter`.
    pub(super) fn transform_named_parameter(&mut self, parse_result: &ParseResult) -> Result<MacroParameter> {
        Err(not_ported("TransformNamedParameter", parse_result))
    }

    /// `TransformTableFunctionArguments`.
    pub(super) fn transform_table_function_arguments(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformTableFunctionArguments", parse_result))
    }

    /// `TransformBaseTableName`.
    pub(super) fn transform_base_table_name(&mut self, parse_result: &ParseResult) -> Result<BaseTable> {
        Err(not_ported("TransformBaseTableName", parse_result))
    }

    /// `TransformSchemaReservedTable`.
    pub(super) fn transform_schema_reserved_table(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<BaseTable> {
        Err(not_ported("TransformSchemaReservedTable", parse_result))
    }

    /// `TransformCatalogReservedSchemaTable`.
    pub(super) fn transform_catalog_reserved_schema_table(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<BaseTable> {
        Err(not_ported("TransformCatalogReservedSchemaTable", parse_result))
    }

    /// `TransformSchemaQualification`.
    pub(super) fn transform_schema_qualification(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformSchemaQualification", parse_result))
    }

    /// `TransformCatalogQualification`.
    pub(super) fn transform_catalog_qualification(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformCatalogQualification", parse_result))
    }

    /// `TransformQualifiedName`.
    pub(super) fn transform_qualified_name(&mut self, parse_result: &ParseResult) -> Result<QualifiedName> {
        Err(not_ported("TransformQualifiedName", parse_result))
    }

    /// `TransformCatalogReservedSchemaIdentifierOrStringLiteral`.
    pub(super) fn transform_catalog_reserved_schema_identifier_or_string_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Err(not_ported(
            "TransformCatalogReservedSchemaIdentifierOrStringLiteral",
            parse_result,
        ))
    }

    /// `TransformCatalogReservedSchemaIdentifier`.
    pub(super) fn transform_catalog_reserved_schema_identifier(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Err(not_ported(
            "TransformCatalogReservedSchemaIdentifier",
            parse_result,
        ))
    }

    /// `TransformSchemaReservedIdentifierOrStringLiteral`.
    pub(super) fn transform_schema_reserved_identifier_or_string_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Err(not_ported(
            "TransformSchemaReservedIdentifierOrStringLiteral",
            parse_result,
        ))
    }

    /// `TransformReservedIdentifierOrStringLiteral`.
    pub(super) fn transform_reserved_identifier_or_string_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<String> {
        Err(not_ported(
            "TransformReservedIdentifierOrStringLiteral",
            parse_result,
        ))
    }

    /// `TransformTableNameIdentifierOrStringLiteral`.
    pub(super) fn transform_table_name_identifier_or_string_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Err(not_ported(
            "TransformTableNameIdentifierOrStringLiteral",
            parse_result,
        ))
    }

    /// `TransformWhereClause`.
    pub(super) fn transform_where_clause(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformWhereClause", parse_result))
    }

    /// `TransformTargetList`.
    pub(super) fn transform_target_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformTargetList", parse_result))
    }

    /// `TransformAliasedExpression`.
    pub(super) fn transform_aliased_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformAliasedExpression", parse_result))
    }

    /// `TransformExpressionAsCollabel`.
    pub(super) fn transform_expression_as_collabel(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformExpressionAsCollabel", parse_result))
    }

    /// `TransformColIdExpression`.
    pub(super) fn transform_col_id_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformColIdExpression", parse_result))
    }

    /// `TransformExpressionOptIdentifier`.
    pub(super) fn transform_expression_opt_identifier(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformExpressionOptIdentifier", parse_result))
    }

    /// `TransformTableAlias`.
    pub(super) fn transform_table_alias(&mut self, parse_result: &ParseResult) -> Result<TableAlias> {
        Err(not_ported("TransformTableAlias", parse_result))
    }

    /// `TransformTableAliasAs`.
    pub(super) fn transform_table_alias_as(&mut self, parse_result: &ParseResult) -> Result<TableAlias> {
        Err(not_ported("TransformTableAliasAs", parse_result))
    }

    /// `TransformTableAliasWithoutAs`.
    pub(super) fn transform_table_alias_without_as(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TableAlias> {
        Err(not_ported("TransformTableAliasWithoutAs", parse_result))
    }

    /// `TransformColumnAliases`.
    pub(super) fn transform_column_aliases(&mut self, parse_result: &ParseResult) -> Result<Vec<String>> {
        Err(not_ported("TransformColumnAliases", parse_result))
    }

    /// `TransformTableRef`.
    pub(super) fn transform_table_ref(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformTableRef", parse_result))
    }

    /// `TransformJoinOrPivot`.
    pub(super) fn transform_join_or_pivot(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformJoinOrPivot", parse_result))
    }

    /// `TransformTableUnpivotClause`.
    pub(super) fn transform_table_unpivot_clause(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformTableUnpivotClause", parse_result))
    }

    /// `TransformUnpivotValueList`.
    pub(super) fn transform_unpivot_value_list(&mut self, parse_result: &ParseResult) -> Result<PivotColumn> {
        Err(not_ported("TransformUnpivotValueList", parse_result))
    }

    /// `TransformUnpivotTargetList`.
    pub(super) fn transform_unpivot_target_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<PivotColumnEntry>> {
        Err(not_ported("TransformUnpivotTargetList", parse_result))
    }

    /// `TransformTablePivotClause`.
    pub(super) fn transform_table_pivot_clause(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformTablePivotClause", parse_result))
    }

    /// `TransformPivotGroupByList`.
    pub(super) fn transform_pivot_group_by_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<String>> {
        Err(not_ported("TransformPivotGroupByList", parse_result))
    }

    /// `TransformPivotValueList`.
    pub(super) fn transform_pivot_value_list(&mut self, parse_result: &ParseResult) -> Result<PivotColumn> {
        Err(not_ported("TransformPivotValueList", parse_result))
    }

    /// `TransformPivotHeader`.
    pub(super) fn transform_pivot_header(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformPivotHeader", parse_result))
    }

    /// `TransformPivotTargetList`.
    pub(super) fn transform_pivot_target_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<PivotColumnEntry>> {
        Err(not_ported("TransformPivotTargetList", parse_result))
    }

    /// `TransformJoinClause`.
    pub(super) fn transform_join_clause(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformJoinClause", parse_result))
    }

    /// `TransformRegularJoinClause`.
    pub(super) fn transform_regular_join_clause(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformRegularJoinClause", parse_result))
    }

    /// `TransformJoinType`.
    pub(super) fn transform_join_type(&mut self, parse_result: &ParseResult) -> Result<JoinType> {
        Err(not_ported("TransformJoinType", parse_result))
    }

    /// `TransformJoinQualifier`.
    pub(super) fn transform_join_qualifier(&mut self, parse_result: &ParseResult) -> Result<JoinQualifier> {
        Err(not_ported("TransformJoinQualifier", parse_result))
    }

    /// `TransformOnClause`.
    pub(super) fn transform_on_clause(&mut self, parse_result: &ParseResult) -> Result<JoinQualifier> {
        Err(not_ported("TransformOnClause", parse_result))
    }

    /// `TransformUsingClause`.
    pub(super) fn transform_using_clause(&mut self, parse_result: &ParseResult) -> Result<JoinQualifier> {
        Err(not_ported("TransformUsingClause", parse_result))
    }

    /// `TransformJoinWithoutOnClause`.
    pub(super) fn transform_join_without_on_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TableRef> {
        Err(not_ported("TransformJoinWithoutOnClause", parse_result))
    }

    /// `TransformJoinPrefix`.
    pub(super) fn transform_join_prefix(&mut self, parse_result: &ParseResult) -> Result<JoinPrefix> {
        Err(not_ported("TransformJoinPrefix", parse_result))
    }

    /// `TransformCrossJoinPrefix`.
    pub(super) fn transform_cross_join_prefix(&mut self, parse_result: &ParseResult) -> Result<JoinPrefix> {
        Err(not_ported("TransformCrossJoinPrefix", parse_result))
    }

    /// `TransformNaturalJoinPrefix`.
    pub(super) fn transform_natural_join_prefix(&mut self, parse_result: &ParseResult) -> Result<JoinPrefix> {
        Err(not_ported("TransformNaturalJoinPrefix", parse_result))
    }

    /// `TransformPositionalJoinPrefix`.
    pub(super) fn transform_positional_join_prefix(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<JoinPrefix> {
        Err(not_ported("TransformPositionalJoinPrefix", parse_result))
    }

    /// `TransformInnerTableRef`.
    pub(super) fn transform_inner_table_ref(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformInnerTableRef", parse_result))
    }

    /// `TransformTableFunction`.
    pub(super) fn transform_table_function(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformTableFunction", parse_result))
    }

    /// `TransformTableFunctionLateralOpt`.
    pub(super) fn transform_table_function_lateral_opt(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TableRef> {
        Err(not_ported("TransformTableFunctionLateralOpt", parse_result))
    }

    /// `TransformTableFunctionAliasColon`.
    pub(super) fn transform_table_function_alias_colon(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<TableRef> {
        Err(not_ported("TransformTableFunctionAliasColon", parse_result))
    }

    /// `TransformTableAliasColon`.
    pub(super) fn transform_table_alias_colon(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformTableAliasColon", parse_result))
    }

    /// `TransformQualifiedTableFunction`.
    pub(super) fn transform_qualified_table_function(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Err(not_ported("TransformQualifiedTableFunction", parse_result))
    }

    /// `TransformTableSubquery`.
    pub(super) fn transform_table_subquery(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformTableSubquery", parse_result))
    }

    /// `TransformSubqueryReference`.
    pub(super) fn transform_subquery_reference(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformSubqueryReference", parse_result))
    }

    /// `TransformBaseTableRef`.
    pub(super) fn transform_base_table_ref(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformBaseTableRef", parse_result))
    }

    /// `TransformParensTableRef`.
    pub(super) fn transform_parens_table_ref(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformParensTableRef", parse_result))
    }

    /// `TransformAtClause`.
    pub(super) fn transform_at_clause(&mut self, parse_result: &ParseResult) -> Result<AtClause> {
        Err(not_ported("TransformAtClause", parse_result))
    }

    /// `TransformAtSpecifier`.
    pub(super) fn transform_at_specifier(&mut self, parse_result: &ParseResult) -> Result<AtClause> {
        Err(not_ported("TransformAtSpecifier", parse_result))
    }

    /// `TransformAtUnit`.
    pub(super) fn transform_at_unit(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformAtUnit", parse_result))
    }

    /// `TransformValuesRef`.
    pub(super) fn transform_values_ref(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformValuesRef", parse_result))
    }

    /// `TransformValuesClause`.
    pub(super) fn transform_values_clause(&mut self, parse_result: &ParseResult) -> Result<SelectStatement> {
        Err(not_ported("TransformValuesClause", parse_result))
    }

    /// `TransformValuesExpressions`.
    pub(super) fn transform_values_expressions(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformValuesExpressions", parse_result))
    }

    /// `TransformTableStatement`.
    pub(super) fn transform_table_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SelectStatement> {
        Err(not_ported("TransformTableStatement", parse_result))
    }

    /// `TransformOrderByClause`.
    pub(super) fn transform_order_by_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<OrderByNode>> {
        Err(not_ported("TransformOrderByClause", parse_result))
    }

    /// `TransformOrderByExpressions`.
    pub(super) fn transform_order_by_expressions(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<OrderByNode>> {
        Err(not_ported("TransformOrderByExpressions", parse_result))
    }

    /// `TransformOrderByExpressionList`.
    pub(super) fn transform_order_by_expression_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<OrderByNode>> {
        Err(not_ported("TransformOrderByExpressionList", parse_result))
    }

    /// `TransformOrderByAll`.
    pub(super) fn transform_order_by_all(&mut self, parse_result: &ParseResult) -> Result<Vec<OrderByNode>> {
        Err(not_ported("TransformOrderByAll", parse_result))
    }

    /// `TransformOrderByExpression`.
    pub(super) fn transform_order_by_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OrderByNode> {
        Err(not_ported("TransformOrderByExpression", parse_result))
    }

    /// `TransformDescOrAsc`.
    pub(super) fn transform_desc_or_asc(&mut self, parse_result: &ParseResult) -> Result<OrderType> {
        Err(not_ported("TransformDescOrAsc", parse_result))
    }

    /// `TransformNullsFirstOrLast`.
    pub(super) fn transform_nulls_first_or_last(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OrderByNullType> {
        Err(not_ported("TransformNullsFirstOrLast", parse_result))
    }

    /// `TransformResultModifiers`.
    pub(super) fn transform_result_modifiers(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ResultModifier>> {
        Err(not_ported("TransformResultModifiers", parse_result))
    }

    /// `TransformLimitOffset`.
    pub(super) fn transform_limit_offset(&mut self, parse_result: &ParseResult) -> Result<ResultModifier> {
        Err(not_ported("TransformLimitOffset", parse_result))
    }

    /// `TransformOffsetLimitClause`.
    pub(super) fn transform_offset_limit_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ResultModifier> {
        Err(not_ported("TransformOffsetLimitClause", parse_result))
    }

    /// `TransformLimitOffsetClause`.
    pub(super) fn transform_limit_offset_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ResultModifier> {
        Err(not_ported("TransformLimitOffsetClause", parse_result))
    }

    /// `TransformLimitClause`.
    pub(super) fn transform_limit_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<LimitPercentResult> {
        Err(not_ported("TransformLimitClause", parse_result))
    }

    /// `TransformLimitValue`.
    pub(super) fn transform_limit_value(&mut self, parse_result: &ParseResult) -> Result<LimitPercentResult> {
        Err(not_ported("TransformLimitValue", parse_result))
    }

    /// `TransformLimitAll`.
    pub(super) fn transform_limit_all(&mut self, parse_result: &ParseResult) -> Result<LimitPercentResult> {
        Err(not_ported("TransformLimitAll", parse_result))
    }

    /// `TransformLimitLiteralPercent`.
    pub(super) fn transform_limit_literal_percent(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<LimitPercentResult> {
        Err(not_ported("TransformLimitLiteralPercent", parse_result))
    }

    /// `TransformLimitExpression`.
    pub(super) fn transform_limit_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<LimitPercentResult> {
        Err(not_ported("TransformLimitExpression", parse_result))
    }

    /// `TransformOffsetClause`.
    pub(super) fn transform_offset_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<LimitPercentResult> {
        Err(not_ported("TransformOffsetClause", parse_result))
    }

    /// `TransformGroupByClause`.
    pub(super) fn transform_group_by_clause(&mut self, parse_result: &ParseResult) -> Result<GroupByNode> {
        Err(not_ported("TransformGroupByClause", parse_result))
    }

    /// `TransformGroupByExpressions`.
    pub(super) fn transform_group_by_expressions(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<GroupByNode> {
        Err(not_ported("TransformGroupByExpressions", parse_result))
    }

    /// `TransformGroupByAll`.
    pub(super) fn transform_group_by_all(&mut self, parse_result: &ParseResult) -> Result<GroupByNode> {
        Err(not_ported("TransformGroupByAll", parse_result))
    }

    /// `TransformCubeOrRollup`.
    pub(super) fn transform_cube_or_rollup(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformCubeOrRollup", parse_result))
    }

    /// `TransformGroupByList`.
    pub(super) fn transform_group_by_list(&mut self, parse_result: &ParseResult) -> Result<GroupByNode> {
        Err(not_ported("TransformGroupByList", parse_result))
    }

    /// `TransformGroupByExpression`.
    pub(super) fn transform_group_by_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformGroupByExpression", parse_result))
    }

    /// `TransformEmptyGroupingItem`.
    pub(super) fn transform_empty_grouping_item(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformEmptyGroupingItem", parse_result))
    }

    /// `TransformCubeOrRollupClause`.
    pub(super) fn transform_cube_or_rollup_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformCubeOrRollupClause", parse_result))
    }

    /// `TransformGroupingSetsClause`.
    pub(super) fn transform_grouping_sets_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformGroupingSetsClause", parse_result))
    }

    /// `TransformWithClause`.
    pub(super) fn transform_with_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CommonTableExpressionMap> {
        Err(not_ported("TransformWithClause", parse_result))
    }

    /// `TransformWithStatement`.
    pub(super) fn transform_with_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, CommonTableExpressionInfo)> {
        Err(not_ported("TransformWithStatement", parse_result))
    }

    /// `TransformCTEBody`.
    pub(super) fn transform_cte_body(&mut self, parse_result: &ParseResult) -> Result<TableRef> {
        Err(not_ported("TransformCTEBody", parse_result))
    }

    /// `TransformMaterialized`.
    pub(super) fn transform_materialized(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformMaterialized", parse_result))
    }

    /// `TransformUsingKey`.
    pub(super) fn transform_using_key(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformUsingKey", parse_result))
    }

    /// `TransformHavingClause`.
    pub(super) fn transform_having_clause(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformHavingClause", parse_result))
    }

    /// `TransformOffsetValue`.
    pub(super) fn transform_offset_value(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<LimitPercentResult> {
        Err(not_ported("TransformOffsetValue", parse_result))
    }

    /// `TransformQualifyClause`.
    pub(super) fn transform_qualify_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformQualifyClause", parse_result))
    }

    /// `TransformWindowClause`.
    pub(super) fn transform_window_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformWindowClause", parse_result))
    }

    /// `TransformWindowDefinition`.
    pub(super) fn transform_window_definition(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformWindowDefinition", parse_result))
    }

    /// `TransformSampleClause`.
    pub(super) fn transform_sample_clause(&mut self, parse_result: &ParseResult) -> Result<SampleOptions> {
        Err(not_ported("TransformSampleClause", parse_result))
    }

    /// `TransformSampleEntry`.
    pub(super) fn transform_sample_entry(&mut self, parse_result: &ParseResult) -> Result<SampleOptions> {
        Err(not_ported("TransformSampleEntry", parse_result))
    }

    /// `TransformSampleEntryFunction`.
    pub(super) fn transform_sample_entry_function(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SampleOptions> {
        Err(not_ported("TransformSampleEntryFunction", parse_result))
    }

    /// `TransformSampleEntryCount`.
    pub(super) fn transform_sample_entry_count(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<SampleOptions> {
        Err(not_ported("TransformSampleEntryCount", parse_result))
    }

    /// `TransformSampleFunction`.
    pub(super) fn transform_sample_function(&mut self, parse_result: &ParseResult) -> Result<SampleMethod> {
        Err(not_ported("TransformSampleFunction", parse_result))
    }

    /// `TransformSampleCount`.
    pub(super) fn transform_sample_count(&mut self, parse_result: &ParseResult) -> Result<SampleOptions> {
        Err(not_ported("TransformSampleCount", parse_result))
    }

    /// `TransformSampleValue`.
    pub(super) fn transform_sample_value(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformSampleValue", parse_result))
    }

    /// `TransformSampleUnit`.
    pub(super) fn transform_sample_unit(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformSampleUnit", parse_result))
    }

    /// `TransformSampleProperties`.
    pub(super) fn transform_sample_properties(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(SampleMethod, Option<u64>)> {
        Err(not_ported("TransformSampleProperties", parse_result))
    }

    /// `TransformRepeatableSample`.
    pub(super) fn transform_repeatable_sample(&mut self, parse_result: &ParseResult) -> Result<Option<u64>> {
        Err(not_ported("TransformRepeatableSample", parse_result))
    }

    /// `TransformSampleSeed`.
    pub(super) fn transform_sample_seed(&mut self, parse_result: &ParseResult) -> Result<Option<u64>> {
        Err(not_ported("TransformSampleSeed", parse_result))
    }
}
