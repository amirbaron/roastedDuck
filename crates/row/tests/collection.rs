//! Scatter → gather round trips and collection mechanics.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use common::{
    Rng, TestValue, all_types, buffer_manager, build_chunks, build_vector, random_values, read_rows,
    read_vector, spilling_buffer_manager,
};
use roastedduck_buffer::MemoryTag;
use roastedduck_row::{ChunkIterator, PinProperties, TupleDataCollection, TupleDataLayout, ValidityType};
use roastedduck_types::{DataChunk, LogicalType, SelectionVector, Value, Vector};

fn collection(types: &[LogicalType]) -> TupleDataCollection {
    let layout = TupleDataLayout::new(types.to_vec(), ValidityType::CanHaveNull).unwrap();
    TupleDataCollection::new(buffer_manager(), Arc::new(layout), MemoryTag::HashTable).unwrap()
}

fn random_rows(types: &[LogicalType], count: usize, seed: u64, null_percent: u64) -> Vec<Vec<TestValue>> {
    let mut rng = Rng::new(seed);
    let columns: Vec<Vec<TestValue>> = types
        .iter()
        .map(|ty| random_values(ty, count, &mut rng, null_percent))
        .collect();
    (0..count)
        .map(|row| columns.iter().map(|column| column[row].clone()).collect())
        .collect()
}

fn append_all(collection: &mut TupleDataCollection, chunks: &[DataChunk], properties: PinProperties) {
    let mut state = collection.initialize_append(properties);
    for chunk in chunks {
        collection.append(&mut state, chunk).unwrap();
    }
    collection.finalize_pin_state(&mut state.pin_state);
}

#[test]
fn every_type_round_trips_with_nulls() {
    for ty in all_types() {
        let types = vec![ty.clone()];
        let rows = random_rows(&types, 5000, 7, 20);
        let mut rows_collection = collection(&types);
        append_all(
            &mut rows_collection,
            &build_chunks(&types, &rows, 1500),
            PinProperties::UnpinAfterDone,
        );
        assert_eq!(rows_collection.count(), 5000, "{ty}");
        let scanned = read_rows(&rows_collection.to_chunks().unwrap());
        assert_eq!(scanned, rows, "round trip of {ty}");
    }
}

#[test]
fn all_types_together_round_trip() {
    let types = all_types();
    let rows = random_rows(&types, 3000, 11, 15);
    let mut rows_collection = collection(&types);
    append_all(
        &mut rows_collection,
        &build_chunks(&types, &rows, 2048),
        PinProperties::KeepEverythingPinned,
    );
    assert_eq!(rows_collection.chunk_count(), 2);
    assert_eq!(read_rows(&rows_collection.to_chunks().unwrap()), rows);
}

#[test]
fn no_null_layouts_round_trip() {
    let types = vec![
        LogicalType::BIGINT,
        LogicalType::VARCHAR,
        LogicalType::list(LogicalType::INTEGER),
    ];
    let rows = random_rows(&types, 3000, 3, 0);
    let layout = TupleDataLayout::new(types.clone(), ValidityType::CannotHaveNull).unwrap();
    let mut rows_collection =
        TupleDataCollection::new(buffer_manager(), Arc::new(layout), MemoryTag::HashTable).unwrap();
    append_all(
        &mut rows_collection,
        &build_chunks(&types, &rows, 2048),
        PinProperties::UnpinAfterDone,
    );
    assert_eq!(read_rows(&rows_collection.to_chunks().unwrap()), rows);
}

#[test]
fn dictionary_and_constant_inputs() {
    let types = vec![
        LogicalType::INTEGER,
        LogicalType::VARCHAR,
        LogicalType::list(LogicalType::VARCHAR),
    ];
    let rows = random_rows(&types, 100, 5, 20);
    let base = &build_chunks(&types, &rows, 100)[0];
    let selection: Vec<u32> = (0..100).rev().step_by(3).collect();
    let sliced: Vec<Vector> = base
        .columns()
        .iter()
        .map(|column| column.slice(&SelectionVector::new(selection.clone())))
        .collect();
    let chunk = DataChunk::new(sliced, selection.len());
    let mut rows_collection = collection(&types);
    rows_collection.append_chunk(&chunk).unwrap();
    let expected: Vec<_> = selection.iter().map(|&row| rows[row as usize].clone()).collect();
    assert_eq!(read_rows(&rows_collection.to_chunks().unwrap()), expected);

    // Constant vectors repeat their value.
    let constant = DataChunk::new(
        vec![
            Vector::constant(&Value::integer(42)).unwrap(),
            Vector::constant(&Value::varchar("a constant string value")).unwrap(),
            build_vector(&types[2], &[TestValue::List(vec![TestValue::Str(b"x".to_vec())])])
                .slice(&SelectionVector::new(vec![0; 5])),
        ],
        5,
    );
    let mut rows_collection = collection(&types);
    rows_collection.append_chunk(&constant).unwrap();
    let scanned = read_rows(&rows_collection.to_chunks().unwrap());
    assert_eq!(scanned.len(), 5);
    assert!(scanned.iter().all(|row| row == &scanned[0]));
    assert_eq!(scanned[0][1], TestValue::Str(b"a constant string value".to_vec()));
}

