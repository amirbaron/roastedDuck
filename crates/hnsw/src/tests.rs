use super::*;

fn exact(metric: Metric, vectors: &[(i64, Vec<f32>)], query: &[f32], k: usize) -> Vec<Neighbor> {
    let mut results: Vec<_> = vectors
        .iter()
        .map(|(label, vector)| Neighbor {
            label: *label,
            distance: metric.distance(vector, query),
        })
        .collect();
    results.sort_by(|left, right| {
        distance_order(left.distance, right.distance).then(left.label.cmp(&right.label))
    });
    results.truncate(k);
    results
}

fn generator(state: &mut u64) -> f32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    ((*state >> 40) as u32) as f32 / 16_777_216.0 - 0.5
}

#[test]
fn small_graph_matches_exhaustive_ground_truth_for_every_metric() -> Result<()> {
    let vectors = vec![
        (0, vec![1.0, 0.0, 0.0]),
        (1, vec![0.0, 1.0, 0.0]),
        (2, vec![0.0, 0.0, 1.0]),
        (3, vec![-1.0, -1.0, 0.25]),
        (4, vec![0.5, 0.5, 0.5]),
        (5, vec![2.0, 1.0, -1.0]),
        (6, vec![-0.25, 0.75, 0.125]),
    ];
    for metric in [Metric::L2Squared, Metric::Cosine, Metric::InnerProduct] {
        let mut index = Hnsw::new(Config::new(3, metric))?;
        index.insert_batch(&vectors)?;
        for query in [
            vec![1.0, 0.2, -0.3],
            vec![0.4, 0.5, 0.6],
            vec![-0.5, -0.25, 0.125],
        ] {
            for k in 1..=vectors.len() {
                let found = index.search(&query, k, Some(vectors.len()))?;
                let expected = exact(metric, &vectors, &query, k);
                assert_eq!(
                    found.neighbors.iter().map(|n| n.label).collect::<Vec<_>>(),
                    expected.iter().map(|n| n.label).collect::<Vec<_>>()
                );
                for (actual, expected) in found.neighbors.iter().zip(expected) {
                    assert!((actual.distance - expected.distance).abs() < 1e-12);
                }
                assert!(found.stats.distance_evaluations > 0);
            }
        }
    }
    Ok(())
}

#[test]
fn batch_validation_is_atomic_including_rng_and_topology() -> Result<()> {
    let config = Config::new(2, Metric::L2Squared);
    let mut tested = Hnsw::new(config.clone())?;
    let mut control = Hnsw::new(config)?;
    tested.insert(1, &[1.0, 2.0])?;
    control.insert(1, &[1.0, 2.0])?;
    let baseline = tested.stats();
    for batch in [
        vec![(2, vec![1.0, 2.0]), (3, vec![1.0])],
        vec![(2, vec![1.0, 2.0]), (1, vec![3.0, 4.0])],
        vec![(2, vec![1.0, 2.0]), (2, vec![3.0, 4.0])],
    ] {
        assert!(tested.insert_batch(&batch).is_err());
        assert_eq!(tested.stats(), baseline);
        assert!(!tested.contains(2));
    }
    for label in 2..100 {
        let vector = [label as f32, (label % 7) as f32];
        tested.insert(label, &vector)?;
        control.insert(label, &vector)?;
    }
    assert_eq!(tested.stats(), control.stats());
    for (left, right) in tested.nodes.iter().zip(&control.nodes) {
        assert_eq!(left.neighbors, right.neighbors);
    }
    Ok(())
}

#[test]
fn tombstones_route_but_never_return_and_reinsert_is_a_new_node() -> Result<()> {
    let mut config = Config::new(2, Metric::L2Squared);
    config.m = 4;
    config.m0 = 8;
    let mut index = Hnsw::new(config)?;
    for label in 0..200 {
        index.insert(label, &[label as f32, (label % 3) as f32])?;
    }
    let original_entry = index
        .entry_point
        .ok_or_else(|| Error::internal("test graph has no entry"))?;
    let removed_label = index.nodes[original_entry].label;
    assert!(index.remove(removed_label));
    assert!(!index.remove(removed_label));
    assert!(index.vector(removed_label).is_none());
    assert_eq!(index.entry_point, Some(original_entry));
    let query = index.nodes[original_entry].vector.clone();
    assert!(
        index
            .search(&query, 200, Some(200))?
            .neighbors
            .iter()
            .all(|n| n.label != removed_label)
    );
    index.insert(removed_label, &[-1000.0, -1000.0])?;
    let found = index.search(&[-1000.0, -1000.0], 1, Some(200))?;
    assert_eq!(found.neighbors[0].label, removed_label);
    assert_eq!(found.neighbors[0].distance.to_bits(), 0.0f64.to_bits());
    assert_eq!(index.stats().deleted, 1);
    assert_eq!(index.stats().count, 200);
    assert!(!index.nodes[original_entry].active);
    index.compact()?;
    assert_eq!(index.stats().deleted, 0);
    assert_eq!(index.nodes.len(), 200);
    assert_eq!(
        index.search(&[-1000.0, -1000.0], 1, Some(200))?.neighbors[0].label,
        removed_label
    );
    assert_eq!(index.remove_batch(&(0..200).collect::<Vec<_>>()), 200);
    assert!(index.search(&[0.0, 0.0], 10, None)?.neighbors.is_empty());
    index.compact()?;
    assert_eq!(index.stats().levels, 0);
    assert_eq!(index.stats().deleted, 0);
    index.insert(-1, &[0.25, 0.5])?;
    assert_eq!(index.search(&[0.25, 0.5], 10, None)?.neighbors[0].label, -1);
    Ok(())
}

