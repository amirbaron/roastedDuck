//! Real Rust HNSW wrapper validation and transaction lifecycle.
#![allow(clippy::unwrap_used)]

use roastedduck_buffer::{BufferManager, BufferManagerConfig, InMemoryBlockManager};
use roastedduck_common::{CaseInsensitiveMap, ErrorKind, Result};
use roastedduck_storage::index::{
    BoundIndex, HnswIndex, HnswIndexType, IndexConstraintType, IndexKeyExpressions, IndexRemovalType,
    IndexTypeRegistry, SecondaryIndex, SecondaryIndexSpec,
};
use roastedduck_storage::table::StorageIndex;
use roastedduck_storage::{
    ColumnDefinition, DataTable, TableConstraints, TableIoManager, TransactionManager,
};
use roastedduck_types::{DataChunk, LogicalType, SelectionVector, Value, Vector};
use std::sync::Arc;

fn spec() -> SecondaryIndexSpec {
    SecondaryIndexSpec {
        constraint: IndexConstraintType::None,
        name: "idx".into(),
        column_ids: vec![0],
        key_types: vec![LogicalType::array(LogicalType::FLOAT, 3)],
        expressions: IndexKeyExpressions::Columns(vec!["vec".into()]),
        options: CaseInsensitiveMap::default(),
        persistent: false,
    }
}
fn chunk(values: &[Option<[f32; 3]>]) -> DataChunk {
    let ty = LogicalType::array(LogicalType::FLOAT, 3);
    let mut vector = Vector::flat(ty.clone(), values.len());
    for (row, value) in values.iter().enumerate() {
        let value = value.map_or_else(
            || Value::null(ty.clone()),
            |v| Value::array(LogicalType::FLOAT, v.into_iter().map(Value::float).collect()),
        );
        vector.set_value(row, &value).unwrap();
    }
    DataChunk::new(vec![vector], values.len())
}
fn table() -> Arc<DataTable> {
    let io = Arc::new(TableIoManager::new(
        BufferManager::new(BufferManagerConfig::default()).unwrap(),
        Arc::new(InMemoryBlockManager::default()),
    ));
    DataTable::new(
        io,
        "main",
        "vectors",
        vec![ColumnDefinition::new(
            "vec",
            LogicalType::array(LogicalType::FLOAT, 3),
        )],
    )
}

#[test]
fn options_match_pinned_validation_and_derived_defaults() -> Result<()> {
    let index = HnswIndex::new(spec())?;
    assert_eq!(index.metric(), "l2sq");
    assert_eq!(index.dimensions(), 3);
    assert_eq!(
        (
            index.config().m,
            index.config().m0,
            index.config().ef_construction,
            index.config().ef_search
        ),
        (16, 32, 128, 64)
    );
    for (key, value, message) in [
        (
            "ef_construction",
            Value::varchar("foo"),
            "HNSW index 'ef_construction' must be an integer",
        ),
        (
            "ef_construction",
            Value::integer(0),
            "HNSW index 'ef_construction' must be at least 1",
        ),
        (
            "ef_search",
            Value::bigint(12),
            "HNSW index 'ef_search' must be an integer",
        ),
        (
            "ef_search",
            Value::integer(0),
            "HNSW index 'ef_search' must be at least 1",
        ),
        ("M", Value::varchar("foo"), "HNSW index 'M' must be an integer"),
        ("M", Value::integer(1), "HNSW index 'M' must be at least 2"),
        ("M0", Value::integer(0), "HNSW index 'M0' must be at least 2"),
        (
            "metric",
            Value::integer(1),
            "HNSW index 'metric' must be a string",
        ),
        (
            "unrecognized",
            Value::integer(1),
            "Unknown option for HNSW index: 'unrecognized'",
        ),
    ] {
        let mut input = spec();
        input.options.insert(key, value);
        let error = HnswIndex::new(input).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Binder);
        assert_eq!(error.raw_message(), message);
    }
    let mut input = spec();
    input.options.insert("m", Value::integer(7));
    input.options.insert("metric", Value::varchar("CoSiNe"));
    let index = HnswIndex::new(input.clone())?;
    assert_eq!(index.config().m0, 14);
    assert_eq!(index.metric(), "cosine");
    input.options.insert("M0", Value::integer(9));
    assert_eq!(HnswIndex::new(input)?.config().m0, 9);
    for constraint in [
        IndexConstraintType::Unique,
        IndexConstraintType::Primary,
        IndexConstraintType::Foreign,
    ] {
        let mut input = spec();
        input.constraint = constraint;
        assert_eq!(
            HnswIndex::new(input).unwrap_err().raw_message(),
            "HNSW indexes do not support unique or primary key constraints"
        );
    }
    let mut input = spec();
    input.key_types.push(input.key_types[0].clone());
    assert_eq!(
        HnswIndex::new(input).unwrap_err().raw_message(),
        "HNSW indexes can only be created over a single column of keys."
    );
    let mut input = spec();
    input.key_types = vec![LogicalType::list(LogicalType::FLOAT)];
    assert_eq!(
        HnswIndex::new(input).unwrap_err().raw_message(),
        "HNSW index keys must be of type FLOAT[N]"
    );
    let mut input = spec();
    input.key_types = vec![LogicalType::array(LogicalType::DOUBLE, 3)];
    assert_eq!(
        HnswIndex::new(input).unwrap_err().raw_message(),
        "HNSW index key type must be one of: 'FLOAT[N]'"
    );
    Ok(())
}

