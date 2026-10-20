//! The transform functions of DuckDB's `transform_expression.cpp`.

use super::prelude::*;

impl PegTransformer {
    /// `TransformBaseExpression`.
    pub(super) fn transform_base_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformBaseExpression", parse_result))
    }

    /// `TransformNestedColumnName`.
    pub(super) fn transform_nested_column_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ColumnRefExpression> {
        Err(not_ported("TransformNestedColumnName", parse_result))
    }

    /// `TransformColumnReference`.
    pub(super) fn transform_column_reference(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformColumnReference", parse_result))
    }

    /// `TransformCatalogReservedSchemaTableColumnName`.
    pub(super) fn transform_catalog_reserved_schema_table_column_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ColumnRefExpression> {
        Err(not_ported(
            "TransformCatalogReservedSchemaTableColumnName",
            parse_result,
        ))
    }

    /// `TransformSchemaReservedTableColumnName`.
    pub(super) fn transform_schema_reserved_table_column_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ColumnRefExpression> {
        Err(not_ported("TransformSchemaReservedTableColumnName", parse_result))
    }

    /// `TransformReservedTableQualification`.
    pub(super) fn transform_reserved_table_qualification(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<String> {
        Err(not_ported("TransformReservedTableQualification", parse_result))
    }

    /// `TransformFunctionExpression`.
    pub(super) fn transform_function_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformFunctionExpression", parse_result))
    }

    /// `TransformWithinGroupClause`.
    pub(super) fn transform_within_group_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<OrderByNode>> {
        Err(not_ported("TransformWithinGroupClause", parse_result))
    }

    /// `TransformFunctionIdentifier`.
    pub(super) fn transform_function_identifier(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Err(not_ported("TransformFunctionIdentifier", parse_result))
    }

    /// `TransformSchemaReservedFunctionName`.
    pub(super) fn transform_schema_reserved_function_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Err(not_ported("TransformSchemaReservedFunctionName", parse_result))
    }

    /// `TransformCatalogReservedSchemaFunctionName`.
    pub(super) fn transform_catalog_reserved_schema_function_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        Err(not_ported(
            "TransformCatalogReservedSchemaFunctionName",
            parse_result,
        ))
    }

    /// `TransformListExpression`.
    pub(super) fn transform_list_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformListExpression", parse_result))
    }

    /// `TransformArrayBoundedListExpression`.
    pub(super) fn transform_array_bounded_list_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformArrayBoundedListExpression", parse_result))
    }

    /// `TransformFilterClause`.
    pub(super) fn transform_filter_clause(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformFilterClause", parse_result))
    }

    /// `TransformParenthesisExpression`.
    pub(super) fn transform_parenthesis_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformParenthesisExpression", parse_result))
    }

    /// `TransformArrayParensSelect`.
    pub(super) fn transform_array_parens_select(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformArrayParensSelect", parse_result))
    }

    /// `TransformStructExpression`.
    pub(super) fn transform_struct_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformStructExpression", parse_result))
    }

    /// `TransformStructField`.
    pub(super) fn transform_struct_field(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformStructField", parse_result))
    }

    /// `TransformBoundedListExpression`.
    pub(super) fn transform_bounded_list_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformBoundedListExpression", parse_result))
    }

    /// `TransformExpression`.
    pub(super) fn transform_expression(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformExpression", parse_result))
    }

    /// `TransformLambdaArrowExpression`.
    pub(super) fn transform_lambda_arrow_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformLambdaArrowExpression", parse_result))
    }

    /// `TransformLogicalOrExpression`.
    pub(super) fn transform_logical_or_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformLogicalOrExpression", parse_result))
    }

    /// `TransformLogicalAndExpression`.
    pub(super) fn transform_logical_and_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformLogicalAndExpression", parse_result))
    }

    /// `TransformLogicalNotExpression`.
    pub(super) fn transform_logical_not_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformLogicalNotExpression", parse_result))
    }

    /// `TransformIsExpression`.
    pub(super) fn transform_is_expression(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformIsExpression", parse_result))
    }

    /// `TransformIsTest`.
    pub(super) fn transform_is_test(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformIsTest", parse_result))
    }

    /// `TransformIsLiteral`.
    pub(super) fn transform_is_literal(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformIsLiteral", parse_result))
    }

    /// `TransformNotNull`.
    pub(super) fn transform_not_null(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformNotNull", parse_result))
    }

    /// `TransformIsNull`.
    pub(super) fn transform_is_null(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformIsNull", parse_result))
    }

    /// `TransformIsDistinctFromExpression`.
    pub(super) fn transform_is_distinct_from_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformIsDistinctFromExpression", parse_result))
    }

    /// `TransformComparisonExpression`.
    pub(super) fn transform_comparison_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformComparisonExpression", parse_result))
    }

    /// `TransformComparisonOperator`.
    pub(super) fn transform_comparison_operator(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ExpressionType> {
        Err(not_ported("TransformComparisonOperator", parse_result))
    }

    /// `TransformBetweenInLikeExpression`.
    pub(super) fn transform_between_in_like_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformBetweenInLikeExpression", parse_result))
    }

    /// `TransformBetweenInLikeOp`.
    pub(super) fn transform_between_in_like_op(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformBetweenInLikeOp", parse_result))
    }

    /// `TransformInClause`.
    pub(super) fn transform_in_clause(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformInClause", parse_result))
    }

    /// `TransformInExpression`.
    pub(super) fn transform_in_expression(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformInExpression", parse_result))
    }

    /// `TransformInExpressionList`.
    pub(super) fn transform_in_expression_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformInExpressionList", parse_result))
    }

    /// `TransformInSelectStatement`.
    pub(super) fn transform_in_select_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformInSelectStatement", parse_result))
    }

    /// `TransformBetweenClause`.
    pub(super) fn transform_between_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformBetweenClause", parse_result))
    }

    /// `TransformLikeClause`.
    pub(super) fn transform_like_clause(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformLikeClause", parse_result))
    }

    /// `TransformEscapeClause`.
    pub(super) fn transform_escape_clause(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformEscapeClause", parse_result))
    }

    /// `TransformLikeVariations`.
    pub(super) fn transform_like_variations(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformLikeVariations", parse_result))
    }

    /// `TransformOtherOperatorExpression`.
    pub(super) fn transform_other_operator_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformOtherOperatorExpression", parse_result))
    }

    /// `TransformOtherOperator`.
    pub(super) fn transform_other_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformOtherOperator", parse_result))
    }

    /// `TransformQualifiedOperator`.
    pub(super) fn transform_qualified_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformQualifiedOperator", parse_result))
    }

    /// `TransformAnyOp`.
    pub(super) fn transform_any_op(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformAnyOp", parse_result))
    }

    /// `TransformJsonOperator`.
    pub(super) fn transform_json_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformJsonOperator", parse_result))
    }

    /// `TransformInetOperator`.
    pub(super) fn transform_inet_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformInetOperator", parse_result))
    }

    /// `TransformStringOperator`.
    pub(super) fn transform_string_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformStringOperator", parse_result))
    }

    /// `TransformListOperator`.
    pub(super) fn transform_list_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformListOperator", parse_result))
    }

    /// `TransformAnyAllOperator`.
    pub(super) fn transform_any_all_operator(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, bool)> {
        Err(not_ported("TransformAnyAllOperator", parse_result))
    }

    /// `TransformAnyOrAll`.
    pub(super) fn transform_any_or_all(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformAnyOrAll", parse_result))
    }

    /// `TransformBitwiseExpression`.
    pub(super) fn transform_bitwise_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformBitwiseExpression", parse_result))
    }

    /// `TransformBitOperator`.
    pub(super) fn transform_bit_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformBitOperator", parse_result))
    }

    /// `TransformAdditiveExpression`.
    pub(super) fn transform_additive_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformAdditiveExpression", parse_result))
    }

    /// `TransformTerm`.
    pub(super) fn transform_term(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformTerm", parse_result))
    }

    /// `TransformMultiplicativeExpression`.
    pub(super) fn transform_multiplicative_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformMultiplicativeExpression", parse_result))
    }

    /// `TransformFactor`.
    pub(super) fn transform_factor(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformFactor", parse_result))
    }

    /// `TransformExponentiationExpression`.
    pub(super) fn transform_exponentiation_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformExponentiationExpression", parse_result))
    }

    /// `TransformExponentOperator`.
    pub(super) fn transform_exponent_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformExponentOperator", parse_result))
    }

    /// `TransformCollateExpression`.
    pub(super) fn transform_collate_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformCollateExpression", parse_result))
    }

    /// `TransformAtTimeZoneExpression`.
    pub(super) fn transform_at_time_zone_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformAtTimeZoneExpression", parse_result))
    }

    /// `TransformPrefixExpression`.
    pub(super) fn transform_prefix_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformPrefixExpression", parse_result))
    }

    /// `TransformPrefixOperator`.
    pub(super) fn transform_prefix_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformPrefixOperator", parse_result))
    }

    /// `TransformParameter`.
    pub(super) fn transform_parameter(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformParameter", parse_result))
    }

    /// `TransformAnonymousParameter`.
    pub(super) fn transform_anonymous_parameter(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformAnonymousParameter", parse_result))
    }

    /// `TransformQuestionMarkNumberedParameter`.
    pub(super) fn transform_question_mark_numbered_parameter(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformQuestionMarkNumberedParameter", parse_result))
    }

    /// `TransformNumberedParameter`.
    pub(super) fn transform_numbered_parameter(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformNumberedParameter", parse_result))
    }

    /// `TransformColLabelParameter`.
    pub(super) fn transform_col_label_parameter(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformColLabelParameter", parse_result))
    }

    /// `TransformPositionalExpression`.
    pub(super) fn transform_positional_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformPositionalExpression", parse_result))
    }

    /// `TransformLiteralExpression`.
    pub(super) fn transform_literal_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformLiteralExpression", parse_result))
    }

    /// `TransformParensExpression`.
    pub(super) fn transform_parens_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformParensExpression", parse_result))
    }

    /// `TransformConstantLiteral`.
    pub(super) fn transform_constant_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformConstantLiteral", parse_result))
    }

    /// `TransformSingleExpression`.
    pub(super) fn transform_single_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformSingleExpression", parse_result))
    }

    /// `TransformOperator`.
    pub(super) fn transform_operator(&mut self, parse_result: &ParseResult) -> Result<ExpressionType> {
        Err(not_ported("TransformOperator", parse_result))
    }

    /// `TransformConjunctionOperator`.
    pub(super) fn transform_conjunction_operator(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ExpressionType> {
        Err(not_ported("TransformConjunctionOperator", parse_result))
    }

    /// `TransformIsOperator`.
    pub(super) fn transform_is_operator(&mut self, parse_result: &ParseResult) -> Result<ExpressionType> {
        Err(not_ported("TransformIsOperator", parse_result))
    }

    /// `TransformInOperator`.
    pub(super) fn transform_in_operator(&mut self, parse_result: &ParseResult) -> Result<ExpressionType> {
        Err(not_ported("TransformInOperator", parse_result))
    }

    /// `TransformLambdaOperator`.
    pub(super) fn transform_lambda_operator(&mut self, parse_result: &ParseResult) -> Result<ExpressionType> {
        Err(not_ported("TransformLambdaOperator", parse_result))
    }

    /// `TransformBetweenOperator`.
    pub(super) fn transform_between_operator(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ExpressionType> {
        Err(not_ported("TransformBetweenOperator", parse_result))
    }

    /// `TransformIndirection`.
    pub(super) fn transform_indirection(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformIndirection", parse_result))
    }

    /// `TransformPostfixOperator`.
    pub(super) fn transform_postfix_operator(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformPostfixOperator", parse_result))
    }

    /// `TransformCastOperator`.
    pub(super) fn transform_cast_operator(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformCastOperator", parse_result))
    }

    /// `TransformDotOperator`.
    pub(super) fn transform_dot_operator(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformDotOperator", parse_result))
    }

    /// `TransformMethodExpression`.
    pub(super) fn transform_method_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformMethodExpression", parse_result))
    }

    /// `TransformSliceExpression`.
    pub(super) fn transform_slice_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformSliceExpression", parse_result))
    }

    /// `TransformSliceBound`.
    pub(super) fn transform_slice_bound(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformSliceBound", parse_result))
    }

    /// `TransformEndSliceBound`.
    pub(super) fn transform_end_slice_bound(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformEndSliceBound", parse_result))
    }

    /// `TransformStepSliceBound`.
    pub(super) fn transform_step_slice_bound(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformStepSliceBound", parse_result))
    }

    /// `TransformTableReservedColumnName`.
    pub(super) fn transform_table_reserved_column_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ColumnRefExpression> {
        Err(not_ported("TransformTableReservedColumnName", parse_result))
    }

    /// `TransformTableQualification`.
    pub(super) fn transform_table_qualification(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformTableQualification", parse_result))
    }

    /// `TransformStarExpression`.
    pub(super) fn transform_star_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformStarExpression", parse_result))
    }

    /// `TransformExcludeList`.
    pub(super) fn transform_exclude_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<QualifiedColumnName>> {
        Err(not_ported("TransformExcludeList", parse_result))
    }

    /// `TransformExcludeNameList`.
    pub(super) fn transform_exclude_name_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<QualifiedColumnName>> {
        Err(not_ported("TransformExcludeNameList", parse_result))
    }

    /// `TransformExcludeNameSingle`.
    pub(super) fn transform_exclude_name_single(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<QualifiedColumnName>> {
        Err(not_ported("TransformExcludeNameSingle", parse_result))
    }

    /// `TransformExcludeName`.
    pub(super) fn transform_exclude_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedColumnName> {
        Err(not_ported("TransformExcludeName", parse_result))
    }

    /// `TransformOverClause`.
    pub(super) fn transform_over_clause(&mut self, parse_result: &ParseResult) -> Result<WindowExpr> {
        Err(not_ported("TransformOverClause", parse_result))
    }

    /// `TransformWindowFrame`.
    pub(super) fn transform_window_frame(&mut self, parse_result: &ParseResult) -> Result<WindowExpr> {
        Err(not_ported("TransformWindowFrame", parse_result))
    }

    /// `TransformParensIdentifier`.
    pub(super) fn transform_parens_identifier(&mut self, parse_result: &ParseResult) -> Result<WindowExpr> {
        Err(not_ported("TransformParensIdentifier", parse_result))
    }

    /// `TransformWindowFrameDefinition`.
    pub(super) fn transform_window_frame_definition(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowExpr> {
        Err(not_ported("TransformWindowFrameDefinition", parse_result))
    }

    /// `TransformWindowFrameContentsParens`.
    pub(super) fn transform_window_frame_contents_parens(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowExpr> {
        Err(not_ported("TransformWindowFrameContentsParens", parse_result))
    }

    /// `TransformWindowFrameNameContentsParens`.
    pub(super) fn transform_window_frame_name_contents_parens(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowExpr> {
        Err(not_ported("TransformWindowFrameNameContentsParens", parse_result))
    }

    /// `TransformBaseWindowName`.
    pub(super) fn transform_base_window_name(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformBaseWindowName", parse_result))
    }

    /// `TransformWindowFrameContents`.
    pub(super) fn transform_window_frame_contents(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowExpr> {
        Err(not_ported("TransformWindowFrameContents", parse_result))
    }

    /// `TransformFrameClause`.
    pub(super) fn transform_frame_clause(&mut self, parse_result: &ParseResult) -> Result<WindowFrame> {
        Err(not_ported("TransformFrameClause", parse_result))
    }

    /// `TransformFrameExtent`.
    pub(super) fn transform_frame_extent(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<WindowBoundaryExpression>> {
        Err(not_ported("TransformFrameExtent", parse_result))
    }

    /// `TransformBetweenFrameExtent`.
    pub(super) fn transform_between_frame_extent(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<WindowBoundaryExpression>> {
        Err(not_ported("TransformBetweenFrameExtent", parse_result))
    }

    /// `TransformSingleFrameExtent`.
    pub(super) fn transform_single_frame_extent(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<WindowBoundaryExpression>> {
        Err(not_ported("TransformSingleFrameExtent", parse_result))
    }

    /// `TransformFrameBound`.
    pub(super) fn transform_frame_bound(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowBoundaryExpression> {
        Err(not_ported("TransformFrameBound", parse_result))
    }

    /// `TransformFrameUnbounded`.
    pub(super) fn transform_frame_unbounded(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowBoundaryExpression> {
        Err(not_ported("TransformFrameUnbounded", parse_result))
    }

    /// `TransformFrameExpression`.
    pub(super) fn transform_frame_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowBoundaryExpression> {
        Err(not_ported("TransformFrameExpression", parse_result))
    }

    /// `TransformFrameCurrentRow`.
    pub(super) fn transform_frame_current_row(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowBoundaryExpression> {
        Err(not_ported("TransformFrameCurrentRow", parse_result))
    }

    /// `TransformPrecedingOrFollowing`.
    pub(super) fn transform_preceding_or_following(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformPrecedingOrFollowing", parse_result))
    }

    /// `TransformWindowExcludeClause`.
    pub(super) fn transform_window_exclude_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowExcludeMode> {
        Err(not_ported("TransformWindowExcludeClause", parse_result))
    }

    /// `TransformFraming`.
    pub(super) fn transform_framing(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformFraming", parse_result))
    }

    /// `TransformWindowExcludeElement`.
    pub(super) fn transform_window_exclude_element(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowExcludeMode> {
        Err(not_ported("TransformWindowExcludeElement", parse_result))
    }

    /// `TransformWindowPartition`.
    pub(super) fn transform_window_partition(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformWindowPartition", parse_result))
    }

    /// `TransformSpecialFunctionExpression`.
    pub(super) fn transform_special_function_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformSpecialFunctionExpression", parse_result))
    }

    /// `TransformCoalesceExpression`.
    pub(super) fn transform_coalesce_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformCoalesceExpression", parse_result))
    }

    /// `TransformUnpackExpression`.
    pub(super) fn transform_unpack_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformUnpackExpression", parse_result))
    }

    /// `TransformTryExpression`.
    pub(super) fn transform_try_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformTryExpression", parse_result))
    }

    /// `TransformColumnsExpression`.
    pub(super) fn transform_columns_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformColumnsExpression", parse_result))
    }

    /// `TransformExtractExpression`.
    pub(super) fn transform_extract_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformExtractExpression", parse_result))
    }

    /// `TransformExtractArgument`.
    pub(super) fn transform_extract_argument(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformExtractArgument", parse_result))
    }

    /// `TransformLambdaExpression`.
    pub(super) fn transform_lambda_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformLambdaExpression", parse_result))
    }

    /// `TransformNullIfExpression`.
    pub(super) fn transform_null_if_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformNullIfExpression", parse_result))
    }

    /// `TransformRowExpression`.
    pub(super) fn transform_row_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformRowExpression", parse_result))
    }

    /// `TransformSubstringExpression`.
    pub(super) fn transform_substring_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformSubstringExpression", parse_result))
    }

    /// `TransformSubstringArguments`.
    pub(super) fn transform_substring_arguments(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformSubstringArguments", parse_result))
    }

    /// `TransformSubstringExpressionList`.
    pub(super) fn transform_substring_expression_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformSubstringExpressionList", parse_result))
    }

    /// `TransformSubstringParameters`.
    pub(super) fn transform_substring_parameters(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformSubstringParameters", parse_result))
    }

    /// `TransformTrimExpression`.
    pub(super) fn transform_trim_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformTrimExpression", parse_result))
    }

    /// `TransformTrimDirection`.
    pub(super) fn transform_trim_direction(&mut self, parse_result: &ParseResult) -> Result<String> {
        Err(not_ported("TransformTrimDirection", parse_result))
    }

    /// `TransformTrimSource`.
    pub(super) fn transform_trim_source(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformTrimSource", parse_result))
    }

    /// `TransformPositionExpression`.
    pub(super) fn transform_position_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformPositionExpression", parse_result))
    }

    /// `TransformCastExpression`.
    pub(super) fn transform_cast_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformCastExpression", parse_result))
    }

    /// `TransformCastOrTryCast`.
    pub(super) fn transform_cast_or_try_cast(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformCastOrTryCast", parse_result))
    }

    /// `TransformCaseExpression`.
    pub(super) fn transform_case_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformCaseExpression", parse_result))
    }

    /// `TransformCaseElse`.
    pub(super) fn transform_case_else(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformCaseElse", parse_result))
    }

    /// `TransformCaseWhenThen`.
    pub(super) fn transform_case_when_then(&mut self, parse_result: &ParseResult) -> Result<CaseCheck> {
        Err(not_ported("TransformCaseWhenThen", parse_result))
    }

    /// `TransformTypeLiteral`.
    pub(super) fn transform_type_literal(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        Err(not_ported("TransformTypeLiteral", parse_result))
    }

    /// `TransformDefaultExpression`.
    pub(super) fn transform_default_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformDefaultExpression", parse_result))
    }

    /// `TransformIntervalLiteral`.
    pub(super) fn transform_interval_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformIntervalLiteral", parse_result))
    }

    /// `TransformIntervalParameter`.
    pub(super) fn transform_interval_parameter(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformIntervalParameter", parse_result))
    }

    /// `TransformSubqueryExpression`.
    pub(super) fn transform_subquery_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformSubqueryExpression", parse_result))
    }

    /// `TransformMapExpression`.
    pub(super) fn transform_map_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformMapExpression", parse_result))
    }

    /// `TransformMapStructExpression`.
    pub(super) fn transform_map_struct_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformMapStructExpression", parse_result))
    }

    /// `TransformMapStructField`.
    pub(super) fn transform_map_struct_field(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        Err(not_ported("TransformMapStructField", parse_result))
    }

    /// `TransformListComprehensionExpression`.
    pub(super) fn transform_list_comprehension_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformListComprehensionExpression", parse_result))
    }

    /// `TransformListComprehensionFilter`.
    pub(super) fn transform_list_comprehension_filter(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformListComprehensionFilter", parse_result))
    }

    /// `TransformReplaceList`.
    pub(super) fn transform_replace_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression>> {
        Err(not_ported("TransformReplaceList", parse_result))
    }

    /// `TransformReplaceEntries`.
    pub(super) fn transform_replace_entries(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression>> {
        Err(not_ported("TransformReplaceEntries", parse_result))
    }

    /// `TransformReplaceEntrySingle`.
    pub(super) fn transform_replace_entry_single(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression>> {
        Err(not_ported("TransformReplaceEntrySingle", parse_result))
    }

    /// `TransformReplaceEntryList`.
    pub(super) fn transform_replace_entry_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression>> {
        Err(not_ported("TransformReplaceEntryList", parse_result))
    }

    /// `TransformReplaceEntry`.
    pub(super) fn transform_replace_entry(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, ParsedExpression)> {
        Err(not_ported("TransformReplaceEntry", parse_result))
    }

    /// `TransformIsDistinctFromOp`.
    pub(super) fn transform_is_distinct_from_op(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ExpressionType> {
        Err(not_ported("TransformIsDistinctFromOp", parse_result))
    }

    /// `TransformGroupingExpression`.
    pub(super) fn transform_grouping_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Err(not_ported("TransformGroupingExpression", parse_result))
    }

    /// `TransformRenameList`.
    pub(super) fn transform_rename_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OrderedMap<QualifiedColumnName, String>> {
        Err(not_ported("TransformRenameList", parse_result))
    }

    /// `TransformRenameEntryList`.
    pub(super) fn transform_rename_entry_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OrderedMap<QualifiedColumnName, String>> {
        Err(not_ported("TransformRenameEntryList", parse_result))
    }

    /// `TransformSingleRenameEntry`.
    pub(super) fn transform_single_rename_entry(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OrderedMap<QualifiedColumnName, String>> {
        Err(not_ported("TransformSingleRenameEntry", parse_result))
    }

    /// `TransformRenameEntry`.
    pub(super) fn transform_rename_entry(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(QualifiedColumnName, String)> {
        Err(not_ported("TransformRenameEntry", parse_result))
    }

    /// `TransformIgnoreOrRespectNulls`.
    pub(super) fn transform_ignore_or_respect_nulls(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Err(not_ported("TransformIgnoreOrRespectNulls", parse_result))
    }
}
