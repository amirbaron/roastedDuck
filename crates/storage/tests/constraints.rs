//! PRIMARY KEY, UNIQUE and FOREIGN KEY enforcement through ART indexes, with DuckDB's
//! error messages, across transactions; CREATE INDEX; index scans.
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation)]

use std::sync::Arc;

use rustdb_buffer::{BufferManager, BufferManagerConfig, InMemoryBlockManager};
use rustdb_storage::index::IndexConstraintType;
use rustdb_storage::table::StorageIndex;
use rustdb_storage::table_filter::{TableFilter, TableFilterSet};
use rustdb_storage::{
    BoundForeignKey, ColumnDefinition, DataTable, ForeignKeyKind, TableConstraints, TableIoManager,
    TableScanState, Transaction, TransactionManager,
};
use rustdb_types::compare::CompareOp;
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

fn io() -> Arc<TableIoManager> {
    let buffer_manager = BufferManager::new(BufferManagerConfig::default()).unwrap();
    Arc::new(TableIoManager::new(
        buffer_manager,
        Arc::new(InMemoryBlockManager::default()),
    ))
}

fn table(io: &Arc<TableIoManager>, name: &str) -> Arc<DataTable> {
    DataTable::new(
        Arc::clone(io),
        "main",
        name,
        vec![
            ColumnDefinition::new("i", LogicalType::INTEGER),
            ColumnDefinition::new("j", LogicalType::VARCHAR),
        ],
    )
}

fn chunk(rows: &[(Option<i32>, &str)]) -> DataChunk {
    let mut ints = Vector::flat(LogicalType::INTEGER, rows.len());
    let mut strings = Vector::flat(LogicalType::VARCHAR, rows.len());
    for (row, (i, s)) in rows.iter().enumerate() {
        let value = i.map_or_else(|| Value::null(LogicalType::INTEGER), Value::integer);
        ints.set_value(row, &value).unwrap();
        strings.set_value(row, &Value::varchar(s)).unwrap();
    }
    DataChunk::new(vec![ints, strings], rows.len())
}

fn insert_with(
    table: &Arc<DataTable>,
    transaction: &Transaction,
    rows: &[(Option<i32>, &str)],
    constraints: &TableConstraints,
) -> Result<(), String> {
    table
        .append_chunk(transaction, &chunk(rows), constraints.clone())
        .map_err(|e| e.to_string())
}

fn insert(table: &Arc<DataTable>, transaction: &Transaction, rows: &[i32]) -> Result<(), String> {
    let rows: Vec<(Option<i32>, &str)> = rows.iter().map(|&i| (Some(i), "x")).collect();
    insert_with(table, transaction, &rows, &TableConstraints::default())
}

/// The visible rows as `(rowid, i)`, filtered by `filters`.
fn scan(
    table: &Arc<DataTable>,
    transaction: &Transaction,
    filters: Option<&TableFilterSet>,
) -> Vec<(i64, Option<i32>)> {
    let mut state = TableScanState::default();
    table
        .initialize_scan(
            transaction,
            &mut state,
            vec![StorageIndex::row_id(), StorageIndex::new(0)],
            filters,
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
            rows.push((
                result.column(0).get_value(row).as_i64().unwrap(),
                result.column(1).get_value(row).as_i32(),
            ));
        }
    }
}

fn values(table: &Arc<DataTable>, transaction: &Transaction) -> Vec<i32> {
    let mut values: Vec<i32> = scan(table, transaction, None)
        .into_iter()
        .filter_map(|(_, i)| i)
        .collect();
    values.sort_unstable();
    values
}

fn row_id_of(table: &Arc<DataTable>, transaction: &Transaction, value: i32) -> i64 {
    scan(table, transaction, None)
        .into_iter()
        .find(|(_, i)| *i == Some(value))
        .unwrap()
        .0
}

#[test]
fn primary_key_messages() {
    let io = io();
    let manager = TransactionManager::default();
    let t = table(&io, "t");
    t.add_constraint_index(IndexConstraintType::Primary, &[0], 0)
        .unwrap();
    assert_eq!(t.indexes().entries()[0].name(), "PRIMARY_t_0");
    let tx = manager.begin();
    assert_eq!(
        insert(&t, &tx, &[1, 1]).unwrap_err(),
        "Constraint Error: PRIMARY KEY or UNIQUE constraint violation: duplicate key \"1\""
    );
    manager.rollback(&tx);
    let tx = manager.begin();
    insert(&t, &tx, &[1, 2]).unwrap();
    manager.commit(&tx).unwrap();
    let tx = manager.begin();
    assert_eq!(
        insert(&t, &tx, &[3, 2]).unwrap_err(),
        "Constraint Error: Duplicate key \"i: 2\" violates primary key constraint."
    );
    manager.rollback(&tx);
}

