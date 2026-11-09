//! Factoring common terms out of an OR: `(X AND A) OR (X AND B)` becomes
//! `X AND (A OR B)` (DuckDB's `DistributivityRule`).

use roastedduck_ast::ExpressionType;
use roastedduck_common::Result;
use roastedduck_planner::{Expression, ExpressionKind};

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;

/// The terms of an OR branch: the children of an AND, or the branch itself
/// (`AddExpressionSet`), without duplicates.
fn expression_set(expression: &Expression) -> Vec<&Expression> {
    let terms: Vec<&Expression> = match &expression.kind {
        ExpressionKind::Conjunction(conjunction)
            if expression.expression_type == ExpressionType::ConjunctionAnd =>
        {
            conjunction.children.iter().collect()
        }
        _ => vec![expression],
    };
    let mut set: Vec<&Expression> = Vec::new();
    for term in terms {
        if !set.iter().any(|existing| existing.equals(term)) {
            set.push(term);
        }
    }
    set
}

/// Removes `term` from OR branch `index` and returns it (`ExtractExpression`); a branch
/// that was only `term` becomes empty.
fn extract_expression(
    branches: &mut [Option<Expression>],
    index: usize,
    term: &Expression,
) -> Option<Expression> {
    let branch = branches[index].as_mut()?;
    let mut result = None;
    if branch.expression_type == ExpressionType::ConjunctionAnd
        && let ExpressionKind::Conjunction(conjunction) = &mut branch.kind
    {
        if let Some(position) = conjunction.children.iter().position(|child| child.equals(term)) {
            result = Some(conjunction.children.remove(position));
        }
        if conjunction.children.len() == 1 {
            let single = conjunction.children.pop();
            branches[index] = single;
        }
    }
    if result.is_none() {
        result = branches[index].take();
    }
    result
}

/// `X` of `(X AND A) OR (X AND B)` taken out of the OR.
pub fn distributivity(_: &RuleContext<'_>, expression: &mut Expression, _: bool) -> Result<RuleOutcome> {
    if expression.expression_type != ExpressionType::ConjunctionOr {
        return Ok(RuleOutcome::Unchanged);
    }
    let ExpressionKind::Conjunction(conjunction) = &expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    let Some(first) = conjunction.children.first() else {
        return Ok(RuleOutcome::Unchanged);
    };
    let mut candidates: Vec<Expression> = expression_set(first).into_iter().cloned().collect();
    for child in &conjunction.children[1..] {
        let next = expression_set(child);
        candidates.retain(|candidate| next.iter().any(|term| term.equals(candidate)));
    }
    if candidates.is_empty() {
        return Ok(RuleOutcome::Unchanged);
    }
    let ExpressionKind::Conjunction(conjunction) =
        std::mem::replace(&mut expression.kind, ExpressionKind::Default)
    else {
        return Ok(RuleOutcome::Unchanged);
    };
    let mut branches: Vec<Option<Expression>> = conjunction.children.into_iter().map(Some).collect();
    let mut new_root = Vec::new();
    for candidate in &candidates {
        let Some(result) = extract_expression(&mut branches, 0, candidate) else {
            continue;
        };
        for index in 1..branches.len() {
            extract_expression(&mut branches, index, &result);
        }
        new_root.push(result);
    }
    let and = |mut children: Vec<Expression>| {
        if children.len() == 1 {
            children.remove(0)
        } else {
            Expression::conjunction(ExpressionType::ConjunctionAnd, children)
        }
    };
    if branches.iter().any(Option::is_none) {
        // `X OR (X AND A)` is `X`.
        return Ok(RuleOutcome::Replaced(and(new_root)));
    }
    let mut remaining: Vec<Expression> = branches.into_iter().flatten().collect();
    if remaining.len() == 1 {
        new_root.push(remaining.remove(0));
    } else if remaining.len() > 1 {
        new_root.push(Expression::conjunction(ExpressionType::ConjunctionOr, remaining));
    }
    Ok(RuleOutcome::Replaced(and(new_root)))
}
