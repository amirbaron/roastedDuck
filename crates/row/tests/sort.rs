//! Sorting: whole pipelines (sink, runs, merge, decode, payload gather) compared against
//! a reference sort of the same data, for all ORDER BY modifiers, nested types, many
//! threads, external (spilling) sorts and the window sort strategies.
#![allow(clippy::unwrap_used)]

mod common;

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use common::{
    Rng, TestValue, all_types, buffer_manager, build_chunks, read_rows, spilling_buffer_manager, struct_type,
};
use rustdb_buffer::BufferManager;
use rustdb_row::sort::{
    NullOrder, OrderModifiers, OrderType, Sort, SortColumn, SortConfig, SortSource, SortStrategy,
    StrategyGlobalState, create_sort_keys,
};
use rustdb_types::{DataChunk, LogicalType};

const ALL_MODIFIERS: [OrderModifiers; 4] = [
    OrderModifiers::new(OrderType::Ascending, NullOrder::NullsFirst),
    OrderModifiers::new(OrderType::Ascending, NullOrder::NullsLast),
    OrderModifiers::new(OrderType::Descending, NullOrder::NullsFirst),
    OrderModifiers::new(OrderType::Descending, NullOrder::NullsLast),
];

fn order(column: usize, modifiers: OrderModifiers) -> SortColumn {
    SortColumn {
        column,
        modifiers,
        stats: None,
    }
}

fn random_rows(types: &[LogicalType], count: usize, rng: &mut Rng, null_percent: u64) -> Vec<Vec<TestValue>> {
    (0..count)
        .map(|_| {
            types
                .iter()
                .map(|ty| common::random_value(ty, rng, null_percent))
                .collect()
        })
        .collect()
}

/// The sort keys of `rows` for `orders` (the byte encoding verified against DuckDB).
fn keys_of(types: &[LogicalType], rows: &[Vec<TestValue>], orders: &[SortColumn]) -> Vec<Vec<u8>> {
    let mut keys = Vec::with_capacity(rows.len());
    for chunk in build_chunks(types, rows, 2048) {
        let columns: Vec<_> = orders.iter().map(|order| chunk.column(order.column)).collect();
        let modifiers: Vec<_> = orders.iter().map(|order| order.modifiers).collect();
        let buffer = create_sort_keys(&columns, &modifiers, chunk.len()).unwrap();
        keys.extend((0..chunk.len()).map(|row| buffer.key(row).to_vec()));
    }
    keys
}

/// Sorts rows into a canonical order (to compare multisets).
fn canonical(rows: &mut [Vec<TestValue>]) {
    rows.sort_by_cached_key(|row| format!("{row:?}"));
}

/// The value a key column holds after the sort-key round trip: like DuckDB, keys are
/// decoded from their normalized form, which folds -0.0 into 0.0 and every NaN into one.
fn normalize_key(ty: &LogicalType, value: &mut TestValue) {
    use rustdb_types::{PhysicalType, TypeInfo};
    match (ty.physical_type(), value) {
        (PhysicalType::Double, TestValue::Fixed(bytes)) => {
            let x = f64::from_ne_bytes(bytes[..8].try_into().unwrap());
            let x = if x.is_nan() {
                f64::NAN
            } else if x == 0.0 {
                0.0
            } else {
                x
            };
            *bytes = x.to_ne_bytes().to_vec();
        }
        (PhysicalType::Float, TestValue::Fixed(bytes)) => {
            let x = f32::from_ne_bytes(bytes[..4].try_into().unwrap());
            let x = if x.is_nan() {
                f32::NAN
            } else if x == 0.0 {
                0.0
            } else {
                x
            };
            *bytes = x.to_ne_bytes().to_vec();
        }
        (_, TestValue::List(values)) => {
            let child = match ty.info() {
                Some(TypeInfo::List { child } | TypeInfo::Array { child, .. }) => child.clone(),
                _ => return,
            };
            for value in values.iter_mut() {
                normalize_key(&child, value);
            }
        }
        (_, TestValue::Struct(values)) => {
            if let Some(TypeInfo::Struct { children }) = ty.info() {
                for ((_, child), value) in children.iter().zip(values) {
                    normalize_key(child, value);
                }
            }
        }
        _ => {}
    }
}

