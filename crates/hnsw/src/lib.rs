//! Pure Rust hierarchical navigable small world graph for compiled-in VSS.
//!
//! Construction and search follow Malkov and Yashunin's HNSW algorithms:
//! <https://arxiv.org/abs/1603.09320> (algorithms 1–4). There is no dependency on
//! `USearch`, no persisted binary format, and no exact-scan fallback. The database
//! wrapper owns transaction visibility and SQL metric/error semantics.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::mem::size_of;

use roastedduck_common::{Error, Result};

#[cfg(test)]
mod tests;

/// Internal navigation distance. SQL result scores are evaluated by the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    L2Squared,
    Cosine,
    InnerProduct,
}

impl Metric {
    /// Compute navigation distance from f32 coordinates. L2/IP accumulate in f64;
    /// cosine preserves DuckDB's FLOAT kernel arithmetic and comparison-order clamp.
    /// Other non-finite results retain NaN, ordered after +infinity by the graph.
    pub fn distance(self, left: &[f32], right: &[f32]) -> f64 {
        match self {
            Self::L2Squared => left
                .iter()
                .zip(right)
                .map(|(&a, &b)| {
                    let delta = f64::from(a) - f64::from(b);
                    delta * delta
                })
                .sum(),
            Self::InnerProduct => -left
                .iter()
                .zip(right)
                .map(|(&a, &b)| f64::from(a) * f64::from(b))
                .sum::<f64>(),
            Self::Cosine => {
                // DuckDB 1.5.6 core_functions/array_kernels.hpp, also ported in
                // functions/src/scalar/array/kernels.rs. FLOAT accumulation matters:
                // promoting before the norms would hide SQL's f32 overflow cases.
                let mut dot = 0.0f32;
                let mut left_norm = 0.0f32;
                let mut right_norm = 0.0f32;
                for (&a, &b) in left.iter().zip(right) {
                    dot = a.mul_add(b, dot);
                    left_norm = a.mul_add(a, left_norm);
                    right_norm = b.mul_add(b, right_norm);
                }
                let similarity = dot / (left_norm * right_norm).sqrt();
                // Preserve std::max(-1, std::min(similarity, 1)) comparison order:
                // NaN survives min, then becomes -1 in max. Rust's clamp/min/max
                // have different NaN conventions and must not replace this.
                let upper = if 1.0 < similarity { 1.0 } else { similarity };
                let bounded = if -1.0 < upper { upper } else { -1.0 };
                f64::from(1.0 - bounded)
            }
        }
    }
}

/// Immutable graph construction and default search settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub dimensions: usize,
    pub metric: Metric,
    pub m: usize,
    pub m0: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
    pub seed: u64,
}

impl Config {
    /// Defaults corresponding to the pinned VSS extension's construction settings.
    pub const fn new(dimensions: usize, metric: Metric) -> Self {
        Self {
            dimensions,
            metric,
            m: 16,
            m0: 32,
            ef_construction: 128,
            ef_search: 64,
            seed: 0x4d59_5df4_d0f3_3173,
        }
    }

    /// Validate allocation-size arithmetic without imposing artificial SQL bounds.
    pub fn validate(&self) -> Result<()> {
        if self.dimensions == 0 {
            return Err(Error::invalid_input("HNSW dimensions must be positive"));
        }
        if self.m < 2 || self.m0 < 2 {
            return Err(Error::invalid_input("HNSW M and M0 must be at least 2"));
        }
        if self.ef_construction == 0 || self.ef_search == 0 {
            return Err(Error::invalid_input(
                "HNSW ef_construction and ef_search must be positive",
            ));
        }
        for (count, element_size) in [
            (self.dimensions, size_of::<f32>()),
            (self.m, size_of::<usize>()),
            (self.m0, size_of::<usize>()),
            (self.ef_construction, size_of::<RetainedCandidate>()),
            (self.ef_search, size_of::<RetainedCandidate>()),
        ] {
            if count
                .checked_mul(element_size)
                .is_none_or(|bytes| bytes > isize::MAX as usize)
            {
                return Err(Error::invalid_input(
                    "HNSW configuration exceeds addressable allocation size",
                ));
            }
        }
        Ok(())
    }
}

