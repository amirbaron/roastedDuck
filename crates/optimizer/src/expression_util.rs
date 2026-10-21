//! Helpers on bound expressions that the passes share (DuckDB's `ExpressionIterator`
//! utilities, `BoundCastExpression::CastIsInvertible`, `Expression::CanThrow`, ...).

use std::cmp::Ordering;

use rustdb_ast::ExpressionType;
use rustdb_common::{Error, Result};
use rustdb_function::cast::ValueCast;
use rustdb_planner::expression::{BoundColumnRefExpression, BoundFunctionExpression};
use rustdb_planner::{ClientContext, ColumnBinding, Expression, ExpressionKind};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

/// Calls `callback` on every column reference in `expression` (DuckDB's
/// `ExpressionIterator::VisitExpression<BoundColumnRefExpression>`).
pub fn visit_column_refs(expression: &Expression, callback: &mut impl FnMut(&BoundColumnRefExpression)) {
    if let ExpressionKind::ColumnRef(column) = &expression.kind {
        callback(column);
        return;
    }
    for child in expression.children() {
        visit_column_refs(child, callback);
    }
}

/// Replaces every column reference for which `replace` returns an expression (DuckDB's
/// `ExpressionIterator::VisitExpressionMutable<BoundColumnRefExpression>`).
pub fn replace_column_refs(
    expression: &mut Expression,
    replace: &mut impl FnMut(&Expression) -> Option<Expression>,
) {
    if matches!(expression.kind, ExpressionKind::ColumnRef(_)) {
        if let Some(replacement) = replace(expression) {
            *expression = replacement;
        }
        return;
    }
    expression.for_each_child_mut(|child| replace_column_refs(child, replace));
}

/// The column bindings an expression references, in order of appearance (DuckDB's
/// `ColumnLifetimeAnalyzer::ExtractColumnBindings` / `ExtractFilterBindings`).
pub fn column_bindings(expression: &Expression) -> Vec<ColumnBinding> {
    let mut bindings = Vec::new();
    visit_column_refs(expression, &mut |column| bindings.push(column.binding));
    bindings
}

/// The table indexes an expression references (DuckDB's `LogicalJoin::GetExpressionBindings`).
pub fn table_indexes(expression: &Expression) -> Vec<u64> {
    let mut tables: Vec<u64> = Vec::new();
    visit_column_refs(expression, &mut |column| {
        if !tables.contains(&column.binding.table_index) {
            tables.push(column.binding.table_index);
        }
    });
    tables
}

/// Calls `callback` on `expression` and every expression below it, parents first
/// (DuckDB's `ExpressionIterator::EnumerateExpression`).
pub fn enumerate_expression(expression: &Expression, callback: &mut impl FnMut(&Expression)) {
    callback(expression);
    for child in expression.children() {
        enumerate_expression(child, callback);
    }
}

/// The comparison with its sides swapped (DuckDB's `FlipComparisonExpression`).
pub fn flip_comparison(expression_type: ExpressionType) -> ExpressionType {
    use ExpressionType as E;
    match expression_type {
        E::CompareLessthan => E::CompareGreaterthan,
        E::CompareGreaterthan => E::CompareLessthan,
        E::CompareLessthanorequalto => E::CompareGreaterthanorequalto,
        E::CompareGreaterthanorequalto => E::CompareLessthanorequalto,
        other => other,
    }
}

/// Whether an expression type is one of the six basic comparisons or `[NOT] DISTINCT FROM`.
pub const fn is_comparison(expression_type: ExpressionType) -> bool {
    use ExpressionType as E;
    matches!(
        expression_type,
        E::CompareEqual
            | E::CompareNotequal
            | E::CompareLessthan
            | E::CompareGreaterthan
            | E::CompareLessthanorequalto
            | E::CompareGreaterthanorequalto
            | E::CompareDistinctFrom
            | E::CompareNotDistinctFrom
    )
}

/// The width and scale of a numeric type as a decimal (DuckDB's
/// `LogicalType::GetDecimalProperties`).
pub fn decimal_properties(ty: &LogicalType) -> Option<(u8, u8)> {
    use LogicalTypeId as T;
    Some(match ty.id() {
        T::Boolean => (1, 0),
        T::TinyInt | T::UTinyInt => (3, 0),
        T::SmallInt | T::USmallInt => (5, 0),
        T::Integer | T::UInteger => (10, 0),
        T::BigInt => (19, 0),
        T::UBigInt => (20, 0),
        T::HugeInt => (38, 0),
        T::UHugeInt => (39, 0),
        T::Decimal => return ty.decimal_properties(),
        _ => return None,
    })
}

