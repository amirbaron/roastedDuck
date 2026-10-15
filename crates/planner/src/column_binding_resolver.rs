//! Replaces column bindings by input positions once a plan is final (DuckDB's
//! `ColumnBindingResolver`): every `BOUND_COLUMN_REF` becomes a `BOUND_REF` to the column
//! of the operator's input chunk that produces that binding.

use rustdb_ast::JoinType;
use rustdb_common::{Error, Result};
use rustdb_types::LogicalType;

use crate::expression::{Expression, ExpressionKind};
use crate::generated::enums::LogicalOperatorType;
use crate::operator::{LogicalKind, LogicalOperator};
use crate::ColumnBinding;

/// Resolves the column references of a plan.
#[derive(Debug, Default)]
pub struct ColumnBindingResolver {
    bindings: Vec<ColumnBinding>,
    types: Vec<LogicalType>,
}

impl ColumnBindingResolver {
    /// Resolves every column reference in `plan` (DuckDB's `ColumnBindingResolver::VisitOperator`
    /// on the root, after `ResolveOperatorTypes`).
    pub fn resolve(plan: &mut LogicalOperator) -> Result<()> {
        plan.resolve_operator_types()?;
        Self::default().visit_operator(plan)
    }

    fn visit_children(&mut self, op: &mut LogicalOperator) -> Result<()> {
        for child in &mut op.children {
            self.visit_operator(child)?;
        }
        Ok(())
    }

    fn set_bindings(&mut self, op: &LogicalOperator) {
        self.bindings = op.column_bindings();
        self.types.clone_from(&op.types);
    }

    fn visit_operator(&mut self, op: &mut LogicalOperator) -> Result<()> {
        match op.operator_type() {
            LogicalOperatorType::LogicalComparisonJoin | LogicalOperatorType::LogicalAsofJoin => {
                let [left, right] = &mut op.children[..] else {
                    return Err(Error::internal("join without two children"));
                };
                self.visit_operator(left)?;
                let LogicalKind::ComparisonJoin(join) = &mut op.kind else {
                    return Err(Error::internal("expected a comparison join"));
                };
                for condition in &mut join.conditions {
                    self.visit_expression(&mut condition.left)?;
                }
                for expr in &mut join.duplicate_eliminated_columns {
                    self.visit_expression(expr)?;
                }
                self.visit_operator(right)?;
                for condition in &mut join.conditions {
                    self.visit_expression(&mut condition.right)?;
                }
                let semi = matches!(join.join_type, JoinType::Semi | JoinType::Anti);
                let has_predicate = join.predicate.is_some();
                self.set_bindings(op);
                if has_predicate {
                    if semi {
                        self.bindings.extend(op.children[1].column_bindings());
                        self.types.extend(op.children[1].types.iter().cloned());
                    }
                    if let LogicalKind::ComparisonJoin(join) = &mut op.kind
                        && let Some(predicate) = &mut join.predicate
                    {
                        self.visit_expression(predicate)?;
                    }
                }
                Ok(())
            }
            LogicalOperatorType::LogicalDelimJoin => {
                let LogicalKind::ComparisonJoin(join) = &mut op.kind else {
                    return Err(Error::internal("expected a delim join"));
                };
                let flipped = join.delim_flipped;
                let (delim_index, other_index) = if flipped { (1, 0) } else { (0, 1) };
                self.visit_operator(&mut op.children[delim_index])?;
                for condition in &mut join.conditions {
                    self.visit_expression(if flipped { &mut condition.right } else { &mut condition.left })?;
                }
                for expr in &mut join.duplicate_eliminated_columns {
                    self.visit_expression(expr)?;
                }
                self.visit_operator(&mut op.children[other_index])?;
                for condition in &mut join.conditions {
                    self.visit_expression(if flipped { &mut condition.left } else { &mut condition.right })?;
                }
                self.set_bindings(op);
                Ok(())
            }
            LogicalOperatorType::LogicalAnyJoin => {
                self.visit_children(op)?;
                self.set_bindings(op);
                if matches!(op.join_type(), Some(JoinType::Semi | JoinType::Anti)) {
                    self.bindings.extend(op.children[1].column_bindings());
                    self.types.extend(op.children[1].types.iter().cloned());
                }
                op.try_for_each_expression_mut(|expr| self.visit_expression(expr))
            }
            LogicalOperatorType::LogicalGet => {
                self.set_bindings(op);
                op.try_for_each_expression_mut(|expr| self.visit_expression(expr))
            }
            LogicalOperatorType::LogicalRecursiveCte => {
                self.visit_children(op)?;
                self.set_bindings(op);
                op.try_for_each_expression_mut(|expr| self.visit_expression(expr))
            }
            _ => {
                self.visit_children(op)?;
                op.try_for_each_expression_mut(|expr| self.visit_expression(expr))?;
                self.set_bindings(op);
                Ok(())
            }
        }
    }

    fn visit_expression(&mut self, expr: &mut Expression) -> Result<()> {
        if let ExpressionKind::ColumnRef(column) = &expr.kind {
            let binding = column.binding;
            let Some(index) = self.bindings.iter().position(|candidate| *candidate == binding) else {
                return Err(Error::internal(format!(
                    "Failed to bind column reference \"{}\" [{}.{}] (bindings: {})",
                    expr.alias,
                    binding.table_index,
                    binding.column_index,
                    self.bindings.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
                )));
            };
            if let Some(ty) = self.types.get(index)
                && ty != &expr.return_type
            {
                return Err(Error::internal(format!(
                    "Failed to bind column reference \"{}\" [{}.{}]: inequal types ({} != {ty})",
                    expr.alias, binding.table_index, binding.column_index, expr.return_type
                )));
            }
            *expr = Expression::reference(expr.alias.clone(), expr.return_type.clone(), index as u64);
            return Ok(());
        }
        expr.try_for_each_child_mut(|child| self.visit_expression(child))
    }
}
