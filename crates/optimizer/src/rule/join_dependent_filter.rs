//! Single-table filters derived from a join-dependent OR (DuckDB's
//! `JoinDependentFilterRule`): from `(a.x = 1 AND b.y = 2) OR (a.x = 3 AND b.y = 4)` it
//! adds `(a.x = 1 OR a.x = 3) AND (b.y = 2 OR b.y = 4)`, which can be pushed down.

use std::collections::BTreeMap;

use roastedduck_ast::ExpressionType;
use roastedduck_common::Result;
use roastedduck_planner::{Expression, ExpressionKind, LogicalOperatorType};

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::expression_util::table_indexes;

fn conjunction_children(expression: &Expression) -> Option<&[Expression]> {
    match &expression.kind {
        ExpressionKind::Conjunction(conjunction) => Some(&conjunction.children),
        _ => None,
    }
}

/// The AND-ed terms of `expression` that reference exactly one table, AND-ed together per
/// table (`ExtractConjunctedExpressions`).
fn extract_conjuncted(expression: &Expression, expressions: &mut BTreeMap<u64, Expression>) {
    if expression.expression_type == ExpressionType::ConjunctionAnd
        && let Some(children) = conjunction_children(expression)
    {
        for child in children {
            extract_conjuncted(child, expressions);
        }
        return;
    }
    if expression.is_volatile() {
        return;
    }
    let tables = table_indexes(expression);
    let [table] = tables.as_slice() else {
        return;
    };
    let combined = match expressions.remove(table) {
        Some(existing) => {
            Expression::conjunction(ExpressionType::ConjunctionAnd, vec![existing, expression.clone()])
        }
        None => expression.clone(),
    };
    expressions.insert(*table, combined);
}

pub fn join_dependent_filter(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    is_root: bool,
) -> Result<RuleOutcome> {
    if !is_root
        || !matches!(
            context.op_type,
            LogicalOperatorType::LogicalFilter | LogicalOperatorType::LogicalAnyJoin
        )
        || expression.expression_type != ExpressionType::ConjunctionOr
    {
        return Ok(RuleOutcome::Unchanged);
    }
    let Some(children) = conjunction_children(expression) else {
        return Ok(RuleOutcome::Unchanged);
    };
    // The matcher: at least one child is a conjunction.
    if !children
        .iter()
        .any(|child| matches!(child.kind, ExpressionKind::Conjunction(_)))
    {
        return Ok(RuleOutcome::Unchanged);
    }
    let eligible = children.iter().any(|child| {
        child.expression_type == ExpressionType::ConjunctionAnd && table_indexes(child).len() > 1
    });
    if !eligible {
        return Ok(RuleOutcome::Unchanged);
    }
    let conjuncted: Vec<BTreeMap<u64, Expression>> = children
        .iter()
        .map(|child| {
            let mut expressions = BTreeMap::new();
            extract_conjuncted(child, &mut expressions);
            expressions
        })
        .collect();
    let mut derived = Vec::new();
    for (table, first) in &conjuncted[0] {
        let mut alternatives = vec![first.clone()];
        let found = conjuncted[1..].iter().all(|other| match other.get(table) {
            Some(expression) => {
                alternatives.push(expression.clone());
                true
            }
            None => false,
        });
        if found {
            derived.push(Expression::conjunction(
                ExpressionType::ConjunctionOr,
                alternatives,
            ));
        }
    }
    if derived.is_empty() {
        return Ok(RuleOutcome::Unchanged);
    }
    let derived = if derived.len() == 1 {
        derived.remove(0)
    } else {
        Expression::conjunction(ExpressionType::ConjunctionAnd, derived)
    };
    Ok(RuleOutcome::Replaced(Expression::conjunction(
        ExpressionType::ConjunctionAnd,
        vec![expression.clone(), derived],
    )))
}