const fn is_signed(id: LogicalTypeId) -> bool {
    use LogicalTypeId as T;
    matches!(id, T::TinyInt | T::SmallInt | T::Integer | T::BigInt | T::HugeInt)
}

const fn is_unsigned(id: LogicalTypeId) -> bool {
    use LogicalTypeId as T;
    matches!(
        id,
        T::UTinyInt | T::USmallInt | T::UInteger | T::UBigInt | T::UHugeInt
    )
}

/// Whether casting from `source` to `target` and back gives the original value, so that
/// a comparison can be done on the uncast side (DuckDB's
/// `BoundCastExpression::CastIsInvertible`).
pub fn cast_is_invertible(source: &LogicalType, target: &LogicalType) -> bool {
    use LogicalTypeId as T;
    let (source_id, target_id) = (source.id(), target.id());
    let either = |id: LogicalTypeId| source_id == id || target_id == id;
    if either(T::Boolean) || either(T::Float) || either(T::Double) || either(T::Variant) {
        return false;
    }
    if either(T::Decimal) {
        let (Some((_, source_scale)), Some((_, target_scale))) =
            (decimal_properties(source), decimal_properties(target))
        else {
            return false;
        };
        return target_scale >= source_scale;
    }
    match source_id {
        T::Timestamp | T::TimestampTz | T::TimestampSec | T::TimestampMs | T::TimestampNs => {
            match target_id {
                T::Timestamp => return (source_id as u8) <= (T::Timestamp as u8),
                T::TimestampSec => return (source_id as u8) <= (T::TimestampSec as u8),
                T::TimestampMs => return (source_id as u8) <= (T::TimestampMs as u8),
                T::TimestampNs => return (source_id as u8) <= (T::TimestampNs as u8),
                T::Date | T::Time | T::TimeTz => return false,
                T::TimestampTz => return source_id == T::TimestampTz,
                _ => {}
            }
        }
        T::Varchar | T::Bit | T::TimeTz => return false,
        _ => {}
    }
    if target_id == T::Varchar {
        return matches!(
            source_id,
            T::Date
                | T::Time
                | T::TimeNs
                | T::Timestamp
                | T::TimestampNs
                | T::TimestampMs
                | T::TimestampSec
                | T::TimeTz
                | T::TimestampTz
        );
    }
    !(is_signed(source_id) && is_unsigned(target_id))
}

/// The built-in scalar functions DuckDB marks as able to raise runtime errors
/// (`SetFallible` / `FunctionErrors::CAN_THROW_RUNTIME_ERROR`), by name: arithmetic
/// (overflow, division by zero), math functions with restricted domains, list access,
/// date parts and construction, encodings, and the like. Sorted for binary search.
const FALLIBLE_FUNCTIONS: &[&str] = &[
    "!__postfix",
    "%",
    "&",
    "*",
    "+",
    "-",
    "/",
    "//",
    "<->",
    "<<",
    ">>",
    "acos",
    "add",
    "aggregate",
    "array_aggr",
    "array_aggregate",
    "array_cosine_distance",
    "array_cosine_similarity",
    "array_cross_product",
    "array_distance",
    "array_dot_product",
    "array_inner_product",
    "array_length",
    "array_negative_dot_product",
    "array_negative_inner_product",
    "array_resize",
    "array_slice",
    "asin",
    "atanh",
    "base64",
    "bitstring",
    "century",
    "cos",
    "cot",
    "currval",
    "date_part",
    "date_trunc",
    "datepart",
    "datetrunc",
    "day",
    "dayofmonth",
    "dayofweek",
    "dayofyear",
    "decade",
    "decode",
    "divide",
    "encode",
    "era",
    "error",
    "factorial",
    "fdiv",
    "fmod",
    "format",
    "from_base64",
    "from_binary",
    "from_hex",
    "gamma",
    "generate_series",
    "get_bit",
    "isodow",
    "isoyear",
    "lcm",
    "least_common_multiple",
    "lgamma",
    "list_aggr",
    "list_aggregate",
    "list_cosine_distance",
    "list_cosine_similarity",
    "list_distance",
    "list_dot_product",
    "list_element",
    "list_extract",
    "list_inner_product",
    "list_negative_dot_product",
    "list_negative_inner_product",
    "list_resize",
    "list_slice",
    "ln",
    "log",
    "log10",
    "log2",
    "lpad",
    "make_date",
    "make_time",
    "make_timestamp",
    "map",
    "map_from_entries",
    "map_keys",
    "map_values",
    "millennium",
    "mod",
    "month",
    "multiply",
    "nextval",
    "printf",
    "quarter",
    "range",
    "regexp_matches",
    "repeat",
    "rpad",
    "set_bit",
    "setseed",
    "sin",
    "sqrt",
    "strftime",
    "strptime",
    "subtract",
    "tan",
    "time_bucket",
    "timezone",
    "timezone_hour",
    "timezone_minute",
    "to_base64",
    "to_days",
    "to_hours",
    "to_microseconds",
    "to_milliseconds",
    "to_minutes",
    "to_seconds",
    "to_weeks",
    "unbin",
    "unhex",
    "url_decode",
    "week",
    "weekday",
    "weekofyear",
    "xor",
    "year",
    "yearweek",
    "|",
    "~",
];

