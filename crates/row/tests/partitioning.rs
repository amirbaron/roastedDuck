//! Radix partitioning invariants.
#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use std::sync::Arc;

use common::{
    Rng, TestValue, buffer_manager, build_vector, random_values, read_rows, spilling_buffer_manager,
};
use rustdb_buffer::{BufferManager, MemoryTag};
use rustdb_row::hash::hash_vector;
use rustdb_row::{PartitionedTupleData, PinProperties, RadixPartitioning, TupleDataLayout, ValidityType};
use rustdb_types::{DataChunk, LogicalType, Vector};

fn types() -> Vec<LogicalType> {
    vec![LogicalType::BIGINT, LogicalType::VARCHAR, LogicalType::UBIGINT]
}

/// Chunks of random keys and strings with the key's hash as the last column.
fn chunks(count: usize, seed: u64) -> Vec<DataChunk> {
    let mut rng = Rng::new(seed);
    let mut chunks = Vec::new();
    let mut remaining = count;
    while remaining > 0 {
        let n = remaining.min(2048);
        remaining -= n;
        let keys = build_vector(
            &LogicalType::BIGINT,
            &random_values(&LogicalType::BIGINT, n, &mut rng, 5),
        );
        let strings = build_vector(
            &LogicalType::VARCHAR,
            &random_values(&LogicalType::VARCHAR, n, &mut rng, 5),
        );
        let mut hashes = Vector::flat(LogicalType::UBIGINT, n);
        hash_vector(&keys, n, &mut hashes).unwrap();
        chunks.push(DataChunk::new(vec![keys, strings, hashes], n));
    }
    chunks
}

fn partitioned(manager: &BufferManager, radix_bits: usize) -> PartitionedTupleData {
    let layout = Arc::new(TupleDataLayout::new(types(), ValidityType::CanHaveNull).unwrap());
    PartitionedTupleData::new(manager.clone(), layout, MemoryTag::HashTable, radix_bits, 2).unwrap()
}

fn hash_of(row: &[TestValue]) -> u64 {
    match &row[2] {
        TestValue::Fixed(bytes) => u64::from_ne_bytes(bytes.as_slice().try_into().unwrap()),
        other => panic!("hash column holds {other:?}"),
    }
}

/// Every row sits in the partition its hash selects; returns all rows, sorted.
fn check_partitions(data: &PartitionedTupleData) -> Vec<String> {
    let mut all = Vec::new();
    for (index, partition) in data.partitions().iter().enumerate() {
        for row in read_rows(&partition.to_chunks().unwrap()) {
            assert_eq!(
                RadixPartitioning::partition_index(hash_of(&row), data.radix_bits()),
                index
            );
            all.push(format!("{row:?}"));
        }
    }
    all.sort();
    all
}

fn expected_rows(chunks: &[DataChunk]) -> Vec<String> {
    let mut rows: Vec<String> = read_rows(chunks).iter().map(|row| format!("{row:?}")).collect();
    rows.sort();
    rows
}

#[test]
fn rows_land_in_their_hash_partition() {
    let manager = buffer_manager();
    let input = chunks(20_000, 1);
    let mut data = partitioned(&manager, 4);
    let mut state = data.initialize_append_state(PinProperties::UnpinAfterDone);
    for chunk in &input {
        data.append(&mut state, chunk).unwrap();
    }
    data.flush_append_state(&mut state);
    assert_eq!(data.count(), 20_000);
    assert_eq!(data.partition_count(), 16);
    assert!(data.partitions().iter().all(|partition| partition.count() > 0));
    assert_eq!(check_partitions(&data), expected_rows(&input));
}