fn normalize_keys(types: &[LogicalType], rows: &mut [Vec<TestValue>], orders: &[SortColumn]) {
    for row in rows {
        for order in orders {
            normalize_key(&types[order.column], &mut row[order.column]);
        }
    }
}

/// Checks that `output` is a permutation of `input` whose keys are non-decreasing.
fn assert_sorted(
    types: &[LogicalType],
    input: &[Vec<TestValue>],
    output: &[Vec<TestValue>],
    orders: &[SortColumn],
) {
    assert_eq!(output.len(), input.len(), "row count");
    let keys = keys_of(types, output, orders);
    for (i, pair) in keys.windows(2).enumerate() {
        assert!(
            pair[0] <= pair[1],
            "rows {i} and {} out of order: {:?} / {:?}",
            i + 1,
            output[i],
            output[i + 1]
        );
    }
    let mut expected = input.to_vec();
    let mut actual = output.to_vec();
    normalize_keys(types, &mut expected, orders);
    normalize_keys(types, &mut actual, orders);
    canonical(&mut expected);
    canonical(&mut actual);
    assert!(expected == actual, "output is not a permutation of the input");
}

/// Runs a sort over `chunks` with `threads` sinking and producing threads.
fn run_sort(sort: &Sort, chunks: &[DataChunk], config: SortConfig) -> Option<SortSource> {
    let threads = config.threads.max(1);
    let global = sort.global_sink_state(config);
    std::thread::scope(|scope| {
        for thread in 0..threads {
            let global = &global;
            scope.spawn(move || {
                let mut local = sort.local_sink_state();
                for chunk in chunks.iter().skip(thread).step_by(threads) {
                    sort.sink(chunk, global, &mut local).unwrap();
                }
                sort.combine(global, local).unwrap();
            });
        }
    });
    sort.finalize(global).unwrap()
}

/// Every row of `source`, produced by `threads` threads and put back in partition order.
fn parallel_output(source: &SortSource, threads: usize) -> Vec<Vec<TestValue>> {
    let mut parts: Vec<(usize, usize, Vec<Vec<TestValue>>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut local = source.local_state();
                    let mut parts = Vec::new();
                    let mut sequence = 0;
                    loop {
                        let mut chunk = DataChunk::default();
                        if !source.get_data(&mut chunk, &mut local).unwrap() {
                            break;
                        }
                        let partition = local.partition_index().unwrap();
                        parts.push((partition, sequence, read_rows(&[chunk])));
                        sequence += 1;
                    }
                    parts
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect()
    });
    parts.sort_by_key(|(partition, sequence, _)| (*partition, *sequence));
    parts.into_iter().flat_map(|(_, _, rows)| rows).collect()
}

fn sort_rows(
    buffer_manager: &BufferManager,
    types: &[LogicalType],
    rows: &[Vec<TestValue>],
    orders: &[SortColumn],
    config: SortConfig,
) -> Vec<Vec<TestValue>> {
    let threads = config.threads.max(1);
    let sort = Sort::new(buffer_manager.clone(), orders.to_vec(), types, None).unwrap();
    let chunks = build_chunks(types, rows, 2048);
    match run_sort(&sort, &chunks, config) {
        Some(source) => {
            assert_eq!(source.count(), rows.len());
            parallel_output(&source, threads)
        }
        None => Vec::new(),
    }
}

fn config(threads: usize) -> SortConfig {
    SortConfig {
        threads,
        ..SortConfig::default()
    }
}

#[test]
fn integers_with_every_modifier() {
    let bm = buffer_manager();
    let types = [LogicalType::BIGINT, LogicalType::INTEGER];
    let mut rng = Rng::new(1);
    let rows = random_rows(&types, 20_000, &mut rng, 10);
    for modifiers in ALL_MODIFIERS {
        let orders = [order(0, modifiers)];
        let output = sort_rows(&bm, &types, &rows, &orders, config(1));
        assert_sorted(&types, &rows, &output, &orders);
    }
}

