//! Physical planning of comparison joins (DuckDB's `plan_comparison_join.cpp`,
//! `plan_delim_join.cpp`) and the cardinality estimates that choose between them
//! (`LogicalOperator::EstimateCardinality`).

use std::sync::Arc;

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_common::{Error, Result};
use rustdb_planner::operator::{JoinCondition, LogicalComparisonJoin};
use rustdb_planner::{BoundLimitNode, Expression, ExpressionKind, LogicalKind, LogicalOperator};
use rustdb_types::LogicalType;

use super::{PlanGenerator, table_storage};
use crate::operator::PhysicalRef;
use crate::operators::{PhysicalBlockwiseNLJoin, PhysicalCrossProduct, PhysicalHashJoin, PhysicalNestedLoopJoin};

/// The settings that steer join planning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinPlanSettings {
    /// `nested_loop_join_threshold`: below this many rows on either side, range joins
    /// use a nested loop join.
    pub nested_loop_join_threshold: u64,
    /// `merge_join_threshold`: below this many rows on either side, a piecewise merge
    /// join is preferred over an IEJoin.
    pub merge_join_threshold: u64,
    /// `prefer_range_joins`: plan range joins even with an equality condition.
    pub prefer_range_joins: bool,
    /// `scalar_subquery_error_on_multiple_rows`.
    pub scalar_subquery_error_on_multiple_rows: bool,
}

impl Default for JoinPlanSettings {
    fn default() -> Self {
        Self {
            nested_loop_join_threshold: 5,
            merge_join_threshold: 1000,
            prefer_range_joins: false,
            scalar_subquery_error_on_multiple_rows: true,
        }
    }
}

/// DuckDB's `LogicalOperator::EstimateCardinality` and its overrides: the larger
/// estimate of the children unless the operator knows better.
pub(crate) fn estimate_cardinality(op: &LogicalOperator) -> u64 {
    if let Some(estimate) = op.estimated_cardinality {
        return estimate;
    }
    let children_max = || op.children.iter().map(estimate_cardinality).max().unwrap_or(0);
    match &op.kind {
        LogicalKind::Get(get) => {
            if let Some(rows) = get
                .table
                .as_ref()
                .and_then(|entry| table_storage(entry).ok())
                .map(|storage| storage.total_rows())
            {
                return rows;
            }
            op.children.first().map_or(1, estimate_cardinality)
        }
        LogicalKind::Aggregate(aggregate) if aggregate.groups.is_empty() => 1,
        LogicalKind::Limit { limit, .. } => {
            let child = children_max();
            match limit {
                BoundLimitNode::ConstantValue(value) => child.min(*value),
                BoundLimitNode::ConstantPercentage(percentage) => (child as f64 * percentage) as u64,
                _ => child,
            }
        }
        LogicalKind::TopN { limit, .. } => children_max().min(*limit),
        LogicalKind::Sample { sample_options } => {
            let child = children_max();
            if sample_options.is_percentage {
                let percentage = sample_options.sample_size.as_f64().unwrap_or(100.0);
                let sampled = child as f64 * (percentage / 100.0);
                if sampled > child as f64 { child } else { sampled as u64 }
            } else {
                let size = sample_options
                    .sample_size
                    .as_i128()
                    .and_then(|size| u64::try_from(size).ok())
                    .unwrap_or(u64::MAX);
                child.min(size)
            }
        }
        LogicalKind::Insert(_) | LogicalKind::Delete(_) | LogicalKind::Update(_) | LogicalKind::MergeInto(_) => {
            children_max()
        }
        LogicalKind::Set { .. }
        | LogicalKind::Reset { .. }
        | LogicalKind::Pragma { .. }
        | LogicalKind::Prepare { .. }
        | LogicalKind::CreateTable(_)
        | LogicalKind::Create { .. } => 1,
        _ => children_max(),
    }
}

/// Whether the conditions contain an equality, and how many range comparisons
/// (`LogicalComparisonJoin::HasEquality`).
fn has_equality(conditions: &[JoinCondition]) -> Result<(bool, usize)> {
    let mut equality = false;
    let mut ranges = 0;
    for condition in conditions {
        match condition.comparison {
            ExpressionType::CompareEqual | ExpressionType::CompareNotDistinctFrom => equality = true,
            ExpressionType::CompareLessthan
            | ExpressionType::CompareGreaterthan
            | ExpressionType::CompareLessthanorequalto
            | ExpressionType::CompareGreaterthanorequalto => ranges += 1,
            ExpressionType::CompareNotequal | ExpressionType::CompareDistinctFrom => {}
            _ => return Err(Error::not_implemented("Unimplemented comparison join")),
        }
    }
    Ok((equality, ranges))
}

