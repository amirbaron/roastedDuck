//! Statistics of operators (DuckDB's `statistics/operator/propagate_*.cpp`).

use std::sync::Arc;

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_common::Result;
use rustdb_planner::operator::ExpressionTableFilter;
use rustdb_planner::{
    BoundLimitNode, ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator,
    LogicalOperatorType,
};
use rustdb_storage::TableFilter;
use rustdb_storage::statistics::{BaseStatistics, FilterPropagateResult};
use rustdb_types::compare::CompareOp;
use rustdb_types::{LogicalTypeId, Value};

use super::expression::is_compare_distinct;
use super::{
    NodeStatistics, StatisticsPropagator, greater, has_min_max, is_always_false, numeric_max, numeric_min,
    numeric_value, set_max, set_min,
};
use crate::expression_util::{flip_comparison, try_evaluate_scalar};
use crate::filter_pushdown::{FilterPushdown, empty_result};
use crate::join_order::relation_statistics::table_column_statistics;

/// A placeholder while an operator is moved out of the plan.
fn placeholder() -> LogicalOperator {
    LogicalOperator::new(LogicalKind::CrossProduct)
}

/// Replaces `node` by an empty result with its columns (DuckDB's
/// `ReplaceWithEmptyResult`).
fn replace_with_empty_result(node: &mut LogicalOperator) -> Result<()> {
    let op = std::mem::replace(node, placeholder());
    *node = empty_result(op)?;
    Ok(())
}

/// The comparison of a constant table filter as an expression type.
const fn comparison_type(op: CompareOp) -> ExpressionType {
    match op {
        CompareOp::Equal => ExpressionType::CompareEqual,
        CompareOp::NotEqual => ExpressionType::CompareNotequal,
        CompareOp::LessThan => ExpressionType::CompareLessthan,
        CompareOp::LessThanOrEqual => ExpressionType::CompareLessthanorequalto,
        CompareOp::GreaterThan => ExpressionType::CompareGreaterthan,
        CompareOp::GreaterThanOrEqual => ExpressionType::CompareGreaterthanorequalto,
    }
}

/// Whether the join type pads the left side with NULLs (DuckDB's `IsRightOuterJoin`).
const fn is_right_outer_join(join_type: JoinType) -> bool {
    matches!(join_type, JoinType::Outer | JoinType::Right)
}

/// Whether the join type pads the right side with NULLs (DuckDB's `IsLeftOuterJoin`).
const fn is_left_outer_join(join_type: JoinType) -> bool {
    matches!(join_type, JoinType::Left | JoinType::Outer)
}

/// Sets whether statistics can contain NULLs (DuckDB's `StatsInfo::CAN_HAVE_NULL_VALUES`
/// / `CANNOT_HAVE_NULL_VALUES`).
fn set_can_have_null(stats: &mut BaseStatistics, can_have_null: bool) {
    stats.set_validity(can_have_null, stats.can_have_no_null());
}

/// `stats_a.max * stats_b.max` style combination of cardinalities for joins (DuckDB's
/// `MultiplyCardinalities`).
fn multiply_cardinalities(
    stats: Option<NodeStatistics>,
    new_stats: NodeStatistics,
) -> Option<NodeStatistics> {
    let stats = stats?;
    let (Some(estimate), Some(max), Some(new_estimate), Some(new_max)) = (
        stats.estimated_cardinality,
        stats.max_cardinality,
        new_stats.estimated_cardinality,
        new_stats.max_cardinality,
    ) else {
        return None;
    };
    let product = i128::from(max) * i128::from(new_max);
    if product >= i128::from(i64::MAX) {
        return None;
    }
    Some(NodeStatistics::new(
        estimate.max(new_estimate),
        u64::try_from(product).ok()?,
    ))
}

/// The sum of cardinalities for unions (DuckDB's `AddCardinalities`).
fn add_cardinalities(stats: Option<NodeStatistics>, new_stats: NodeStatistics) -> Option<NodeStatistics> {
    let stats = stats?;
    let (Some(estimate), Some(max), Some(new_estimate), Some(new_max)) = (
        stats.estimated_cardinality,
        stats.max_cardinality,
        new_stats.estimated_cardinality,
        new_stats.max_cardinality,
    ) else {
        return None;
    };
    let sum = i128::from(max) + i128::from(new_max);
    if sum >= i128::from(i64::MAX) {
        return None;
    }
    Some(NodeStatistics::new(
        estimate + new_estimate,
        u64::try_from(sum).ok()?,
    ))
}

