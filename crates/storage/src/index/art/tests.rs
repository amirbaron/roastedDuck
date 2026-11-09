//! Randomised tests of the ART against a `BTreeMap` model.
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::collections::{BTreeMap, BTreeSet};

use roastedduck_buffer::{BufferManager, BufferManagerConfig};
use roastedduck_types::LogicalType;

use super::node::ALLOCATOR_COUNT;
use super::*;
use crate::index::{IndexAppendMode, IndexConstraintType};

/// A small xorshift generator so the tests are deterministic.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn art(constraint: IndexConstraintType, ty: LogicalType) -> Art {
    let manager = BufferManager::new(BufferManagerConfig::default()).unwrap();
    Art::new(
        "idx",
        constraint,
        vec![0],
        vec![ty],
        IndexKeyExpressions::Columns(vec!["k".into()]),
        manager,
        262_136,
    )
    .unwrap()
}

type Model = BTreeMap<Vec<u8>, BTreeSet<i64>>;

/// Counts the segments reachable from the root per allocator (`VerifyAllocations`).
fn count_segments(art: &Art) -> [usize; ALLOCATOR_COUNT] {
    let mut counts = [0; ALLOCATOR_COUNT];
    let mut stack = vec![art.tree];
    while let Some(node) = stack.pop() {
        if !node.has_metadata() {
            continue;
        }
        let ty = node.ntype().unwrap();
        if let Some(index) = ty.allocator_index() {
            counts[index] += 1;
        }
        match ty {
            NType::Prefix => stack.push(art.prefix_child(node).unwrap()),
            NType::Node4 | NType::Node16 | NType::Node48 | NType::Node256 => {
                for (_, child) in art.children(node).unwrap() {
                    stack.push(child);
                }
            }
            _ => {}
        }
    }
    counts
}

fn check(art: &Art, model: &Model) {
    let counts = count_segments(art);
    for (i, count) in counts.iter().enumerate() {
        assert_eq!(*count, art.allocators[i].segment_count(), "allocator {i}");
    }
    let mut all = BTreeSet::new();
    assert!(art.full_scan(usize::MAX, &mut all).unwrap());
    let expected: BTreeSet<i64> = model.values().flatten().copied().collect();
    assert_eq!(all, expected);
    for (key, row_ids) in model {
        let mut found = BTreeSet::new();
        assert!(art.search_equal(key, usize::MAX, &mut found).unwrap());
        assert_eq!(&found, row_ids, "key {key:?}");
        for &row_id in row_ids {
            assert!(art.contains(key, row_id).unwrap());
        }
    }
}

fn check_ranges(art: &Art, model: &Model, rng: &mut Rng, keys: &[Vec<u8>]) {
    for _ in 0..20 {
        let a = &keys[rng.below(keys.len() as u64) as usize];
        let b = &keys[rng.below(keys.len() as u64) as usize];
        let (low, high) = if a <= b { (a, b) } else { (b, a) };
        for equal in [false, true] {
            let mut found = BTreeSet::new();
            art.search_greater(low, equal, usize::MAX, &mut found).unwrap();
            let expected: BTreeSet<i64> = model
                .iter()
                .filter(|(k, _)| if equal { *k >= low } else { *k > low })
                .flat_map(|(_, v)| v.iter().copied())
                .collect();
            assert_eq!(found, expected, "greater {low:?} {equal}");
            let mut found = BTreeSet::new();
            art.search_less(high, equal, usize::MAX, &mut found).unwrap();
            let expected: BTreeSet<i64> = model
                .iter()
                .filter(|(k, _)| if equal { *k <= high } else { *k < high })
                .flat_map(|(_, v)| v.iter().copied())
                .collect();
            assert_eq!(found, expected, "less {high:?} {equal}");
            let mut found = BTreeSet::new();
            art.search_close_range(low, high, equal, !equal, usize::MAX, &mut found)
                .unwrap();
            let expected: BTreeSet<i64> = model
                .iter()
                .filter(|(k, _)| {
                    (if equal { *k >= low } else { *k > low }) && (if equal { *k < high } else { *k <= high })
                })
                .flat_map(|(_, v)| v.iter().copied())
                .collect();
            assert_eq!(found, expected, "range {low:?} {high:?}");
        }
    }
}

