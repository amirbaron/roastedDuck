//! Walking a parse tree mutably (DuckDB's `ParsedExpressionIterator`).
//!
//! Implement [`VisitorMut`], override the `visit_*` methods of interest, and call the
//! matching `walk_*` function from them to continue into the children:
//!
//! ```
//! use rustdb_ast::visit::{VisitorMut, walk_table_ref};
//! use rustdb_ast::TableRef;
//!
//! /// Drops all table aliases.
//! struct ClearAliases;
//!
//! impl VisitorMut for ClearAliases {
//!     fn visit_table_ref(&mut self, table_ref: &mut TableRef) {
//!         table_ref.alias.clear();
//!         walk_table_ref(self, table_ref);
//!     }
//! }
//! ```

use crate::{
    CommonTableExpressionMap, OrderByNode, ParsedExpression, ParsedExpressionKind, PivotColumn, QueryNode,
    QueryNodeKind, ResultModifier, SelectStatement, TableRef, TableRefKind,
};

/// A mutable visit of expressions, query nodes and table refs, including those nested
/// in subqueries, CTEs, joins and set operations.
pub trait VisitorMut {
    /// Visits an expression; the default continues into its children.
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        walk_expression(self, expression);
    }

    /// Visits a query node; the default continues into its children.
    fn visit_query_node(&mut self, node: &mut QueryNode) {
        walk_query_node(self, node);
    }

    /// Visits a table ref; the default continues into its children.
    fn visit_table_ref(&mut self, table_ref: &mut TableRef) {
        walk_table_ref(self, table_ref);
    }
}

fn visit_all<V: VisitorMut + ?Sized>(visitor: &mut V, expressions: &mut [ParsedExpression]) {
    for expression in expressions {
        visitor.visit_expression(expression);
    }
}

fn visit_optional<V: VisitorMut + ?Sized>(visitor: &mut V, expression: &mut Option<Box<ParsedExpression>>) {
    if let Some(expression) = expression {
        visitor.visit_expression(expression);
    }
}

fn visit_orders<V: VisitorMut + ?Sized>(visitor: &mut V, orders: &mut [OrderByNode]) {
    for order in orders {
        visitor.visit_expression(&mut order.expression);
    }
}

fn visit_statement<V: VisitorMut + ?Sized>(visitor: &mut V, statement: &mut SelectStatement) {
    visitor.visit_query_node(&mut statement.node);
}

/// Visits the children of an expression.
pub fn walk_expression<V: VisitorMut + ?Sized>(visitor: &mut V, expression: &mut ParsedExpression) {
    match &mut expression.kind {
        ParsedExpressionKind::Between(between) => {
            visitor.visit_expression(&mut between.input);
            visitor.visit_expression(&mut between.lower);
            visitor.visit_expression(&mut between.upper);
        }
        ParsedExpressionKind::Case(case) => {
            for check in &mut case.case_checks {
                visitor.visit_expression(&mut check.when_expr);
                visitor.visit_expression(&mut check.then_expr);
            }
            visitor.visit_expression(&mut case.else_expr);
        }
        ParsedExpressionKind::Cast(cast) => visitor.visit_expression(&mut cast.child),
        ParsedExpressionKind::Collate(collate) => visitor.visit_expression(&mut collate.child),
        ParsedExpressionKind::Comparison(comparison) => {
            visitor.visit_expression(&mut comparison.left);
            visitor.visit_expression(&mut comparison.right);
        }
        ParsedExpressionKind::Conjunction(conjunction) => visit_all(visitor, &mut conjunction.children),
        ParsedExpressionKind::Function(function) => {
            visit_all(visitor, &mut function.children);
            visit_optional(visitor, &mut function.filter);
            visit_orders(visitor, &mut function.order_bys.orders);
        }
        ParsedExpressionKind::Lambda(lambda) => {
            visitor.visit_expression(&mut lambda.lhs);
            visitor.visit_expression(&mut lambda.expr);
        }
        ParsedExpressionKind::Operator(operator) => visit_all(visitor, &mut operator.children),
        ParsedExpressionKind::Star(star) => {
            for (_, replacement) in star.replace_list.iter_mut() {
                visitor.visit_expression(replacement);
            }
            visit_optional(visitor, &mut star.expr);
        }
        ParsedExpressionKind::Subquery(subquery) => {
            visit_statement(visitor, &mut subquery.subquery);
            visit_optional(visitor, &mut subquery.child);
        }
        ParsedExpressionKind::Type(ty) => visit_all(visitor, &mut ty.children),
        ParsedExpressionKind::Window(window) => {
            visit_all(visitor, &mut window.children);
            visit_all(visitor, &mut window.partitions);
            visit_orders(visitor, &mut window.orders);
            for bound in [
                &mut window.start_expr,
                &mut window.end_expr,
                &mut window.offset_expr,
                &mut window.default_expr,
                &mut window.filter_expr,
            ] {
                visit_optional(visitor, bound);
            }
            visit_orders(visitor, &mut window.arg_orders);
        }
        ParsedExpressionKind::ColumnRef(_)
        | ParsedExpressionKind::Constant(_)
        | ParsedExpressionKind::Default
        | ParsedExpressionKind::LambdaRef(_)
        | ParsedExpressionKind::Parameter(_)
        | ParsedExpressionKind::PositionalReference(_)
        | ParsedExpressionKind::Bound(_) => {}
    }
}

