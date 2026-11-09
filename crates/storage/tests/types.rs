//! Round trips of every column type (with NULLs) through table storage, and storage
//! under a memory limit that forces blocks to spill.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use roastedduck_buffer::{BufferManager, BufferManagerConfig, InMemoryBlockManager};
use roastedduck_storage::table::StorageIndex;
use roastedduck_storage::{
    ColumnDefinition, DataTable, TableConstraints, TableIoManager, TableScanState, Transaction,
    TransactionManager,
};
use roastedduck_types::{DataChunk, Interval, LogicalType, Value, Vector};

fn io_with(config: BufferManagerConfig) -> Arc<TableIoManager> {
    let buffer_manager = BufferManager::new(config).unwrap();
    Arc::new(TableIoManager::new(
        buffer_manager,
        Arc::new(InMemoryBlockManager::default()),
    ))
}

fn scan_all(table: &Arc<DataTable>, transaction: &Transaction) -> Vec<Vec<Value>> {
    let column_ids = (0..table.columns().len()).map(StorageIndex::from).collect();
    let mut state = TableScanState::default();
    table
        .initialize_scan(transaction, &mut state, column_ids, None)
        .unwrap();
    let mut rows = Vec::new();
    let mut chunk = DataChunk::default();
    loop {
        table.scan(transaction, &mut chunk, &mut state).unwrap();
        if chunk.is_empty() {
            return rows;
        }
        for row in 0..chunk.len() {
            rows.push(
                chunk
                    .columns()
                    .iter()
                    .map(|column| column.get_value(row))
                    .collect(),
            );
        }
    }
}

/// Inserts `rows` (row-major) in chunks and commits.
fn insert_rows(manager: &TransactionManager, table: &Arc<DataTable>, rows: &[Vec<Value>]) {
    let transaction = manager.begin();
    let types = table.types();
    let mut state = table
        .initialize_local_append(&transaction, TableConstraints::default())
        .unwrap();
    for chunk_rows in rows.chunks(2048) {
        let columns = types
            .iter()
            .enumerate()
            .map(|(column, ty)| {
                let mut vector = Vector::flat(ty.clone(), chunk_rows.len());
                for (row, values) in chunk_rows.iter().enumerate() {
                    vector.set_value(row, &values[column]).unwrap();
                }
                vector
            })
            .collect();
        table
            .local_append(&mut state, &DataChunk::new(columns, chunk_rows.len()))
            .unwrap();
    }
    table.finalize_local_append(state);
    manager.commit(&transaction).unwrap();
}

/// One sample value per type, varied by `i`; NULL every 5th row.
fn sample(ty: &LogicalType, i: i64) -> Value {
    use roastedduck_types::LogicalTypeId as L;
    if i % 5 == 3 {
        return Value::null(ty.clone());
    }
    match ty.id() {
        L::Boolean => Value::boolean(i % 2 == 0),
        L::TinyInt => Value::tinyint((i % 100) as i8),
        L::SmallInt => Value::smallint(i as i16),
        L::Integer => Value::integer(i as i32 - 1000),
        L::BigInt => Value::bigint(i * 1_000_000_007),
        L::HugeInt => Value::hugeint(i128::from(i) << 70),
        L::UTinyInt => Value::utinyint((i % 200) as u8),
        L::USmallInt => Value::usmallint(i as u16),
        L::UInteger => Value::uinteger(i as u32 * 3),
        L::UBigInt => Value::ubigint(i as u64 * 7),
        L::UHugeInt => Value::uhugeint(u128::from(i as u64) << 90),
        L::Float => Value::float(if i % 7 == 0 { f32::NAN } else { i as f32 / 3.0 }),
        L::Double => Value::double(i as f64 * -1.5),
        L::Decimal => Value::bigint(i * 12_345).reinterpret(ty.clone()),
        L::Date => Value::integer(i as i32 - 500).reinterpret(ty.clone()),
        L::Time | L::Timestamp | L::TimestampTz | L::TimestampSec | L::TimestampMs | L::TimestampNs => {
            Value::bigint(i * 86_400_000).reinterpret(ty.clone())
        }
        L::Interval => Value::interval(Interval {
            months: i as i32,
            days: -(i as i32),
            micros: i * 1000,
        }),
        L::Uuid => Value::hugeint(i128::from(i) * 0x1234_5678_9abc_def0).reinterpret(ty.clone()),
        L::Varchar => Value::varchar(if i % 13 == 0 {
            "a string that is long enough not to be inlined, number ".repeat(3) + &i.to_string()
        } else {
            format!("s{i}")
        }),
        L::Blob => Value::blob(&i.to_le_bytes()),
        L::Enum => Value::enum_value(ty.clone(), (i % 3) as u32),
        L::List => Value::list(
            LogicalType::BIGINT,
            (0..i % 4)
                .map(|j| {
                    if j == 1 {
                        Value::null(LogicalType::BIGINT)
                    } else {
                        Value::bigint(i + j)
                    }
                })
                .collect(),
        ),
        L::Struct => Value::struct_value(vec![
            ("x".into(), Value::integer(i as i32)),
            ("y".into(), Value::varchar(format!("y{i}"))),
        ]),
        L::Map => Value::map(
            LogicalType::VARCHAR,
            LogicalType::INTEGER,
            (0..i % 3)
                .map(|j| (Value::varchar(format!("k{j}")), Value::integer(j as i32)))
                .collect(),
        ),
        L::Array => Value::array(
            LogicalType::DOUBLE,
            vec![Value::double(i as f64), Value::double(-(i as f64))],
        ),
        other => panic!("no sample for {other:?}"),
    }
}

