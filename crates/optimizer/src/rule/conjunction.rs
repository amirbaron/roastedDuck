//! AND/OR with constant terms (DuckDB's `ConjunctionSimplificationRule`) and
//! `a = b OR (a IS NULL AND b IS NULL)` as `a IS NOT DISTINCT FROM b` (DuckDB's
//! `EqualOrNullSimplification`).

use rustdb_ast::ExpressionType;
use rustdb_common::Result;
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_types::Value;

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::expression_util::{boolean_value, try_evaluate_scalar};

pub fn conjunction_simplification(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    let is_and = expression.expression_type == ExpressionType::ConjunctionAnd;
    let ExpressionKind::Conjunction(conjunction) = &mut expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    // SOME policy with one foldable matcher: the first foldable child.
    let Some(index) = conjunction.children.iter().position(Expression::is_foldable) else {
        return Ok(RuleOutcome::Unchanged);
    };
    let Some(value) = try_evaluate_scalar(context.client, &conjunction.children[index]) else {
        return Ok(RuleOutcome::Unchanged);
    };
    let Some(value) = boolean_value(&value)? else {
        // A constant NULL cannot be simplified away.
        return Ok(RuleOutcome::Unchanged);
    };
    if value != is_and {
        // FALSE in an AND, TRUE in an OR: the result is that constant.
        return Ok(RuleOutcome::Replaced(Expression::constant(Value::boolean(value))));
    }
    // TRUE in an AND, FALSE in an OR: the term does not matter (`RemoveExpression`).
    conjunction.children.remove(index);
    if conjunction.children.len() == 1 {
        return Ok(RuleOutcome::Replaced(conjunction.children.remove(0)));
    }
    Ok(RuleOutcome::Unchanged)
}

/// The child of `IS NULL`.
fn is_null_child(expression: &Expression) -> Option<&Expression> {
    if expression.expression_type != ExpressionType::OperatorIsNull {
        return None;
    }
    match &expression.kind {
        ExpressionKind::Operator(operator) => operator.children.first(),
        _ => None,
    }
}

/// `a = b` and `a IS NULL AND b IS NULL` as `a IS NOT DISTINCT FROM b`
/// (`TryRewriteEqualOrIsNull`).
fn try_rewrite_equal_or_is_null(equal: &Expression, and: &Expression) -> Option<Expression> {
    if equal.expression_type != ExpressionType::CompareEqual
        || and.expression_type != ExpressionType::ConjunctionAnd
    {
        return None;
    }
    let (ExpressionKind::Comparison(comparison), ExpressionKind::Conjunction(conjunction)) =
        (&equal.kind, &and.kind)
    else {
        return None;
    };
    if conjunction.children.len() != 2 {
        return None;
    }
    let (mut a_is_null_found, mut b_is_null_found) = (false, false);
    for item in &conjunction.children {
        let child = is_null_child(item)?;
        if child.equals(&comparison.left) {
            a_is_null_found = true;
        } else if child.equals(&comparison.right) {
            b_is_null_found = true;
        } else {
            return None;
        }
    }
    (a_is_null_found && b_is_null_found).then(|| {
        Expression::comparison(
            ExpressionType::CompareNotDistinctFrom,
            comparison.left.clone(),
            comparison.right.clone(),
        )
    })
}

/// Whether an AND has at least two `IS NULL` children (the rule's matcher).
fn has_two_is_null(expression: &Expression) -> bool {
    expression.expression_type == ExpressionType::ConjunctionAnd
        && matches!(&expression.kind, ExpressionKind::Conjunction(conjunction)
            if conjunction.children.iter().filter(|child| child.expression_type == ExpressionType::OperatorIsNull).count() >= 2)
}

pub fn equal_or_null_simplification(
    _: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    if expression.expression_type != ExpressionType::ConjunctionOr {
        return Ok(RuleOutcome::Unchanged);
    }
    let ExpressionKind::Conjunction(conjunction) = &expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    let has_equal = conjunction.children.iter().any(|child| {
        child.expression_type == ExpressionType::CompareEqual
            && matches!(child.kind, ExpressionKind::Comparison(_))
    });
    if !has_equal || !conjunction.children.iter().any(has_two_is_null) || conjunction.children.len() != 2 {
        return Ok(RuleOutcome::Unchanged);
    }
    let (left, right) = (&conjunction.children[0], &conjunction.children[1]);
    let rewritten =
        try_rewrite_equal_or_is_null(left, right).or_else(|| try_rewrite_equal_or_is_null(right, left));
    Ok(rewritten.map_or(RuleOutcome::Unchanged, RuleOutcome::Replaced))
}
