//! The storage-backed physical operators: CREATE TABLE, INSERT (with RETURNING), table
//! scans with filters, DELETE and UPDATE, run through the executor.
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use rustdb_buffer::{BufferManager, BufferManagerConfig, InMemoryBlockManager};
use rustdb_common::Result;
use rustdb_execution::operators::{
    CollectedResult, InsertTarget, PhysicalColumnDataScan, PhysicalCreateTable, PhysicalDelete,
    PhysicalInsert, PhysicalResultCollector, PhysicalSeqScan, PhysicalUpdate, TableCreator,
};
use rustdb_execution::{Executor, PhysicalRef};
use rustdb_storage::table_filter::{TableFilter, TableFilterSet};
use rustdb_storage::{
    ColumnDefinition, DataTable, StorageIndex, TableConstraints, TableIoManager, Transaction,
    TransactionManager,
};
use rustdb_types::compare::CompareOp;
use rustdb_types::{DataChunk, LogicalType, Value};

/// Creates the test table and remembers it, standing in for the catalog.
#[derive(Debug)]
struct Creator {
    io: Arc<TableIoManager>,
    created: Mutex<Option<Arc<DataTable>>>,
}

impl TableCreator for Creator {
    fn create(&self, _: &Transaction) -> Result<Option<Arc<DataTable>>> {
        let table = DataTable::new(
            Arc::clone(&self.io),
            "main",
            "t",
            vec![
                ColumnDefinition::new("i", LogicalType::INTEGER),
                ColumnDefinition::new("s", LogicalType::VARCHAR),
            ],
        );
        *self.created.lock().unwrap() = Some(Arc::clone(&table));
        Ok(Some(table))
    }
}

fn run(plan: PhysicalRef) -> Vec<Vec<Value>> {
    let root: PhysicalRef = Arc::new(PhysicalResultCollector::new(plan));
    let state = Executor::execute(&root, rustdb_execution::ClientEnv::standalone().unwrap()).unwrap();
    let collected = state
        .downcast_ref::<Mutex<CollectedResult>>()
        .unwrap()
        .lock()
        .unwrap();
    collected
        .chunks
        .iter()
        .flat_map(|chunk| {
            (0..chunk.len()).map(move |row| {
                (0..chunk.column_count())
                    .map(|c| chunk.column(c).get_value(row))
                    .collect()
            })
        })
        .collect()
}

fn values_scan(types: Vec<LogicalType>, rows: &[Vec<Value>]) -> PhysicalRef {
    let mut chunk = DataChunk::with_types(&types, rows.len());
    for (row, values) in rows.iter().enumerate() {
        for (column, value) in values.iter().enumerate() {
            chunk.column_mut(column).set_value(row, value).unwrap();
        }
    }
    chunk.set_len(rows.len());
    Arc::new(PhysicalColumnDataScan::new(types, Arc::from(vec![chunk])))
}

fn scan(
    table: &Arc<DataTable>,
    transaction: &Arc<Transaction>,
    filters: Option<TableFilterSet>,
) -> Vec<Vec<Value>> {
    run(Arc::new(PhysicalSeqScan::new(
        vec![LogicalType::INTEGER, LogicalType::VARCHAR],
        Arc::clone(table),
        Arc::clone(transaction),
        vec![StorageIndex::new(0), StorageIndex::new(1)],
        filters,
        Vec::new(),
    )))
}