#[test]
fn appends_with_a_selection() {
    let types = vec![LogicalType::BIGINT, LogicalType::VARCHAR];
    let rows = random_rows(&types, 2048, 9, 10);
    let chunk = &build_chunks(&types, &rows, 2048)[0];
    let formats = roastedduck_row::ChunkFormat::new(chunk).unwrap();
    let selection: Vec<u32> = (0..2048).filter(|row| row % 7 == 3).collect();
    let mut rows_collection = collection(&types);
    let mut state = rows_collection.initialize_append(PinProperties::UnpinAfterDone);
    rows_collection
        .append_formatted(&mut state, &formats, Some(&selection), selection.len())
        .unwrap();
    rows_collection.finalize_pin_state(&mut state.pin_state);
    let expected: Vec<_> = selection.iter().map(|&row| rows[row as usize].clone()).collect();
    assert_eq!(read_rows(&rows_collection.to_chunks().unwrap()), expected);
}

#[test]
fn strings_larger_than_a_block() {
    let types = vec![LogicalType::VARCHAR, LogicalType::INTEGER];
    let big = TestValue::Str(vec![b'z'; 700_000]);
    let rows = vec![
        vec![
            TestValue::Str(b"small one".to_vec()),
            TestValue::Fixed(1i32.to_ne_bytes().to_vec()),
        ],
        vec![big.clone(), TestValue::Null],
        vec![
            TestValue::Str(vec![b'q'; 300]),
            TestValue::Fixed(3i32.to_ne_bytes().to_vec()),
        ],
        vec![big, TestValue::Fixed(4i32.to_ne_bytes().to_vec())],
    ];
    let mut rows_collection = collection(&types);
    append_all(
        &mut rows_collection,
        &build_chunks(&types, &rows, 4),
        PinProperties::UnpinAfterDone,
    );
    assert_eq!(read_rows(&rows_collection.to_chunks().unwrap()), rows);
}

#[test]
fn many_chunks_span_blocks_and_parts() {
    // Wide rows with heap data so parts break at row and heap block boundaries.
    let types = vec![
        LogicalType::VARCHAR,
        LogicalType::HUGEINT,
        LogicalType::list(LogicalType::VARCHAR),
    ];
    let rows = random_rows(&types, 40_000, 13, 5);
    let mut rows_collection = collection(&types);
    append_all(
        &mut rows_collection,
        &build_chunks(&types, &rows, 777),
        PinProperties::UnpinAfterDone,
    );
    assert_eq!(rows_collection.count(), 40_000);
    assert_eq!(read_rows(&rows_collection.to_chunks().unwrap()), rows);
}

#[test]
fn spilled_rows_reload_with_recomputed_heap_pointers() {
    let manager = spilling_buffer_manager("spill-collection", 4 << 20);
    let types = vec![
        LogicalType::VARCHAR,
        LogicalType::list(LogicalType::VARCHAR),
        LogicalType::BIGINT,
    ];
    let rows = random_rows(&types, 60_000, 21, 10);
    let layout = TupleDataLayout::new(types.clone(), ValidityType::CanHaveNull).unwrap();
    let mut rows_collection =
        TupleDataCollection::new(manager.clone(), Arc::new(layout), MemoryTag::HashTable).unwrap();
    append_all(
        &mut rows_collection,
        &build_chunks(&types, &rows, 2048),
        PinProperties::UnpinAfterDone,
    );
    assert!(manager.used_memory() <= 4 << 20);
    assert!(manager.used_swap() > 0, "data must have been spilled");
    // Read twice: the second pass reloads blocks at other addresses again.
    for _ in 0..2 {
        assert_eq!(read_rows(&rows_collection.to_chunks().unwrap()), rows);
    }
    drop(rows_collection);
    assert_eq!(manager.used_memory(), 0);
    assert_eq!(manager.used_swap(), 0);
}

