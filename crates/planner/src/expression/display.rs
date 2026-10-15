//! The text of bound expressions (DuckDB's `Expression::ToString`), used for column names
//! and plan rendering.

use std::fmt::{self, Write as _};

use rustdb_ast::{
    ExpressionType, OrderByNullType, OrderType, WindowBoundary, WindowExcludeMode,
    expression_type_to_operator, expression_type_to_string, value_to_sql_string,
};

use super::{BoundOrderByNode, Expression, ExpressionKind};

fn join(expressions: &[Expression], separator: &str) -> String {
    expressions
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(separator)
}

/// `expr, expr, ...`.
pub fn expression_list_to_string(expressions: &[Expression]) -> String {
    join(expressions, ", ")
}

impl fmt::Display for BoundOrderByNode {
    /// DuckDB's `BoundOrderByNode::ToString`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.expression)?;
        match self.order_type {
            OrderType::Ascending => f.write_str(" ASC")?,
            OrderType::Descending => f.write_str(" DESC")?,
            OrderType::Invalid | OrderType::OrderDefault => {}
        }
        match self.null_order {
            OrderByNullType::NullsFirst => f.write_str(" NULLS FIRST"),
            OrderByNullType::NullsLast => f.write_str(" NULLS LAST"),
            OrderByNullType::Invalid | OrderByNullType::OrderDefault => Ok(()),
        }
    }
}

/// DuckDB's `FunctionExpression::ToString` template as instantiated for bound functions
/// and aggregates (no catalog, schema or argument names).
fn function_call(
    name: &str,
    children: &[Expression],
    is_operator: bool,
    distinct: bool,
    filter: Option<&Expression>,
    orders: &[BoundOrderByNode],
) -> String {
    if is_operator {
        match children {
            [child] => {
                return if name.contains("__postfix") {
                    format!("(({child}){})", name.replace("__postfix", ""))
                } else {
                    format!("{name}({child})")
                };
            }
            [left, right] => return format!("({left} {name} {right})"),
            _ => {}
        }
    }
    let mut result = rustdb_ast::keyword::write_optionally_quoted(name);
    result.push('(');
    if distinct {
        result.push_str("DISTINCT ");
    }
    result.push_str(&join(children, ", "));
    if !orders.is_empty() {
        if children.is_empty() {
            result.push_str(") WITHIN GROUP (");
        }
        result.push_str(" ORDER BY ");
        let orders: Vec<String> = orders.iter().map(ToString::to_string).collect();
        result.push_str(&orders.join(", "));
    }
    result.push(')');
    if let Some(filter) = filter {
        let _ = write!(result, " FILTER (WHERE {filter})");
    }
    result
}

