//! Constructors of parse-tree nodes with the defaults of DuckDB's C++ constructors.

use rustdb_ast::{
    CaseCheck, CaseExpression, CastExpression, ColumnRefExpression, CommonTableExpressionMap,
    ComparisonExpression, ConjunctionExpression, ExpressionType, FunctionExpression, OperatorExpression,
    OrderModifier, ParsedExpression, ParsedExpressionKind, PositionalReferenceExpression, QueryNode,
    QueryNodeKind, SelectNode, SelectStatement, StarExpression, SubqueryRef, TableRef, TableRefKind,
};
use rustdb_common::{CaseInsensitiveMap, OrderedMap};
use rustdb_types::{LogicalType, Value};

/// `SetQueryLocation`: negative (unknown) locations are ignored.
pub(crate) fn set_location(expr: &mut ParsedExpression, location: i32) {
    if let Ok(location) = u64::try_from(location) {
        expr.query_location = Some(location);
    }
}

/// `SetQueryLocation` for table refs.
pub(crate) fn set_ref_location(table_ref: &mut TableRef, location: i32) {
    if let Ok(location) = u64::try_from(location) {
        table_ref.query_location = Some(location);
    }
}

/// `ConstantExpression(value)`.
pub(crate) fn constant(value: Value) -> ParsedExpression {
    ParsedExpression::constant(value)
}

/// `ColumnRefExpression(names)`.
pub(crate) fn column_ref(names: Vec<String>) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::ColumnRef,
        ParsedExpressionKind::ColumnRef(ColumnRefExpression { column_names: names }),
    )
}

/// `ColumnRefExpression(column_name, table_name)`.
pub(crate) fn qualified_column_ref(column: String, table: String) -> ParsedExpression {
    if table.is_empty() {
        column_ref(vec![column])
    } else {
        column_ref(vec![table, column])
    }
}

/// `StarExpression()` / `StarExpression(relation_name)`.
pub(crate) fn star_expression(relation_name: String) -> StarExpression {
    StarExpression {
        relation_name,
        exclude_list: Vec::new(),
        replace_list: CaseInsensitiveMap::default(),
        rename_list: OrderedMap::default(),
        columns: false,
        expr: None,
    }
}

/// A plain `*`.
pub(crate) fn star() -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::Star,
        ParsedExpressionKind::Star(Box::new(star_expression(String::new()))),
    )
}

/// `FunctionExpression(catalog, schema, name, children, filter, order_bys, distinct,
/// is_operator, export_state)`; the name is lower-cased like DuckDB does.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
pub(crate) fn function_full(
    catalog: String,
    schema: String,
    name: &str,
    children: Vec<ParsedExpression>,
    filter: Option<Box<ParsedExpression>>,
    order_bys: OrderModifier,
    distinct: bool,
    is_operator: bool,
    export_state: bool,
) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::Function,
        ParsedExpressionKind::Function(Box::new(FunctionExpression {
            function_name: name.to_ascii_lowercase(),
            schema,
            children,
            filter,
            order_bys,
            distinct,
            is_operator,
            export_state,
            catalog,
        })),
    )
}

/// `FunctionExpression(name, children)`.
pub(crate) fn function(name: &str, children: Vec<ParsedExpression>) -> ParsedExpression {
    function_full(
        String::new(),
        String::new(),
        name,
        children,
        None,
        OrderModifier { orders: Vec::new() },
        false,
        false,
        false,
    )
}

/// `OperatorExpression(type, children)`.
pub(crate) fn operator(expression_type: ExpressionType, children: Vec<ParsedExpression>) -> ParsedExpression {
    ParsedExpression::new(
        expression_type,
        ParsedExpressionKind::Operator(OperatorExpression { children }),
    )
}

/// `ComparisonExpression(type, left, right)`.
pub(crate) fn comparison(
    expression_type: ExpressionType,
    left: ParsedExpression,
    right: ParsedExpression,
) -> ParsedExpression {
    ParsedExpression::new(
        expression_type,
        ParsedExpressionKind::Comparison(ComparisonExpression {
            left: Box::new(left),
            right: Box::new(right),
        }),
    )
}

/// `ConjunctionExpression(type, left, right)`: children that are conjunctions of the
/// same type are flattened (`AddExpression`).
pub(crate) fn conjunction(
    expression_type: ExpressionType,
    left: ParsedExpression,
    right: ParsedExpression,
) -> ParsedExpression {
    let mut children = Vec::new();
    for child in [left, right] {
        if child.expression_type == expression_type
            && let ParsedExpressionKind::Conjunction(inner) = child.kind
        {
            children.extend(inner.children);
        } else {
            children.push(child);
        }
    }
    ParsedExpression::new(
        expression_type,
        ParsedExpressionKind::Conjunction(ConjunctionExpression { children }),
    )
}

/// `CastExpression(target, child, try_cast)`.
pub(crate) fn cast(target: LogicalType, child: ParsedExpression, try_cast: bool) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::OperatorCast,
        ParsedExpressionKind::Cast(CastExpression {
            child: Box::new(child),
            cast_type: target,
            try_cast,
        }),
    )
}

/// A one-armed `CASE WHEN when THEN then ELSE else END`.
pub(crate) fn case_expression(checks: Vec<CaseCheck>, else_expr: ParsedExpression) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::CaseExpr,
        ParsedExpressionKind::Case(CaseExpression {
            case_checks: checks,
            else_expr: Box::new(else_expr),
        }),
    )
}

/// `PositionalReferenceExpression(index)`.
pub(crate) fn positional_reference(index: u64) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::PositionalReference,
        ParsedExpressionKind::PositionalReference(PositionalReferenceExpression { index }),
    )
}

/// A table ref with no alias, sample or location.
pub(crate) fn table_ref(kind: TableRefKind) -> TableRef {
    TableRef {
        alias: String::new(),
        sample: None,
        query_location: None,
        kind,
    }
}

/// `EmptyTableRef`.
pub(crate) fn empty_table_ref() -> TableRef {
    table_ref(TableRefKind::Empty)
}

/// `SubqueryRef(subquery)`.
pub(crate) fn subquery_ref(subquery: SelectStatement) -> TableRef {
    table_ref(TableRefKind::Subquery(SubqueryRef {
        subquery: Box::new(subquery),
        column_name_alias: Vec::new(),
    }))
}

/// A `SelectNode` with an empty FROM and nothing else.
pub(crate) fn select_node() -> SelectNode {
    SelectNode {
        select_list: Vec::new(),
        from_table: Box::new(empty_table_ref()),
        where_clause: None,
        group_expressions: Vec::new(),
        group_sets: Vec::new(),
        aggregate_handling: rustdb_ast::AggregateHandling::StandardHandling,
        having: None,
        sample: None,
        qualify: None,
    }
}

/// A query node without modifiers or CTEs.
pub(crate) fn query_node(kind: QueryNodeKind) -> QueryNode {
    QueryNode {
        modifiers: Vec::new(),
        cte_map: CommonTableExpressionMap::default(),
        kind,
    }
}

/// A `SelectStatement` around a query node.
pub(crate) fn select_statement(node: QueryNode) -> SelectStatement {
    SelectStatement {
        node: Box::new(node),
        named_param_map: CaseInsensitiveMap::default(),
    }
}

/// `ConstantExpression(Value(text))`.
pub(crate) fn varchar_constant(text: &str) -> ParsedExpression {
    constant(Value::varchar(text))
}
