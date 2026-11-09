//! Projecting columns out as soon as they are no longer needed (DuckDB's
//! `ColumnLifetimeAnalyzer`): filters, ORDER BYs and hash joins get projection maps
//! listing the input columns the operators above still use.

use std::collections::HashSet;

use roastedduck_common::{Error, Result};
use roastedduck_planner::{
    ColumnBinding, DistinctType, Expression, ExpressionType, LogicalKind, LogicalOperator,
    LogicalOperatorType,
};

use crate::expression_util::visit_column_refs;
use crate::topn::can_optimize as topn_can_optimize;

/// Sets the projection maps in `plan` (DuckDB's `ColumnLifetimeAnalyzer` run from the
/// root, where everything the plan outputs is referenced).
pub fn analyze_column_lifetime(plan: &mut LogicalOperator, prefer_range_joins: bool) -> Result<()> {
    ColumnLifetimeAnalyzer::new(prefer_range_joins, true).visit_operator(plan)
}

/// One analysis scope: the bindings referenced by the operators above, up to the closest
/// operator that defines new bindings.
struct ColumnLifetimeAnalyzer {
    prefer_range_joins: bool,
    everything_referenced: bool,
    column_references: HashSet<ColumnBinding>,
}

impl ColumnLifetimeAnalyzer {
    fn new(prefer_range_joins: bool, everything_referenced: bool) -> Self {
        Self {
            prefer_range_joins,
            everything_referenced,
            column_references: HashSet::new(),
        }
    }

    /// The bindings of `bindings` nobody above references (DuckDB's
    /// `ExtractUnusedColumnBindings`).
    fn unused_bindings(&self, bindings: &[ColumnBinding]) -> HashSet<ColumnBinding> {
        bindings
            .iter()
            .filter(|binding| !self.column_references.contains(binding))
            .copied()
            .collect()
    }

    fn visit_expression(&mut self, expression: &Expression) {
        visit_column_refs(expression, &mut |column| {
            self.column_references.insert(column.binding);
        });
    }

    fn visit_operator_expressions(&mut self, op: &mut LogicalOperator) {
        op.for_each_expression_mut(|expression| self.visit_expression(expression));
    }

    /// DuckDB's `StandardVisitOperator`: the expressions, then the children.
    fn standard_visit_operator(&mut self, op: &mut LogicalOperator) -> Result<()> {
        self.visit_operator_expressions(op);
        for child in &mut op.children {
            self.visit_operator(child)?;
        }
        Ok(())
    }