#[test]
fn dml_through_the_executor() {
    let buffer_manager = BufferManager::new(BufferManagerConfig::default()).unwrap();
    let io = Arc::new(TableIoManager::new(
        buffer_manager,
        Arc::new(InMemoryBlockManager::default()),
    ));
    let creator = Arc::new(Creator {
        io,
        created: Mutex::default(),
    });
    let manager = TransactionManager::default();
    let transaction = Arc::new(manager.begin());

    let created = run(Arc::new(PhysicalCreateTable::new(
        creator.clone(),
        Arc::clone(&transaction),
    )));
    assert_eq!(created, Vec::<Vec<Value>>::new());
    let table = creator.created.lock().unwrap().clone().unwrap();

    let types = vec![LogicalType::INTEGER, LogicalType::VARCHAR];
    let rows: Vec<Vec<Value>> = (0..5)
        .map(|i| vec![Value::integer(i), Value::varchar(format!("row {i}"))])
        .collect();
    let insert = PhysicalInsert::new(
        values_scan(types.clone(), &rows),
        InsertTarget::Table(Arc::clone(&table)),
        Arc::clone(&transaction),
        TableConstraints::default(),
        true,
    );
    assert_eq!(run(Arc::new(insert)), rows, "RETURNING yields the inserted rows");

    let mut filters = TableFilterSet::default();
    filters.push_filter(
        0,
        TableFilter::ConstantComparison {
            op: CompareOp::GreaterThanOrEqual,
            constant: Value::integer(3),
        },
    );
    assert_eq!(scan(&table, &transaction, Some(filters)), rows[3..]);

    // DELETE WHERE i < 2: scan row ids with a filter, then delete
    let mut filters = TableFilterSet::default();
    filters.push_filter(
        1,
        TableFilter::ConstantComparison {
            op: CompareOp::LessThan,
            constant: Value::integer(2),
        },
    );
    let row_ids = Arc::new(PhysicalSeqScan::new(
        vec![LogicalType::BIGINT],
        Arc::clone(&table),
        Arc::clone(&transaction),
        vec![StorageIndex::row_id(), StorageIndex::new(0)],
        Some(filters),
        vec![0],
    ));
    let delete = PhysicalDelete::new(row_ids, Arc::clone(&table), Arc::clone(&transaction), 0, false);
    assert_eq!(run(Arc::new(delete)), vec![vec![Value::bigint(2)]]);
    manager.commit(&transaction).unwrap();

    // the committed rows are compacted: i = 2, 3, 4 have row ids 0, 1, 2
    // UPDATE t SET s = 'updated' WHERE i = 4
    let transaction = Arc::new(manager.begin());
    let input = values_scan(
        vec![LogicalType::VARCHAR, LogicalType::BIGINT],
        &[vec![Value::varchar("updated"), Value::bigint(2)]],
    );
    let update = PhysicalUpdate::new(
        input,
        Arc::clone(&table),
        Arc::clone(&transaction),
        vec![1],
        TableConstraints::default(),
        false,
        false,
    );
    assert_eq!(run(Arc::new(update)), vec![vec![Value::bigint(1)]]);
    let mut expected = rows[2..].to_vec();
    expected[2][1] = Value::varchar("updated");
    assert_eq!(scan(&table, &transaction, None), expected);
    // the same as a delete + insert moves the row to the end
    let input = values_scan(
        vec![LogicalType::INTEGER, LogicalType::BIGINT],
        &[
            vec![Value::integer(20), Value::bigint(0)],
            vec![Value::integer(21), Value::bigint(0)],
        ],
    );
    let update = PhysicalUpdate::new(
        input,
        Arc::clone(&table),
        Arc::clone(&transaction),
        vec![0],
        TableConstraints::default(),
        true,
        true,
    );
    assert_eq!(
        run(Arc::new(update)),
        vec![vec![Value::integer(20), Value::varchar("row 2")]],
        "each row is updated once per statement"
    );
    manager.commit(&transaction).unwrap();
    let reader = Arc::new(manager.begin());
    let result = scan(&table, &reader, None);
    assert_eq!(
        result,
        vec![
            vec![Value::integer(3), Value::varchar("row 3")],
            vec![Value::integer(4), Value::varchar("updated")],
            vec![Value::integer(20), Value::varchar("row 2")],
        ]
    );
    manager.commit(&reader).unwrap();
}

#[test]
fn index_scans_return_the_same_rows_as_table_scans() {
    let buffer_manager = BufferManager::new(BufferManagerConfig::default()).unwrap();
    let io = Arc::new(TableIoManager::new(
        buffer_manager,
        Arc::new(InMemoryBlockManager::default()),
    ));
    let table = DataTable::new(
        io,
        "main",
        "t",
        vec![
            ColumnDefinition::new("i", LogicalType::INTEGER),
            ColumnDefinition::new("s", LogicalType::VARCHAR),
        ],
    );
    let index = table
        .new_art("idx", rustdb_storage::index::IndexConstraintType::None, &[0])
        .unwrap();
    table.add_index(index).unwrap();
    let manager = TransactionManager::default();
    let writer = Arc::new(manager.begin());
    let types = vec![LogicalType::INTEGER, LogicalType::VARCHAR];
    let rows: Vec<Vec<Value>> = (0..3000)
        .map(|i| vec![Value::integer(i % 100), Value::varchar(format!("row {i}"))])
        .collect();
    for batch in rows.chunks(1000) {
        let insert = PhysicalInsert::new(
            values_scan(types.clone(), batch),
            InsertTarget::Table(Arc::clone(&table)),
            Arc::clone(&writer),
            TableConstraints::default(),
            false,
        );
        run(Arc::new(insert));
    }
    manager.commit(&writer).unwrap();
    // uncommitted rows of the reader are found by its scans too
    let reader = Arc::new(manager.begin());
    let insert = PhysicalInsert::new(
        values_scan(types.clone(), &[vec![Value::integer(7), Value::varchar("local")]]),
        InsertTarget::Table(Arc::clone(&table)),
        Arc::clone(&reader),
        TableConstraints::default(),
        false,
    );
    run(Arc::new(insert));
    for op in [
        CompareOp::Equal,
        CompareOp::LessThan,
        CompareOp::GreaterThanOrEqual,
    ] {
        let mut filters = TableFilterSet::default();
        filters.push_filter(
            0,
            TableFilter::ConstantComparison {
                op,
                constant: Value::integer(7),
            },
        );
        let make = |index: bool| {
            Arc::new(
                PhysicalSeqScan::new(
                    vec![LogicalType::VARCHAR],
                    Arc::clone(&table),
                    Arc::clone(&reader),
                    vec![StorageIndex::new(0), StorageIndex::new(1)],
                    Some(filters.clone()),
                    vec![1],
                )
                .with_index_scan(index.then_some((100_000, 0.001))),
            )
        };
        let mut with_index = run(make(true));
        let mut without = run(make(false));
        with_index.sort_by_key(|row| row[0].to_varchar().unwrap());
        without.sort_by_key(|row| row[0].to_varchar().unwrap());
        assert_ne!(without.len(), 0);
        assert_eq!(with_index, without, "{op:?}");
    }
    manager.commit(&reader).unwrap();
}