fn operator_text(expression: &Expression, children: &[Expression]) -> String {
    let ty = expression.expression_type;
    let child = |i: usize| children.get(i).map(ToString::to_string).unwrap_or_default();
    let infix = expression_type_to_operator(ty);
    if !infix.is_empty() {
        return format!("{} {infix} {}", child(0), child(1));
    }
    match ty {
        ExpressionType::CompareIn | ExpressionType::CompareNotIn => {
            let keyword = if ty == ExpressionType::CompareIn {
                " IN "
            } else {
                " NOT IN "
            };
            format!(
                "({}{keyword}({}))",
                child(0),
                join(children.get(1..).unwrap_or_default(), ", ")
            )
        }
        ExpressionType::OperatorUnpack => format!("UNPACK({})", child(0)),
        ExpressionType::OperatorTry => format!("TRY({})", child(0)),
        ExpressionType::OperatorNot => {
            format!("({} {})", expression_type_to_string(ty), join(children, ", "))
        }
        ExpressionType::GroupingFunction | ExpressionType::OperatorCoalesce => {
            format!("{}({})", expression_type_to_string(ty), join(children, ", "))
        }
        ExpressionType::OperatorIsNull => format!("({} IS NULL)", child(0)),
        ExpressionType::OperatorIsNotNull => format!("({} IS NOT NULL)", child(0)),
        ExpressionType::ArrayExtract => format!("{}[{}]", child(0), child(1)),
        ExpressionType::ArrayConstructor => format!("(ARRAY[{}])", join(children, ", ")),
        ExpressionType::StructExtract => {
            let field = child(1);
            let inner = field.get(1..field.len().saturating_sub(1)).unwrap_or_default();
            format!("({}).{}", child(0), rustdb_ast::keyword::sql_identifier(inner))
        }
        _ => String::new(),
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

/// DuckDB's `WindowExpression::ToString` template as instantiated for bound windows.
fn window_text(expression: &Expression, window: &super::BoundWindowExpression) -> String {
    use WindowBoundary as B;
    let name = window.aggregate.as_ref().map_or_else(
        || expression_type_to_string(expression.expression_type).to_owned(),
        |aggregate| aggregate.name.to_string(),
    );
    let mut result = format!("{name}(");
    for (i, child) in window.children.iter().enumerate() {
        if i > 0 {
            result.push_str(", ");
        }
        if window.distinct && i == 0 {
            result.push_str("DISTINCT ");
        }
        let _ = write!(result, "{child}");
    }
    for extra in [&window.offset_expr, &window.default_expr].into_iter().flatten() {
        let _ = write!(result, ", {extra}");
    }
    let orders = |orders: &[BoundOrderByNode]| {
        orders
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !window.arg_orders.is_empty() {
        let _ = write!(result, " ORDER BY {}", orders(&window.arg_orders));
    }
    if window.ignore_nulls {
        result.push_str(" IGNORE NULLS");
    }
    if let Some(filter) = &window.filter_expr {
        let _ = write!(result, ") FILTER (WHERE {filter}");
    }
    result.push_str(") OVER (");
    let mut separator = "";
    if !window.partitions.is_empty() {
        let _ = write!(result, "PARTITION BY {}", join(&window.partitions, ", "));
        separator = " ";
    }
    if !window.orders.is_empty() {
        let _ = write!(result, "{separator}ORDER BY {}", orders(&window.orders));
        separator = " ";
    }
    let text = |expr: &Option<Expression>| expr.as_ref().map(ToString::to_string).unwrap_or_default();
    let mut unit = "ROWS";
    let mut from = String::new();
    match window.start {
        B::CurrentRowRange | B::CurrentRowRows | B::CurrentRowGroups => {
            from.push_str("CURRENT ROW");
            unit = units(window.start, B::CurrentRowRows, B::CurrentRowRange);
        }
        B::UnboundedPreceding if window.end != B::CurrentRowRange => from.push_str("UNBOUNDED PRECEDING"),
        B::ExprPrecedingRows | B::ExprPrecedingRange | B::ExprPrecedingGroups => {
            let _ = write!(from, "{} PRECEDING", text(&window.start_expr));
            unit = units(window.start, B::ExprPrecedingRows, B::ExprPrecedingRange);
        }
        B::ExprFollowingRows | B::ExprFollowingRange | B::ExprFollowingGroups => {
            let _ = write!(from, "{} FOLLOWING", text(&window.start_expr));
            unit = units(window.start, B::ExprFollowingRows, B::ExprFollowingRange);
        }
        _ => {}
    }
    let mut to = String::new();
    match window.end {
        B::CurrentRowRange if window.start != B::UnboundedPreceding => {
            to.push_str("CURRENT ROW");
            unit = "RANGE";
        }
        B::CurrentRowRows | B::CurrentRowGroups => {
            to.push_str("CURRENT ROW");
            unit = units(window.end, B::CurrentRowRows, B::CurrentRowRange);
        }
        B::UnboundedPreceding => to.push_str("UNBOUNDED PRECEDING"),
        B::UnboundedFollowing => to.push_str("UNBOUNDED FOLLOWING"),
        B::ExprPrecedingRows | B::ExprPrecedingRange | B::ExprPrecedingGroups => {
            let _ = write!(to, "{} PRECEDING", text(&window.end_expr));
            unit = units(window.end, B::ExprPrecedingRows, B::ExprPrecedingRange);
        }
        B::ExprFollowingRows | B::ExprFollowingRange | B::ExprFollowingGroups => {
            let _ = write!(to, "{} FOLLOWING", text(&window.end_expr));
            unit = units(window.end, B::ExprFollowingRows, B::ExprFollowingRange);
        }
        _ => {}
    }
    if window.exclude_clause != WindowExcludeMode::NoOther {
        if from.is_empty() {
            from.push_str("UNBOUNDED PRECEDING");
        }
        if to.is_empty() {
            to.push_str("CURRENT ROW");
            unit = "RANGE";
        }
    }
    if !from.is_empty() || !to.is_empty() {
        result.push_str(separator);
        result.push_str(unit);
    }
    match (from.is_empty(), to.is_empty()) {
        (false, false) => {
            let _ = write!(result, " BETWEEN {from} AND {to}");
        }
        (false, true) => {
            let _ = write!(result, " {from}");
        }
        (true, false) => {
            let _ = write!(result, " {to}");
        }
        (true, true) => {}
    }
    match window.exclude_clause {
        WindowExcludeMode::NoOther => {}
        WindowExcludeMode::CurrentRow => result.push_str(" EXCLUDE CURRENT ROW"),
        WindowExcludeMode::Group => result.push_str(" EXCLUDE GROUP"),
        WindowExcludeMode::Ties => result.push_str(" EXCLUDE TIES"),
    }
    result.push(')');
    result
}

impl fmt::Display for Expression {
    /// DuckDB's `Expression::ToString`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match &self.kind {
            ExpressionKind::Aggregate(aggregate) => function_call(
                &aggregate.function.name,
                &aggregate.children,
                false,
                aggregate.is_distinct(),
                aggregate.filter.as_ref(),
                aggregate
                    .order_bys
                    .as_ref()
                    .map_or(&[][..], |order| &order.orders),
            ),
            ExpressionKind::Between(between) => format!(
                "({} BETWEEN {} AND {})",
                between.input, between.lower, between.upper
            ),
            ExpressionKind::Case(case) => {
                let mut text = String::from("CASE ");
                for check in &case.case_checks {
                    let _ = write!(text, " WHEN ({}) THEN ({})", check.when_expr, check.then_expr);
                }
                let _ = write!(text, " ELSE {} END", case.else_expr);
                text
            }
            ExpressionKind::Cast(cast) => format!(
                "{}{} AS {})",
                if cast.try_cast { "TRY_CAST(" } else { "CAST(" },
                cast.child.name(),
                self.return_type
            ),
            ExpressionKind::ColumnRef(column) => {
                if self.alias.is_empty() {
                    column.binding.to_string()
                } else {
                    self.alias.clone()
                }
            }
            ExpressionKind::Comparison(comparison) => format!(
                "({} {} {})",
                comparison.left,
                expression_type_to_operator(self.expression_type),
                comparison.right
            ),
            ExpressionKind::Conjunction(conjunction) => {
                let separator = format!(" {} ", expression_type_to_operator(self.expression_type));
                format!("({})", join(&conjunction.children, &separator))
            }
            ExpressionKind::Constant(constant) => value_to_sql_string(&constant.value),
            ExpressionKind::Default => "DEFAULT".to_owned(),
            ExpressionKind::Function(function) => function_call(
                &function.function.name,
                &function.children,
                function.is_operator,
                false,
                None,
                &[],
            ),
            ExpressionKind::Lambda(lambda) => lambda.lambda_expr.to_string(),
            ExpressionKind::LambdaRef(lambda) => {
                if self.alias.is_empty() {
                    format!(
                        "#[{}.{}.{}]",
                        lambda.binding.table_index, lambda.binding.column_index, lambda.lambda_index
                    )
                } else {
                    self.alias.clone()
                }
            }
            ExpressionKind::Operator(operator) => operator_text(self, &operator.children),
            ExpressionKind::Parameter(parameter) => format!("${}", parameter.identifier),
            ExpressionKind::Reference(reference) => {
                if self.alias.is_empty() {
                    format!("#{}", reference.index)
                } else {
                    self.alias.clone()
                }
            }
            ExpressionKind::Subquery(_) => "SUBQUERY".to_owned(),
            ExpressionKind::Unnest(unnest) => format!("UNNEST({})", unnest.child),
            ExpressionKind::Window(window) => window_text(self, window),
            ExpressionKind::Expanded(_) => "BOUND_EXPANDED".to_owned(),
        };
        f.write_str(&text)
    }
}
