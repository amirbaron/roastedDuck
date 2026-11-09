//! Building the join graph and rebuilding the plan from the chosen join order (DuckDB's
//! `QueryGraphManager`).

use std::collections::{BTreeSet, HashMap};

use roastedduck_ast::{ExpressionType, JoinType};
use roastedduck_common::{Error, Result};
use roastedduck_planner::operator::{JoinCondition, LogicalComparisonJoin};
use roastedduck_planner::{
    ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};

use super::JoinOrderOptimizer;
use super::plan_enumerator::DpJoinNode;
use super::query_graph::QueryGraphEdges;
use super::relation_manager::{RelationManager, node_mut};
use super::relation_set::RelationSet;
use crate::expression_util::{flip_comparison, visit_column_refs};

/// A filter or join condition of the join graph (DuckDB's `FilterInfo`).
#[derive(Clone, Debug)]
pub struct FilterInfo {
    /// The filter; taken out when it becomes part of the rebuilt plan.
    pub filter: Option<Expression>,
    /// The relations the filter references.
    pub set: RelationSet,
    pub join_type: JoinType,
    pub left_set: Option<RelationSet>,
    pub right_set: Option<RelationSet>,
    /// The (relation, column) of the left and right sides, for the cardinality estimator.
    pub left_binding: ColumnBinding,
    pub right_binding: ColumnBinding,
}

impl FilterInfo {
    pub fn new(filter: Expression, set: RelationSet, join_type: JoinType) -> Self {
        Self {
            filter: Some(filter),
            set,
            join_type,
            left_set: None,
            right_set: None,
            left_binding: ColumnBinding::new(u64::MAX, u64::MAX),
            right_binding: ColumnBinding::new(u64::MAX, u64::MAX),
        }
    }
}

/// The relations, filters and edges of one join tree (DuckDB's `QueryGraphManager`).
#[derive(Debug, Default)]
pub struct QueryGraphManager {
    pub relation_manager: RelationManager,
    pub filters_and_bindings: Vec<FilterInfo>,
    pub query_graph: QueryGraphEdges,
}

/// Adds `expression` to the filter on top of `node`, creating one if needed (DuckDB's
/// `PushFilter`).
fn push_filter(node: LogicalOperator, expression: Expression) -> LogicalOperator {
    let mut node = if matches!(node.kind, LogicalKind::Filter { .. }) {
        node
    } else {
        LogicalOperator::new(LogicalKind::Filter {
            projection_map: Vec::new(),
        })
        .with_children(vec![node])
    };
    node.expressions.push(expression);
    node
}

/// A comparison as a join condition, with its sides swapped if `invert` (DuckDB's
/// `MaybeInvertConditions`).
fn maybe_invert_condition(condition: Expression, invert: bool) -> Result<JoinCondition> {
    let comparison_type = condition.expression_type;
    let ExpressionKind::Comparison(comparison) = condition.kind else {
        return Err(Error::internal("join condition is not a comparison"));
    };
    let comparison = *comparison;
    Ok(if invert {
        JoinCondition {
            left: comparison.right,
            right: comparison.left,
            comparison: flip_comparison(comparison_type),
        }
    } else {
        JoinCondition {
            left: comparison.left,
            right: comparison.right,
            comparison: comparison_type,
        }
    })
}

impl QueryGraphManager {
    /// Extracts the relations and, if the tree can be reordered, the edges (DuckDB's
    /// `QueryGraphManager::Build`).
    pub fn build(&mut self, optimizer: &JoinOrderOptimizer<'_>, root: &mut LogicalOperator) -> Result<bool> {
        let mut filter_operators = Vec::new();
        let can_reorder =
            self.relation_manager
                .extract_join_relations(optimizer, root, &[], &mut filter_operators)?;
        if self.relation_manager.relations.len() <= 1 || !can_reorder {
            return Ok(false);
        }
        self.filters_and_bindings = self.relation_manager.extract_edges(root, &filter_operators);
        self.create_hyper_graph_edges();
        Ok(true)
    }

    /// The (relation, column) of the last column reference in an expression (DuckDB's
    /// `GetColumnBinding`).
    fn column_binding(&self, expression: &Expression, binding: &mut ColumnBinding) {
        visit_column_refs(expression, &mut |column| {
            let relation = self
                .relation_manager
                .relation_mapping
                .get(&column.binding.table_index)
                .copied()
                .unwrap_or(0);
            *binding = ColumnBinding::new(relation as u64, column.binding.column_index);
        });
    }

    fn add_edges(&mut self, index: usize, left_bindings: &BTreeSet<usize>, right_bindings: &BTreeSet<usize>) {
        let (Some(left_set), Some(right_set)) = (
            self.filters_and_bindings[index].left_set.clone(),
            self.filters_and_bindings[index].right_set.clone(),
        ) else {
            return;
        };
        if left_set != right_set && left_bindings.is_disjoint(right_bindings) {
            self.query_graph.create_edge(&left_set, &right_set, Some(index));
            self.query_graph.create_edge(&right_set, &left_set, Some(index));
        }
    }