#[test]
fn compound_unique_key_and_nulls() {
    let io = io();
    let manager = TransactionManager::default();
    let t = table(&io, "t");
    t.add_constraint_index(IndexConstraintType::Unique, &[0, 1], 0)
        .unwrap();
    let tx = manager.begin();
    let none = TableConstraints::default();
    insert_with(
        &t,
        &tx,
        &[(Some(1), "a"), (Some(1), "b"), (None, "a"), (None, "a")],
        &none,
    )
    .unwrap();
    manager.commit(&tx).unwrap();
    let tx = manager.begin();
    assert_eq!(
        insert_with(&t, &tx, &[(Some(1), "a")], &none).unwrap_err(),
        "Constraint Error: Duplicate key \"i: 1, j: a\" violates unique constraint."
    );
    manager.rollback(&tx);
}

#[test]
fn delete_then_insert_the_same_key() {
    let io = io();
    let manager = TransactionManager::default();
    let t = table(&io, "t");
    t.add_constraint_index(IndexConstraintType::Unique, &[0], 0)
        .unwrap();
    let tx = manager.begin();
    insert(&t, &tx, &[1, 2]).unwrap();
    manager.commit(&tx).unwrap();
    let reader = manager.begin();

    let tx = manager.begin();
    let old = row_id_of(&t, &tx, 1);
    assert_eq!(t.delete(&tx, &[old]).unwrap(), 1);
    insert(&t, &tx, &[1]).unwrap();
    // a second insert of the key in the same transaction is a duplicate again
    assert_eq!(
        insert(&t, &tx, &[1]).unwrap_err(),
        "Constraint Error: PRIMARY KEY or UNIQUE constraint violation: duplicate key \"1\""
    );
    manager.rollback(&tx);

    let tx = manager.begin();
    assert_eq!(t.delete(&tx, &[old]).unwrap(), 1);
    insert(&t, &tx, &[1]).unwrap();
    manager.commit(&tx).unwrap();
    assert_eq!(values(&t, &reader), [1, 2], "the reader still sees the old rows");

    // the deleted row is still found by index scans of the older reader
    let mut filters = TableFilterSet::default();
    filters.push_filter(
        0,
        TableFilter::ConstantComparison {
            op: CompareOp::Equal,
            constant: Value::integer(1),
        },
    );
    let row_ids = t
        .try_index_scan(&[StorageIndex::new(0)], &filters, usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(row_ids.len(), 2, "old and new row of key 1");
    let mut fetched = DataChunk::default();
    t.fetch(&reader, &mut fetched, &[StorageIndex::new(0)], &row_ids)
        .unwrap();
    assert_eq!(fetched.len(), 1, "the reader sees only the old row");
    assert_eq!(fetched.column(0).get_value(0), Value::integer(1));
    manager.commit(&reader).unwrap();

    // once the reader is gone, the deleted row leaves the side index
    let row_ids = t
        .try_index_scan(&[StorageIndex::new(0)], &filters, usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(row_ids.len(), 1);
    let tx = manager.begin();
    assert_eq!(
        insert(&t, &tx, &[1]).unwrap_err(),
        "Constraint Error: Duplicate key \"i: 1\" violates unique constraint."
    );
    manager.rollback(&tx);
}

#[test]
fn concurrent_inserts_conflict_at_commit() {
    let io = io();
    let manager = TransactionManager::default();
    let t = table(&io, "t");
    t.add_constraint_index(IndexConstraintType::Primary, &[0], 0)
        .unwrap();
    let t1 = manager.begin();
    let t2 = manager.begin();
    insert(&t, &t1, &[5]).unwrap();
    insert(&t, &t2, &[4, 5, 6]).unwrap();
    manager.commit(&t1).unwrap();
    let error = manager.commit(&t2).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Constraint Error: PRIMARY KEY or UNIQUE constraint violation: duplicate key \"5\""
    );
    // the failed commit left nothing behind in the index or the table
    let tx = manager.begin();
    assert_eq!(values(&t, &tx), [5]);
    insert(&t, &tx, &[4, 6]).unwrap();
    manager.commit(&tx).unwrap();
    let tx = manager.begin();
    assert_eq!(values(&t, &tx), [4, 5, 6]);
    manager.commit(&tx).unwrap();
}

#[test]
fn foreign_keys() {
    let io = io();
    let manager = TransactionManager::default();
    let p = table(&io, "p");
    let c = table(&io, "c");
    p.add_constraint_index(IndexConstraintType::Primary, &[0], 0)
        .unwrap();
    c.add_constraint_index(IndexConstraintType::Foreign, &[0], 0)
        .unwrap();
    let child = TableConstraints {
        foreign_keys: vec![BoundForeignKey {
            kind: ForeignKeyKind::Append,
            other: Arc::clone(&p),
            columns: vec![0],
            other_columns: vec![0],
        }],
        ..TableConstraints::default()
    };
    let parent = TableConstraints {
        foreign_keys: vec![BoundForeignKey {
            kind: ForeignKeyKind::Delete,
            other: Arc::clone(&c),
            columns: vec![0],
            other_columns: vec![0],
        }],
        ..TableConstraints::default()
    };
    let tx = manager.begin();
    insert(&p, &tx, &[1, 2]).unwrap();
    manager.commit(&tx).unwrap();

    let tx = manager.begin();
    insert_with(&c, &tx, &[(Some(1), "a"), (None, "b")], &child).unwrap();
    assert_eq!(
        insert_with(&c, &tx, &[(Some(3), "a")], &child).unwrap_err(),
        "Constraint Error: Violates foreign key constraint because key \"i: 3\" does not exist in the referenced table"
    );
    manager.commit(&tx).unwrap();

    // a key inserted into the parent in the same transaction can be referenced
    let tx = manager.begin();
    insert(&p, &tx, &[7]).unwrap();
    insert_with(&c, &tx, &[(Some(7), "a")], &child).unwrap();
    manager.commit(&tx).unwrap();

    // DuckDB reports the first locally found key when both lookups miss some keys
    let tx = manager.begin();
    insert(&p, &tx, &[8]).unwrap();
    assert_eq!(
        insert_with(&c, &tx, &[(Some(8), "a"), (Some(9), "b")], &child).unwrap_err(),
        "Constraint Error: Violates foreign key constraint because key \"i: 8\" does not exist in the referenced table"
    );
    manager.rollback(&tx);

    let tx = manager.begin();
    let referenced = row_id_of(&p, &tx, 1);
    assert_eq!(
        p.delete_with_constraints(&tx, &[referenced], &parent)
            .unwrap_err()
            .to_string(),
        "Constraint Error: Violates foreign key constraint because key \"i: 1\" is still referenced by a foreign key in a different table. If this is an unexpected constraint violation, please refer to our foreign key limitations in the documentation"
    );
    manager.rollback(&tx);
    let tx = manager.begin();
    let free = row_id_of(&p, &tx, 2);
    assert_eq!(p.delete_with_constraints(&tx, &[free], &parent).unwrap(), 1);
    manager.commit(&tx).unwrap();
}

#[test]
fn create_index_on_existing_data() {
    let io = io();
    let manager = TransactionManager::default();
    let t = table(&io, "t");
    let tx = manager.begin();
    insert(&t, &tx, &[1, 1]).unwrap();
    manager.commit(&tx).unwrap();
    let tx = manager.begin();
    let art = t.new_art("idx", IndexConstraintType::Unique, &[0]).unwrap();
    assert_eq!(
        t.create_index(&tx, art, true).unwrap_err().to_string(),
        "Constraint Error: Data contains duplicates on indexed column(s)"
    );
    manager.rollback(&tx);

    let t = table(&io, "t2");
    let tx = manager.begin();
    insert(&t, &tx, &[1, 2]).unwrap();
    manager.commit(&tx).unwrap();
    let tx = manager.begin();
    let art = t.new_art("idx", IndexConstraintType::Unique, &[0]).unwrap();
    t.create_index(&tx, art, true).unwrap();
    assert_eq!(
        insert(&t, &tx, &[2]).unwrap_err(),
        "Constraint Error: Duplicate key \"i: 2\" violates unique constraint."
    );
    manager.rollback(&tx);
    assert!(t.index_columns_updated(&[0]));
    assert!(!t.index_columns_updated(&[1]));
    let error = t.remove_column(&manager.begin(), 0).unwrap_err().to_string();
    assert_eq!(
        error,
        "Catalog Error: Cannot drop this column: an index depends on it!"
    );
}

#[test]
fn invalid_key_type() {
    let io = io();
    let t = DataTable::new(
        io,
        "main",
        "t",
        vec![ColumnDefinition::new(
            "l",
            LogicalType::list(LogicalType::INTEGER),
        )],
    );
    assert_eq!(
        t.add_constraint_index(IndexConstraintType::Primary, &[0], 0)
            .unwrap_err()
            .to_string(),
        "Invalid type Error: Invalid Type [INTEGER[]]: Invalid type for index key."
    );
}

#[test]
fn index_scans_match_table_scans() {
    let io = io();
    let manager = TransactionManager::default();
    let t = table(&io, "t");
    let art = t.new_art("idx", IndexConstraintType::None, &[0]).unwrap();
    t.add_index(art).unwrap();
    let tx = manager.begin();
    let rows: Vec<i32> = (0..5000).map(|i| (i * 7919) % 1000).collect();
    for batch in rows.chunks(2048) {
        insert(&t, &tx, batch).unwrap();
    }
    manager.commit(&tx).unwrap();
    let tx = manager.begin();
    // delete some rows so visibility matters
    let deleted: Vec<i64> = scan(&t, &tx, None)
        .into_iter()
        .filter(|(_, i)| i.is_some_and(|i| i % 10 == 3))
        .map(|(id, _)| id)
        .collect();
    t.delete(&tx, &deleted).unwrap();
    manager.commit(&tx).unwrap();
    let tx = manager.begin();
    let ops = [
        CompareOp::Equal,
        CompareOp::LessThan,
        CompareOp::LessThanOrEqual,
        CompareOp::GreaterThan,
        CompareOp::GreaterThanOrEqual,
    ];
    for (k, op) in ops.iter().enumerate() {
        for constant in [-5, 0, 3, 13, 500, 999, 2000] {
            let mut filters_on_1 = TableFilterSet::default();
            filters_on_1.push_filter(
                1,
                TableFilter::ConstantComparison {
                    op: *op,
                    constant: Value::integer(constant),
                },
            );
            let mut expected: Vec<i64> = scan(&t, &tx, Some(&filters_on_1))
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            expected.sort_unstable();
            let mut on_row_id = TableFilterSet::default();
            on_row_id.push_filter(
                0,
                TableFilter::ConstantComparison {
                    op: *op,
                    constant: Value::bigint(i64::from(constant)),
                },
            );
            let row_ids = t
                .try_index_scan(
                    &[StorageIndex::row_id(), StorageIndex::new(0)],
                    &on_row_id,
                    usize::MAX,
                )
                .unwrap();
            assert!(
                row_ids.is_none(),
                "the filter is on the row id, not the indexed column"
            );
            let row_ids = t
                .try_index_scan(
                    &[StorageIndex::row_id(), StorageIndex::new(0)],
                    &filters_on_1,
                    usize::MAX,
                )
                .unwrap()
                .unwrap();
            let mut fetched = DataChunk::default();
            t.fetch(&tx, &mut fetched, &[StorageIndex::row_id()], &row_ids)
                .unwrap();
            let mut found: Vec<i64> = (0..fetched.len())
                .map(|r| fetched.column(0).get_value(r).as_i64().unwrap())
                .collect();
            found.sort_unstable();
            assert_eq!(found, expected, "op {k} constant {constant}");
        }
    }
    // IN lists use one lookup per value
    let mut filters = TableFilterSet::default();
    filters.push_filter(
        0,
        TableFilter::In(vec![Value::integer(1), Value::integer(2), Value::integer(1)]),
    );
    let row_ids = t
        .try_index_scan(&[StorageIndex::new(0)], &filters, usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(row_ids.len(), 10);
    // too many matches fall back to a table scan
    let mut filters = TableFilterSet::default();
    filters.push_filter(
        0,
        TableFilter::ConstantComparison {
            op: CompareOp::GreaterThan,
            constant: Value::integer(10),
        },
    );
    assert!(
        t.try_index_scan(&[StorageIndex::new(0)], &filters, 100)
            .unwrap()
            .is_none()
    );
    manager.commit(&tx).unwrap();
}

#[test]
fn many_rows_with_primary_key() {
    let io = io();
    let manager = TransactionManager::default();
    let t = table(&io, "t");
    t.add_constraint_index(IndexConstraintType::Primary, &[0], 0)
        .unwrap();
    let tx = manager.begin();
    let rows: Vec<i32> = (0..200_000).collect();
    for batch in rows.chunks(2048) {
        insert(&t, &tx, batch).unwrap();
    }
    manager.commit(&tx).unwrap();
    let tx = manager.begin();
    assert_eq!(
        insert(&t, &tx, &[200_000, 123_456]).unwrap_err(),
        "Constraint Error: Duplicate key \"i: 123456\" violates primary key constraint."
    );
    manager.rollback(&tx);
    // delete half and insert them again in another transaction
    let tx = manager.begin();
    let ids: Vec<i64> = scan(&t, &tx, None)
        .into_iter()
        .filter(|(_, i)| i.is_some_and(|i| i % 2 == 0))
        .map(|(id, _)| id)
        .collect();
    for batch in ids.chunks(2048) {
        t.delete(&tx, batch).unwrap();
    }
    manager.commit(&tx).unwrap();
    let tx = manager.begin();
    let evens: Vec<i32> = (0..200_000).filter(|i| i % 2 == 0).collect();
    for batch in evens.chunks(2048) {
        insert(&t, &tx, batch).unwrap();
    }
    manager.commit(&tx).unwrap();
    let tx = manager.begin();
    assert_eq!(values(&t, &tx).len(), 200_000);
    manager.commit(&tx).unwrap();
}
