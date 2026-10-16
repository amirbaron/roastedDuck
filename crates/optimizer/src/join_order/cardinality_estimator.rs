//! Join cardinality estimation (DuckDB's `CardinalityEstimator`): the product of the
//! relations' cardinalities divided by the total domains (distinct counts) of the joined
//! column equivalence classes.

use std::collections::{HashMap, HashSet};

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_planner::ColumnBinding;

use super::query_graph_manager::FilterInfo;
use super::relation_set::RelationSet;
use super::relation_statistics::{RelationStats, comparison_type};

/// The selectivity assumed for SEMI and ANTI joins (DuckDB's
/// `CardinalityEstimator::DEFAULT_SEMI_ANTI_SELECTIVITY`).
pub const DEFAULT_SEMI_ANTI_SELECTIVITY: f64 = 5.0;

/// Columns known equal through join filters and their total domain (DuckDB's
/// `RelationsSetToStats`).
#[derive(Clone, Debug)]
struct RelationsSetToStats {
    equivalent_relations: HashSet<ColumnBinding>,
    distinct_count_hll: u64,
    distinct_count_no_hll: u64,
    has_distinct_count_hll: bool,
    /// Indexes into the filter infos.
    filters: Vec<usize>,
}

impl RelationsSetToStats {
    fn new(equivalent_relations: HashSet<ColumnBinding>) -> Self {
        Self {
            equivalent_relations,
            distinct_count_hll: 0,
            distinct_count_no_hll: u64::MAX,
            has_distinct_count_hll: false,
            filters: Vec::new(),
        }
    }
}

/// A filter with the total domain of its equivalence class (DuckDB's
/// `FilterInfoWithTotalDomains`).
#[derive(Clone, Copy, Debug)]
struct FilterWithTotalDomains {
    filter: usize,
    distinct_count_hll: u64,
    distinct_count_no_hll: u64,
    has_distinct_count_hll: bool,
}

impl FilterWithTotalDomains {
    fn total_domain(&self) -> f64 {
        if self.has_distinct_count_hll {
            self.distinct_count_hll as f64
        } else {
            self.distinct_count_no_hll as f64
        }
    }
}

/// A connected part of the set being estimated (DuckDB's `Subgraph2Denominator`).
#[derive(Clone, Debug)]
struct Subgraph {
    relations: RelationSet,
    numerator_relations: RelationSet,
    denom: f64,
}

/// Estimates join cardinalities (DuckDB's `CardinalityEstimator`).
#[derive(Debug, Default)]
pub struct CardinalityEstimator {
    relation_set_stats: Vec<RelationsSetToStats>,
    /// Estimates by set, keyed by the set's text like DuckDB.
    relation_set_cardinality: HashMap<String, f64>,
}

impl CardinalityEstimator {
    /// Whether a filter has no relations on either side (DuckDB's `EmptyFilter`).
    fn empty_filter(filter: &FilterInfo) -> bool {
        filter.left_set.is_none() && filter.right_set.is_none()
    }

    /// DuckDB's `SingleColumnFilter`.
    fn single_column_filter(filter: &FilterInfo) -> bool {
        if filter.left_set.is_some() && filter.right_set.is_some() && filter.set.count() > 1 {
            return false;
        }
        if Self::empty_filter(filter) {
            return false;
        }
        !matches!(filter.join_type, JoinType::Semi | JoinType::Anti)
    }

    /// DuckDB's `AddRelationStats`.
    fn add_relation_stats(&mut self, filter: &FilterInfo) {
        if self
            .relation_set_stats
            .iter()
            .any(|stats| stats.equivalent_relations.contains(&filter.left_binding))
        {
            return;
        }
        self.relation_set_stats
            .push(RelationsSetToStats::new(HashSet::from([filter.left_binding])));
    }

