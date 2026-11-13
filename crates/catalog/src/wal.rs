//! Logging catalog changes to the write-ahead log (DuckDB's
//! `WALWriteState::WriteCatalogEntry` and the `WriteAheadLog::Write*` methods of catalog
//! entries).

use std::sync::Arc;

use roastedduck_ast::{AlterInfo, AlterInfoKind, AlterTableInfo, Constraint, CreateInfoKind, ParseInfo};
use roastedduck_common::{ByteString, CaseInsensitiveMap, Error, Result};
use roastedduck_serde::Serializer;
use roastedduck_storage::checkpoint::pointers::serialize_index_storage_info;
use roastedduck_storage::index::art::ArtStorageInfo;
use roastedduck_storage::wal::{WalBatch, WalType};
use roastedduck_types::Value;

use crate::checkpoint::{
    base_info, macro_create_info, sequence_create_info, table_create_info, type_create_info,
    view_create_info, write_create_info,
};
use crate::{CatalogEntry, EntryKind};

/// How a catalog change is logged.
#[derive(Debug)]
pub enum WalRecord {
    /// The `CREATE` of the new entry.
    Create,
    /// The `DROP` of this entry.
    Drop(Arc<CatalogEntry>),
    /// The `ALTER` that produced the new entry.
    Alter(Box<AlterInfo>),
    /// Nothing: the tombstone of a rename, or an internal change replay redoes itself
    /// (the referenced side of a foreign key).
    None,
}

/// The options of index storage in the WAL (`SerializeIndex`): files of storage version
/// 3 and up mark their indexes as not using the v1.0.0 layout.
fn index_options(batch: &WalBatch) -> CaseInsensitiveMap<Value> {
    let mut options = CaseInsensitiveMap::default();
    if batch.options().compatibility.serialization_version >= 3 {
        options.insert("v1_0_0_storage".to_owned(), Value::boolean(false));
    }
    options
}

/// The index `name` of `table`, serialized for the WAL (`SerializeIndex`): its
/// description and the bytes of its buffers per allocator.
fn serialize_index(
    batch: &WalBatch,
    table: Option<&Arc<roastedduck_storage::DataTableInfo>>,
    name: impl AsRef<[u8]>,
) -> Result<(ArtStorageInfo, Vec<Vec<Vec<u8>>>)> {
    let options = index_options(batch);
    let Some(entry) = table.and_then(|table| table.indexes().find(name.as_ref())) else {
        // The index is already gone from storage: log it empty.
        let info = ArtStorageInfo {
            name: name.as_ref().into(),
            options,
            ..ArtStorageInfo::default()
        };
        return Ok((info, Vec::new()));
    };
    let mut state = entry.lock();
    state
        .index
        .as_art_mut()
        .ok_or_else(|| Error::not_implemented("Persistence of secondary indexes is not supported"))?
        .serialize_to_wal(&options)
}

/// Writes the storage of an index (`index_storage_info` and `index_storage`).
fn write_index_storage(
    serializer: &mut dyn Serializer,
    index: &(ArtStorageInfo, Vec<Vec<Vec<u8>>>),
) -> Result<()> {
    let (info, buffers) = index;
    serializer.write_object_property(102, "index_storage_info", |s| {
        serialize_index_storage_info(s, info)
    })?;
    serializer.write_list_property(103, "index_storage", buffers.len(), |s, allocator| {
        for buffer in &buffers[allocator] {
            s.write_data(buffer);
        }
        Ok(())
    })
}

