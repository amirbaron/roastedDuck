//! Read preserved C++ databases with raw names through the native catalog/storage APIs.
use rustdb_buffer::{BufferManager, BufferManagerConfig};
use rustdb_catalog::{Catalog, CatalogType, EntryKind, Transaction, TransactionManager};
use rustdb_common::{ByteString, Error, Result};
use rustdb_storage::storage_manager::{StorageManager, StorageOptions};
use rustdb_storage::table::StorageIndex;
use rustdb_storage::{TableConstraints, TableScanState};
use rustdb_types::{DataChunk, Value, Vector};
use std::sync::Arc;
fn hex(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &byte in bytes {
        let digits = b"0123456789abcdef";
        out.push(char::from(digits[usize::from(byte >> 4)]));
        out.push(char::from(digits[usize::from(byte & 15)]));
    }
    out
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 && args.len() != 5 {
        return Err(Error::invalid_input(
            "expected PATH table|column|schema|type|enum raw|utf8 [checkpoint|read2]",
        ));
    }
    let mode = args.get(4).map_or("read", String::as_str);
    if !matches!(mode, "read" | "checkpoint" | "read2") {
        return Err(Error::invalid_input("unknown fixture mode"));
    }
    let name = if args[3] == "raw" {
        ByteString::from(b"caf\xe9")
    } else {
        ByteString::from("café")
    };
    let schema = if args[2] == "schema" {
        name.clone()
    } else {
        "main".into()
    };
    let table = if args[2] == "table" {
        name.clone()
    } else {
        "t".into()
    };
    let buffer = BufferManager::new(BufferManagerConfig::default())?;
    let storage = Arc::new(StorageManager::open(
        &args[1],
        buffer,
        StorageOptions {
            read_only: mode != "checkpoint",
            ..StorageOptions::default()
        },
    )?);
    let catalog = Catalog::new("raw_name_fixture")?;
    catalog.set_io(Arc::clone(storage.io()))?;
    catalog.set_storage(&storage);
    rustdb_catalog::checkpoint::load_checkpoint(&catalog, &storage)?;
    let transaction = Transaction::system();
    let entry = catalog
        .get_entry(&transaction, CatalogType::Table, &schema, &table)
        .ok_or_else(|| Error::internal("missing fixture table"))?;
    assert_eq!(entry.name(), &table);
    let EntryKind::Table(definition) = &entry.kind else {
        return Err(Error::internal("fixture entry is not a table"));
    };
    assert_eq!(definition.columns.len(), 1);
    if args[2] == "column" {
        assert_eq!(definition.columns[0].name, name);
    }
    if args[2] == "type" {
        let ty = catalog
            .get_entry(&transaction, CatalogType::Type, "main", &name)
            .ok_or_else(|| Error::internal("missing fixture enum"))?;
        assert_eq!(ty.name(), &name);
        // C++ query metadata reports no alias for these stored enum columns.
        assert!(definition.columns[0].logical_type.alias().is_none());
        let EntryKind::Type(ty) = &ty.kind else {
            return Err(Error::internal("fixture enum entry is not a type"));
        };
        assert_eq!(
            ty.user_type.enum_values(),
            definition.columns[0].logical_type.enum_values()
        );
    }
    if args[2] == "enum" {
        let expected = [name.clone(), ByteString::from("tail")];
        assert_eq!(
            definition.columns[0].logical_type.enum_values(),
            Some(expected.as_slice())
        );
    }
    let data = definition
        .storage
        .as_ref()
        .ok_or_else(|| Error::internal("missing table storage"))?;
    assert_eq!(data.info().schema_name(), schema);
    assert_eq!(data.name(), table);
    let mut scan = TableScanState::default();
    data.initialize_scan(&transaction, &mut scan, vec![StorageIndex::from(0usize)], None)?;
    let mut values = Vec::new();
    let mut chunk = DataChunk::default();
    loop {
        data.scan(&transaction, &mut chunk, &mut scan)?;
        if chunk.is_empty() {
            break;
        }
        for row in 0..chunk.len() {
            let value = chunk.column(0).get_value(row);
            values.push(if args[2] == "enum" {
                // Read the actual stored code; VARCHAR conversion of the raw label is
                // separately expected to fail exactly as in C++.
                rustdb_common::byte_format!(
                    "{}",
                    value
                        .as_i128()
                        .ok_or_else(|| Error::internal("ENUM code missing"))?
                )
            } else {
                value.to_varchar_bytes()?
            });
        }
    }
    assert_eq!(values.len(), if mode == "read2" { 2 } else { 1 });
    assert_eq!(
        values[0],
        if args[2] == "type" {
            "a"
        } else if args[2] == "enum" {
            "0"
        } else {
            "42"
        }
    );
    if mode == "read2" {
        assert_eq!(
            values[1],
            if args[2] == "type" {
                "b"
            } else if args[2] == "enum" {
                "1"
            } else {
                "43"
            }
        );
    }
    if mode == "checkpoint" {
        let manager = TransactionManager::default();
        let append = manager.begin();
        let next = if matches!(args[2].as_str(), "type" | "enum") {
            Value::enum_value(definition.columns[0].logical_type.clone(), 1)
        } else {
            Value::integer(43)
        };
        let mut local = data.initialize_local_append(&append, TableConstraints::default())?;
        data.local_append(&mut local, &DataChunk::new(vec![Vector::constant(&next)?], 1))?;
        data.finalize_local_append(local);
        manager.commit(&append)?;
        let mut on_start = |_: rustdb_storage::metadata::MetaBlockPointer, lock: &mut dyn FnMut()| {
            lock();
            Ok(manager.last_commit())
        };
        rustdb_catalog::checkpoint::write_checkpoint(
            &catalog,
            &storage,
            &Transaction::system(),
            &mut on_start,
            false,
            false,
            u64::MAX,
        )?;
    }
    println!(
        "{} {} schema={} table={} column={} value={}",
        args[2],
        args[3],
        hex(schema.as_bytes()),
        hex(table.as_bytes()),
        hex(definition.columns[0].name.as_bytes()),
        hex(values[0].as_bytes())
    );
    Ok(())
}
fn main() -> Result<()> {
    run()
}
