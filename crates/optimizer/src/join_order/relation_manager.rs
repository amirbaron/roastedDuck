//! Extraction of the join relations (DuckDB's `RelationManager`): the inputs of a tree of
//! inner joins and cross products, with their statistics, and the filters and join
//! conditions that connect them.
//!
//! DuckDB keeps pointers to the relations and filter operators inside the plan; here they
//! are addressed by their path of child indexes from the root of the plan being
//! optimized, which stays valid because the tree's shape does not change until the plan
//! is rebuilt.

use std::collections::{BTreeSet, HashMap};

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_common::Result;
use rustdb_planner::{
    BoundLimitNode, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};

use super::JoinOrderOptimizer;
use super::cardinality_estimator::DEFAULT_SEMI_ANTI_SELECTIVITY;
use super::query_graph_manager::FilterInfo;
use super::relation_set::RelationSet;
use super::relation_statistics::{
    DEFAULT_SELECTIVITY, RelationStats, combine_stats_of_non_reorderable_operator, extract_aggregation_stats,
    extract_get_stats, extract_projection_stats, extract_window_stats, uniform_stats,
};
use crate::expression_util::visit_column_refs;

/// The path of child indexes from the root to an operator.
pub type OpPath = Vec<usize>;

/// The operator at `path` below `root`.
pub fn node_mut<'a>(root: &'a mut LogicalOperator, path: &[usize]) -> &'a mut LogicalOperator {
    path.iter().fold(root, |op, &index| &mut op.children[index])
}

/// A join input (DuckDB's `SingleJoinRelation`).
#[derive(Debug)]
pub struct SingleJoinRelation {
    pub path: OpPath,
    pub stats: RelationStats,
}

/// Collects the relations of a join tree (DuckDB's `RelationManager`).
#[derive(Debug, Default)]
pub struct RelationManager {
    pub relations: Vec<SingleJoinRelation>,
    /// The relation each table index belongs to.
    pub relation_mapping: HashMap<u64, usize>,
    /// Relations that forced cross products may not combine (right sides of SEMI/ANTI).
    pub no_cross_product_relations: BTreeSet<usize>,
}

/// Operators that start a relation (DuckDB's `OperatorNeedsRelation`).
const fn operator_needs_relation(op_type: LogicalOperatorType) -> bool {
    matches!(
        op_type,
        LogicalOperatorType::LogicalProjection
            | LogicalOperatorType::LogicalExpressionGet
            | LogicalOperatorType::LogicalGet
            | LogicalOperatorType::LogicalUnnest
            | LogicalOperatorType::LogicalDelimGet
            | LogicalOperatorType::LogicalAggregateAndGroupBy
            | LogicalOperatorType::LogicalWindow
            | LogicalOperatorType::LogicalSample
    )
}

/// Operators whose inputs are optimized separately (DuckDB's `OperatorIsNonReorderable`).
const fn operator_is_non_reorderable(op_type: LogicalOperatorType) -> bool {
    matches!(
        op_type,
        LogicalOperatorType::LogicalUnion
            | LogicalOperatorType::LogicalExcept
            | LogicalOperatorType::LogicalIntersect
            | LogicalOperatorType::LogicalAnyJoin
            | LogicalOperatorType::LogicalAsofJoin
    )
}

fn contains_column_ref(expression: &Expression) -> bool {
    let mut found = false;
    visit_column_refs(expression, &mut |_| found = true);
    found
}

/// Whether a join may be reordered: cross products, and inner/semi/anti comparison
/// joins with a condition between columns of both sides (DuckDB's `JoinIsReorderable`).
fn join_is_reorderable(op: &LogicalOperator) -> bool {
    match &op.kind {
        LogicalKind::CrossProduct => true,
        LogicalKind::ComparisonJoin(join)
            if op.operator_type() == LogicalOperatorType::LogicalComparisonJoin =>
        {
            matches!(join.join_type, JoinType::Inner | JoinType::Semi | JoinType::Anti)
                && join.conditions.iter().any(|condition| {
                    contains_column_ref(&condition.left) && contains_column_ref(&condition.right)
                })
        }
        _ => false,
    }
}

