//! Rewriting column references from one binding to another throughout a plan (DuckDB's
//! `ColumnBindingReplacer`).

use rustdb_planner::{ColumnBinding, Expression, ExpressionKind, LogicalOperator};
use rustdb_types::LogicalType;

/// One binding to rewrite, optionally also changing the reference's type (DuckDB's
/// `ReplacementBinding`).
#[derive(Clone, Debug)]
pub struct ReplacementBinding {
    pub old_binding: ColumnBinding,
    pub new_binding: ColumnBinding,
    /// The new type of the reference, when it changes.
    pub new_type: Option<LogicalType>,
}

impl ReplacementBinding {
    /// A replacement keeping the reference's type.
    pub const fn new(old_binding: ColumnBinding, new_binding: ColumnBinding) -> Self {
        Self {
            old_binding,
            new_binding,
            new_type: None,
        }
    }

    /// A replacement that also changes the reference's type.
    pub const fn with_type(old_binding: ColumnBinding, new_binding: ColumnBinding, new_type: LogicalType) -> Self {
        Self {
            old_binding,
            new_binding,
            new_type: Some(new_type),
        }
    }
}

/// Replaces the bindings of column references in a plan (DuckDB's
/// `ColumnBindingReplacer`): the replacements are tried in order, each one seeing the
/// result of the previous ones.
#[derive(Clone, Debug, Default)]
pub struct ColumnBindingReplacer {
    pub replacement_bindings: Vec<ReplacementBinding>,
}

impl ColumnBindingReplacer {
    /// Rewrites `op` and everything below it, children first.
    pub fn visit_operator(&self, op: &mut LogicalOperator) {
        for child in &mut op.children {
            self.visit_operator(child);
        }
        op.for_each_expression_mut(|expression| self.visit_expression(expression));
    }

    /// Rewrites the column references in `expression`.
    pub fn visit_expression(&self, expression: &mut Expression) {
        if let ExpressionKind::ColumnRef(column) = &mut expression.kind {
            let mut new_type = None;
            for replacement in &self.replacement_bindings {
                if column.binding == replacement.old_binding {
                    column.binding = replacement.new_binding;
                    if let Some(replacement_type) = &replacement.new_type {
                        new_type = Some(replacement_type.clone());
                    }
                }
            }
            if let Some(new_type) = new_type {
                expression.return_type = new_type;
            }
        }
        expression.for_each_child_mut(|child| self.visit_expression(child));
    }
}
