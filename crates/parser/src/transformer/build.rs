//! Constructors of parse-tree nodes with the defaults of DuckDB's C++ constructors.

use roastedduck_ast::{
    CaseCheck, CaseExpression, CastExpression, ColumnRefExpression, CommonTableExpressionMap,
    ComparisonExpression, ConjunctionExpression, ExpressionType, FunctionExpression, OperatorExpression,
    OrderModifier, ParsedExpression, ParsedExpressionKind, PositionalReferenceExpression, QueryNode,
    QueryNodeKind, SelectNode, SelectStatement, StarExpression, SubqueryRef, TableRef, TableRefKind,
};
use roastedduck_common::{CaseInsensitiveMap, OrderedMap};
use roastedduck_types::{LogicalType, Value};

/// `SetQueryLocation`: negative (unknown) locations are ignored.
pub fn set_location(expr: &mut ParsedExpression, location: i32) {
    if let Ok(location) = u64::try_from(location) {
        expr.query_location = Some(location);
    }
}

/// `SetQueryLocation` for table refs.
pub fn set_ref_location(table_ref: &mut TableRef, location: i32) {
    if let Ok(location) = u64::try_from(location) {
        table_ref.query_location = Some(location);
    }
}

/// `ConstantExpression(value)`.
pub fn constant(value: Value) -> ParsedExpression {
    ParsedExpression::constant(value)
}

/// `ColumnRefExpression(names)`.
pub fn column_ref<K: Into<roastedduck_common::ByteString>>(names: Vec<K>) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::ColumnRef,
        ParsedExpressionKind::ColumnRef(ColumnRefExpression {
            column_names: names.into_iter().map(Into::into).collect(),
        }),
    )
}

/// `ColumnRefExpression(column_name, table_name)`.
pub fn qualified_column_ref(
    column: roastedduck_common::ByteString,
    table: roastedduck_common::ByteString,
) -> ParsedExpression {
    if table.is_empty() {
        column_ref(vec![column])
    } else {
        column_ref(vec![table, column])
    }
}

/// `StarExpression()` / `StarExpression(relation_name)`.
pub fn star_expression(relation_name: impl Into<roastedduck_common::ByteString>) -> StarExpression {
    StarExpression {
        relation_name: relation_name.into(),
        exclude_list: Vec::new(),
        replace_list: CaseInsensitiveMap::default(),
        rename_list: OrderedMap::default(),
        columns: false,
        expr: None,
    }
}

/// A plain `*`.
pub fn star() -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::Star,
        ParsedExpressionKind::Star(Box::new(star_expression(String::new()))),
    )
}

/// `FunctionExpression(catalog, schema, name, children, filter, order_bys, distinct,
/// is_operator, export_state)`; the name is lower-cased like DuckDB does.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
pub fn function_full(
    catalog: impl Into<roastedduck_common::ByteString>,
    schema: impl Into<roastedduck_common::ByteString>,
    name: impl AsRef<[u8]>,
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
            function_name: name.as_ref().to_ascii_lowercase().into(),
            schema: schema.into(),
            children,
            filter,
            order_bys,
            distinct,
            is_operator,
            export_state,
            catalog: catalog.into(),
        })),
    )
}

/// `FunctionExpression(name, children)`.
pub fn function(name: impl AsRef<[u8]>, children: Vec<ParsedExpression>) -> ParsedExpression {
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
pub fn operator(expression_type: ExpressionType, children: Vec<ParsedExpression>) -> ParsedExpression {
    ParsedExpression::new(
        expression_type,
        ParsedExpressionKind::Operator(OperatorExpression { children }),
    )
}

/// `ComparisonExpression(type, left, right)`.
pub fn comparison(
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
pub fn conjunction(
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
pub fn cast(target: LogicalType, child: ParsedExpression, try_cast: bool) -> ParsedExpression {
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
pub fn case_expression(checks: Vec<CaseCheck>, else_expr: ParsedExpression) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::CaseExpr,
        ParsedExpressionKind::Case(CaseExpression {
            case_checks: checks,
            else_expr: Box::new(else_expr),
        }),
    )
}

/// `PositionalReferenceExpression(index)`.
pub fn positional_reference(index: u64) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::PositionalReference,
        ParsedExpressionKind::PositionalReference(PositionalReferenceExpression { index }),
    )
}

/// A table ref with no alias, sample or location.
pub fn table_ref(kind: TableRefKind) -> TableRef {
    TableRef {
        external_dependency: roastedduck_ast::ExternalDependency::default(),
        alias: String::new().into(),
        sample: None,
        query_location: None,
        kind,
    }
}

/// `EmptyTableRef`.
pub fn empty_table_ref() -> TableRef {
    table_ref(TableRefKind::Empty)
}

/// `SubqueryRef(subquery)`.
pub fn subquery_ref(subquery: SelectStatement) -> TableRef {
    table_ref(TableRefKind::Subquery(SubqueryRef {
        subquery: Box::new(subquery),
        column_name_alias: Vec::new(),
    }))
}

/// A `SelectNode` with an empty FROM and nothing else.
pub fn select_node() -> SelectNode {
    SelectNode {
        select_list: Vec::new(),
        from_table: Box::new(empty_table_ref()),
        where_clause: None,
        group_expressions: Vec::new(),
        group_sets: Vec::new(),
        aggregate_handling: roastedduck_ast::AggregateHandling::StandardHandling,
        having: None,
        sample: None,
        qualify: None,
    }
}

/// A query node without modifiers or CTEs.
pub fn query_node(kind: QueryNodeKind) -> QueryNode {
    QueryNode {
        modifiers: Vec::new(),
        cte_map: CommonTableExpressionMap::default(),
        kind,
    }
}

/// A `SelectStatement` around a query node.
pub fn select_statement(node: QueryNode) -> SelectStatement {
    SelectStatement {
        node: Box::new(node),
        named_param_map: CaseInsensitiveMap::default(),
    }
}

/// `ConstantExpression(Value(text))`.
pub fn varchar_constant(text: &str) -> ParsedExpression {
    constant(Value::varchar(text))
}

/// Native `ConstantExpression(Value(bytes))`; validation matches C++'s Value constructor.
pub fn try_varchar_constant(bytes: impl AsRef<[u8]>) -> roastedduck_common::Result<ParsedExpression> {
    Ok(constant(Value::try_varchar(bytes.as_ref())?))
}

/// `UUID::ToString(UUID::GenerateRandomUUID())`: a random version-4 UUID (the names of
/// the enum types PIVOT statements create).
pub fn random_uuid() -> String {
    use std::fmt::Write as _;
    use std::hash::{BuildHasher, Hasher};

    let state = std::collections::hash_map::RandomState::new();
    let mut bytes = [0_u8; 16];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let mut hasher = state.build_hasher();
        hasher.write_usize(i);
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        chunk.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.iter().fold(String::with_capacity(32), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Converts name bytes into a SQL value at DuckDB's checked VARCHAR boundary.
pub fn varchar_constant_bytes(text: &[u8]) -> roastedduck_common::Result<ParsedExpression> {
    Ok(constant(Value::try_varchar(text)?))
}