    /// DuckDB's `DetermineMatchingEquivalentSets`.
    fn matching_equivalent_sets(&self, filter: &FilterInfo) -> Vec<usize> {
        self.relation_set_stats
            .iter()
            .enumerate()
            .filter(|(_, stats)| {
                stats.equivalent_relations.contains(&filter.left_binding)
                    || stats.equivalent_relations.contains(&filter.right_binding)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// DuckDB's `AddToEquivalenceSets`.
    fn add_to_equivalence_sets(&mut self, filter_index: usize, filter: &FilterInfo, matching: &[usize]) {
        match matching {
            [first, second, ..] => {
                let moved: Vec<ColumnBinding> = self.relation_set_stats[*second]
                    .equivalent_relations
                    .drain()
                    .collect();
                self.relation_set_stats[*first].equivalent_relations.extend(moved);
                self.relation_set_stats[*first].filters.push(filter_index);
            }
            [single] => {
                let stats = &mut self.relation_set_stats[*single];
                stats.equivalent_relations.insert(filter.left_binding);
                stats.equivalent_relations.insert(filter.right_binding);
                stats.filters.push(filter_index);
            }
            [] => {
                let mut stats =
                    RelationsSetToStats::new(HashSet::from([filter.left_binding, filter.right_binding]));
                stats.filters.push(filter_index);
                self.relation_set_stats.push(stats);
            }
        }
    }

    /// Groups the join filters' columns into equivalence classes (DuckDB's
    /// `InitEquivalentRelations`).
    pub fn init_equivalent_relations(&mut self, filters: &[FilterInfo]) {
        for (index, filter) in filters.iter().enumerate() {
            if Self::single_column_filter(filter) {
                self.add_relation_stats(filter);
                continue;
            }
            if Self::empty_filter(filter) {
                continue;
            }
            let matching = self.matching_equivalent_sets(filter);
            self.add_to_equivalence_sets(index, filter, &matching);
        }
        self.relation_set_stats
            .retain(|stats| !stats.equivalent_relations.is_empty());
    }

    /// Records a base relation's cardinality and distinct counts (DuckDB's
    /// `InitCardinalityEstimatorProps`).
    pub fn init_cardinality_estimator_props(&mut self, set: &RelationSet, stats: &RelationStats) {
        self.relation_set_cardinality
            .insert(set.to_string(), stats.cardinality as f64);
        self.update_total_domains(set, stats);
        self.relation_set_stats.sort_by(|a, b| {
            // Largest total domain first (DuckDB's `SortTdoms`).
            let key = |stats: &RelationsSetToStats| {
                if stats.has_distinct_count_hll {
                    stats.distinct_count_hll
                } else {
                    stats.distinct_count_no_hll
                }
            };
            key(b).cmp(&key(a))
        });
    }

    /// DuckDB's `UpdateTotalDomains`.
    fn update_total_domains(&mut self, set: &RelationSet, stats: &RelationStats) {
        let Some(&relation_id) = set.relations().first() else {
            return;
        };
        for (column, distinct) in stats.column_distinct_count.iter().enumerate() {
            let key = ColumnBinding::new(relation_id as u64, column as u64);
            let Some(domain) = self
                .relation_set_stats
                .iter_mut()
                .find(|domain| domain.equivalent_relations.contains(&key))
            else {
                continue;
            };
            if distinct.from_hll && domain.has_distinct_count_hll {
                domain.distinct_count_hll = domain.distinct_count_hll.max(distinct.distinct_count);
            } else if distinct.from_hll {
                domain.has_distinct_count_hll = true;
                domain.distinct_count_hll = distinct.distinct_count;
            } else {
                domain.distinct_count_no_hll = domain.distinct_count_no_hll.min(distinct.distinct_count);
            }
        }
    }

    /// The product of the base cardinalities of a set (DuckDB's `GetNumerator`).
    fn numerator(&self, set: &RelationSet) -> f64 {
        set.relations()
            .iter()
            .map(|&relation| {
                let cardinality = self
                    .relation_set_cardinality
                    .get(&RelationSet::single(relation).to_string())
                    .copied()
                    .unwrap_or(0.0);
                if cardinality == 0.0 { 1.0 } else { cardinality }
            })
            .product()
    }

    /// Whether an edge touches a subgraph (DuckDB's `EdgeConnects`).
    fn edge_connects(filter: &FilterInfo, subgraph: &Subgraph) -> bool {
        filter
            .left_set
            .as_ref()
            .is_some_and(|left| subgraph.relations.contains_set(left))
            || filter
                .right_set
                .as_ref()
                .is_some_and(|right| subgraph.relations.contains_set(right))
    }

    /// DuckDB's `SubgraphsConnectedByEdge`.
    fn subgraphs_connected_by_edge(filter: &FilterInfo, subgraphs: &[Subgraph]) -> Vec<usize> {
        for outer in 0..subgraphs.len() {
            for inner in outer + 1..subgraphs.len() {
                if Self::edge_connects(filter, &subgraphs[outer])
                    && Self::edge_connects(filter, &subgraphs[inner])
                {
                    return vec![outer, inner];
                }
            }
            if Self::edge_connects(filter, &subgraphs[outer]) {
                return vec![outer];
            }
        }
        Vec::new()
    }

    /// DuckDB's `UpdateNumeratorRelations`.
    fn update_numerator_relations(left: &Subgraph, right: &Subgraph, filter: &FilterInfo) -> RelationSet {
        match filter.join_type {
            JoinType::Semi | JoinType::Anti => {
                if Self::sides_match(left, right, filter) {
                    left.numerator_relations.clone()
                } else {
                    right.numerator_relations.clone()
                }
            }
            _ => left.numerator_relations.union(&right.numerator_relations),
        }
    }

    fn sides_match(left: &Subgraph, right: &Subgraph, filter: &FilterInfo) -> bool {
        filter
            .left_set
            .as_ref()
            .is_some_and(|set| left.relations.contains_set(set))
            && filter
                .right_set
                .as_ref()
                .is_some_and(|set| right.relations.contains_set(set))
    }

    /// DuckDB's `CalculateUpdatedDenom`.
    fn calculate_updated_denom(
        left: &Subgraph,
        right: &Subgraph,
        edge: &FilterWithTotalDomains,
        filter: &FilterInfo,
    ) -> f64 {
        let new_denom = left.denom * right.denom;
        match filter.join_type {
            JoinType::Inner => {
                let comparison = filter.filter.as_ref().and_then(comparison_type);
                let Some(comparison) = comparison else {
                    return new_denom * edge.total_domain();
                };
                let extra_ratio = match comparison {
                    ExpressionType::CompareEqual | ExpressionType::CompareNotDistinctFrom => {
                        edge.total_domain()
                    }
                    ExpressionType::CompareLessthanorequalto
                    | ExpressionType::CompareLessthan
                    | ExpressionType::CompareGreaterthanorequalto
                    | ExpressionType::CompareGreaterthan
                    | ExpressionType::CompareNotequal
                    | ExpressionType::CompareDistinctFrom => edge.total_domain().powf(2.0 / 3.0),
                    _ => 1.0,
                };
                new_denom * extra_ratio
            }
            JoinType::Semi | JoinType::Anti => {
                if Self::sides_match(left, right, filter) {
                    left.denom * DEFAULT_SEMI_ANTI_SELECTIVITY
                } else {
                    right.denom * DEFAULT_SEMI_ANTI_SELECTIVITY
                }
            }
            _ => new_denom,
        }
    }

    /// The relations to multiply and the denominator to divide by (DuckDB's
    /// `GetDenominator`).
    fn denominator(&self, set: &RelationSet, filters: &[FilterInfo]) -> (RelationSet, f64) {
        let mut subgraphs: Vec<Subgraph> = Vec::new();
        let mut unused_edge_tdoms: HashSet<u64> = HashSet::new();
        // Edges in order of decreasing total domain.
        let edges: Vec<FilterWithTotalDomains> = self
            .relation_set_stats
            .iter()
            .flat_map(|stats| {
                stats
                    .filters
                    .iter()
                    .filter(|&&filter| set.contains_set(&filters[filter].set))
                    .map(|&filter| FilterWithTotalDomains {
                        filter,
                        distinct_count_hll: stats.distinct_count_hll,
                        distinct_count_no_hll: stats.distinct_count_no_hll,
                        has_distinct_count_hll: stats.has_distinct_count_hll,
                    })
            })
            .collect();
        for edge in &edges {
            let filter = &filters[edge.filter];
            if subgraphs.len() == 1 && subgraphs[0].relations == *set {
                if edge.has_distinct_count_hll {
                    unused_edge_tdoms.insert(edge.distinct_count_hll);
                }
                continue;
            }
            let (Some(left_set), Some(right_set)) = (filter.left_set.clone(), filter.right_set.clone())
            else {
                continue;
            };
            let connections = Self::subgraphs_connected_by_edge(filter, &subgraphs);
            match connections.as_slice() {
                [] => {
                    let mut left = Subgraph {
                        relations: left_set.clone(),
                        numerator_relations: left_set,
                        denom: 1.0,
                    };
                    let right = Subgraph {
                        relations: right_set.clone(),
                        numerator_relations: right_set,
                        denom: 1.0,
                    };
                    left.numerator_relations = Self::update_numerator_relations(&left, &right, filter);
                    left.relations = filter.set.clone();
                    left.denom = Self::calculate_updated_denom(&left, &right, edge, filter);
                    subgraphs.push(left);
                }
                [index] => {
                    let mut right = Subgraph {
                        relations: right_set.clone(),
                        numerator_relations: right_set,
                        denom: 1.0,
                    };
                    let left = &subgraphs[*index];
                    if left.relations.contains_set(&right.relations) {
                        right.relations = left_set.clone();
                        right.numerator_relations = left_set.clone();
                    }
                    if left.relations.contains_set(&left_set)
                        && filter
                            .right_set
                            .as_ref()
                            .is_some_and(|set| left.relations.contains_set(set))
                    {
                        continue;
                    }
                    let numerator = Self::update_numerator_relations(left, &right, filter);
                    let relations = left.relations.union(&right.relations);
                    let mut updated = left.clone();
                    updated.numerator_relations = numerator;
                    updated.relations = relations;
                    updated.denom = Self::calculate_updated_denom(&updated, &right, edge, filter);
                    subgraphs[*index] = updated;
                }
                [into, delete, ..] => {
                    let to_delete = subgraphs[*delete].clone();
                    let merge = &subgraphs[*into];
                    let mut updated = merge.clone();
                    updated.relations = merge.relations.union(&to_delete.relations);
                    updated.numerator_relations =
                        Self::update_numerator_relations(&updated, &to_delete, filter);
                    updated.denom = Self::calculate_updated_denom(&updated, &to_delete, edge, filter);
                    subgraphs[*into] = updated;
                    subgraphs.remove(*delete);
                }
            }
        }
        let denom_multiplier = 1.0 + unused_edge_tdoms.len() as f64;
        // DuckDB merges further subgraphs (connected by cross products) into a copy of the
        // first one that it then discards, so only the first subgraph counts.
        if let Some(returning) = subgraphs.first_mut()
            && returning.relations.count() != set.count()
        {
            for &relation in set.relations() {
                let single = RelationSet::single(relation);
                if !returning.relations.contains_set(&single) {
                    returning.numerator_relations = returning.numerator_relations.union(&single);
                    returning.relations = returning.relations.union(&single);
                }
            }
        }
        match subgraphs.first() {
            Some(subgraph) if subgraph.denom != 0.0 => (
                subgraph.numerator_relations.clone(),
                subgraph.denom * denom_multiplier,
            ),
            _ => (set.clone(), 1.0),
        }
    }

    /// The estimated cardinality of joining a set of relations (DuckDB's
    /// `EstimateCardinalityWithSet<double>`).
    pub fn estimate_cardinality(&mut self, set: &RelationSet, filters: &[FilterInfo]) -> f64 {
        let key = set.to_string();
        if let Some(&cardinality) = self.relation_set_cardinality.get(&key) {
            return cardinality;
        }
        let (numerator_relations, denominator) = self.denominator(set, filters);
        let result = self.numerator(&numerator_relations) / denominator;
        self.relation_set_cardinality.insert(key, result);
        result
    }

    /// [`CardinalityEstimator::estimate_cardinality`] as a row count (DuckDB's
    /// `EstimateCardinalityWithSet<idx_t>`).
    pub fn estimate_cardinality_rows(&mut self, set: &RelationSet, filters: &[FilterInfo]) -> u64 {
        let estimate = self.estimate_cardinality(set, filters);
        if estimate >= u64::MAX as f64 {
            u64::MAX
        } else {
            estimate as u64
        }
    }
}
