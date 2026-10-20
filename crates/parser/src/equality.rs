//! DuckDB's structural comparisons of parsed expressions (`ParsedExpression::Equals`
//! and the subclasses' `Equal`), and the child enumeration they rely on
//! (`ParsedExpressionIterator::EnumerateChildren`).
//!
//! These differ from the derived `PartialEq`: aliases and query locations are ignored,
//! column names and parameters compare case-insensitively, and a few classes compare
//! only some of their members, exactly as in DuckDB.

use std::cmp::Ordering;
use std::hash::{DefaultHasher, Hash, Hasher};

use rustdb_ast::visit::{VisitorMut, walk_expression, walk_table_ref};
use rustdb_ast::{
    OrderByNode, OrderModifier, ParsedExpression, ParsedExpressionKind, QualifiedColumnName, SelectStatement,
    StarExpression, TableRef,
};
use rustdb_types::{LogicalType, TypeInfo, TypeParameter, TypeParameterValue, UnboundTypeInfo, Value};

/// Calls `callback` on each direct child of `expr`, in DuckDB's order; subqueries are
/// not entered (only the child of `x IN (SELECT ...)` is visited).
pub(crate) fn for_each_child_mut(
    expr: &mut ParsedExpression,
    callback: &mut dyn FnMut(&mut ParsedExpression),
) {
    match &mut expr.kind {
        ParsedExpressionKind::Between(between) => {
            callback(&mut between.input);
            callback(&mut between.lower);
            callback(&mut between.upper);
        }
        ParsedExpressionKind::Case(case) => {
            for check in &mut case.case_checks {
                callback(&mut check.when_expr);
                callback(&mut check.then_expr);
            }
            callback(&mut case.else_expr);
        }
        ParsedExpressionKind::Cast(cast) => callback(&mut cast.child),
        ParsedExpressionKind::Collate(collate) => callback(&mut collate.child),
        ParsedExpressionKind::Comparison(comparison) => {
            callback(&mut comparison.left);
            callback(&mut comparison.right);
        }
        ParsedExpressionKind::Conjunction(conjunction) => conjunction.children.iter_mut().for_each(callback),
        ParsedExpressionKind::Function(function) => {
            function.children.iter_mut().for_each(&mut *callback);
            if let Some(filter) = &mut function.filter {
                callback(filter);
            }
            for order in &mut function.order_bys.orders {
                callback(&mut order.expression);
            }
        }
        ParsedExpressionKind::Type(type_expr) => type_expr.children.iter_mut().for_each(callback),
        ParsedExpressionKind::Lambda(lambda) => {
            callback(&mut lambda.lhs);
            callback(&mut lambda.expr);
        }
        ParsedExpressionKind::Operator(operator) => operator.children.iter_mut().for_each(callback),
        ParsedExpressionKind::Star(star) => {
            if let Some(child) = &mut star.expr {
                callback(child);
            }
            for (_, replacement) in star.replace_list.iter_mut() {
                callback(replacement);
            }
        }
        ParsedExpressionKind::Subquery(subquery) => {
            if let Some(child) = &mut subquery.child {
                callback(child);
            }
        }
        ParsedExpressionKind::Window(window) => {
            window.partitions.iter_mut().for_each(&mut *callback);
            for order in &mut window.orders {
                callback(&mut order.expression);
            }
            window.children.iter_mut().for_each(&mut *callback);
            for child in [
                &mut window.filter_expr,
                &mut window.start_expr,
                &mut window.end_expr,
                &mut window.offset_expr,
                &mut window.default_expr,
            ]
            .into_iter()
            .flatten()
            {
                callback(child);
            }
            for order in &mut window.arg_orders {
                callback(&mut order.expression);
            }
        }
        ParsedExpressionKind::ColumnRef(_)
        | ParsedExpressionKind::LambdaRef(_)
        | ParsedExpressionKind::Constant(_)
        | ParsedExpressionKind::Default
        | ParsedExpressionKind::Parameter(_)
        | ParsedExpressionKind::PositionalReference(_)
        | ParsedExpressionKind::Bound(_) => {}
    }
}

/// Whether `predicate` holds for any direct child (immutable [`for_each_child_mut`]).
fn any_child(expr: &ParsedExpression, predicate: &dyn Fn(&ParsedExpression) -> bool) -> bool {
    let mut copy_free = false;
    // Children are visited through a shared view: clone-free traversal of the same
    // structure as `for_each_child_mut`.
    children(expr, &mut |child| copy_free |= predicate(child));
    copy_free
}