fn run_random(keys: &[Vec<u8>], unique: bool, seed: u64, operations: usize) {
    let constraint = if unique {
        IndexConstraintType::Unique
    } else {
        IndexConstraintType::None
    };
    let mut art = art(constraint, LogicalType::BIGINT);
    let mut model = Model::new();
    let mut rng = Rng(seed);
    let mut next_row_id = 0i64;
    let mut live: Vec<(Vec<u8>, i64)> = Vec::new();
    for step in 0..operations {
        if live.is_empty() || rng.below(3) != 0 {
            let key = keys[rng.below(keys.len() as u64) as usize].clone();
            // sparse row ids exercise every nested node type
            let row_id = if rng.below(2) == 0 {
                next_row_id
            } else {
                (rng.next() % (1 << 50)) as i64
            };
            next_row_id += 1;
            if model.values().any(|v| v.contains(&row_id)) {
                continue;
            }
            let result = art.insert_keys(
                &[Some(key.clone())],
                &[row_id],
                &[],
                IndexAppendMode::Default,
                None,
            );
            if unique && model.contains_key(&key) {
                assert!(result.is_err());
            } else {
                result.unwrap();
                model.entry(key.clone()).or_default().insert(row_id);
                live.push((key, row_id));
            }
        } else {
            let pos = rng.below(live.len() as u64) as usize;
            let (key, row_id) = live.swap_remove(pos);
            assert!(art.delete_key(&key, row_id_key(row_id)).unwrap());
            assert!(!art.delete_key(&key, row_id_key(row_id)).unwrap());
            let set = model.get_mut(&key).unwrap();
            set.remove(&row_id);
            if set.is_empty() {
                model.remove(&key);
            }
        }
        if step % 97 == 0 {
            check(&art, &model);
            check_ranges(&art, &model, &mut rng, keys);
        }
    }
    check(&art, &model);
    check_ranges(&art, &model, &mut rng, keys);
    // delete everything: all memory is released
    for (key, row_id) in live {
        assert!(art.delete_key(&key, row_id_key(row_id)).unwrap());
    }
    assert!(art.is_empty());
    for allocator in &art.allocators {
        assert_eq!(allocator.segment_count(), 0);
    }
}

fn int_keys(count: usize, spread: u64, rng: &mut Rng) -> Vec<Vec<u8>> {
    (0..count)
        .map(|_| row_id_key((rng.below(spread) as i64) - (spread as i64 / 2)).to_vec())
        .collect()
}

fn string_keys(count: usize, rng: &mut Rng) -> Vec<Vec<u8>> {
    (0..count)
        .map(|_| {
            let len = rng.below(40) as usize;
            let mut bytes = Vec::new();
            // long shared prefixes exercise prefix chains
            if rng.below(2) == 0 {
                bytes.extend_from_slice(b"a shared prefix that is longer than one prefix segment ");
            }
            for _ in 0..len {
                bytes.push(b"ab\x00\x01xyz"[rng.below(7) as usize]);
            }
            let mut key = Vec::new();
            key::encode_value(&roastedduck_types::Value::blob(&bytes), &mut key).unwrap();
            key
        })
        .collect()
}

#[test]
fn non_unique_integer_keys() {
    let mut rng = Rng(7);
    let keys = int_keys(300, 1000, &mut rng);
    run_random(&keys, false, 11, 6000);
}

#[test]
fn non_unique_few_keys_many_row_ids() {
    let mut rng = Rng(3);
    let keys = int_keys(4, 10, &mut rng);
    run_random(&keys, false, 5, 4000);
}

