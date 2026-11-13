//! Secondary index hooks through real table builds, commits, rollback and MVCC.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use roastedduck_buffer::{BufferManager, BufferManagerConfig, InMemoryBlockManager};
use roastedduck_common::{Error, Result};
use roastedduck_function::ObjectCache;
use roastedduck_storage::index::{
    BoundIndex, IndexConstraintType, IndexKeyExpressions, IndexRemovalType, IndexType, IndexTypeRegistry,
    SecondaryIndex, SecondaryIndexSpec,
};
use roastedduck_storage::table::StorageIndex;
use roastedduck_storage::{
    ColumnDefinition, DataTable, TableConstraints, TableIoManager, Transaction, TransactionManager,
};
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};

/// A test-only row-key index, deliberately unrelated to HNSW. Its candidate map
/// lets tests inspect committed IDs independently of table storage visibility.
#[derive(Debug)]
struct RowIndex {
    spec: SecondaryIndexSpec,
    rows: BTreeMap<i64, i32>,
    retired: BTreeSet<i64>,
    reject: Option<i32>,
    removals: Vec<IndexRemovalType>,
}

impl SecondaryIndex for RowIndex {
    fn spec(&self) -> &SecondaryIndexSpec {
        &self.spec
    }
    fn type_name(&self) -> &'static str {
        "ROW_TEST"
    }
    fn append(&mut self, table_chunk: &DataChunk, row_ids: &[i64]) -> Result<()> {
        let keys = self.spec.execute_expressions(table_chunk)?;
        let mut pending = Vec::new();
        for (row, &id) in row_ids.iter().enumerate() {
            let value = keys
                .column(0)
                .get_value(row)
                .as_i32()
                .ok_or_else(|| Error::invalid_input("test key is null"))?;
            if self.reject == Some(value) {
                return Err(Error::invalid_input("test secondary append rejected"));
            }
            pending.push((id, value));
        }
        self.rows.extend(pending);
        Ok(())
    }
    fn revert_append(&mut self, _chunk: &DataChunk, rows: &[i64]) -> Result<usize> {
        let mut removed = 0;
        for row in rows {
            removed += usize::from(self.rows.remove(row).is_some());
            self.retired.remove(row);
        }
        Ok(removed)
    }
    fn remove(&mut self, chunk: &DataChunk, rows: &[i64], removal: IndexRemovalType) -> Result<()> {
        self.removals.push(removal);
        match removal {
            IndexRemovalType::MainIndex => self.retired.extend(rows),
            IndexRemovalType::MainIndexOnly => {
                self.revert_append(chunk, rows)?;
            }
            IndexRemovalType::RevertMainIndex => {
                for row in rows {
                    self.retired.remove(row);
                }
            }
            IndexRemovalType::RevertMainIndexOnly => self.append(chunk, rows)?,
            IndexRemovalType::DeletedRowsInUse => {
                for row in rows {
                    if self.retired.remove(row) {
                        self.rows.remove(row);
                    }
                }
            }
        }
        Ok(())
    }
    fn empty_like(&self) -> Result<Box<dyn SecondaryIndex>> {
        Ok(Box::new(Self {
            spec: self.spec.clone(),
            rows: BTreeMap::new(),
            retired: BTreeSet::new(),
            reject: self.reject,
            removals: Vec::new(),
        }))
    }
    fn compact(&mut self) -> Result<()> {
        Ok(())
    }
    fn reset(&mut self) {
        self.rows.clear();
        self.retired.clear();
    }
    fn in_memory_size(&self) -> usize {
        self.rows.len() * 16 + self.retired.len() * 8
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[derive(Debug)]
struct RowIndexType;
impl IndexType for RowIndexType {
    fn name(&self) -> &'static str {
        "ROW_TEST"
    }
    fn create(&self, spec: SecondaryIndexSpec) -> Result<Box<dyn SecondaryIndex>> {
        let reject = spec.options.get("reject").and_then(Value::as_i32);
        Ok(Box::new(RowIndex {
            spec,
            rows: BTreeMap::new(),
            retired: BTreeSet::new(),
            reject,
            removals: Vec::new(),
        }))
    }
}

