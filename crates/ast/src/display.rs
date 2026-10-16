//! SQL text of parse trees (DuckDB's `ToString` methods of parsed expressions, query
//! nodes, table refs and result modifiers).
//!
//! The binder names result columns after the text of their expressions, so this output
//! must match DuckDB's character for character.

use std::borrow::Cow;
use std::fmt::{self, Write as _};

use rustdb_types::{LogicalType, LogicalTypeId, TypeInfo, TypeParameterValue, UnboundTypeInfo, Value};

use crate::keyword::{
    KeywordCategory, sql_identifier, write_optionally_quoted, write_optionally_quoted_with,
};
use crate::{
    AggregateHandling, BaseTableRef, CaseExpression, CommonTableExpressionMap, CteMaterialize,
    ExpressionClass, ExpressionListRef, ExpressionType, FunctionExpression, JoinRef, JoinRefType,
    LambdaExpression, LambdaSyntaxType, OperatorExpression, OrderByNode, OrderByNullType, OrderType,
    OrdinalityType, ParsedExpression, ParsedExpressionKind, PivotColumn, PivotRef, QualifiedColumnName,
    QueryNode, QueryNodeKind, RecursiveCteNode, ResultModifier, SampleOptions, SelectNode, SelectStatement,
    SetOperationNode, SetOperationType, ShowRef, ShowType, StarExpression, SubqueryType, TableRef,
    TableRefKind, TypeExpression, WindowBoundary, WindowExcludeMode, WindowExpression,
};

/// The infix operator of a comparison or conjunction (DuckDB's `ExpressionTypeToOperator`),
/// or `""` for other expression types.
pub const fn expression_type_to_operator(ty: ExpressionType) -> &'static str {
    match ty {
        ExpressionType::CompareEqual => "=",
        ExpressionType::CompareNotequal => "!=",
        ExpressionType::CompareLessthan => "<",
        ExpressionType::CompareGreaterthan => ">",
        ExpressionType::CompareLessthanorequalto => "<=",
        ExpressionType::CompareGreaterthanorequalto => ">=",
        ExpressionType::CompareDistinctFrom => "IS DISTINCT FROM",
        ExpressionType::CompareNotDistinctFrom => "IS NOT DISTINCT FROM",
        ExpressionType::ConjunctionAnd => "AND",
        ExpressionType::ConjunctionOr => "OR",
        _ => "",
    }
}

/// DuckDB's `ExpressionTypeToString` (which differs from the enum's own names).
pub const fn expression_type_to_string(ty: ExpressionType) -> &'static str {
    use ExpressionType as E;
    match ty {
        E::OperatorCast | E::Cast => "CAST",
        E::OperatorNot => "NOT",
        E::OperatorIsNull => "IS_NULL",
        E::OperatorIsNotNull => "IS_NOT_NULL",
        E::OperatorUnpack => "UNPACK",
        E::CompareEqual => "EQUAL",
        E::CompareNotequal => "NOTEQUAL",
        E::CompareLessthan => "LESSTHAN",
        E::CompareGreaterthan => "GREATERTHAN",
        E::CompareLessthanorequalto => "LESSTHANOREQUALTO",
        E::CompareGreaterthanorequalto => "GREATERTHANOREQUALTO",
        E::CompareIn => "IN",
        E::CompareDistinctFrom => "DISTINCT_FROM",
        E::CompareNotDistinctFrom => "NOT_DISTINCT_FROM",
        E::ConjunctionAnd => "AND",
        E::ConjunctionOr => "OR",
        E::ValueConstant => "CONSTANT",
        E::ValueParameter => "PARAMETER",
        E::ValueTuple => "TUPLE",
        E::ValueTupleAddress => "TUPLE_ADDRESS",
        E::ValueNull => "NULL",
        E::ValueVector => "VECTOR",
        E::ValueScalar => "SCALAR",
        E::Aggregate => "AGGREGATE",
        E::WindowAggregate => "WINDOW_AGGREGATE",
        E::WindowRank => "RANK",
        E::WindowRankDense => "RANK_DENSE",
        E::WindowPercentRank => "PERCENT_RANK",
        E::WindowRowNumber => "ROW_NUMBER",
        E::WindowFirstValue => "FIRST_VALUE",
        E::WindowLastValue => "LAST_VALUE",
        E::WindowNthValue => "NTH_VALUE",
        E::WindowCumeDist => "CUME_DIST",
        E::WindowLead => "LEAD",
        E::WindowLag => "LAG",
        E::WindowNtile => "NTILE",
        E::WindowFill => "FILL",
        E::Function => "FUNCTION",
        E::CaseExpr => "CASE",
        E::OperatorNullif => "NULLIF",
        E::OperatorCoalesce => "COALESCE",
        E::OperatorTry => "TRY",
        E::ArrayExtract => "ARRAY_EXTRACT",
        E::ArraySlice => "ARRAY_SLICE",
        E::StructExtract => "STRUCT_EXTRACT",
        E::Subquery => "SUBQUERY",
        E::Star => "STAR",
        E::Placeholder => "PLACEHOLDER",
        E::ColumnRef => "COLUMN_REF",
        E::LambdaRef => "LAMBDA_REF",
        E::FunctionRef => "FUNCTION_REF",
        E::TableRef => "TABLE_REF",
        E::CompareNotIn => "COMPARE_NOT_IN",
        E::CompareBetween => "COMPARE_BETWEEN",
        E::CompareNotBetween => "COMPARE_NOT_BETWEEN",
        E::ValueDefault => "VALUE_DEFAULT",
        E::BoundRef => "BOUND_REF",
        E::BoundColumnRef => "BOUND_COLUMN_REF",
        E::BoundFunction => "BOUND_FUNCTION",
        E::BoundAggregate => "BOUND_AGGREGATE",
        E::GroupingFunction => "GROUPING",
        E::ArrayConstructor => "ARRAY_CONSTRUCTOR",
        E::TableStar => "TABLE_STAR",
        E::BoundUnnest => "BOUND_UNNEST",
        E::Collate => "COLLATE",
        E::PositionalReference => "POSITIONAL_REFERENCE",
        E::BoundLambdaRef => "BOUND_LAMBDA_REF",
        E::Lambda => "LAMBDA",
        E::Arrow => "ARROW",
        E::Type => "TYPE",
        E::BoundExpanded => "BOUND_EXPANDED",
        E::Invalid => "INVALID",
    }
}