fn walk_cte_map<V: VisitorMut + ?Sized>(visitor: &mut V, cte_map: &mut CommonTableExpressionMap) {
    for (_, cte) in cte_map.map.iter_mut() {
        visit_statement(visitor, &mut cte.query);
        visit_all(visitor, &mut cte.key_targets);
    }
}

/// Visits the children of a query node: modifiers, CTEs and the node's own clauses.
pub fn walk_query_node<V: VisitorMut + ?Sized>(visitor: &mut V, node: &mut QueryNode) {
    for modifier in &mut node.modifiers {
        match modifier {
            ResultModifier::Limit(limit) => {
                visit_optional(visitor, &mut limit.limit);
                visit_optional(visitor, &mut limit.offset);
            }
            ResultModifier::LimitPercent(limit) => {
                visit_optional(visitor, &mut limit.limit);
                visit_optional(visitor, &mut limit.offset);
            }
            ResultModifier::Distinct(distinct) => visit_all(visitor, &mut distinct.distinct_on_targets),
            ResultModifier::Order(order) => visit_orders(visitor, &mut order.orders),
        }
    }
    walk_cte_map(visitor, &mut node.cte_map);
    match &mut node.kind {
        QueryNodeKind::Select(select) => {
            visit_all(visitor, &mut select.select_list);
            visitor.visit_table_ref(&mut select.from_table);
            visit_optional(visitor, &mut select.where_clause);
            visit_all(visitor, &mut select.group_expressions);
            visit_optional(visitor, &mut select.having);
            visit_optional(visitor, &mut select.qualify);
        }
        QueryNodeKind::SetOperation(set_operation) => {
            for child in &mut set_operation.children {
                visitor.visit_query_node(child);
            }
        }
        QueryNodeKind::RecursiveCte(cte) => {
            visitor.visit_query_node(&mut cte.left);
            visitor.visit_query_node(&mut cte.right);
            visit_all(visitor, &mut cte.key_targets);
        }
    }
}

fn walk_pivot<V: VisitorMut + ?Sized>(visitor: &mut V, pivot: &mut PivotColumn) {
    visit_all(visitor, &mut pivot.pivot_expressions);
    for entry in &mut pivot.entries {
        visit_optional(visitor, &mut entry.star_expr);
    }
}

/// Visits the children of a table ref.
pub fn walk_table_ref<V: VisitorMut + ?Sized>(visitor: &mut V, table_ref: &mut TableRef) {
    match &mut table_ref.kind {
        TableRefKind::BaseTable(table) => {
            if let Some(at) = &mut table.at_clause {
                visitor.visit_expression(&mut at.expr);
            }
        }
        TableRefKind::Join(join) => {
            visitor.visit_table_ref(&mut join.left);
            visitor.visit_table_ref(&mut join.right);
            visit_optional(visitor, &mut join.condition);
            visit_all(visitor, &mut join.duplicate_eliminated_columns);
        }
        TableRefKind::Subquery(subquery) => visit_statement(visitor, &mut subquery.subquery),
        TableRefKind::TableFunction(function) => visitor.visit_expression(&mut function.function),
        TableRefKind::ExpressionList(list) => {
            for row in &mut list.values {
                visit_all(visitor, row);
            }
        }
        TableRefKind::Pivot(pivot) => {
            visitor.visit_table_ref(&mut pivot.source);
            visit_all(visitor, &mut pivot.aggregates);
            for column in &mut pivot.pivots {
                walk_pivot(visitor, column);
            }
        }
        TableRefKind::Show(show) => {
            if let Some(query) = &mut show.query {
                visitor.visit_query_node(query);
            }
        }
        TableRefKind::Empty => {}
    }
}
