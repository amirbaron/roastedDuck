//! The join hypergraph (DuckDB's `QueryGraphEdges`): edges from sets of relations to
//! neighboring sets, each with the filters that connect them. Edges are stored in a trie
//! keyed by the relations of the source set.

use std::collections::{BTreeSet, HashMap};

use super::relation_set::RelationSet;

/// An edge to a neighboring set and the filters on it (DuckDB's `NeighborInfo`). An edge
/// without filters is a cross product.
#[derive(Clone, Debug)]
pub struct NeighborInfo {
    pub neighbor: RelationSet,
    /// Indexes into the query graph manager's filters.
    pub filters: Vec<usize>,
}

/// A trie node (DuckDB's `QueryGraphEdges::QueryEdge`).
#[derive(Debug, Default)]
struct QueryEdge {
    neighbors: Vec<NeighborInfo>,
    children: HashMap<usize, QueryEdge>,
}

/// The edges of the join graph (DuckDB's `QueryGraphEdges`).
#[derive(Debug, Default)]
pub struct QueryGraphEdges {
    root: QueryEdge,
}

impl QueryGraphEdges {
    /// The trie node of a set, created if needed (DuckDB's `GetQueryEdge`).
    fn query_edge_mut(&mut self, set: &RelationSet) -> &mut QueryEdge {
        let mut info = &mut self.root;
        for &relation in set.relations() {
            info = info.children.entry(relation).or_default();
        }
        info
    }

    /// Adds an edge from `left` to `right`, or the filter to an existing one (DuckDB's
    /// `CreateEdge`).
    pub fn create_edge(&mut self, left: &RelationSet, right: &RelationSet, filter: Option<usize>) {
        let info = self.query_edge_mut(left);
        if let Some(existing) = info.neighbors.iter_mut().find(|n| &n.neighbor == right) {
            if let Some(filter) = filter {
                existing.filters.push(filter);
            }
            return;
        }
        info.neighbors.push(NeighborInfo {
            neighbor: right.clone(),
            filters: filter.into_iter().collect(),
        });
    }

    /// Calls `callback` on the edges of every subset of `node`, stopping when it returns
    /// true (DuckDB's `EnumerateNeighborsDFS`).
    fn enumerate_dfs<'a>(
        node: &RelationSet,
        info: &'a QueryEdge,
        index: usize,
        callback: &mut impl FnMut(&'a NeighborInfo) -> bool,
    ) {
        for neighbor in &info.neighbors {
            if callback(neighbor) {
                return;
            }
        }
        for node_index in index..node.count() {
            if let Some(child) = info.children.get(&node.relations()[node_index]) {
                Self::enumerate_dfs(node, child, node_index + 1, callback);
            }
        }
    }

    /// Calls `callback` on every edge leaving a subset of `node` (DuckDB's
    /// `EnumerateNeighbors`).
    pub fn enumerate_neighbors<'a>(
        &'a self,
        node: &RelationSet,
        mut callback: impl FnMut(&'a NeighborInfo) -> bool,
    ) {
        for (j, relation) in node.relations().iter().enumerate() {
            if let Some(child) = self.root.children.get(relation) {
                Self::enumerate_dfs(node, child, j + 1, &mut callback);
            }
        }
    }

    /// The smallest relation of every neighbor not in `exclusion_set` (DuckDB's
    /// `GetNeighbors`); callers sort the result.
    pub fn neighbors(&self, node: &RelationSet, exclusion_set: &BTreeSet<usize>) -> Vec<usize> {
        let mut result = BTreeSet::new();
        self.enumerate_neighbors(node, |info| {
            if let Some(&first) = info.neighbor.relations().first()
                && !exclusion_set.contains(&first)
            {
                result.insert(first);
            }
            false
        });
        result.into_iter().collect()
    }

    /// The edges from `node` into subsets of `other` (DuckDB's `GetConnections`).
    pub fn connections(&self, node: &RelationSet, other: &RelationSet) -> Vec<&NeighborInfo> {
        let mut connections = Vec::new();
        self.enumerate_neighbors(node, |info| {
            if other.contains_set(&info.neighbor) {
                connections.push(info);
            }
            false
        });
        connections
    }
}