impl StatisticsPropagator<'_> {
    /// Propagates statistics through `node` and its subtree, possibly replacing it
    /// (DuckDB's `PropagateStatistics`).
    pub fn propagate_statistics(&mut self, node: &mut LogicalOperator) -> Result<Option<NodeStatistics>> {
        let result = self.propagate_node(node)?;
        // Materializing operators store their inputs compressed where statistics allow.
        self.compress_materialization(node)?;
        Ok(result)
    }

    /// DuckDB's `PropagateStatistics` dispatch by operator type.
    fn propagate_node(&mut self, node: &mut LogicalOperator) -> Result<Option<NodeStatistics>> {
        match node.operator_type() {
            LogicalOperatorType::LogicalAggregateAndGroupBy => self.propagate_aggregate(node),
            LogicalOperatorType::LogicalCrossProduct => self.propagate_cross_product(node),
            LogicalOperatorType::LogicalFilter => self.propagate_filter(node),
            LogicalOperatorType::LogicalGet => self.propagate_get(node),
            LogicalOperatorType::LogicalProjection => self.propagate_projection(node),
            LogicalOperatorType::LogicalAnyJoin
            | LogicalOperatorType::LogicalAsofJoin
            | LogicalOperatorType::LogicalComparisonJoin
            | LogicalOperatorType::LogicalDelimJoin => self.propagate_join(node),
            LogicalOperatorType::LogicalPositionalJoin => self.propagate_positional_join(node),
            LogicalOperatorType::LogicalUnion
            | LogicalOperatorType::LogicalExcept
            | LogicalOperatorType::LogicalIntersect => self.propagate_set_operation(node),
            LogicalOperatorType::LogicalOrderBy => self.propagate_order(node),
            LogicalOperatorType::LogicalWindow => self.propagate_window(node),
            LogicalOperatorType::LogicalLimit => {
                // DuckDB's `PropagateStatistics(LogicalLimit)`: a constant limit bounds the
                // rows.
                self.propagate_child(node, 0)?;
                Ok(match &node.kind {
                    LogicalKind::Limit {
                        limit: rustdb_planner::BoundLimitNode::ConstantValue(limit),
                        ..
                    } => Some(NodeStatistics::new(*limit, *limit)),
                    _ => None,
                })
            }
            _ => {
                for index in 0..node.children.len() {
                    self.propagate_child(node, index)?;
                }
                Ok(None)
            }
        }
    }

    /// Whether `expression` is (or folds to) `value` (DuckDB's `ExpressionIsConstant`).
    fn expression_is_constant(&self, expression: &Expression, value: &Value) -> bool {
        let constant = if let Some(constant) = expression.as_constant() {
            constant.clone()
        } else if expression.is_foldable() {
            match try_evaluate_scalar(self.optimizer.context(), expression) {
                Some(constant) => constant,
                None => return false,
            }
        } else {
            return false;
        };
        constant == *value
    }

    /// DuckDB's `HandleFilter`: what the statistics say about a filter condition; the
    /// statistics are narrowed by conditions that cannot be decided.
    fn handle_filter(&mut self, condition: &mut Expression) -> Result<FilterPropagateResult> {
        self.propagate_expression(condition)?;
        if self.expression_is_constant(condition, &Value::boolean(true)) {
            return Ok(FilterPropagateResult::FilterAlwaysTrue);
        }
        if is_constant_or_null(condition, true) {
            return Ok(FilterPropagateResult::FilterTrueOrNull);
        }
        if self.expression_is_constant(condition, &Value::boolean(false))
            || is_constant_or_null(condition, false)
        {
            return Ok(FilterPropagateResult::FilterFalseOrNull);
        }
        self.update_filter_statistics(condition);
        Ok(FilterPropagateResult::NoPruningPossible)
    }

    /// Narrows statistics by a filter condition (DuckDB's `UpdateFilterStatistics(Expression)`).
    fn update_filter_statistics(&mut self, condition: &Expression) {
        match &condition.kind {
            ExpressionKind::Between(between) => {
                self.update_filter_statistics_comparison(
                    &between.input,
                    &between.lower,
                    between.lower_comparison_type(),
                );
                self.update_filter_statistics_comparison(
                    &between.input,
                    &between.upper,
                    between.upper_comparison_type(),
                );
            }
            ExpressionKind::Comparison(comparison) => {
                self.update_filter_statistics_comparison(
                    &comparison.left,
                    &comparison.right,
                    condition.expression_type,
                );
            }
            _ => {}
        }
    }

    /// Marks a column as free of NULLs (DuckDB's `SetStatisticsNotNull`).
    fn set_statistics_not_null(&mut self, binding: ColumnBinding) {
        if let Some(stats) = self.statistics_map.get_mut(&binding) {
            set_can_have_null(stats, false);
        }
    }

    /// DuckDB's `UpdateFilterStatistics(Expression &left, Expression &right, ...)`.
    fn update_filter_statistics_comparison(
        &mut self,
        left: &Expression,
        right: &Expression,
        comparison: ExpressionType,
    ) {
        let compare_distinct = is_compare_distinct(comparison);
        if !compare_distinct {
            if let ExpressionKind::ColumnRef(column) = &left.kind {
                self.set_statistics_not_null(column.binding);
            }
            if let ExpressionKind::ColumnRef(column) = &right.kind {
                self.set_statistics_not_null(column.binding);
            }
        }
        match (&left.kind, &right.kind) {
            (ExpressionKind::Constant(constant), ExpressionKind::ColumnRef(column)) => {
                if let Some(stats) = self.statistics_map.get_mut(&column.binding) {
                    update_filter_statistics_constant(stats, flip_comparison(comparison), &constant.value);
                }
            }
            (ExpressionKind::ColumnRef(column), ExpressionKind::Constant(constant)) => {
                if let Some(stats) = self.statistics_map.get_mut(&column.binding) {
                    update_filter_statistics_constant(stats, comparison, &constant.value);
                }
            }
            (ExpressionKind::ColumnRef(left), ExpressionKind::ColumnRef(right)) => {
                if left.binding == right.binding {
                    let Some(stats) = self.statistics_map.get_mut(&left.binding) else {
                        return;
                    };
                    let mut copy = stats.clone();
                    update_filter_statistics_pair(stats, &mut copy, comparison);
                    return;
                }
                let (Some(mut lstats), Some(mut rstats)) = (
                    self.statistics_map.remove(&left.binding),
                    self.statistics_map.remove(&right.binding),
                ) else {
                    return;
                };
                update_filter_statistics_pair(&mut lstats, &mut rstats, comparison);
                self.statistics_map.insert(left.binding, lstats);
                self.statistics_map.insert(right.binding, rstats);
            }
            _ => {}
        }
    }

    /// DuckDB's `PropagateStatistics(LogicalFilter)`.
    fn propagate_filter(&mut self, node: &mut LogicalOperator) -> Result<Option<NodeStatistics>> {
        self.node_stats = self.propagate_child(node, 0)?;
        if node.children[0].operator_type() == LogicalOperatorType::LogicalEmptyResult {
            replace_with_empty_result(node)?;
            return Ok(Some(NodeStatistics::new(0, 0)));
        }
        let mut index = 0;
        while index < node.expressions.len() {
            let mut condition = std::mem::replace(
                &mut node.expressions[index],
                Expression::constant(Value::boolean(true)),
            );
            let result = self.handle_filter(&mut condition)?;
            node.expressions[index] = condition;
            match result {
                FilterPropagateResult::FilterAlwaysTrue => {
                    node.expressions.remove(index);
                    if node.expressions.is_empty() {
                        // Keep a filter with a projection map; the physical planner skips it.
                        let keep = matches!(&node.kind, LogicalKind::Filter { projection_map } if !projection_map.is_empty());
                        if !keep && let Some(child) = node.children.pop() {
                            *node = child;
                        }
                        break;
                    }
                }
                FilterPropagateResult::FilterFalseOrNull => {
                    replace_with_empty_result(node)?;
                    return Ok(Some(NodeStatistics::new(0, 0)));
                }
                _ => index += 1,
            }
        }
        Ok(self.node_stats.take())
    }

    /// DuckDB's `PropagateStatistics(LogicalGet)`: the table's column statistics,
    /// narrowed by (and pruning) the pushed-down table filters.
    fn propagate_get(&mut self, node: &mut LogicalOperator) -> Result<Option<NodeStatistics>> {
        let LogicalKind::Get(get) = &mut node.kind else {
            return Ok(None);
        };
        // Like DuckDB's member `node_stats`, only set when the function has a cardinality.
        if let Some(rows) = get.table_cardinality() {
            self.node_stats = Some(NodeStatistics::new(rows, rows));
        }
        if get.table.is_none() {
            // Only base table scans have column statistics.
            return Ok(self.node_stats.take());
        }
        for (index, column) in get.column_ids.iter().enumerate() {
            if let Some(stats) = table_column_statistics(self.optimizer.context(), get, column.index) {
                self.statistics_map
                    .insert(ColumnBinding::new(get.table_index, index as u64), stats);
            }
        }
        let filter_columns: Vec<usize> = get.table_filters.filters.keys().copied().collect();
        for filter_column in filter_columns {
            let Some(column_index) = get
                .column_ids
                .iter()
                .position(|column| column.index == filter_column as u64)
            else {
                continue;
            };
            let stats_binding = ColumnBinding::new(get.table_index, column_index as u64);
            let Some(mut stats) = self.statistics_map.remove(&stats_binding) else {
                continue;
            };
            let Some(mut filter) = get.table_filters.filters.remove(&filter_column) else {
                self.statistics_map.insert(stats_binding, stats);
                continue;
            };
            let result = self.propagate_table_filter(stats_binding, &mut stats, &mut filter)?;
            match result {
                FilterPropagateResult::FilterAlwaysTrue => {
                    // The filter is useless: drop it.
                }
                FilterPropagateResult::FilterTrueOrNull => {
                    let keep = is_constant_or_null_filter(&filter) && !can_replace_constant_or_null(&filter);
                    let replacement = if keep { filter } else { TableFilter::IsNotNull };
                    get.table_filters.filters.insert(filter_column, replacement);
                }
                FilterPropagateResult::FilterFalseOrNull | FilterPropagateResult::FilterAlwaysFalse => {
                    self.statistics_map.insert(stats_binding, stats);
                    replace_with_empty_result(node)?;
                    return Ok(Some(NodeStatistics::new(0, 0)));
                }
                FilterPropagateResult::NoPruningPossible => {
                    update_filter_statistics_table_filter(&mut stats, &filter);
                    get.table_filters.filters.insert(filter_column, filter);
                }
            }
            self.statistics_map.insert(stats_binding, stats);
        }
        Ok(self.node_stats.take())
    }

    /// DuckDB's `PropagateTableFilter`: expression filters are propagated as
    /// expressions first, then the statistics are checked against the filter.
    fn propagate_table_filter(
        &mut self,
        stats_binding: ColumnBinding,
        stats: &mut BaseStatistics,
        filter: &mut TableFilter,
    ) -> Result<FilterPropagateResult> {
        if let TableFilter::Expression(expression_filter) = filter
            && let Some(table_filter) = expression_filter.as_any().downcast_ref::<ExpressionTableFilter>()
        {
            // Run the predicate on the column binding, with the column's statistics.
            let mut reference = None;
            let mut condition = table_filter.expression.clone();
            replace_references(&mut condition, &mut |expression| {
                reference = Some(expression.clone());
                let mut column = Expression::column_ref(
                    expression.alias.clone(),
                    stats.logical_type().clone(),
                    stats_binding,
                    0,
                );
                column.alias.clone_from(&expression.alias);
                column
            });
            self.statistics_map.insert(stats_binding, stats.clone());
            let result = self.handle_filter(&mut condition)?;
            self.update_filter_statistics(&condition);
            if let Some(updated) = self.statistics_map.remove(&stats_binding) {
                *stats = updated;
            }
            if let Some(reference) = reference {
                replace_column_refs_with(&mut condition, &reference);
            }
            *filter = TableFilter::Expression(Arc::new(ExpressionTableFilter {
                expression: condition,
            }));
            if result != FilterPropagateResult::NoPruningPossible {
                return Ok(result);
            }
        }
        Ok(filter.check_statistics(stats))
    }

    /// DuckDB's `PropagateStatistics(LogicalProjection)`.
    fn propagate_projection(&mut self, node: &mut LogicalOperator) -> Result<Option<NodeStatistics>> {
        self.node_stats = self.propagate_child(node, 0)?;
        if node.children[0].operator_type() == LogicalOperatorType::LogicalEmptyResult {
            replace_with_empty_result(node)?;
            return Ok(self.node_stats.take());
        }
        let LogicalKind::Projection { table_index } = node.kind else {
            return Ok(self.node_stats.take());
        };
        for (index, expression) in node.expressions.iter_mut().enumerate() {
            if let Some(stats) = self.propagate_expression(expression)? {
                self.statistics_map
                    .entry(ColumnBinding::new(table_index, index as u64))
                    .or_insert(stats);
            }
        }
        Ok(self.node_stats.take())
    }

    /// DuckDB's `PropagateStatistics(LogicalAggregate)`.
    fn propagate_aggregate(&mut self, node: &mut LogicalOperator) -> Result<Option<NodeStatistics>> {
        self.node_stats = self.propagate_child(node, 0)?;
        let LogicalKind::Aggregate(aggregate) = &mut node.kind else {
            return Ok(self.node_stats.take());
        };
        let multiple_grouping_sets = aggregate.grouping_sets.len() > 1;
        let group_index = aggregate.group_index;
        let aggregate_index = aggregate.aggregate_index;
        aggregate.group_stats = vec![None; aggregate.groups.len()];
        for (index, group) in aggregate.groups.iter_mut().enumerate() {
            let Some(stats) = self.propagate_expression(group)? else {
                continue;
            };
            aggregate.group_stats[index] = Some(stats.clone());
            if multiple_grouping_sets {
                // Grouping sets can introduce NULLs into the groups.
                continue;
            }
            self.statistics_map
                .insert(ColumnBinding::new(group_index, index as u64), stats);
        }
        for (index, expression) in node.expressions.iter_mut().enumerate() {
            if let Some(stats) = self.propagate_expression(expression)? {
                self.statistics_map
                    .insert(ColumnBinding::new(aggregate_index, index as u64), stats);
            }
        }
        // Whole-table count/min/max may be answered by the row group statistics.
        self.try_execute_aggregates(node);
        Ok(self.node_stats.take())
    }

    /// DuckDB's `PropagateStatistics(LogicalCrossProduct)`.
    fn propagate_cross_product(&mut self, node: &mut LogicalOperator) -> Result<Option<NodeStatistics>> {
        let left = self.propagate_child(node, 0)?;
        let right = self.propagate_child(node, 1)?;
        let (Some(left), Some(right)) = (left, right) else {
            return Ok(None);
        };
        Ok(multiply_cardinalities(Some(left), right))
    }

    /// DuckDB's `PropagateStatistics(LogicalOrder)`.
    fn propagate_order(&mut self, node: &mut LogicalOperator) -> Result<Option<NodeStatistics>> {
        self.node_stats = self.propagate_child(node, 0)?;
        if let LogicalKind::Order { orders, .. } = &mut node.kind {
            for order in orders {
                self.propagate_expression(&mut order.expression)?;
            }
        }
        Ok(self.node_stats.take())
    }

    /// DuckDB's `PropagateStatistics(LogicalWindow)`: the partitions, orders and frame
    /// expressions of the window functions.
    fn propagate_window(&mut self, node: &mut LogicalOperator) -> Result<Option<NodeStatistics>> {
        self.node_stats = self.propagate_child(node, 0)?;
        for expression in &mut node.expressions {
            let ExpressionKind::Window(window) = &mut expression.kind else {
                continue;
            };
            for partition in &mut window.partitions {
                self.propagate_expression(partition)?;
            }
            for order in &mut window.orders {
                self.propagate_expression(&mut order.expression)?;
            }
            for frame in [
                &mut window.start_expr,
                &mut window.end_expr,
                &mut window.offset_expr,
                &mut window.default_expr,
            ]
            .into_iter()
            .flatten()
            {
                self.propagate_expression(frame)?;
            }
            for order in &mut window.arg_orders {
                self.propagate_expression(&mut order.expression)?;
            }
        }
        Ok(self.node_stats.take())
    }

    /// DuckDB's `PropagateUnion` / `PropagateStatistics(LogicalSetOperation)`.
    fn propagate_set_operation(&mut self, node: &mut LogicalOperator) -> Result<Option<NodeStatistics>> {
        let LogicalKind::SetOperation {
            table_index,
            column_count,
            ..
        } = node.kind
        else {
            return Ok(None);
        };
        let is_union = node.operator_type() == LogicalOperatorType::LogicalUnion;
        let mut child_stats = Vec::with_capacity(node.children.len());
        for index in 0..node.children.len() {
            child_stats.push(self.propagate_child(node, index)?);
        }
        let child_bindings: Vec<Vec<ColumnBinding>> = node
            .children
            .iter()
            .map(LogicalOperator::column_bindings)
            .collect();
        for column in 0..column_count as usize {
            let new_stats = if is_union {
                let mut merged: Option<BaseStatistics> = None;
                let mut complete = true;
                for bindings in &child_bindings {
                    let Some(stats) = bindings
                        .get(column)
                        .and_then(|binding| self.statistics_map.get(binding))
                    else {
                        complete = false;
                        break;
                    };
                    match &mut merged {
                        Some(merged) => merged.merge(stats),
                        None => merged = Some(stats.clone()),
                    }
                }
                if complete { merged } else { None }
            } else {
                // EXCEPT/INTERSECT: the left side's statistics are correct.
                let left = child_bindings[0]
                    .get(column)
                    .and_then(|binding| self.statistics_map.get(binding));
                let right = child_bindings
                    .get(1)
                    .and_then(|bindings| bindings.get(column))
                    .and_then(|binding| self.statistics_map.get(binding));
                left.filter(|_| right.is_some()).cloned()
            };
            if let Some(stats) = new_stats {
                self.statistics_map
                    .insert(ColumnBinding::new(table_index, column as u64), stats);
            }
        }
        if is_union {
            let mut stats = child_stats.into_iter();
            let mut result = stats.next().flatten();
            for other in stats {
                let Some(other) = other else {
                    return Ok(None);
                };
                result = add_cardinalities(result, other);
            }
            return Ok(result);
        }
        match (
            child_stats.first().copied().flatten(),
            child_stats.get(1).copied().flatten(),
        ) {
            (Some(left), Some(_)) => Ok(Some(left)),
            _ => Ok(None),
        }
    }

    /// DuckDB's `PropagateStatistics(LogicalPositionalJoin)`: both sides may be padded
    /// with NULLs.
    fn propagate_positional_join(&mut self, node: &mut LogicalOperator) -> Result<Option<NodeStatistics>> {
        // As in DuckDB, the cardinality lives in the member `node_stats`, which the
        // propagation of the other children moves out (so it is usually gone).
        self.node_stats = self.propagate_child(node, 0)?;
        for index in 1..node.children.len() {
            let child_stats = self.propagate_child(node, index)?;
            self.node_stats = match (self.node_stats, child_stats) {
                (Some(stats), Some(child)) => match (
                    stats.estimated_cardinality,
                    stats.max_cardinality,
                    child.estimated_cardinality,
                    child.max_cardinality,
                ) {
                    (Some(estimate), Some(max), Some(child_estimate), Some(child_max)) => Some(
                        NodeStatistics::new(estimate.max(child_estimate), max.max(child_max)),
                    ),
                    _ => None,
                },
                _ => None,
            };
        }
        for child in &node.children {
            for binding in child.column_bindings() {
                if let Some(stats) = self.statistics_map.get_mut(&binding) {
                    set_can_have_null(stats, true);
                }
            }
        }
        Ok(self.node_stats.take())
    }

    /// DuckDB's `PropagateStatistics(LogicalJoin)`.
    fn propagate_join(&mut self, node: &mut LogicalOperator) -> Result<Option<NodeStatistics>> {
        // As in DuckDB, the cardinality lives in the member `node_stats`, which the
        // propagation of the right child moves out (so join cardinalities are usually
        // unknown).
        self.node_stats = self.propagate_child(node, 0)?;
        for index in 1..node.children.len() {
            let child_stats = self.propagate_child(node, index)?;
            self.node_stats = match child_stats {
                None => None,
                Some(child) => self
                    .node_stats
                    .and_then(|stats| multiply_cardinalities(Some(stats), child)),
            };
        }
        let Some(join_type) = node.join_type() else {
            return Ok(self.node_stats.take());
        };
        let adds_null_on_left = is_right_outer_join(join_type);
        let adds_null_on_right = is_left_outer_join(join_type) || join_type == JoinType::Single;
        let left_bindings = if adds_null_on_left {
            node.children[0].column_bindings()
        } else {
            Vec::new()
        };
        let right_bindings = if adds_null_on_right {
            node.children[1].column_bindings()
        } else {
            Vec::new()
        };
        if matches!(node.kind, LogicalKind::ComparisonJoin(_)) {
            self.propagate_comparison_join(node)?;
        } else if let LogicalKind::AnyJoin { condition, .. } = &mut node.kind {
            self.propagate_expression(condition)?;
        }
        for binding in right_bindings.iter().chain(&left_bindings) {
            if let Some(stats) = self.statistics_map.get_mut(binding) {
                set_can_have_null(stats, true);
            }
        }
        Ok(self.node_stats.take())
    }

    /// DuckDB's `PropagateStatistics(LogicalComparisonJoin)`: conditions the statistics
    /// decide simplify or eliminate the join; inner joins narrow the statistics of both
    /// sides and filter the inputs accordingly.
    #[allow(clippy::too_many_lines)]
    fn propagate_comparison_join(&mut self, node: &mut LogicalOperator) -> Result<()> {
        let mut index = 0;
        loop {
            let LogicalKind::ComparisonJoin(join) = &mut node.kind else {
                return Ok(());
            };
            if index >= join.conditions.len() {
                return Ok(());
            }
            let join_type = join.join_type;
            let is_asof = join.operator_type == LogicalOperatorType::LogicalAsofJoin;
            let condition_count = join.conditions.len();
            let condition = &mut join.conditions[index];
            let comparison = condition.comparison;
            let stats_left = self.propagate_expression(&mut condition.left)?;
            let stats_right = self.propagate_expression(&mut condition.right)?;
            if let (Some(left), Some(right)) = (&stats_left, &stats_right) {
                if is_compare_distinct(comparison) && left.can_have_null() && right.can_have_null() {
                    // NULLs compare equal and both sides have them: nothing to do.
                    index += 1;
                    continue;
                }
                let prune_result = Self::propagate_comparison(left, right, comparison);
                if is_always_false(prune_result) {
                    match join_type {
                        JoinType::RightSemi | JoinType::Semi | JoinType::Inner => {
                            replace_with_empty_result(node)?;
                            return Ok(());
                        }
                        JoinType::RightAnti | JoinType::Anti => {
                            if join_type == JoinType::RightAnti {
                                node.children.swap(0, 1);
                            }
                            let child = std::mem::replace(&mut node.children[0], placeholder());
                            *node = child;
                            return Ok(());
                        }
                        JoinType::Left => {
                            replace_with_empty_result(&mut node.children[1])?;
                            return Ok(());
                        }
                        JoinType::Right => {
                            replace_with_empty_result(&mut node.children[0])?;
                            return Ok(());
                        }
                        _ => {}
                    }
                } else if prune_result == FilterPropagateResult::FilterAlwaysTrue {
                    let asof_inequality = is_asof
                        && matches!(
                            comparison,
                            ExpressionType::CompareGreaterthan
                                | ExpressionType::CompareGreaterthanorequalto
                                | ExpressionType::CompareLessthan
                                | ExpressionType::CompareLessthanorequalto
                        );
                    if asof_inequality {
                        // The ASOF inequality also means "closest match only".
                        index += 1;
                        continue;
                    }
                    if condition_count > 1 {
                        join.conditions.remove(index);
                        continue;
                    }
                    match join_type {
                        JoinType::RightSemi | JoinType::Semi => {
                            if join_type == JoinType::RightSemi {
                                node.children.swap(0, 1);
                            }
                            // The right side only has to produce a row.
                            let right = std::mem::replace(&mut node.children[1], placeholder());
                            let mut limit = LogicalOperator::new(LogicalKind::Limit {
                                limit: BoundLimitNode::ConstantValue(1),
                                offset: BoundLimitNode::Unset,
                            })
                            .with_children(vec![right]);
                            limit.estimated_cardinality = Some(1);
                            let left = std::mem::replace(&mut node.children[0], placeholder());
                            *node = cross_product(left, limit);
                            return Ok(());
                        }
                        JoinType::Inner => {
                            let left = std::mem::replace(&mut node.children[0], placeholder());
                            let right = std::mem::replace(&mut node.children[1], placeholder());
                            *node = cross_product(left, right);
                            return Ok(());
                        }
                        JoinType::Anti | JoinType::RightAnti => {
                            replace_with_empty_result(node)?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
            if is_compare_distinct(comparison) {
                index += 1;
                continue;
            }
            if matches!(join_type, JoinType::Inner | JoinType::Semi) {
                let LogicalKind::ComparisonJoin(join) = &mut node.kind else {
                    return Ok(());
                };
                let condition = &mut join.conditions[index];
                self.update_filter_statistics_comparison(&condition.left, &condition.right, comparison);
                let updated_left = self.propagate_expression(&mut condition.left)?;
                let updated_right = self.propagate_expression(&mut condition.right)?;
                if let (Some(before_left), Some(before_right), Some(after_left), Some(after_right)) =
                    (&stats_left, &stats_right, &updated_left, &updated_right)
                    && matches!(condition.left.kind, ExpressionKind::ColumnRef(_))
                    && matches!(condition.right.kind, ExpressionKind::ColumnRef(_))
                {
                    let left_expression = condition.left.clone();
                    let right_expression = condition.right.clone();
                    self.create_filter_from_join_stats(
                        &mut node.children[0],
                        &left_expression,
                        before_left,
                        after_left,
                    )?;
                    self.create_filter_from_join_stats(
                        &mut node.children[1],
                        &right_expression,
                        before_right,
                        after_right,
                    )?;
                }
            }
            index += 1;
        }
    }

    /// Filters the input of a join to the range the join keeps (DuckDB's
    /// `CreateFilterFromJoinStats`).
    fn create_filter_from_join_stats(
        &mut self,
        child: &mut LogicalOperator,
        expression: &Expression,
        before: &BaseStatistics,
        after: &BaseStatistics,
    ) -> Result<()> {
        if !matches!(expression.kind, ExpressionKind::ColumnRef(_))
            || !is_integral(&expression.return_type)
            || !has_min_max(before)
            || !has_min_max(after)
        {
            return Ok(());
        }
        let (Some(min_before), Some(max_before), Some(min_after), Some(max_after)) = (
            numeric_min(before),
            numeric_max(before),
            numeric_min(after),
            numeric_max(after),
        ) else {
            return Ok(());
        };
        let ty = &expression.return_type;
        let mut filters = Vec::new();
        if greater(min_after, min_before) {
            filters.push(Expression::comparison(
                ExpressionType::CompareGreaterthanorequalto,
                expression.clone(),
                Expression::constant(min_after.to_value(ty)),
            ));
        }
        if greater(max_before, max_after) {
            filters.push(Expression::comparison(
                ExpressionType::CompareLessthanorequalto,
                expression.clone(),
                Expression::constant(max_after.to_value(ty)),
            ));
        }
        if filters.is_empty() {
            return Ok(());
        }
        let input = std::mem::replace(child, placeholder());
        let filter = LogicalOperator::new(LogicalKind::Filter {
            projection_map: Vec::new(),
        })
        .with_expressions(filters)
        .with_children(vec![input]);
        // Mark joins must not become semi joins here.
        *child = FilterPushdown::new(self.optimizer, false).rewrite(filter)?;
        Ok(())
    }
}

/// A cross product of two inputs (DuckDB's `LogicalCrossProduct::Create`).
fn cross_product(left: LogicalOperator, right: LogicalOperator) -> LogicalOperator {
    LogicalOperator::new(LogicalKind::CrossProduct).with_children(vec![left, right])
}

/// DuckDB's `LogicalType::IsIntegral`.
fn is_integral(ty: &rustdb_types::LogicalType) -> bool {
    matches!(
        ty.id(),
        LogicalTypeId::TinyInt
            | LogicalTypeId::SmallInt
            | LogicalTypeId::Integer
            | LogicalTypeId::BigInt
            | LogicalTypeId::UTinyInt
            | LogicalTypeId::USmallInt
            | LogicalTypeId::UInteger
            | LogicalTypeId::UBigInt
            | LogicalTypeId::HugeInt
            | LogicalTypeId::UHugeInt
    )
}

/// Whether `expression` is `constant_or_null(value, ...)` (DuckDB's
/// `ConstantOrNull::IsConstantOrNull`).
pub(super) fn is_constant_or_null(expression: &Expression, value: bool) -> bool {
    let ExpressionKind::Function(function) = &expression.kind else {
        return false;
    };
    &*function.function.name == "constant_or_null"
        && function
            .children
            .first()
            .and_then(Expression::as_constant)
            .is_some_and(|constant| *constant == Value::boolean(value))
}

/// Whether a table filter is a `constant_or_null(TRUE, ...)` expression filter.
fn is_constant_or_null_filter(filter: &TableFilter) -> bool {
    expression_filter(filter).is_some_and(|expression| is_constant_or_null(expression, true))
}

/// Whether a `constant_or_null` table filter only reads the column and constants (DuckDB's
/// `CanReplaceConstantOrNull`).
fn can_replace_constant_or_null(filter: &TableFilter) -> bool {
    let Some(expression) = expression_filter(filter) else {
        return true;
    };
    let ExpressionKind::Function(function) = &expression.kind else {
        return true;
    };
    function.children.iter().skip(1).all(|child| {
        matches!(
            child.kind,
            ExpressionKind::Reference(_) | ExpressionKind::Constant(_)
        )
    })
}

/// The predicate of an expression table filter.
fn expression_filter(filter: &TableFilter) -> Option<&Expression> {
    let TableFilter::Expression(expression) = filter else {
        return None;
    };
    expression
        .as_any()
        .downcast_ref::<ExpressionTableFilter>()
        .map(|filter| &filter.expression)
}

/// Replaces the input references of a table filter predicate.
fn replace_references(expression: &mut Expression, replace: &mut impl FnMut(&Expression) -> Expression) {
    if matches!(expression.kind, ExpressionKind::Reference(_)) {
        *expression = replace(expression);
        return;
    }
    expression.for_each_child_mut(|child| replace_references(child, replace));
}

/// Turns the column references back into the table filter's input reference.
fn replace_column_refs_with(expression: &mut Expression, reference: &Expression) {
    if matches!(expression.kind, ExpressionKind::ColumnRef(_)) {
        let alias = std::mem::take(&mut expression.alias);
        *expression = reference.clone();
        if !alias.is_empty() {
            expression.alias = alias;
        }
        return;
    }
    expression.for_each_child_mut(|child| replace_column_refs_with(child, reference));
}

/// Narrows statistics by `x <comparison> constant` (DuckDB's
/// `UpdateFilterStatistics(BaseStatistics &, ExpressionType, const Value &)`).
fn update_filter_statistics_constant(
    stats: &mut BaseStatistics,
    comparison: ExpressionType,
    constant: &Value,
) {
    if !is_compare_distinct(comparison) {
        set_can_have_null(stats, false);
    }
    if !stats.logical_type().is_numeric() || !has_min_max(stats) {
        return;
    }
    let bound = numeric_value(constant);
    if bound.is_some_and(|bound| numeric_min(stats).is_some_and(|min| min.compare(bound).is_none())) {
        return;
    }
    match comparison {
        ExpressionType::CompareLessthan | ExpressionType::CompareLessthanorequalto => set_max(stats, bound),
        ExpressionType::CompareGreaterthan | ExpressionType::CompareGreaterthanorequalto => {
            set_min(stats, bound);
        }
        ExpressionType::CompareEqual => {
            set_min(stats, bound);
            set_max(stats, bound);
        }
        _ => {}
    }
}

/// Narrows the statistics of both sides of `left <comparison> right` (DuckDB's
/// `UpdateFilterStatistics(BaseStatistics &, BaseStatistics &, ExpressionType)`).
fn update_filter_statistics_pair(
    left: &mut BaseStatistics,
    right: &mut BaseStatistics,
    comparison: ExpressionType,
) {
    if !is_compare_distinct(comparison) {
        set_can_have_null(left, false);
        set_can_have_null(right, false);
    }
    if !left.logical_type().is_numeric() || !has_min_max(left) || !has_min_max(right) {
        return;
    }
    let (Some(lmin), Some(lmax), Some(rmin), Some(rmax)) = (
        numeric_min(left),
        numeric_max(left),
        numeric_min(right),
        numeric_max(right),
    ) else {
        return;
    };
    if lmin.compare(rmin).is_none() {
        return;
    }
    match comparison {
        ExpressionType::CompareLessthan | ExpressionType::CompareLessthanorequalto => {
            if greater(lmax, rmax) {
                set_max(left, Some(rmax));
            }
            if greater(lmin, rmin) {
                set_min(right, Some(lmin));
            }
        }
        ExpressionType::CompareGreaterthan | ExpressionType::CompareGreaterthanorequalto => {
            if greater(rmax, lmax) {
                set_max(right, Some(lmax));
            }
            if greater(rmin, lmin) {
                set_min(left, Some(rmin));
            }
        }
        ExpressionType::CompareEqual | ExpressionType::CompareNotDistinctFrom => {
            if greater(lmin, rmin) {
                set_min(right, Some(lmin));
            } else {
                set_min(left, Some(rmin));
            }
            if greater(rmax, lmax) {
                set_max(right, Some(lmax));
            } else {
                set_max(left, Some(rmax));
            }
        }
        _ => {}
    }
}

/// Narrows statistics by a table filter (DuckDB's `UpdateFilterStatistics(BaseStatistics &,
/// const TableFilter &)`).
fn update_filter_statistics_table_filter(stats: &mut BaseStatistics, filter: &TableFilter) {
    match filter {
        TableFilter::And(children) => {
            for child in children {
                update_filter_statistics_table_filter(stats, child);
            }
        }
        TableFilter::ConstantComparison { op, constant } => {
            update_filter_statistics_constant(stats, comparison_type(*op), constant);
        }
        _ => {}
    }
}