fn spec(name: &str, reject: Option<i32>) -> SecondaryIndexSpec {
    let mut options = roastedduck_common::CaseInsensitiveMap::default();
    if let Some(value) = reject {
        options.insert("reject", Value::integer(value));
    }
    SecondaryIndexSpec {
        constraint: IndexConstraintType::None,
        name: name.into(),
        column_ids: vec![0],
        key_types: vec![LogicalType::INTEGER],
        expressions: IndexKeyExpressions::Columns(vec!["i".into()]),
        options,
        persistent: false,
    }
}
fn registry() -> IndexTypeRegistry {
    let registry = IndexTypeRegistry::default();
    registry.register(Arc::new(RowIndexType)).unwrap();
    registry
}
fn table() -> Arc<DataTable> {
    let io = Arc::new(TableIoManager::new(
        BufferManager::new(BufferManagerConfig::default()).unwrap(),
        Arc::new(InMemoryBlockManager::default()),
    ));
    DataTable::new(
        io,
        "main",
        "t",
        vec![ColumnDefinition::new("i", LogicalType::INTEGER)],
    )
}
fn insert(table: &Arc<DataTable>, txn: &Transaction, values: &[i32]) {
    let mut column = Vector::flat(LogicalType::INTEGER, values.len());
    for (row, &value) in values.iter().enumerate() {
        column.set_value(row, &Value::integer(value)).unwrap();
    }
    table
        .append_chunk(
            txn,
            &DataChunk::new(vec![column], values.len()),
            TableConstraints::default(),
        )
        .unwrap();
}
fn rows(table: &DataTable, name: &str) -> (BTreeMap<i64, i32>, BTreeSet<i64>) {
    let entry = table.indexes().find(name).unwrap();
    let guard = entry.lock();
    let index = guard
        .index
        .as_secondary()
        .unwrap()
        .as_any()
        .downcast_ref::<RowIndex>()
        .unwrap();
    (index.rows.clone(), index.retired.clone())
}

#[test]
fn registry_is_database_owned_and_rejects_replacement() {
    let cache = ObjectCache::default();
    let other_cache = ObjectCache::default();
    let first = Arc::new(registry());
    let second = Arc::new(IndexTypeRegistry::default());
    first.attach(&cache).unwrap();
    first.attach(&cache).unwrap();
    assert!(second.attach(&cache).is_err());
    second.attach(&other_cache).unwrap();
    assert!(
        IndexTypeRegistry::from_cache(&cache)
            .unwrap()
            .contains("row_test")
    );
    assert!(
        !IndexTypeRegistry::from_cache(&other_cache)
            .unwrap()
            .contains("ROW_TEST")
    );
    assert!(first.register(Arc::new(RowIndexType)).is_err());
    let mut persistent = spec("persisted", None);
    persistent.persistent = true;
    assert!(first.create("row_test", persistent).is_err());
}

#[test]
fn failed_build_never_publishes_and_failed_commit_compensates_all_indexes() {
    let table = table();
    let manager = TransactionManager::default();
    let registry = registry();
    let seed = manager.begin();
    insert(&table, &seed, &[1]);
    manager.commit(&seed).unwrap();
    let build = manager.begin();
    assert!(
        table
            .create_index(
                &build,
                registry.create("row_test", spec("bad_build", Some(1))).unwrap(),
                true
            )
            .is_err()
    );
    assert!(!table.indexes().contains("bad_build"));
    manager.rollback(&build);
    let build = manager.begin();
    table
        .create_index(
            &build,
            registry.create("row_test", spec("first", None)).unwrap(),
            true,
        )
        .unwrap();
    table
        .create_index(
            &build,
            table
                .new_art("ordinary", IndexConstraintType::None, &[0])
                .unwrap(),
            true,
        )
        .unwrap();
    table
        .create_index(
            &build,
            registry.create("row_test", spec("rejector", Some(9))).unwrap(),
            true,
        )
        .unwrap();
    manager.commit(&build).unwrap();
    let before = rows(&table, "first");
    let writer = manager.begin();
    table.delete(&writer, &[0]).unwrap();
    insert(&table, &writer, &[2, 9]);
    assert!(
        manager
            .commit(&writer)
            .unwrap_err()
            .to_string()
            .contains("test secondary append rejected")
    );
    assert_eq!(rows(&table, "first"), before);
    assert_eq!(rows(&table, "rejector"), before);
    let successful = manager.begin();
    insert(&table, &successful, &[3]);
    manager.commit(&successful).unwrap();
    assert_eq!(
        rows(&table, "first").0.values().copied().collect::<Vec<_>>(),
        vec![1, 3]
    );
}