#[test]
fn held_out_recall_uses_hierarchy_with_bounded_degrees() -> Result<()> {
    for metric in [Metric::L2Squared, Metric::Cosine, Metric::InnerProduct] {
        let config = Config::new(16, metric);
        let mut random = 12345;
        let vectors: Vec<_> = (0..1500)
            .map(|label| (label, (0..16).map(|_| generator(&mut random)).collect::<Vec<_>>()))
            .collect();
        let queries: Vec<_> = (0..30)
            .map(|_| (0..16).map(|_| generator(&mut random)).collect::<Vec<_>>())
            .collect();
        let mut index = Hnsw::new(config)?;
        index.insert_batch(&vectors)?;
        assert!(index.stats().levels > 1);
        for node in &index.nodes {
            for (level, neighbors) in node.neighbors.iter().enumerate() {
                assert!(neighbors.len() <= index.max_connections(level));
                let unique: HashSet<_> = neighbors.iter().collect();
                assert_eq!(neighbors.len(), unique.len());
            }
        }
        let mut matched = 0;
        let mut distances = 0;
        for query in &queries {
            let found = index.search(query, 10, Some(100))?;
            let expected: HashSet<_> = exact(metric, &vectors, query, 10)
                .into_iter()
                .map(|n| n.label)
                .collect();
            matched += found
                .neighbors
                .iter()
                .filter(|n| expected.contains(&n.label))
                .count();
            distances += found.stats.distance_evaluations;
        }
        assert!(matched >= 285, "{metric:?}: recall {}", matched as f64 / 300.0);
        assert!(
            distances < vectors.len() * queries.len(),
            "graph traversal evaluated all rows for every query: {metric:?}"
        );
    }
    Ok(())
}

#[test]
fn invalid_configuration_dimensions_and_search_boundaries() -> Result<()> {
    for (dimensions, m, m0, construction, search) in [
        (0, 16, 32, 128, 64),
        (3, 1, 32, 128, 64),
        (3, 16, 1, 128, 64),
        (3, 16, 32, 0, 64),
        (3, 16, 32, 128, 0),
        (usize::MAX, 16, 32, 128, 64),
        (3, usize::MAX, 32, 128, 64),
    ] {
        let mut config = Config::new(dimensions, Metric::L2Squared);
        config.m = m;
        config.m0 = m0;
        config.ef_construction = construction;
        config.ef_search = search;
        assert!(Hnsw::new(config).is_err());
    }
    let mut index = Hnsw::new(Config::new(2, Metric::L2Squared))?;
    assert!(index.insert(1, &[1.0]).is_err());
    assert!(index.search(&[1.0], 1, None).is_err());
    assert!(index.search(&[1.0, 2.0], 1, Some(0)).is_err());
    assert!(index.search(&[1.0, 2.0], 1, None)?.neighbors.is_empty());
    index.insert(1, &[1.0, 2.0])?;
    assert!(index.insert(1, &[3.0, 4.0]).is_err());
    assert!(index.search(&[1.0, 2.0], 0, None)?.neighbors.is_empty());
    assert_eq!(index.search(&[1.0, 2.0], usize::MAX, None)?.neighbors.len(), 1);
    index.clear();
    assert_eq!(index.stats().count, 0);
    assert_eq!(index.stats().capacity, 0);
    Ok(())
}