#[test]
fn unique_integer_keys() {
    let mut rng = Rng(13);
    let keys = int_keys(2000, 1 << 40, &mut rng);
    run_random(&keys, true, 17, 5000);
}

#[test]
fn string_keys_with_long_prefixes() {
    let mut rng = Rng(23);
    let keys = string_keys(400, &mut rng);
    run_random(&keys, false, 29, 5000);
    run_random(&keys, true, 31, 3000);
}

#[test]
fn unique_conflict_rolls_back_the_batch() {
    let mut art = art(IndexConstraintType::Primary, LogicalType::BIGINT);
    let k = |v: i64| Some(row_id_key(v).to_vec());
    art.insert_keys(&[k(1)], &[100], &[], IndexAppendMode::Default, None)
        .unwrap();
    let err = art
        .insert_keys(
            &[k(2), k(3), k(1)],
            &[0, 1, 2],
            &[],
            IndexAppendMode::Default,
            None,
        )
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Constraint Error: PRIMARY KEY or UNIQUE constraint violation: duplicate key \"???\""
    );
    let mut all = BTreeSet::new();
    art.full_scan(usize::MAX, &mut all).unwrap();
    assert_eq!(all, BTreeSet::from([100]));
    // ignore duplicates
    art.insert_keys(
        &[k(1), k(5)],
        &[7, 8],
        &[],
        IndexAppendMode::IgnoreDuplicates,
        None,
    )
    .unwrap();
    let mut all = BTreeSet::new();
    art.full_scan(usize::MAX, &mut all).unwrap();
    assert_eq!(all, BTreeSet::from([8, 100]));
}

#[test]
fn delete_index_allows_reinsert() {
    let mut main = art(IndexConstraintType::Unique, LogicalType::BIGINT);
    let mut deleted = art(IndexConstraintType::Unique, LogicalType::BIGINT);
    let k = |v: i64| Some(row_id_key(v).to_vec());
    main.insert_keys(&[k(1)], &[10], &[], IndexAppendMode::Default, None)
        .unwrap();
    deleted
        .insert_keys(&[k(1)], &[10], &[], IndexAppendMode::Default, None)
        .unwrap();
    main.insert_keys(&[k(1)], &[11], &[&deleted], IndexAppendMode::Default, None)
        .unwrap();
    let mut found = BTreeSet::new();
    main.search_equal(&row_id_key(1), usize::MAX, &mut found).unwrap();
    assert_eq!(found, BTreeSet::from([10, 11]));
    assert!(main.delete_key(&row_id_key(1), row_id_key(10)).unwrap());
    let mut found = BTreeSet::new();
    main.search_equal(&row_id_key(1), usize::MAX, &mut found).unwrap();
    assert_eq!(found, BTreeSet::from([11]));
}

#[test]
fn paused_scans_resume() {
    let mut art = art(IndexConstraintType::None, LogicalType::BIGINT);
    let key = row_id_key(42).to_vec();
    let row_ids: Vec<i64> = (0..600).map(|i| i * 3).collect();
    let keys: Vec<Option<ArtKey>> = row_ids.iter().map(|_| Some(key.clone())).collect();
    art.insert_keys(&keys, &row_ids, &[], IndexAppendMode::Default, None)
        .unwrap();
    let mut found = BTreeSet::new();
    assert!(!art.search_equal(&key, 100, &mut found).unwrap());
    assert_eq!(found.len(), 100);
    let mut found = BTreeSet::new();
    assert!(art.search_equal(&key, 600, &mut found).unwrap());
    assert_eq!(found.len(), 600);
}

/// A checkpoint store with deliberately offset buffers, counting physical reads.
#[derive(Debug, Default)]
struct CheckpointBuffers {
    buffers: std::sync::Mutex<Vec<Vec<u8>>>,
    reads: std::sync::atomic::AtomicUsize,
}

