//! Constant folding (DuckDB's `ConstantFoldingRule`) and the normalization that moves
//! the constants of `+`/`*` chains to the front so that they fold together (DuckDB's
//! `ConstantOrderNormalizationRule`).

use rustdb_ast::ExpressionType;
use rustdb_common::Result;
use rustdb_planner::{Expression, ExpressionKind};

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::expression_util::try_evaluate_scalar;
use crate::optimizer::bind_scalar_function;

/// Folds a foldable, non-constant expression into its value.
pub fn constant_folding(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    if expression.expression_type == ExpressionType::ValueConstant || !expression.is_foldable() {
        return Ok(RuleOutcome::Unchanged);
    }
    Ok(match try_evaluate_scalar(context.client, expression) {
        Some(value) => RuleOutcome::Replaced(Expression::constant(value)),
        None => RuleOutcome::Unchanged,
    })
}

/// Whether `expression` is a two-argument integral `name` call with integral arguments
/// (the `FunctionExpressionMatcher` of the normalization rule).
fn is_integral_call(expression: &Expression, name: &str) -> bool {
    let ExpressionKind::Function(function) = &expression.kind else {
        return false;
    };
    &*function.function.name == name
        && expression.return_type.is_integral()
        && function.children.len() == 2
        && function
            .children
            .iter()
            .all(|child| child.return_type.is_integral())
}

/// The operands of a nested chain of the same commutative operator, left to right
/// (DuckDB's `RecursiveFunctionExpressionMatcher::RecursiveMatch`).
fn collect_operands(expression: Expression, name: &str, operands: &mut Vec<Expression>) {
    if !is_integral_call(&expression, name) {
        operands.push(expression);
        return;
    }
    if let ExpressionKind::Function(function) = expression.kind {
        for child in function.children {
            collect_operands(child, name, operands);
        }
    }
}

/// Puts the foldable operands of an integral `+` or `*` chain first.
pub fn constant_order_normalization(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    let Some(name) = ["+", "*"]
        .into_iter()
        .find(|name| is_integral_call(expression, name))
    else {
        return Ok(RuleOutcome::Unchanged);
    };
    let (is_operator, return_type) = match &expression.kind {
        ExpressionKind::Function(function) => (function.is_operator, expression.return_type.clone()),
        _ => return Ok(RuleOutcome::Unchanged),
    };
    // Check the order on a borrowed view first: only rebuild when it changes.
    let mut operands = Vec::new();
    collect_operands(expression.clone(), name, &mut operands);
    let mut last_constant_position = 0;
    let mut constant_count = 0;
    for (i, operand) in operands.iter().enumerate() {
        if operand.is_foldable() {
            constant_count += 1;
            last_constant_position = i;
        }
    }
    if constant_count <= 1 || last_constant_position == constant_count - 1 {
        return Ok(RuleOutcome::Unchanged);
    }
    let (mut ordered, remaining): (Vec<Expression>, Vec<Expression>) =
        operands.into_iter().partition(Expression::is_foldable);
    ordered.extend(remaining);
    let mut operands = ordered.into_iter();
    let Some(mut root) = operands.next() else {
        return Ok(RuleOutcome::Unchanged);
    };
    for operand in operands {
        root = bind_scalar_function(context.client, name, vec![root, operand], is_operator)?;
    }
    debug_assert_eq!(root.return_type, return_type);
    Ok(RuleOutcome::Replaced(root))
}