/// A value as SQL text (DuckDB's `Value::ToSQLString`).
pub fn value_to_sql_string(value: &Value) -> String {
    value.to_sql_string()
}

/// `EnumUtil::ToString(JoinType)`.
fn join_type_name(join: crate::JoinType) -> &'static str {
    join.name()
}

fn join_list<T>(items: &[T], separator: &str, render: impl Fn(&T) -> String) -> String {
    items.iter().map(render).collect::<Vec<_>>().join(separator)
}

impl ParsedExpression {
    /// The alias, or else the expression's text (DuckDB's `GetName`).
    pub fn name(&self) -> String {
        if !self.alias.is_empty() {
            return self.alias.clone();
        }
        match &self.kind {
            ParsedExpressionKind::ColumnRef(column) => {
                column.column_names.last().cloned().unwrap_or_default()
            }
            ParsedExpressionKind::LambdaRef(lambda) => lambda.column_name.clone(),
            _ => self.to_string(),
        }
    }
}

impl fmt::Display for ParsedExpression {
    /// DuckDB's `ParsedExpression::ToString`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ty = self.expression_type;
        match &self.kind {
            ParsedExpressionKind::Between(between) => write!(
                f,
                "({} BETWEEN {} AND {})",
                between.input, between.lower, between.upper
            ),
            ParsedExpressionKind::Case(case) => write_case(f, case),
            ParsedExpressionKind::Cast(cast) => write!(
                f,
                "{}{} AS {})",
                if cast.try_cast { "TRY_CAST(" } else { "CAST(" },
                cast.child,
                logical_type_to_string(&cast.cast_type)
            ),
            ParsedExpressionKind::Collate(collate) => {
                write!(
                    f,
                    "{} COLLATE {}",
                    collate.child,
                    sql_identifier(&collate.collation)
                )
            }
            ParsedExpressionKind::ColumnRef(column) => {
                f.write_str(&join_list(&column.column_names, ".", |name| {
                    write_optionally_quoted(name)
                }))
            }
            ParsedExpressionKind::LambdaRef(lambda) => f.write_str(&lambda.column_name),
            ParsedExpressionKind::Comparison(comparison) => write!(
                f,
                "({} {} {})",
                comparison.left,
                expression_type_to_operator(ty),
                comparison.right
            ),
            ParsedExpressionKind::Conjunction(conjunction) => {
                let operator = format!(" {} ", expression_type_to_operator(ty));
                write!(
                    f,
                    "({})",
                    join_list(&conjunction.children, &operator, ToString::to_string)
                )
            }
            ParsedExpressionKind::Constant(constant) => f.write_str(&value_to_sql_string(&constant.value)),
            ParsedExpressionKind::Default => f.write_str("DEFAULT"),
            ParsedExpressionKind::Function(function) => f.write_str(&function_to_string(function)),
            ParsedExpressionKind::Lambda(lambda) => write_lambda(f, lambda),
            ParsedExpressionKind::Operator(operator) => f.write_str(&operator_to_string(ty, operator)),
            ParsedExpressionKind::Parameter(parameter) => write!(f, "${}", parameter.identifier),
            ParsedExpressionKind::PositionalReference(reference) => write!(f, "#{}", reference.index),
            ParsedExpressionKind::Star(star) => fmt::Display::fmt(star, f),
            ParsedExpressionKind::Subquery(subquery) => match subquery.subquery_type {
                SubqueryType::Any => write!(
                    f,
                    "({} {} ANY({}))",
                    subquery
                        .child
                        .as_deref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                    expression_type_to_operator(subquery.comparison_type),
                    subquery.subquery
                ),
                SubqueryType::Exists => write!(f, "EXISTS({})", subquery.subquery),
                SubqueryType::NotExists => write!(f, "NOT EXISTS({})", subquery.subquery),
                SubqueryType::Scalar | SubqueryType::Invalid => write!(f, "({})", subquery.subquery),
            },
            ParsedExpressionKind::Window(window) => f.write_str(&window_to_string(window)),
            ParsedExpressionKind::Type(type_expression) => {
                f.write_str(&type_expression_to_string(type_expression))
            }
            ParsedExpressionKind::Bound(bound) => f.write_str(&bound.text),
        }
    }
}

fn write_case(f: &mut fmt::Formatter<'_>, case: &CaseExpression) -> fmt::Result {
    f.write_str("CASE ")?;
    for check in &case.case_checks {
        write!(f, " WHEN ({}) THEN ({})", check.when_expr, check.then_expr)?;
    }
    write!(f, " ELSE {} END", case.else_expr)
}

/// The column references making up a lambda's parameter list, if it is well-formed
/// (DuckDB's `LambdaExpression::ExtractColumnRefExpressions`).
pub fn lambda_parameters(lambda: &LambdaExpression) -> Option<Vec<&ParsedExpression>> {
    match &lambda.lhs.kind {
        ParsedExpressionKind::ColumnRef(_) => Some(vec![&*lambda.lhs]),
        ParsedExpressionKind::Function(function) if function.function_name == "row" => {
            let parameters: Vec<&ParsedExpression> = function.children.iter().collect();
            let all_columns = parameters
                .iter()
                .all(|child| child.expression_class() == ExpressionClass::ColumnRef);
            (all_columns && !parameters.is_empty()).then_some(parameters)
        }
        _ => None,
    }
}

