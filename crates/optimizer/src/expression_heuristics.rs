//! Ordering filter predicates and conjunction children by estimated cost (DuckDB's
//! `ExpressionHeuristics`), so that cheap predicates run first and short-circuit
//! expensive ones.

use rustdb_ast::ExpressionType;
use rustdb_planner::{Expression, ExpressionKind, LogicalOperator, LogicalOperatorType};
use rustdb_types::{LogicalTypeId, PhysicalType};

use crate::expression_util::can_throw;

/// Reorders the filters and conjunctions of `plan` (DuckDB's
/// `ExpressionHeuristics::Rewrite`).
pub fn reorder_filters(op: &mut LogicalOperator) {
    if op.operator_type() == LogicalOperatorType::LogicalFilter && op.expressions.len() > 1 {
        reorder_expressions(&mut op.expressions);
    }
    for child in &mut op.children {
        reorder_filters(child);
    }
    op.for_each_expression_mut(visit_expression);
}

/// Reorders conjunctions top-down (DuckDB's `VisitReplace(BoundConjunctionExpression)`).
fn visit_expression(expression: &mut Expression) {
    if let ExpressionKind::Conjunction(conjunction) = &mut expression.kind {
        reorder_expressions(&mut conjunction.children);
    }
    expression.for_each_child_mut(visit_expression);
}

/// Sorts `expressions` by cost, unless one of them can throw (DuckDB's
/// `ReorderExpressions`).
fn reorder_expressions(expressions: &mut Vec<Expression>) {
    if expressions.iter().any(can_throw) {
        return;
    }
    let mut costed: Vec<(u64, Expression)> = std::mem::take(expressions)
        .into_iter()
        .map(|expression| (cost(&expression), expression))
        .collect();
    costed.sort_by_key(|(cost, _)| *cost);
    *expressions = costed.into_iter().map(|(_, expression)| expression).collect();
}

/// The cost of reading a value of a physical type (DuckDB's
/// `ExpressionCost(PhysicalType, multiplier)`).
const fn type_cost(physical: PhysicalType, multiplier: u64) -> u64 {
    match physical {
        PhysicalType::Varchar => 5 * multiplier,
        PhysicalType::Float | PhysicalType::Double => 2 * multiplier,
        _ => multiplier,
    }
}

/// The cost of a function call (DuckDB's `ExpressionCost(BoundFunctionExpression)`).
fn function_cost(name: &str) -> u64 {
    match name {
        "+" | "-" | "&" | "#" | ">>" | "<<" | "abs" => 5,
        "*" | "%" => 10,
        "/" => 15,
        "date_part" | "year" => 20,
        "round" => 100,
        "~~" | "!~~" | "regexp_matches" | "||" => 200,
        _ => 1000,
    }
}

/// The estimated cost of evaluating `expression` (DuckDB's `ExpressionHeuristics::Cost`).
fn cost(expression: &Expression) -> u64 {
    match &expression.kind {
        ExpressionKind::Case(case) => {
            case.case_checks
                .iter()
                .map(|check| cost(&check.then_expr) + cost(&check.when_expr))
                .sum::<u64>()
                + cost(&case.else_expr)
        }
        ExpressionKind::Between(between) => {
            cost(&between.input) + cost(&between.lower) + cost(&between.upper) + 10
        }
        ExpressionKind::Cast(cast) => {
            let source = &cast.child.return_type;
            let target = &expression.return_type;
            let cast_cost = if target == source {
                0
            } else if [target.id(), source.id()]
                .iter()
                .any(|id| matches!(id, LogicalTypeId::Varchar | LogicalTypeId::Blob))
            {
                200
            } else {
                5
            };
            cost(&cast.child) + cast_cost
        }
        ExpressionKind::Comparison(comparison) => cost(&comparison.left) + 5 + cost(&comparison.right),
        ExpressionKind::Conjunction(conjunction) => 5 + conjunction.children.iter().map(cost).sum::<u64>(),
        ExpressionKind::Function(function) => {
            function.children.iter().map(cost).sum::<u64>() + function_cost(&function.function.name)
        }
        ExpressionKind::Operator(operator) => {
            let sum: u64 = operator.children.iter().map(cost).sum();
            match expression.expression_type {
                ExpressionType::OperatorIsNull | ExpressionType::OperatorIsNotNull => sum + 5,
                ExpressionType::CompareIn | ExpressionType::CompareNotIn => {
                    sum + (operator.children.len().saturating_sub(1) as u64) * 100
                }
                ExpressionType::OperatorNot => sum + 10,
                _ => sum + 1000,
            }
        }
        ExpressionKind::ColumnRef(_) | ExpressionKind::Reference(_) => {
            type_cost(expression.return_type.physical_type(), 8)
        }
        ExpressionKind::Constant(_) | ExpressionKind::Parameter(_) => {
            type_cost(expression.return_type.physical_type(), 1)
        }
        _ => 1000,
    }
}
