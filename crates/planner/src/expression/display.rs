//! Native byte rendering of bound expressions (DuckDB's `Expression::ToString`).
//! The legacy Rust Display interface is a checked UTF-8 view of this representation.

use std::fmt;

use rustdb_ast::{
    ExpressionType, OrderByNullType, OrderType, WindowBoundary, WindowExcludeMode,
    expression_type_to_operator, expression_type_to_string, value_to_sql_string,
};
use rustdb_common::ByteString;

use super::{BoundOrderByNode, Expression, ExpressionKind};

macro_rules! bytes {
    ($($part:expr),* $(,)?) => {{
        let mut out = ByteString::new();
        $(out.push_bytes($part);)*
        out
    }};
}

fn join(expressions: &[Expression], separator: &str) -> ByteString {
    let mut out = ByteString::new();
    for (i, expression) in expressions.iter().enumerate() {
        if i > 0 {
            out.push_bytes(separator);
        }
        out.push_bytes(expression.to_bytes());
    }
    out
}

/// Native `expr, expr, ...` bytes.
pub fn expression_list_to_string(expressions: &[Expression]) -> ByteString {
    join(expressions, ", ")
}

impl BoundOrderByNode {
    pub fn to_bytes(&self) -> ByteString {
        let mut out = self.expression.to_bytes();
        match self.order_type {
            OrderType::Ascending => out.push_bytes(" ASC"),
            OrderType::Descending => out.push_bytes(" DESC"),
            OrderType::Invalid | OrderType::OrderDefault => {}
        }
        match self.null_order {
            OrderByNullType::NullsFirst => out.push_bytes(" NULLS FIRST"),
            OrderByNullType::NullsLast => out.push_bytes(" NULLS LAST"),
            OrderByNullType::Invalid | OrderByNullType::OrderDefault => {}
        }
        out
    }
}

impl fmt::Display for BoundOrderByNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.to_bytes();
        f.write_str(bytes.to_str().map_err(|_| fmt::Error)?)
    }
}

fn order_list(orders: &[BoundOrderByNode]) -> ByteString {
    let mut out = ByteString::new();
    for (i, order) in orders.iter().enumerate() {
        if i > 0 {
            out.push_bytes(", ");
        }
        out.push_bytes(order.to_bytes());
    }
    out
}

fn function_call(
    name: &[u8],
    children: &[Expression],
    is_operator: bool,
    distinct: bool,
    filter: Option<&Expression>,
    orders: &[BoundOrderByNode],
) -> ByteString {
    if is_operator {
        match children {
            [child] => {
                return if name.windows(9).any(|part| part == b"__postfix") {
                    let mut operator = ByteString::new();
                    let mut remaining = name;
                    while let Some(index) = remaining.windows(9).position(|part| part == b"__postfix") {
                        operator.push_bytes(&remaining[..index]);
                        remaining = &remaining[index + 9..];
                    }
                    operator.push_bytes(remaining);
                    bytes!("((", child.to_bytes(), ")", operator, ")")
                } else {
                    bytes!(name, "(", child.to_bytes(), ")")
                };
            }
            [left, right] => return bytes!("(", left.to_bytes(), " ", name, " ", right.to_bytes(), ")"),
            _ => {}
        }
    }
    let mut out = rustdb_ast::keyword::write_optionally_quoted_bytes(name);
    out.push(b'(');
    if distinct {
        out.push_bytes("DISTINCT ");
    }
    out.push_bytes(join(children, ", "));
    if !orders.is_empty() {
        if children.is_empty() {
            out.push_bytes(") WITHIN GROUP (");
        }
        out.push_bytes(" ORDER BY ");
        out.push_bytes(order_list(orders));
    }
    out.push(b')');
    if let Some(filter) = filter {
        out.push_bytes(bytes!(" FILTER (WHERE ", filter.to_bytes(), ")"));
    }
    out
}

fn operator_text(expression: &Expression, children: &[Expression]) -> ByteString {
    let ty = expression.expression_type;
    let child = |i: usize| children.get(i).map(Expression::to_bytes).unwrap_or_default();
    let infix = expression_type_to_operator(ty);
    if !infix.is_empty() {
        return bytes!(child(0), " ", infix, " ", child(1));
    }
    match ty {
        ExpressionType::CompareIn | ExpressionType::CompareNotIn => bytes!(
            "(",
            child(0),
            if ty == ExpressionType::CompareIn {
                " IN "
            } else {
                " NOT IN "
            },
            "(",
            join(children.get(1..).unwrap_or_default(), ", "),
            "))"
        ),
        ExpressionType::OperatorUnpack => bytes!("UNPACK(", child(0), ")"),
        ExpressionType::OperatorTry => bytes!("TRY(", child(0), ")"),
        ExpressionType::OperatorNot => {
            bytes!("(", expression_type_to_string(ty), " ", join(children, ", "), ")")
        }
        ExpressionType::GroupingFunction | ExpressionType::OperatorCoalesce => {
            bytes!(expression_type_to_string(ty), "(", join(children, ", "), ")")
        }
        ExpressionType::OperatorIsNull => bytes!("(", child(0), " IS NULL)"),
        ExpressionType::OperatorIsNotNull => bytes!("(", child(0), " IS NOT NULL)"),
        ExpressionType::ArrayExtract => bytes!(child(0), "[", child(1), "]"),
        ExpressionType::ArrayConstructor => bytes!("(ARRAY[", join(children, ", "), "])"),
        ExpressionType::StructExtract => {
            let field = child(1);
            let inner = field.get(1..field.len().saturating_sub(1)).unwrap_or_default();
            bytes!(
                "(",
                child(0),
                ").",
                rustdb_ast::keyword::sql_identifier_bytes(inner)
            )
        }
        _ => ByteString::new(),
    }
}