/// A live result label and its internal navigation distance.
#[derive(Clone, Copy, Debug)]
pub struct Neighbor {
    pub label: i64,
    pub distance: f64,
}

/// Actual work performed by graph traversal, including repeated upper-layer and cross-layer visits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchStats {
    pub distance_evaluations: usize,
    pub visited_nodes: usize,
    pub expanded_nodes: usize,
}

/// Sorted nearest candidates and traversal counters; results can be approximate.
#[derive(Clone, Debug, Default)]
pub struct SearchResult {
    pub neighbors: Vec<Neighbor>,
    pub stats: SearchStats,
}

/// Structural accounting for one layer (including routing tombstones).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LevelStats {
    pub nodes: usize,
    pub edges: usize,
    pub max_edges: usize,
    pub allocated_bytes: usize,
}

/// Structural memory estimates describe this Rust graph, not an upstream graph image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphStats {
    pub dimensions: usize,
    pub count: usize,
    pub capacity: usize,
    pub deleted: usize,
    pub levels: usize,
    pub approx_memory_usage: usize,
    pub levels_stats: Vec<LevelStats>,
}

#[derive(Clone, Debug)]
struct Node {
    label: i64,
    vector: Box<[f32]>,
    neighbors: Vec<Vec<usize>>,
    active: bool,
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
    node: usize,
    distance: f64,
}

fn distance_order(left: f64, right: f64) -> Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
    }
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        distance_order(self.distance, other.distance).then_with(|| self.node.cmp(&other.node))
    }
}

// Retention ordering is separate from frontier traversal. The pinned extension
// admits candidates while its bounded result buffer is not full (or the distance
// improves), then inserts a new equal-distance candidate before previous equals.
// A discovery sequence expresses that convention without assuming row-ID order.
#[derive(Clone, Copy, Debug)]
struct RetainedCandidate {
    candidate: Candidate,
    discovery_order: usize,
}

impl PartialEq for RetainedCandidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for RetainedCandidate {}
impl PartialOrd for RetainedCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RetainedCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        distance_order(self.candidate.distance, other.candidate.distance)
            .then_with(|| other.discovery_order.cmp(&self.discovery_order))
            .then_with(|| self.candidate.node.cmp(&other.candidate.node))
    }
}

/// In-memory HNSW with independently owned vectors and stable internal node IDs.
///
/// Live labels are unique. Removing a label keeps its node/vector as a routing
/// tombstone until compact. Reinserting a removed label creates a new node; the
/// old node stays inactive and can never be returned as a search result.
#[derive(Debug)]
pub struct Hnsw {
    config: Config,
    nodes: Vec<Node>,
    labels: HashMap<i64, usize>,
    entry_point: Option<usize>,
    max_level: usize,
    random_state: u64,
}

