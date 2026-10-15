//! Subqueries used as expressions (DuckDB's `bind_subquery_expression.cpp`).

use rustdb_ast::{BoundExpression, ExpressionType, ParsedExpression, ParsedExpressionKind, SubqueryType};
use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, LogicalTypeId};

use crate::binder::{BindScope, Binder, BoundStatement};
use crate::expression::{BoundSubqueryExpression, Expression, ExpressionKind};
use crate::expression_binder::{BindResult, ExpressionBinder, expression_return_type, take_bound};
use crate::function_binder::add_cast_to_type;

/// A subquery that is bound while the expression to its left (`x IN (...)`) is not yet
/// (DuckDB's `BoundSubqueryNode`, which replaces the subquery in the parse tree): binding
/// is retried in an enclosing query without binding the subquery again.
#[derive(Clone, Debug)]
pub struct PendingSubquery {
    pub binder: Binder,
    pub bound_node: BoundStatement,
    pub subquery_type: SubqueryType,
    pub comparison_type: ExpressionType,
    pub child: Option<Box<ParsedExpression>>,
}

fn is_unnamed_struct(ty: &LogicalType) -> bool {
    ty.id() == LogicalTypeId::Struct && ty.is_unnamed_struct()
}

/// `(a, b) = ANY(...)`: the row's elements compared one by one (DuckDB's
/// `ExtractSubqueryChildren`).
fn extract_subquery_children(child: &mut Option<Expression>, types: &[LogicalType], comparison: ExpressionType) -> Vec<Expression> {
    let Some(expression) = child else {
        return Vec::new();
    };
    if !is_unnamed_struct(&expression.return_type) {
        return Vec::new();
    }
    let ExpressionKind::Function(function) = &mut expression.kind else {
        return Vec::new();
    };
    if &*function.function.name != "row" {
        return Vec::new();
    }
    if types.len() == 1 && is_unnamed_struct(&types[0]) && function.children.len() != types.len() {
        return Vec::new();
    }
    if !matches!(
        comparison,
        ExpressionType::CompareEqual
            | ExpressionType::CompareNotequal
            | ExpressionType::CompareDistinctFrom
            | ExpressionType::CompareNotDistinctFrom
    ) {
        return Vec::new();
    }
    std::mem::take(&mut function.children)
}

/// DuckDB's `BindExpression(SubqueryExpression &)`.
pub fn bind_subquery_expression<B: ExpressionBinder + ?Sized>(
    binder: &mut B,
    expr: &mut ParsedExpression,
    depth: u64,
) -> Result<BindResult> {
    let location = expr.query_location;
    let mut pending = match std::mem::replace(&mut expr.kind, ParsedExpressionKind::Default) {
        ParsedExpressionKind::Bound(bound) => match bound.payload.downcast::<PendingSubquery>() {
            Ok(pending) => std::sync::Arc::try_unwrap(pending).unwrap_or_else(|shared| (*shared).clone()),
            Err(_) => return Err(Error::internal("expected a pending subquery")),
        },
        ParsedExpressionKind::Subquery(mut subquery) => {
            let mut child_binder = binder.binder_ref().child()?;
            child_binder.can_contain_nulls = true;
            let bound_node = {
                let mut scope = BindScope {
                    binder: &mut child_binder,
                    outer: Some(binder.as_scope()),
                };
                crate::binder::query_node::bind_node(&mut scope, &mut subquery.subquery.node)?
            };
            for correlated in &child_binder.correlated_columns.clone() {
                if correlated.depth > 1 {
                    let mut info = correlated.clone();
                    info.depth -= 1;
                    binder.binder().add_correlated_column(info);
                }
            }
            PendingSubquery {
                binder: child_binder,
                bound_node,
                subquery_type: subquery.subquery_type,
                comparison_type: subquery.comparison_type,
                child: subquery.child,
            }
        }
        _ => return Err(Error::internal("expected a subquery")),
    };
    if let Some(child) = &mut pending.child
        && let Some(error) = binder.bind_in_place(child, depth, false)?
    {
        let text = "SUBQUERY".to_owned();
        expr.kind = ParsedExpressionKind::Bound(BoundExpression::new(pending, text));
        return Ok(BindResult::Unbound(error));
    }
    let mut child = match &mut pending.child {
        Some(child) => Some(take_bound(child)?),
        None => None,
    };
    let types = pending.bound_node.types.clone();
    let mut child_expressions = Vec::new();
    if pending.subquery_type != SubqueryType::Exists {
        let mut expected_columns = 1;
        let mut has_unexpanded_struct = false;
        if child.is_some() {
            has_unexpanded_struct = child.as_ref().is_some_and(|c| is_unnamed_struct(&c.return_type));
            child_expressions = extract_subquery_children(&mut child, &types, pending.comparison_type);
            if child_expressions.is_empty() {
                child_expressions.extend(child.take());
            }
            expected_columns = child_expressions.len();
        }
        if has_unexpanded_struct
            && expected_columns == 1
            && types.len() > 1
            && is_unnamed_struct(&child_expressions[0].return_type)
        {
            expected_columns = types.len();
        }
        if types.len() != expected_columns {
            return Err(Error::binder(format!(
                "Subquery returns {} columns - expected {expected_columns}",
                types.len()
            ))
            .with_position(location));
        }
    }
    let mut return_type = if pending.subquery_type == SubqueryType::Scalar {
        types.first().cloned().unwrap_or_default()
    } else {
        LogicalType::BOOLEAN
    };
    if return_type.id() == LogicalTypeId::Unknown {
        return_type = LogicalType::SQLNULL;
    }
    let mut children = Vec::new();
    let mut child_types = Vec::new();
    let mut child_targets = Vec::new();
    if pending.subquery_type == SubqueryType::Any {
        if child_expressions.len() == 1 && types.len() > 1 && is_unnamed_struct(&child_expressions[0].return_type) {
            children.append(&mut child_expressions);
            child_types.clone_from(&types);
            child_targets.clone_from(&types);
        } else {
            let context = binder.binder_ref().context.clone();
            for (child, subquery_type) in child_expressions.into_iter().zip(&types) {
                let child_type = expression_return_type(&child);
                let compare_type = LogicalType::try_max_logical_type(&child_type, subquery_type).ok_or_else(|| {
                    Error::binder(format!(
                        "Cannot compare values of type {child_type} and {subquery_type} in IN/ANY/ALL clause - an \
                         explicit cast is required"
                    ))
                    .with_position(location)
                })?;
                children.push(add_cast_to_type(&context, child, &compare_type, false)?);
                child_types.push(subquery_type.clone());
                child_targets.push(compare_type);
            }
        }
    }
    Ok(Expression::new(
        ExpressionType::Subquery,
        return_type,
        ExpressionKind::Subquery(Box::new(BoundSubqueryExpression {
            binder: Box::new(pending.binder),
            subquery: pending.bound_node,
            subquery_type: pending.subquery_type,
            children,
            comparison_type: pending.comparison_type,
            child_types,
            child_targets,
        })),
    )
    .into())
}