/// Adds `offset` to every column reference in `expression` (`RewriteJoinCondition`).
fn rewrite_join_condition(expression: &mut Expression, offset: u64) {
    if let ExpressionKind::Reference(reference) = &mut expression.kind {
        reference.index += offset;
    }
    expression.for_each_child_mut(|child| rewrite_join_condition(child, offset));
}

/// The physical join for a comparison join of the planned `left` and `right` inputs
/// with the given cardinality estimates (DuckDB's `PlanComparisonJoin` after planning
/// the children).
pub(crate) fn create_comparison_join(
    generator: &PlanGenerator,
    join: LogicalComparisonJoin,
    types: Vec<LogicalType>,
    left: PhysicalRef,
    right: PhysicalRef,
    cardinalities: (u64, u64),
) -> Result<PhysicalRef> {
    let settings = generator.join_settings();
    if join.conditions.is_empty() {
        return Ok(Arc::new(PhysicalCrossProduct::new(types, left, right)));
    }
    let (equality, ranges) = has_equality(&join.conditions)?;
    let mut can_merge = ranges > 0;
    let mut can_iejoin = ranges >= 2 && generator.working_tables.is_empty();
    if matches!(
        join.join_type,
        JoinType::Semi | JoinType::Anti | JoinType::RightAnti | JoinType::RightSemi | JoinType::Mark
    ) {
        can_merge = can_merge && join.conditions.len() == 1;
        can_iejoin = false;
    }
    let prefer_range_joins = settings.prefer_range_joins && can_iejoin;
    if equality && !prefer_range_joins {
        return Ok(Arc::new(PhysicalHashJoin::new(
            types,
            join.join_type,
            join.conditions,
            &join.left_projection_map,
            &join.right_projection_map,
            join.mark_types,
            settings.scalar_subquery_error_on_multiple_rows,
            left,
            right,
        )?));
    }
    if !join.left_projection_map.is_empty() || !join.right_projection_map.is_empty() {
        return Err(Error::not_implemented("join projection maps for range joins"));
    }
    let (lhs_cardinality, rhs_cardinality) = cardinalities;
    if lhs_cardinality < settings.nested_loop_join_threshold || rhs_cardinality < settings.nested_loop_join_threshold
    {
        can_iejoin = false;
        can_merge = false;
    }
    if can_merge
        && can_iejoin
        && (lhs_cardinality < settings.merge_join_threshold || rhs_cardinality < settings.merge_join_threshold)
    {
        can_iejoin = false;
    }
    // TODO(range joins): IEJoin and piecewise merge join.
    let _ = (can_iejoin, can_merge);
    if PhysicalNestedLoopJoin::is_supported(&join.conditions, join.join_type) {
        return Ok(Arc::new(PhysicalNestedLoopJoin::new(
            types,
            join.join_type,
            join.conditions,
            join.predicate,
            left,
            right,
        )));
    }
    let offset = left.types().len() as u64;
    let mut conditions = join.conditions;
    for condition in &mut conditions {
        rewrite_join_condition(&mut condition.right, offset);
    }
    let condition = conditions
        .into_iter()
        .map(JoinCondition::into_expression)
        .reduce(|left, right| Expression::conjunction(ExpressionType::ConjunctionAnd, vec![left, right]))
        .ok_or_else(|| Error::internal("blockwise join without conditions"))?;
    Ok(Arc::new(PhysicalBlockwiseNLJoin::new(
        types,
        join.join_type,
        condition,
        left,
        right,
    )?))
}

impl PlanGenerator {
    /// Plans a `LOGICAL_COMPARISON_JOIN` (`PlanComparisonJoin`).
    pub(crate) fn plan_comparison_join(
        &mut self,
        join: LogicalComparisonJoin,
        children: Vec<LogicalOperator>,
        types: Vec<LogicalType>,
    ) -> Result<PhysicalRef> {
        let mut children = children.into_iter();
        let (Some(left), Some(right)) = (children.next(), children.next()) else {
            return Err(Error::internal("comparison join must have two inputs"));
        };
        let cardinalities = (estimate_cardinality(&left), estimate_cardinality(&right));
        let left = self.plan(left)?;
        let right = self.plan(right)?;
        create_comparison_join(self, join, types, left, right, cardinalities)
    }
}
