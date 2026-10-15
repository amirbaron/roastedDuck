//! MVCC behaviour of tables: visibility of inserts, deletes and updates across
//! transactions, rollback, write-write conflicts and cleanup.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use rustdb_buffer::{BufferManager, BufferManagerConfig, InMemoryBlockManager};
use rustdb_storage::table::StorageIndex;
use rustdb_storage::{
    ColumnDefinition, DataTable, TableConstraints, TableIoManager, TableScanState, Transaction,
    TransactionManager,
};
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

fn io() -> Arc<TableIoManager> {
    let buffer_manager = BufferManager::new(BufferManagerConfig::default()).unwrap();
    Arc::new(TableIoManager::new(
        buffer_manager,
        Arc::new(InMemoryBlockManager::default()),
    ))
}

fn table() -> Arc<DataTable> {
    DataTable::new(
        io(),
        "main",
        "t",
        vec![
            ColumnDefinition::new("i", LogicalType::INTEGER),
            ColumnDefinition::new("s", LogicalType::VARCHAR),
        ],
    )
}

fn chunk(rows: &[(i32, &str)]) -> DataChunk {
    let mut ints = Vector::flat(LogicalType::INTEGER, rows.len());
    let mut strings = Vector::flat(LogicalType::VARCHAR, rows.len());
    for (row, (i, s)) in rows.iter().enumerate() {
        ints.set_value(row, &Value::integer(*i)).unwrap();
        strings.set_value(row, &Value::varchar(s)).unwrap();
    }
    DataChunk::new(vec![ints, strings], rows.len())
}

fn insert(table: &Arc<DataTable>, transaction: &Transaction, rows: &[(i32, &str)]) {
    table
        .append_chunk(transaction, &chunk(rows), TableConstraints::default())
        .unwrap();
}

/// Every visible row as `(rowid, i, s)`.
fn scan(table: &Arc<DataTable>, transaction: &Transaction) -> Vec<(i64, i32, String)> {
    let mut state = TableScanState::default();
    table
        .initialize_scan(
            transaction,
            &mut state,
            vec![StorageIndex::row_id(), StorageIndex::new(0), StorageIndex::new(1)],
            None,
        )
        .unwrap();
    let mut rows = Vec::new();
    let mut result = DataChunk::default();
    loop {
        table.scan(transaction, &mut result, &mut state).unwrap();
        if result.is_empty() {
            return rows;
        }
        for row in 0..result.len() {
            let row_id = result.column(0).get_value(row).as_i64().unwrap();
            let i = result.column(1).get_value(row).as_i32().unwrap_or(-1);
            let s = result
                .column(2)
                .get_value(row)
                .as_str()
                .map_or_else(|| "NULL".to_owned(), str::to_owned);
            rows.push((row_id, i, s));
        }
    }
}

fn values(rows: &[(i64, i32, String)]) -> Vec<i32> {
    rows.iter().map(|(_, i, _)| *i).collect()
}

#[test]
fn inserts_are_isolated_until_commit() {
    let manager = TransactionManager::default();
    let table = table();
    let t1 = manager.begin();
    insert(&table, &t1, &[(1, "a"), (2, "b")]);
    let t2 = manager.begin();
    assert_eq!(values(&scan(&table, &t1)), [1, 2], "own inserts are visible");
    assert!(scan(&table, &t2).is_empty(), "uncommitted inserts are not");
    // local rows carry transaction-local row ids
    assert!(scan(&table, &t1)[0].0 >= rustdb_storage::MAX_ROW_ID as i64);
    manager.commit(&t1).unwrap();
    assert!(scan(&table, &t2).is_empty(), "t2 started before the commit");
    let t3 = manager.begin();
    let rows = scan(&table, &t3);
    assert_eq!(values(&rows), [1, 2]);
    assert_eq!(rows[0].0, 0, "committed rows have table row ids");
    manager.commit(&t2).unwrap();
    manager.commit(&t3).unwrap();
}

#[test]
fn rollback_discards_inserts_deletes_and_updates() {
    let manager = TransactionManager::default();
    let table = table();
    let setup = manager.begin();
    insert(&table, &setup, &[(1, "a"), (2, "b"), (3, "c")]);
    manager.commit(&setup).unwrap();

    let t1 = manager.begin();
    assert_eq!(table.delete(&t1, &[0]).unwrap(), 1);
    let mut update = Vector::flat(LogicalType::VARCHAR, 1);
    update
        .set_value(0, &Value::varchar("a much longer replacement value"))
        .unwrap();
    table
        .update(
            &t1,
            &[1],
            &[1],
            &DataChunk::new(vec![update], 1),
            &TableConstraints::default(),
        )
        .unwrap();
    insert(&table, &t1, &[(4, "d")]);
    let seen = scan(&table, &t1);
    assert_eq!(values(&seen), [2, 3, 4]);
    assert_eq!(seen[0].2, "a much longer replacement value");
    manager.rollback(&t1);

    let t2 = manager.begin();
    let rows = scan(&table, &t2);
    assert_eq!(values(&rows), [1, 2, 3]);
    assert_eq!(rows[1].2, "b");
    manager.commit(&t2).unwrap();
}