/// An independent comparator for BIGINT/VARCHAR/DOUBLE, so the encoding itself is checked
/// too (not only the pipeline against the encoding).
#[test]
fn scalar_order_matches_an_independent_comparator() {
    fn value_order(ty: &LogicalType, a: &TestValue, b: &TestValue) -> Ordering {
        match (a, b) {
            (TestValue::Fixed(a), TestValue::Fixed(b)) if *ty == LogicalType::BIGINT => {
                i64::from_ne_bytes(a[..8].try_into().unwrap())
                    .cmp(&i64::from_ne_bytes(b[..8].try_into().unwrap()))
            }
            (TestValue::Fixed(a), TestValue::Fixed(b)) => {
                let a = f64::from_ne_bytes(a[..8].try_into().unwrap());
                let b = f64::from_ne_bytes(b[..8].try_into().unwrap());
                // DuckDB: NaN is the largest value, -0.0 == 0.0.
                match (a.is_nan(), b.is_nan()) {
                    (true, true) => Ordering::Equal,
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    (false, false) => a.partial_cmp(&b).unwrap(),
                }
            }
            (TestValue::Str(a), TestValue::Str(b)) => a.cmp(b),
            _ => unreachable!(),
        }
    }
    let bm = buffer_manager();
    let types = [LogicalType::BIGINT, LogicalType::VARCHAR, LogicalType::DOUBLE];
    let mut rng = Rng::new(2);
    let mut rows = random_rows(&types, 6000, &mut rng, 15);
    // Small domains so later keys break ties; a few special doubles.
    for (i, row) in rows.iter_mut().enumerate() {
        if let TestValue::Fixed(bytes) = &mut row[0] {
            *bytes = ((i64::from_ne_bytes(bytes[..8].try_into().unwrap()) % 7) - 3)
                .to_ne_bytes()
                .to_vec();
        }
        if let TestValue::Str(bytes) = &mut row[1] {
            bytes.truncate(1 + i % 3);
        }
        if let TestValue::Fixed(bytes) = &mut row[2] {
            let special = [f64::NAN, -0.0, 0.0, f64::INFINITY, f64::NEG_INFINITY, -1.5, 2.25];
            *bytes = special[i % special.len()].to_ne_bytes().to_vec();
        }
    }
    let mut rng = Rng::new(3);
    for _ in 0..8 {
        let orders: Vec<SortColumn> = (0..3)
            .map(|column| order(column, ALL_MODIFIERS[rng.below(4) as usize]))
            .collect();
        let output = sort_rows(&bm, &types, &rows, &orders, config(2));
        assert_eq!(output.len(), rows.len());
        for pair in output.windows(2) {
            let mut ordering = Ordering::Equal;
            for order in &orders {
                let (a, b) = (&pair[0][order.column], &pair[1][order.column]);
                let column_order = match (a, b) {
                    (TestValue::Null, TestValue::Null) => Ordering::Equal,
                    (TestValue::Null, _) => match order.modifiers.nulls {
                        NullOrder::NullsFirst => Ordering::Less,
                        NullOrder::NullsLast => Ordering::Greater,
                    },
                    (_, TestValue::Null) => match order.modifiers.nulls {
                        NullOrder::NullsFirst => Ordering::Greater,
                        NullOrder::NullsLast => Ordering::Less,
                    },
                    _ => {
                        let ordering = value_order(&types[order.column], a, b);
                        match order.modifiers.order {
                            OrderType::Ascending => ordering,
                            OrderType::Descending => ordering.reverse(),
                        }
                    }
                };
                if column_order != Ordering::Equal {
                    ordering = column_order;
                    break;
                }
            }
            assert_ne!(
                ordering,
                Ordering::Greater,
                "{:?} before {:?} for {orders:?}",
                pair[0],
                pair[1]
            );
        }
    }
}

