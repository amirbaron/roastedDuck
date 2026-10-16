//! The expression rewriter (DuckDB's `ExpressionRewriter`): applies the rewrite rules to
//! every expression of the plan until none of them changes anything.

use rustdb_common::Result;
use rustdb_planner::operator::{GroupingSet, split_predicates};
use rustdb_planner::{ClientContext, Expression, LogicalKind, LogicalOperator, LogicalOperatorType};

use crate::optimizer::Optimizer;
use crate::rule::{RULES, RuleOutcome};

/// What a rule sees of the operator whose expressions are rewritten.
#[derive(Debug)]
pub struct RuleContext<'a> {
    pub client: &'a ClientContext,
    /// The type of the operator the expression belongs to.
    pub op_type: LogicalOperatorType,
    /// The groups and grouping sets of an aggregate operator (for the ordered aggregate
    /// rule).
    pub aggregate: Option<(Vec<Expression>, Vec<GroupingSet>)>,
}

/// Applies the expression rules to a plan (DuckDB's `ExpressionRewriter`).
#[derive(Debug)]
pub struct ExpressionRewriter<'a> {
    client: &'a ClientContext,
}

impl<'a> ExpressionRewriter<'a> {
    pub fn new(optimizer: &'a Optimizer) -> Self {
        Self {
            client: optimizer.context(),
        }
    }

    /// A rewriter outside the optimizer driver.
    pub fn with_context(client: &'a ClientContext) -> Self {
        Self { client }
    }

    /// Rewrites the expressions of `op` and its children, children first (DuckDB's
    /// `ExpressionRewriter::VisitOperator`).
    pub fn visit_operator(&self, op: &mut LogicalOperator) -> Result<()> {
        for child in &mut op.children {
            self.visit_operator(child)?;
        }
        let aggregate = match &op.kind {
            LogicalKind::Aggregate(aggregate) => {
                Some((aggregate.groups.clone(), aggregate.grouping_sets.clone()))
            }
            _ => None,
        };
        let context = RuleContext {
            client: self.client,
            op_type: op.operator_type(),
            aggregate,
        };
        op.try_for_each_expression_mut(|expression| Self::visit_expression(&context, expression))?;
        if matches!(op.kind, LogicalKind::Filter { .. }) {
            split_predicates(&mut op.expressions);
        }
        Ok(())
    }

    /// Applies the rules until the expression no longer changes (DuckDB's
    /// `ExpressionRewriter::VisitExpression`).
    pub fn visit_expression(context: &RuleContext<'_>, expression: &mut Expression) -> Result<()> {
        loop {
            let mut changes_made = false;
            Self::apply_rules(context, expression, &mut changes_made, true)?;
            if !changes_made {
                return Ok(());
            }
        }
    }

    /// Applies the first rule that changes the expression, then descends into the
    /// children if none did (DuckDB's `ExpressionRewriter::ApplyRules`).
    fn apply_rules(
        context: &RuleContext<'_>,
        expression: &mut Expression,
        changes_made: &mut bool,
        is_root: bool,
    ) -> Result<()> {
        for rule in RULES {
            match rule(context, expression, is_root)? {
                RuleOutcome::Replaced(mut result) => {
                    *changes_made = true;
                    if !expression.alias.is_empty() {
                        result.alias = std::mem::take(&mut expression.alias);
                    }
                    *expression = result;
                    return Self::apply_rules(context, expression, changes_made, false);
                }
                RuleOutcome::Changed => {
                    *changes_made = true;
                    return Ok(());
                }
                RuleOutcome::Unchanged => {}
            }
        }
        expression.try_for_each_child_mut(|child| Self::apply_rules(context, child, changes_made, false))
    }
}