impl super::super::BufferLoader for CheckpointBuffers {
    fn load(
        &self,
        pointer: super::super::BufferPointer,
        allocation_size: usize,
    ) -> roastedduck_common::Result<Vec<u8>> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let buffers = self.buffers.lock().unwrap();
        let start = pointer.offset as usize;
        Ok(buffers[pointer.block_id as usize][start..start + allocation_size].to_vec())
    }
}

fn stored_art_round_trip(deprecated: bool) {
    use roastedduck_common::CaseInsensitiveMap;
    use roastedduck_types::Value;
    use std::sync::atomic::Ordering;

    let store = std::sync::Arc::new(CheckpointBuffers::default());
    let mut source = art(IndexConstraintType::None, LogicalType::BIGINT);
    source.set_storage_version(Some(7));
    let mut model = Model::new();
    let mut row = 0;
    for key in (0..500).map(row_id_key).map(|key| key.to_vec()).chain(
        (0..40).map(|i| format!("a-long-shared-prefix-that-spans-several-segments-{i:04}").into_bytes()),
    ) {
        let duplicates = if row % 3 == 0 { 25 } else { 1 };
        for _ in 0..duplicates {
            source
                .insert_keys(&[Some(key.clone())], &[row], &[], IndexAppendMode::Default, None)
                .unwrap();
            model.entry(key.clone()).or_default().insert(row);
            row += 1;
        }
    }
    let mut options = CaseInsensitiveMap::new();
    options.insert("v1_0_0_storage", Value::boolean(deprecated));
    let mut write = |data: &[u8]| {
        let mut buffers = store.buffers.lock().unwrap();
        let id = buffers.len() as i64;
        let offset = if data.len() + 16 <= 262_136 { 16 } else { 0 };
        let mut bytes = vec![0; offset];
        bytes.extend_from_slice(data);
        buffers.push(bytes);
        Ok(super::super::BufferPointer {
            block_id: id,
            offset: offset as u32,
        })
    };
    let info = source
        .serialize_to_disk(&options, &mut write, &mut |_| {})
        .unwrap();
    let mut restored = art(IndexConstraintType::None, LogicalType::BIGINT);
    restored.load_from_storage(&info, store.clone()).unwrap();
    assert_eq!(restored.storage_version(), Some(7));
    assert_eq!(
        restored.prefix_count(),
        if deprecated { DEPRECATED_PREFIX_COUNT } else { 7 }
    );
    assert_eq!(restored.in_memory_size(), 0);
    assert_eq!(store.reads.load(Ordering::Relaxed), 0);
    for (key, expected) in &model {
        let mut found = BTreeSet::new();
        assert!(restored.search_equal(key, usize::MAX, &mut found).unwrap());
        assert_eq!(&found, expected);
    }
    let reads = store.reads.load(Ordering::Relaxed);
    assert!(reads > 0);
    let mut all = BTreeSet::new();
    restored.full_scan(usize::MAX, &mut all).unwrap();
    assert_eq!(all, model.values().flatten().copied().collect());
    assert_eq!(store.reads.load(Ordering::Relaxed), reads);

    let (first_key, first_rows) = model.first_key_value().unwrap();
    let first_key = first_key.clone();
    let first_row = *first_rows.first().unwrap();
    assert!(restored.delete_key(&first_key, row_id_key(first_row)).unwrap());
    let rows = model.get_mut(&first_key).unwrap();
    rows.remove(&first_row);
    if rows.is_empty() {
        model.remove(&first_key);
    }
    let key = row_id_key(100_000).to_vec();
    restored
        .insert_keys(&[Some(key.clone())], &[row], &[], IndexAppendMode::Default, None)
        .unwrap();
    model.insert(key, BTreeSet::from([row]));
    let mut kept = 0;
    let info = restored
        .serialize_to_disk(&options, &mut write, &mut |_| kept += 1)
        .unwrap();
    assert!(
        kept > 0,
        "unchanged persisted buffers must survive a subsequent checkpoint"
    );
    store.reads.store(0, Ordering::Relaxed);
    let mut again = art(IndexConstraintType::None, LogicalType::BIGINT);
    again.load_from_storage(&info, store.clone()).unwrap();
    assert_eq!(store.reads.load(Ordering::Relaxed), 0);
    for (key, expected) in model {
        let mut found = BTreeSet::new();
        assert!(again.search_equal(&key, usize::MAX, &mut found).unwrap());
        assert_eq!(found, expected);
    }
}