    /// Turns the join filters into hypergraph edges (DuckDB's `CreateHyperGraphEdges`).
    fn create_hyper_graph_edges(&mut self) {
        for index in 0..self.filters_and_bindings.len() {
            let Some(filter) = self.filters_and_bindings[index].filter.clone() else {
                continue;
            };
            match &filter.kind {
                ExpressionKind::Comparison(comparison) => {
                    let mut left_bindings = BTreeSet::new();
                    let mut right_bindings = BTreeSet::new();
                    self.relation_manager
                        .extract_bindings(&comparison.left, &mut left_bindings);
                    self.relation_manager
                        .extract_bindings(&comparison.right, &mut right_bindings);
                    let mut left_binding = self.filters_and_bindings[index].left_binding;
                    let mut right_binding = self.filters_and_bindings[index].right_binding;
                    self.column_binding(&comparison.left, &mut left_binding);
                    self.column_binding(&comparison.right, &mut right_binding);
                    let info = &mut self.filters_and_bindings[index];
                    info.left_binding = left_binding;
                    info.right_binding = right_binding;
                    if left_bindings.is_empty() || right_bindings.is_empty() {
                        continue;
                    }
                    if info.left_set.is_none() {
                        info.left_set = Some(RelationSet::new(left_bindings.iter().copied()));
                    }
                    if info.right_set.is_none() {
                        info.right_set = Some(RelationSet::new(right_bindings.iter().copied()));
                    }
                    self.add_edges(index, &left_bindings, &right_bindings);
                }
                ExpressionKind::Conjunction(conjunction) => {
                    let join_type = self.filters_and_bindings[index].join_type;
                    if filter.expression_type == ExpressionType::ConjunctionOr
                        || matches!(join_type, JoinType::Inner | JoinType::Invalid)
                    {
                        continue;
                    }
                    let mut left_bindings = BTreeSet::new();
                    let mut right_bindings = BTreeSet::new();
                    for child in &conjunction.children {
                        let ExpressionKind::Comparison(comparison) = &child.kind else {
                            continue;
                        };
                        self.relation_manager
                            .extract_bindings(&comparison.left, &mut left_bindings);
                        self.relation_manager
                            .extract_bindings(&comparison.right, &mut right_bindings);
                        let invalid = ColumnBinding::new(u64::MAX, u64::MAX);
                        if self.filters_and_bindings[index].left_binding == invalid {
                            let mut binding = invalid;
                            self.column_binding(&comparison.left, &mut binding);
                            self.filters_and_bindings[index].left_binding = binding;
                        }
                        if self.filters_and_bindings[index].right_binding == invalid {
                            let mut binding = invalid;
                            self.column_binding(&comparison.right, &mut binding);
                            self.filters_and_bindings[index].right_binding = binding;
                        }
                    }
                    if !left_bindings.is_empty() && !right_bindings.is_empty() {
                        self.add_edges(index, &left_bindings, &right_bindings);
                    }
                }
                _ => {}
            }
        }
    }

    /// Rebuilds the plan with the joins of the chosen plan (DuckDB's
    /// `QueryGraphManager::Reconstruct`).
    pub fn reconstruct(
        &mut self,
        mut plan: LogicalOperator,
        plans: &HashMap<RelationSet, DpJoinNode>,
    ) -> Result<LogicalOperator> {
        let root_is_join = plan.children.len() > 1;
        let total = RelationSet::new(0..self.relation_manager.relations.len());
        let mut extracted: Vec<Option<LogicalOperator>> =
            Vec::with_capacity(self.relation_manager.relations.len());
        for relation in &self.relation_manager.relations {
            let op = node_mut(&mut plan, &relation.path);
            extracted.push(Some(std::mem::replace(
                op,
                LogicalOperator::new(LogicalKind::DummyScan {
                    table_index: u64::MAX,
                }),
            )));
        }
        let (_, mut join_tree) = self.generate_joins(&mut extracted, &total, plans)?;
        for info in &mut self.filters_and_bindings {
            if let Some(filter) = info.filter.take() {
                join_tree = push_filter(join_tree, filter);
            }
        }
        if root_is_join {
            return Ok(join_tree);
        }
        // Replace the first join below the chain of single-child operators.
        let mut path = Vec::new();
        loop {
            let op = node_mut(&mut plan, &path);
            if matches!(
                op.operator_type(),
                LogicalOperatorType::LogicalCrossProduct
                    | LogicalOperatorType::LogicalComparisonJoin
                    | LogicalOperatorType::LogicalAsofJoin
            ) {
                break;
            }
            if op.children.len() != 1 {
                return Err(Error::internal("join order optimizer lost the join"));
            }
            path.push(0);
        }
        *node_mut(&mut plan, &path) = join_tree;
        Ok(plan)
    }