#[test]
fn reverse_selection_locates_each_input_row() {
    let manager = buffer_manager();
    let input = chunks(2048, 2);
    let mut data = partitioned(&manager, 3);
    let mut state = data.initialize_append_state(PinProperties::KeepEverythingPinned);
    data.append(&mut state, &input[0]).unwrap();
    let keys = &input[0].columns()[0];
    let locations = state.chunk_state.row_locations().to_vec();
    for row in 0..2048 {
        let location = locations[state.reverse_partition_sel()[row] as usize];
        // Column 0 (BIGINT) starts after one validity byte and the 4-byte heap size.
        // SAFETY: the rows are pinned (KeepEverythingPinned).
        let stored = unsafe { location.as_ptr().add(5).cast::<i64>().read_unaligned() };
        if let TestValue::Fixed(bytes) = common::read_value(keys, row) {
            assert_eq!(stored, i64::from_ne_bytes(bytes.as_slice().try_into().unwrap()));
        }
    }
    data.flush_append_state(&mut state);
}

#[test]
fn thread_local_partitions_combine() {
    let manager = buffer_manager();
    let inputs: Vec<Vec<DataChunk>> = (0..4).map(|seed| chunks(6000, 10 + seed)).collect();
    let locals: Vec<PartitionedTupleData> = std::thread::scope(|scope| {
        let handles: Vec<_> = inputs
            .iter()
            .map(|input| {
                let manager = manager.clone();
                scope.spawn(move || {
                    let mut local = partitioned(&manager, 5);
                    let mut state = local.initialize_append_state(PinProperties::UnpinAfterDone);
                    for chunk in input {
                        local.append(&mut state, chunk).unwrap();
                    }
                    local.flush_append_state(&mut state);
                    local
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().unwrap()).collect()
    });
    let mut global = partitioned(&manager, 5);
    for local in locals {
        global.combine(local).unwrap();
    }
    assert_eq!(global.count(), 24_000);
    let all: Vec<DataChunk> = inputs.into_iter().flatten().collect();
    assert_eq!(check_partitions(&global), expected_rows(&all));
}

#[test]
fn repartitioning_refines_partitions_and_frees_the_source() {
    let manager = spilling_buffer_manager("repartition", 16 << 20);
    let input = chunks(50_000, 3);
    let mut coarse = partitioned(&manager, 2);
    let mut state = coarse.initialize_append_state(PinProperties::UnpinAfterDone);
    for chunk in &input {
        coarse.append(&mut state, chunk).unwrap();
    }
    coarse.flush_append_state(&mut state);
    let mut fine = coarse.with_radix_bits(5).unwrap();
    coarse.repartition(&mut fine).unwrap();
    assert_eq!(coarse.count(), 0);
    assert!(coarse.partitions().iter().all(|partition| partition.count() == 0));
    assert_eq!(fine.count(), 50_000);
    assert_eq!(check_partitions(&fine), expected_rows(&input));

    // Back to one collection.
    let all = fine.take_unpartitioned().unwrap();
    assert_eq!(all.count(), 50_000);
    let mut rows: Vec<String> = read_rows(&all.to_chunks().unwrap())
        .iter()
        .map(|row| format!("{row:?}"))
        .collect();
    rows.sort();
    assert_eq!(rows, expected_rows(&input));
    assert_eq!(fine.count(), 0);
}

#[test]
fn radix_select_splits_by_partition() {
    let input = chunks(1000, 4);
    let hashes = &input[0].columns()[2];
    let mut wanted = vec![false; 8];
    wanted[3] = true;
    wanted[5] = true;
    let mut true_sel = Vec::new();
    let mut false_sel = Vec::new();
    let selected = RadixPartitioning::select(
        hashes,
        None,
        1000,
        3,
        &wanted,
        Some(&mut true_sel),
        Some(&mut false_sel),
    );
    assert_eq!(selected, true_sel.len());
    assert_eq!(true_sel.len() + false_sel.len(), 1000);
    let rows = read_rows(std::slice::from_ref(&input[0]));
    for &row in &true_sel {
        let partition = RadixPartitioning::partition_index(hash_of(&rows[row as usize]), 3);
        assert!(partition == 3 || partition == 5);
    }
}
