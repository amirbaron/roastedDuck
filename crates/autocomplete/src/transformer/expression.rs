//! The transform functions of DuckDB's `transform_expression.cpp`.

use rustdb_function::cast::ValueCast;
use rustdb_parser::build;

use rustdb_parser::QualifiedColumnMap;
use rustdb_parser::hash_order::CiHashMap;

use super::common::{convert_number_to_value, simple_type_expression, try_negate_value, unbound_type};
use super::parse_result::string_literal_expression;
use super::prelude::*;

/// `ExpressionIsEmptyStar`: a plain `*` without `COLUMNS`, `EXCLUDE` or `REPLACE`.
pub(super) fn expression_is_empty_star(expression: &ParsedExpression) -> bool {
    matches!(
        &expression.kind,
        ParsedExpressionKind::Star(star)
            if !star.columns && star.exclude_list.is_empty() && star.replace_list.is_empty()
    )
}

/// `RemoveOrderQualificationRecursive`: `t.x` becomes `x` in every column reference.
fn remove_order_qualification_recursive(expression: &mut ParsedExpression) {
    if let ParsedExpressionKind::ColumnRef(column_ref) = &mut expression.kind
        && column_ref.column_names.len() > 1
        && let Some(last) = column_ref.column_names.pop()
    {
        column_ref.column_names = vec![last];
    }
    rustdb_parser::equality::for_each_child_mut(expression, &mut remove_order_qualification_recursive);
}

/// `FunctionExpression(catalog, schema, name, children)`.
fn qualified_function(
    catalog: &str,
    schema: &str,
    name: &str,
    children: Vec<ParsedExpression>,
) -> ParsedExpression {
    build::function_full(
        catalog.to_owned(),
        schema.to_owned(),
        name,
        children,
        None,
        OrderModifier { orders: Vec::new() },
        false,
        false,
        false,
    )
}

/// `LambdaExpression(lhs, expr)`.
pub(super) fn lambda_expression(
    lhs: ParsedExpression,
    expr: ParsedExpression,
    syntax_type: LambdaSyntaxType,
) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::Lambda,
        ParsedExpressionKind::Lambda(LambdaExpression {
            lhs: Box::new(lhs),
            expr: Box::new(expr),
            syntax_type,
        }),
    )
}

/// The window expression of an `OVER` clause.
fn window_mut(window: &mut WindowExpr) -> Result<&mut WindowExpression> {
    match &mut window.0.kind {
        ParsedExpressionKind::Window(window) => Ok(window),
        _ => Err(Error::internal("Expected a window expression")),
    }
}