    /// The plan of a set of relations from the DP table (DuckDB's `GenerateJoins`).
    fn generate_joins(
        &mut self,
        extracted: &mut [Option<LogicalOperator>],
        set: &RelationSet,
        plans: &HashMap<RelationSet, DpJoinNode>,
    ) -> Result<(RelationSet, LogicalOperator)> {
        let node = plans
            .get(set)
            .ok_or_else(|| Error::internal("Join Order Optimizer Error: No full plan was created"))?;
        let mut left_node: Option<RelationSet> = None;
        let mut right_node: Option<RelationSet> = None;
        let (result_relation, mut result_operator) = if node.is_leaf {
            let relation = set.relations().first().copied().unwrap_or(0);
            let op = extracted
                .get_mut(relation)
                .and_then(Option::take)
                .ok_or_else(|| Error::internal("join relation extracted twice"))?;
            (node.set.clone(), op)
        } else {
            let (left_set, left) = self.generate_joins(extracted, &node.left_set, plans)?;
            let (right_set, right) = self.generate_joins(extracted, &node.right_set, plans)?;
            let filters = node
                .info
                .as_ref()
                .map(|info| info.filters.clone())
                .unwrap_or_default();
            let op = if filters.is_empty() {
                let cardinality = left
                    .estimated_cardinality
                    .unwrap_or(0)
                    .wrapping_mul(right.estimated_cardinality.unwrap_or(0));
                let mut cross_product = LogicalOperator::cross_product(left, right);
                cross_product.estimated_cardinality = Some(cardinality);
                cross_product
            } else {
                let chosen = filters
                    .iter()
                    .copied()
                    .find(|&filter| self.filters_and_bindings[filter].join_type == JoinType::Inner)
                    .unwrap_or(filters[0]);
                let join_type = self.filters_and_bindings[chosen].join_type;
                let mut join =
                    LogicalComparisonJoin::new(join_type, LogicalOperatorType::LogicalComparisonJoin);
                let mut children = vec![left, right];
                for &filter_index in &filters {
                    let info = &mut self.filters_and_bindings[filter_index];
                    let condition = info
                        .filter
                        .take()
                        .ok_or_else(|| Error::internal("join filter used twice"))?;
                    let mut invert = !info
                        .left_set
                        .as_ref()
                        .is_some_and(|filter_left| left_set.contains_set(filter_left));
                    if invert && matches!(info.join_type, JoinType::Semi | JoinType::Anti) {
                        children.swap(0, 1);
                        invert = false;
                    }
                    match condition.kind {
                        ExpressionKind::Comparison(_) => {
                            join.conditions.push(maybe_invert_condition(condition, invert)?);
                        }
                        ExpressionKind::Conjunction(conjunction) => {
                            for child in conjunction.children {
                                join.conditions.push(maybe_invert_condition(child, invert)?);
                            }
                        }
                        _ => return Err(Error::internal("join filter is not a comparison")),
                    }
                }
                LogicalOperator::new(LogicalKind::ComparisonJoin(Box::new(join))).with_children(children)
            };
            let union = left_set.union(&right_set);
            left_node = Some(left_set);
            right_node = Some(right_set);
            (union, op)
        };
        result_operator.estimated_cardinality = Some(node.cardinality);
        // Filters on relations of this node that no join used go here.
        for index in 0..self.filters_and_bindings.len() {
            let info = &self.filters_and_bindings[index];
            if info.filter.is_none() || info.set.is_empty() || !result_relation.contains_set(&info.set) {
                continue;
            }
            let (filter_left, filter_right) = (info.left_set.clone(), info.right_set.clone());
            let Some(filter) = self.filters_and_bindings[index].filter.take() else {
                continue;
            };
            let (Some(left_node), Some(right_node), Some(filter_left), Some(filter_right)) =
                (&left_node, &right_node, filter_left, filter_right)
            else {
                result_operator = push_filter(result_operator, filter);
                continue;
            };
            let invert = if left_node.contains_set(&filter_left) && right_node.contains_set(&filter_right) {
                false
            } else if right_node.contains_set(&filter_left) && left_node.contains_set(&filter_right) {
                true
            } else {
                result_operator = push_filter(result_operator, filter);
                continue;
            };
            let condition = maybe_invert_condition(filter, invert)?;
            let in_filter = matches!(result_operator.kind, LogicalKind::Filter { .. });
            let join_node = if in_filter {
                &mut result_operator.children[0]
            } else {
                &mut result_operator
            };
            if matches!(join_node.kind, LogicalKind::CrossProduct) {
                let mut join =
                    LogicalComparisonJoin::new(JoinType::Inner, LogicalOperatorType::LogicalComparisonJoin);
                join.conditions.push(condition);
                let children = std::mem::take(&mut join_node.children);
                *join_node =
                    LogicalOperator::new(LogicalKind::ComparisonJoin(Box::new(join))).with_children(children);
            } else if let LogicalKind::ComparisonJoin(join) = &mut join_node.kind {
                join.conditions.push(condition);
            }
        }
        Ok((result_relation, result_operator))
    }
}