/// Several fixed-width keys with NULLs: a NULL has no value bytes, so later columns move
/// within the key.
#[test]
fn fixed_width_multi_key_with_nulls() {
    let bm = buffer_manager();
    let types = [
        LogicalType::INTEGER,
        LogicalType::SMALLINT,
        LogicalType::DOUBLE,
        LogicalType::VARCHAR,
    ];
    let mut rng = Rng::new(17);
    let mut rows = random_rows(&types, 12_000, &mut rng, 25);
    for row in &mut rows {
        if let TestValue::Fixed(bytes) = &mut row[0] {
            *bytes = (i32::from_ne_bytes(bytes[..4].try_into().unwrap()) % 5)
                .to_ne_bytes()
                .to_vec();
        }
    }
    for (i, first) in ALL_MODIFIERS.iter().enumerate() {
        let orders = [
            order(0, *first),
            order(1, ALL_MODIFIERS[(i + 1) % 4]),
            order(2, ALL_MODIFIERS[(i + 2) % 4]),
        ];
        let output = sort_rows(&bm, &types, &rows, &orders, config(2));
        assert_sorted(&types, &rows, &output, &orders);
    }
}

#[test]
fn strings_short_and_long() {
    let bm = buffer_manager();
    let types = [LogicalType::VARCHAR, LogicalType::BIGINT];
    let mut rng = Rng::new(4);
    let mut rows = random_rows(&types, 30_000, &mut rng, 5);
    // Shared prefixes make the radix sort recurse past the inline part.
    for (i, row) in rows.iter_mut().enumerate() {
        if let TestValue::Str(bytes) = &mut row[0]
            && i % 2 == 0
        {
            let mut prefixed = b"common-prefix-".to_vec();
            prefixed.append(bytes);
            *bytes = prefixed;
        }
    }
    for modifiers in ALL_MODIFIERS {
        let orders = [order(0, modifiers)];
        let output = sort_rows(&bm, &types, &rows, &orders, config(4));
        assert_sorted(&types, &rows, &output, &orders);
    }
}

#[test]
fn multi_key_mixed_modifiers() {
    let bm = buffer_manager();
    let types = [
        LogicalType::SMALLINT,
        LogicalType::VARCHAR,
        LogicalType::DOUBLE,
        LogicalType::BIGINT,
        LogicalType::INTERVAL,
    ];
    let mut rng = Rng::new(5);
    let mut rows = random_rows(&types, 15_000, &mut rng, 10);
    for row in &mut rows {
        if let TestValue::Fixed(bytes) = &mut row[0] {
            bytes[1] = 0;
            bytes[0] %= 4;
        }
        if let TestValue::Str(bytes) = &mut row[1] {
            bytes.truncate(2);
        }
    }
    for (i, a) in ALL_MODIFIERS.iter().enumerate() {
        let orders = [
            order(0, *a),
            order(1, ALL_MODIFIERS[(i + 1) % 4]),
            order(2, ALL_MODIFIERS[(i + 2) % 4]),
            order(4, ALL_MODIFIERS[(i + 3) % 4]),
        ];
        let output = sort_rows(&bm, &types, &rows, &orders, config(3));
        assert_sorted(&types, &rows, &output, &orders);
    }
}

#[test]
fn every_type_as_key_and_payload() {
    let bm = buffer_manager();
    let types = all_types();
    let mut rng = Rng::new(6);
    let rows = random_rows(&types, 3000, &mut rng, 15);
    for column in 0..types.len() {
        let modifiers = ALL_MODIFIERS[column % 4];
        let orders = [order(column, modifiers)];
        let output = sort_rows(&bm, &types, &rows, &orders, config(2));
        assert_sorted(&types, &rows, &output, &orders);
    }
}

#[test]
fn nested_keys_with_ties() {
    // Small element domains so that many lists/structs compare equal or share prefixes.
    fn shrink(value: &mut TestValue) {
        match value {
            TestValue::Fixed(bytes) => {
                for byte in bytes.iter_mut().skip(1) {
                    *byte = 0;
                }
                bytes[0] %= 3;
            }
            TestValue::Str(bytes) => bytes.truncate(1),
            TestValue::List(values) | TestValue::Struct(values) => values.iter_mut().for_each(shrink),
            TestValue::Null => {}
        }
    }
    let bm = buffer_manager();
    let point = struct_type(&[("x", LogicalType::INTEGER), ("label", LogicalType::VARCHAR)]);
    let types = [
        LogicalType::list(LogicalType::INTEGER),
        point,
        LogicalType::list(LogicalType::VARCHAR),
        LogicalType::BIGINT,
    ];
    let mut rng = Rng::new(7);
    let mut rows = random_rows(&types, 8000, &mut rng, 10);
    for row in &mut rows {
        for value in &mut row[..3] {
            shrink(value);
        }
    }
    for modifiers in ALL_MODIFIERS {
        let orders = [
            order(0, modifiers),
            order(1, modifiers),
            order(2, ALL_MODIFIERS[0]),
        ];
        let output = sort_rows(&bm, &types, &rows, &orders, config(2));
        assert_sorted(&types, &rows, &output, &orders);
    }
}