fn children(expr: &ParsedExpression, callback: &mut dyn FnMut(&ParsedExpression)) {
    match &expr.kind {
        ParsedExpressionKind::Between(between) => {
            callback(&between.input);
            callback(&between.lower);
            callback(&between.upper);
        }
        ParsedExpressionKind::Case(case) => {
            for check in &case.case_checks {
                callback(&check.when_expr);
                callback(&check.then_expr);
            }
            callback(&case.else_expr);
        }
        ParsedExpressionKind::Cast(cast) => callback(&cast.child),
        ParsedExpressionKind::Collate(collate) => callback(&collate.child),
        ParsedExpressionKind::Comparison(comparison) => {
            callback(&comparison.left);
            callback(&comparison.right);
        }
        ParsedExpressionKind::Conjunction(conjunction) => conjunction.children.iter().for_each(callback),
        ParsedExpressionKind::Function(function) => {
            function.children.iter().for_each(&mut *callback);
            if let Some(filter) = &function.filter {
                callback(filter);
            }
            for order in &function.order_bys.orders {
                callback(&order.expression);
            }
        }
        ParsedExpressionKind::Type(type_expr) => type_expr.children.iter().for_each(callback),
        ParsedExpressionKind::Lambda(lambda) => {
            callback(&lambda.lhs);
            callback(&lambda.expr);
        }
        ParsedExpressionKind::Operator(operator) => operator.children.iter().for_each(callback),
        ParsedExpressionKind::Star(star) => {
            if let Some(child) = &star.expr {
                callback(child);
            }
            for (_, replacement) in star.replace_list.iter() {
                callback(replacement);
            }
        }
        ParsedExpressionKind::Subquery(subquery) => {
            if let Some(child) = &subquery.child {
                callback(child);
            }
        }
        ParsedExpressionKind::Window(window) => {
            window.partitions.iter().for_each(&mut *callback);
            for order in &window.orders {
                callback(&order.expression);
            }
            window.children.iter().for_each(&mut *callback);
            for child in [
                &window.filter_expr,
                &window.start_expr,
                &window.end_expr,
                &window.offset_expr,
                &window.default_expr,
            ]
            .into_iter()
            .flatten()
            {
                callback(child);
            }
            for order in &window.arg_orders {
                callback(&order.expression);
            }
        }
        ParsedExpressionKind::ColumnRef(_)
        | ParsedExpressionKind::LambdaRef(_)
        | ParsedExpressionKind::Constant(_)
        | ParsedExpressionKind::Default
        | ParsedExpressionKind::Parameter(_)
        | ParsedExpressionKind::PositionalReference(_)
        | ParsedExpressionKind::Bound(_) => {}
    }
}

/// `ParsedExpression::IsScalar`: no column, subquery or positional references.
pub fn is_scalar(expr: &ParsedExpression) -> bool {
    match &expr.kind {
        ParsedExpressionKind::ColumnRef(_)
        | ParsedExpressionKind::Subquery(_)
        | ParsedExpressionKind::PositionalReference(_)
        | ParsedExpressionKind::Default
        | ParsedExpressionKind::LambdaRef(_) => false,
        ParsedExpressionKind::Parameter(_) => true,
        _ => !any_child(expr, &|child| !is_scalar(child)),
    }
}

/// `ParsedExpression::HasSubquery`.
pub fn has_subquery(expr: &ParsedExpression) -> bool {
    match &expr.kind {
        ParsedExpressionKind::Subquery(_) => true,
        _ => any_child(expr, &has_subquery),
    }
}

/// A hash consistent with [`expressions_equal`]: equal expressions hash equally.
pub(crate) fn expression_hash(expr: &ParsedExpression) -> u64 {
    let mut hasher = DefaultHasher::new();
    (expr.expression_type as u8).hash(&mut hasher);
    (expr.expression_class() as u8).hash(&mut hasher);
    match &expr.kind {
        ParsedExpressionKind::ColumnRef(column_ref) => {
            for name in &column_ref.column_names {
                name.to_ascii_lowercase().hash(&mut hasher);
            }
        }
        ParsedExpressionKind::Function(function) => function.function_name.hash(&mut hasher),
        _ => {}
    }
    hasher.finish()
}

fn optional_equal(a: Option<&ParsedExpression>, b: Option<&ParsedExpression>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => expressions_equal(a, b),
        _ => false,
    }
}

fn list_equal(a: &[ParsedExpression], b: &[ParsedExpression]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| expressions_equal(a, b))
}

/// `ExpressionUtil::SetEquals`: equal as multisets.
fn set_equal(a: &[ParsedExpression], b: &[ParsedExpression]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut remaining: Vec<&ParsedExpression> = a.iter().collect();
    for expr in b {
        let Some(position) = remaining
            .iter()
            .position(|candidate| expressions_equal(candidate, expr))
        else {
            return false;
        };
        remaining.swap_remove(position);
    }
    true
}

