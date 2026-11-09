//! `CASE` with constant conditions (DuckDB's `CaseSimplificationRule`): checks that are
//! always false are dropped, the first one that is always true becomes the ELSE.

use roastedduck_common::Result;
use roastedduck_planner::{Expression, ExpressionKind};

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::expression_util::{boolean_value, evaluate_scalar};

pub fn case_simplification(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    let ExpressionKind::Case(case) = &mut expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    let mut i = 0;
    while i < case.case_checks.len() {
        if !case.case_checks[i].when_expr.is_foldable() {
            i += 1;
            continue;
        }
        let value = evaluate_scalar(context.client, &case.case_checks[i].when_expr)?;
        if boolean_value(&value)? == Some(true) {
            // Always true: its THEN is the result when no earlier check matched.
            let check = case.case_checks.swap_remove(i);
            case.else_expr = check.then_expr;
            case.case_checks.truncate(i);
            break;
        }
        // Always false (or NULL): the check never matches.
        case.case_checks.remove(i);
    }
    if case.case_checks.is_empty() {
        let else_expr = std::mem::replace(
            &mut case.else_expr,
            Expression::constant(roastedduck_types::Value::default()),
        );
        return Ok(RuleOutcome::Replaced(else_expr));
    }
    Ok(RuleOutcome::Unchanged)
}