#[test]
fn projection_decodes_keys_and_gathers_payload() {
    let bm = buffer_manager();
    let types = [
        LogicalType::VARCHAR,
        LogicalType::INTEGER,
        LogicalType::list(LogicalType::VARCHAR),
    ];
    let mut rng = Rng::new(8);
    let rows = random_rows(&types, 5000, &mut rng, 10);
    let orders = [order(1, ALL_MODIFIERS[2]), order(0, ALL_MODIFIERS[1])];
    // Output column 1 (INTEGER) twice and the list; column 0 only as a key.
    let sort = Sort::new(bm.clone(), orders.to_vec(), &types, Some(vec![2, 1, 1])).unwrap();
    assert_eq!(
        sort.output_types(),
        &[types[2].clone(), types[1].clone(), types[1].clone()]
    );
    let chunks = build_chunks(&types, &rows, 2048);
    let source = run_sort(&sort, &chunks, config(2)).unwrap();
    let output = parallel_output(&source, 2);
    // Compare against the full sort's projection.
    let full = sort_rows(&bm, &types, &rows, &orders, config(1));
    let keys_out: Vec<_> = output.iter().map(|row| row[1].clone()).collect();
    let keys_full: Vec<_> = full.iter().map(|row| row[1].clone()).collect();
    assert_eq!(keys_out, keys_full);
    let mut projected: Vec<_> = rows
        .iter()
        .map(|row| vec![row[2].clone(), row[1].clone(), row[1].clone()])
        .collect();
    let mut actual = output.clone();
    canonical(&mut projected);
    canonical(&mut actual);
    assert_eq!(projected, actual);
}

#[test]
fn many_threads_and_small_merge_partitions() {
    let bm = buffer_manager();
    let types = [LogicalType::BIGINT, LogicalType::VARCHAR];
    let mut rng = Rng::new(9);
    let rows = random_rows(&types, 50_000, &mut rng, 5);
    let orders = [order(0, ALL_MODIFIERS[1])];
    let output = sort_rows(
        &bm,
        &types,
        &rows,
        &orders,
        SortConfig {
            threads: 8,
            verify_parallelism: true,
            ..SortConfig::default()
        },
    );
    assert_sorted(&types, &rows, &output, &orders);
}

#[test]
fn empty_input_has_no_output() {
    let bm = buffer_manager();
    let types = [LogicalType::INTEGER];
    let sort = Sort::new(bm, vec![order(0, ALL_MODIFIERS[0])], &types, None).unwrap();
    assert!(run_sort(&sort, &[], config(2)).is_none());
}

#[test]
fn force_external_sort() {
    let bm = buffer_manager();
    let types = [
        LogicalType::INTEGER,
        LogicalType::VARCHAR,
        LogicalType::list(LogicalType::BIGINT),
    ];
    let mut rng = Rng::new(10);
    let rows = random_rows(&types, 40_000, &mut rng, 5);
    let orders = [order(1, ALL_MODIFIERS[3]), order(0, ALL_MODIFIERS[0])];
    let output = sort_rows(
        &bm,
        &types,
        &rows,
        &orders,
        SortConfig {
            threads: 4,
            force_external: true,
            ..SortConfig::default()
        },
    );
    assert_sorted(&types, &rows, &output, &orders);
}