#[test]
fn append_skips_null_arrays_preserves_raw_child_payload_and_is_atomic() -> Result<()> {
    let mut index = HnswIndex::new(spec())?;
    let mut batch = chunk(&[Some([1.0, 8.0, 3.0]), None]);
    // The pinned upstream code ignores child validity, passing the raw f32 buffer.
    // Give that null slot an explicit payload to test this mechanism deterministically.
    batch
        .column_mut(0)
        .array_child_mut()
        .unwrap()
        .validity_mut()
        .set_invalid(1, 6);
    index.append(&batch, &[0, 1])?;
    assert_eq!(index.active_count(), 1);
    assert!(index.contains(0));
    assert!(!index.contains(1));
    assert_eq!(
        index.search(&[1.0, 8.0, 3.0], 1, None)?.neighbors[0]
            .distance
            .to_bits(),
        0.0f64.to_bits()
    );
    let before = index.stats();
    assert!(
        index
            .append(&chunk(&[Some([2.0, 2.0, 2.0]), Some([3.0, 3.0, 3.0])]), &[2, 0])
            .is_err()
    );
    assert_eq!(index.stats(), before);
    assert!(!index.contains(2));
    assert!(index.append(&batch, &[0]).is_err());
    assert_eq!(index.stats(), before);
    assert_eq!(index.revert_append(&batch, &[0, 1])?, 1);
    assert_eq!(index.revert_append(&batch, &[0, 1])?, 0);
    Ok(())
}

#[test]
fn dictionary_constant_and_bound_expression_keys_keep_row_alignment() -> Result<()> {
    use roastedduck_storage::index::art::IndexKeyExecutor;
    #[derive(Debug)]
    struct SecondColumn {
        names: Vec<roastedduck_common::ByteString>,
    }
    impl IndexKeyExecutor for SecondColumn {
        fn execute(&self, data: &DataChunk) -> Result<DataChunk> {
            Ok(DataChunk::new(vec![data.column(1).clone()], data.len()))
        }
        fn names(&self) -> &[roastedduck_common::ByteString] {
            &self.names
        }
    }
    let data = chunk(&[
        Some([1.0, 0.0, 0.0]),
        Some([2.0, 0.0, 0.0]),
        Some([3.0, 0.0, 0.0]),
    ]);
    let selected = data.column(0).slice(&SelectionVector::new(vec![2, 0]));
    let mut input = spec();
    input.column_ids = vec![1];
    input.expressions = IndexKeyExpressions::Custom(Arc::new(SecondColumn {
        names: vec!["computed".into()],
    }));
    let mut index = HnswIndex::new(input)?;
    let batch = DataChunk::new(vec![Vector::constant(&Value::integer(99))?, selected], 2);
    index.append(&batch, &[10, 11])?;
    assert_eq!(index.search(&[3.0, 0.0, 0.0], 1, None)?.neighbors[0].label, 10);
    assert_eq!(index.search(&[1.0, 0.0, 0.0], 1, None)?.neighbors[0].label, 11);
    let constant = Vector::constant(&Value::array(
        LogicalType::FLOAT,
        vec![Value::float(4.0), Value::float(0.0), Value::float(0.0)],
    ))?;
    index.append(
        &DataChunk::new(vec![Vector::constant(&Value::integer(99))?, constant], 2),
        &[12, 13],
    )?;
    let found = index.search(&[4.0, 0.0, 0.0], 2, None)?;
    assert!(found.neighbors.iter().all(|n| [12, 13].contains(&n.label)));
    Ok(())
}

