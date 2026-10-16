//! Single-column predicates factored out of an OR (DuckDB's `PredicateFactoringRule`):
//! `(x = 1 AND y = 2) OR (x = 3 AND z = 4)` gains the pushable `x = 1 OR x = 3`.

use rustdb_ast::ExpressionType;
use rustdb_common::Result;
use rustdb_planner::{ColumnBinding, Expression, ExpressionKind, LogicalOperatorType};

use super::RuleOutcome;
use crate::expression_rewriter::RuleContext;
use crate::expression_util::visit_column_refs;

/// Single-column predicates per column, in order of first appearance (DuckDB's
/// `column_binding_map_t<vector<reference<Expression>>>`).
type BindingMap<'a> = Vec<(ColumnBinding, Vec<&'a Expression>)>;

fn is_disjunction(expression: &Expression) -> bool {
    expression.expression_type == ExpressionType::ConjunctionOr
        && matches!(expression.kind, ExpressionKind::Conjunction(_))
}

fn is_conjunction(expression: &Expression) -> bool {
    expression.expression_type == ExpressionType::ConjunctionAnd
        && matches!(expression.kind, ExpressionKind::Conjunction(_))
}

fn conjunction_children(expression: &Expression) -> &[Expression] {
    match &expression.kind {
        ExpressionKind::Conjunction(conjunction) => &conjunction.children,
        _ => &[],
    }
}

/// The OR-ed branches, nested ORs flattened.
fn disjuncted_children<'a>(expression: &'a Expression, result: &mut Vec<&'a Expression>) {
    if is_disjunction(expression) {
        for child in conjunction_children(expression) {
            disjuncted_children(child, result);
        }
    } else {
        result.push(expression);
    }
}

/// The only column a non-volatile expression references (`GetSingleColumnBinding`).
fn single_column_binding(expression: &Expression) -> Option<ColumnBinding> {
    if expression.is_volatile() {
        return None;
    }
    let mut binding: Option<ColumnBinding> = None;
    let mut multiple = false;
    visit_column_refs(expression, &mut |column| match binding {
        None => binding = Some(column.binding),
        Some(existing) if existing != column.binding => multiple = true,
        Some(_) => {}
    });
    if multiple { None } else { binding }
}

/// Adds the single-column predicates of one OR branch (`ExtractDisjunctedPredicates` /
/// `ExtractConjunctedPredicates`).
fn extract_predicates<'a>(expression: &'a Expression, map: &mut BindingMap<'a>) {
    if is_conjunction(expression) {
        for child in conjunction_children(expression) {
            extract_predicates(child, map);
        }
        return;
    }
    if let Some(binding) = single_column_binding(expression) {
        match map.iter_mut().find(|(existing, _)| *existing == binding) {
            Some((_, predicates)) => predicates.push(expression),
            None => map.push((binding, vec![expression])),
        }
    }
}

/// The columns that have a predicate in every OR branch, with all those predicates
/// (`GetDisjunctedPredicateMap`).
fn disjuncted_predicate_map(expression: &Expression) -> BindingMap<'_> {
    let mut branches = Vec::new();
    disjuncted_children(expression, &mut branches);
    let mut remaining = BindingMap::new();
    if let Some(first) = branches.first() {
        extract_predicates(first, &mut remaining);
    }
    for branch in branches.iter().skip(1) {
        let mut child_map = BindingMap::new();
        extract_predicates(branch, &mut child_map);
        remaining.retain_mut(|(binding, predicates)| {
            let Some((_, child_predicates)) = child_map.iter().find(|(other, _)| other == binding) else {
                return false;
            };
            for predicate in child_predicates {
                if !predicates.iter().any(|existing| existing.equals(predicate)) {
                    predicates.push(predicate);
                }
            }
            true
        });
    }
    remaining
}

pub fn predicate_factoring(
    context: &RuleContext<'_>,
    expression: &mut Expression,
    is_root: bool,
) -> Result<RuleOutcome> {
    if !is_root
        || !matches!(
            context.op_type,
            LogicalOperatorType::LogicalFilter | LogicalOperatorType::LogicalAnyJoin
        )
        || !is_disjunction(expression)
    {
        return Ok(RuleOutcome::Unchanged);
    }
    // The matcher: at least one child is a conjunction.
    if !conjunction_children(expression)
        .iter()
        .any(|child| matches!(child.kind, ExpressionKind::Conjunction(_)))
    {
        return Ok(RuleOutcome::Unchanged);
    }
    if single_column_binding(expression).is_some() {
        // A filter on one column can be pushed down as it is.
        return Ok(RuleOutcome::Unchanged);
    }
    let binding_map = disjuncted_predicate_map(expression);
    if binding_map.is_empty() {
        return Ok(RuleOutcome::Unchanged);
    }
    let mut derived: Option<Expression> = None;
    for (_, predicates) in binding_map {
        let mut predicates = predicates.into_iter().cloned();
        let Some(first) = predicates.next() else {
            continue;
        };
        let column_filter = predicates.fold(first, |filter, predicate| {
            Expression::conjunction(ExpressionType::ConjunctionOr, vec![filter, predicate])
        });
        derived = Some(match derived {
            None => column_filter,
            Some(existing) => {
                Expression::conjunction(ExpressionType::ConjunctionAnd, vec![column_filter, existing])
            }
        });
    }
    let Some(derived) = derived else {
        return Ok(RuleOutcome::Unchanged);
    };
    Ok(RuleOutcome::Replaced(Expression::conjunction(
        ExpressionType::ConjunctionAnd,
        vec![expression.clone(), derived],
    )))
}