#[test]
fn write_write_conflicts_use_duckdb_messages() {
    let manager = TransactionManager::default();
    let table = table();
    let setup = manager.begin();
    insert(&table, &setup, &[(1, "a"), (2, "b")]);
    manager.commit(&setup).unwrap();

    let t1 = manager.begin();
    let t2 = manager.begin();
    table.delete(&t1, &[0]).unwrap();
    let error = table.delete(&t2, &[0]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "TransactionContext Error: Conflict on tuple deletion!"
    );

    let ints = |value| {
        let mut vector = Vector::flat(LogicalType::INTEGER, 1);
        vector.set_value(0, &Value::integer(value)).unwrap();
        DataChunk::new(vec![vector], 1)
    };
    table
        .update(&t1, &[1], &[0], &ints(20), &TableConstraints::default())
        .unwrap();
    let error = table
        .update(&t2, &[1], &[0], &ints(30), &TableConstraints::default())
        .unwrap_err();
    assert_eq!(error.to_string(), "TransactionContext Error: Conflict on update!");
    manager.commit(&t1).unwrap();
    // t2 started before t1 committed: updating the row is still a conflict
    let error = table
        .update(&t2, &[1], &[0], &ints(40), &TableConstraints::default())
        .unwrap_err();
    assert_eq!(error.to_string(), "TransactionContext Error: Conflict on update!");
    manager.rollback(&t2);

    let t3 = manager.begin();
    assert_eq!(values(&scan(&table, &t3)), [20]);
    manager.commit(&t3).unwrap();
}

#[test]
fn snapshots_see_old_versions_until_cleanup() {
    let manager = TransactionManager::default();
    let table = table();
    let setup = manager.begin();
    insert(&table, &setup, &[(1, "a"), (2, "b")]);
    manager.commit(&setup).unwrap();

    let reader = manager.begin();
    let writer = manager.begin();
    let mut update = Vector::flat(LogicalType::INTEGER, 2);
    update.set_value(0, &Value::integer(10)).unwrap();
    update.set_value(1, &Value::integer(20)).unwrap();
    table
        .update(
            &writer,
            &[0, 1],
            &[0],
            &DataChunk::new(vec![update], 2),
            &TableConstraints::default(),
        )
        .unwrap();
    table.delete(&writer, &[0]).unwrap();
    manager.commit(&writer).unwrap();
    assert_eq!(
        values(&scan(&table, &reader)),
        [1, 2],
        "the reader keeps its snapshot"
    );
    manager.commit(&reader).unwrap();
    let late = manager.begin();
    assert_eq!(values(&scan(&table, &late)), [20]);
    manager.commit(&late).unwrap();
}

#[test]
fn local_deletes_and_updates_apply_to_transaction_local_rows() {
    let manager = TransactionManager::default();
    let table = table();
    let t1 = manager.begin();
    insert(&table, &t1, &[(1, "a"), (2, "b"), (3, "c")]);
    let rows = scan(&table, &t1);
    assert_eq!(table.delete(&t1, &[rows[1].0]).unwrap(), 1);
    let mut update = Vector::flat(LogicalType::INTEGER, 1);
    update.set_value(0, &Value::integer(30)).unwrap();
    table
        .update(
            &t1,
            &[rows[2].0],
            &[0],
            &DataChunk::new(vec![update], 1),
            &TableConstraints::default(),
        )
        .unwrap();
    assert_eq!(values(&scan(&table, &t1)), [1, 30]);
    manager.commit(&t1).unwrap();
    let t2 = manager.begin();
    let rows = scan(&table, &t2);
    assert_eq!(values(&rows), [1, 30]);
    assert_eq!((rows[0].0, rows[1].0), (0, 1), "rows are compacted when flushed");
    manager.commit(&t2).unwrap();
}

#[test]
fn not_null_constraints_reject_rows() {
    let manager = TransactionManager::default();
    let table = table();
    let t1 = manager.begin();
    let mut rows = chunk(&[(1, "a")]);
    rows.column_mut(1)
        .set_value(0, &Value::null(LogicalType::VARCHAR))
        .unwrap();
    let constraints = TableConstraints {
        not_null: vec![1],
        ..TableConstraints::default()
    };
    let error = table.append_chunk(&t1, &rows, constraints).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Constraint Error: NOT NULL constraint failed: t.s"
    );
    manager.rollback(&t1);
}