fn orders_equal(a: &[OrderByNode], b: &[OrderByNode]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.order_type == b.order_type
                && a.null_order == b.null_order
                && expressions_equal(&a.expression, &b.expression)
        })
}

/// `OrderModifier::Equals`: only the directions and the expressions' classes and types.
fn order_modifiers_equal(a: &OrderModifier, b: &OrderModifier) -> bool {
    a.orders.len() == b.orders.len()
        && a.orders.iter().zip(&b.orders).all(|(a, b)| {
            a.order_type == b.order_type
                && a.expression.expression_class() == b.expression.expression_class()
                && a.expression.expression_type == b.expression.expression_type
        })
}

fn qualified_sets_equal(a: &[QualifiedColumnName], b: &[QualifiedColumnName]) -> bool {
    a.len() == b.len() && a.iter().all(|name| b.contains(name))
}

fn stars_equal(a: &StarExpression, b: &StarExpression) -> bool {
    if a.relation_name != b.relation_name
        || !qualified_sets_equal(&a.exclude_list, &b.exclude_list)
        || a.rename_list.len() != b.rename_list.len()
        || a.rename_list
            .iter()
            .any(|(key, value)| b.rename_list.get(key) != Some(value))
        || a.columns != b.columns
        || a.replace_list.len() != b.replace_list.len()
    {
        return false;
    }
    for (name, expr) in a.replace_list.iter() {
        match b.replace_list.get(name) {
            Some(other) if expressions_equal(expr, other) => {}
            _ => return false,
        }
    }
    optional_equal(a.expr.as_deref(), b.expr.as_deref())
}

fn unbound_equal(a: &UnboundTypeInfo, b: &UnboundTypeInfo) -> bool {
    a.catalog == b.catalog
        && a.schema == b.schema
        && a.name == b.name
        && a.parameters.len() == b.parameters.len()
        && a.parameters
            .iter()
            .zip(&b.parameters)
            .all(|(a, b)| parameters_equal(a, b))
}

fn parameters_equal(a: &TypeParameter, b: &TypeParameter) -> bool {
    match (&a.value, &b.value) {
        (TypeParameterValue::Type(a), TypeParameterValue::Type(b)) => unbound_equal(a, b),
        (TypeParameterValue::Constant(a), TypeParameterValue::Constant(b)) => values_equal(a, b),
        _ => false,
    }
}

/// `LogicalType::operator==`, where unbound types compare their type expressions.
fn types_equal(a: &LogicalType, b: &LogicalType) -> bool {
    match (a.info(), b.info()) {
        (Some(TypeInfo::Unbound(a_info)), Some(TypeInfo::Unbound(b_info))) => {
            a.id() == b.id() && unbound_equal(a_info, b_info)
        }
        _ => a == b,
    }
}

/// `ConstantExpression::Equal`: same type and not distinct.
fn values_equal(a: &Value, b: &Value) -> bool {
    a.logical_type() == b.logical_type() && a.compare_not_distinct(b) == Some(Ordering::Equal)
}

/// `SelectStatement::Equals`, approximated by comparing the trees without query
/// locations.
fn statements_equal(a: &SelectStatement, b: &SelectStatement) -> bool {
    struct ClearLocations;
    impl VisitorMut for ClearLocations {
        fn visit_expression(&mut self, expression: &mut ParsedExpression) {
            expression.query_location = None;
            walk_expression(self, expression);
        }
        fn visit_table_ref(&mut self, table_ref: &mut TableRef) {
            table_ref.query_location = None;
            walk_table_ref(self, table_ref);
        }
    }
    let mut a = a.clone();
    let mut b = b.clone();
    ClearLocations.visit_query_node(&mut a.node);
    ClearLocations.visit_query_node(&mut b.node);
    a == b
}