    fn visit_operator(&mut self, op: &mut LogicalOperator) -> Result<()> {
        if topn_can_optimize(op)
            && op
                .children
                .first()
                .is_some_and(|child| child.operator_type() == LogicalOperatorType::LogicalOrderBy)
        {
            // TopN matters more than projection maps (and has none).
            self.visit_operator_expressions(op);
            let order = &mut op.children[0];
            self.visit_operator_expressions(order);
            if let Some(input) = order.children.first_mut() {
                self.standard_visit_operator(input)?;
            }
            return Ok(());
        }
        match op.operator_type() {
            LogicalOperatorType::LogicalAggregateAndGroupBy | LogicalOperatorType::LogicalProjection => {
                return Self::new(self.prefer_range_joins, false).standard_visit_operator(op);
            }
            LogicalOperatorType::LogicalAsofJoin
            | LogicalOperatorType::LogicalDelimJoin
            | LogicalOperatorType::LogicalComparisonJoin => {
                if !self.everything_referenced {
                    return self.visit_comparison_join(op);
                }
            }
            LogicalOperatorType::LogicalInsert
            | LogicalOperatorType::LogicalUpdate
            | LogicalOperatorType::LogicalDelete
            | LogicalOperatorType::LogicalMergeInto
            | LogicalOperatorType::LogicalGet
            | LogicalOperatorType::LogicalUnion
            | LogicalOperatorType::LogicalExcept
            | LogicalOperatorType::LogicalIntersect
            | LogicalOperatorType::LogicalRecursiveCte
            | LogicalOperatorType::LogicalMaterializedCte => {
                // Nothing is removed below set operations and CTEs.
                return Self::new(self.prefer_range_joins, true).standard_visit_operator(op);
            }
            LogicalOperatorType::LogicalOrderBy | LogicalOperatorType::LogicalFilter => {
                if !self.everything_referenced {
                    let unused = self.unused_bindings(&op.children[0].column_bindings());
                    self.standard_visit_operator(op)?;
                    let map = projection_map(&op.children[0].column_bindings(), &unused);
                    match &mut op.kind {
                        LogicalKind::Order { projection_map, .. }
                        | LogicalKind::Filter { projection_map } => {
                            *projection_map = map;
                        }
                        _ => return Err(Error::internal("expected an ORDER BY or a filter")),
                    }
                    return Ok(());
                }
            }
            LogicalOperatorType::LogicalDistinct => {
                if let LogicalKind::Distinct {
                    distinct_type,
                    distinct_targets,
                    order_by,
                } = &op.kind
                {
                    if *distinct_type == DistinctType::DistinctOn {
                        // DISTINCT ON references only its targets and ORDER BY.
                        let mut bindings = Vec::new();
                        for target in distinct_targets {
                            visit_column_refs(target, &mut |column| bindings.push(column.binding));
                        }
                        if let Some(order_by) = order_by {
                            for order in &order_by.orders {
                                visit_column_refs(&order.expression, &mut |column| {
                                    bindings.push(column.binding);
                                });
                            }
                        }
                        self.column_references.extend(bindings);
                    } else {
                        self.everything_referenced = true;
                    }
                }
            }
            _ => {}
        }
        self.standard_visit_operator(op)
    }

    /// Projection maps for hash joins (DuckDB's comparison join case): columns only the
    /// join conditions use are dropped from the join's output.
    fn visit_comparison_join(&mut self, op: &mut LogicalOperator) -> Result<()> {
        let LogicalKind::ComparisonJoin(join) = &op.kind else {
            return Err(Error::internal("expected a comparison join"));
        };
        let mut has_equality = false;
        for condition in &join.conditions {
            match condition.comparison {
                ExpressionType::CompareEqual | ExpressionType::CompareNotDistinctFrom => has_equality = true,
                ExpressionType::CompareLessthan
                | ExpressionType::CompareGreaterthan
                | ExpressionType::CompareLessthanorequalto
                | ExpressionType::CompareGreaterthanorequalto
                | ExpressionType::CompareNotequal
                | ExpressionType::CompareDistinctFrom => {}
                _ => return Err(Error::not_implemented("Unimplemented comparison join")),
            }
        }
        if !has_equality || self.prefer_range_joins {
            return Ok(());
        }
        let is_asof = join.operator_type == LogicalOperatorType::LogicalAsofJoin;
        let lhs_unused = self.unused_bindings(&op.children[0].column_bindings());
        let rhs_unused = self.unused_bindings(&op.children[1].column_bindings());
        self.standard_visit_operator(op)?;
        let left_map = projection_map(&op.children[0].column_bindings(), &lhs_unused);
        let right_map = projection_map(&op.children[1].column_bindings(), &rhs_unused);
        if let LogicalKind::ComparisonJoin(join) = &mut op.kind {
            if !is_asof {
                join.left_projection_map = left_map;
            }
            join.right_projection_map = right_map;
        }
        Ok(())
    }
}

/// The positions of `bindings` that are not unused; empty when nothing is dropped
/// (DuckDB's `GenerateProjectionMap`).
fn projection_map(bindings: &[ColumnBinding], unused: &HashSet<ColumnBinding>) -> Vec<u64> {
    if unused.is_empty() {
        return Vec::new();
    }
    let map: Vec<u64> = bindings
        .iter()
        .enumerate()
        .filter(|(_, binding)| !unused.contains(binding))
        .map(|(position, _)| position as u64)
        .collect();
    if map.len() == bindings.len() {
        Vec::new()
    } else {
        map
    }
}