#[test]
fn spills_under_a_small_memory_limit() {
    // Like DuckDB, an external merge pins about a row group (122880 rows) of keys and
    // payload per thread (~28 MiB here), so the limit must leave room for two threads.
    let bm = spilling_buffer_manager("sort-spill", 80 << 20);
    let types = [LogicalType::BIGINT, LogicalType::VARCHAR, LogicalType::VARCHAR];
    let mut rng = Rng::new(11);
    // ~110 MiB of row data (long strings live in the heaps).
    let rows: Vec<Vec<TestValue>> = (0..400_000)
        .map(|_| {
            vec![
                TestValue::Fixed(rng.next_u64().to_ne_bytes().to_vec()),
                TestValue::Str(
                    (0..20 + rng.below(60))
                        .map(|_| b'a' + rng.below(26) as u8)
                        .collect(),
                ),
                TestValue::Str((0..80).map(|_| b'a' + rng.below(26) as u8).collect()),
            ]
        })
        .collect();
    let orders = [order(1, ALL_MODIFIERS[0]), order(0, ALL_MODIFIERS[2])];
    let sort = Sort::new(bm.clone(), orders.to_vec(), &types, None).unwrap();
    let chunks = build_chunks(&types, &rows, 2048);
    let source = run_sort(&sort, &chunks, config(2)).unwrap();
    assert!(
        source.is_external(),
        "a 110 MiB sort under 80 MiB must go external"
    );
    assert!(bm.used_swap() > 0, "the runs must have spilled");
    let output = parallel_output(&source, 2);
    assert_sorted(&types, &rows, &output, &orders);
}

#[test]
fn materialized_sorted_run() {
    let bm = buffer_manager();
    let types = [LogicalType::INTEGER, LogicalType::VARCHAR];
    let mut rng = Rng::new(12);
    let rows = random_rows(&types, 10_000, &mut rng, 5);
    let sort = Sort::new(bm, vec![order(0, ALL_MODIFIERS[0])], &types, None).unwrap();
    let chunks = build_chunks(&types, &rows, 2048);
    let source = run_sort(
        &sort,
        &chunks,
        SortConfig {
            threads: 3,
            verify_parallelism: true,
            ..SortConfig::default()
        },
    )
    .unwrap();
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| {
                let mut local = source.local_state();
                source.materialize(&mut local).unwrap();
            });
        }
    });
    let run = source.take_sorted_run().unwrap().unwrap();
    assert_eq!(run.count(), rows.len());
}

fn strategy_rows(strategy: &SortStrategy, global: &StrategyGlobalState) -> Vec<Vec<Vec<TestValue>>> {
    let groups = strategy.hash_groups(global);
    std::thread::scope(|scope| {
        for group in 0..groups.len() {
            scope.spawn(move || strategy.sort_hash_group(global, group).unwrap());
        }
    });
    (0..groups.len())
        .filter_map(|group| strategy.group_rows(global, group).unwrap())
        .map(|rows| read_rows(&rows.to_chunks().unwrap()))
        .collect()
}

fn sink_strategy(strategy: &SortStrategy, chunks: &[DataChunk], threads: usize) -> StrategyGlobalState {
    let mut global = strategy.global_state(config(threads)).unwrap();
    std::thread::scope(|scope| {
        for thread in 0..threads {
            let global = &global;
            scope.spawn(move || {
                let mut local = strategy.local_state();
                for chunk in chunks.iter().skip(thread).step_by(threads) {
                    strategy.sink(chunk, global, &mut local).unwrap();
                }
                strategy.combine(global, local).unwrap();
            });
        }
    });
    strategy.finalize(&mut global).unwrap();
    global
}