#[test]
fn retire_compact_revert_and_cleanup_preserve_old_visible_candidates() -> Result<()> {
    let mut index = HnswIndex::new(spec())?;
    let batch = chunk(&[Some([1.0, 0.0, 0.0]), Some([2.0, 0.0, 0.0])]);
    index.append(&batch, &[0, 1])?;
    let first = chunk(&[Some([1.0, 0.0, 0.0])]);
    index.remove(&first, &[0], IndexRemovalType::MainIndex)?;
    assert_eq!(index.active_count(), 1);
    assert_eq!(index.searchable_count(), 2);
    assert!(index.is_retired(0));
    index.compact()?;
    assert_eq!(index.search(&[1.0, 0.0, 0.0], 1, None)?.neighbors[0].label, 0);
    index.remove(&first, &[0], IndexRemovalType::RevertMainIndex)?;
    assert_eq!(index.retired_count(), 0);
    assert_eq!(index.active_count(), 2);
    index.remove(&first, &[0], IndexRemovalType::MainIndex)?;
    index.remove(&first, &[0], IndexRemovalType::DeletedRowsInUse)?;
    index.remove(&first, &[0], IndexRemovalType::DeletedRowsInUse)?;
    assert!(!index.contains(0));
    assert_eq!(index.search(&[1.0, 0.0, 0.0], 1, None)?.neighbors[0].label, 1);
    index.compact()?;
    assert_eq!(index.stats().deleted, 0);
    index.remove(&first, &[0], IndexRemovalType::RevertMainIndexOnly)?;
    index.remove(&first, &[0], IndexRemovalType::RevertMainIndexOnly)?;
    assert_eq!(index.searchable_count(), 2);
    assert_eq!(index.search(&[1.0, 0.0, 0.0], 1, None)?.neighbors[0].label, 0);
    index.reset();
    assert_eq!(index.searchable_count(), 0);
    assert_eq!(index.retired_count(), 0);
    Ok(())
}

#[test]
fn real_table_commit_delete_and_snapshot_cleanup_drive_the_graph() -> Result<()> {
    let table = table();
    let manager = TransactionManager::default();
    let registry = IndexTypeRegistry::default();
    registry.register(Arc::new(HnswIndexType))?;
    let create = manager.begin();
    table.create_index(&create, registry.create("hnsw", spec())?, true)?;
    table.append_chunk(
        &create,
        &chunk(&[Some([1.0, 0.0, 0.0]), Some([2.0, 0.0, 0.0]), None]),
        TableConstraints::default(),
    )?;
    manager.commit(&create)?;
    let old = manager.begin();
    let delete = manager.begin();
    table.delete(&delete, &[0])?;
    manager.commit(&delete)?;
    let entry = table.indexes().find("idx").unwrap();
    {
        let mut guard = entry.lock();
        let index = guard
            .index
            .as_secondary_mut()
            .unwrap()
            .as_any_mut()
            .downcast_mut::<HnswIndex>()
            .unwrap();
        assert_eq!(index.searchable_count(), 2);
        assert_eq!(index.active_count(), 1);
        index.compact()?;
        assert_eq!(index.search(&[1.0, 0.0, 0.0], 1, None)?.neighbors[0].label, 0);
    }
    let mut fetched = DataChunk::default();
    table.fetch(&old, &mut fetched, &[StorageIndex::new(0)], &[0])?;
    assert_eq!(fetched.len(), 1);
    let current = manager.begin();
    table.fetch(&current, &mut fetched, &[StorageIndex::new(0)], &[0])?;
    assert_eq!(fetched.len(), 0);
    manager.rollback(&current);
    manager.rollback(&old);
    {
        let guard = entry.lock();
        let index = guard
            .index
            .as_secondary()
            .unwrap()
            .as_any()
            .downcast_ref::<HnswIndex>()
            .unwrap();
        assert_eq!(index.searchable_count(), 1);
        assert!(!index.contains(0));
        assert_eq!(index.retired_count(), 0);
    }
    assert!(matches!(entry.lock().index, BoundIndex::Secondary(_)));
    Ok(())
}

