//! Join order enumeration (DuckDB's `PlanEnumerator`): the `DPhyp` dynamic program of
//! "Dynamic Programming Strikes Back" (Moerkotte and Neumann) over connected subgraphs,
//! with a greedy fallback (GOO) for large or slow searches.

use std::collections::{BTreeSet, HashMap};

use roastedduck_ast::JoinType;
use roastedduck_common::{Error, Result};

use super::cardinality_estimator::CardinalityEstimator;
use super::query_graph::{NeighborInfo, QueryGraphEdges};
use super::query_graph_manager::FilterInfo;
use super::relation_set::RelationSet;
use super::relation_statistics::RelationStats;

/// Above this many relations the greedy algorithm is used directly (DuckDB's
/// `THRESHOLD_TO_SWAP_TO_APPROXIMATE`).
const THRESHOLD_TO_SWAP_TO_APPROXIMATE: usize = 12;
/// The exact search gives up after this many pairs.
const MAX_PAIRS: usize = 10_000;

/// A plan for a set of relations (DuckDB's `DPJoinNode`).
#[derive(Clone, Debug)]
pub struct DpJoinNode {
    pub set: RelationSet,
    /// How the two sides are connected; `None` for leaves.
    pub info: Option<NeighborInfo>,
    pub is_leaf: bool,
    pub left_set: RelationSet,
    pub right_set: RelationSet,
    pub cost: f64,
    pub cardinality: u64,
}

impl DpJoinNode {
    fn leaf(set: RelationSet) -> Self {
        Self {
            left_set: set.clone(),
            right_set: set.clone(),
            set,
            info: None,
            is_leaf: true,
            cost: 0.0,
            cardinality: 0,
        }
    }
}

/// What the enumerator needs of the query graph manager.
#[derive(Debug)]
pub struct PlanEnumerator<'a> {
    pub query_graph: &'a mut QueryGraphEdges,
    pub filters: &'a [FilterInfo],
    pub relation_count: usize,
    /// Relations that may not take part in forced cross products (the right sides of
    /// SEMI and ANTI joins).
    pub no_cross_product_relations: &'a BTreeSet<usize>,
    pub force_no_cross_product: bool,
    pub estimator: CardinalityEstimator,
    pub plans: HashMap<RelationSet, DpJoinNode>,
    pairs: usize,
}

impl<'a> PlanEnumerator<'a> {
    pub fn new(
        query_graph: &'a mut QueryGraphEdges,
        filters: &'a [FilterInfo],
        relation_count: usize,
        no_cross_product_relations: &'a BTreeSet<usize>,
        force_no_cross_product: bool,
    ) -> Self {
        Self {
            query_graph,
            filters,
            relation_count,
            no_cross_product_relations,
            force_no_cross_product,
            estimator: CardinalityEstimator::default(),
            plans: HashMap::new(),
            pairs: 0,
        }
    }

    /// The single-relation plans (DuckDB's `InitLeafPlans`).
    pub fn init_leaf_plans(&mut self, relation_stats: &[RelationStats]) {
        self.estimator.init_equivalent_relations(self.filters);
        for (relation, stats) in relation_stats.iter().enumerate() {
            let set = RelationSet::single(relation);
            let mut node = DpJoinNode::leaf(set.clone());
            node.cardinality = stats.cardinality;
            self.plans.insert(set.clone(), node);
            self.estimator.init_cardinality_estimator_props(&set, stats);
        }
    }

    /// DuckDB's `CostModel::ComputeCost`: the join's cardinality plus its inputs' costs.
    fn compute_cost(&mut self, left: &DpJoinNode, right: &DpJoinNode) -> f64 {
        let combination = left.set.union(&right.set);
        let join_cardinality = self.estimator.estimate_cardinality(&combination, self.filters);
        join_cardinality + left.cost + right.cost
    }