#[test]
fn persisted_modern_nodes_load_lazily_and_remain_mutable() {
    stored_art_round_trip(false);
}

#[test]
fn persisted_deprecated_leaf_layout_loads_lazily_and_remains_mutable() {
    stored_art_round_trip(true);
}

/// Defers writes until flush, so publishing a pointer early cannot pass a reload.
struct DeferredCheckpoint {
    store: std::sync::Arc<CheckpointBuffers>,
    pending: Vec<Vec<u8>>,
    flushes: usize,
    fail_flush: bool,
}

impl DeferredCheckpoint {
    fn new(store: std::sync::Arc<CheckpointBuffers>) -> Self {
        Self {
            store,
            pending: Vec::new(),
            flushes: 0,
            fail_flush: false,
        }
    }
}

impl super::super::IndexBufferWriter for DeferredCheckpoint {
    fn write(&mut self, data: &[u8]) -> roastedduck_common::Result<super::super::BufferPointer> {
        let id = self.store.buffers.lock().unwrap().len() + self.pending.len();
        self.pending.push(data.to_vec());
        Ok(super::super::BufferPointer {
            block_id: id as i64,
            offset: 0,
        })
    }

    fn flush(&mut self) -> roastedduck_common::Result<()> {
        self.flushes += 1;
        if self.fail_flush {
            return Err(roastedduck_common::Error::new(
                roastedduck_common::ErrorKind::Io,
                "injected flush failure",
            ));
        }
        self.store.buffers.lock().unwrap().append(&mut self.pending);
        Ok(())
    }
}

fn one_key_index() -> Art {
    let mut source = art(IndexConstraintType::None, LogicalType::BIGINT);
    source
        .insert_keys(
            &[Some(row_id_key(42).to_vec())],
            &[17],
            &[],
            IndexAppendMode::Default,
            None,
        )
        .unwrap();
    source
}

fn check_one_persisted_key(info: &ArtStorageInfo, store: std::sync::Arc<CheckpointBuffers>) {
    let mut restored = art(IndexConstraintType::None, LogicalType::BIGINT);
    restored.load_from_storage(info, store).unwrap();
    let mut found = BTreeSet::new();
    assert!(
        restored
            .search_equal(&row_id_key(42), usize::MAX, &mut found)
            .unwrap()
    );
    assert_eq!(found, BTreeSet::from([17]));
}