impl PegTransformer {
    /// `TransformBaseExpression`: `SingleExpression Indirection*`; casts, subscripts,
    /// method calls and field accesses apply to the expression in turn.
    pub(super) fn transform_base_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut expr: ParsedExpression = self.transform_child(list, 0)?;
        let Some(indirections) = list.optional(1)? else {
            return Ok(expr);
        };
        let mut prev_indirection_was_cast = false;
        for child in indirections.repeat()? {
            let mut indirection: ParsedExpression = self.transform(child)?;
            match &mut indirection.kind {
                ParsedExpressionKind::Cast(cast) => {
                    cast.child = Box::new(expr);
                    prev_indirection_was_cast = true;
                }
                ParsedExpressionKind::Operator(operator) => {
                    if prev_indirection_was_cast {
                        return Err(Error::parser(
                            "Subscript/slice cannot be applied directly after a cast operator (e.g. x::TYPE[1:3] \
                             is not allowed). Wrap the cast in parentheses: (x::TYPE)[1:3]",
                        ));
                    }
                    operator.children.insert(0, expr);
                    prev_indirection_was_cast = false;
                }
                ParsedExpressionKind::Function(function) => {
                    function.children.insert(0, expr);
                    prev_indirection_was_cast = false;
                }
                ParsedExpressionKind::Constant(_) => {
                    indirection =
                        ParsedExpression::operator(ExpressionType::StructExtract, vec![expr, indirection]);
                    prev_indirection_was_cast = false;
                }
                _ => {
                    return Err(Error::not_implemented(
                        "Unhandled case for Base Expression with indirection",
                    ));
                }
            }
            expr = indirection;
        }
        Ok(expr)
    }

    /// `TransformNestedColumnName`: `(Identifier '.')* Identifier`.
    pub(super) fn transform_nested_column_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ColumnRefExpression> {
        let list = parse_result.list()?;
        let mut column_names = Vec::new();
        if let Some(qualifiers) = list.optional(0)? {
            for qualifier in qualifiers.repeat()? {
                column_names.push(qualifier.list()?.identifier(0)?.to_owned());
            }
        }
        column_names.push(list.identifier(1)?.to_owned());
        Ok(ColumnRefExpression { column_names })
    }

    /// `TransformColumnReference`.
    pub(super) fn transform_column_reference(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let column_ref: ColumnRefExpression = self.transform(parse_result.list()?.choice(0)?)?;
        Ok(ParsedExpression::new(
            ExpressionType::ColumnRef,
            ParsedExpressionKind::ColumnRef(column_ref),
        ))
    }

    /// `TransformCatalogReservedSchemaTableColumnName`: `catalog.schema.table.column`.
    pub(super) fn transform_catalog_reserved_schema_table_column_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ColumnRefExpression> {
        let list = parse_result.list()?;
        let column_names = vec![
            self.transform_child(list, 0)?,
            self.transform_child(list, 1)?,
            self.transform_child(list, 2)?,
            list.identifier(3)?.to_owned(),
        ];
        Ok(ColumnRefExpression { column_names })
    }

    /// `TransformSchemaReservedTableColumnName`: `schema.table.column`.
    pub(super) fn transform_schema_reserved_table_column_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ColumnRefExpression> {
        let list = parse_result.list()?;
        let column_names = vec![
            self.transform_child(list, 0)?,
            self.transform_child(list, 1)?,
            list.identifier(2)?.to_owned(),
        ];
        Ok(ColumnRefExpression { column_names })
    }

    /// `TransformReservedTableQualification`: the table of `table.column`.
    pub(super) fn transform_reserved_table_qualification(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<String> {
        Ok(parse_result.list()?.identifier(0)?.to_owned())
    }

    /// `TransformFunctionExpression`: a function call, with its `DISTINCT`, `ORDER BY`,
    /// `FILTER`, `WITHIN GROUP`, `IGNORE NULLS`, `EXPORT_STATE` and `OVER` clauses, and
    /// DuckDB's rewrites of special functions.
    #[allow(clippy::too_many_lines, reason = "one function, like DuckDB's")]
    pub(super) fn transform_function_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let function: QualifiedName = self.transform_child(list, 0)?;
        let arguments = list.child(1)?.parens_content()?.list()?;
        let mut distinct: bool = self.transform_optional(arguments, 0)?.unwrap_or(false);
        let mut children: Vec<ParsedExpression> = Vec::new();
        if let Some(argument_list) = arguments.optional(1)? {
            for argument in argument_list.list_elements()? {
                children.push(self.transform(argument)?);
            }
        }
        let mut order_modifier = OrderModifier {
            orders: self.transform_optional(arguments, 2)?.unwrap_or_default(),
        };
        let mut filter_expr: Option<ParsedExpression> = self.transform_optional(list, 3)?;
        let has_ignore_nulls_result = arguments.has(3)?;
        let ignore_nulls: bool = self.transform_optional(arguments, 3)?.unwrap_or(false);
        let export_state = list.has(4)?;
        if children.len() == 1
            && expression_is_empty_star(&children[0])
            && !distinct
            && order_modifier.orders.is_empty()
        {
            // COUNT(*) becomes COUNT().
            children.clear();
        }
        let mut lowercase_name = function.name.to_ascii_lowercase();
        if let Some(over) = list.optional(5)? {
            return self.transform_window_function(
                over,
                &function,
                lowercase_name,
                children,
                WindowArguments {
                    distinct,
                    ignore_nulls,
                    has_ignore_nulls_result,
                    filter_expr,
                    orders: order_modifier.orders,
                    export_state,
                },
            );
        }
        if lowercase_name == "count" && children.is_empty() {
            "count_star".clone_into(&mut lowercase_name);
        }
        match lowercase_name.as_str() {
            "if" => {
                let [when, then, otherwise]: [ParsedExpression; 3] = children
                    .try_into()
                    .map_err(|_| Error::parser("Wrong number of arguments to IF."))?;
                let check = CaseCheck {
                    when_expr: Box::new(when),
                    then_expr: Box::new(then),
                };
                return Ok(build::case_expression(vec![check], otherwise));
            }
            "unpack" => {
                if children.len() != 1 {
                    return Err(Error::parser("Wrong number of arguments to the UNPACK operator"));
                }
                return Ok(ParsedExpression::operator(
                    ExpressionType::OperatorUnpack,
                    children,
                ));
            }
            "try" => {
                if children.len() != 1 {
                    return Err(Error::parser(
                        "Wrong number of arguments provided to TRY expression",
                    ));
                }
                return Ok(ParsedExpression::operator(ExpressionType::OperatorTry, children));
            }
            "construct_array" => {
                return Ok(ParsedExpression::operator(
                    ExpressionType::ArrayConstructor,
                    children,
                ));
            }
            "position" => {
                if children.len() != 2 {
                    return Err(Error::parser(
                        "Wrong number of arguments to __internal_position_operator.",
                    ));
                }
                // POSITION(x IN y) takes its arguments the other way round.
                children.swap(0, 1);
            }
            "ifnull" => {
                if children.len() != 2 {
                    return Err(Error::parser("Wrong number of arguments to IFNULL."));
                }
                // A two-argument COALESCE.
                return Ok(ParsedExpression::operator(
                    ExpressionType::OperatorCoalesce,
                    children,
                ));
            }
            "date" => {
                let [child]: [ParsedExpression; 1] = children
                    .try_into()
                    .map_err(|_| Error::parser("Wrong number of arguments provided to DATE function"))?;
                return Ok(build::cast(LogicalType::DATE, child, false));
            }
            "list" if order_modifier.orders.len() == 1 => {
                // list(expr ORDER BY expr <sense> <nulls>) is list_sort(list(expr), <sense>, <nulls>).
                if children.len() != 1 {
                    return Err(Error::parser("Wrong number of arguments to LIST."));
                }
                let order_by = &order_modifier.orders[0];
                if rustdb_parser::equality::expressions_equal(&children[0], &order_by.expression) {
                    let sense = build::varchar_constant(order_by.order_type.name());
                    let nulls = build::varchar_constant(order_by.null_order.name());
                    let unordered = build::function_full(
                        function.catalog.clone(),
                        function.schema.clone(),
                        &lowercase_name,
                        std::mem::take(&mut children),
                        filter_expr.take().map(Box::new),
                        std::mem::replace(&mut order_modifier, OrderModifier { orders: Vec::new() }),
                        distinct,
                        false,
                        export_state,
                    );
                    "list_sort".clone_into(&mut lowercase_name);
                    distinct = false;
                    children = vec![unordered, sense, nulls];
                }
            }
            _ => {}
        }
        if let Some(within_group) = list.optional(2)? {
            let order_by_clause: Vec<OrderByNode> = self.transform(within_group)?;
            if distinct {
                return Err(Error::parser(
                    "DISTINCT is not allowed in combination with WITHIN GROUP",
                ));
            }
            if !order_modifier.orders.is_empty() {
                return Err(Error::parser(
                    "Cannot use multiple ORDER BY statements with WITHIN GROUP",
                ));
            }
            order_modifier.orders = order_by_clause;
            if order_modifier.orders.len() != 1 {
                return Err(Error::parser(
                    "Cannot use multiple ORDER BY clauses with WITHIN GROUP",
                ));
            }
            match lowercase_name.as_str() {
                "percentile_cont" => {
                    if children.len() != 1 {
                        return Err(Error::parser("Wrong number of arguments for PERCENTILE_CONT"));
                    }
                    "quantile_cont".clone_into(&mut lowercase_name);
                }
                "percentile_disc" => {
                    if children.len() != 1 {
                        return Err(Error::parser("Wrong number of arguments for PERCENTILE_DISC"));
                    }
                    "quantile_disc".clone_into(&mut lowercase_name);
                }
                "mode" => {
                    if !children.is_empty() {
                        return Err(Error::parser("Wrong number of arguments for MODE"));
                    }
                }
                _ => {
                    return Err(Error::parser(format!(
                        "Unknown ordered aggregate \"{}\".",
                        function.name
                    )));
                }
            }
        }
        if has_ignore_nulls_result {
            return Err(Error::parser(
                "RESPECT/IGNORE NULLS is not supported for non-window functions",
            ));
        }
        Ok(build::function_full(
            function.catalog,
            function.schema,
            &lowercase_name,
            children,
            filter_expr.map(Box::new),
            order_modifier,
            distinct,
            false,
            export_state,
        ))
    }

    /// The `OVER` branch of `TransformFunctionExpression`: a window function.
    fn transform_window_function(
        &mut self,
        over: &ParseResult,
        function: &QualifiedName,
        lowercase_name: String,
        mut children: Vec<ParsedExpression>,
        arguments: WindowArguments,
    ) -> Result<ParsedExpression> {
        if self.in_window_definition {
            return Err(Error::parser(
                "window functions are not allowed in window definitions",
            ));
        }
        let window_type = rustdb_parser::window_to_expression_type(&lowercase_name);
        if window_type != ExpressionType::WindowAggregate && arguments.distinct {
            return Err(Error::parser(
                "DISTINCT is not implemented for non-aggregate window functions!",
            ));
        }
        if !arguments.orders.is_empty() && !rustdb_parser::is_orderable_window_function(window_type)? {
            return Err(Error::parser(format!(
                "ORDER BY is not supported for the window function \"{lowercase_name}\""
            )));
        }
        if window_type != ExpressionType::WindowAggregate && arguments.filter_expr.is_some() {
            return Err(Error::parser(
                "FILTER is not implemented for non-aggregate window functions!",
            ));
        }
        if arguments.export_state {
            return Err(Error::parser(
                "EXPORT_STATE is not supported for window functions!",
            ));
        }
        if window_type == ExpressionType::WindowAggregate && arguments.has_ignore_nulls_result {
            return Err(Error::parser(
                "RESPECT/IGNORE NULLS is not supported for windowed aggregates",
            ));
        }
        self.in_window_definition = true;
        let mut expr: WindowExpr = self.transform(over)?;
        expr.0.expression_type = window_type;
        let window = window_mut(&mut expr)?;
        function.catalog.clone_into(&mut window.catalog);
        function.schema.clone_into(&mut window.schema);
        window.function_name = lowercase_name;
        let wrong_arguments = || {
            Error::parser(format!(
                "Incorrect number of parameters for function {}",
                function.name
            ))
        };
        if window_type == ExpressionType::WindowAggregate {
            window.children = children;
        } else {
            let argument_count = children.len();
            let mut arguments_iter = children.drain(..);
            if let Some(first) = arguments_iter.next() {
                window.children.push(first);
            }
            match window_type {
                ExpressionType::WindowLead | ExpressionType::WindowLag => {
                    window.offset_expr = arguments_iter.next().map(Box::new);
                    window.default_expr = arguments_iter.next().map(Box::new);
                    if argument_count > 3 {
                        return Err(wrong_arguments());
                    }
                }
                ExpressionType::WindowNthValue => {
                    window.children.extend(arguments_iter.next());
                    if argument_count > 2 {
                        return Err(wrong_arguments());
                    }
                }
                _ => {
                    if argument_count > 1 {
                        return Err(wrong_arguments());
                    }
                }
            }
        }
        window.ignore_nulls = arguments.ignore_nulls;
        window.filter_expr = arguments.filter_expr.map(Box::new);
        window.arg_orders = arguments.orders;
        window.distinct = arguments.distinct;
        if window.exclude_clause != WindowExcludeMode::NoOther
            && !window.arg_orders.is_empty()
            && !rustdb_parser::is_excludable_window_function(window_type)?
        {
            return Err(Error::parser(format!(
                "EXCLUDE is not supported for the window function \"{}\"",
                window.function_name
            )));
        }
        self.in_window_definition = false;
        Ok(expr.0)
    }

    /// `TransformWithinGroupClause`: `WITHIN GROUP (ORDER BY ...)`.
    pub(super) fn transform_within_group_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<OrderByNode>> {
        self.transform(parse_result.list()?.child(2)?.parens_content()?)
    }

    /// `TransformFunctionIdentifier`: a (qualified) function name.
    pub(super) fn transform_function_identifier(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        let name = parse_result.list()?.choice(0)?;
        if name.result_type() == ParseResultType::Identifier {
            return Ok(QualifiedName {
                name: name.identifier()?.to_owned(),
                ..QualifiedName::default()
            });
        }
        self.transform(name)
    }

    /// `TransformSchemaReservedFunctionName`: `schema.function`.
    pub(super) fn transform_schema_reserved_function_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        let list = parse_result.list()?;
        Ok(QualifiedName {
            catalog: String::new(),
            schema: self.transform_child(list, 0)?,
            name: list.identifier(1)?.to_owned(),
        })
    }

    /// `TransformCatalogReservedSchemaFunctionName`: `catalog.schema.function` (or
    /// `schema.function`).
    pub(super) fn transform_catalog_reserved_schema_function_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedName> {
        let list = parse_result.list()?;
        let mut result = QualifiedName::default();
        if let Some(schema) = list.optional(1)? {
            result.catalog = self.transform_child(list, 0)?;
            result.schema = self.transform(schema)?;
        } else {
            result.schema = self.transform_child(list, 0)?;
        }
        list.identifier(2)?.clone_into(&mut result.name);
        Ok(result)
    }

    /// `TransformListExpression`.
    pub(super) fn transform_list_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformArrayBoundedListExpression`: `[...]` is a list, `ARRAY[...]` an array.
    pub(super) fn transform_array_bounded_list_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let is_array = list.has(0)?;
        let elements: Vec<ParsedExpression> = self.transform_child(list, 1)?;
        if is_array {
            Ok(ParsedExpression::operator(
                ExpressionType::ArrayConstructor,
                elements,
            ))
        } else {
            Ok(qualified_function("", "main", "list_value", elements))
        }
    }

    /// `TransformFilterClause`: `FILTER (WHERE expr)`.
    pub(super) fn transform_filter_clause(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let inner = parse_result.list()?.child(1)?.parens_content()?.list()?;
        self.transform_child(inner, 1)
    }

    /// `TransformParenthesisExpression`: `(expr)` is the expression, `(a, b, ...)` a row.
    pub(super) fn transform_parenthesis_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let elements = parse_result.list()?.child(0)?.parens_content()?.list_elements()?;
        let mut children = elements
            .into_iter()
            .map(|element| self.transform(element))
            .collect::<Result<Vec<ParsedExpression>>>()?;
        if children.len() == 1
            && let Some(child) = children.pop()
        {
            return Ok(child);
        }
        Ok(qualified_function("", "main", "row", children))
    }

    /// `TransformArrayParensSelect`: `ARRAY(subquery)` is
    /// `(SELECT CASE WHEN array_agg(col) IS NULL THEN [] ELSE array_agg(col) END FROM (subquery))`,
    /// with the subquery's ORDER BY moved into the aggregate.
    pub(super) fn transform_array_parens_select(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let mut subquery: SelectStatement =
            self.transform(parse_result.list()?.child(1)?.parens_content()?)?;
        let is_select = matches!(subquery.node.kind, QueryNodeKind::Select(_));
        let array_agg_child = if let QueryNodeKind::Select(select) = &subquery.node.kind {
            // The easy case: the subquery is a SELECT.
            if select.select_list.len() != 1 {
                return Err(Error::binder(format!(
                    "Subquery returns {} columns - expected 1",
                    select.select_list.len()
                )));
            }
            build::positional_reference(1)
        } else {
            // A set operation: the ORDER BY columns cannot be pushed into it.
            let mut columns_star = build::star_expression(String::new());
            columns_star.columns = true;
            ParsedExpression::new(
                ExpressionType::Star,
                ParsedExpressionKind::Star(Box::new(columns_star)),
            )
        };
        // array_agg(COLUMNS(*)), with the subquery's ORDER BY.
        let mut orders: Option<Vec<OrderByNode>> =
            subquery
                .node
                .modifiers
                .iter()
                .find_map(|modifier| match modifier {
                    ResultModifier::Order(order) => Some(order.orders.clone()),
                    _ => None,
                });
        // Constants (ORDER BY 1) become positional references (ORDER BY #1).
        let mut array_idx = 0u64;
        if let Some(orders) = &mut orders {
            for order in orders.iter_mut() {
                if order.expression.expression_type == ExpressionType::ValueConstant {
                    let ParsedExpressionKind::Constant(constant) = &order.expression.kind else {
                        continue;
                    };
                    if let Ok(index) = constant.value.try_cast(&LogicalType::BIGINT)
                        && let Some(index) = index.as_i64()
                    {
                        let positional_index = u64::try_from(index).unwrap_or(u64::MAX);
                        order.expression = Box::new(build::positional_reference(positional_index));
                    }
                } else if is_select {
                    // Push the ORDER BY expression into the SELECT list and refer to it.
                    array_idx += 1;
                    let alias = format!("__array_internal_idx_{array_idx}");
                    let mut expression = std::mem::replace(
                        &mut order.expression,
                        Box::new(build::column_ref(vec![alias.clone()])),
                    );
                    alias.clone_into(&mut expression.alias);
                    if let QueryNodeKind::Select(select) = &mut subquery.node.kind {
                        select.select_list.push(*expression);
                    }
                } else {
                    remove_order_qualification_recursive(&mut order.expression);
                }
            }
        }
        let aggregate = build::function_full(
            String::new(),
            String::new(),
            "array_agg",
            vec![array_agg_child],
            None,
            OrderModifier {
                orders: orders.unwrap_or_default(),
            },
            false,
            false,
            false,
        );
        let aggregate_is_null =
            ParsedExpression::operator(ExpressionType::OperatorIsNull, vec![aggregate.clone()]);
        let check = CaseCheck {
            when_expr: Box::new(aggregate_is_null),
            then_expr: Box::new(build::function("list_value", Vec::new())),
        };
        let mut select_node = build::select_node();
        select_node
            .select_list
            .push(build::case_expression(vec![check], aggregate));
        select_node.from_table = Box::new(build::subquery_ref(subquery));
        let subquery =
            build::select_statement(build::query_node(QueryNodeKind::Select(Box::new(select_node))));
        Ok(ParsedExpression::new(
            ExpressionType::Subquery,
            ParsedExpressionKind::Subquery(SubqueryExpression {
                subquery_type: SubqueryType::Scalar,
                subquery: Box::new(subquery),
                child: None,
                comparison_type: ExpressionType::Invalid,
            }),
        ))
    }

    /// `TransformStructExpression`: `{name: value, ...}` is `struct_pack(...)`.
    pub(super) fn transform_struct_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let fields = parse_result.list()?.child(1)?.list_elements()?;
        let children = fields
            .into_iter()
            .map(|field| self.transform(field))
            .collect::<Result<Vec<ParsedExpression>>>()?;
        Ok(qualified_function("", "main", "struct_pack", children))
    }

    /// `TransformStructField`: `name: value`, the value aliased by the name.
    pub(super) fn transform_struct_field(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let alias: String = self.transform_child(list, 0)?;
        let expr: ParsedExpression = self.transform_child(list, 2)?;
        Ok(expr.with_alias(alias))
    }

    /// `TransformBoundedListExpression`: the elements of `[...]`.
    pub(super) fn transform_bounded_list_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        let Some(elements) = parse_result.list()?.optional(1)? else {
            return Ok(Vec::new());
        };
        elements
            .list_elements()?
            .into_iter()
            .map(|element| self.transform(element))
            .collect()
    }

    /// `TransformExpression`.
    pub(super) fn transform_expression(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform_child(parse_result.list()?, 0)
    }

    /// `TransformLambdaArrowExpression`: `x -> y -> ...` nests lambdas to the right of
    /// the first expression.
    pub(super) fn transform_lambda_arrow_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut expr: ParsedExpression = self.transform_child(list, 0)?;
        let Some(arrows) = list.optional(1)? else {
            return Ok(expr);
        };
        for pair in arrows.repeat()? {
            let right: ParsedExpression = self.transform_child(pair.list()?, 1)?;
            expr = lambda_expression(expr, right, LambdaSyntaxType::SingleArrow);
        }
        Ok(expr)
    }

    /// `TransformLogicalOrExpression`: `a OR b OR ...`.
    pub(super) fn transform_logical_or_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform_conjunction(parse_result, ExpressionType::ConjunctionOr)
    }

    /// `TransformLogicalAndExpression`: `a AND b AND ...`.
    pub(super) fn transform_logical_and_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform_conjunction(parse_result, ExpressionType::ConjunctionAnd)
    }

    /// The conjunction of `operand (keyword operand)*`.
    fn transform_conjunction(
        &mut self,
        parse_result: &ParseResult,
        conjunction: ExpressionType,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut expr: ParsedExpression = self.transform_child(list, 0)?;
        if let Some(operands) = list.optional(1)? {
            for operand in operands.repeat()? {
                let right: ParsedExpression = self.transform_child(operand.list()?, 1)?;
                expr = build::conjunction(conjunction, expr, right);
            }
        }
        Ok(expr)
    }
}

/// The parts of a function call that apply to its window (see
/// `transform_window_function`).
struct WindowArguments {
    distinct: bool,
    ignore_nulls: bool,
    has_ignore_nulls_result: bool,
    filter_expr: Option<ParsedExpression>,
    orders: Vec<OrderByNode>,
    export_state: bool,
}

/// A placeholder for the operand an enclosing rule fills in (DuckDB passes `nullptr`).
fn operand_placeholder() -> ParsedExpression {
    ParsedExpression::constant(Value::default())
}

/// `OperatorExpression(type, child)`.
fn unary_operator(expression_type: ExpressionType, child: ParsedExpression) -> ParsedExpression {
    ParsedExpression::operator(expression_type, vec![child])
}

/// `FunctionExpression(name, children)` with `is_operator` set.
fn operator_function(name: &str, children: Vec<ParsedExpression>) -> ParsedExpression {
    let mut function = build::function(name, children);
    if let ParsedExpressionKind::Function(inner) = &mut function.kind {
        inner.is_operator = true;
    }
    function
}

/// `SubqueryExpression` of `subquery`.
fn subquery_expression(
    subquery_type: SubqueryType,
    subquery: SelectStatement,
    child: Option<ParsedExpression>,
    comparison_type: ExpressionType,
) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::Subquery,
        ParsedExpressionKind::Subquery(SubqueryExpression {
            subquery_type,
            subquery: Box::new(subquery),
            child: child.map(Box::new),
            comparison_type,
        }),
    )
}

/// `TryNegateLikeFunction`: `NOT LIKE` / `NOT ILIKE` have operators of their own.
fn try_negate_like_function(function_name: &mut String) -> bool {
    let negated = match function_name.as_str() {
        "~~" => "!~~",
        "~~*" => "!~~*",
        _ => return false,
    };
    negated.clone_into(function_name);
    true
}

/// `IsNumberLiteral`: whether the parse result is a plain number, possibly wrapped in
/// expression rules.
fn is_number_literal(parse_result: &ParseResult) -> Result<bool> {
    match parse_result.name {
        "BaseExpression" => {
            let list = parse_result.list()?;
            if list.has(1)? {
                return Ok(false);
            }
            is_number_literal(list.child(0)?)
        }
        "SingleExpression" | "LiteralExpression" => is_number_literal(parse_result.list()?.choice(0)?),
        name => Ok(name == "NumberLiteral"),
    }
}

/// `GetRawText`: the text of the number `is_number_literal` found.
fn raw_number_text(parse_result: &ParseResult) -> Result<String> {
    match parse_result.name {
        "NumberLiteral" => Ok(parse_result.number()?.to_owned()),
        "BaseExpression" => raw_number_text(parse_result.list()?.child(0)?),
        "SingleExpression" | "LiteralExpression" => raw_number_text(parse_result.list()?.choice(0)?),
        _ => Ok(String::new()),
    }
}

/// The value of a constant parameter number (`Value::GetValue<int32_t>`).
fn integer_constant(expression: &ParsedExpression) -> Result<(i32, Value)> {
    let ParsedExpressionKind::Constant(constant) = &expression.kind else {
        return Err(Error::internal("Expected a constant"));
    };
    let number = constant
        .value
        .default_cast_as(&LogicalType::INTEGER)?
        .as_i32()
        .ok_or_else(|| Error::internal("INTEGER value without payload"))?;
    Ok((number, constant.value.clone()))
}

/// `LambdaExpression(named_parameters, expr)`: `lambda x, y: expr`.
fn keyword_lambda(parameters: &[String], expr: ParsedExpression) -> ParsedExpression {
    let lhs = match parameters {
        [single] => build::column_ref(vec![single.clone()]),
        _ => build::function(
            "row",
            parameters
                .iter()
                .map(|name| build::column_ref(vec![name.clone()]))
                .collect(),
        ),
    };
    lambda_expression(lhs, expr, LambdaSyntaxType::LambdaKeyword)
}

/// `WindowExpression(type, catalog, schema, name)`.
fn new_window_expression(expression_type: ExpressionType, name: &str) -> WindowExpr {
    WindowExpr(ParsedExpression::new(
        expression_type,
        ParsedExpressionKind::Window(Box::new(WindowExpression {
            function_name: name.to_ascii_lowercase(),
            schema: String::new(),
            catalog: String::new(),
            children: Vec::new(),
            partitions: Vec::new(),
            orders: Vec::new(),
            start: WindowBoundary::Invalid,
            end: WindowBoundary::Invalid,
            start_expr: None,
            end_expr: None,
            offset_expr: None,
            default_expr: None,
            ignore_nulls: false,
            filter_expr: None,
            exclude_clause: WindowExcludeMode::NoOther,
            distinct: false,
            arg_orders: Vec::new(),
        })),
    ))
}

/// `PEGTransformer::IsWindowFrameDefault`: `RANGE BETWEEN UNBOUNDED PRECEDING AND
/// CURRENT ROW`.
fn is_window_frame_default(window: &WindowExpression) -> bool {
    window.start == WindowBoundary::UnboundedPreceding && window.end == WindowBoundary::CurrentRowRange
}