#[test]
fn large_appends_span_row_groups_and_scan_in_parallel() {
    let manager = TransactionManager::default();
    let table = table();
    let total = 300_000i32;
    let t1 = manager.begin();
    let mut state = table
        .initialize_local_append(&t1, TableConstraints::default())
        .unwrap();
    let mut next = 0;
    while next < total {
        let count = (total - next).min(2048) as usize;
        let mut ints = Vector::flat(LogicalType::INTEGER, count);
        ints.values_mut::<i32>()[..count]
            .iter_mut()
            .enumerate()
            .for_each(|(row, value)| *value = next + row as i32);
        let strings = Vector::constant(&Value::varchar("constant")).unwrap();
        table
            .local_append(&mut state, &DataChunk::new(vec![ints, strings], count))
            .unwrap();
        next += count as i32;
    }
    table.finalize_local_append(state);
    manager.commit(&t1).unwrap();
    assert_eq!(table.row_group_count(), 3);

    let t2 = manager.begin();
    let parallel = table.initialize_parallel_scan(&t2);
    let sums: Vec<i64> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    let mut state = TableScanState::default();
                    state.initialize(vec![StorageIndex::new(0)], None);
                    let mut sum = 0i64;
                    let mut chunk = DataChunk::default();
                    while table.next_parallel_scan(&t2, &parallel, &mut state).unwrap() {
                        while table.scan_morsel(&t2, &mut chunk, &mut state).unwrap() {
                            for row in 0..chunk.len() {
                                sum += i64::from(chunk.column(0).get_value(row).as_i32().unwrap());
                            }
                        }
                    }
                    sum
                })
            })
            .collect();
        workers.into_iter().map(|worker| worker.join().unwrap()).collect()
    });
    let expected: i64 = (0..i64::from(total)).sum();
    assert_eq!(sums.iter().sum::<i64>(), expected);
    let stats = table.column_statistics(0).unwrap();
    assert_eq!(stats.numeric_max(), Some(Value::integer(total - 1)));
    manager.commit(&t2).unwrap();
}

#[test]
fn alter_table_creates_new_versions() {
    let manager = TransactionManager::default();
    let table = table();
    let setup = manager.begin();
    insert(&table, &setup, &[(1, "a"), (2, "b")]);
    manager.commit(&setup).unwrap();

    let t1 = manager.begin();
    insert(&table, &t1, &[(3, "c")]);
    let added = table
        .add_column(&t1, &ColumnDefinition::new("d", LogicalType::BIGINT), &|count| {
            let mut vector = Vector::flat(LogicalType::BIGINT, count);
            vector.values_mut::<i64>()[..count].fill(42);
            Ok(vector)
        })
        .unwrap();
    assert!(!table.is_main_table());
    let error = table
        .append_chunk(&t1, &chunk(&[(9, "z")]), TableConstraints::default())
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "TransactionContext Error: Transaction conflict: attempting to insert into table \"t\" but it has been altered by a different transaction"
    );
    let retyped = added
        .alter_type(
            &t1,
            0,
            &LogicalType::BIGINT,
            &[StorageIndex::new(0)],
            &|chunk: &DataChunk| {
                let mut vector = Vector::flat(LogicalType::BIGINT, chunk.len());
                for row in 0..chunk.len() {
                    let value = chunk.column(0).get_value(row).as_i32().map_or_else(
                        || Value::null(LogicalType::BIGINT),
                        |i| Value::bigint(i64::from(i) * 100),
                    );
                    vector.set_value(row, &value).unwrap();
                }
                Ok(vector)
            },
        )
        .unwrap();
    let dropped = retyped.remove_column(&t1, 1).unwrap();
    assert_eq!(dropped.columns().len(), 2);
    let mut state = TableScanState::default();
    dropped
        .initialize_scan(
            &t1,
            &mut state,
            vec![StorageIndex::new(0), StorageIndex::new(1)],
            None,
        )
        .unwrap();
    let mut result = DataChunk::default();
    let mut rows = Vec::new();
    loop {
        dropped.scan(&t1, &mut result, &mut state).unwrap();
        if result.is_empty() {
            break;
        }
        for row in 0..result.len() {
            rows.push((result.column(0).get_value(row), result.column(1).get_value(row)));
        }
    }
    assert_eq!(
        rows,
        vec![
            (Value::bigint(100), Value::bigint(42)),
            (Value::bigint(200), Value::bigint(42)),
            (Value::bigint(300), Value::bigint(42)),
        ],
        "transaction-local rows are altered too"
    );
    manager.commit(&t1).unwrap();
    let t2 = manager.begin();
    let mut null = Vector::flat(LogicalType::BIGINT, 1);
    null.set_value(0, &Value::null(LogicalType::BIGINT)).unwrap();
    dropped
        .update(
            &t2,
            &[0],
            &[1],
            &DataChunk::new(vec![null], 1),
            &TableConstraints::default(),
        )
        .unwrap();
    manager.commit(&t2).unwrap();
    let t3 = manager.begin();
    let error = dropped.add_not_null_constraint(&t3, 1).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Constraint Error: NOT NULL constraint failed: t.d"
    );
    assert!(dropped.add_not_null_constraint(&t3, 0).is_ok());
    manager.commit(&t3).unwrap();
}