fn write_lambda(f: &mut fmt::Formatter<'_>, lambda: &LambdaExpression) -> fmt::Result {
    if lambda.syntax_type != LambdaSyntaxType::LambdaKeyword {
        return write!(f, "({} -> {})", lambda.lhs, lambda.expr);
    }
    let parameters = lambda_parameters(lambda).unwrap_or_default();
    write!(
        f,
        "(lambda {}: {})",
        join_list(&parameters, ", ", ToString::to_string),
        lambda.expr
    )
}

/// DuckDB's `FunctionExpression::ToString` (template shared with bound functions).
fn function_to_string(function: &FunctionExpression) -> String {
    let name = &function.function_name;
    let children = &function.children;
    if function.is_operator {
        match children.as_slice() {
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
    let mut result = String::new();
    if !function.catalog.is_empty() {
        let _ = write!(result, "{}.", write_optionally_quoted(&function.catalog));
    }
    if !function.schema.is_empty() {
        let _ = write!(result, "{}.", write_optionally_quoted(&function.schema));
    }
    result.push_str(&write_optionally_quoted(name));
    result.push('(');
    if function.distinct {
        result.push_str("DISTINCT ");
    }
    result.push_str(&join_list(children, ", ", |child| {
        if child.alias.is_empty() {
            child.to_string()
        } else {
            format!("{} := {child}", sql_identifier(&child.alias))
        }
    }));
    let orders = &function.order_bys.orders;
    if !orders.is_empty() {
        if children.is_empty() {
            result.push_str(") WITHIN GROUP (");
        }
        result.push_str(" ORDER BY ");
        result.push_str(&join_list(orders, ", ", ToString::to_string));
    }
    result.push(')');
    if let Some(filter) = &function.filter {
        let _ = write!(result, " FILTER (WHERE {filter})");
    }
    if function.export_state {
        result.push_str(" EXPORT_STATE");
    }
    result
}

/// DuckDB's `OperatorExpression::ToString`.
fn operator_to_string(ty: ExpressionType, operator: &OperatorExpression) -> String {
    let children = &operator.children;
    let child = |i: usize| children.get(i).map(ToString::to_string).unwrap_or_default();
    let infix = expression_type_to_operator(ty);
    if !infix.is_empty() {
        return format!("{} {infix} {}", child(0), child(1));
    }
    let all = || join_list(children, ", ", ToString::to_string);
    match ty {
        ExpressionType::CompareIn | ExpressionType::CompareNotIn => {
            let keyword = if ty == ExpressionType::CompareIn {
                " IN "
            } else {
                " NOT IN "
            };
            let list = join_list(children.get(1..).unwrap_or_default(), ", ", ToString::to_string);
            format!("({}{keyword}({list}))", child(0))
        }
        ExpressionType::OperatorUnpack => format!("UNPACK({})", child(0)),
        ExpressionType::OperatorTry => format!("TRY({})", child(0)),
        ExpressionType::OperatorNot => format!("({} {})", expression_type_to_string(ty), all()),
        ExpressionType::GroupingFunction | ExpressionType::OperatorCoalesce => {
            format!("{}({})", expression_type_to_string(ty), all())
        }
        ExpressionType::OperatorIsNull => format!("({} IS NULL)", child(0)),
        ExpressionType::OperatorIsNotNull => format!("({} IS NOT NULL)", child(0)),
        ExpressionType::ArrayExtract => format!("{}[{}]", child(0), child(1)),
        ExpressionType::ArraySlice => {
            let mut begin = child(1);
            if begin == "[]" {
                begin.clear();
            }
            let mut end = child(2);
            if end == "[]" {
                end = if children.len() == 4 {
                    "-".to_owned()
                } else {
                    String::new()
                };
            }
            if children.len() == 4 {
                format!("{}[{begin}:{end}:{}]", child(0), child(3))
            } else {
                format!("{}[{begin}:{end}]", child(0))
            }
        }
        ExpressionType::StructExtract => {
            let is_constant = children
                .get(1)
                .is_some_and(|field| field.expression_type == ExpressionType::ValueConstant);
            if !is_constant {
                return String::new();
            }
            let field = child(1);
            let inner = field.get(1..field.len().saturating_sub(1)).unwrap_or_default();
            format!("({}).{}", child(0), sql_identifier(inner))
        }
        ExpressionType::ArrayConstructor => format!("(ARRAY[{}])", all()),
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

/// DuckDB's `WindowExpression::ToString`.
fn window_to_string(window: &WindowExpression) -> String {
    use WindowBoundary as B;
    let mut result = if window.schema.is_empty() {
        window.function_name.clone()
    } else {
        format!("{}.{}", window.schema, window.function_name)
    };
    result.push('(');
    let mut first = true;
    for child in &window.children {
        if !first {
            result.push_str(", ");
        }
        if window.distinct && first {
            result.push_str("DISTINCT ");
        }
        let _ = write!(result, "{child}");
        first = false;
    }
    for extra in [&window.offset_expr, &window.default_expr].into_iter().flatten() {
        let _ = write!(result, ", {extra}");
    }
    if !window.arg_orders.is_empty() {
        result.push_str(" ORDER BY ");
        result.push_str(&join_list(&window.arg_orders, ", ", ToString::to_string));
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
        result.push_str("PARTITION BY ");
        result.push_str(&join_list(&window.partitions, ", ", ToString::to_string));
        separator = " ";
    }
    if !window.orders.is_empty() {
        result.push_str(separator);
        result.push_str("ORDER BY ");
        result.push_str(&join_list(&window.orders, ", ", ToString::to_string));
        separator = " ";
    }
    let expr_text =
        |expr: &Option<Box<ParsedExpression>>| expr.as_deref().map(ToString::to_string).unwrap_or_default();
    let mut unit = "ROWS";
    let mut from: Cow<'static, str> = Cow::Borrowed("");
    match window.start {
        B::CurrentRowRange | B::CurrentRowRows | B::CurrentRowGroups => {
            from = Cow::Borrowed("CURRENT ROW");
            unit = units(window.start, B::CurrentRowRows, B::CurrentRowRange);
        }
        B::UnboundedPreceding => {
            if window.end != B::CurrentRowRange {
                from = Cow::Borrowed("UNBOUNDED PRECEDING");
            }
        }
        B::ExprPrecedingRows | B::ExprPrecedingRange | B::ExprPrecedingGroups => {
            from = Cow::Owned(format!("{} PRECEDING", expr_text(&window.start_expr)));
            unit = units(window.start, B::ExprPrecedingRows, B::ExprPrecedingRange);
        }
        B::ExprFollowingRows | B::ExprFollowingRange | B::ExprFollowingGroups => {
            from = Cow::Owned(format!("{} FOLLOWING", expr_text(&window.start_expr)));
            unit = units(window.start, B::ExprFollowingRows, B::ExprFollowingRange);
        }
        B::UnboundedFollowing | B::Invalid => {}
    }
    let mut to: Cow<'static, str> = Cow::Borrowed("");
    match window.end {
        B::CurrentRowRange => {
            if window.start != B::UnboundedPreceding {
                to = Cow::Borrowed("CURRENT ROW");
                unit = "RANGE";
            }
        }
        B::CurrentRowRows | B::CurrentRowGroups => {
            to = Cow::Borrowed("CURRENT ROW");
            unit = units(window.end, B::CurrentRowRows, B::CurrentRowRange);
        }
        B::UnboundedPreceding => to = Cow::Borrowed("UNBOUNDED PRECEDING"),
        B::UnboundedFollowing => to = Cow::Borrowed("UNBOUNDED FOLLOWING"),
        B::ExprPrecedingRows | B::ExprPrecedingRange | B::ExprPrecedingGroups => {
            to = Cow::Owned(format!("{} PRECEDING", expr_text(&window.end_expr)));
            unit = units(window.end, B::ExprPrecedingRows, B::ExprPrecedingRange);
        }
        B::ExprFollowingRows | B::ExprFollowingRange | B::ExprFollowingGroups => {
            to = Cow::Owned(format!("{} FOLLOWING", expr_text(&window.end_expr)));
            unit = units(window.end, B::ExprFollowingRows, B::ExprFollowingRange);
        }
        B::Invalid => {}
    }
    if window.exclude_clause != WindowExcludeMode::NoOther {
        if from.is_empty() {
            from = Cow::Borrowed("UNBOUNDED PRECEDING");
        }
        if to.is_empty() {
            to = Cow::Borrowed("CURRENT ROW");
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

/// DuckDB's `LogicalType::ToString` for types in parse trees: unbound types print as
/// the type expression they were parsed from.
pub fn logical_type_to_string(ty: &LogicalType) -> String {
    match ty.info() {
        Some(TypeInfo::Unbound(info)) if ty.id() == LogicalTypeId::Unbound => unbound_type_to_string(info),
        _ => ty.to_string(),
    }
}

/// [`type_expression_to_string`] for the unbound type form of a type expression.
fn unbound_type_to_string(info: &UnboundTypeInfo) -> String {
    let mut result = String::new();
    if !info.catalog.is_empty() {
        let _ = write!(result, "{}.", write_optionally_quoted(&info.catalog));
    }
    if !info.schema.is_empty() {
        let _ = write!(result, "{}.", write_optionally_quoted(&info.schema));
    }
    let param = |value: &TypeParameterValue| match value {
        TypeParameterValue::Type(ty) => unbound_type_to_string(ty),
        TypeParameterValue::Constant(value) => value_to_sql_string(value),
    };
    let params = &info.parameters;
    let named = |kind: &str| {
        let fields: Vec<String> = params
            .iter()
            .map(|p| format!("{} {}", write_optionally_quoted(&p.name), param(&p.value)))
            .collect();
        format!("{kind}({})", fields.join(", "))
    };
    if result.is_empty() {
        match (info.name.to_ascii_uppercase().as_str(), params.as_slice()) {
            ("LIST", [child]) => return format!("{}[]", param(&child.value)),
            ("ARRAY", [child, size]) => return format!("{}[{}]", param(&child.value), param(&size.value)),
            ("STRUCT", []) => return "STRUCT".to_owned(),
            ("STRUCT", _) => return named("STRUCT"),
            ("UNION", []) => return "UNION".to_owned(),
            ("UNION", _) => return named("UNION"),
            ("MAP", [key, value]) => return format!("MAP({}, {})", param(&key.value), param(&value.value)),
            ("VARCHAR", [.., last]) if last.name.eq_ignore_ascii_case("collation") => {
                if let TypeParameterValue::Constant(value) = &last.value {
                    let collation = value.to_varchar().unwrap_or_default();
                    return format!("VARCHAR COLLATE {}", sql_identifier(&collation));
                }
            }
            ("INTERVAL", [_, ..]) => return "INTERVAL".to_owned(),
            _ => {}
        }
    }
    result.push_str(&write_optionally_quoted_with(
        &info.name,
        '"',
        true,
        KeywordCategory::ColName,
    ));
    if !params.is_empty() {
        let rendered: Vec<String> = params.iter().map(|p| param(&p.value)).collect();
        let _ = write!(result, "({})", rendered.join(", "));
    }
    result
}

/// DuckDB's `TypeExpression::ToString`.
fn type_expression_to_string(ty: &TypeExpression) -> String {
    let mut result = String::new();
    if !ty.catalog.is_empty() {
        let _ = write!(result, "{}.", write_optionally_quoted(&ty.catalog));
    }
    if !ty.schema.is_empty() {
        let _ = write!(result, "{}.", write_optionally_quoted(&ty.schema));
    }
    let params = &ty.children;
    let name = ty.type_name.as_str();
    let unqualified = result.is_empty();
    let named = |kind: &str| {
        let fields = join_list(params, ", ", |param| {
            format!("{} {param}", write_optionally_quoted(&param.alias))
        });
        format!("{kind}({fields})")
    };
    if unqualified {
        match (name.to_ascii_uppercase().as_str(), params.as_slice()) {
            ("LIST", [child]) => return format!("{child}[]"),
            ("ARRAY", [child, size]) => return format!("{child}[{size}]"),
            ("STRUCT", []) => return "STRUCT".to_owned(),
            ("STRUCT", _) => return named("STRUCT"),
            ("UNION", []) => return "UNION".to_owned(),
            ("UNION", _) => return named("UNION"),
            ("MAP", [key, value]) => return format!("MAP({key}, {value})"),
            ("VARCHAR", [.., last]) if last.alias.eq_ignore_ascii_case("collation") => {
                if let ParsedExpressionKind::Constant(constant) = &last.kind {
                    let collation = constant.value.to_varchar().unwrap_or_default();
                    return format!("VARCHAR COLLATE {}", sql_identifier(&collation));
                }
            }
            ("INTERVAL", [_, ..]) => return "INTERVAL".to_owned(),
            _ => {}
        }
    }
    result.push_str(&write_optionally_quoted_with(
        name,
        '"',
        true,
        KeywordCategory::ColName,
    ));
    if !params.is_empty() {
        let _ = write!(result, "({})", join_list(params, ", ", ToString::to_string));
    }
    result
}

impl fmt::Display for StarExpression {
    /// DuckDB's `StarExpression::ToString`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(expr) = &self.expr {
            return write!(f, "COLUMNS({expr})");
        }
        if self.columns {
            f.write_str("COLUMNS(")?;
        }
        if self.relation_name.is_empty() {
            f.write_str("*")?;
        } else {
            write!(f, "{}.*", self.relation_name)?;
        }
        if !self.exclude_list.is_empty() {
            write!(
                f,
                " EXCLUDE ({})",
                join_list(&self.exclude_list, ", ", ToString::to_string)
            )?;
        }
        if !self.replace_list.is_empty() {
            let entries: Vec<String> = self
                .replace_list
                .iter()
                .map(|(name, expr)| format!("{expr} AS {}", write_optionally_quoted(name)))
                .collect();
            write!(f, " REPLACE ({})", entries.join(", "))?;
        }
        if !self.rename_list.is_empty() {
            let entries: Vec<String> = self
                .rename_list
                .iter()
                .map(|(name, alias)| format!("{name} AS {}", write_optionally_quoted(alias)))
                .collect();
            write!(f, " RENAME ({})", entries.join(", "))?;
        }
        if self.columns {
            f.write_str(")")?;
        }
        Ok(())
    }
}

impl fmt::Display for QualifiedColumnName {
    /// DuckDB's `QualifiedColumnName::ToString`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for part in [&self.catalog, &self.schema, &self.table] {
            if !part.is_empty() {
                write!(f, "{}.", write_optionally_quoted(part))?;
            }
        }
        f.write_str(&write_optionally_quoted(&self.column))
    }
}

impl fmt::Display for OrderByNode {
    /// DuckDB's `OrderByNode::ToString`.
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

impl fmt::Display for SelectStatement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.node, f)
    }
}

impl CommonTableExpressionMap {
    /// The `WITH ...` clause, or `""` without CTEs (DuckDB's `CommonTableExpressionMap::ToString`).
    pub fn to_sql(&self) -> String {
        if self.map.is_empty() {
            return String::new();
        }
        let recursive = self
            .map
            .iter()
            .any(|(_, cte)| matches!(cte.query.node.kind, QueryNodeKind::RecursiveCte(_)));
        let mut result = String::from("WITH ");
        if recursive {
            result.push_str("RECURSIVE ");
        }
        let mut first = true;
        for (name, cte) in self.map.iter() {
            if !first {
                result.push_str(", ");
            }
            first = false;
            result.push_str(&write_optionally_quoted(name));
            if !cte.aliases.is_empty() {
                let _ = write!(
                    result,
                    " ({})",
                    join_list(&cte.aliases, ", ", |a| write_optionally_quoted(a))
                );
            }
            if !cte.key_targets.is_empty() {
                let _ = write!(
                    result,
                    " USING KEY ({}) ",
                    join_list(&cte.key_targets, ", ", ToString::to_string)
                );
            }
            result.push_str(match cte.materialized {
                CteMaterialize::CteMaterializeAlways => " AS MATERIALIZED (",
                CteMaterialize::CteMaterializeNever => " AS NOT MATERIALIZED (",
                CteMaterialize::CteMaterializeDefault => " AS (",
            });
            let _ = write!(result, "{})", cte.query);
        }
        result
    }
}

impl QueryNode {
    /// `ORDER BY` / `LIMIT` / `OFFSET` of this node (DuckDB's `ResultModifiersToString`).
    fn modifiers_to_sql(&self) -> String {
        let mut result = String::new();
        for modifier in &self.modifiers {
            match modifier {
                ResultModifier::Order(order) => {
                    let _ = write!(
                        result,
                        " ORDER BY {}",
                        join_list(&order.orders, ", ", ToString::to_string)
                    );
                }
                ResultModifier::Limit(limit) => {
                    if let Some(value) = &limit.limit {
                        let _ = write!(result, " LIMIT {value}");
                    }
                    if let Some(offset) = &limit.offset {
                        let _ = write!(result, " OFFSET {offset}");
                    }
                }
                ResultModifier::LimitPercent(limit) => {
                    if let Some(value) = &limit.limit {
                        let _ = write!(result, " LIMIT ({value}) %");
                    }
                    if let Some(offset) = &limit.offset {
                        let _ = write!(result, " OFFSET {offset}");
                    }
                }
                ResultModifier::Distinct(_) => {}
            }
        }
        result
    }
}

impl fmt::Display for QueryNode {
    /// DuckDB's `QueryNode::ToString`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            QueryNodeKind::Select(select) => f.write_str(&select_to_sql(self, select)),
            QueryNodeKind::SetOperation(setop) => f.write_str(&setop_to_sql(self, setop)),
            QueryNodeKind::RecursiveCte(cte) => f.write_str(&recursive_cte_to_sql(self, cte)),
        }
    }
}