fn units(boundary: WindowBoundary, rows: WindowBoundary, range: WindowBoundary) -> &'static str {
    if boundary == rows {
        "ROWS"
    } else if boundary == range {
        "RANGE"
    } else {
        "GROUPS"
    }
}

fn window_text(expression: &Expression, window: &super::BoundWindowExpression) -> ByteString {
    use WindowBoundary as B;
    let name = window.aggregate.as_ref().map_or_else(
        || expression_type_to_string(expression.expression_type).to_owned(),
        |aggregate| aggregate.name.to_string(),
    );
    let mut result = bytes!(name, "(");
    for (i, child) in window.children.iter().enumerate() {
        if i > 0 {
            result.push_bytes(", ");
        }
        if window.distinct && i == 0 {
            result.push_bytes("DISTINCT ");
        }
        result.push_bytes(child.to_bytes());
    }
    for extra in [&window.offset_expr, &window.default_expr].into_iter().flatten() {
        result.push_bytes(bytes!(", ", extra.to_bytes()));
    }
    if !window.arg_orders.is_empty() {
        result.push_bytes(bytes!(" ORDER BY ", order_list(&window.arg_orders)));
    }
    if window.ignore_nulls {
        result.push_bytes(" IGNORE NULLS");
    }
    if let Some(filter) = &window.filter_expr {
        result.push_bytes(bytes!(") FILTER (WHERE ", filter.to_bytes()));
    }
    result.push_bytes(") OVER (");
    let mut separator = "";
    if !window.partitions.is_empty() {
        result.push_bytes(bytes!("PARTITION BY ", join(&window.partitions, ", ")));
        separator = " ";
    }
    if !window.orders.is_empty() {
        result.push_bytes(bytes!(separator, "ORDER BY ", order_list(&window.orders)));
        separator = " ";
    }
    let text = |expr: &Option<Expression>| expr.as_ref().map(Expression::to_bytes).unwrap_or_default();
    let mut unit = "ROWS";
    let mut from = ByteString::new();
    match window.start {
        B::CurrentRowRange | B::CurrentRowRows | B::CurrentRowGroups => {
            from.push_bytes("CURRENT ROW");
            unit = units(window.start, B::CurrentRowRows, B::CurrentRowRange);
        }
        B::UnboundedPreceding if window.end != B::CurrentRowRange => from.push_bytes("UNBOUNDED PRECEDING"),
        B::ExprPrecedingRows | B::ExprPrecedingRange | B::ExprPrecedingGroups => {
            from.push_bytes(bytes!(text(&window.start_expr), " PRECEDING"));
            unit = units(window.start, B::ExprPrecedingRows, B::ExprPrecedingRange);
        }
        B::ExprFollowingRows | B::ExprFollowingRange | B::ExprFollowingGroups => {
            from.push_bytes(bytes!(text(&window.start_expr), " FOLLOWING"));
            unit = units(window.start, B::ExprFollowingRows, B::ExprFollowingRange);
        }
        _ => {}
    }
    let mut to = ByteString::new();
    match window.end {
        B::CurrentRowRange if window.start != B::UnboundedPreceding => {
            to.push_bytes("CURRENT ROW");
            unit = "RANGE";
        }
        B::CurrentRowRows | B::CurrentRowGroups => {
            to.push_bytes("CURRENT ROW");
            unit = units(window.end, B::CurrentRowRows, B::CurrentRowRange);
        }
        B::UnboundedPreceding => to.push_bytes("UNBOUNDED PRECEDING"),
        B::UnboundedFollowing => to.push_bytes("UNBOUNDED FOLLOWING"),
        B::ExprPrecedingRows | B::ExprPrecedingRange | B::ExprPrecedingGroups => {
            to.push_bytes(bytes!(text(&window.end_expr), " PRECEDING"));
            unit = units(window.end, B::ExprPrecedingRows, B::ExprPrecedingRange);
        }
        B::ExprFollowingRows | B::ExprFollowingRange | B::ExprFollowingGroups => {
            to.push_bytes(bytes!(text(&window.end_expr), " FOLLOWING"));
            unit = units(window.end, B::ExprFollowingRows, B::ExprFollowingRange);
        }
        _ => {}
    }
    if window.exclude_clause != WindowExcludeMode::NoOther {
        if from.is_empty() {
            from.push_bytes("UNBOUNDED PRECEDING");
        }
        if to.is_empty() {
            to.push_bytes("CURRENT ROW");
            unit = "RANGE";
        }
    }
    if !from.is_empty() || !to.is_empty() {
        result.push_bytes(bytes!(separator, unit));
    }
    match (from.is_empty(), to.is_empty()) {
        (false, false) => result.push_bytes(bytes!(" BETWEEN ", from, " AND ", to)),
        (false, true) => result.push_bytes(bytes!(" ", from)),
        (true, false) => result.push_bytes(bytes!(" ", to)),
        (true, true) => {}
    }
    match window.exclude_clause {
        WindowExcludeMode::NoOther => {}
        WindowExcludeMode::CurrentRow => result.push_bytes(" EXCLUDE CURRENT ROW"),
        WindowExcludeMode::Group => result.push_bytes(" EXCLUDE GROUP"),
        WindowExcludeMode::Ties => result.push_bytes(" EXCLUDE TIES"),
    }
    result.push(b')');
    result
}