impl Hnsw {
    pub fn new(config: Config) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            random_state: config.seed,
            config,
            nodes: Vec::new(),
            labels: HashMap::new(),
            entry_point: None,
            max_level: 0,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn len(&self) -> usize {
        self.labels.len()
    }
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }
    pub fn contains(&self, label: i64) -> bool {
        self.labels.contains_key(&label)
    }
    pub fn vector(&self, label: i64) -> Option<&[f32]> {
        self.labels
            .get(&label)
            .map(|&node| self.nodes[node].vector.as_ref())
    }

    fn validate_vector(&self, vector: &[f32]) -> Result<()> {
        if vector.len() != self.config.dimensions {
            return Err(Error::invalid_input(format!(
                "HNSW expected {} dimensions, got {}",
                self.config.dimensions,
                vector.len()
            )));
        }
        Ok(())
    }

    /// Validate before changing graph state. Non-finite coordinates are accepted;
    /// their SQL treatment is owned by the database wrapper, not narrowed here.
    pub fn insert(&mut self, label: i64, vector: &[f32]) -> Result<()> {
        self.validate_vector(vector)?;
        if self.labels.contains_key(&label) {
            return Err(Error::invalid_input(format!("HNSW label {label} already exists")));
        }
        self.insert_validated(label, vector);
        Ok(())
    }

    /// Every input is validated before any node, edge, label or RNG state changes.
    /// A duplicate live label or duplicate within the batch rejects the whole batch.
    pub fn insert_batch(&mut self, vectors: &[(i64, Vec<f32>)]) -> Result<()> {
        let mut incoming = HashSet::new();
        for (label, vector) in vectors {
            self.validate_vector(vector)?;
            if self.labels.contains_key(label) || !incoming.insert(*label) {
                return Err(Error::invalid_input(format!("HNSW label {label} already exists")));
            }
        }
        for (label, vector) in vectors {
            self.insert_validated(*label, vector);
        }
        Ok(())
    }

    /// Remove a live label idempotently, retaining its vector and edges for routing.
    pub fn remove(&mut self, label: i64) -> bool {
        let Some(node) = self.labels.remove(&label) else {
            return false;
        };
        self.nodes[node].active = false;
        true
    }

    pub fn remove_batch(&mut self, labels: &[i64]) -> usize {
        labels.iter().filter(|&&label| self.remove(label)).count()
    }

    /// Rebuild only live labels into a fresh hierarchy, then atomically replace it.
    /// The caller must not remove an old-visible row before transaction cleanup.
    pub fn compact(&mut self) -> Result<()> {
        let mut rebuilt = Self::new(self.config.clone())?;
        for node in &self.nodes {
            if node.active {
                rebuilt.insert_validated(node.label, &node.vector);
            }
        }
        *self = rebuilt;
        Ok(())
    }

    /// Release all graph nodes and edges, preserving construction options.
    pub fn clear(&mut self) {
        self.nodes = Vec::new();
        self.labels = HashMap::new();
        self.entry_point = None;
        self.max_level = 0;
        self.random_state = self.config.seed;
    }

    /// Graph-only approximate top-k. No array scan or exact-search fallback occurs.
    /// ef is at least k so the search result heap can hold the requested count.
    pub fn search(&self, query: &[f32], k: usize, ef_search: Option<usize>) -> Result<SearchResult> {
        self.validate_vector(query)?;
        let ef = ef_search.unwrap_or(self.config.ef_search);
        if ef == 0 {
            return Err(Error::invalid_input("HNSW ef_search must be positive"));
        }
        let mut result = SearchResult::default();
        if k == 0 || self.labels.is_empty() {
            return Ok(result);
        }
        let Some(mut entry) = self.entry_point else {
            return Ok(result);
        };
        for layer in (1..=self.max_level).rev() {
            entry = self.greedy(query, entry, layer, &mut result.stats).node;
        }
        let candidates = self.search_layer(
            query,
            &[entry],
            ef.max(k).min(self.nodes.len()),
            0,
            false,
            &mut result.stats,
        );
        result.neighbors = candidates
            .into_iter()
            .take(k)
            .map(|candidate| Neighbor {
                label: self.nodes[candidate.node].label,
                distance: candidate.distance,
            })
            .collect();
        Ok(result)
    }

    /// Approximate allocated bytes include vector/link capacities and hash buckets.
    pub fn stats(&self) -> GraphStats {
        let levels = self.entry_point.map_or(0, |_| self.max_level + 1);
        let mut levels_stats = vec![LevelStats::default(); levels];
        let mut bytes = size_of::<Self>()
            + self.nodes.capacity() * size_of::<Node>()
            + self.labels.capacity() * (size_of::<i64>() + size_of::<usize>() + 1);
        for node in &self.nodes {
            bytes +=
                node.vector.len() * size_of::<f32>() + node.neighbors.capacity() * size_of::<Vec<usize>>();
            for (level, neighbors) in node.neighbors.iter().enumerate() {
                let allocated = neighbors.capacity() * size_of::<usize>();
                bytes += allocated;
                let stats = &mut levels_stats[level];
                stats.nodes += 1;
                stats.edges += neighbors.len();
                stats.max_edges = stats.max_edges.saturating_add(self.max_connections(level));
                stats.allocated_bytes += allocated + size_of::<Vec<usize>>();
            }
        }
        GraphStats {
            dimensions: self.config.dimensions,
            count: self.labels.len(),
            capacity: self.nodes.capacity(),
            deleted: self.nodes.len() - self.labels.len(),
            levels,
            approx_memory_usage: bytes,
            levels_stats,
        }
    }

    fn max_connections(&self, layer: usize) -> usize {
        if layer == 0 { self.config.m0 } else { self.config.m }
    }

    fn random_level(&mut self) -> usize {
        // SplitMix64: deterministic construction without an external RNG dependency.
        self.random_state = self.random_state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut random = self.random_state;
        random = (random ^ (random >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        random = (random ^ (random >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        random ^= random >> 31;
        let uniform = ((random >> 11) + 1) as f64 / 9_007_199_254_740_992.0;
        (-uniform.ln() / (self.config.m as f64).ln()).floor() as usize
    }

    fn candidate(&self, query: &[f32], node: usize, stats: &mut SearchStats) -> Candidate {
        stats.distance_evaluations += 1;
        stats.visited_nodes += 1;
        Candidate {
            node,
            distance: self.config.metric.distance(query, &self.nodes[node].vector),
        }
    }

    fn greedy(&self, query: &[f32], entry: usize, layer: usize, stats: &mut SearchStats) -> Candidate {
        let mut best = self.candidate(query, entry, stats);
        loop {
            let previous = best.node;
            stats.expanded_nodes += 1;
            for &neighbor in &self.nodes[previous].neighbors[layer] {
                let next = self.candidate(query, neighbor, stats);
                if next < best {
                    best = next;
                }
            }
            if best.node == previous {
                return best;
            }
        }
    }

    fn search_layer(
        &self,
        query: &[f32],
        entries: &[usize],
        ef: usize,
        layer: usize,
        include_removed: bool,
        stats: &mut SearchStats,
    ) -> Vec<Candidate> {
        let mut visited = HashSet::new();
        let mut frontier = BinaryHeap::new();
        let mut nearest = BinaryHeap::new();
        for &entry in entries {
            if visited.insert(entry) {
                let candidate = self.candidate(query, entry, stats);
                frontier.push(Reverse(candidate));
                if include_removed || self.nodes[entry].active {
                    nearest.push(RetainedCandidate {
                        candidate,
                        discovery_order: visited.len(),
                    });
                    if nearest.len() > ef {
                        nearest.pop();
                    }
                }
            }
        }
        while let Some(Reverse(candidate)) = frontier.pop() {
            // Construction keeps the classical radius stop. During a query,
            // finish work already admitted to the frontier: a candidate that
            // became worse than the retained ef radius can still be the bridge
            // to a nearer node. This spends more real graph traversal work but
            // does not enlarge the retained result heap or scan the node array.
            if include_removed
                && nearest.len() >= ef
                && nearest.peek().is_some_and(|worst: &RetainedCandidate| {
                    distance_order(candidate.distance, worst.candidate.distance) == Ordering::Greater
                })
            {
                break;
            }
            stats.expanded_nodes += 1;
            for &neighbor in &self.nodes[candidate.node].neighbors[layer] {
                if !visited.insert(neighbor) {
                    continue;
                }
                let next = self.candidate(query, neighbor, stats);
                if nearest.len() < ef
                    || nearest.peek().is_some_and(|worst: &RetainedCandidate| {
                        distance_order(next.distance, worst.candidate.distance) == Ordering::Less
                    })
                {
                    frontier.push(Reverse(next));
                    if include_removed || self.nodes[neighbor].active {
                        nearest.push(RetainedCandidate {
                            candidate: next,
                            discovery_order: visited.len(),
                        });
                        if nearest.len() > ef {
                            nearest.pop();
                        }
                    }
                }
            }
        }
        nearest
            .into_sorted_vec()
            .into_iter()
            .map(|entry| entry.candidate)
            .collect()
    }

    fn select_neighbors(&self, candidates: &[Candidate], maximum: usize, center: usize) -> Vec<usize> {
        // Equal distances must not permanently favor the oldest nodes: otherwise
        // identical vectors saturate the first hub and later nodes get no backlinks.
        // HNSW leaves tie order unspecified; a stable pair hash distributes it.
        let mut candidates = candidates.to_vec();
        candidates.sort_unstable_by(|left, right| {
            distance_order(left.distance, right.distance)
                .then_with(|| Self::tie_key(center, left.node).cmp(&Self::tie_key(center, right.node)))
                .then_with(|| left.node.cmp(&right.node))
        });
        let mut selected = Vec::new();
        let mut rejected = Vec::new();
        for candidate in &candidates {
            if selected.len() == maximum {
                break;
            }
            // Algorithm 4: keep a candidate if no previously selected neighbor
            // is closer to it than the inserted/pruned node is.
            let diverse = selected.iter().all(|&other: &usize| {
                let candidate_vector = &self.nodes[candidate.node].vector;
                let selected_vector = &self.nodes[other].vector;
                // Labels with identical vectors are distinct results but provide
                // no additional routing direction. In particular, 0 >= 0 must
                // not let a zero-distance duplicate group consume every link.
                // Keep these candidates for the final capacity fill instead.
                if candidate_vector == selected_vector {
                    return false;
                }
                // Inner product is not a metric: a large-norm neighbor can
                // appear closer to every candidate than the center does, which
                // suppresses useful directions. Keep inner-product candidate
                // priority, but measure routing diversity in Euclidean space.
                // Query distances and the configured M/ef bounds are unchanged.
                let (between, from_center) = if self.config.metric == Metric::InnerProduct {
                    (
                        Metric::L2Squared.distance(candidate_vector, selected_vector),
                        Metric::L2Squared.distance(candidate_vector, &self.nodes[center].vector),
                    )
                } else {
                    (
                        self.config.metric.distance(candidate_vector, selected_vector),
                        candidate.distance,
                    )
                };
                distance_order(between, from_center) != Ordering::Less
            });
            if diverse {
                selected.push(candidate.node);
            } else {
                rejected.push(candidate.node);
            }
        }
        // keepPrunedConnections=true: fill remaining capacity with closest rejects.
        selected.extend(rejected.into_iter().take(maximum - selected.len()));
        selected
    }

    fn tie_key(center: usize, neighbor: usize) -> u64 {
        let mut value = (center as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ neighbor as u64;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn insert_validated(&mut self, label: i64, vector: &[f32]) {
        let level = self.random_level();
        let node_id = self.nodes.len();
        self.nodes.push(Node {
            label,
            vector: vector.into(),
            neighbors: vec![Vec::new(); level + 1],
            active: true,
        });
        self.labels.insert(label, node_id);
        let Some(mut entry) = self.entry_point else {
            self.entry_point = Some(node_id);
            self.max_level = level;
            return;
        };
        let mut stats = SearchStats::default();
        if level < self.max_level {
            for layer in ((level + 1)..=self.max_level).rev() {
                entry = self.greedy(vector, entry, layer, &mut stats).node;
            }
        }
        let mut entries = vec![entry];
        for layer in (0..=level.min(self.max_level)).rev() {
            let candidates = self.search_layer(
                vector,
                &entries,
                self.config.ef_construction.min(node_id).max(1),
                layer,
                true,
                &mut stats,
            );
            let neighbors = self.select_neighbors(
                &candidates,
                self.config.m.min(self.max_connections(layer)),
                node_id,
            );
            self.nodes[node_id].neighbors[layer].clone_from(&neighbors);
            for neighbor in neighbors {
                self.nodes[neighbor].neighbors[layer].push(node_id);
                if self.nodes[neighbor].neighbors[layer].len() > self.max_connections(layer) {
                    let mut candidates: Vec<_> = self.nodes[neighbor].neighbors[layer]
                        .iter()
                        .map(|&other| Candidate {
                            node: other,
                            distance: self
                                .config
                                .metric
                                .distance(&self.nodes[neighbor].vector, &self.nodes[other].vector),
                        })
                        .collect();
                    candidates.sort_unstable();
                    self.nodes[neighbor].neighbors[layer] =
                        self.select_neighbors(&candidates, self.max_connections(layer), neighbor);
                }
            }
            entries = candidates.into_iter().map(|candidate| candidate.node).collect();
        }
        if level > self.max_level {
            self.entry_point = Some(node_id);
            self.max_level = level;
        }
    }
}