fn sample_to_sql(sample: &SampleOptions) -> String {
    let mut result = format!(
        " USING SAMPLE {}",
        sample.sample_size.to_varchar().unwrap_or_default()
    );
    if sample.is_percentage {
        result.push('%');
    }
    let _ = write!(result, " ({}", sample.method.name());
    if let Some(seed) = sample.seed {
        let _ = write!(result, ", {seed}");
    }
    result.push(')');
    result
}

/// DuckDB's `SelectNode::ToString`.
fn select_to_sql(node: &QueryNode, select: &SelectNode) -> String {
    if let TableRefKind::Show(_) = &select.from_table.kind {
        return select.from_table.to_string();
    }
    let mut result = node.cte_map.to_sql();
    result.push_str("SELECT ");
    for modifier in &node.modifiers {
        if let ResultModifier::Distinct(distinct) = modifier {
            result.push_str("DISTINCT ");
            if !distinct.distinct_on_targets.is_empty() {
                let _ = write!(
                    result,
                    "ON ({}) ",
                    join_list(&distinct.distinct_on_targets, ", ", ToString::to_string)
                );
            }
        }
    }
    result.push_str(&join_list(&select.select_list, ", ", |expr| {
        if expr.alias.is_empty() {
            expr.to_string()
        } else {
            format!("{expr} AS {}", sql_identifier(&expr.alias))
        }
    }));
    if !matches!(select.from_table.kind, TableRefKind::Empty) {
        let _ = write!(result, " FROM {}", select.from_table);
    }
    if let Some(condition) = &select.where_clause {
        let _ = write!(result, " WHERE {condition}");
    }
    if !select.group_sets.is_empty() {
        result.push_str(" GROUP BY ");
        let multiple = select.group_sets.len() > 1;
        if multiple {
            result.push_str("GROUPING SETS (");
        }
        for (i, set) in select.group_sets.iter().enumerate() {
            if i > 0 {
                result.push(',');
            }
            if set.is_empty() {
                result.push_str("()");
                continue;
            }
            if multiple {
                result.push('(');
            }
            let groups: Vec<String> = set
                .iter()
                .filter_map(|&index| usize::try_from(index).ok())
                .filter_map(|index| select.group_expressions.get(index))
                .map(ToString::to_string)
                .collect();
            result.push_str(&groups.join(", "));
            if multiple {
                result.push(')');
            }
        }
        if multiple {
            result.push(')');
        }
    } else if select.aggregate_handling == AggregateHandling::ForceAggregates {
        result.push_str(" GROUP BY ALL");
    }
    if let Some(having) = &select.having {
        let _ = write!(result, " HAVING {having}");
    }
    if let Some(qualify) = &select.qualify {
        let _ = write!(result, " QUALIFY {qualify}");
    }
    if let Some(sample) = &select.sample {
        result.push_str(&sample_to_sql(sample));
    }
    result + &node.modifiers_to_sql()
}