impl Expression {
    /// The byte-preserving native `Expression::ToString` representation.
    pub fn to_bytes(&self) -> ByteString {
        match &self.kind {
            ExpressionKind::Aggregate(aggregate) => function_call(
                aggregate.function.name.as_bytes(),
                &aggregate.children,
                false,
                aggregate.is_distinct(),
                aggregate.filter.as_ref(),
                aggregate
                    .order_bys
                    .as_ref()
                    .map_or(&[][..], |order| &order.orders),
            ),
            ExpressionKind::Between(between) => bytes!(
                "(",
                between.input.to_bytes(),
                " BETWEEN ",
                between.lower.to_bytes(),
                " AND ",
                between.upper.to_bytes(),
                ")"
            ),
            ExpressionKind::Case(case) => {
                let mut text = ByteString::from("CASE ");
                for check in &case.case_checks {
                    text.push_bytes(bytes!(
                        " WHEN (",
                        check.when_expr.to_bytes(),
                        ") THEN (",
                        check.then_expr.to_bytes(),
                        ")"
                    ));
                }
                text.push_bytes(bytes!(" ELSE ", case.else_expr.to_bytes(), " END"));
                text
            }
            ExpressionKind::Cast(cast) => bytes!(
                if cast.try_cast { "TRY_CAST(" } else { "CAST(" },
                cast.child.name(),
                " AS ",
                self.return_type.to_string(),
                ")"
            ),
            ExpressionKind::ColumnRef(column) => {
                if self.alias.is_empty() {
                    column.binding.to_string().into()
                } else {
                    self.alias.clone()
                }
            }
            ExpressionKind::Comparison(comparison) => bytes!(
                "(",
                comparison.left.to_bytes(),
                " ",
                expression_type_to_operator(self.expression_type),
                " ",
                comparison.right.to_bytes(),
                ")"
            ),
            ExpressionKind::Conjunction(conjunction) => bytes!(
                "(",
                join(
                    &conjunction.children,
                    &format!(" {} ", expression_type_to_operator(self.expression_type))
                ),
                ")"
            ),
            ExpressionKind::Constant(constant) => value_to_sql_string(&constant.value),
            ExpressionKind::Default => "DEFAULT".into(),
            ExpressionKind::Function(function) => function_call(
                function.function.name.as_bytes(),
                &function.children,
                function.is_operator,
                false,
                None,
                &[],
            ),
            ExpressionKind::Lambda(lambda) => lambda.lambda_expr.to_bytes(),
            ExpressionKind::LambdaRef(lambda) => {
                if self.alias.is_empty() {
                    format!(
                        "#[{}.{}.{}]",
                        lambda.binding.table_index, lambda.binding.column_index, lambda.lambda_index
                    )
                    .into()
                } else {
                    self.alias.clone()
                }
            }
            ExpressionKind::Operator(operator) => operator_text(self, &operator.children),
            ExpressionKind::Parameter(parameter) => rustdb_common::byte_format!("${}", parameter.identifier),
            ExpressionKind::Reference(reference) => {
                if self.alias.is_empty() {
                    format!("#{}", reference.index).into()
                } else {
                    self.alias.clone()
                }
            }
            ExpressionKind::Subquery(_) => "SUBQUERY".into(),
            ExpressionKind::Unnest(unnest) => bytes!("UNNEST(", unnest.child.to_bytes(), ")"),
            ExpressionKind::Window(window) => window_text(self, window),
            ExpressionKind::Expanded(_) => "BOUND_EXPANDED".into(),
        }
    }
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.to_bytes();
        f.write_str(bytes.to_str().map_err(|_| fmt::Error)?)
    }
}

impl rustdb_common::byte_format::NativeByteFormat for Expression {
    fn append_native_bytes(&self, output: &mut ByteString) {
        output.push_bytes(self.to_bytes());
    }
}

impl rustdb_common::byte_format::NativeByteFormat for BoundOrderByNode {
    fn append_native_bytes(&self, output: &mut ByteString) {
        output.push_bytes(self.to_bytes());
    }
}