/// `DateTruncSimplificationRule::DatePartToFunc`: the function building an interval of
/// the unit.
const fn date_part_to_function(unit: DatePartSpecifier) -> &'static str {
    match unit {
        DatePartSpecifier::Year => "to_years",
        DatePartSpecifier::Month => "to_months",
        DatePartSpecifier::Day => "to_days",
        DatePartSpecifier::Decade => "to_decades",
        DatePartSpecifier::Century => "to_centuries",
        DatePartSpecifier::Millennium => "to_millennia",
        DatePartSpecifier::Microseconds => "to_microseconds",
        DatePartSpecifier::Milliseconds => "to_milliseconds",
        DatePartSpecifier::Second => "to_seconds",
        DatePartSpecifier::Minute => "to_minutes",
        DatePartSpecifier::Hour => "to_hours",
        DatePartSpecifier::Week => "to_weeks",
        DatePartSpecifier::Quarter => "to_quarters",
    }
}

/// `QualifiedName::ParseComponents`: the dot-separated parts of a name; double quotes
/// protect dots.
fn parse_components(input: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut entry = String::new();
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => loop {
                match chars.next() {
                    Some('"') => break,
                    Some(quoted) => entry.push(quoted),
                    None => {
                        return Err(Error::parser(format!(
                            "Unterminated quote in qualified name! (input: {input})"
                        )));
                    }
                }
            },
            '.' => result.push(std::mem::take(&mut entry)),
            other => entry.push(other),
        }
    }
    if !entry.is_empty() {
        result.push(entry);
    }
    Ok(result)
}

/// `QualifiedColumnName::Parse`.
fn parse_qualified_column_name(input: &str) -> Result<QualifiedColumnName> {
    let components = parse_components(input)?;
    let count = components.len();
    let mut components = components.into_iter();
    let mut next = || components.next().unwrap_or_default();
    Ok(match count {
        1 => QualifiedColumnName::column(next()),
        2 => QualifiedColumnName {
            table: next(),
            column: next(),
            ..QualifiedColumnName::default()
        },
        3 => QualifiedColumnName {
            schema: next(),
            table: next(),
            column: next(),
            ..QualifiedColumnName::default()
        },
        4 => QualifiedColumnName {
            catalog: next(),
            schema: next(),
            table: next(),
            column: next(),
        },
        count => {
            return Err(Error::parser(format!(
                "Expected at most 4 entries (catalog.schema.table.column), but found {count} entries (input: \
                 {input})"
            )));
        }
    })
}

/// `QualifiedColumnEquality`: a prefix comparison, so `tbl.i` equals `i` but not `tbl2.i`.
fn qualified_column_equal(a: &QualifiedColumnName, b: &QualifiedColumnName) -> bool {
    let part_matches = |x: &str, y: &str| x.is_empty() || y.is_empty() || x.eq_ignore_ascii_case(y);
    part_matches(&a.catalog, &b.catalog)
        && part_matches(&a.schema, &b.schema)
        && part_matches(&a.table, &b.table)
        && a.column.eq_ignore_ascii_case(&b.column)
}

