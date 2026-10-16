//! `LIKE` patterns as cheaper string functions (DuckDB's `LikeOptimizationRule`) and
//! `prefix`/`contains`/`suffix` with an empty needle (DuckDB's `EmptyNeedleRemovalRule`).

use rustdb_ast::ExpressionType;
use rustdb_common::Result;
use rustdb_planner::{Expression, ExpressionKind};
use rustdb_types::{LogicalType, PhysicalType, Value};

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::expression_util::{constant_or_null, evaluate_scalar};
use crate::optimizer::bind_scalar_function;

/// A pattern without wildcards (`PatternIsConstant`).
fn is_constant(pattern: &[u8]) -> bool {
    !pattern.iter().any(|&c| c == b'%' || c == b'_')
}

/// `abc%`: a fixed prefix followed by `%`s only (`PatternIsPrefix`).
fn is_prefix(pattern: &[u8]) -> bool {
    let end = pattern.iter().rposition(|&c| c != b'%').map_or(0, |i| i + 1);
    end != pattern.len() && is_constant(&pattern[..end])
}

/// `%abc`: `%`s followed by a fixed suffix (`PatternIsSuffix`).
fn is_suffix(pattern: &[u8]) -> bool {
    let start = pattern.iter().position(|&c| c != b'%').unwrap_or(pattern.len());
    start != 0 && is_constant(&pattern[start..])
}

/// `%abc%`: a fixed infix between `%`s (`PatternIsContains`).
fn is_contains(pattern: &[u8]) -> bool {
    let start = pattern.iter().position(|&c| c != b'%').unwrap_or(pattern.len());
    let end = pattern.iter().rposition(|&c| c != b'%').map_or(0, |i| i + 1);
    if start == 0 || end == pattern.len() {
        return false;
    }
    start >= end || is_constant(&pattern[start..end])
}

pub fn like_optimization(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    let return_type = expression.return_type.clone();
    let ExpressionKind::Function(function) = &mut expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    let is_not_like = match &*function.function.name {
        "~~" => false,
        "!~~" => true,
        _ => return Ok(RuleOutcome::Unchanged),
    };
    if function.children.len() != 2 {
        return Ok(RuleOutcome::Unchanged);
    }
    let Some(constant) = function.children[1].as_constant() else {
        return Ok(RuleOutcome::Unchanged);
    };
    if constant.is_null() {
        return Ok(RuleOutcome::Replaced(Expression::constant(Value::null(
            return_type,
        ))));
    }
    let value = evaluate_scalar(context.client, &function.children[1])?;
    let Some(pattern) = value.as_bytes().map(<[u8]>::to_vec) else {
        return Ok(RuleOutcome::Unchanged);
    };
    let mut children = std::mem::take(&mut function.children);
    if is_constant(&pattern) {
        let comparison = if is_not_like {
            ExpressionType::CompareNotequal
        } else {
            ExpressionType::CompareEqual
        };
        let right = children.pop();
        let left = children.pop();
        return Ok(match (left, right) {
            (Some(left), Some(right)) => {
                RuleOutcome::Replaced(Expression::comparison(comparison, left, right))
            }
            _ => RuleOutcome::Unchanged,
        });
    }
    let name = if is_prefix(&pattern) {
        "prefix"
    } else if is_suffix(&pattern) {
        "suffix"
    } else if is_contains(&pattern) {
        "contains"
    } else {
        function.children = children;
        return Ok(RuleOutcome::Unchanged);
    };
    // The pattern without its `%`s.
    let needle: Vec<u8> = pattern.into_iter().filter(|&c| c != b'%').collect();
    children[1] = Expression::constant(Value::varchar(String::from_utf8_lossy(&needle)));
    let mut result = bind_scalar_function(context.client, name, children, false)?;
    result.return_type = return_type;
    if is_not_like {
        result = Expression::operator(ExpressionType::OperatorNot, LogicalType::BOOLEAN, vec![result]);
    }
    Ok(RuleOutcome::Replaced(result))
}

pub fn empty_needle_removal(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    _: bool,
) -> Result<RuleOutcome> {
    let ExpressionKind::Function(function) = &mut expression.kind else {
        return Ok(RuleOutcome::Unchanged);
    };
    if !matches!(&*function.function.name, "prefix" | "contains" | "suffix") || function.children.len() != 2 {
        return Ok(RuleOutcome::Unchanged);
    }
    let needle = &function.children[1];
    if !needle.is_foldable() {
        return Ok(RuleOutcome::Unchanged);
    }
    let value = evaluate_scalar(context.client, needle)?;
    if value.is_null() {
        return Ok(RuleOutcome::Replaced(Expression::constant(Value::null(
            LogicalType::BOOLEAN,
        ))));
    }
    if value.logical_type().physical_type() != PhysicalType::Varchar {
        return Ok(RuleOutcome::Unchanged);
    }
    if value.as_bytes().is_some_and(<[u8]>::is_empty) {
        let haystack = function.children.swap_remove(0);
        return Ok(RuleOutcome::Replaced(constant_or_null(
            context.client,
            vec![haystack],
            Value::boolean(true),
        )?));
    }
    Ok(RuleOutcome::Unchanged)
}