#[test]
fn index_built_after_delete_classifies_old_visible_rows_for_later_cleanup() -> Result<()> {
    let table = table();
    let manager = TransactionManager::default();
    let seed = manager.begin();
    table.append_chunk(
        &seed,
        &chunk(&[Some([1.0, 0.0, 0.0]), Some([2.0, 0.0, 0.0])]),
        TableConstraints::default(),
    )?;
    manager.commit(&seed)?;
    let old_reader = manager.begin();
    let delete = manager.begin();
    table.delete(&delete, &[0])?;
    manager.commit(&delete)?;
    // The new index must include row0 for old_reader, despite being created only
    // after the delete committed; its MainIndex callback never saw that delete.
    let registry = IndexTypeRegistry::default();
    registry.register(Arc::new(HnswIndexType))?;
    let build = manager.begin();
    table.create_index(&build, registry.create("hnsw", spec())?, true)?;
    manager.commit(&build)?;
    let entry = table.indexes().find("idx").unwrap();
    let active_before_cleanup = {
        let mut guard = entry.lock();
        let index = guard
            .index
            .as_secondary_mut()
            .unwrap()
            .as_any_mut()
            .downcast_mut::<HnswIndex>()
            .unwrap();
        assert_eq!(index.searchable_count(), 2);
        index.compact()?;
        assert!(index.contains(0));
        index.active_count()
    };
    let mut fetched = DataChunk::default();
    table.fetch(&old_reader, &mut fetched, &[StorageIndex::new(0)], &[0])?;
    assert_eq!(fetched.len(), 1);
    manager.rollback(&old_reader);
    let mut guard = entry.lock();
    let index = guard
        .index
        .as_secondary_mut()
        .unwrap()
        .as_any_mut()
        .downcast_mut::<HnswIndex>()
        .unwrap();
    assert_eq!(
        index.searchable_count(),
        1,
        "cleanup must retire a deletion that predates index creation"
    );
    assert_eq!(
        active_before_cleanup, 1,
        "old-visible row must not count as currently active"
    );
    assert!(!index.contains(0));
    assert_eq!(index.retired_count(), 0);
    index.compact()?;
    assert_eq!(index.stats().deleted, 0);
    Ok(())
}

#[test]
fn uncommitted_delete_during_build_does_not_retire_a_rolled_back_row() -> Result<()> {
    let table = table();
    let manager = TransactionManager::default();
    let seed = manager.begin();
    table.append_chunk(
        &seed,
        &chunk(&[Some([1.0, 0.0, 0.0])]),
        TableConstraints::default(),
    )?;
    manager.commit(&seed)?;
    let deletion = manager.begin();
    table.delete(&deletion, &[0])?;
    let builder = manager.begin();
    table.create_index(
        &builder,
        BoundIndex::Secondary(Box::new(HnswIndex::new(spec())?)),
        true,
    )?;
    manager.commit(&builder)?;
    manager.rollback(&deletion);
    let entry = table.indexes().find("idx").unwrap();
    let guard = entry.lock();
    let index = guard
        .index
        .as_secondary()
        .unwrap()
        .as_any()
        .downcast_ref::<HnswIndex>()
        .unwrap();
    assert_eq!(index.active_count(), 1);
    assert_eq!(index.retired_count(), 0);
    assert!(index.contains(0));
    Ok(())
}

#[test]
fn rebuilt_index_ignores_delayed_cleanup_for_reused_row_ids() -> Result<()> {
    let mut old = HnswIndex::new(spec())?;
    let old_row = chunk(&[Some([1.0, 0.0, 0.0])]);
    old.append(&old_row, &[0])?;
    old.remove(&old_row, &[0], IndexRemovalType::MainIndex)?;
    // Vacuum builds an empty definition and installs current rows with relocated
    // IDs. A prior generation's cleanup cannot remove an unmarked new row0.
    let mut rebuilt = old.empty_like()?;
    rebuilt.append(&chunk(&[Some([9.0, 0.0, 0.0])]), &[0])?;
    rebuilt.remove(&old_row, &[0], IndexRemovalType::DeletedRowsInUse)?;
    let rebuilt = rebuilt.as_any().downcast_ref::<HnswIndex>().unwrap();
    assert!(rebuilt.contains(0));
    assert_eq!(rebuilt.active_count(), 1);
    assert_eq!(
        rebuilt.search(&[9.0, 0.0, 0.0], 1, None)?.neighbors[0]
            .distance
            .to_bits(),
        0.0f64.to_bits()
    );
    Ok(())
}