/// DuckDB's `SetOperationNode::ToString`.
fn setop_to_sql(node: &QueryNode, setop: &SetOperationNode) -> String {
    let mut result = node.cte_map.to_sql();
    let Some((first, rest)) = setop.children.split_first() else {
        return result;
    };
    let _ = write!(result, "({first}) ");
    let keyword = match (setop.setop_type, setop.setop_all) {
        (SetOperationType::Union, true) => "UNION ALL",
        (SetOperationType::Union, false) => "UNION",
        (SetOperationType::UnionByName, true) => "UNION ALL BY NAME",
        (SetOperationType::UnionByName, false) => "UNION BY NAME",
        (SetOperationType::Except, true) => "EXCEPT ALL",
        (SetOperationType::Except, false) => "EXCEPT",
        (SetOperationType::Intersect, true) => "INTERSECT ALL",
        (SetOperationType::Intersect, false) => "INTERSECT",
        (SetOperationType::None, _) => "",
    };
    for child in rest {
        let _ = write!(result, "{keyword} ({child})");
    }
    result + &node.modifiers_to_sql()
}

/// DuckDB's `RecursiveCTENode::ToString`.
fn recursive_cte_to_sql(node: &QueryNode, cte: &RecursiveCteNode) -> String {
    let mut result = node.cte_map.to_sql();
    let _ = write!(result, "({}) UNION ", cte.left);
    if cte.union_all {
        result.push_str(" ALL ");
    }
    let _ = write!(result, "({})", cte.right);
    result
}