#[test]
fn deleted_candidates_survive_old_readers_until_cleanup() {
    let table = table();
    let manager = TransactionManager::default();
    let registry = registry();
    let create = manager.begin();
    table
        .create_index(
            &create,
            registry.create("row_test", spec("idx", None)).unwrap(),
            true,
        )
        .unwrap();
    insert(&table, &create, &[4, 5]);
    manager.commit(&create).unwrap();
    let old_reader = manager.begin();
    let writer = manager.begin();
    table.delete(&writer, &[0]).unwrap();
    manager.commit(&writer).unwrap();
    assert_eq!(rows(&table, "idx").0.get(&0), Some(&4));
    assert!(rows(&table, "idx").1.contains(&0));
    table.vacuum_indexes().unwrap();
    assert!(rows(&table, "idx").0.contains_key(&0));
    let mut fetched = DataChunk::with_types(&[LogicalType::INTEGER], 2);
    table
        .fetch(&old_reader, &mut fetched, &[StorageIndex::new(0)], &[0, 1])
        .unwrap();
    assert_eq!(fetched.len(), 2);
    let new_reader = manager.begin();
    fetched.reset();
    table
        .fetch(&new_reader, &mut fetched, &[StorageIndex::new(0)], &[0, 1])
        .unwrap();
    assert_eq!(fetched.len(), 1);
    manager.rollback(&new_reader);
    manager.rollback(&old_reader);
    assert!(!rows(&table, "idx").0.contains_key(&0));
    assert!(rows(&table, "idx").1.is_empty());
    let entry = table.indexes().find("idx").unwrap();
    assert!(table.remove_index("idx"));
    assert_eq!(entry.lock().index.in_memory_size(), 0);
}

#[test]
fn secondary_indexes_never_enter_local_constraint_deltas() {
    let table = table();
    let registry = registry();
    table
        .add_index(registry.create("row_test", spec("secondary", None)).unwrap())
        .unwrap();
    table
        .add_constraint_index(IndexConstraintType::Unique, &[0], 0)
        .unwrap();
    let deltas = table.indexes().delta_list().unwrap();
    assert_eq!(deltas.len(), 1);
    assert!(matches!(deltas.entries()[0].lock().index, BoundIndex::Art(_)));
    assert!(!deltas.contains("secondary"));
}