/// Logs the creation of `entry` in schema `schema` of catalog `catalog`.
pub(crate) fn write_create(
    batch: &mut WalBatch,
    catalog: impl AsRef<[u8]>,
    schema: impl AsRef<[u8]>,
    entry: &CatalogEntry,
) -> Result<()> {
    let catalog = catalog.as_ref();
    let schema = schema.as_ref();
    if entry.temporary || entry.internal {
        return Ok(());
    }
    match &entry.kind {
        EntryKind::Schema(_) => batch.entry(WalType::CreateSchema, |s| {
            s.write_property(101, "schema", entry.name())
        }),
        EntryKind::Table(table) => {
            let info = table_create_info(catalog, schema, entry, table);
            batch.entry(WalType::CreateTable, |s| {
                write_create_info(s, 101, "table", &info)
            })
        }
        EntryKind::View(view) => {
            let info = view_create_info(catalog, schema, entry, view);
            batch.entry(WalType::CreateView, |s| write_create_info(s, 101, "view", &info))
        }
        EntryKind::Sequence(sequence) => {
            let info = sequence_create_info(catalog, schema, entry, sequence);
            batch.entry(WalType::CreateSequence, |s| {
                write_create_info(s, 101, "sequence", &info)
            })
        }
        EntryKind::Type(ty) => {
            let info = type_create_info(catalog, schema, entry, ty);
            batch.entry(WalType::CreateType, |s| write_create_info(s, 101, "type", &info))
        }
        EntryKind::Macro(_) | EntryKind::TableMacro(_) => {
            let info = macro_create_info(catalog, schema, entry)
                .ok_or_else(|| roastedduck_common::Error::internal("expected a macro"))?;
            let (wal_type, tag) = if matches!(entry.kind, EntryKind::Macro(_)) {
                (WalType::CreateMacro, "macro")
            } else {
                (WalType::CreateTableMacro, "table")
            };
            batch.entry(wal_type, |s| write_create_info(s, 101, tag, &info))
        }
        EntryKind::Index(index) => {
            let info = base_info(
                catalog,
                &index.schema,
                entry.temporary,
                CreateInfoKind::Index(Box::new(index.info.clone())),
            );
            let storage = serialize_index(batch, index.storage.as_ref(), entry.name())?;
            batch.entry(WalType::CreateIndex, |s| {
                write_create_info(s, 101, "index_catalog_entry", &info)?;
                write_index_storage(s, &storage)
            })
        }
        _ => Ok(()),
    }
}

/// Logs the drop of `entry` from schema `schema`.
pub(crate) fn write_drop(batch: &mut WalBatch, schema: impl AsRef<[u8]>, entry: &CatalogEntry) -> Result<()> {
    let schema = schema.as_ref();
    if entry.temporary || entry.internal {
        return Ok(());
    }
    let wal_type = match &entry.kind {
        EntryKind::Schema(_) => {
            return batch.entry(WalType::DropSchema, |s| {
                s.write_property(101, "schema", entry.name())
            });
        }
        EntryKind::Table(_) => WalType::DropTable,
        EntryKind::View(_) => WalType::DropView,
        EntryKind::Sequence(_) => WalType::DropSequence,
        EntryKind::Index(_) => WalType::DropIndex,
        EntryKind::Type(_) => WalType::DropType,
        EntryKind::Macro(_) => WalType::DropMacro,
        EntryKind::TableMacro(_) => WalType::DropTableMacro,
        _ => return Ok(()),
    };
    batch.entry(wal_type, |s| {
        s.write_property(101, "schema", schema)?;
        s.write_property(102, "name", entry.name())
    })
}

/// The name of the index an `ADD PRIMARY KEY` creates (`UniqueConstraint::GetName`).
fn primary_key_index_name(info: &AlterInfo) -> Option<ByteString> {
    let AlterInfoKind::AlterTable(AlterTableInfo::AddConstraint(add)) = &info.kind else {
        return None;
    };
    let Constraint::Unique(unique) = add.constraint.as_ref() else {
        return None;
    };
    if !unique.is_primary_key {
        return None;
    }
    let mut name = roastedduck_common::byte_format!("PRIMARY_{}", info.name);
    for column in &unique.columns {
        name.push(b'_');
        name.push_bytes(column);
    }
    Some(name)
}

/// Logs an `ALTER` (`WriteAlter`); `ADD PRIMARY KEY` also logs the new index of the
/// altered entry `altered`.
pub(crate) fn write_alter(
    batch: &mut WalBatch,
    info: &AlterInfo,
    altered: Option<&CatalogEntry>,
) -> Result<()> {
    let parse_info = ParseInfo::Alter(Box::new(info.clone()));
    let index = match primary_key_index_name(info) {
        Some(name) => {
            let table = altered.and_then(|entry| match &entry.kind {
                EntryKind::Table(table) => table.storage.as_ref().map(|storage| Arc::clone(storage.info())),
                _ => None,
            });
            Some(serialize_index(batch, table.as_ref(), &name)?)
        }
        None => None,
    };
    batch.entry(WalType::AlterInfo, |s| {
        s.on_property_begin(101, "info");
        s.on_nullable_begin(true);
        s.write_object(&parse_info)?;
        s.on_nullable_end();
        s.on_property_end();
        match &index {
            Some(index) => write_index_storage(s, index),
            None => Ok(()),
        }
    })
}