impl TableRef {
    /// ` AS alias(col, ...)` (DuckDB's `TableRef::AliasToString`).
    fn alias_to_sql(&self, column_name_alias: &[String]) -> String {
        let mut result = String::new();
        if !self.alias.is_empty() {
            let _ = write!(result, " AS {}", sql_identifier(&self.alias));
        }
        if !column_name_alias.is_empty() {
            let _ = write!(
                result,
                "({})",
                join_list(column_name_alias, ", ", |name| write_optionally_quoted(name))
            );
        }
        result
    }

    /// ` TABLESAMPLE ...` (DuckDB's `TableRef::SampleToString`).
    fn sample_to_sql(&self) -> String {
        let Some(sample) = &self.sample else {
            return String::new();
        };
        let mut result = format!(
            " TABLESAMPLE {}({} {})",
            sample.method.name(),
            sample.sample_size.to_varchar().unwrap_or_default(),
            if sample.is_percentage { "PERCENT" } else { "ROWS" }
        );
        if let Some(seed) = sample.seed {
            let _ = write!(result, "REPEATABLE ({seed})");
        }
        result
    }

    fn base_to_sql(&self, text: String, column_name_alias: &[String]) -> String {
        text + &self.alias_to_sql(column_name_alias) + &self.sample_to_sql()
    }
}

