//! Structural equality of bound expressions (DuckDB's `Expression::Equals`), used to
//! deduplicate aggregates and to match expressions against GROUP BY terms.

use std::sync::Arc;

use rustdb_function::{AggregateFunction, FunctionData, ScalarFunction};

use super::{BoundOrderByNode, BoundOrderModifier, Expression, ExpressionKind};

fn list_equals(left: &[Expression], right: &[Expression]) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(l, r)| l.equals(r))
}

/// Order-insensitive equality (DuckDB's `ExpressionUtil::SetEquals`).
fn set_equals(left: &[Expression], right: &[Expression]) -> bool {
    left.len() == right.len()
        && left.iter().all(|l| right.iter().any(|r| l.equals(r)))
        && right.iter().all(|r| left.iter().any(|l| l.equals(r)))
}

fn option_equals(left: Option<&Expression>, right: Option<&Expression>) -> bool {
    match (left, right) {
        (Some(l), Some(r)) => l.equals(r),
        (None, None) => true,
        _ => false,
    }
}

fn orders_equal(left: &[BoundOrderByNode], right: &[BoundOrderByNode]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(l, r)| {
            l.order_type == r.order_type && l.null_order == r.null_order && l.expression.equals(&r.expression)
        })
}

fn order_modifiers_equal(left: Option<&BoundOrderModifier>, right: Option<&BoundOrderModifier>) -> bool {
    match (left, right) {
        (Some(l), Some(r)) => orders_equal(&l.orders, &r.orders),
        (None, None) => true,
        _ => false,
    }
}

/// Bind data equality: data is only known to be equal when it is shared.
fn bind_data_equal(left: Option<&Arc<dyn FunctionData>>, right: Option<&Arc<dyn FunctionData>>) -> bool {
    match (left, right) {
        (Some(l), Some(r)) => Arc::ptr_eq(l, r),
        (None, None) => true,
        _ => false,
    }
}

/// DuckDB's `BaseScalarFunction::Equal`.
pub(crate) fn scalar_functions_equal(left: &ScalarFunction, right: &ScalarFunction) -> bool {
    left.name == right.name
        && left.arguments == right.arguments
        && left.return_type == right.return_type
        && left.varargs == right.varargs
}

/// DuckDB's `AggregateFunction::operator==`.
pub(crate) fn aggregate_functions_equal(left: &AggregateFunction, right: &AggregateFunction) -> bool {
    left.name == right.name
        && left.arguments == right.arguments
        && left.return_type == right.return_type
        && left.varargs == right.varargs
}

impl Expression {
    /// Whether two expressions compute the same thing (DuckDB's `Expression::Equals`;
    /// aliases and locations are ignored).
    pub fn equals(&self, other: &Self) -> bool {
        if self.expression_type != other.expression_type
            || self.expression_class() != other.expression_class()
        {
            return false;
        }
        match (&self.kind, &other.kind) {
            (ExpressionKind::Aggregate(l), ExpressionKind::Aggregate(r)) => {
                l.aggr_type == r.aggr_type
                    && aggregate_functions_equal(&l.function, &r.function)
                    && option_equals(l.filter.as_ref(), r.filter.as_ref())
                    && list_equals(&l.children, &r.children)
                    && bind_data_equal(l.bind_info.as_ref(), r.bind_info.as_ref())
                    && order_modifiers_equal(l.order_bys.as_ref(), r.order_bys.as_ref())
            }
            (ExpressionKind::Between(l), ExpressionKind::Between(r)) => {
                l.input.equals(&r.input)
                    && l.lower.equals(&r.lower)
                    && l.upper.equals(&r.upper)
                    && l.lower_inclusive == r.lower_inclusive
                    && l.upper_inclusive == r.upper_inclusive
            }
            (ExpressionKind::Case(l), ExpressionKind::Case(r)) => {
                l.case_checks.len() == r.case_checks.len()
                    && l.case_checks.iter().zip(&r.case_checks).all(|(lc, rc)| {
                        lc.when_expr.equals(&rc.when_expr) && lc.then_expr.equals(&rc.then_expr)
                    })
                    && l.else_expr.equals(&r.else_expr)
            }
            (ExpressionKind::Cast(l), ExpressionKind::Cast(r)) => {
                self.return_type == other.return_type && l.try_cast == r.try_cast && l.child.equals(&r.child)
            }
            (ExpressionKind::ColumnRef(l), ExpressionKind::ColumnRef(r)) => l == r,
            (ExpressionKind::Comparison(l), ExpressionKind::Comparison(r)) => {
                l.left.equals(&r.left) && l.right.equals(&r.right)
            }
            (ExpressionKind::Conjunction(l), ExpressionKind::Conjunction(r)) => {
                set_equals(&l.children, &r.children)
            }
            (ExpressionKind::Constant(l), ExpressionKind::Constant(r)) => l.value == r.value,
            (ExpressionKind::Default, ExpressionKind::Default) => true,
            (ExpressionKind::Function(l), ExpressionKind::Function(r)) => {
                scalar_functions_equal(&l.function, &r.function)
                    && list_equals(&l.children, &r.children)
                    && bind_data_equal(l.bind_info.as_ref(), r.bind_info.as_ref())
                    && option_equals(l.lambda.as_deref(), r.lambda.as_deref())
            }
            (ExpressionKind::Lambda(l), ExpressionKind::Lambda(r)) => {
                l.parameter_count == r.parameter_count
                    && l.lambda_expr.equals(&r.lambda_expr)
                    && list_equals(&l.captures, &r.captures)
            }
            (ExpressionKind::LambdaRef(l), ExpressionKind::LambdaRef(r)) => l == r,
            (ExpressionKind::Operator(l), ExpressionKind::Operator(r)) => {
                list_equals(&l.children, &r.children)
            }
            (ExpressionKind::Parameter(l), ExpressionKind::Parameter(r)) => l.identifier == r.identifier,
            (ExpressionKind::Reference(l), ExpressionKind::Reference(r)) => l == r,
            (ExpressionKind::Unnest(l), ExpressionKind::Unnest(r)) => l.child.equals(&r.child),
            (ExpressionKind::Window(l), ExpressionKind::Window(r)) => {
                let aggregates_equal = match (&l.aggregate, &r.aggregate) {
                    (Some(a), Some(b)) => aggregate_functions_equal(a, b),
                    (None, None) => true,
                    _ => false,
                };
                aggregates_equal
                    && l.ignore_nulls == r.ignore_nulls
                    && l.distinct == r.distinct
                    && l.start == r.start
                    && l.end == r.end
                    && l.exclude_clause == r.exclude_clause
                    && list_equals(&l.children, &r.children)
                    && list_equals(&l.partitions, &r.partitions)
                    && orders_equal(&l.orders, &r.orders)
                    && orders_equal(&l.arg_orders, &r.arg_orders)
                    && option_equals(l.filter_expr.as_ref(), r.filter_expr.as_ref())
                    && option_equals(l.start_expr.as_ref(), r.start_expr.as_ref())
                    && option_equals(l.end_expr.as_ref(), r.end_expr.as_ref())
                    && option_equals(l.offset_expr.as_ref(), r.offset_expr.as_ref())
                    && option_equals(l.default_expr.as_ref(), r.default_expr.as_ref())
                    && bind_data_equal(l.bind_info.as_ref(), r.bind_info.as_ref())
            }
            (ExpressionKind::Expanded(l), ExpressionKind::Expanded(r)) => {
                list_equals(&l.expanded_expressions, &r.expanded_expressions)
            }
            // Subqueries are never equal to each other.
            _ => false,
        }
    }
}