#[test]
fn nonfinite_inputs_have_deterministic_total_order_without_rejection() -> Result<()> {
    assert_eq!(distance_order(f64::NAN, f64::INFINITY), Ordering::Greater);
    assert_eq!(distance_order(-0.0, 0.0), Ordering::Equal);
    let mut index = Hnsw::new(Config::new(1, Metric::L2Squared))?;
    index.insert(0, &[0.0])?;
    index.insert(1, &[f32::INFINITY])?;
    index.insert(2, &[f32::NAN])?;
    let results = index.search(&[0.0], 3, Some(3))?;
    assert_eq!(
        results.neighbors.iter().map(|n| n.label).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(results.neighbors[1].distance.is_infinite());
    assert!(results.neighbors[2].distance.is_nan());
    let mut cosine = Hnsw::new(Config::new(2, Metric::Cosine))?;
    cosine.insert(0, &[0.0, 0.0])?;
    assert_eq!(
        cosine.search(&[1.0, 0.0], 1, None)?.neighbors[0]
            .distance
            .to_bits(),
        2.0f64.to_bits()
    );
    Ok(())
}

#[test]
fn duplicate_vectors_do_not_starve_later_labels() -> Result<()> {
    let mut index = Hnsw::new(Config::new(3, Metric::L2Squared))?;
    for label in 0..256 {
        index.insert(label, &[1.0, 1.0, 1.0])?;
    }
    let found = index.search(&[1.0, 1.0, 1.0], 100, Some(200))?;
    assert_eq!(found.neighbors.len(), 100);
    assert!(
        found
            .neighbors
            .iter()
            .all(|n| n.distance.to_bits() == 0.0f64.to_bits())
    );
    index.remove_batch(&(0..200).collect::<Vec<_>>());
    let found = index.search(&[1.0, 1.0, 1.0], 56, Some(256))?;
    assert_eq!(found.neighbors.len(), 56);
    assert!(found.neighbors.iter().all(|n| n.label >= 200));
    Ok(())
}

#[test]
fn cosine_matches_duckdb_zero_nonfinite_and_float_norm_conventions() {
    // Pinned source: DuckDB 1.5.6 core_functions/array_kernels.hpp.
    // Actual zero-vector oracle receipt:
    // .context/artifacts/vss-fts/vss-oracle-probes-1/zero_cosine.stdout
    // yields distance 2.0 and similarity -1.0. Additional cases exercise that
    // same source arithmetic; their separate SQL oracle coverage is not implied.
    for (left, right, expected) in [
        ([0.0, 0.0], [1.0, 0.0], 2.0f64),
        ([0.0, 0.0], [0.0, 0.0], 2.0),
        ([1.0, 0.0], [1.0, 0.0], 0.0),
        ([1.0, 0.0], [-1.0, 0.0], 2.0),
        ([1.0, 0.0], [0.0, 1.0], 1.0),
        ([f32::NAN, 0.0], [1.0, 0.0], 2.0),
        ([f32::INFINITY, 0.0], [1.0, 0.0], 2.0),
        ([f32::MAX, 0.0], [1.0, 0.0], 1.0),
        ([f32::MAX, 0.0], [f32::MAX, 0.0], 2.0),
    ] {
        assert_eq!(
            Metric::Cosine.distance(&left, &right).to_bits(),
            expected.to_bits()
        );
    }
}

#[test]
fn tied_candidates_follow_discovery_retention_not_label_order() -> Result<()> {
    // Controlled reachable star: entry label 1 discovers labels 3, 0, 2 in that
    // order. This deliberately differs from both ascending and descending IDs.
    // Pinned candidate-buffer source and oracle evidence are recorded in
    // .context/artifacts/vss-fts/tie-policy-probes/finding.md.
    let mut index = Hnsw::new(Config::new(1, Metric::L2Squared))?;
    for label in 0..4 {
        index.insert(label, &[0.0])?;
    }
    index.entry_point = Some(1);
    index.max_level = 0;
    for (node_id, node) in index.nodes.iter_mut().enumerate() {
        node.neighbors = vec![if node_id == 1 { vec![3, 0, 2] } else { vec![1] }];
    }
    // With room for every candidate, latest discovered ties come first.
    assert_eq!(
        index
            .search(&[0.0], 3, Some(4))?
            .neighbors
            .iter()
            .map(|n| n.label)
            .collect::<Vec<_>>(),
        vec![2, 0, 3]
    );
    // Once the bounded buffer is full, equal-radius successors are not admitted.
    // This is why a larger search budget may change the tied candidate subset.
    assert_eq!(
        index
            .search(&[0.0], 2, Some(2))?
            .neighbors
            .iter()
            .map(|n| n.label)
            .collect::<Vec<_>>(),
        vec![3, 1]
    );
    // The entry remains available for routing but does not consume result space
    // after removal: the next two live discoveries fill the bounded result heap.
    assert!(index.remove(1));
    assert_eq!(
        index
            .search(&[0.0], 2, Some(2))?
            .neighbors
            .iter()
            .map(|n| n.label)
            .collect::<Vec<_>>(),
        vec![0, 3]
    );
    Ok(())
}

#[test]
fn duplicate_labels_do_not_exhaust_the_diverse_neighbor_budget() -> Result<()> {
    let mut index = Hnsw::new(Config::new(2, Metric::L2Squared))?;
    index.insert(0, &[0.0, 0.0])?;
    for label in 1..=8 {
        index.insert(label, &[0.0, 0.0])?;
    }
    for (label, vector) in [
        (9, [1.0, 0.0]),
        (10, [0.0, 1.0]),
        (11, [-1.0, 0.0]),
        (12, [0.0, -1.0]),
    ] {
        index.insert(label, &vector)?;
    }
    let candidates: Vec<_> = (1..index.nodes.len())
        .map(|node| Candidate {
            node,
            distance: Metric::L2Squared.distance(&[0.0, 0.0], &index.nodes[node].vector),
        })
        .collect();
    let neighbors = index.select_neighbors(&candidates, 4, 0);
    let directions: HashSet<_> = neighbors
        .iter()
        .map(|&node| {
            index.nodes[node]
                .vector
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(neighbors.len(), 4);
    assert_eq!(
        directions.len(),
        4,
        "duplicate row labels must not consume distinct routing directions"
    );
    // When no distinct directions remain, fill with duplicate labels to retain
    // graph connectivity; deduplication is not deletion of legitimate SQL rows.
    let duplicate_neighbors = index.select_neighbors(&candidates[..8], 4, 0);
    assert_eq!(duplicate_neighbors.len(), 4);
    assert_eq!(
        duplicate_neighbors.iter().copied().collect::<HashSet<_>>().len(),
        4
    );
    Ok(())
}

#[test]
fn query_expands_an_admitted_bridge_even_after_its_candidate_is_displaced() -> Result<()> {
    let mut index = Hnsw::new(Config::new(1, Metric::L2Squared))?;
    for (label, value) in [(0, 10.0), (1, 4.0), (2, 5.0), (3, 1.0), (4, 0.0)] {
        index.insert(label, &[value])?;
    }
    index.entry_point = Some(0);
    index.max_level = 0;
    // Entry0 admits bridge2 first, then nearer1 displaces2 from the ef=1 heap.
    // Only the already-admitted bridge2 reaches the even-nearer node3.
    // Node4 is intentionally unreachable: returning it would reveal a full scan.
    for (node, neighbors) in index
        .nodes
        .iter_mut()
        .zip([vec![2, 1], vec![0], vec![0, 3], vec![2], vec![]])
    {
        node.neighbors = vec![neighbors];
    }
    let found = index.search(&[0.0], 1, Some(1))?;
    assert_eq!(found.neighbors.len(), 1);
    assert_eq!(found.neighbors[0].label, 3);
    assert_eq!(found.stats.distance_evaluations, 4);
    assert_eq!(found.stats.visited_nodes, 4);
    assert_eq!(found.stats.expanded_nodes, 4);
    // Index construction deliberately retains its prior radius-bounded search.
    let built = index.search_layer(&[0.0], &[0], 1, 0, true, &mut SearchStats::default());
    assert_eq!(built[0].node, 1);
    Ok(())
}

#[test]
fn inner_product_links_keep_geometric_directions_despite_large_norms() -> Result<()> {
    let mut index = Hnsw::new(Config::new(2, Metric::InnerProduct))?;
    for (label, vector) in [
        (0, [1.0, 0.0]),
        (1, [100.0, 0.0]),
        (2, [90.0, 0.0]),
        (3, [1.0, 1.0]),
    ] {
        index.insert(label, &vector)?;
    }
    let center = index.labels[&0];
    let candidates: Vec<_> = [1, 2, 3]
        .into_iter()
        .map(|label| {
            let node = index.labels[&label];
            Candidate {
                node,
                distance: Metric::InnerProduct
                    .distance(&index.nodes[center].vector, &index.nodes[node].vector),
            }
        })
        .collect();
    let neighbors = index.select_neighbors(&candidates, 2, center);
    assert_eq!(neighbors, vec![index.labels[&1], index.labels[&3]]);
    // Search still ranks by inner product, including the large-norm vectors.
    let results = index.search(&[1.0, 0.0], 2, Some(4))?;
    assert_eq!(
        results.neighbors.iter().map(|n| n.label).collect::<Vec<_>>(),
        vec![1, 2]
    );
    Ok(())
}