/// Whether a chain of single-child operators ends in a non-reorderable operator or a
/// leaf (DuckDB's `HasNonReorderableChild`).
fn has_non_reorderable_child(op: &LogicalOperator) -> bool {
    let mut current = op;
    while current.children.len() == 1 {
        let op_type = current.operator_type();
        if operator_needs_relation(op_type) || operator_is_non_reorderable(op_type) {
            return true;
        }
        current = &current.children[0];
        if current.operator_type() == LogicalOperatorType::LogicalComparisonJoin
            && !join_is_reorderable(current)
        {
            return true;
        }
    }
    current.children.is_empty()
}

/// Caps the stats by a constant LIMIT above the relation (DuckDB's `ModifyStatsIfLimit`).
fn modify_stats_if_limit(limit: Option<u64>, stats: &mut RelationStats) {
    if let Some(limit) = limit {
        stats.cardinality = stats.cardinality.min(limit);
    }
}

/// Replaces a child with a placeholder and returns it.
fn take_child(op: &mut LogicalOperator, index: usize) -> LogicalOperator {
    std::mem::replace(
        &mut op.children[index],
        LogicalOperator::new(LogicalKind::DummyScan {
            table_index: u64::MAX,
        }),
    )
}

impl RelationManager {
    pub fn relation_stats(&self) -> Vec<RelationStats> {
        self.relations
            .iter()
            .map(|relation| relation.stats.clone())
            .collect()
    }

    /// DuckDB's `AddAggregateOrWindowRelation`: every table index of the operator maps to
    /// the new relation.
    fn add_aggregate_or_window_relation(
        &mut self,
        root: &mut LogicalOperator,
        path: &[usize],
        stats: RelationStats,
    ) {
        let relation_id = self.relations.len();
        let op = node_mut(root, path);
        for binding in op.column_bindings() {
            self.relation_mapping
                .entry(binding.table_index)
                .or_insert(relation_id);
        }
        op.estimated_cardinality = Some(stats.cardinality);
        self.relations.push(SingleJoinRelation {
            path: path.to_vec(),
            stats,
        });
    }

    /// DuckDB's `AddRelation`.
    fn add_relation(&mut self, root: &mut LogicalOperator, path: &[usize], stats: RelationStats) {
        let relation_id = self.relations.len();
        let op = node_mut(root, path);
        let table_indexes = op.table_indexes();
        let is_mark = matches!(&op.kind, LogicalKind::ComparisonJoin(join)
            if op.operator_type() == LogicalOperatorType::LogicalComparisonJoin && join.join_type == JoinType::Mark);
        let get_all_child_bindings = match op.operator_type() {
            LogicalOperatorType::LogicalUnnest => true,
            LogicalOperatorType::LogicalGet => !op.children.is_empty(),
            _ => false,
        };
        if table_indexes.is_empty() || is_mark {
            for reference in crate::filter_pushdown::table_references(op) {
                self.relation_mapping.insert(reference, relation_id);
            }
        } else if get_all_child_bindings {
            for binding in op.column_bindings() {
                self.relation_mapping.insert(binding.table_index, relation_id);
            }
        } else if let Some(&table_index) = table_indexes.first() {
            self.relation_mapping.insert(table_index, relation_id);
        }
        op.estimated_cardinality = Some(stats.cardinality);
        self.relations.push(SingleJoinRelation {
            path: path.to_vec(),
            stats,
        });
    }

    /// Optimizes one child of the operator at `path` with a child optimizer.
    fn optimize_child(
        optimizer: &JoinOrderOptimizer<'_>,
        root: &mut LogicalOperator,
        path: &[usize],
        child_index: usize,
        stats: &mut RelationStats,
    ) -> Result<()> {
        let op = node_mut(root, path);
        let child = take_child(op, child_index);
        let child = optimizer.create_child_optimizer().optimize(child, Some(stats))?;
        node_mut(root, path).children[child_index] = child;
        Ok(())
    }

