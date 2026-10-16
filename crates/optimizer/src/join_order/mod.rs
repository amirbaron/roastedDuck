//! The join order optimizer (DuckDB's `optimizer/join_order`): the inputs of trees of
//! inner joins and cross products are reordered by the `DPhyp` algorithm with a cost model
//! based on estimated cardinalities, and filters over several inputs become join
//! conditions. It also sets the estimated cardinalities of the plan's operators.

mod cardinality_estimator;
mod plan_enumerator;
mod query_graph;
mod query_graph_manager;
mod relation_manager;
mod relation_set;
mod relation_statistics;

use std::collections::{HashMap, HashSet};

use rustdb_common::Result;
use rustdb_planner::{ClientContext, LogicalKind, LogicalOperator};

use plan_enumerator::PlanEnumerator;
use query_graph_manager::QueryGraphManager;
pub use relation_statistics::RelationStats;

/// Optimizes the join order of a plan (DuckDB's `JoinOrderOptimizer`).
#[derive(Debug)]
pub struct JoinOrderOptimizer<'a> {
    pub context: &'a ClientContext,
    /// `debug_force_no_cross_product`.
    pub force_no_cross_product: bool,
    /// `max_expression_depth`: deeper plans are left alone.
    pub max_expression_depth: u64,
    depth: u64,
    /// The stats of materialized CTEs by table index, for their references.
    pub(crate) materialized_cte_stats: HashMap<u64, RelationStats>,
    /// The stats of the delim scans of the delim join being optimized.
    pub(crate) delim_scan_stats: Option<RelationStats>,
    /// The recursive CTEs being optimized.
    pub(crate) recursive_cte_indexes: HashSet<u64>,
}

impl<'a> JoinOrderOptimizer<'a> {
    pub fn new(context: &'a ClientContext, force_no_cross_product: bool, max_expression_depth: u64) -> Self {
        Self {
            context,
            force_no_cross_product,
            max_expression_depth,
            depth: 1,
            materialized_cte_stats: HashMap::new(),
            delim_scan_stats: None,
            recursive_cte_indexes: HashSet::new(),
        }
    }

    /// An optimizer for a subtree (DuckDB's `CreateChildOptimizer`).
    pub(crate) fn create_child_optimizer(&self) -> Self {
        Self {
            context: self.context,
            force_no_cross_product: self.force_no_cross_product,
            max_expression_depth: self.max_expression_depth,
            depth: self.depth + 1,
            materialized_cte_stats: self.materialized_cte_stats.clone(),
            delim_scan_stats: self.delim_scan_stats.clone(),
            recursive_cte_indexes: self.recursive_cte_indexes.clone(),
        }
    }

    /// Reorders the joins of `plan`; with `stats`, also returns the stats of its result
    /// (DuckDB's `JoinOrderOptimizer::Optimize`).
    pub fn optimize(
        &self,
        mut plan: LogicalOperator,
        stats: Option<&mut RelationStats>,
    ) -> Result<LogicalOperator> {
        if self.depth > self.max_expression_depth {
            return Ok(plan);
        }
        let mut manager = QueryGraphManager::default();
        let reorderable = manager.build(self, &mut plan)?;
        let relation_stats = manager.relation_manager.relation_stats();
        let mut new_plan = if reorderable {
            let filters = std::mem::take(&mut manager.filters_and_bindings);
            let relation_count = manager.relation_manager.relations.len();
            let no_cross_product = manager.relation_manager.no_cross_product_relations.clone();
            let plans = {
                let mut enumerator = PlanEnumerator::new(
                    &mut manager.query_graph,
                    &filters,
                    relation_count,
                    &no_cross_product,
                    self.force_no_cross_product,
                );
                enumerator.init_leaf_plans(&relation_stats);
                enumerator.solve_join_order()?;
                std::mem::take(&mut enumerator.plans)
            };
            manager.filters_and_bindings = filters;
            manager.reconstruct(plan, &plans)?
        } else {
            if let [single] = relation_stats.as_slice() {
                plan.estimated_cardinality = Some(single.cardinality);
            }
            plan
        };
        if let Some(stats) = stats {
            let cardinality = new_plan.estimate_cardinality();
            let mut new_stats = relation_statistics::combine_stats_of_reorderable_operator(&relation_stats);
            new_stats.cardinality = cardinality;
            *stats = new_stats;
        } else {
            new_plan.estimate_cardinality();
        }
        if matches!(new_plan.kind, LogicalKind::Explain { .. }) {
            new_plan.estimated_cardinality = Some(3);
        }
        Ok(new_plan)
    }
}