#[test]
fn parallel_scan_reads_every_chunk_once() {
    let types = vec![LogicalType::BIGINT, LogicalType::VARCHAR];
    let rows = random_rows(&types, 50_000, 17, 10);
    let mut rows_collection = collection(&types);
    append_all(
        &mut rows_collection,
        &build_chunks(&types, &rows, 2048),
        PinProperties::UnpinAfterDone,
    );
    let global = rows_collection.initialize_parallel_scan(None, PinProperties::UnpinAfterDone);
    let mut scanned: Vec<Vec<TestValue>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    let mut local = rows_collection.initialize_local_scan(&global);
                    let mut chunk = DataChunk::default();
                    let mut rows = Vec::new();
                    while rows_collection
                        .parallel_scan(&global, &mut local, &mut chunk)
                        .unwrap()
                    {
                        rows.extend(read_rows(std::slice::from_ref(&chunk)));
                    }
                    rows
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect()
    });
    let key = |row: &Vec<TestValue>| format!("{row:?}");
    scanned.sort_by_key(key);
    let mut expected = rows;
    expected.sort_by_key(key);
    assert_eq!(scanned, expected);
}

#[test]
fn gather_by_row_pointer_keeps_strings_alive() {
    let types = vec![LogicalType::INTEGER, LogicalType::VARCHAR];
    let rows = random_rows(&types, 3000, 23, 10);
    let mut rows_collection = collection(&types);
    let mut state = rows_collection.initialize_append(PinProperties::KeepEverythingPinned);
    let mut locations = Vec::new();
    for chunk in build_chunks(&types, &rows, 1000) {
        rows_collection.append(&mut state, &chunk).unwrap();
        locations.extend_from_slice(&state.chunk_state.row_locations()[..chunk.len()]);
    }
    rows_collection.finalize_pin_state(&mut state.pin_state);
    // Gather every 10th row, in reverse order.
    let picked: Vec<u32> = (0..3000).rev().step_by(10).collect();
    let mut result = vec![
        Vector::flat(LogicalType::INTEGER, picked.len()),
        Vector::flat(LogicalType::VARCHAR, picked.len()),
    ];
    let keep_alive = rows_collection.heap_keep_alive();
    // SAFETY: all blocks stay pinned (KeepEverythingPinned) while the collection lives.
    unsafe {
        rows_collection
            .gather(
                &locations,
                Some(&picked),
                picked.len(),
                &[0, 1],
                &mut result,
                None,
                Some(&keep_alive),
            )
            .unwrap();
    }
    drop(keep_alive);
    drop(rows_collection);
    let gathered = read_vector(&result[1], picked.len());
    let expected: Vec<_> = picked.iter().map(|&row| rows[row as usize][1].clone()).collect();
    assert_eq!(
        gathered, expected,
        "strings outlive the collection through the keep-alive"
    );
}

#[test]
fn combine_iterate_and_seek() {
    let types = vec![LogicalType::BIGINT, LogicalType::VARCHAR];
    let first_rows = random_rows(&types, 5000, 29, 10);
    let second_rows = random_rows(&types, 3000, 31, 10);
    let mut first = collection(&types);
    let mut second = collection(&types);
    append_all(
        &mut first,
        &build_chunks(&types, &first_rows, 2048),
        PinProperties::UnpinAfterDone,
    );
    append_all(
        &mut second,
        &build_chunks(&types, &second_rows, 2048),
        PinProperties::UnpinAfterDone,
    );
    first.combine(second).unwrap();
    assert_eq!(first.count(), 8000);
    assert_eq!(first.chunk_count(), 5);
    let mut all = first_rows.clone();
    all.extend(second_rows);
    assert_eq!(read_rows(&first.to_chunks().unwrap()), all);

    let mut iterator = ChunkIterator::new(&first, PinProperties::UnpinAfterDone, true).unwrap();
    let mut counted = 0;
    loop {
        counted += iterator.current_chunk_count();
        assert!(iterator.heap_sizes().iter().all(|&size| size < 1000));
        if !iterator.next_chunk().unwrap() {
            break;
        }
    }
    assert_eq!(counted, 8000);

    let mut scan = first.initialize_scan(PinProperties::UnpinAfterDone);
    assert_eq!(first.seek(&mut scan, 3), 2048 * 2 + 904);
    let mut chunk = DataChunk::default();
    assert!(first.scan(&mut scan, &mut chunk).unwrap());
    assert_eq!(read_rows(std::slice::from_ref(&chunk))[0], all[2048 * 2 + 904]);
}

#[test]
fn too_wide_rows_are_rejected() {
    let types = vec![LogicalType::HUGEINT; 20_000];
    let layout = TupleDataLayout::new(types, ValidityType::CanHaveNull).unwrap();
    let error =
        TupleDataCollection::new(buffer_manager(), Arc::new(layout), MemoryTag::HashTable).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Not implemented Error: Too many columns: tuple width exceeds block size of 262144"
    );
}