    /// DuckDB's `AddRelationWithChildren`.
    #[allow(clippy::too_many_arguments)]
    fn add_relation_with_children(
        &mut self,
        optimizer: &JoinOrderOptimizer<'_>,
        root: &mut LogicalOperator,
        op_path: &[usize],
        input_path: &[usize],
        limit: Option<u64>,
        has_datasource_filters: bool,
    ) -> Result<()> {
        let mut child_stats = RelationStats::default();
        Self::optimize_child(optimizer, root, op_path, 0, &mut child_stats)?;
        if has_datasource_filters {
            child_stats.cardinality = (child_stats.cardinality as f64 * DEFAULT_SELECTIVITY) as u64;
        }
        modify_stats_if_limit(limit, &mut child_stats);
        self.add_relation(root, input_path, child_stats);
        Ok(())
    }

    /// Collects the relations below `input_path` (DuckDB's `ExtractJoinRelations`);
    /// returns whether the tree can be reordered.
    pub fn extract_join_relations(
        &mut self,
        optimizer: &JoinOrderOptimizer<'_>,
        root: &mut LogicalOperator,
        input_path: &[usize],
        filter_operators: &mut Vec<OpPath>,
    ) -> Result<bool> {
        let mut path = input_path.to_vec();
        let mut has_datasource_filters = false;
        let mut limit: Option<u64> = None;
        loop {
            let op = node_mut(root, &path);
            if op.children.len() != 1 || operator_needs_relation(op.operator_type()) {
                break;
            }
            if matches!(op.kind, LogicalKind::Filter { .. }) {
                if has_non_reorderable_child(op) {
                    has_datasource_filters = true;
                }
                filter_operators.push(path.clone());
            }
            if let LogicalKind::Limit { limit: node, .. } = &op.kind {
                limit = match node {
                    BoundLimitNode::ConstantValue(value) => Some(*value),
                    _ => None,
                };
            }
            path.push(0);
        }
        let op = node_mut(root, &path);
        let op_type = op.operator_type();
        let mut non_reorderable_operation = operator_is_non_reorderable(op_type);
        if op_type == LogicalOperatorType::LogicalComparisonJoin {
            if join_is_reorderable(op) {
                filter_operators.push(path.clone());
            } else {
                non_reorderable_operation = true;
            }
        }
        if non_reorderable_operation {
            let child_count = node_mut(root, &path).children.len();
            let mut children_stats = Vec::with_capacity(child_count);
            for child_index in 0..child_count {
                let mut stats = RelationStats::default();
                Self::optimize_child(optimizer, root, &path, child_index, &mut stats)?;
                children_stats.push(stats);
            }
            let op = node_mut(root, &path);
            let mut combined = combine_stats_of_non_reorderable_operator(op, &children_stats);
            op.estimated_cardinality = Some(combined.cardinality);
            if has_datasource_filters {
                combined.cardinality = (combined.cardinality as f64 * DEFAULT_SELECTIVITY).max(1.0) as u64;
            }
            self.add_relation(root, input_path, combined);
            return Ok(true);
        }
        match op_type {
            LogicalOperatorType::LogicalAggregateAndGroupBy => {
                let mut child_stats = RelationStats::default();
                Self::optimize_child(optimizer, root, &path, 0, &mut child_stats)?;
                let op = node_mut(root, &path);
                let bindings = op.column_bindings();
                let LogicalKind::Aggregate(aggregate) = &op.kind else {
                    return Ok(false);
                };
                let mut stats = extract_aggregation_stats(aggregate, &bindings, &child_stats);
                op.estimated_cardinality = Some(stats.cardinality);
                if has_datasource_filters {
                    stats.cardinality = (stats.cardinality as f64 * DEFAULT_SELECTIVITY) as u64;
                }
                modify_stats_if_limit(limit, &mut child_stats);
                self.add_aggregate_or_window_relation(root, input_path, stats);
                Ok(true)
            }
            LogicalOperatorType::LogicalWindow => {
                let mut child_stats = RelationStats::default();
                Self::optimize_child(optimizer, root, &path, 0, &mut child_stats)?;
                let op = node_mut(root, &path);
                let mut stats = extract_window_stats(op, &child_stats);
                op.estimated_cardinality = Some(stats.cardinality);
                if has_datasource_filters {
                    stats.cardinality = (stats.cardinality as f64 * DEFAULT_SELECTIVITY) as u64;
                }
                modify_stats_if_limit(limit, &mut child_stats);
                self.add_aggregate_or_window_relation(root, input_path, stats);
                Ok(true)
            }
            LogicalOperatorType::LogicalUnnest => {
                self.add_relation_with_children(
                    optimizer,
                    root,
                    &path,
                    input_path,
                    limit,
                    has_datasource_filters,
                )?;
                Ok(true)
            }
            LogicalOperatorType::LogicalComparisonJoin => {
                let join_type = op.join_type();
                let mut left_path = path.clone();
                left_path.push(0);
                let can_reorder_left =
                    self.extract_join_relations(optimizer, root, &left_path, filter_operators)?;
                let mut right_path = path.clone();
                right_path.push(1);
                let can_reorder_right = if matches!(join_type, Some(JoinType::Semi | JoinType::Anti)) {
                    let mut child_stats = RelationStats::default();
                    Self::optimize_child(optimizer, root, &path, 1, &mut child_stats)?;
                    self.add_relation(root, &right_path, child_stats);
                    let relation_id = self.relations.len() - 1;
                    self.no_cross_product_relations.insert(relation_id);
                    for binding in node_mut(root, &right_path).column_bindings() {
                        self.relation_mapping.insert(binding.table_index, relation_id);
                    }
                    true
                } else {
                    self.extract_join_relations(optimizer, root, &right_path, filter_operators)?
                };
                Ok(can_reorder_left && can_reorder_right)
            }
            LogicalOperatorType::LogicalCrossProduct => {
                let mut left_path = path.clone();
                left_path.push(0);
                let can_reorder_left =
                    self.extract_join_relations(optimizer, root, &left_path, filter_operators)?;
                let mut right_path = path.clone();
                right_path.push(1);
                let can_reorder_right =
                    self.extract_join_relations(optimizer, root, &right_path, filter_operators)?;
                Ok(can_reorder_left && can_reorder_right)
            }
            LogicalOperatorType::LogicalDummyScan => {
                let stats = uniform_stats(op, 1, 1, "dummy_scan_column", "dummy scan");
                self.add_relation(root, input_path, stats);
                Ok(true)
            }
            LogicalOperatorType::LogicalExpressionGet => {
                let cardinality = op.estimate_cardinality();
                let stats = uniform_stats(
                    op,
                    cardinality,
                    cardinality,
                    "expression_get_column",
                    "expression_get",
                );
                self.add_relation(root, input_path, stats);
                Ok(true)
            }
            LogicalOperatorType::LogicalGet => {
                if !op.children.is_empty() {
                    self.add_relation_with_children(
                        optimizer,
                        root,
                        &path,
                        input_path,
                        limit,
                        has_datasource_filters,
                    )?;
                    return Ok(true);
                }
                let mut stats = extract_get_stats(op, optimizer.context);
                if has_datasource_filters {
                    stats.cardinality = (stats.cardinality as f64 * DEFAULT_SELECTIVITY).max(1.0) as u64;
                }
                modify_stats_if_limit(limit, &mut stats);
                self.add_relation(root, input_path, stats);
                Ok(true)
            }
            LogicalOperatorType::LogicalProjection => {
                let mut child_stats = RelationStats::default();
                Self::optimize_child(optimizer, root, &path, 0, &mut child_stats)?;
                let op = node_mut(root, &path);
                let mut stats = extract_projection_stats(op, &child_stats);
                op.estimated_cardinality = Some(stats.cardinality);
                modify_stats_if_limit(limit, &mut stats);
                self.add_relation(root, input_path, stats);
                Ok(true)
            }
            LogicalOperatorType::LogicalEmptyResult => {
                let mut stats = uniform_stats(op, 1, 0, "empty_result_column", "");
                stats.cardinality = RelationStats::default().cardinality;
                op.estimated_cardinality = Some(stats.cardinality);
                self.add_relation(root, input_path, stats);
                Ok(true)
            }
            LogicalOperatorType::LogicalMaterializedCte | LogicalOperatorType::LogicalRecursiveCte => {
                let table_index = op.table_indexes().first().copied().unwrap_or(0);
                let mut lhs_stats = RelationStats::default();
                Self::optimize_child(optimizer, root, &path, 0, &mut lhs_stats)?;
                let child_1_card = if lhs_stats.stats_initialized {
                    lhs_stats.cardinality
                } else {
                    0
                };
                let mut rhs_optimizer = optimizer.create_child_optimizer();
                rhs_optimizer
                    .materialized_cte_stats
                    .insert(table_index, lhs_stats);
                if op_type == LogicalOperatorType::LogicalRecursiveCte {
                    rhs_optimizer.recursive_cte_indexes.insert(table_index);
                }
                let mut rhs_stats = RelationStats::default();
                let op = node_mut(root, &path);
                let child = take_child(op, 1);
                let child = rhs_optimizer.optimize(child, Some(&mut rhs_stats))?;
                let op = node_mut(root, &path);
                op.children[1] = child;
                let child_2_card = if rhs_stats.stats_initialized {
                    rhs_stats.cardinality
                } else {
                    0
                };
                op.estimated_cardinality = Some(if op_type == LogicalOperatorType::LogicalRecursiveCte {
                    child_1_card + child_2_card * 1000
                } else {
                    child_2_card
                });
                Ok(false)
            }
            LogicalOperatorType::LogicalCteRef => {
                let LogicalKind::CteRef { cte_index, .. } = op.kind else {
                    return Ok(false);
                };
                let Some(stats) = optimizer.materialized_cte_stats.get(&cte_index).cloned() else {
                    return Err(rustdb_common::Error::internal(format!(
                        "Unable to find materialized CTE stats with index {cte_index}"
                    )));
                };
                op.estimated_cardinality = Some(stats.cardinality);
                self.add_relation(root, input_path, stats);
                Ok(!optimizer.recursive_cte_indexes.contains(&cte_index))
            }
            LogicalOperatorType::LogicalDelimJoin => {
                self.extract_delim_join(optimizer, root, &path, input_path)?;
                Ok(false)
            }
            LogicalOperatorType::LogicalDelimGet => {
                let stats = optimizer
                    .delim_scan_stats
                    .clone()
                    .ok_or_else(|| rustdb_common::Error::internal("Unable to find delim scan stats!"))?;
                op.estimated_cardinality = Some(stats.cardinality);
                self.add_aggregate_or_window_relation(root, input_path, stats);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// The delim join case of `ExtractJoinRelations`: its inputs are optimized with the
    /// duplicate-eliminated columns' stats available to the delim scans.
    fn extract_delim_join(
        &mut self,
        optimizer: &JoinOrderOptimizer<'_>,
        root: &mut LogicalOperator,
        path: &[usize],
        input_path: &[usize],
    ) -> Result<()> {
        let mut lhs_stats = RelationStats::default();
        Self::optimize_child(optimizer, root, path, 0, &mut lhs_stats)?;
        let op = node_mut(root, path);
        let LogicalKind::ComparisonJoin(join) = &op.kind else {
            return Ok(());
        };
        let join_type = join.join_type;
        let groups = join.duplicate_eliminated_columns.clone();
        let grouping_set: rustdb_planner::operator::GroupingSet = (0..groups.len() as u64).collect();
        let dummy_aggregate = rustdb_planner::operator::LogicalAggregate {
            group_index: u64::MAX - 1,
            aggregate_index: u64::MAX,
            groupings_index: u64::MAX,
            groups,
            grouping_sets: vec![grouping_set],
            grouping_functions: Vec::new(),
            group_stats: Vec::new(),
            distinct_validity: rustdb_planner::TupleDataValidityType::default(),
        };
        let dummy = LogicalOperator::new(LogicalKind::Aggregate(Box::new(dummy_aggregate)));
        let bindings = dummy.column_bindings();
        let LogicalKind::Aggregate(dummy_aggregate) = &dummy.kind else {
            return Ok(());
        };
        let lhs_delim_stats = extract_aggregation_stats(dummy_aggregate, &bindings, &lhs_stats);
        let mut rhs_optimizer = optimizer.create_child_optimizer();
        rhs_optimizer.delim_scan_stats = Some(lhs_delim_stats);
        let mut rhs_stats = RelationStats::default();
        let child = take_child(node_mut(root, path), 1);
        let child = rhs_optimizer.optimize(child, Some(&mut rhs_stats))?;
        node_mut(root, path).children[1] = child;
        let mut stats = match join_type {
            JoinType::Right | JoinType::RightSemi | JoinType::RightAnti => rhs_stats,
            _ => lhs_stats,
        };
        if matches!(
            join_type,
            JoinType::Semi | JoinType::Anti | JoinType::RightSemi | JoinType::RightAnti
        ) {
            stats.cardinality = ((stats.cardinality as f64 / DEFAULT_SEMI_ANTI_SELECTIVITY) as u64).max(1);
        }
        self.add_aggregate_or_window_relation(root, input_path, stats);
        Ok(())
    }

    /// The relations an expression references (DuckDB's `ExtractBindings`); false if it
    /// cannot be reordered.
    pub fn extract_bindings(&self, expression: &Expression, bindings: &mut BTreeSet<usize>) -> bool {
        if let ExpressionKind::ColumnRef(column) = &expression.kind {
            let mapped = self.relation_mapping.get(&column.binding.table_index);
            if expression.alias == "SUBQUERY" && mapped.is_none() {
                return true;
            }
            if let Some(&relation) = mapped {
                bindings.insert(relation);
            }
        }
        if expression.expression_type == ExpressionType::BoundRef {
            bindings.clear();
            return false;
        }
        let mut can_reorder = true;
        for child in expression.children() {
            if !self.extract_bindings(child, bindings) {
                can_reorder = false;
            }
        }
        can_reorder
    }

    /// Takes the filters and join conditions out of the filter operators, removing
    /// duplicates (DuckDB's `ExtractEdges`).
    pub fn extract_edges(&self, root: &mut LogicalOperator, filter_operators: &[OpPath]) -> Vec<FilterInfo> {
        let mut filters: Vec<FilterInfo> = Vec::new();
        let mut filter_set: Vec<Expression> = Vec::new();
        for path in filter_operators {
            let op = node_mut(root, path);
            if let LogicalKind::ComparisonJoin(join) = &mut op.kind {
                let join_type = join.join_type;
                let conditions = std::mem::take(&mut join.conditions);
                if matches!(join_type, JoinType::Semi | JoinType::Anti) {
                    let comparisons: Vec<Expression> = conditions
                        .into_iter()
                        .map(rustdb_planner::operator::JoinCondition::into_expression)
                        .collect();
                    let mut left_set = RelationSet::default();
                    let mut right_set = RelationSet::default();
                    for comparison in &comparisons {
                        if let ExpressionKind::Comparison(comparison) = &comparison.kind {
                            let mut right_bindings = BTreeSet::new();
                            let mut left_bindings = BTreeSet::new();
                            self.extract_bindings(&comparison.right, &mut right_bindings);
                            self.extract_bindings(&comparison.left, &mut left_bindings);
                            left_set = left_set.union(&RelationSet::new(left_bindings));
                            right_set = right_set.union(&RelationSet::new(right_bindings));
                        }
                    }
                    let full_set = left_set.union(&right_set);
                    let conjunction = Expression::conjunction(ExpressionType::ConjunctionAnd, comparisons);
                    let mut info = FilterInfo::new(conjunction, full_set, join_type);
                    info.left_set = Some(left_set);
                    info.right_set = Some(right_set);
                    filters.push(info);
                } else {
                    for condition in conditions {
                        let comparison = condition.into_expression();
                        if filter_set.iter().any(|existing| existing.equals(&comparison)) {
                            continue;
                        }
                        filter_set.push(comparison.clone());
                        let mut bindings = BTreeSet::new();
                        self.extract_bindings(&comparison, &mut bindings);
                        filters.push(FilterInfo::new(comparison, RelationSet::new(bindings), join_type));
                    }
                }
            } else {
                let mut leftover = Vec::new();
                for expression in std::mem::take(&mut op.expressions) {
                    if filter_set.iter().any(|existing| existing.equals(&expression)) {
                        continue;
                    }
                    filter_set.push(expression.clone());
                    let mut bindings = BTreeSet::new();
                    self.extract_bindings(&expression, &mut bindings);
                    if bindings.is_empty() {
                        leftover.push(expression);
                        continue;
                    }
                    filters.push(FilterInfo::new(
                        expression,
                        RelationSet::new(bindings),
                        JoinType::Inner,
                    ));
                }
                op.expressions = leftover;
            }
        }
        filters
    }
}
