//! The expression binder active over the left side of a join while its right side is
//! bound (DuckDB's `LateralBinder`), and the depth reduction applied when the right side
//! turns out not to reference the left side.

use roastedduck_ast::{ParsedExpression, ParsedExpressionKind};
use roastedduck_common::{Error, Result};

use super::base::{BinderBase, expression_binder_accessors, expression_binder_base};
use super::{BindResult, ExpressionBinder};
use crate::binder::expression::{bind_expression_base, bind_unsupported_expression};
use crate::binder::{Binder, CorrelatedColumnInfo, CorrelatedColumns, Scope};
use crate::expression::{BoundSubqueryExpression, Expression, ExpressionKind};
use crate::operator::{LogicalKind, LogicalOperator};

/// Binds references from the right side of a join to the left side (DuckDB's
/// `LateralBinder`): such references are found when binding the right side fails and
/// are correlated columns of depth 1.
#[derive(Debug)]
pub struct LateralBinder<'a> {
    base: BinderBase<'a>,
    /// The correlated columns bound through this binder.
    pub correlated_columns: CorrelatedColumns,
}

impl<'a> LateralBinder<'a> {
    pub fn new(binder: &'a mut Binder, outer: Option<&'a mut dyn Scope>) -> Self {
        Self {
            base: BinderBase::new(binder, outer),
            correlated_columns: CorrelatedColumns::default(),
        }
    }

    /// DuckDB's `LateralBinder::ExtractCorrelatedColumns`.
    fn extract_correlated_columns(&mut self, expression: &Expression) {
        if let Some(column) = expression.as_column_ref()
            && column.depth > 0
            && let Some(info) = CorrelatedColumnInfo::from_expression(expression)
            && !self.correlated_columns.contains(info.binding)
        {
            self.correlated_columns.add_column(info);
        }
        for child in expression.children() {
            self.extract_correlated_columns(child);
        }
    }
}

expression_binder_base!(LateralBinder);

impl ExpressionBinder for LateralBinder<'_> {
    expression_binder_accessors!();

    fn bind_expression(
        &mut self,
        expr: &mut ParsedExpression,
        depth: u64,
        root_expression: bool,
    ) -> Result<BindResult> {
        match &expr.kind {
            ParsedExpressionKind::Default => {
                bind_unsupported_expression(self, expr, depth, "LATERAL join cannot contain DEFAULT clause!")
            }
            ParsedExpressionKind::Window(_) => {
                Ok(BindResult::error("LATERAL join cannot contain window functions!"))
            }
            ParsedExpressionKind::ColumnRef(_) => {
                if depth == 0 {
                    return Err(Error::internal("Lateral binder can only bind correlated columns"));
                }
                let result = bind_expression_base(self, expr, depth, root_expression)?;
                if let BindResult::Bound(expression) = &result {
                    self.extract_correlated_columns(expression);
                }
                Ok(result)
            }
            _ => bind_expression_base(self, expr, depth, root_expression),
        }
    }

    fn unsupported_aggregate_message(&self) -> roastedduck_common::ByteString {
        "LATERAL join cannot contain aggregates!".into()
    }
}

/// DuckDB's `ReduceColumnDepth`.
fn reduce_column_depth(columns: &mut CorrelatedColumns, affected: &CorrelatedColumns) {
    for column in columns.iter_mut() {
        if affected.contains(column.binding) {
            column.depth = column.depth.saturating_sub(1);
        }
    }
}

/// Reduces the depths in an expression tree; subqueries are entered (DuckDB's
/// `VisitExpression` of the depth reducers).
fn reduce_expression(expression: &mut Expression, correlated: &CorrelatedColumns) {
    match &mut expression.kind {
        // DuckDB's `ReduceColumnRefDepth`.
        ExpressionKind::ColumnRef(column) if column.depth > 0 && correlated.contains(column.binding) => {
            column.depth -= 1;
        }
        ExpressionKind::Subquery(subquery) => reduce_expression_subquery(subquery, correlated),
        _ => {}
    }
    expression.for_each_child_mut(|child| reduce_expression(child, correlated));
}

/// DuckDB's `ExpressionDepthReducerRecursive::ReduceExpressionSubquery`.
fn reduce_expression_subquery(subquery: &mut BoundSubqueryExpression, correlated: &CorrelatedColumns) {
    reduce_column_depth(&mut subquery.binder.correlated_columns, correlated);
    reduce_operator(&mut subquery.subquery.plan, correlated, true);
}

/// Visits an operator tree; inside subqueries (`recursive`) the correlated columns of
/// dependent joins are reduced as well.
fn reduce_operator(op: &mut LogicalOperator, correlated: &CorrelatedColumns, recursive: bool) {
    if recursive && let LogicalKind::DependentJoin(join) = &mut op.kind {
        reduce_column_depth(&mut join.correlated_columns, correlated);
    }
    op.for_each_expression_mut(|expression| reduce_expression(expression, correlated));
    for child in &mut op.children {
        reduce_operator(child, correlated, recursive);
    }
}

/// Makes references to `correlated` one level less deep throughout `op` (DuckDB's
/// `LateralBinder::ReduceExpressionDepth`).
pub fn reduce_expression_depth(op: &mut LogicalOperator, correlated: &CorrelatedColumns) {
    reduce_operator(op, correlated, false);
}