#[test]
fn serialization_flushes_deferred_buffers_before_returning() {
    let store = std::sync::Arc::new(CheckpointBuffers::default());
    let mut writer = DeferredCheckpoint::new(store.clone());
    let info = one_key_index()
        .serialize_to_disk(
            &roastedduck_common::CaseInsensitiveMap::default(),
            &mut writer,
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(writer.flushes, 1);
    assert_eq!(writer.pending, Vec::<Vec<u8>>::new());
    check_one_persisted_key(&info, store);
}

#[test]
fn unbound_wal_metadata_is_not_published_before_flush_succeeds() {
    let options = roastedduck_common::CaseInsensitiveMap::default();
    let (info, buffers) = one_key_index().serialize_to_wal(&options).unwrap();
    let unbound = super::super::UnboundIndex::new(
        "idx",
        "ART",
        IndexConstraintType::None,
        vec![0],
        std::sync::Arc::new(()),
        super::super::UnboundIndexStorage::Wal(info, buffers),
    );
    let mut failed = DeferredCheckpoint::new(std::sync::Arc::new(CheckpointBuffers::default()));
    failed.fail_flush = true;
    assert!(unbound.serialize_to_disk(&mut failed, &mut |_| {}).is_err());
    let store = std::sync::Arc::new(CheckpointBuffers::default());
    let mut retry = DeferredCheckpoint::new(store.clone());
    let info = unbound.serialize_to_disk(&mut retry, &mut |_| {}).unwrap();
    assert_eq!(retry.flushes, 1);
    assert_eq!(retry.pending, Vec::<Vec<u8>>::new());
    check_one_persisted_key(&info, store);
}

#[test]
fn unchanged_deprecated_checkpoint_keeps_buffers_unloaded() {
    let options = roastedduck_common::CaseInsensitiveMap::default();
    let store = std::sync::Arc::new(CheckpointBuffers::default());
    let mut writer = DeferredCheckpoint::new(store.clone());
    let info = one_key_index()
        .serialize_to_disk(&options, &mut writer, &mut |_| {})
        .unwrap();
    let mut restored = art(IndexConstraintType::None, LogicalType::BIGINT);
    restored.load_from_storage(&info, store.clone()).unwrap();
    let again = restored
        .serialize_to_disk(&options, &mut writer, &mut |_| {})
        .unwrap();
    assert_eq!(store.reads.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(restored.in_memory_size(), 0);
    assert_eq!(again.root, info.root);
    for (a, b) in again.allocator_infos.iter().zip(&info.allocator_infos) {
        assert_eq!(a.block_pointers, b.block_pointers);
    }
    check_one_persisted_key(&again, store);
}

#[test]
fn cold_modern_to_deprecated_conversion_still_loads_and_converts_gates() {
    let store = std::sync::Arc::new(CheckpointBuffers::default());
    let mut writer = DeferredCheckpoint::new(store.clone());
    let mut source = art(IndexConstraintType::None, LogicalType::BIGINT);
    let rows: Vec<i64> = (0..64).collect();
    source
        .insert_keys(
            &vec![Some(row_id_key(42).to_vec()); rows.len()],
            &rows,
            &[],
            IndexAppendMode::Default,
            None,
        )
        .unwrap();
    let mut modern = roastedduck_common::CaseInsensitiveMap::default();
    modern.insert("v1_0_0_storage", roastedduck_types::Value::boolean(false));
    let info = source
        .serialize_to_disk(&modern, &mut writer, &mut |_| {})
        .unwrap();
    let mut restored = art(IndexConstraintType::None, LogicalType::BIGINT);
    restored.load_from_storage(&info, store.clone()).unwrap();
    assert_eq!(store.reads.load(std::sync::atomic::Ordering::Relaxed), 0);
    let old = restored
        .serialize_to_disk(
            &roastedduck_common::CaseInsensitiveMap::default(),
            &mut writer,
            &mut |_| {},
        )
        .unwrap();
    assert!(store.reads.load(std::sync::atomic::Ordering::Relaxed) > 0);
    assert_eq!(old.allocator_infos.len(), super::node::DEPRECATED_ALLOCATOR_COUNT);
    let mut again = art(IndexConstraintType::None, LogicalType::BIGINT);
    again.load_from_storage(&old, store).unwrap();
    let mut found = BTreeSet::new();
    again
        .search_equal(&row_id_key(42), usize::MAX, &mut found)
        .unwrap();
    assert_eq!(found, rows.into_iter().collect());
}

#[test]
fn partial_persisted_mutation_converts_new_gates_and_preserves_other_keys() {
    let store = std::sync::Arc::new(CheckpointBuffers::default());
    let mut writer = DeferredCheckpoint::new(store.clone());
    let options = roastedduck_common::CaseInsensitiveMap::default();
    let mut source = art(IndexConstraintType::None, LogicalType::BIGINT);
    for i in 0..256 {
        source
            .insert_keys(
                &[Some(row_id_key(i).to_vec())],
                &[i],
                &[],
                IndexAppendMode::Default,
                None,
            )
            .unwrap();
    }
    let info = source
        .serialize_to_disk(&options, &mut writer, &mut |_| {})
        .unwrap();
    let mut restored = art(IndexConstraintType::None, LogicalType::BIGINT);
    restored.load_from_storage(&info, store.clone()).unwrap();
    restored
        .insert_keys(
            &[Some(row_id_key(42).to_vec()), Some(row_id_key(42).to_vec())],
            &[10_000, 10_001],
            &[],
            IndexAppendMode::Default,
            None,
        )
        .unwrap();
    let info = restored
        .serialize_to_disk(&options, &mut writer, &mut |_| {})
        .unwrap();
    let mut again = art(IndexConstraintType::None, LogicalType::BIGINT);
    again.load_from_storage(&info, store).unwrap();
    for i in 0..256 {
        let mut found = BTreeSet::new();
        again
            .search_equal(&row_id_key(i), usize::MAX, &mut found)
            .unwrap();
        assert_eq!(
            found,
            if i == 42 {
                BTreeSet::from([42, 10_000, 10_001])
            } else {
                BTreeSet::from([i])
            }
        );
    }
}

#[test]
fn unbound_replays_keep_physical_columns_ownership_order_and_retry_state() {
    use crate::index::unbound_index::BufferedIndexReplay;
    use crate::index::{UnboundIndex, UnboundIndexStorage};
    use roastedduck_types::{DataChunk, Value, Vector};

    let unbound = UnboundIndex::new(
        "idx",
        "ART",
        IndexConstraintType::Unique,
        vec![2],
        std::sync::Arc::new(()),
        UnboundIndexStorage::Disk(ArtStorageInfo::default()),
    );
    let mut key = Vector::flat(LogicalType::BIGINT, 1);
    key.set_value(0, &Value::bigint(42)).unwrap();
    let mut chunk = DataChunk::new(
        vec![
            Vector::constant_null(LogicalType::INTEGER),
            Vector::constant_null(LogicalType::VARCHAR),
            key,
        ],
        1,
    );
    for kind in [BufferedIndexReplay::Insert, BufferedIndexReplay::Delete] {
        unbound.push_replay(unbound.prepare_replay(&chunk, &[17], kind).unwrap());
    }
    chunk.column_mut(2).set_value(0, &Value::bigint(43)).unwrap();
    unbound.push_replay(
        unbound
            .prepare_replay(&chunk, &[19], BufferedIndexReplay::Insert)
            .unwrap(),
    );
    chunk.column_mut(2).set_value(0, &Value::bigint(44)).unwrap();
    drop(chunk);
    assert!(unbound.has_buffered_replays());
    let make = || {
        Art::new(
            "idx",
            IndexConstraintType::Unique,
            vec![2],
            vec![LogicalType::BIGINT],
            IndexKeyExpressions::Columns(vec!["key".into()]),
            BufferManager::new(BufferManagerConfig::default()).unwrap(),
            262_136,
        )
        .unwrap()
    };
    assert!(
        unbound
            .apply_buffered_replays(&mut make(), &[LogicalType::BIGINT])
            .is_err()
    );
    let types = [LogicalType::INTEGER, LogicalType::VARCHAR, LogicalType::BIGINT];
    for _ in 0..2 {
        let mut restored = make();
        unbound.apply_buffered_replays(&mut restored, &types).unwrap();
        let mut rows = BTreeSet::new();
        restored.full_scan(usize::MAX, &mut rows).unwrap();
        assert_eq!(rows, BTreeSet::from([19]));
        rows.clear();
        restored
            .search_equal(&row_id_key(43), usize::MAX, &mut rows)
            .unwrap();
        assert_eq!(rows, BTreeSet::from([19]));
    }
}