    /// DuckDB's `CreateJoinTree`: the connection with a join filter is preferred over a
    /// cross product.
    fn create_join_tree(
        &mut self,
        set: RelationSet,
        connections: &[NeighborInfo],
        left: &DpJoinNode,
        right: &DpJoinNode,
    ) -> Result<DpJoinNode> {
        let mut best_connection = connections
            .last()
            .ok_or_else(|| Error::internal("join without a connection"))?;
        'search: for connection in connections {
            for &filter in &connection.filters {
                if self.filters[filter].join_type != JoinType::Invalid {
                    best_connection = connection;
                    break 'search;
                }
            }
        }
        let cost = self.compute_cost(left, right);
        let cardinality = self.estimator.estimate_cardinality_rows(&set, self.filters);
        Ok(DpJoinNode {
            set,
            info: Some(best_connection.clone()),
            is_leaf: false,
            left_set: left.set.clone(),
            right_set: right.set.clone(),
            cost,
            cardinality,
        })
    }

    /// Records the pair's plan if it is the cheapest for its set (DuckDB's `EmitPair`).
    fn emit_pair(
        &mut self,
        left: &RelationSet,
        right: &RelationSet,
        connections: &[NeighborInfo],
    ) -> Result<DpJoinNode> {
        let (Some(left_plan), Some(right_plan)) =
            (self.plans.get(left).cloned(), self.plans.get(right).cloned())
        else {
            return Err(Error::internal(
                "No left or right plan: internal error in join order optimizer",
            ));
        };
        let new_set = left.union(right);
        let new_plan = self.create_join_tree(new_set.clone(), connections, &left_plan, &right_plan)?;
        match self.plans.get(&new_set) {
            Some(existing) if new_plan.cost >= existing.cost => Ok(existing.clone()),
            _ => {
                self.plans.insert(new_set, new_plan.clone());
                Ok(new_plan)
            }
        }
    }

    /// DuckDB's `TryEmitPair`: false once the exact search has gone on too long.
    fn try_emit_pair(
        &mut self,
        left: &RelationSet,
        right: &RelationSet,
        connections: &[NeighborInfo],
    ) -> Result<bool> {
        self.pairs += 1;
        if self.pairs >= MAX_PAIRS {
            return Ok(false);
        }
        self.emit_pair(left, right, connections)?;
        Ok(true)
    }

    fn connections(&self, node: &RelationSet, other: &RelationSet) -> Vec<NeighborInfo> {
        self.query_graph
            .connections(node, other)
            .into_iter()
            .cloned()
            .collect()
    }

    /// DuckDB's `EmitCSG`.
    fn emit_csg(&mut self, node: &RelationSet) -> Result<bool> {
        if node.count() == self.relation_count {
            return Ok(true);
        }
        let first = node.relations().first().copied().unwrap_or(0);
        let mut exclusion_set: BTreeSet<usize> = (0..first).collect();
        exclusion_set.extend(node.relations().iter().copied());
        let mut neighbors = self.query_graph.neighbors(node, &exclusion_set);
        if neighbors.is_empty() {
            return Ok(true);
        }
        neighbors.sort_unstable_by(|a, b| b.cmp(a));
        let mut new_exclusion_set = exclusion_set;
        new_exclusion_set.extend(neighbors.iter().copied());
        for neighbor in neighbors {
            let neighbor_relation = RelationSet::single(neighbor);
            let connections = self.connections(node, &neighbor_relation);
            if !connections.is_empty() && !self.try_emit_pair(node, &neighbor_relation, &connections)? {
                return Ok(false);
            }
            if !self.enumerate_cmp_recursive(node, &neighbor_relation, &new_exclusion_set)? {
                return Ok(false);
            }
            new_exclusion_set.remove(&neighbor);
        }
        Ok(true)
    }

    /// Every non-empty subset of `neighbors`, by increasing size (DuckDB's
    /// `GetAllNeighborSets`).
    fn all_neighbor_sets(mut neighbors: Vec<usize>) -> Vec<RelationSet> {
        neighbors.sort_unstable();
        let mut result: Vec<Vec<usize>> = Vec::new();
        let mut added: Vec<Vec<usize>> = neighbors.iter().map(|&neighbor| vec![neighbor]).collect();
        result.extend(added.iter().cloned());
        loop {
            let mut next = Vec::new();
            for set in &added {
                let max = set.iter().copied().max().unwrap_or(0);
                for &neighbor in &neighbors {
                    if max >= neighbor || set.contains(&neighbor) {
                        continue;
                    }
                    let mut new_set = set.clone();
                    new_set.push(neighbor);
                    next.push(new_set);
                }
            }
            if next.is_empty() {
                break;
            }
            result.extend(next.iter().cloned());
            added = next;
        }
        result.into_iter().map(RelationSet::new).collect()
    }

    /// DuckDB's `EnumerateCmpRecursive`.
    fn enumerate_cmp_recursive(
        &mut self,
        left: &RelationSet,
        right: &RelationSet,
        exclusion_set: &BTreeSet<usize>,
    ) -> Result<bool> {
        let neighbors = self.query_graph.neighbors(right, exclusion_set);
        if neighbors.is_empty() {
            return Ok(true);
        }
        let mut union_sets = Vec::new();
        for neighbor in Self::all_neighbor_sets(neighbors.clone()) {
            let combined_set = right.union(&neighbor);
            if self.plans.contains_key(&combined_set) {
                let connections = self.connections(left, &combined_set);
                if !connections.is_empty() && !self.try_emit_pair(left, &combined_set, &connections)? {
                    return Ok(false);
                }
            }
            union_sets.push(combined_set);
        }
        let mut new_exclusion_set = exclusion_set.clone();
        new_exclusion_set.extend(neighbors);
        for set in &union_sets {
            if !self.enumerate_cmp_recursive(left, set, &new_exclusion_set)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// DuckDB's `EnumerateCSGRecursive`.
    fn enumerate_csg_recursive(
        &mut self,
        node: &RelationSet,
        exclusion_set: &BTreeSet<usize>,
    ) -> Result<bool> {
        let neighbors = self.query_graph.neighbors(node, exclusion_set);
        if neighbors.is_empty() {
            return Ok(true);
        }
        let mut union_sets = Vec::new();
        for neighbor in Self::all_neighbor_sets(neighbors.clone()) {
            let new_set = node.union(&neighbor);
            if self.plans.contains_key(&new_set) && !self.emit_csg(&new_set)? {
                return Ok(false);
            }
            union_sets.push(new_set);
        }
        let mut new_exclusion_set = exclusion_set.clone();
        new_exclusion_set.extend(neighbors);
        for set in &union_sets {
            if !self.enumerate_csg_recursive(set, &new_exclusion_set)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// DuckDB's `SolveJoinOrderExactly`.
    fn solve_join_order_exactly(&mut self) -> Result<bool> {
        for i in (1..=self.relation_count).rev() {
            let start_node = RelationSet::single(i - 1);
            if !self.emit_csg(&start_node)? {
                return Ok(false);
            }
            let exclusion_set: BTreeSet<usize> = (0..i).collect();
            if !self.enumerate_csg_recursive(&start_node, &exclusion_set)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Adds a cross product edge between two sets (DuckDB's
    /// `QueryGraphManager::CreateQueryGraphCrossProduct`).
    fn create_cross_product(&mut self, left: &RelationSet, right: &RelationSet) {
        self.query_graph.create_edge(left, right, None);
        self.query_graph.create_edge(right, left, None);
    }

    /// Greedy operator ordering (DuckDB's `SolveJoinOrderApproximately`).
    fn solve_join_order_approximately(&mut self) -> Result<()> {
        let mut join_relations: Vec<RelationSet> =
            (0..self.relation_count).map(RelationSet::single).collect();
        while join_relations.len() > 1 {
            let mut best: Option<(DpJoinNode, usize, usize)> = None;
            for i in 0..join_relations.len() {
                for j in i + 1..join_relations.len() {
                    let (left, right) = (join_relations[i].clone(), join_relations[j].clone());
                    let connections = self.connections(&left, &right);
                    if connections.is_empty() {
                        continue;
                    }
                    let node = self.emit_pair(&left, &right, &connections)?;
                    if best
                        .as_ref()
                        .is_none_or(|(best_node, _, _)| node.cost < best_node.cost)
                    {
                        let node = self.emit_pair(&left, &right, &connections)?;
                        best = Some((node, i, j));
                    }
                }
            }
            let (best_left, best_right) = if let Some((_, left, right)) = best {
                (left, right)
            } else {
                // No connection: add a cross product between the two cheapest plans.
                let mut smallest: [Option<(f64, usize)>; 2] = [None, None];
                for (i, slot) in smallest.iter_mut().enumerate() {
                    let cost = self.plans.get(&join_relations[i]).map_or(0.0, |plan| plan.cost);
                    *slot = Some((cost, i));
                }
                for (i, relation) in join_relations.iter().enumerate().skip(2) {
                    let cost = self.plans.get(relation).map_or(0.0, |plan| plan.cost);
                    for slot in &mut smallest {
                        if slot.is_none_or(|(smallest_cost, _)| smallest_cost > cost) {
                            *slot = Some((cost, i));
                            break;
                        }
                    }
                }
                let (Some((_, first)), Some((_, second))) = (smallest[0], smallest[1]) else {
                    return Err(Error::internal("Internal error in join order optimizer"));
                };
                let (left, right) = (join_relations[first].clone(), join_relations[second].clone());
                self.create_cross_product(&left, &right);
                let connections = self.connections(&left, &right);
                self.emit_pair(&left, &right, &connections)?;
                (first.min(second), first.max(second))
            };
            let new_set = join_relations[best_left].union(&join_relations[best_right]);
            join_relations.remove(best_right);
            join_relations.remove(best_left);
            join_relations.push(new_set);
        }
        Ok(())
    }

    /// Adds cross product edges between all pairs of relations (DuckDB's
    /// `GenerateCrossProducts`).
    fn generate_cross_products(&mut self) {
        for i in 0..self.relation_count {
            let left = RelationSet::single(i);
            for j in 0..self.relation_count {
                let allowed = !self.no_cross_product_relations.contains(&i)
                    && !self.no_cross_product_relations.contains(&j);
                if i != j && allowed {
                    let right = RelationSet::single(j);
                    self.create_cross_product(&left, &right);
                }
            }
        }
    }

    /// Finds the cheapest join order (DuckDB's `SolveJoinOrder`).
    pub fn solve_join_order(&mut self) -> Result<()> {
        if self.relation_count >= THRESHOLD_TO_SWAP_TO_APPROXIMATE || !self.solve_join_order_exactly()? {
            self.solve_join_order_approximately()?;
        }
        let total = RelationSet::new(0..self.relation_count);
        if self.plans.contains_key(&total) {
            return Ok(());
        }
        if self.force_no_cross_product {
            return Err(Error::invalid_input(
                "Query requires a cross-product, but 'force_no_cross_product' PRAGMA is enabled",
            ));
        }
        self.generate_cross_products();
        self.solve_join_order()
    }
}