/// Whether evaluating the expression can raise an error (DuckDB's `Expression::CanThrow`).
pub fn can_throw(expression: &Expression) -> bool {
    match &expression.kind {
        ExpressionKind::Function(function) if function_can_throw(function) => return true,
        ExpressionKind::Cast(cast) => {
            let child_type = &cast.child.return_type;
            let target = &expression.return_type;
            // DuckDB compares with `LogicalType(child_type.id())`, a type without
            // parameters: a cast to a type with parameters (DECIMAL, VARIANT, nested
            // types, ...) never counts as throwing here.
            if target.id() != child_type.id()
                && LogicalType::force_max_logical_type(target, child_type)
                    == LogicalType::simple(child_type.id())
            {
                return true;
            }
            if is_json(target) && !is_json(child_type) {
                return true;
            }
        }
        _ => {}
    }
    expression.children().into_iter().any(can_throw)
}

/// Whether a type is `JSON` (VARCHAR aliased as JSON, DuckDB's `IsJSONType`).
fn is_json(ty: &LogicalType) -> bool {
    ty.id() == LogicalTypeId::Varchar && ty.alias() == Some("JSON")
}

fn function_can_throw(function: &BoundFunctionExpression) -> bool {
    FALLIBLE_FUNCTIONS
        .binary_search(&&*function.function.name)
        .is_ok()
}

/// The value of a foldable expression, or `None` if evaluating it fails (DuckDB's
/// `ExpressionExecutor::TryEvaluateScalar`).
pub fn try_evaluate_scalar(context: &ClientContext, expression: &Expression) -> Option<Value> {
    context.evaluator.evaluate_scalar(expression).ok()
}

/// The value of a foldable expression (DuckDB's `ExpressionExecutor::EvaluateScalar`).
pub fn evaluate_scalar(context: &ClientContext, expression: &Expression) -> Result<Value> {
    context.evaluator.evaluate_scalar(expression)
}

/// Compares two values like DuckDB's `Value` comparison operators: values of different
/// types are compared at their common type.
pub fn compare_values(left: &Value, right: &Value) -> Option<Ordering> {
    if left.logical_type() == right.logical_type() {
        return left.compare_not_distinct(right);
    }
    let common = LogicalType::force_max_logical_type(left.logical_type(), right.logical_type());
    let left = left.default_cast_as(&common).ok()?;
    let right = right.default_cast_as(&common).ok()?;
    left.compare_not_distinct(&right)
}

/// A boolean constant's value, NULL as `None` (DuckDB's `BooleanValue::Get` after a
/// `DefaultCastAs(BOOLEAN)`).
pub fn boolean_value(value: &Value) -> Result<Option<bool>> {
    if value.is_null() {
        return Ok(None);
    }
    let cast = value.default_cast_as(&LogicalType::BOOLEAN)?;
    if cast.is_null() {
        return Ok(None);
    }
    cast.as_bool()
        .map(Some)
        .ok_or_else(|| Error::internal("boolean value expected"))
}

/// `constant_or_null(value, children...)`: `value` unless one of `children` is NULL
/// (DuckDB's `ExpressionRewriter::ConstantOrNull`).
pub fn constant_or_null(
    context: &ClientContext,
    children: Vec<Expression>,
    value: Value,
) -> Result<Expression> {
    let mut arguments = Vec::with_capacity(children.len() + 1);
    arguments.push(Expression::constant(value));
    arguments.extend(children);
    crate::optimizer::bind_scalar_function(context, "constant_or_null", arguments, false)
}