impl fmt::Display for TableRef {
    /// DuckDB's `TableRef::ToString`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match &self.kind {
            TableRefKind::BaseTable(base) => base_table_to_sql(self, base),
            TableRefKind::Join(join) => join_to_sql(join),
            TableRefKind::Subquery(subquery) => {
                self.base_to_sql(format!("({})", subquery.subquery), &subquery.column_name_alias)
            }
            TableRefKind::TableFunction(function) => {
                let mut text = function.function.to_string();
                if function.with_ordinality == OrdinalityType::WithOrdinality {
                    text.push_str(" WITH ORDINALITY");
                }
                self.base_to_sql(text, &function.column_name_alias)
            }
            TableRefKind::Empty => String::new(),
            TableRefKind::ExpressionList(list) => self.base_to_sql(values_to_sql(list), &list.expected_names),
            TableRefKind::Pivot(pivot) => pivot_to_sql(self, pivot),
            TableRefKind::Show(show) => show_to_sql(show),
        };
        f.write_str(&text)
    }
}

fn base_table_to_sql(table_ref: &TableRef, base: &BaseTableRef) -> String {
    let mut result = String::new();
    for part in [&base.catalog_name, &base.schema_name] {
        if !part.is_empty() {
            let _ = write!(result, "{}.", write_optionally_quoted(part));
        }
    }
    result.push_str(&write_optionally_quoted(&base.table_name));
    result.push_str(&table_ref.alias_to_sql(&base.column_name_alias));
    if let Some(at) = &base.at_clause {
        let _ = write!(result, " AT ({} => {})", at.unit, at.expr);
    }
    result + &table_ref.sample_to_sql()
}

fn join_to_sql(join: &JoinRef) -> String {
    let mut result = format!("{} ", join.left);
    let join_type = join_type_name(join.join_type);
    match join.ref_type {
        JoinRefType::Regular => {
            let _ = write!(result, "{join_type} JOIN ");
        }
        JoinRefType::Natural => {
            let _ = write!(result, "NATURAL {join_type} JOIN ");
        }
        JoinRefType::Asof => {
            let _ = write!(result, "ASOF {join_type} JOIN ");
        }
        JoinRefType::Cross => result.push_str(if join.is_implicit { ", " } else { "CROSS JOIN " }),
        JoinRefType::Positional => result.push_str("POSITIONAL JOIN "),
        JoinRefType::Dependent => result.push_str("DEPENDENT JOIN "),
    }
    let _ = write!(result, "{}", join.right);
    if let Some(condition) = &join.condition {
        let _ = write!(result, " ON ({condition})");
    } else if !join.using_columns.is_empty() {
        let _ = write!(
            result,
            " USING ({})",
            join_list(&join.using_columns, ", ", |name| write_optionally_quoted(name))
        );
    }
    result
}

fn values_to_sql(list: &ExpressionListRef) -> String {
    let rows = join_list(&list.values, ", ", |row| {
        format!("({})", join_list(row, ", ", ToString::to_string))
    });
    format!("(VALUES {rows})")
}

fn pivot_column_to_sql(pivot: &PivotColumn) -> String {
    let mut result = String::new();
    if !pivot.unpivot_names.is_empty() {
        if let [name] = pivot.unpivot_names.as_slice() {
            result.push_str(&write_optionally_quoted(name));
        } else {
            let _ = write!(
                result,
                "({})",
                join_list(&pivot.unpivot_names, ", ", |name| write_optionally_quoted(name))
            );
        }
    } else if !pivot.pivot_expressions.is_empty() {
        let _ = write!(
            result,
            "({})",
            join_list(&pivot.pivot_expressions, ", ", ToString::to_string)
        );
    }
    result.push_str(" IN ");
    if pivot.pivot_enum.is_empty() {
        let entries = join_list(&pivot.entries, ", ", |entry| {
            let mut text = if let Some(expr) = &entry.star_expr {
                expr.to_string()
            } else if let [value] = entry.values.as_slice() {
                value_to_sql_string(value)
            } else {
                format!("({})", join_list(&entry.values, ", ", value_to_sql_string))
            };
            if !entry.alias.is_empty() {
                let _ = write!(text, " AS {}", write_optionally_quoted(&entry.alias));
            }
            text
        });
        let _ = write!(result, "({entries})");
    } else {
        result.push_str(&write_optionally_quoted(&pivot.pivot_enum));
    }
    result
}

