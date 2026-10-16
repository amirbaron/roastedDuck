//! Comparing two ENUMs through VARCHAR casts (DuckDB's `EnumComparisonRule`): enums
//! without common members never compare equal, and in a filter the cast to VARCHAR
//! becomes a cast of one enum to the other.

use rustdb_ast::ExpressionType;
use rustdb_common::Result;
use rustdb_planner::{Expression, ExpressionKind, LogicalOperatorType};
use rustdb_types::{LogicalType, LogicalTypeId, TypeInfo, Value};

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::expression_util::constant_or_null;

/// The members of an ENUM type.
fn enum_values(ty: &LogicalType) -> &[String] {
    match ty.info() {
        Some(TypeInfo::Enum(info)) => &info.values,
        _ => &[],
    }
}

/// Whether the two enums share a member (DuckDB's `AreMatchesPossible`).
fn are_matches_possible(left: &LogicalType, right: &LogicalType) -> bool {
    let (left, right) = (enum_values(left), enum_values(right));
    let (small, big) = if left.len() < right.len() {
        (left, right)
    } else {
        (right, left)
    };
    small.iter().any(|value| big.contains(value))
}

/// The child of a VARCHAR cast of an ENUM.
fn enum_cast_child(expression: &Expression) -> Option<&Expression> {
    match &expression.kind {
        ExpressionKind::Cast(cast)
            if expression.return_type.id() == LogicalTypeId::Varchar
                && cast.child.return_type.id() == LogicalTypeId::Enum =>
        {
            Some(&cast.child)
        }
        _ => None,
    }
}

pub fn enum_comparison(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    is_root: bool,
) -> Result<RuleOutcome> {
    if expression.expression_type != ExpressionType::CompareEqual {
        return Ok(RuleOutcome::Unchanged);
    }
    let ExpressionKind::Comparison(comparison) = &expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    let (Some(left), Some(right)) = (
        enum_cast_child(&comparison.left),
        enum_cast_child(&comparison.right),
    ) else {
        return Ok(RuleOutcome::Unchanged);
    };
    if !are_matches_possible(&left.return_type, &right.return_type) {
        return Ok(RuleOutcome::Replaced(constant_or_null(
            context.client,
            vec![comparison.left.clone(), comparison.right.clone()],
            Value::boolean(false),
        )?));
    }
    if !is_root || context.op_type != LogicalOperatorType::LogicalFilter {
        return Ok(RuleOutcome::Unchanged);
    }
    let cast_left = rustdb_planner::function_binder::add_cast_to_type(
        context.client,
        left.clone(),
        &right.return_type,
        true,
    )?;
    Ok(RuleOutcome::Replaced(Expression::comparison(
        ExpressionType::CompareEqual,
        cast_left,
        right.clone(),
    )))
}
