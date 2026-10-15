//! Query nodes: SELECT, set operations, recursive CTEs and the CTEs attached to any node
//! (DuckDB's `planner/binder/query_node`). Since DuckDB 1.4 binding a node also plans it.

mod bind_select_node;
mod bind_setop_node;
mod modifiers;
mod plan_subquery;

use std::sync::{Arc, Mutex};

use rustdb_ast::{CommonTableExpressionInfo, CteMaterialize, QueryNode, QueryNodeKind, SampleOptions};
use rustdb_common::{Error, Result};
use rustdb_types::LogicalType;

use super::binding::{Binding, BindingAlias, BindingKind, CteBindState};
use super::{BindScope, BoundStatement};
use crate::expression::{BoundOrderModifier, Expression};
use crate::expression_binder::SelectBindState;
use crate::generated::enums::DistinctType;
use crate::operator::{BoundLimitNode, GroupingSet, LogicalKind, LogicalOperator};

pub use bind_select_node::bind_select_node;
pub use plan_subquery::plan_subqueries;

/// GROUP BY terms and grouping sets (DuckDB's `BoundGroupByNode`).
#[derive(Clone, Debug, Default)]
pub struct BoundGroupByNode {
    pub group_expressions: Vec<Expression>,
    pub grouping_sets: Vec<GroupingSet>,
}

/// The UNNEST expressions of one nesting level (DuckDB's `BoundUnnestNode`).
#[derive(Clone, Debug, Default)]
pub struct BoundUnnestNode {
    pub index: u64,
    pub expressions: Vec<Expression>,
}

/// A bound DISTINCT / ORDER BY / LIMIT (DuckDB's `BoundResultModifier`).
#[derive(Clone, Debug)]
pub enum BoundResultModifier {
    Distinct {
        distinct_type: DistinctType,
        targets: Vec<Expression>,
    },
    Order(BoundOrderModifier),
    Limit { limit: BoundLimitNode, offset: BoundLimitNode },
}

/// A bound SELECT node before planning (DuckDB's `BoundSelectNode`).
#[derive(Clone, Debug, Default)]
pub struct BoundSelectNode {
    pub bind_state: SelectBindState,
    pub select_list: Vec<Expression>,
    pub where_clause: Option<Expression>,
    pub groups: BoundGroupByNode,
    pub having: Option<Expression>,
    pub qualify: Option<Expression>,
    pub sample_options: Option<Box<SampleOptions>>,
    pub modifiers: Vec<BoundResultModifier>,
    pub names: Vec<String>,
    pub types: Vec<LogicalType>,
    /// The number of result columns.
    pub column_count: usize,
    /// The number of select-list entries bound so far.
    pub bound_column_count: usize,
    pub projection_index: u64,
    pub group_index: u64,
    pub aggregate_index: u64,
    pub groupings_index: u64,
    pub window_index: u64,
    pub prune_index: u64,
    pub need_prune: bool,
    pub aggregates: Vec<Expression>,
    pub grouping_functions: Vec<Vec<u64>>,
    pub windows: Vec<Expression>,
    /// UNNESTs by nesting level.
    pub unnests: std::collections::BTreeMap<u64, BoundUnnestNode>,
}

/// A CTE being bound with the node it is attached to (DuckDB's `BoundCTEData`).
#[derive(Debug)]
struct BoundCteData {
    name: String,
    materialized: CteMaterialize,
    setop_index: u64,
    bind_state: Arc<Mutex<CteBindState>>,
}

/// Binds and plans a query node with its CTEs (DuckDB's `Binder::BindNode(QueryNode &)`).
pub fn bind_node(scope: &mut BindScope<'_>, node: &mut QueryNode) -> Result<BoundStatement> {
    let ctes: Vec<(String, CommonTableExpressionInfo)> = node
        .cte_map
        .map
        .iter()
        .map(|(name, info)| (name.to_owned(), info.clone()))
        .collect();
    bind_node_with_ctes(scope, node, &ctes)
}

/// Prepares the first CTE in a child binder, binds the rest and the node in it, then
/// wraps the result in the materialized CTE if it was referenced.
fn bind_node_with_ctes(
    scope: &mut BindScope<'_>,
    node: &mut QueryNode,
    ctes: &[(String, CommonTableExpressionInfo)],
) -> Result<BoundStatement> {
    let Some(((name, info), rest)) = ctes.split_first() else {
        return bind_node_kind(scope, node);
    };
    let cte = prepare_cte(scope, name, info);
    let mut child = scope.binder.child()?;
    let binding = Binding::new(
        BindingKind::Cte {
            can_be_referenced: true,
            reference_count: 0,
            bind_state: Some(Arc::clone(&cte.bind_state)),
        },
        BindingAlias::new(name),
        Vec::new(),
        Vec::new(),
        cte.setop_index,
    )?;
    child.bind_context.add_cte_binding(binding)?;
    let child_result = {
        let mut child_scope = BindScope {
            binder: &mut child,
            outer: Some(scope),
        };
        bind_node_with_ctes(&mut child_scope, node, rest)?
    };
    finish_cte(scope, cte, child, child_result)
}

/// DuckDB's `Binder::PrepareCTE`: the CTE is bound lazily, on first reference.
fn prepare_cte(scope: &mut BindScope<'_>, name: &str, info: &CommonTableExpressionInfo) -> BoundCteData {
    let setop_index = scope.generate_table_index();
    BoundCteData {
        name: name.to_owned(),
        materialized: info.materialized,
        setop_index,
        bind_state: Arc::new(Mutex::new(CteBindState {
            definition: (*info.query.node).clone(),
            aliases: info.aliases.clone(),
            bound: None,
            query_binder: None,
        })),
    }
}

/// DuckDB's `Binder::FinishCTE`.
fn finish_cte(
    scope: &mut BindScope<'_>,
    cte: BoundCteData,
    mut child_binder: super::Binder,
    child: BoundStatement,
) -> Result<BoundStatement> {
    let (bound, query_binder) = {
        let mut state = cte.bind_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.bound.take(), state.query_binder.take())
    };
    let (Some(bound), Some(mut query_binder)) = (bound, query_binder) else {
        // Never referenced: the CTE is not bound at all.
        scope.binder.move_correlated_expressions(&mut child_binder);
        return Ok(child);
    };
    child_binder.merge_correlated_columns(&query_binder.correlated_columns.clone());
    scope.binder.move_correlated_expressions(&mut child_binder);
    scope.binder.move_correlated_expressions(&mut query_binder);
    let column_count = child.types.len() as u64;
    let plan = LogicalOperator::new(LogicalKind::MaterializedCte {
        ctename: cte.name,
        table_index: cte.setop_index,
        column_count,
        materialize: cte.materialized,
        correlated_columns: super::CorrelatedColumns::default(),
    })
    .with_children(vec![bound.plan, child.plan]);
    scope.binder.has_unplanned_dependent_joins = scope.binder.has_unplanned_dependent_joins
        || child_binder.has_unplanned_dependent_joins
        || query_binder.has_unplanned_dependent_joins;
    Ok(BoundStatement::new(plan, child.names, child.types))
}

/// Binds the node itself (DuckDB's `BindNode` switch).
fn bind_node_kind(scope: &mut BindScope<'_>, node: &mut QueryNode) -> Result<BoundStatement> {
    match &node.kind {
        QueryNodeKind::Select(_) => bind_select_node::bind_select(scope, node),
        QueryNodeKind::SetOperation(_) => bind_setop_node::bind_set_operation(scope, node),
        QueryNodeKind::RecursiveCte(_) => Err(Error::not_implemented("recursive CTEs")),
    }
}