fn pivot_to_sql(table_ref: &TableRef, pivot: &PivotRef) -> String {
    let mut result = pivot.source.to_string();
    if pivot.aggregates.is_empty() {
        result.push_str(" UNPIVOT ");
        if pivot.include_nulls {
            result.push_str("INCLUDE NULLS ");
        }
        result.push('(');
        if let [name] = pivot.unpivot_names.as_slice() {
            result.push_str(&write_optionally_quoted(name));
        } else {
            let _ = write!(
                result,
                "({})",
                join_list(&pivot.unpivot_names, ", ", |name| write_optionally_quoted(name))
            );
        }
    } else {
        result.push_str(" PIVOT (");
        result.push_str(&join_list(&pivot.aggregates, ", ", |aggregate| {
            if aggregate.alias.is_empty() {
                aggregate.to_string()
            } else {
                format!("{aggregate} AS {}", write_optionally_quoted(&aggregate.alias))
            }
        }));
    }
    result.push_str(" FOR");
    for column in &pivot.pivots {
        let _ = write!(result, " {}", pivot_column_to_sql(column));
    }
    if !pivot.groups.is_empty() {
        let _ = write!(
            result,
            " GROUP BY {}",
            join_list(&pivot.groups, ", ", |name| write_optionally_quoted(name))
        );
    }
    result.push(')');
    if !table_ref.alias.is_empty() {
        let _ = write!(result, " AS {}", write_optionally_quoted(&table_ref.alias));
        if !pivot.column_name_alias.is_empty() {
            let _ = write!(
                result,
                "({})",
                join_list(&pivot.column_name_alias, ", ", |name| write_optionally_quoted(
                    name
                ))
            );
        }
    }
    result
}

fn show_to_sql(show: &ShowRef) -> String {
    let mut result = String::new();
    match show.show_type {
        ShowType::Summary => result.push_str("SUMMARIZE "),
        ShowType::ShowFrom => {
            result.push_str("SHOW TABLES FROM ");
            if !show.catalog_name.is_empty() {
                result.push_str(&write_optionally_quoted(&show.catalog_name));
                if !show.schema_name.is_empty() {
                    result.push('.');
                }
            }
            result.push_str(&write_optionally_quoted(&show.schema_name));
        }
        ShowType::Describe | ShowType::ShowUnqualified => result.push_str("DESCRIBE "),
    }
    if let Some(query) = &show.query {
        let _ = write!(result, "({query})");
    } else if show.table_name != "__show_tables_expanded" {
        result.push_str(&show.table_name);
    }
    result
}

impl fmt::Display for crate::InsertStatement {
    /// DuckDB's `InsertStatement::ToString`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use crate::OnConflictAction;
        let mut result = self.cte_map.to_sql();
        result.push_str("INSERT");
        let or_replace = self
            .on_conflict_info
            .as_ref()
            .is_some_and(|info| info.action_type == OnConflictAction::Replace);
        if or_replace {
            result.push_str(" OR REPLACE");
        }
        result.push_str(" INTO ");
        for qualifier in [&self.catalog, &self.schema] {
            if !qualifier.is_empty() {
                result.push_str(&write_optionally_quoted(qualifier));
                result.push('.');
            }
        }
        result.push_str(&write_optionally_quoted(&self.table));
        if let Some(table_ref) = &self.table_ref
            && !table_ref.alias.is_empty()
        {
            let _ = write!(result, " AS {}", write_optionally_quoted(&table_ref.alias));
        }
        if self.column_order == crate::InsertColumnOrder::InsertByName {
            result.push_str(" BY NAME");
        }
        if !self.columns.is_empty() {
            let columns = join_list(&self.columns, ", ", |column| write_optionally_quoted(column));
            let _ = write!(result, " ({columns} )");
        }
        result.push(' ');
        if let Some(values) = self.values_list() {
            result.push_str(&values_to_sql(values));
        } else if let Some(select) = &self.select_statement {
            result.push_str(&select.to_string());
        } else {
            result.push_str("DEFAULT VALUES");
        }
        if !or_replace && let Some(info) = &self.on_conflict_info {
            result.push_str(" ON CONFLICT ");
            if !info.indexed_columns.is_empty() {
                let columns = join_list(&info.indexed_columns, ", ", |column| column.to_lowercase());
                let _ = write!(result, "({columns} )");
            }
            if let Some(condition) = &info.condition {
                let _ = write!(result, " WHERE {condition}");
            }
            result.push(' ');
            result.push_str(match info.action_type {
                OnConflictAction::Nothing => "DO NOTHING",
                OnConflictAction::Update | OnConflictAction::Replace => "DO UPDATE",
                OnConflictAction::Throw => "",
            });
            if let Some(set_info) = &info.set_info {
                result.push_str(" SET ");
                let assignments: Vec<String> = set_info
                    .columns
                    .iter()
                    .zip(&set_info.expressions)
                    .map(|(column, expression)| format!("{} = {expression}", column.to_lowercase()))
                    .collect();
                result.push_str(&assignments.join(", "));
                if let Some(condition) = &set_info.condition {
                    let _ = write!(result, " WHERE {condition}");
                }
            }
        }
        if !self.returning_list.is_empty() {
            result.push_str(" RETURNING ");
            let columns = join_list(&self.returning_list, ", ", |expression| {
                if expression.alias.is_empty() {
                    expression.to_string()
                } else {
                    format!("{expression} AS {}", write_optionally_quoted(&expression.alias))
                }
            });
            result.push_str(&columns);
        }
        f.write_str(&result)
    }
}

impl crate::InsertStatement {
    /// The `VALUES` list inserted directly (`INSERT INTO tbl VALUES (...)`), if that is the
    /// source (DuckDB's `InsertStatement::GetValuesList`).
    pub fn values_list(&self) -> Option<&ExpressionListRef> {
        let select = self.select_statement.as_ref()?;
        let QueryNodeKind::Select(node) = &select.node.kind else {
            return None;
        };
        if node.where_clause.is_some()
            || node.qualify.is_some()
            || node.having.is_some()
            || !select.node.cte_map.map.is_empty()
            || !node.group_sets.is_empty()
            || node.aggregate_handling != AggregateHandling::StandardHandling
        {
            return None;
        }
        if node.select_list.len() != 1 || node.select_list[0].expression_type != ExpressionType::Star {
            return None;
        }
        match &node.from_table.kind {
            TableRefKind::ExpressionList(list) => Some(list),
            _ => None,
        }
    }
}