#[test]
fn vacuum_relocates_secondary_candidates_together_with_art() {
    use roastedduck_storage::checkpoint::table_writer::{TableWriteSettings, checkpoint_in_memory_table};
    use roastedduck_storage::checkpoint_lock::CheckpointLock;
    use roastedduck_storage::compression::CompressionConfig;

    let io = Arc::new(TableIoManager::with_row_group_size(
        BufferManager::new(BufferManagerConfig::default()).unwrap(),
        Arc::new(InMemoryBlockManager::default()),
        2048,
    ));
    let vacuum_lock = Arc::new(CheckpointLock::default());
    io.set_vacuum_rebuild_index_threshold(10_000, &vacuum_lock);
    let table = DataTable::new(
        io,
        "main",
        "vacuumed",
        vec![ColumnDefinition::new("i", LogicalType::INTEGER)],
    );
    let manager = TransactionManager::default();
    let registry = registry();
    let create = manager.begin();
    table
        .create_index(
            &create,
            registry.create("row_test", spec("secondary", None)).unwrap(),
            true,
        )
        .unwrap();
    table
        .create_index(
            &create,
            table
                .new_art("ordinary", IndexConstraintType::None, &[0])
                .unwrap(),
            true,
        )
        .unwrap();
    insert(&table, &create, &(0..4096).collect::<Vec<_>>());
    manager.commit(&create).unwrap();
    let delete = manager.begin();
    table.delete(&delete, &(0..2048).collect::<Vec<_>>()).unwrap();
    manager.commit(&delete).unwrap();
    assert_eq!(
        rows(&table, "secondary").0.first_key_value(),
        Some((&2048, &2048))
    );
    checkpoint_in_memory_table(
        &table,
        CompressionConfig::default(),
        TableWriteSettings {
            full_checkpoint: true,
            ..TableWriteSettings::default()
        },
        true,
    )
    .unwrap();
    let candidates = rows(&table, "secondary").0;
    assert_eq!(candidates.len(), 2048);
    assert_eq!(candidates.first_key_value(), Some((&0, &2048)));
    assert_eq!(candidates.last_key_value(), Some((&2047, &4095)));
    let reader = manager.begin();
    let mut fetched = DataChunk::with_types(&[LogicalType::INTEGER], 2);
    table
        .fetch(&reader, &mut fetched, &[StorageIndex::new(0)], &[0, 2047])
        .unwrap();
    assert_eq!(fetched.column(0).get_value(0), Value::integer(2048));
    assert_eq!(fetched.column(0).get_value(1), Value::integer(4095));
    let entry = table.indexes().find("ordinary").unwrap();
    let mut row_ids = BTreeSet::new();
    entry
        .lock()
        .index
        .as_art()
        .unwrap()
        .search_equal(
            &roastedduck_storage::index::art::create_key(&Value::integer(2048)).unwrap(),
            2,
            &mut row_ids,
        )
        .unwrap();
    assert_eq!(row_ids, BTreeSet::from([0]));
    manager.rollback(&reader);
}

#[test]
fn failed_later_delete_restores_secondary_rows_with_and_without_old_readers() {
    for retain_old_rows in [false, true] {
        let first = table();
        let later = table();
        let manager = TransactionManager::default();
        let registry = registry();
        let seed = manager.begin();
        first
            .create_index(
                &seed,
                registry.create("row_test", spec("idx", None)).unwrap(),
                true,
            )
            .unwrap();
        insert(&first, &seed, &[7]);
        insert(&later, &seed, &[8]);
        manager.commit(&seed).unwrap();
        let old_reader = retain_old_rows.then(|| manager.begin());
        let writer = manager.begin();
        first.delete(&writer, &[0]).unwrap();
        later.delete(&writer, &[0]).unwrap();
        // Force a real later commit failure after the first table's deletion was
        // stamped and its index maintained, exercising UndoBuffer compensation.
        later.set_as_dropped();
        assert!(manager.commit(&writer).is_err());
        later.set_as_main_table();
        let (candidates, retired) = rows(&first, "idx");
        assert_eq!(candidates, BTreeMap::from([(0, 7)]));
        assert!(retired.is_empty());
        let entry = first.indexes().find("idx").unwrap();
        let guard = entry.lock();
        let index = guard
            .index
            .as_secondary()
            .unwrap()
            .as_any()
            .downcast_ref::<RowIndex>()
            .unwrap();
        let (removed, reverted) = if retain_old_rows {
            (IndexRemovalType::MainIndex, IndexRemovalType::RevertMainIndex)
        } else {
            (
                IndexRemovalType::MainIndexOnly,
                IndexRemovalType::RevertMainIndexOnly,
            )
        };
        assert!(index.removals.contains(&removed));
        assert!(index.removals.contains(&reverted));
        drop(guard);
        if let Some(reader) = old_reader {
            manager.rollback(&reader);
        }
    }
}