#[test]
fn every_type_round_trips_with_nulls() {
    let types = vec![
        LogicalType::BOOLEAN,
        LogicalType::TINYINT,
        LogicalType::SMALLINT,
        LogicalType::INTEGER,
        LogicalType::BIGINT,
        LogicalType::HUGEINT,
        LogicalType::UTINYINT,
        LogicalType::USMALLINT,
        LogicalType::UINTEGER,
        LogicalType::UBIGINT,
        LogicalType::UHUGEINT,
        LogicalType::FLOAT,
        LogicalType::DOUBLE,
        LogicalType::decimal(18, 3),
        LogicalType::DATE,
        LogicalType::TIME,
        LogicalType::TIMESTAMP,
        LogicalType::TIMESTAMP_TZ,
        LogicalType::TIMESTAMP_S,
        LogicalType::TIMESTAMP_MS,
        LogicalType::TIMESTAMP_NS,
        LogicalType::INTERVAL,
        LogicalType::UUID,
        LogicalType::VARCHAR,
        LogicalType::BLOB,
        LogicalType::enum_type(vec!["a".into(), "b".into(), "c".into()]),
        LogicalType::list(LogicalType::BIGINT),
        LogicalType::struct_type(vec![
            ("x".into(), LogicalType::INTEGER),
            ("y".into(), LogicalType::VARCHAR),
        ]),
        LogicalType::map(LogicalType::VARCHAR, LogicalType::INTEGER),
        LogicalType::array(LogicalType::DOUBLE, 2),
    ];
    let columns = types
        .iter()
        .enumerate()
        .map(|(index, ty)| ColumnDefinition::new(format!("c{index}"), ty.clone()))
        .collect();
    let table = DataTable::new(
        io_with(BufferManagerConfig::default()),
        "main",
        "all_types",
        columns,
    );
    let manager = TransactionManager::default();
    let rows: Vec<Vec<Value>> = (0..5000)
        .map(|i| types.iter().map(|ty| sample(ty, i)).collect())
        .collect();
    insert_rows(&manager, &table, &rows);
    let reader = manager.begin();
    let scanned = scan_all(&table, &reader);
    assert_eq!(scanned.len(), rows.len());
    for (row, (expected, actual)) in rows.iter().zip(&scanned).enumerate() {
        for (column, (expected, actual)) in expected.iter().zip(actual).enumerate() {
            let same = expected == actual
                || expected.compare_not_distinct(actual) == Some(std::cmp::Ordering::Equal);
            assert!(
                same,
                "row {row} column {column} ({}): {expected:?} != {actual:?}",
                types[column]
            );
        }
    }
    manager.commit(&reader).unwrap();
}

fn temp_dir(name: &str) -> PathBuf {
    let unique = format!(
        "roastedduck-storage-{name}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
    );
    std::env::temp_dir().join(unique.replace([' ', ':', '{', '}', ','], "_"))
}

#[test]
fn tables_spill_under_a_small_memory_limit() {
    let temp = temp_dir("spill");
    let io = io_with(BufferManagerConfig {
        memory_limit: Some(8 << 20),
        temp_directory: Some(temp.clone()),
        ..BufferManagerConfig::default()
    });
    let table = DataTable::new(
        Arc::clone(&io),
        "main",
        "big",
        vec![
            ColumnDefinition::new("i", LogicalType::BIGINT),
            ColumnDefinition::new("s", LogicalType::VARCHAR),
        ],
    );
    let manager = TransactionManager::default();
    // ~2M rows of 8-byte integers and 20-byte strings: well above the 8 MiB limit
    let rows: Vec<Vec<Value>> = (0..400_000i64)
        .map(|i| vec![Value::bigint(i), Value::varchar(format!("string value {i:08}"))])
        .collect();
    insert_rows(&manager, &table, &rows);
    assert!(io.buffer_manager().used_memory() <= 8 << 20);
    assert!(io.buffer_manager().used_swap() > 0, "blocks were spilled");
    let reader = manager.begin();
    let scanned = scan_all(&table, &reader);
    assert_eq!(scanned.len(), rows.len());
    assert_eq!(scanned[123_456], rows[123_456]);
    assert_eq!(scanned[399_999], rows[399_999]);
    manager.commit(&reader).unwrap();
    drop(table);
    drop(io);
    let _ = std::fs::remove_dir_all(temp);
}