/// `ParsedExpression::Equals`.
pub(crate) fn expressions_equal(a: &ParsedExpression, b: &ParsedExpression) -> bool {
    if a.expression_class() != b.expression_class() || a.expression_type != b.expression_type {
        return false;
    }
    match (&a.kind, &b.kind) {
        (ParsedExpressionKind::Between(a), ParsedExpressionKind::Between(b)) => {
            expressions_equal(&a.input, &b.input)
                && expressions_equal(&a.lower, &b.lower)
                && expressions_equal(&a.upper, &b.upper)
        }
        (ParsedExpressionKind::Case(a), ParsedExpressionKind::Case(b)) => {
            a.case_checks.len() == b.case_checks.len()
                && a.case_checks.iter().zip(&b.case_checks).all(|(a, b)| {
                    expressions_equal(&a.when_expr, &b.when_expr)
                        && expressions_equal(&a.then_expr, &b.then_expr)
                })
                && expressions_equal(&a.else_expr, &b.else_expr)
        }
        (ParsedExpressionKind::Cast(a), ParsedExpressionKind::Cast(b)) => {
            expressions_equal(&a.child, &b.child)
                && types_equal(&a.cast_type, &b.cast_type)
                && a.try_cast == b.try_cast
        }
        (ParsedExpressionKind::Collate(a), ParsedExpressionKind::Collate(b)) => {
            expressions_equal(&a.child, &b.child) && a.collation == b.collation
        }
        (ParsedExpressionKind::ColumnRef(a), ParsedExpressionKind::ColumnRef(b)) => {
            a.column_names.len() == b.column_names.len()
                && a.column_names
                    .iter()
                    .zip(&b.column_names)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
        }
        (ParsedExpressionKind::Comparison(a), ParsedExpressionKind::Comparison(b)) => {
            expressions_equal(&a.left, &b.left) && expressions_equal(&a.right, &b.right)
        }
        (ParsedExpressionKind::Conjunction(a), ParsedExpressionKind::Conjunction(b)) => {
            set_equal(&a.children, &b.children)
        }
        (ParsedExpressionKind::Constant(a), ParsedExpressionKind::Constant(b)) => {
            values_equal(&a.value, &b.value)
        }
        (ParsedExpressionKind::Default, ParsedExpressionKind::Default) => true,
        (ParsedExpressionKind::Function(a), ParsedExpressionKind::Function(b)) => {
            a.catalog == b.catalog
                && a.schema == b.schema
                && a.function_name == b.function_name
                && a.distinct == b.distinct
                && list_equal(&a.children, &b.children)
                && optional_equal(a.filter.as_deref(), b.filter.as_deref())
                && order_modifiers_equal(&a.order_bys, &b.order_bys)
                && a.export_state == b.export_state
        }
        (ParsedExpressionKind::Lambda(a), ParsedExpressionKind::Lambda(b)) => {
            expressions_equal(&a.lhs, &b.lhs) && expressions_equal(&a.expr, &b.expr)
        }
        (ParsedExpressionKind::Operator(a), ParsedExpressionKind::Operator(b)) => {
            list_equal(&a.children, &b.children)
        }
        (ParsedExpressionKind::Parameter(a), ParsedExpressionKind::Parameter(b)) => {
            a.identifier.eq_ignore_ascii_case(&b.identifier)
        }
        (ParsedExpressionKind::PositionalReference(a), ParsedExpressionKind::PositionalReference(b)) => {
            a.index == b.index
        }
        (ParsedExpressionKind::Star(a), ParsedExpressionKind::Star(b)) => stars_equal(a, b),
        (ParsedExpressionKind::Subquery(a), ParsedExpressionKind::Subquery(b)) => {
            optional_equal(a.child.as_deref(), b.child.as_deref())
                && a.comparison_type == b.comparison_type
                && a.subquery_type == b.subquery_type
                && statements_equal(&a.subquery, &b.subquery)
        }
        (ParsedExpressionKind::Window(a), ParsedExpressionKind::Window(b)) => {
            a.ignore_nulls == b.ignore_nulls
                && a.distinct == b.distinct
                && list_equal(&a.children, &b.children)
                && a.start == b.start
                && a.end == b.end
                && a.exclude_clause == b.exclude_clause
                && optional_equal(a.start_expr.as_deref(), b.start_expr.as_deref())
                && optional_equal(a.end_expr.as_deref(), b.end_expr.as_deref())
                && optional_equal(a.offset_expr.as_deref(), b.offset_expr.as_deref())
                && optional_equal(a.default_expr.as_deref(), b.default_expr.as_deref())
                && orders_equal(&a.arg_orders, &b.arg_orders)
                && list_equal(&a.partitions, &b.partitions)
                && orders_equal(&a.orders, &b.orders)
                && optional_equal(a.filter_expr.as_deref(), b.filter_expr.as_deref())
        }
        (ParsedExpressionKind::Type(a), ParsedExpressionKind::Type(b)) => {
            a.catalog == b.catalog
                && a.schema == b.schema
                && a.type_name == b.type_name
                && list_equal(&a.children, &b.children)
        }
        (ParsedExpressionKind::LambdaRef(a), ParsedExpressionKind::LambdaRef(b)) => a == b,
        (ParsedExpressionKind::Bound(a), ParsedExpressionKind::Bound(b)) => a == b,
        _ => false,
    }
}