#[test]
fn delete_commit_waits_for_index_publication_after_build_classification() -> Result<()> {
    use std::any::Any;
    use std::sync::{Mutex, mpsc};
    use std::time::{Duration, Instant};
    #[derive(Debug)]
    struct PausedIndex {
        inner: HnswIndex,
        started: mpsc::Sender<()>,
        resume: Mutex<mpsc::Receiver<()>>,
    }
    impl SecondaryIndex for PausedIndex {
        fn spec(&self) -> &SecondaryIndexSpec {
            self.inner.spec()
        }
        fn type_name(&self) -> &'static str {
            "HNSW"
        }
        fn append(&mut self, data: &DataChunk, rows: &[i64]) -> Result<()> {
            self.inner.append(data, rows)
        }
        fn revert_append(&mut self, data: &DataChunk, rows: &[i64]) -> Result<usize> {
            self.inner.revert_append(data, rows)
        }
        fn remove(&mut self, data: &DataChunk, rows: &[i64], mode: IndexRemovalType) -> Result<()> {
            self.inner.remove(data, rows, mode)
        }
        fn empty_like(&self) -> Result<Box<dyn SecondaryIndex>> {
            self.inner.empty_like()
        }
        fn compact(&mut self) -> Result<()> {
            self.started.send(()).unwrap();
            self.resume.lock().unwrap().recv().unwrap();
            self.inner.compact()
        }
        fn reset(&mut self) {
            self.inner.reset();
        }
        fn in_memory_size(&self) -> usize {
            self.inner.in_memory_size()
        }
        fn as_any(&self) -> &dyn Any {
            &self.inner
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            &mut self.inner
        }
    }
    let table = table();
    let manager = Arc::new(TransactionManager::default());
    let seed = manager.begin();
    table.append_chunk(
        &seed,
        &chunk(&[Some([1.0, 0.0, 0.0]), Some([2.0, 0.0, 0.0])]),
        TableConstraints::default(),
    )?;
    manager.commit(&seed)?;
    let old_reader = manager.begin();
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let build_table = Arc::clone(&table);
    let build_manager = Arc::clone(&manager);
    let builder = std::thread::spawn(move || -> Result<()> {
        let transaction = build_manager.begin();
        build_table.create_index(
            &transaction,
            BoundIndex::Secondary(Box::new(PausedIndex {
                inner: HnswIndex::new(spec())?,
                started: started_tx,
                resume: Mutex::new(resume_rx),
            })),
            true,
        )?;
        build_manager.commit(&transaction)
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // Build scanning/classification is complete, but publication is paused under
    // append_lock. The delete is stamped now and must maintain the newly published index.
    let deletion = manager.begin();
    table.delete(&deletion, &[0])?;
    let commit_manager = Arc::clone(&manager);
    let deleter = std::thread::spawn(move || commit_manager.commit(&deletion));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let committed_rows: u64 = table
            .row_groups()
            .row_groups()
            .tree()
            .nodes()
            .iter()
            .map(|node| node.node.committed_row_count())
            .sum();
        if committed_rows == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "delete did not reach version stamping");
        std::thread::yield_now();
    }
    resume_tx.send(()).unwrap();
    builder.join().unwrap()?;
    deleter.join().unwrap()?;
    let entry = table.indexes().find("idx").unwrap();
    {
        let guard = entry.lock();
        let index = guard
            .index
            .as_secondary()
            .unwrap()
            .as_any()
            .downcast_ref::<HnswIndex>()
            .unwrap();
        assert_eq!(index.searchable_count(), 2);
        assert_eq!(index.active_count(), 1);
        assert!(index.is_retired(0));
    }
    manager.rollback(&old_reader);
    let guard = entry.lock();
    let index = guard
        .index
        .as_secondary()
        .unwrap()
        .as_any()
        .downcast_ref::<HnswIndex>()
        .unwrap();
    assert_eq!(index.searchable_count(), 1);
    assert!(!index.contains(0));
    Ok(())
}