impl PegTransformer {
    /// `TransformLogicalNotExpression`: `NOT* expr`.
    pub(super) fn transform_logical_not_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut expr: ParsedExpression = self.transform_child(list, 1)?;
        if let Some(nots) = list.optional(0)? {
            for _ in nots.repeat()? {
                expr = unary_operator(ExpressionType::OperatorNot, expr);
            }
        }
        Ok(expr)
    }

    /// `TransformIsExpression`: `expr IsTest*`; each test takes the expression as its
    /// operand.
    pub(super) fn transform_is_expression(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut expr: ParsedExpression = self.transform_child(list, 0)?;
        let Some(tests) = list.optional(1)? else {
            return Ok(expr);
        };
        for test in tests.repeat()? {
            let mut is_expr: ParsedExpression = self.transform(test)?;
            match &mut is_expr.kind {
                ParsedExpressionKind::Comparison(comparison) => {
                    comparison.left = Box::new(build::cast(LogicalType::BOOLEAN, expr, false));
                }
                ParsedExpressionKind::Operator(operator) => operator.children.insert(0, expr),
                _ => {
                    return Err(Error::internal(format!(
                        "Unexpected expression encountered in IsExpression: {}",
                        is_expr.expression_class().name()
                    )));
                }
            }
            expr = is_expr;
        }
        Ok(expr)
    }

    /// `TransformIsTest`.
    pub(super) fn transform_is_test(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformIsLiteral`: `IS [NOT] NULL/TRUE/FALSE/UNKNOWN`, without its operand.
    pub(super) fn transform_is_literal(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let is_not = list.has(1)?;
        let literal: Value = Self::transform_enum(list.list(2)?.choice(0)?)?;
        if literal.is_null() {
            let expression_type = if is_not {
                ExpressionType::OperatorIsNotNull
            } else {
                ExpressionType::OperatorIsNull
            };
            return Ok(ParsedExpression::operator(expression_type, Vec::new()));
        }
        let expression_type = if is_not {
            ExpressionType::CompareDistinctFrom
        } else {
            ExpressionType::CompareNotDistinctFrom
        };
        Ok(ParsedExpression::comparison(
            expression_type,
            operand_placeholder(),
            ParsedExpression::constant(literal),
        ))
    }

    /// `TransformNotNull`: `NOTNULL`, without its operand.
    pub(super) fn transform_not_null(&mut self, _parse_result: &ParseResult) -> Result<ParsedExpression> {
        Ok(ParsedExpression::operator(
            ExpressionType::OperatorIsNotNull,
            Vec::new(),
        ))
    }

    /// `TransformIsNull`: `ISNULL`, without its operand.
    pub(super) fn transform_is_null(&mut self, _parse_result: &ParseResult) -> Result<ParsedExpression> {
        Ok(ParsedExpression::operator(
            ExpressionType::OperatorIsNull,
            Vec::new(),
        ))
    }

    /// `TransformIsDistinctFromExpression`: `a IS [NOT] DISTINCT FROM b ...`.
    pub(super) fn transform_is_distinct_from_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut expr: ParsedExpression = self.transform_child(list, 0)?;
        if let Some(comparisons) = list.optional(1)? {
            for comparison in comparisons.repeat()? {
                let comparison = comparison.list()?;
                let distinct_type: ExpressionType = self.transform_child(comparison, 0)?;
                let right: ParsedExpression = self.transform_child(comparison, 1)?;
                expr = ParsedExpression::comparison(distinct_type, expr, right);
            }
        }
        Ok(expr)
    }

    /// `TransformComparisonExpression`: `a op [NOT]* b ...`.
    pub(super) fn transform_comparison_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut expr: ParsedExpression = self.transform_child(list, 0)?;
        if let Some(comparisons) = list.optional(1)? {
            for comparison in comparisons.repeat()? {
                let comparison = comparison.list()?;
                let comparison_type: ExpressionType = self.transform_child(comparison, 0)?;
                let mut right: ParsedExpression = self.transform_child(comparison, 2)?;
                if let Some(nots) = comparison.optional(1)? {
                    for _ in nots.repeat()? {
                        right = unary_operator(ExpressionType::OperatorNot, right);
                    }
                }
                expr = ParsedExpression::comparison(comparison_type, expr, right);
            }
        }
        Ok(expr)
    }

    /// `TransformComparisonOperator`.
    pub(super) fn transform_comparison_operator(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ExpressionType> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformBetweenInLikeExpression`: `expr [NOT] BETWEEN/IN/LIKE ...`; the
    /// operator takes the expression as its operand.
    pub(super) fn transform_between_in_like_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let expr: ParsedExpression = self.transform_child(list, 0)?;
        let Some(operation) = list.optional(1)? else {
            return Ok(expr);
        };
        let mut result: ParsedExpression = self.transform(operation)?;
        let has_not = operation.list()?.has(0)?;
        // Whether the operation is negated with a NOT around it.
        let wrap_in_not = match &mut result.kind {
            ParsedExpressionKind::Between(between) => {
                between.input = Box::new(expr);
                has_not
            }
            ParsedExpressionKind::Function(function) => {
                if function.function_name == "contains" {
                    function.children.push(expr);
                } else {
                    function.children.insert(0, expr);
                }
                if has_not {
                    // A special LIKE function is negated by name, others by NOT.
                    !try_negate_like_function(&mut function.function_name)
                } else if function.function_name == "!~" {
                    "regexp_full_match".clone_into(&mut function.function_name);
                    function.is_operator = false;
                    true
                } else {
                    false
                }
            }
            ParsedExpressionKind::Operator(operator) => {
                operator.children.insert(0, expr);
                has_not
            }
            ParsedExpressionKind::Subquery(subquery) => {
                subquery.child = Some(Box::new(expr));
                has_not
            }
            _ => return Ok(expr),
        };
        Ok(if wrap_in_not {
            unary_operator(ExpressionType::OperatorNot, result)
        } else {
            result
        })
    }

    /// `TransformBetweenInLikeOp`: the operation, without its operand.
    pub(super) fn transform_between_in_like_op(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.list(1)?.choice(0)?)
    }

    /// `TransformInClause`: `IN ...`.
    pub(super) fn transform_in_clause(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformInExpression`: a list or subquery, or `contains(expr)` for a list value.
    pub(super) fn transform_in_expression(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.name.eq_ignore_ascii_case("InExpressionList")
            || choice.name.eq_ignore_ascii_case("InSelectStatement")
        {
            return self.transform(choice);
        }
        let right: ParsedExpression = self.transform(choice)?;
        Ok(build::function("contains", vec![right]))
    }

    /// `TransformInExpressionList`: `(a, b, ...)`; `(subquery)` is `= ANY (subquery)`.
    pub(super) fn transform_in_expression_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let elements = parse_result.list()?.child(0)?.parens_content()?.list_elements()?;
        let mut children = elements
            .into_iter()
            .map(|element| self.transform(element))
            .collect::<Result<Vec<ParsedExpression>>>()?;
        if let [child] = children.as_mut_slice()
            && let ParsedExpressionKind::Subquery(subquery) = &mut child.kind
        {
            let subquery = std::mem::replace(&mut subquery.subquery, Box::new(empty_select_statement()));
            return Ok(subquery_expression(
                SubqueryType::Any,
                *subquery,
                None,
                ExpressionType::CompareEqual,
            ));
        }
        Ok(ParsedExpression::operator(ExpressionType::CompareIn, children))
    }

    /// `TransformInSelectStatement`: `(SELECT ...)` is `= ANY (subquery)`.
    pub(super) fn transform_in_select_statement(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let subquery: SelectStatement = self.transform(parse_result.list()?.child(0)?.parens_content()?)?;
        Ok(subquery_expression(
            SubqueryType::Any,
            subquery,
            None,
            ExpressionType::CompareEqual,
        ))
    }

    /// `TransformBetweenClause`: `BETWEEN lower AND upper`, without its operand.
    pub(super) fn transform_between_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let lower: ParsedExpression = self.transform_child(list, 1)?;
        let upper: ParsedExpression = self.transform_child(list, 3)?;
        Ok(ParsedExpression::new(
            ExpressionType::CompareBetween,
            ParsedExpressionKind::Between(BetweenExpression {
                input: Box::new(operand_placeholder()),
                lower: Box::new(lower),
                upper: Box::new(upper),
            }),
        ))
    }

    /// `TransformLikeClause`: `LIKE/ILIKE/GLOB/SIMILAR TO pattern [ESCAPE escape]`,
    /// without its operand.
    pub(super) fn transform_like_clause(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut like_variation: String = self.transform_child(list, 0)?;
        let mut children: Vec<ParsedExpression> = vec![self.transform_child(list, 1)?];
        if let Some(escape) = list.optional(2)? {
            match like_variation.as_str() {
                "~~" => "like_escape".clone_into(&mut like_variation),
                "~~*" => "ilike_escape".clone_into(&mut like_variation),
                _ => {}
            }
            children.push(self.transform(escape)?);
        }
        if like_variation == "regexp_full_match" {
            Ok(build::function(&like_variation, children))
        } else {
            Ok(operator_function(&like_variation, children))
        }
    }

    /// `TransformEscapeClause`: `ESCAPE expr`.
    pub(super) fn transform_escape_clause(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.child(1)?)
    }

    /// `TransformLikeVariations`.
    pub(super) fn transform_like_variations(&mut self, parse_result: &ParseResult) -> Result<String> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformOtherOperatorExpression`: `a op b ...` for other operators, and
    /// `a op ANY/ALL (b)`.
    pub(super) fn transform_other_operator_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut expr: ParsedExpression = self.transform_child(list, 0)?;
        let Some(operations) = list.optional(1)? else {
            return Ok(expr);
        };
        for operation in operations.repeat()? {
            let operation = operation.list()?;
            let right: ParsedExpression = self.transform_child(operation, 1)?;
            let operator = operation.child(0)?;
            let operator_choice = operator.list()?.choice(0)?;
            if !operator_choice.name.eq_ignore_ascii_case("AnyAllOperator") {
                let operator: String = self.transform(operator)?;
                expr = operator_function(&operator, vec![expr, right]);
                continue;
            }
            let (op_string, is_any): (String, bool) = self.transform(operator_choice)?;
            // The comparison the operator stands for (INVALID for other operators).
            let expression_type = rustdb_parser::operator_to_expression_type(&op_string);
            let mut subquery = if let ParsedExpressionKind::Subquery(right_subquery) = right.kind {
                if expression_type == ExpressionType::Invalid {
                    return Err(Error::parser(
                        "ANY and ALL operators require one of =,<>,>,<,>=,<= comparisons!",
                    ));
                }
                subquery_expression(
                    SubqueryType::Any,
                    *right_subquery.subquery,
                    Some(expr),
                    expression_type,
                )
            } else {
                // left = ANY(right) is left = ANY((SELECT UNNEST(right))).
                if expression_type == ExpressionType::Invalid {
                    return Err(Error::parser(format!(
                        "Unsupported comparison \"{op_string}\" for ANY/ALL subquery"
                    )));
                }
                let mut select_node = build::select_node();
                select_node
                    .select_list
                    .push(build::function("UNNEST", vec![right]));
                let select =
                    build::select_statement(build::query_node(QueryNodeKind::Select(Box::new(select_node))));
                let subquery = subquery_expression(SubqueryType::Any, select, Some(expr), expression_type);
                if is_any {
                    return Ok(subquery);
                }
                subquery
            };
            if !is_any {
                // An ALL sublink is NOT(ANY) with the comparison inverted: `= ALL()` is
                // `NOT(<> ANY())`.
                if let ParsedExpressionKind::Subquery(inner) = &mut subquery.kind {
                    inner.comparison_type = rustdb_parser::negate_comparison(inner.comparison_type)?;
                }
                return Ok(unary_operator(ExpressionType::OperatorNot, subquery));
            }
            expr = subquery;
        }
        Ok(expr)
    }

    /// `TransformOtherOperator`: the operator's text.
    pub(super) fn transform_other_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        let child = parse_result.list()?.choice(0)?;
        // An `OperatorLiteral` is the operator token itself.
        if child.result_type() == ParseResultType::Operator {
            return Ok(child.operator()?.to_owned());
        }
        self.transform(child)
    }

    /// `TransformQualifiedOperator`: `OPERATOR(op)`.
    pub(super) fn transform_qualified_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        self.transform(parse_result.list()?.child(1)?.parens_content()?)
    }

    /// `TransformAnyOp`.
    pub(super) fn transform_any_op(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.choice(0)?.keyword()?.to_owned())
    }

    /// `TransformStringOperator`.
    pub(super) fn transform_string_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.choice(0)?.keyword()?.to_owned())
    }

    /// `TransformJsonOperator`.
    pub(super) fn transform_json_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.keyword(0)?.to_owned())
    }

    /// `TransformInetOperator`.
    pub(super) fn transform_inet_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.choice(0)?.keyword()?.to_owned())
    }

    /// `TransformListOperator`.
    pub(super) fn transform_list_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.choice(0)?.keyword()?.to_owned())
    }

    /// `TransformAnyAllOperator`: the comparison and whether it is `ANY` (or `ALL`).
    pub(super) fn transform_any_all_operator(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, bool)> {
        let list = parse_result.list()?;
        let op_string: String = self.transform_child(list, 0)?;
        let is_any: bool = self.transform_child(list, 1)?;
        Ok((op_string, is_any))
    }

    /// `TransformAnyOrAll`.
    pub(super) fn transform_any_or_all(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// The binary operators of one precedence level: `operand (op operand)*`, applied
    /// left to right as operator functions.
    fn transform_binary_operators(
        &mut self,
        parse_result: &ParseResult,
        mut map_operator: impl FnMut(&Self, String) -> String,
        locate: bool,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut expr: ParsedExpression = self.transform_child(list, 0)?;
        if let Some(operations) = list.optional(1)? {
            for operation in operations.repeat()? {
                let operation_list = operation.list()?;
                let operator: String = self.transform_child(operation_list, 0)?;
                let operator = map_operator(self, operator);
                let right: ParsedExpression = self.transform_child(operation_list, 1)?;
                expr = operator_function(&operator, vec![expr, right]);
                if locate && let Some(offset) = operation.offset {
                    expr.query_location = Some(offset as u64);
                }
            }
        }
        Ok(expr)
    }

    /// `TransformBitwiseExpression`: `a & b`, `a | b`, `a << b`, ...
    pub(super) fn transform_bitwise_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform_binary_operators(parse_result, |_, op| op, false)
    }

    /// `TransformBitOperator`.
    pub(super) fn transform_bit_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.choice(0)?.keyword()?.to_owned())
    }

    /// `TransformAdditiveExpression`: `a + b`, `a - b`, ...; each operation is located
    /// at its operator.
    pub(super) fn transform_additive_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform_binary_operators(parse_result, |_, op| op, true)
    }

    /// `TransformTerm`.
    pub(super) fn transform_term(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.choice(0)?.keyword()?.to_owned())
    }

    /// `TransformMultiplicativeExpression`: `a * b`, `a / b` (`//` with
    /// `integer_division`), `a % b`, ...
    pub(super) fn transform_multiplicative_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform_binary_operators(
            parse_result,
            |transformer, op| {
                if op == "/" && transformer.options.integer_division {
                    "//".to_owned()
                } else {
                    op
                }
            },
            false,
        )
    }

    /// `TransformFactor`.
    pub(super) fn transform_factor(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.choice(0)?.keyword()?.to_owned())
    }

    /// `TransformExponentiationExpression`: `a ^ b`, `a ** b`.
    pub(super) fn transform_exponentiation_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform_binary_operators(parse_result, |_, op| op, false)
    }

    /// `TransformExponentOperator`.
    pub(super) fn transform_exponent_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.choice(0)?.keyword()?.to_owned())
    }

    /// `TransformCollateExpression`: `expr COLLATE collation ...`.
    pub(super) fn transform_collate_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut expr: ParsedExpression = self.transform_child(list, 0)?;
        if let Some(collations) = list.optional(1)? {
            for collation in collations.repeat()? {
                let collation: ParsedExpression = self.transform_child(collation.list()?, 1)?;
                let collation = match &collation.kind {
                    ParsedExpressionKind::Constant(constant) => constant.value.to_varchar()?,
                    ParsedExpressionKind::ColumnRef(column_ref) => column_ref.column_names.join("."),
                    _ => {
                        return Err(Error::not_implemented(format!(
                            "Unexpected expression encountered for collate, {}",
                            collation.expression_class().name()
                        )));
                    }
                };
                expr = ParsedExpression::new(
                    ExpressionType::Collate,
                    ParsedExpressionKind::Collate(CollateExpression {
                        child: Box::new(expr),
                        collation,
                    }),
                );
            }
        }
        Ok(expr)
    }

    /// `TransformAtTimeZoneExpression`: `expr AT TIME ZONE zone` is `timezone(zone, expr)`.
    pub(super) fn transform_at_time_zone_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut expr: ParsedExpression = self.transform_child(list, 0)?;
        if let Some(zones) = list.optional(1)? {
            for zone in zones.repeat()? {
                let zone: ParsedExpression = self.transform_child(zone.list()?, 1)?;
                expr = qualified_function("", "main", "timezone", vec![zone, expr]);
            }
        }
        Ok(expr)
    }

    /// `TransformPrefixExpression`: `-x`, `+x`, `~x`, ...; `-<number>` is one constant,
    /// and `-` folds into constants it can negate.
    pub(super) fn transform_prefix_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let base = list.child(1)?;
        let Some(prefixes) = list.optional(0)? else {
            return self.transform(base);
        };
        let prefixes = prefixes.repeat()?;
        // `-<number>` as one literal, so that the minimum values do not overflow.
        if let [prefix] = prefixes {
            let prefix: String = self.transform(prefix)?;
            if prefix == "-" && is_number_literal(base)? {
                return convert_number_to_value(&format!("-{}", raw_number_text(base)?));
            }
        }
        let mut expr: ParsedExpression = self.transform(base)?;
        // The rightmost prefix applies first.
        for prefix in prefixes.iter().rev() {
            let prefix: String = self.transform(prefix)?;
            if prefix == "-"
                && expr.expression_type == ExpressionType::ValueConstant
                && let ParsedExpressionKind::Constant(constant) = &mut expr.kind
                && let Some(negated) = try_negate_value(&constant.value)
            {
                constant.value = negated;
                continue;
            }
            expr = operator_function(&prefix, vec![expr]);
        }
        Ok(expr)
    }

    /// `TransformPrefixOperator`.
    pub(super) fn transform_prefix_operator(&mut self, parse_result: &ParseResult) -> Result<String> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.name.eq_ignore_ascii_case("QualifiedOperator") {
            return self.transform(choice);
        }
        Self::transform_enum(choice)
    }

    /// `TransformParameter`.
    pub(super) fn transform_parameter(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformAnonymousParameter`: `?` takes the next parameter number.
    pub(super) fn transform_anonymous_parameter(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let index = self.prepared_statement_parameter_index + 1;
        let identifier = index.to_string();
        self.set_param(&identifier, index, PreparedParamType::AutoIncrement)?;
        self.prepared_statement_parameter_index = self.prepared_statement_parameter_index.max(index);
        Ok(parameter_expression(identifier))
    }

    /// `TransformQuestionMarkNumberedParameter`: `?n`.
    pub(super) fn transform_question_mark_numbered_parameter(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform_numbered_parameter(parse_result)
    }

    /// `TransformNumberedParameter`: `$n`.
    pub(super) fn transform_numbered_parameter(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let number: ParsedExpression = self.transform(parse_result.list()?.child(1)?)?;
        let (param_number, value) = integer_constant(&number)?;
        if param_number <= 0 {
            return Err(Error::parser("Parameter numbers must be greater than 0"));
        }
        let identifier = value.to_varchar()?;
        let index = match self.get_param(&identifier, PreparedParamType::Positional)? {
            Some(index) => index,
            None => {
                let index = u64::from(param_number.unsigned_abs());
                self.set_param(&identifier, index, PreparedParamType::Positional)?;
                index
            }
        };
        self.prepared_statement_parameter_index = self.prepared_statement_parameter_index.max(index);
        Ok(parameter_expression(identifier))
    }

    /// `TransformColLabelParameter`: `$name`; a new name takes the next number.
    pub(super) fn transform_col_label_parameter(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let identifier: String = self.transform(parse_result.list()?.child(1)?)?;
        let index = match self.get_param(&identifier, PreparedParamType::Named)? {
            Some(index) => index,
            None => {
                let index = self.prepared_statement_parameter_index + 1;
                self.set_param(&identifier, index, PreparedParamType::Named)?;
                index
            }
        };
        self.prepared_statement_parameter_index = self.prepared_statement_parameter_index.max(index);
        Ok(parameter_expression(identifier))
    }

    /// `TransformPositionalExpression`: `#n`.
    pub(super) fn transform_positional_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let number: ParsedExpression = self.transform(parse_result.list()?.child(1)?)?;
        let (index, _) = integer_constant(&number)?;
        if index <= 0 {
            return Err(Error::parser("Positional reference node needs to be >= 1"));
        }
        Ok(build::positional_reference(u64::from(index.unsigned_abs())))
    }

    /// `TransformLiteralExpression`: a string, number or NULL / TRUE / FALSE.
    pub(super) fn transform_literal_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let matched = parse_result.list()?.child(0)?;
        if matched.name == "StringLiteral" {
            let (text, string_type) = matched.choice()?.string_literal()?;
            return Ok(string_literal_expression(text, string_type));
        }
        self.transform(matched.choice()?)
    }

    /// `TransformParensExpression`: `(expr)`.
    pub(super) fn transform_parens_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.child(0)?.parens_content()?)
    }

    /// `TransformConstantLiteral`: NULL, or `'t'::BOOLEAN` / `'f'::BOOLEAN`.
    pub(super) fn transform_constant_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let value: Value = Self::transform_enum(parse_result.list()?.choice(0)?)?;
        if value.is_null() {
            return Ok(ParsedExpression::constant(value));
        }
        let text: String = value.to_varchar()?.chars().take(1).collect();
        Ok(build::cast(
            LogicalType::BOOLEAN,
            build::varchar_constant(&text),
            false,
        ))
    }

    /// `TransformSingleExpression`.
    pub(super) fn transform_single_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformOperator`: the comparison an operator stands for.
    pub(super) fn transform_operator(&mut self, parse_result: &ParseResult) -> Result<ExpressionType> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.result_type() == ParseResultType::Operator {
            return Ok(rustdb_parser::operator_to_expression_type(choice.operator()?));
        }
        self.transform(choice)
    }

    /// `TransformConjunctionOperator`.
    pub(super) fn transform_conjunction_operator(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ExpressionType> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformIsOperator`: `IS [NOT] [DISTINCT FROM]`.
    pub(super) fn transform_is_operator(&mut self, parse_result: &ParseResult) -> Result<ExpressionType> {
        let list = parse_result.list()?;
        let is_not = list.has(1)?;
        let is_distinct = list.has(2)?;
        Ok(match (is_distinct, is_not) {
            (true, true) => ExpressionType::CompareNotDistinctFrom,
            (true, false) => ExpressionType::CompareDistinctFrom,
            (false, true) => ExpressionType::OperatorIsNotNull,
            (false, false) => ExpressionType::OperatorIsNull,
        })
    }

    /// `TransformInOperator`: `[NOT] IN`.
    pub(super) fn transform_in_operator(&mut self, parse_result: &ParseResult) -> Result<ExpressionType> {
        Ok(if parse_result.list()?.has(0)? {
            ExpressionType::CompareNotIn
        } else {
            ExpressionType::CompareIn
        })
    }

    /// `TransformLambdaOperator`.
    pub(super) fn transform_lambda_operator(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<ExpressionType> {
        Ok(ExpressionType::Lambda)
    }

    /// `TransformBetweenOperator`: `[NOT] BETWEEN`.
    pub(super) fn transform_between_operator(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ExpressionType> {
        Ok(if parse_result.list()?.has(0)? {
            ExpressionType::CompareNotBetween
        } else {
            ExpressionType::CompareBetween
        })
    }

    /// `TransformIndirection`: a cast, subscript, slice, field access or method call.
    pub(super) fn transform_indirection(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformPostfixOperator`: `x!` is `factorial(x)` (the operand is added later).
    pub(super) fn transform_postfix_operator(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Ok(build::function("factorial", Vec::new()))
    }

    /// `TransformCastOperator`: `::type`; the operand replaces the placeholder later.
    pub(super) fn transform_cast_operator(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let ty: LogicalType = self.transform_child(parse_result.list()?, 1)?;
        Ok(build::cast(ty, operand_placeholder(), false))
    }

    /// `TransformDotOperator`: `.field` (a constant for `struct_extract`) or `.method(...)`.
    pub(super) fn transform_dot_operator(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let choice = parse_result.list()?.list(1)?.child(0)?;
        match choice.name {
            "ColLabel" => {
                let field: String = self.transform(choice.choice()?)?;
                Ok(build::varchar_constant(&field))
            }
            "MethodExpression" => self.transform(choice.choice()?),
            _ => Err(Error::internal("Unexpected rule encountered in 'DotOperator'")),
        }
    }

    /// `TransformMethodExpression`: `.method(args)`, a function whose first argument is
    /// added later.
    pub(super) fn transform_method_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let method: String = self.transform_child(list, 0)?;
        let arguments = list.child(1)?.parens_content()?.list()?;
        let distinct: bool = self.transform_optional(arguments, 0)?.unwrap_or(false);
        let mut children: Vec<ParsedExpression> = Vec::new();
        if let Some(argument_list) = arguments.optional(1)? {
            for argument in argument_list.list_elements()? {
                children.push(self.transform(argument)?);
            }
        }
        if children.len() == 1 && expression_is_empty_star(&children[0]) {
            // COUNT(*) becomes COUNT().
            children.clear();
        }
        let orders: Vec<OrderByNode> = self.transform_optional(arguments, 2)?.unwrap_or_default();
        let _: Option<bool> = self.transform_optional(arguments, 3)?;
        let mut result = qualified_function("", "main", &method, children);
        if let ParsedExpressionKind::Function(function) = &mut result.kind {
            function.distinct = distinct;
            if !orders.is_empty() {
                function.order_bys = OrderModifier { orders };
            }
        }
        Ok(result)
    }

    /// `TransformSliceExpression`: `[i]` (an `ARRAY_EXTRACT`) or `[a:b:c]` (an
    /// `ARRAY_SLICE`), without the sliced expression.
    pub(super) fn transform_slice_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let bounds: Vec<ParsedExpression> = self.transform_child(parse_result.list()?, 1)?;
        match bounds.len() {
            0 => Err(Error::parser("Empty subscript '[]' is not allowed")),
            1 => Ok(ParsedExpression::operator(ExpressionType::ArrayExtract, bounds)),
            _ => Ok(ParsedExpression::operator(ExpressionType::ArraySlice, bounds)),
        }
    }

    /// `TransformSliceBound`: the start, end and step of a slice; missing bounds are
    /// empty lists, which the execution handles.
    pub(super) fn transform_slice_bound(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        let list = parse_result.list()?;
        let start = list.optional(0)?;
        let end = list.optional(1)?;
        let step = list.optional(2)?;
        let mut bounds = Vec::new();
        if end.is_none() && step.is_none() {
            if let Some(start) = start {
                bounds.push(self.transform(start)?);
            }
            return Ok(bounds);
        }
        for bound in [start, end] {
            bounds.push(match bound {
                Some(bound) => self.transform(bound)?,
                None => empty_integer_list(),
            });
        }
        if let Some(step) = step {
            bounds.push(self.transform(step)?);
        }
        Ok(bounds)
    }

    /// `TransformEndSliceBound`: `:end`, `:-` or `:` (an empty list).
    pub(super) fn transform_end_slice_bound(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let Some(bound) = parse_result.list()?.optional(1)? else {
            return Ok(empty_integer_list());
        };
        let choice = bound.list()?.choice(0)?;
        match choice.result_type() {
            // The '-'.
            ParseResultType::Keyword => Ok(empty_integer_list()),
            ParseResultType::List => self.transform(choice),
            _ => Err(Error::internal("Unexpected parse result type encountered")),
        }
    }

    /// `TransformStepSliceBound`: `:step`.
    pub(super) fn transform_step_slice_bound(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        match parse_result.list()?.optional(1)? {
            Some(expression) => self.transform(expression),
            None => Ok(empty_integer_list()),
        }
    }

    /// `TransformTableReservedColumnName`: `table.column`.
    pub(super) fn transform_table_reserved_column_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ColumnRefExpression> {
        let list = parse_result.list()?;
        let table: String = self.transform_child(list, 0)?;
        let column = list.identifier(1)?.to_owned();
        Ok(ColumnRefExpression {
            column_names: vec![table, column],
        })
    }

    /// `TransformTableQualification`.
    pub(super) fn transform_table_qualification(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.identifier(0)?.to_owned())
    }

    /// `TransformStarExpression`: `[table.]* [EXCLUDE ...] [REPLACE ...] [RENAME ...]`.
    pub(super) fn transform_star_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut result = build::star_expression(String::new());
        if let Some(qualifiers) = list.optional(0)? {
            let qualifiers = qualifiers.repeat()?;
            if qualifiers.len() > 1 {
                return Err(Error::parser(
                    "Did not expect more than one column in front of a star expression",
                ));
            }
            if let Some(qualifier) = qualifiers.first() {
                result.relation_name = self.transform_child(qualifier.list()?, 0)?;
            }
        }
        // DuckDB keeps these in hash containers; rebuilding them in insertion order
        // reproduces their iteration order.
        let exclude: Vec<QualifiedColumnName> = self.transform_optional(list, 2)?.unwrap_or_default();
        let mut exclude_set: QualifiedColumnMap<()> = QualifiedColumnMap::default();
        for column in exclude {
            exclude_set.insert(column, ());
        }
        let mut replace_map = CiHashMap::new();
        if let Some(replace) = list.optional(3)? {
            let replace: CaseInsensitiveMap<ParsedExpression> = self.transform(replace)?;
            for (name, expression) in replace {
                replace_map.insert(name, expression);
            }
            for (name, _) in replace_map.iter() {
                if exclude_set.contains(&QualifiedColumnName::column(name)) {
                    return Err(Error::parser(format!(
                        "Column \"{name}\" cannot occur in both EXCLUDE and REPLACE list"
                    )));
                }
            }
        }
        let mut rename_map: QualifiedColumnMap<String> = QualifiedColumnMap::default();
        if let Some(rename) = list.optional(4)? {
            let rename: OrderedMap<QualifiedColumnName, String> = self.transform(rename)?;
            for (column, name) in rename {
                rename_map.insert(column, name);
            }
            let renames = rename_map.into_ordered();
            for (column, _) in &renames {
                if exclude_set.contains(column) {
                    return Err(Error::parser(format!(
                        "Column \"{column}\" cannot occur in both EXCLUDE and RENAME list"
                    )));
                }
                if replace_map.contains_key(&column.column) {
                    return Err(Error::parser(format!(
                        "Column \"{column}\" cannot occur in both REPLACE and RENAME list"
                    )));
                }
            }
            result.rename_list = renames.into_iter().collect();
        }
        result.exclude_list = rustdb_parser::serialized_exclude_list(exclude_set);
        result.replace_list = replace_map.into_ordered().into_iter().collect();
        Ok(ParsedExpression::new(
            ExpressionType::Star,
            ParsedExpressionKind::Star(Box::new(result)),
        ))
    }

    /// `TransformExcludeList`: `EXCLUDE name` / `EXCLUDE (names)`.
    pub(super) fn transform_exclude_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<QualifiedColumnName>> {
        self.transform(parse_result.list()?.choice(1)?)
    }

    /// `TransformExcludeNameList`: `(name, ...)`, without duplicates.
    pub(super) fn transform_exclude_name_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<QualifiedColumnName>> {
        let names = parse_result.list()?.child(0)?.parens_content()?.list_elements()?;
        let mut result: Vec<QualifiedColumnName> = Vec::with_capacity(names.len());
        for name in names {
            let column: QualifiedColumnName = self.transform(name)?;
            if result
                .iter()
                .any(|existing| qualified_column_equal(existing, &column))
            {
                return Err(Error::parser(format!(
                    "Duplicate entry \"{column}\" in EXCLUDE list"
                )));
            }
            result.push(column);
        }
        Ok(result)
    }

    /// `TransformExcludeNameSingle`.
    pub(super) fn transform_exclude_name_single(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<QualifiedColumnName>> {
        Ok(vec![self.transform_child(parse_result.list()?, 0)?])
    }

    /// `TransformExcludeName`: a (qualified) column name.
    pub(super) fn transform_exclude_name(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<QualifiedColumnName> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.name.eq_ignore_ascii_case("dottedidentifier") {
            let parts: Vec<String> = self.transform(choice)?;
            parse_qualified_column_name(&parts.join("."))
        } else if choice.name.eq_ignore_ascii_case("colidorstring") {
            let column: String = self.transform(choice)?;
            Ok(QualifiedColumnName::column(column))
        } else {
            Err(Error::internal("Unexpected option encountered for ExcludeName"))
        }
    }

    /// `TransformOverClause`: `OVER window`.
    pub(super) fn transform_over_clause(&mut self, parse_result: &ParseResult) -> Result<WindowExpr> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformWindowFrame`: a named window or a window definition.
    pub(super) fn transform_window_frame(&mut self, parse_result: &ParseResult) -> Result<WindowExpr> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.result_type() == ParseResultType::Identifier {
            return self.get_window_clause(choice.identifier()?);
        }
        self.transform(choice)
    }

    /// `TransformParensIdentifier`: `(name)`, a copy of a named window without a frame.
    pub(super) fn transform_parens_identifier(&mut self, parse_result: &ParseResult) -> Result<WindowExpr> {
        let name = parse_result.list()?.child(0)?.parens_content()?.identifier()?;
        let mut window = self.get_window_clause(name)?;
        let definition = window_mut(&mut window)?;
        if definition.start_expr.is_some()
            || definition.end_expr.is_some()
            || !is_window_frame_default(definition)
        {
            return Err(Error::parser(format!(
                "cannot copy window \"{name}\" because it has a frame clause"
            )));
        }
        Ok(window)
    }

    /// `TransformWindowFrameDefinition`.
    pub(super) fn transform_window_frame_definition(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowExpr> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformWindowFrameContentsParens`: `(contents)`.
    pub(super) fn transform_window_frame_contents_parens(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowExpr> {
        self.transform(parse_result.list()?.child(0)?.parens_content()?)
    }

    /// `TransformWindowFrameNameContentsParens`: `(name contents)`: the named window
    /// with the frame, ORDER BY and PARTITION BY given here.
    pub(super) fn transform_window_frame_name_contents_parens(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowExpr> {
        let inner = parse_result.list()?.child(0)?.parens_content()?.list()?;
        let window_name: String = self.transform_optional(inner, 0)?.unwrap_or_default();
        let lower_name = window_name.to_ascii_lowercase();
        if matches!(lower_name.as_str(), "partition" | "range" | "rows" | "groups") {
            return Err(Error::parser(format!("Invalid window name \"{window_name}\"")));
        }
        let mut contents: WindowExpr = self.transform_child(inner, 1)?;
        if window_name.is_empty() {
            return Ok(contents);
        }
        let mut copied = self.get_window_clause(&window_name)?;
        let copied_window = window_mut(&mut copied)?;
        if copied_window.start_expr.is_some()
            || copied_window.end_expr.is_some()
            || !is_window_frame_default(copied_window)
        {
            return Err(Error::parser(format!(
                "cannot copy window \"{window_name}\" because it has a frame clause"
            )));
        }
        let contents = window_mut(&mut contents)?;
        copied_window.start = contents.start;
        copied_window.end = contents.end;
        copied_window.exclude_clause = contents.exclude_clause;
        copied_window.start_expr = contents.start_expr.take();
        copied_window.end_expr = contents.end_expr.take();
        copied_window.offset_expr = contents.offset_expr.take();
        copied_window.default_expr = contents.default_expr.take();
        if !copied_window.orders.is_empty() && !contents.orders.is_empty() {
            return Err(Error::parser(format!(
                "Cannot override ORDER BY clause of window \"{window_name}\""
            )));
        }
        if copied_window.orders.is_empty() {
            copied_window.orders = std::mem::take(&mut contents.orders);
        }
        if !copied_window.partitions.is_empty() && !contents.partitions.is_empty() {
            return Err(Error::parser(format!(
                "Cannot override PARTITION BY clause of window \"{window_name}\""
            )));
        }
        if copied_window.partitions.is_empty() {
            copied_window.partitions = std::mem::take(&mut contents.partitions);
        }
        Ok(copied)
    }

    /// `TransformBaseWindowName`.
    pub(super) fn transform_base_window_name(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.identifier(0)?.to_owned())
    }

    /// `TransformWindowFrameContents`: `[PARTITION BY ...] [ORDER BY ...] [frame]`.
    pub(super) fn transform_window_frame_contents(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowExpr> {
        let list = parse_result.list()?;
        // A window to hold the modifiers.
        let mut result = new_window_expression(ExpressionType::WindowAggregate, "");
        let partitions: Option<Vec<ParsedExpression>> = self.transform_optional(list, 0)?;
        let orders: Option<Vec<OrderByNode>> = self.transform_optional(list, 1)?;
        let frame: Option<WindowFrame> = self.transform_optional(list, 2)?;
        let window = window_mut(&mut result)?;
        if let Some(partitions) = partitions {
            window.partitions = partitions;
        }
        if let Some(orders) = orders {
            for order in &orders {
                if order.expression.expression_type == ExpressionType::Star
                    && let ParsedExpressionKind::Star(star) = &order.expression.kind
                    && star.expr.is_none()
                {
                    return Err(Error::parser("Cannot ORDER BY ALL in a window expression"));
                }
            }
            window.orders = orders;
        }
        if let Some(frame) = frame {
            window.start = frame.start;
            window.end = frame.end;
            window.start_expr = frame.start_expr.map(Box::new);
            window.end_expr = frame.end_expr.map(Box::new);
            window.exclude_clause = frame.exclude_clause;
        } else {
            window.start = WindowBoundary::UnboundedPreceding;
            window.end = WindowBoundary::CurrentRowRange;
        }
        Ok(result)
    }

    /// `TransformFrameClause`: `ROWS / RANGE / GROUPS extent [EXCLUDE ...]`.
    pub(super) fn transform_frame_clause(&mut self, parse_result: &ParseResult) -> Result<WindowFrame> {
        let list = parse_result.list()?;
        let framing: String = self.transform_child(list, 0)?;
        let mut extent: Vec<WindowBoundaryExpression> = self.transform_child(list, 1)?;
        for frame in &mut extent {
            use WindowBoundary as B;
            frame.boundary = if framing.eq_ignore_ascii_case("rows") {
                match frame.boundary {
                    B::CurrentRowRange | B::Invalid => B::CurrentRowRows,
                    B::ExprPrecedingRange => B::ExprPrecedingRows,
                    B::ExprFollowingRange => B::ExprFollowingRows,
                    other => other,
                }
            } else if framing.eq_ignore_ascii_case("groups") {
                match frame.boundary {
                    B::CurrentRowRange | B::Invalid => B::CurrentRowGroups,
                    B::ExprPrecedingRange => B::ExprPrecedingGroups,
                    B::ExprFollowingRange => B::ExprFollowingGroups,
                    other => other,
                }
            } else if framing.eq_ignore_ascii_case("range") {
                match frame.boundary {
                    B::Invalid => B::CurrentRowRange,
                    other => other,
                }
            } else {
                return Err(Error::parser(format!("Invalid result from frame: {framing}")));
            };
        }
        let mut extent = extent.into_iter();
        let start = extent
            .next()
            .ok_or_else(|| Error::internal("A window frame without a start"))?;
        if start.boundary == WindowBoundary::UnboundedFollowing {
            return Err(Error::parser("Frame start cannot be UNBOUNDED FOLLOWING"));
        }
        let mut result = WindowFrame {
            start: start.boundary,
            end: WindowBoundary::Invalid,
            exclude_clause: WindowExcludeMode::NoOther,
            start_expr: start.expr,
            end_expr: None,
        };
        if let Some(end) = extent.next() {
            if end.boundary == WindowBoundary::UnboundedPreceding {
                return Err(Error::parser("Frame end cannot be UNBOUNDED PRECEDING"));
            }
            result.end = end.boundary;
            result.end_expr = end.expr;
        }
        if let Some(exclude_clause) = self.transform_optional(list, 2)? {
            result.exclude_clause = exclude_clause;
        }
        Ok(result)
    }

    /// `TransformFrameExtent`.
    pub(super) fn transform_frame_extent(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<WindowBoundaryExpression>> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformBetweenFrameExtent`: `BETWEEN start AND end`.
    pub(super) fn transform_between_frame_extent(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<WindowBoundaryExpression>> {
        let list = parse_result.list()?;
        Ok(vec![
            self.transform_child(list, 1)?,
            self.transform_child(list, 3)?,
        ])
    }

    /// `TransformSingleFrameExtent`: `start`, ending at the current row.
    pub(super) fn transform_single_frame_extent(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<WindowBoundaryExpression>> {
        let start: WindowBoundaryExpression = self.transform_child(parse_result.list()?, 0)?;
        let end_current_row = WindowBoundaryExpression {
            boundary: WindowBoundary::Invalid,
            expr: None,
        };
        Ok(vec![start, end_current_row])
    }

    /// `TransformFrameBound`.
    pub(super) fn transform_frame_bound(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowBoundaryExpression> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformFrameUnbounded`: `UNBOUNDED PRECEDING / FOLLOWING`.
    pub(super) fn transform_frame_unbounded(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowBoundaryExpression> {
        let preceding: bool = self.transform_child(parse_result.list()?, 1)?;
        Ok(WindowBoundaryExpression {
            boundary: if preceding {
                WindowBoundary::UnboundedPreceding
            } else {
                WindowBoundary::UnboundedFollowing
            },
            expr: None,
        })
    }

    /// `TransformFrameExpression`: `expr PRECEDING / FOLLOWING` (RANGE boundaries until
    /// the frame clause knows its unit).
    pub(super) fn transform_frame_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowBoundaryExpression> {
        let list = parse_result.list()?;
        let expr: ParsedExpression = self.transform_child(list, 0)?;
        let preceding: bool = self.transform_child(list, 1)?;
        Ok(WindowBoundaryExpression {
            boundary: if preceding {
                WindowBoundary::ExprPrecedingRange
            } else {
                WindowBoundary::ExprFollowingRange
            },
            expr: Some(expr),
        })
    }

    /// `TransformFrameCurrentRow`: `CURRENT ROW` (a RANGE boundary until the frame
    /// clause knows its unit).
    pub(super) fn transform_frame_current_row(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<WindowBoundaryExpression> {
        Ok(WindowBoundaryExpression {
            boundary: WindowBoundary::CurrentRowRange,
            expr: None,
        })
    }

    /// `TransformPrecedingOrFollowing`: whether `PRECEDING`.
    pub(super) fn transform_preceding_or_following(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Ok(parse_result
            .list()?
            .choice(0)?
            .keyword()?
            .eq_ignore_ascii_case("preceding"))
    }

    /// `TransformWindowExcludeClause`: `EXCLUDE ...`.
    pub(super) fn transform_window_exclude_clause(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowExcludeMode> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformFraming`: `ROWS`, `RANGE` or `GROUPS`.
    pub(super) fn transform_framing(&mut self, parse_result: &ParseResult) -> Result<String> {
        Ok(parse_result.list()?.choice(0)?.keyword()?.to_owned())
    }

    /// `TransformWindowExcludeElement`.
    pub(super) fn transform_window_exclude_element(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<WindowExcludeMode> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformWindowPartition`: `PARTITION BY expr, ...`.
    pub(super) fn transform_window_partition(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        self.transform_expression_list(parse_result.list()?.child(2)?)
    }

    /// The expressions of `List(Expression)`.
    fn transform_expression_list(&mut self, list: &ParseResult) -> Result<Vec<ParsedExpression>> {
        list.list_elements()?
            .into_iter()
            .map(|element| self.transform(element))
            .collect()
    }

    /// `TransformSpecialFunctionExpression`.
    pub(super) fn transform_special_function_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformCoalesceExpression`: `COALESCE(a, b, ...)`.
    pub(super) fn transform_coalesce_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let children = self.transform_expression_list(parse_result.list()?.child(1)?.parens_content()?)?;
        Ok(ParsedExpression::operator(
            ExpressionType::OperatorCoalesce,
            children,
        ))
    }

    /// `TransformUnpackExpression`: `UNPACK(expr)`.
    pub(super) fn transform_unpack_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let child: ParsedExpression = self.transform(parse_result.list()?.child(1)?.parens_content()?)?;
        Ok(unary_operator(ExpressionType::OperatorUnpack, child))
    }

    /// `TransformTryExpression`: `TRY(expr)`.
    pub(super) fn transform_try_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let child: ParsedExpression = self.transform(parse_result.list()?.child(1)?.parens_content()?)?;
        Ok(unary_operator(ExpressionType::OperatorTry, child))
    }

    /// `TransformColumnsExpression`: `[*]COLUMNS(expr)`.
    pub(super) fn transform_columns_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let unpack = list.has(0)?;
        let expr: ParsedExpression = self.transform(list.child(2)?.parens_content()?)?;
        let mut result = ParsedExpression::new(
            ExpressionType::Star,
            ParsedExpressionKind::Star(Box::new(build::star_expression(String::new()))),
        );
        let star_columns = match &expr.kind {
            ParsedExpressionKind::Star(star) if expr.expression_type == ExpressionType::Star => {
                Some(star.columns)
            }
            _ => None,
        };
        let inner = match star_columns {
            // COLUMNS(*) itself: the star expression is the result.
            Some(false) => {
                result = expr;
                None
            }
            Some(true) => Some(expr),
            None if expr.expression_type == ExpressionType::Lambda => {
                Some(build::function("list_filter", vec![build::star(), expr]))
            }
            None => Some(expr),
        };
        if let ParsedExpressionKind::Star(star) = &mut result.kind {
            if let Some(inner) = inner {
                star.expr = Some(Box::new(inner));
            }
            star.columns = true;
        }
        if unpack {
            return Ok(unary_operator(ExpressionType::OperatorUnpack, result));
        }
        Ok(result)
    }

    /// `TransformExtractExpression`: `EXTRACT(part FROM expr)` is `date_part(part, expr)`.
    pub(super) fn transform_extract_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let arguments = parse_result.list()?.child(1)?.parens_content()?.list()?;
        let part: ParsedExpression = self.transform_child(arguments, 0)?;
        let expr: ParsedExpression = self.transform_child(arguments, 2)?;
        Ok(qualified_function("", "main", "date_part", vec![part, expr]))
    }

    /// `TransformExtractArgument`: the date part, as a string constant.
    pub(super) fn transform_extract_argument(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let choice = parse_result.list()?.choice(0)?;
        match choice.result_type() {
            ParseResultType::Identifier => Ok(build::varchar_constant(choice.identifier()?)),
            ParseResultType::String => Ok(build::varchar_constant(choice.string_literal()?.0)),
            _ => {
                let date_part: DatePartSpecifier = Self::transform_enum(choice)?;
                Ok(build::varchar_constant(date_part.enum_name()))
            }
        }
    }

    /// `TransformLambdaExpression`: `lambda x, y: expr`.
    pub(super) fn transform_lambda_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let parameters = list
            .child(1)?
            .list_elements()?
            .into_iter()
            .map(|parameter| self.transform(parameter))
            .collect::<Result<Vec<String>>>()?;
        let expr: ParsedExpression = self.transform_child(list, 3)?;
        Ok(keyword_lambda(&parameters, expr))
    }

    /// `TransformNullIfExpression`: `NULLIF(a, b)`.
    pub(super) fn transform_null_if_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let arguments = parse_result.list()?.child(1)?.parens_content()?.list()?;
        let left: ParsedExpression = self.transform_child(arguments, 0)?;
        let right: ParsedExpression = self.transform_child(arguments, 2)?;
        Ok(build::function("nullif", vec![left, right]))
    }

    /// `TransformRowExpression`: `ROW(a, b, ...)`.
    pub(super) fn transform_row_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let children = match parse_result.list()?.child(1)?.parens_content()?.optional()? {
            Some(elements) => self.transform_expression_list(elements)?,
            None => Vec::new(),
        };
        Ok(qualified_function("", "main", "row", children))
    }

    /// `TransformSubstringExpression`: `SUBSTRING(...)`.
    pub(super) fn transform_substring_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let arguments: Vec<ParsedExpression> =
            self.transform(parse_result.list()?.child(1)?.parens_content()?)?;
        Ok(qualified_function("", "main", "substring", arguments))
    }

    /// `TransformSubstringArguments`.
    pub(super) fn transform_substring_arguments(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformSubstringExpressionList`: `SUBSTRING(a, b, c)`.
    pub(super) fn transform_substring_expression_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        self.transform_expression_list(parse_result.list()?.child(0)?)
    }

    /// `TransformSubstringParameters`: `SUBSTRING(a FROM b FOR c)`.
    pub(super) fn transform_substring_parameters(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        let list = parse_result.list()?;
        Ok(vec![
            self.transform(list.child(0)?)?,
            self.transform(list.child(2)?)?,
            self.transform(list.child(4)?)?,
        ])
    }

    /// `TransformTrimExpression`: `TRIM([BOTH | LEADING | TRAILING] [chars] FROM expr)`.
    pub(super) fn transform_trim_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let inner = parse_result.list()?.child(1)?.parens_content()?.list()?;
        let function_name: String = self
            .transform_optional(inner, 0)?
            .unwrap_or_else(|| "trim".to_owned());
        let mut children = self.transform_expression_list(inner.child(2)?)?;
        if let Some(source) = inner.optional(1)?
            && let Some(source) = self.transform_trim_source_expression(source)?
        {
            children.push(source);
        }
        Ok(qualified_function("", "main", &function_name, children))
    }

    /// `TransformTrimDirection`.
    pub(super) fn transform_trim_direction(&mut self, parse_result: &ParseResult) -> Result<String> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }

    /// `TransformTrimSource` as `TransformTrimExpression` sees it: the expression, if
    /// any (DuckDB returns `nullptr` without one).
    fn transform_trim_source_expression(&mut self, source: &ParseResult) -> Result<Option<ParsedExpression>> {
        if source.name != "TrimSource" {
            return self.transform(source).map(Some);
        }
        source
            .list()?
            .optional(0)?
            .map(|expression| self.transform(expression))
            .transpose()
    }

    /// `TransformTrimSource`: the characters to trim.
    pub(super) fn transform_trim_source(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform_trim_source_expression(parse_result)?
            .ok_or_else(|| Error::internal("TRIM source without an expression"))
    }

    /// `TransformPositionExpression`: `POSITION(a IN b)` is `position(b, a)`.
    pub(super) fn transform_position_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let values = parse_result.list()?.child(1)?.parens_content()?.list()?;
        let string: ParsedExpression = self.transform_child(values, 2)?;
        let search: ParsedExpression = self.transform_child(values, 0)?;
        Ok(build::function("position", vec![string, search]))
    }

    /// `TransformCastExpression`: `CAST(expr AS type)` / `TRY_CAST(...)`.
    pub(super) fn transform_cast_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let try_cast: bool = self.transform_child(list, 0)?;
        let arguments = list.child(1)?.parens_content()?.list()?;
        let expr: ParsedExpression = self.transform_child(arguments, 0)?;
        let ty: LogicalType = self.transform_child(arguments, 2)?;
        Ok(build::cast(ty, expr, try_cast))
    }

    /// `TransformCastOrTryCast`: whether `TRY_CAST`.
    pub(super) fn transform_cast_or_try_cast(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Ok(parse_result
            .list()?
            .choice(0)?
            .keyword()?
            .eq_ignore_ascii_case("try_cast"))
    }

    /// `TransformCaseExpression`: `CASE [expr] WHEN ... THEN ... [ELSE ...] END`.
    pub(super) fn transform_case_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let operand: Option<ParsedExpression> = self.transform_optional(list, 1)?;
        let mut checks = Vec::new();
        for case in list.repeat(2)? {
            let mut check: CaseCheck = self.transform(case)?;
            if let Some(operand) = &operand {
                check.when_expr = Box::new(ParsedExpression::comparison(
                    ExpressionType::CompareEqual,
                    operand.clone(),
                    *check.when_expr,
                ));
            }
            checks.push(check);
        }
        let else_expr = match list.optional(3)? {
            Some(else_expr) => self.transform(else_expr)?,
            None => ParsedExpression::constant(Value::default()),
        };
        Ok(build::case_expression(checks, else_expr))
    }

    /// `TransformCaseElse`: `ELSE expr`.
    pub(super) fn transform_case_else(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformCaseWhenThen`: `WHEN a THEN b`.
    pub(super) fn transform_case_when_then(&mut self, parse_result: &ParseResult) -> Result<CaseCheck> {
        let list = parse_result.list()?;
        let when: ParsedExpression = self.transform_child(list, 1)?;
        let then: ParsedExpression = self.transform_child(list, 3)?;
        Ok(CaseCheck {
            when_expr: Box::new(when),
            then_expr: Box::new(then),
        })
    }

    /// `TransformTypeLiteral`: `type 'string'` is `CAST('string' AS type)`.
    pub(super) fn transform_type_literal(&mut self, parse_result: &ParseResult) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let name: String = self.transform_child(list, 0)?;
        let ty = match rustdb_types::builtin_type_id(&name) {
            Some(id) => LogicalType::simple(id),
            // A user type, which the binder resolves.
            None => unbound_type(simple_type_expression(&name, Vec::new()))?,
        };
        let text = list.child(1)?.string_literal()?.0;
        Ok(build::cast(ty, build::varchar_constant(text), false))
    }

    /// `TransformDefaultExpression`: `DEFAULT`.
    pub(super) fn transform_default_expression(
        &mut self,
        _parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        Ok(ParsedExpression::new(
            ExpressionType::ValueDefault,
            ParsedExpressionKind::Default,
        ))
    }

    /// `TransformIntervalLiteral`: `INTERVAL value [unit]`: a cast to INTERVAL, or the
    /// unit's `to_*` function of the value.
    pub(super) fn transform_interval_literal(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let unit: Option<DatePartSpecifier> = self.transform_optional(list, 2)?;
        let expr: ParsedExpression = self.transform_child(list, 1)?;
        let Some(unit) = unit else {
            return Ok(build::cast(LogicalType::INTERVAL, expr, false));
        };
        let mut expr = build::cast(LogicalType::DOUBLE, expr, false);
        let target_type = unit.interval_target_type();
        if target_type != LogicalType::DOUBLE {
            expr = build::cast(target_type, build::function("trunc", vec![expr]), false);
        }
        Ok(build::function(date_part_to_function(unit), vec![expr]))
    }

    /// `TransformIntervalParameter`: a string or an expression.
    pub(super) fn transform_interval_parameter(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let choice = parse_result.list()?.choice(0)?;
        if choice.result_type() == ParseResultType::String {
            return Ok(build::varchar_constant(choice.string_literal()?.0));
        }
        self.transform(choice)
    }

    /// `TransformSubqueryExpression`: `[NOT] [EXISTS] (subquery)`; a table reference
    /// other than a subquery is `SELECT * FROM ref`.
    pub(super) fn transform_subquery_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let is_not = list.has(0)?;
        let is_exists = list.has(1)?;
        let reference: TableRef = self.transform_child(list, 2)?;
        let subquery_type = if is_exists {
            SubqueryType::Exists
        } else {
            SubqueryType::Scalar
        };
        let subquery = match reference.kind {
            TableRefKind::Subquery(subquery_ref) => *subquery_ref.subquery,
            _ => {
                let mut select_node = build::select_node();
                select_node.select_list.push(build::star());
                select_node.from_table = Box::new(reference);
                build::select_statement(build::query_node(QueryNodeKind::Select(Box::new(select_node))))
            }
        };
        let result = subquery_expression(subquery_type, subquery, None, ExpressionType::Invalid);
        if is_not {
            return Ok(unary_operator(ExpressionType::OperatorNot, result));
        }
        Ok(result)
    }

    /// `TransformMapExpression`: `MAP {k: v, ...}` is `map([k, ...], [v, ...])`.
    pub(super) fn transform_map_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let children: Vec<ParsedExpression> = self.transform_child(parse_result.list()?, 1)?;
        Ok(qualified_function("", "main", "map", children))
    }

    /// `TransformMapStructExpression`: the key list and the value list.
    pub(super) fn transform_map_struct_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        let mut keys = Vec::new();
        let mut values = Vec::new();
        if let Some(fields) = parse_result.list()?.optional(1)? {
            for field in fields.list_elements()? {
                let pair: Vec<ParsedExpression> = self.transform(field)?;
                let [key, value]: [ParsedExpression; 2] = pair
                    .try_into()
                    .map_err(|_| Error::internal("A map entry is not a key and a value"))?;
                keys.push(key);
                values.push(value);
            }
        }
        Ok(vec![
            qualified_function("", "main", "list_value", keys),
            qualified_function("", "main", "list_value", values),
        ])
    }

    /// `TransformMapStructField`: `key: value`.
    pub(super) fn transform_map_struct_field(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<Vec<ParsedExpression>> {
        let list = parse_result.list()?;
        Ok(vec![
            self.transform_child(list, 0)?,
            self.transform_child(list, 2)?,
        ])
    }

    /// `TransformListComprehensionExpression`: `[expr FOR x IN list [IF cond]]` is
    /// `list_apply(list, lambda x: expr)`; with a condition, the elements are computed
    /// with their condition, filtered, then unpacked.
    pub(super) fn transform_list_comprehension_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let list = parse_result.list()?;
        let mut result_expr: ParsedExpression = self.transform_child(list, 1)?;
        let columns = list
            .child(3)?
            .list_elements()?
            .into_iter()
            .map(|column| self.transform(column))
            .collect::<Result<Vec<String>>>()?;
        let in_expr: ParsedExpression = self.transform_child(list, 5)?;
        let Some(filter) = list.optional(6)? else {
            let lambda = keyword_lambda(&columns, result_expr);
            return Ok(qualified_function(
                "",
                "main",
                "list_apply",
                vec![in_expr, lambda],
            ));
        };
        let mut filter_expr: ParsedExpression = self.transform(filter)?;
        // Stage 1: list_apply(list, x -> struct_pack(filter := ..., result := ...)).
        "filter".clone_into(&mut filter_expr.alias);
        "result".clone_into(&mut result_expr.alias);
        let struct_pack = qualified_function("", "main", "struct_pack", vec![filter_expr, result_expr]);
        let stage1 = qualified_function(
            "",
            "main",
            "list_apply",
            vec![in_expr, keyword_lambda(&columns, struct_pack)],
        );
        // Stage 2: list_filter(stage1, elem -> struct_extract(elem, 'filter')).
        let element = || vec!["elem".to_owned()];
        let extract = |field: &str| {
            qualified_function(
                "",
                "main",
                "struct_extract",
                vec![build::column_ref(element()), build::varchar_constant(field)],
            )
        };
        let stage2 = qualified_function(
            "",
            "main",
            "list_filter",
            vec![stage1, keyword_lambda(&element(), extract("filter"))],
        );
        // Stage 3: list_apply(stage2, elem -> struct_extract(elem, 'result')).
        Ok(qualified_function(
            "",
            "main",
            "list_apply",
            vec![stage2, keyword_lambda(&element(), extract("result"))],
        ))
    }

    /// `TransformListComprehensionFilter`: `IF cond`.
    pub(super) fn transform_list_comprehension_filter(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformReplaceList`: `REPLACE ...`.
    pub(super) fn transform_replace_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression>> {
        self.transform_child(parse_result.list()?, 1)
    }

    /// `TransformReplaceEntries`.
    pub(super) fn transform_replace_entries(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression>> {
        self.transform(parse_result.list()?.choice(0)?)
    }

    /// `TransformReplaceEntrySingle`: `expr AS name`.
    pub(super) fn transform_replace_entry_single(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression>> {
        let (name, expression): (String, ParsedExpression) = self.transform_child(parse_result.list()?, 0)?;
        let mut result = CaseInsensitiveMap::default();
        result.insert(&name, expression);
        Ok(result)
    }

    /// `TransformReplaceEntryList`: `(expr AS name, ...)`, without duplicates.
    pub(super) fn transform_replace_entry_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<CaseInsensitiveMap<ParsedExpression>> {
        let entries = parse_result.list()?.child(0)?.parens_content()?.list_elements()?;
        let mut result = CaseInsensitiveMap::default();
        for entry in entries {
            let (name, expression): (String, ParsedExpression) = self.transform(entry)?;
            if result.contains_key(&name) {
                return Err(Error::parser(format!(
                    "Duplicate entry \"{name}\" in REPLACE list"
                )));
            }
            result.insert(&name, expression);
        }
        Ok(result)
    }

    /// `TransformReplaceEntry`: `expr AS column`.
    pub(super) fn transform_replace_entry(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(String, ParsedExpression)> {
        let list = parse_result.list()?;
        let expression: ParsedExpression = self.transform_child(list, 0)?;
        let column: ParsedExpression = self.transform_child(list, 2)?;
        let ParsedExpressionKind::ColumnRef(column_ref) = column.kind else {
            return Err(Error::internal(
                "Expected a column reference in the replace entry",
            ));
        };
        let name = column_ref.column_names.last().cloned().unwrap_or_default();
        Ok((name, expression))
    }

    /// `TransformIsDistinctFromOp`: `IS [NOT] DISTINCT FROM`.
    pub(super) fn transform_is_distinct_from_op(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ExpressionType> {
        Ok(if parse_result.list()?.has(1)? {
            ExpressionType::CompareNotDistinctFrom
        } else {
            ExpressionType::CompareDistinctFrom
        })
    }

    /// `TransformGroupingExpression`: `GROUPING(a, b, ...)`.
    pub(super) fn transform_grouping_expression(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<ParsedExpression> {
        let children = self.transform_expression_list(parse_result.list()?.child(1)?.parens_content()?)?;
        Ok(ParsedExpression::operator(
            ExpressionType::GroupingFunction,
            children,
        ))
    }

    /// `TransformRenameList`: `RENAME ...`.
    pub(super) fn transform_rename_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OrderedMap<QualifiedColumnName, String>> {
        self.transform(parse_result.list()?.list(1)?.choice(0)?)
    }

    /// `TransformRenameEntryList`: `(column AS name, ...)`; a column renamed twice keeps
    /// its last name.
    pub(super) fn transform_rename_entry_list(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OrderedMap<QualifiedColumnName, String>> {
        let entries = parse_result.list()?.child(0)?.parens_content()?.list_elements()?;
        let mut result: Vec<(QualifiedColumnName, String)> = Vec::new();
        for entry in entries {
            let (column, name): (QualifiedColumnName, String) = self.transform(entry)?;
            match result
                .iter_mut()
                .find(|(existing, _)| qualified_column_equal(existing, &column))
            {
                Some((_, existing_name)) => *existing_name = name,
                None => result.push((column, name)),
            }
        }
        Ok(result.into_iter().collect())
    }

    /// `TransformSingleRenameEntry`: `column AS name`.
    pub(super) fn transform_single_rename_entry(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<OrderedMap<QualifiedColumnName, String>> {
        let (column, name): (QualifiedColumnName, String) = self.transform(parse_result.list()?.child(0)?)?;
        Ok(std::iter::once((column, name)).collect())
    }

    /// `TransformRenameEntry`: `column AS name`.
    pub(super) fn transform_rename_entry(
        &mut self,
        parse_result: &ParseResult,
    ) -> Result<(QualifiedColumnName, String)> {
        let list = parse_result.list()?;
        let column: QualifiedColumnName = self.transform(list.child(0)?)?;
        let alias = list.identifier(2)?.to_owned();
        Ok((column, alias))
    }

    /// `TransformIgnoreOrRespectNulls`: whether `IGNORE NULLS`.
    pub(super) fn transform_ignore_or_respect_nulls(&mut self, parse_result: &ParseResult) -> Result<bool> {
        Self::transform_enum(parse_result.list()?.choice(0)?)
    }
}

/// `ParameterExpression` with `identifier`.
fn parameter_expression(identifier: String) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::ValueParameter,
        ParsedExpressionKind::Parameter(ParameterExpression { identifier }),
    )
}

/// `ConstantExpression(Value::LIST(INTEGER, {}))`: a missing slice bound.
fn empty_integer_list() -> ParsedExpression {
    ParsedExpression::constant(Value::list(LogicalType::INTEGER, Vec::new()))
}

/// An empty SELECT, the remains of a subquery moved elsewhere.
fn empty_select_statement() -> SelectStatement {
    build::select_statement(build::query_node(QueryNodeKind::Select(Box::new(
        build::select_node(),
    ))))
}