#[test]
fn hashed_sort_groups_partitions_and_sorts_them() {
    let bm = buffer_manager();
    let types = [LogicalType::INTEGER, LogicalType::VARCHAR, LogicalType::BIGINT];
    let mut rng = Rng::new(13);
    let mut rows = random_rows(&types, 30_000, &mut rng, 5);
    for row in &mut rows {
        if let TestValue::Fixed(bytes) = &mut row[0] {
            *bytes = (i32::from_ne_bytes(bytes[..4].try_into().unwrap()) % 50)
                .to_ne_bytes()
                .to_vec();
        }
    }
    let orders = vec![order(2, ALL_MODIFIERS[3])];
    let strategy =
        SortStrategy::new(bm, vec![0], &[None], orders, types.to_vec(), rows.len(), false).unwrap();
    let chunks = build_chunks(&types, &rows, 2048);
    let global = sink_strategy(&strategy, &chunks, 4);
    let groups = strategy_rows(&strategy, &global);
    assert!(groups.len() > 1, "expected several hash groups");
    // The effective order: partition column ASC NULLS FIRST, then the ORDER BY.
    let effective = [order(0, ALL_MODIFIERS[0]), order(2, ALL_MODIFIERS[3])];
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut all = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        let keys = keys_of(&types, group, &effective);
        for (i, pair) in keys.windows(2).enumerate() {
            assert!(
                pair[0] <= pair[1],
                "group {index} is not sorted at {i}: {:?} / {:?}",
                group[i],
                group[i + 1]
            );
        }
        for row in group {
            let partition = format!("{:?}", row[0]);
            assert_eq!(
                *seen.entry(partition).or_insert(index),
                index,
                "a partition value spans groups"
            );
        }
        all.extend(group.iter().cloned());
    }
    let mut expected = rows.clone();
    canonical(&mut expected);
    canonical(&mut all);
    assert_eq!(expected, all);
}

#[test]
fn full_sort_strategy_with_forced_payload() {
    let bm = buffer_manager();
    let types = [LogicalType::BIGINT];
    let mut rng = Rng::new(14);
    let rows = random_rows(&types, 9000, &mut rng, 10);
    let orders = vec![order(0, ALL_MODIFIERS[2])];
    let strategy = SortStrategy::new(
        bm,
        Vec::new(),
        &[],
        orders.clone(),
        types.to_vec(),
        rows.len(),
        true,
    )
    .unwrap();
    let chunks = build_chunks(&types, &rows, 2048);
    let global = sink_strategy(&strategy, &chunks, 2);
    let groups = strategy_rows(&strategy, &global);
    assert_eq!(groups.len(), 1);
    // The forced payload column is a NULL BOOLEAN after the input.
    assert!(
        groups[0]
            .iter()
            .all(|row| row.len() == 2 && row[1] == TestValue::Null)
    );
    let output: Vec<_> = groups[0].iter().map(|row| vec![row[0].clone()]).collect();
    assert_sorted(&types, &rows, &output, &orders);
}

#[test]
fn natural_sort_keeps_every_row() {
    let bm = buffer_manager();
    let types = [LogicalType::INTEGER, LogicalType::VARCHAR];
    let mut rng = Rng::new(15);
    let rows = random_rows(&types, 7000, &mut rng, 10);
    let strategy =
        SortStrategy::new(bm, Vec::new(), &[], Vec::new(), types.to_vec(), rows.len(), false).unwrap();
    let chunks = build_chunks(&types, &rows, 2048);
    let global = sink_strategy(&strategy, &chunks, 1);
    assert!(strategy.sorted_run(&global, 0).is_err());
    let groups = strategy_rows(&strategy, &global);
    assert_eq!(groups.len(), 1);
    // A single sinking thread keeps the input order.
    assert_eq!(groups[0], rows);
}

#[test]
fn strategy_sorted_runs() {
    let bm = buffer_manager();
    let types = [LogicalType::INTEGER, LogicalType::DOUBLE];
    let mut rng = Rng::new(16);
    let rows = random_rows(&types, 12_000, &mut rng, 5);
    let strategy = SortStrategy::new(
        bm,
        vec![0],
        &[None],
        vec![order(1, ALL_MODIFIERS[0])],
        types.to_vec(),
        rows.len(),
        false,
    )
    .unwrap();
    let chunks = build_chunks(&types, &rows, 2048);
    let global = Arc::new(sink_strategy(&strategy, &chunks, 3));
    let groups = strategy.hash_groups(&global);
    let mut total = 0;
    for group in 0..groups.len() {
        strategy.sort_hash_group(&global, group).unwrap();
        if let Some(run) = strategy.sorted_run(&global, group).unwrap() {
            total += run.count();
        }
    }
    assert_eq!(total, rows.len());
}
