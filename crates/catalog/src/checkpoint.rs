//! Checkpointing the catalog to a database file and loading it back, and checkpointing
//! in-memory databases (DuckDB's `SingleFileCheckpointWriter`, `CheckpointReader` and
//! `InMemoryCheckpointer`).
//!
//! The catalog is written as one list of entries: per schema its sequences, tables
//! (referenced tables before referencing ones), views and indexes. Each entry is its
//! `CreateInfo`; tables are followed by the pointer to their data.

use std::sync::Arc;

use rustdb_ast::{
    CatalogType as AstCatalogType, ColumnDefinition, ColumnList, Constraint, CreateInfo, CreateInfoKind,
    CreateMacroInfo, CreateSequenceInfo, CreateTableInfo, CreateTypeInfo, CreateViewInfo, ForeignKeyType,
    IndexConstraintType as AstIndexConstraintType, LogicalDependencyList, OnCreateConflict,
    ParsedExpressionKind, TableColumnType,
};
use rustdb_common::{CaseInsensitiveMap, Error, OrderedMap, Result};
use rustdb_serde::binary::BinarySerializer;
use rustdb_serde::{Deserializer, Serializer};
use rustdb_storage::checkpoint::pointers::deserialize_index_storage_info;
use rustdb_storage::checkpoint::pointers::{deserialize_meta_block_pointer, serialize_meta_block_pointer};
use rustdb_storage::checkpoint::stream::StreamDeserializer;
use rustdb_storage::checkpoint::table_reader::{load_row_groups, read_table_data};
use rustdb_storage::checkpoint::table_writer::{
    BlockWriter, checkpoint_in_memory_table, finish_table_checkpoint, write_table_data,
};
use rustdb_storage::checkpoint_lock::ExclusiveCheckpointGuard;
use rustdb_storage::index::art::ArtStorageInfo;
use rustdb_storage::index::{IndexConstraintType, IndexSlot, UnboundIndex, UnboundIndexStorage};
use rustdb_storage::metadata::{MetaBlockPointer, MetadataReader, MetadataWriter};
use rustdb_storage::storage_info::{DatabaseHeader, STANDARD_VECTOR_SIZE};
use rustdb_storage::storage_manager::{CheckpointAbort, InMemoryStorageManager, StorageManager};
use rustdb_storage::{ColumnDefinition as StorageColumn, DataTable};
use rustdb_types::{LogicalType, Value};

use crate::{
    Catalog, CatalogEntry, CatalogType, EntryKind, IndexEntry, MacroEntry, SequenceEntry, TableEntry,
    Transaction, TypeEntry, ViewEntry,
};

/// Whether a checkpoint needs a connection to apply deferred expression-index keys.
/// Automatic checkpoints without a binder must retain the recoverable WAL instead.
pub fn has_pending_index_replays(catalog: &Catalog) -> bool {
    let transaction = Transaction::system();
    catalog.schemas().scan(&transaction).iter().any(|schema| {
        schema.as_schema().is_some_and(|schema| {
            schema.tables.scan(&transaction).iter().any(|entry| {
                if let EntryKind::Table(table) = &entry.kind {
                    table.storage.as_ref().is_some_and(|storage| {
                        storage.info().indexes().slots().iter().any(
                            |slot| matches!(slot, IndexSlot::Unbound(index) if index.has_buffered_replays()),
                        )
                    })
                } else {
                    false
                }
            })
        })
    })
}

pub(crate) fn base_info(
    catalog: impl AsRef<[u8]>,
    schema: impl AsRef<[u8]>,
    temporary: bool,
    kind: CreateInfoKind,
) -> CreateInfo {
    let catalog = catalog.as_ref();
    let schema = schema.as_ref();
    CreateInfo {
        catalog: catalog.into(),
        schema: schema.into(),
        temporary,
        internal: false,
        on_conflict: OnCreateConflict::ErrorOnConflict,
        sql: rustdb_common::ByteString::new(),
        comment: Value::default(),
        tags: CaseInsensitiveMap::default(),
        dependencies: LogicalDependencyList::default(),
        kind,
    }
}

/// The `CreateInfo` of schema `schema` (`SchemaCatalogEntry::GetInfo`).
pub fn schema_create_info(catalog: impl AsRef<[u8]>, schema: impl AsRef<[u8]>) -> CreateInfo {
    let catalog = catalog.as_ref();
    let schema = schema.as_ref();
    base_info(catalog, schema, false, CreateInfoKind::Schema)
}

/// [`base_info`] carrying the dependencies `entry` records.
fn entry_info(
    catalog: impl AsRef<[u8]>,
    schema: impl AsRef<[u8]>,
    entry: &CatalogEntry,
    temporary: bool,
    kind: CreateInfoKind,
) -> CreateInfo {
    let catalog = catalog.as_ref();
    let schema = schema.as_ref();
    let mut info = base_info(catalog, schema, temporary, kind);
    info.dependencies.set.clone_from(&entry.dependencies);
    info.comment.clone_from(&entry.comment);
    info
}

/// `TableCatalogEntry::GetInfo`.
pub fn table_create_info(
    catalog: impl AsRef<[u8]>,
    schema: impl AsRef<[u8]>,
    entry: &CatalogEntry,
    table: &TableEntry,
) -> CreateInfo {
    let catalog = catalog.as_ref();
    let schema = schema.as_ref();
    let columns = table
        .columns
        .iter()
        .map(|column| {
            let (expression, category) = match (&column.generated, &column.default_value) {
                (Some(generated), _) => (Some(Box::new(generated.clone())), TableColumnType::Generated),
                (None, default) => (default.clone().map(Box::new), TableColumnType::Standard),
            };
            ColumnDefinition {
                name: column.name.clone(),
                logical_type: column.logical_type.clone(),
                expression,
                category,
                compression_type: column.compression_type,
                comment: column.comment.clone(),
                tags: OrderedMap::default(),
            }
        })
        .collect();
    entry_info(
        catalog,
        schema,
        entry,
        entry.temporary,
        CreateInfoKind::Table(Box::new(CreateTableInfo {
            table: entry.name().to_owned(),
            columns: ColumnList { columns },
            constraints: table.constraints.clone(),
            query: None,
            partition_keys: Vec::new(),
            sort_keys: Vec::new(),
            options: CaseInsensitiveMap::default(),
        })),
    )
}

/// `ViewCatalogEntry::GetInfo`.
pub(crate) fn view_create_info(
    catalog: impl AsRef<[u8]>,
    schema: impl AsRef<[u8]>,
    entry: &CatalogEntry,
    view: &ViewEntry,
) -> CreateInfo {
    let catalog = catalog.as_ref();
    let schema = schema.as_ref();
    entry_info(
        catalog,
        schema,
        entry,
        entry.temporary,
        CreateInfoKind::View(Box::new(CreateViewInfo {
            view_name: entry.name().to_owned(),
            aliases: view.aliases.clone(),
            types: view.types.clone(),
            query: Box::new(view.query.clone()),
            names: view.names.clone(),
            column_comments: view.column_comments.clone(),
        })),
    )
}

/// `SequenceCatalogEntry::GetInfo`: the counter is stored as the start value.
pub fn sequence_create_info(
    catalog: impl AsRef<[u8]>,
    schema: impl AsRef<[u8]>,
    entry: &CatalogEntry,
    sequence: &SequenceEntry,
) -> CreateInfo {
    let catalog = catalog.as_ref();
    let schema = schema.as_ref();
    let data = sequence.data();
    entry_info(
        catalog,
        schema,
        entry,
        entry.temporary,
        CreateInfoKind::Sequence(Box::new(CreateSequenceInfo {
            name: entry.name().to_owned(),
            usage_count: data.usage_count,
            increment: data.increment,
            min_value: data.min_value,
            max_value: data.max_value,
            start_value: data.counter,
            cycle: data.cycle,
        })),
    )
}

/// `TypeCatalogEntry::GetInfo`.
pub(crate) fn type_create_info(
    catalog: impl AsRef<[u8]>,
    schema: impl AsRef<[u8]>,
    entry: &CatalogEntry,
    ty: &TypeEntry,
) -> CreateInfo {
    let catalog = catalog.as_ref();
    let schema = schema.as_ref();
    entry_info(
        catalog,
        schema,
        entry,
        entry.temporary,
        CreateInfoKind::Type(CreateTypeInfo {
            name: entry.name().to_owned(),
            logical_type: ty.user_type.clone(),
            query: None,
        }),
    )
}

/// `MacroCatalogEntry::GetInfo` (the entry's temporary flag is not part of it).
pub(crate) fn macro_create_info(
    catalog: impl AsRef<[u8]>,
    schema: impl AsRef<[u8]>,
    entry: &CatalogEntry,
) -> Option<CreateInfo> {
    let catalog = catalog.as_ref();
    let schema = schema.as_ref();
    let (macros, table) = match &entry.kind {
        EntryKind::Macro(function) => (function, false),
        EntryKind::TableMacro(function) => (function, true),
        _ => return None,
    };
    let info = CreateMacroInfo {
        name: entry.name().to_owned(),
        macros: macros.macros.clone(),
    };
    let kind = if table {
        CreateInfoKind::TableMacro(info)
    } else {
        CreateInfoKind::Macro(info)
    };
    Some(entry_info(catalog, schema, entry, false, kind))
}

/// The catalog entry a `CREATE TYPE` or `CREATE MACRO` info describes (DuckDB's
/// `TypeCatalogEntry` / `MacroCatalogEntry` constructors).
pub fn entry_from_create_info(catalog: &Catalog, info: &CreateInfo) -> Result<CatalogEntry> {
    let mut entry = match &info.kind {
        CreateInfoKind::Type(ty) => catalog.new_entry(
            ty.name.clone(),
            EntryKind::Type(TypeEntry {
                user_type: ty.logical_type.clone(),
            }),
        ),
        CreateInfoKind::Macro(function) => catalog.new_entry(
            function.name.clone(),
            EntryKind::Macro(MacroEntry {
                macros: function.macros.clone(),
            }),
        ),
        CreateInfoKind::TableMacro(function) => catalog.new_entry(
            function.name.clone(),
            EntryKind::TableMacro(MacroEntry {
                macros: function.macros.clone(),
            }),
        ),
        _ => return Err(Error::internal("expected a type or macro")),
    };
    entry.temporary = info.temporary;
    entry.internal = info.internal;
    entry.dependencies.clone_from(&info.dependencies.set);
    entry.comment.clone_from(&info.comment);
    Ok(entry)
}

/// Writes `info` as a nullable object property (`WriteProperty(field, tag, entry pointer)`).
pub(crate) fn write_create_info(
    serializer: &mut dyn Serializer,
    field: rustdb_serde::FieldId,
    tag: &'static str,
    info: &CreateInfo,
) -> Result<()> {
    serializer.on_property_begin(field, tag);
    serializer.on_nullable_begin(true);
    serializer.write_object(info)?;
    serializer.on_nullable_end();
    serializer.on_property_end();
    Ok(())
}

/// Orders tables so that tables referenced by foreign keys come first
/// (`ReorderTableEntries`).
fn reorder_tables(tables: Vec<Arc<CatalogEntry>>) -> Vec<Arc<CatalogEntry>> {
    let mut remaining = tables;
    let mut ordered: Vec<Arc<CatalogEntry>> = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let before = remaining.len();
        let mut index = 0;
        while index < remaining.len() {
            let ready = match &remaining[index].kind {
                EntryKind::Table(table) => table.constraints.iter().all(|constraint| match constraint {
                    Constraint::ForeignKey(fk) if fk.fk_type == ForeignKeyType::FkTypeForeignKeyTable => {
                        ordered
                            .iter()
                            .any(|done| done.name().eq_ignore_ascii_case(&fk.table))
                    }
                    _ => true,
                }),
                _ => true,
            };
            if ready {
                ordered.push(remaining.remove(index));
            } else {
                index += 1;
            }
        }
        if remaining.len() == before {
            // A cycle or a reference to another schema: keep the rest as they are.
            ordered.append(&mut remaining);
        }
    }
    ordered
}

/// `ReorderTableEntries`: tables referenced by foreign keys first.
pub fn reorder_table_entries<T>(tables: Vec<(T, Arc<CatalogEntry>)>) -> Vec<(T, Arc<CatalogEntry>)> {
    let references = |entry: &CatalogEntry| -> Vec<rustdb_common::ByteString> {
        let EntryKind::Table(table) = &entry.kind else {
            return Vec::new();
        };
        table
            .constraints
            .iter()
            .filter_map(|constraint| match constraint {
                Constraint::ForeignKey(fk) if fk.fk_type == ForeignKeyType::FkTypeForeignKeyTable => {
                    Some(fk.table.clone())
                }
                _ => None,
            })
            .collect()
    };
    let mut ordered: Vec<(T, Arc<CatalogEntry>)> = Vec::new();
    // `ScanForeignKeyTable`: moves the tables whose references are ordered.
    let scan = |ordered: &mut Vec<(T, Arc<CatalogEntry>)>,
                unordered: Vec<(T, Arc<CatalogEntry>)>,
                primary_keys_only: bool| {
        let mut remaining = Vec::new();
        for (schema, entry) in unordered {
            let refs = references(&entry);
            let movable = if primary_keys_only {
                refs.is_empty()
            } else {
                refs.iter().all(|table| {
                    ordered
                        .iter()
                        .any(|(_, done)| done.name().eq_ignore_ascii_case(table))
                })
            };
            if movable {
                ordered.push((schema, entry));
            } else {
                remaining.push((schema, entry));
            }
        }
        remaining
    };
    let mut unordered = scan(&mut ordered, tables, true);
    while !unordered.is_empty() {
        let before = unordered.len();
        unordered = scan(&mut ordered, unordered, false);
        if unordered.len() == before {
            // A reference that never resolves (DuckDB would loop): keep the rest as is.
            ordered.append(&mut unordered);
        }
    }
    ordered
}

/// The `CREATE` statements of a catalog's entries in the order `EXPORT DATABASE`
/// writes them (DuckDB's `PhysicalExport::GetNaiveExportOrder`): schemas, sequences,
/// types, tables (referenced tables first), macros (by creation), views, indexes. The
/// catalog name is left out.
pub fn export_create_infos(catalog: &Catalog, transaction: &Transaction) -> Vec<CreateInfo> {
    let mut schemas = Vec::new();
    let mut sequences = Vec::new();
    let mut types = Vec::new();
    let mut tables = Vec::new();
    let mut macros = Vec::new();
    let mut views = Vec::new();
    let mut indexes = Vec::new();
    for schema in catalog.schemas().scan(transaction) {
        let Some(schema_entry) = schema.as_schema() else {
            continue;
        };
        let schema_name = schema.name().to_owned();
        if !schema.internal {
            schemas.push(schema_create_info("", &schema_name));
        }
        for entry in schema_entry.tables.scan(transaction) {
            if entry.internal {
                continue;
            }
            match &entry.kind {
                EntryKind::Table(_) => tables.push((schema_name.clone(), entry)),
                EntryKind::View(view) => views.push(view_create_info("", &schema_name, &entry, view)),
                _ => {}
            }
        }
        for entry in schema_entry.sequences.scan(transaction) {
            if let (false, EntryKind::Sequence(sequence)) = (entry.internal, &entry.kind) {
                sequences.push(sequence_create_info("", &schema_name, &entry, sequence));
            }
        }
        for entry in schema_entry.types.scan(transaction) {
            if let (false, EntryKind::Type(ty)) = (entry.internal, &entry.kind) {
                types.push(type_create_info("", &schema_name, &entry, ty));
            }
        }
        for entry in schema_entry.indexes.scan(transaction) {
            if let (false, EntryKind::Index(index)) = (entry.internal, &entry.kind) {
                indexes.push(entry_info(
                    "",
                    &schema_name,
                    &entry,
                    entry.temporary,
                    CreateInfoKind::Index(Box::new(index.info.clone())),
                ));
            }
        }
        for entry in schema_entry
            .functions
            .scan(transaction)
            .into_iter()
            .chain(schema_entry.table_functions.scan(transaction))
        {
            if !entry.internal && matches!(entry.kind, EntryKind::Macro(_) | EntryKind::TableMacro(_)) {
                macros.push((entry.oid, schema_name.clone(), entry));
            }
        }
    }
    let mut ordered_tables = Vec::new();
    for (schema, entry) in reorder_table_entries(tables) {
        if let EntryKind::Table(table) = &entry.kind {
            ordered_tables.push(table_create_info("", &schema, &entry, table));
        }
    }
    macros.sort_by_key(|(oid, _, _)| *oid);
    let macros = macros
        .iter()
        .filter_map(|(_, schema, entry)| macro_create_info("", schema, entry));
    let mut result = schemas;
    result.extend(sequences);
    result.extend(types);
    result.extend(ordered_tables);
    result.extend(macros);
    result.extend(views);
    result.extend(indexes);
    reorder_by_dependencies(result)
}

/// The name of the entry `info` creates.
fn create_info_name(info: &CreateInfo) -> &[u8] {
    match &info.kind {
        CreateInfoKind::Schema => &info.schema,
        CreateInfoKind::Table(table) => &table.table,
        CreateInfoKind::View(view) => &view.view_name,
        CreateInfoKind::Sequence(sequence) => &sequence.name,
        CreateInfoKind::Type(ty) => &ty.name,
        CreateInfoKind::Macro(function) | CreateInfoKind::TableMacro(function) => &function.name,
        CreateInfoKind::Index(index) => &index.name,
        CreateInfoKind::Secret(_) => b"",
    }
}

/// `DependencyManager::ReorderEntries`: every entry comes after the entries it depends on.
fn reorder_by_dependencies(infos: Vec<CreateInfo>) -> Vec<CreateInfo> {
    fn visit(index: usize, infos: &[CreateInfo], visited: &mut [bool], order: &mut Vec<usize>) {
        if visited[index] {
            return;
        }
        visited[index] = true;
        for dependency in &infos[index].dependencies.set {
            let found = infos.iter().position(|info| {
                info.kind.catalog_type() == dependency.entry.catalog_type
                    && info.schema.eq_ignore_ascii_case(&dependency.entry.schema)
                    && create_info_name(info).eq_ignore_ascii_case(dependency.entry.name.as_bytes())
            });
            if let Some(found) = found {
                visit(found, infos, visited, order);
            }
        }
        order.push(index);
    }
    let mut visited = vec![false; infos.len()];
    let mut order = Vec::with_capacity(infos.len());
    for index in 0..infos.len() {
        visit(index, &infos, &mut visited, &mut order);
    }
    let mut slots: Vec<Option<CreateInfo>> = infos.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|index| slots[index].take())
        .collect()
}

/// What a checkpoint does with the WAL before it writes anything (`WALStartCheckpoint`):
/// called with the root the checkpoint will have and a function to call while commits
/// wait, it records the root and makes commits go to the checkpoint WAL; it returns the
/// last commit the checkpoint includes.
pub type WalStart<'a> = dyn FnMut(MetaBlockPointer, &mut dyn FnMut()) -> Result<u64> + 'a;

/// Writes a checkpoint: the catalog visible to `transaction` and all table data
/// (`SingleFileCheckpointWriter::CreateCheckpoint`).
///
/// `on_start` starts the checkpoint around the WAL (see [`WalStart`]). Commits continue
/// while the tables are written: a commit that appends to a table waits while the
/// checkpoint writes it, and rows appended earlier go to row groups the checkpoint leaves
/// out. Tables with indexes are locked from the start, so that the written indexes hold
/// the rows of the written data.
pub fn write_checkpoint(
    catalog: &Catalog,
    storage: &StorageManager,
    transaction: &Transaction,
    on_start: &mut WalStart<'_>,
    replace_in_memory: bool,
    vacuum: bool,
    lowest_active_start: u64,
) -> Result<()> {
    if storage.is_read_only() {
        return Ok(());
    }
    let metadata = storage.metadata();
    let options = storage.serialization_options();
    let mut writer = MetadataWriter::new(metadata);
    let mut table_writer = MetadataWriter::new(metadata);
    let meta_block = writer.meta_block_pointer()?;
    let mut blocks = BlockWriter::new(Arc::clone(storage.block_manager()));
    let table_settings = storage.table_write_settings(replace_in_memory && vacuum, lowest_active_start);
    let catalog_name = catalog.name();
    let mut persisted = Vec::new();

    // `GetCatalogEntries`.
    let mut entries: Vec<(rustdb_common::ByteString, Arc<CatalogEntry>)> = Vec::new();
    for schema in catalog.schemas().scan(transaction) {
        let Some(schema_entry) = schema.as_schema() else {
            continue;
        };
        let schema_name = schema.name().to_owned();
        entries.push((schema_name.clone(), Arc::clone(&schema)));
        for ty in schema_entry.types.scan(transaction) {
            if !ty.internal {
                entries.push((schema_name.clone(), ty));
            }
        }
        for sequence in schema_entry.sequences.scan(transaction) {
            if !sequence.internal {
                entries.push((schema_name.clone(), sequence));
            }
        }
        let mut tables = Vec::new();
        let mut views = Vec::new();
        for entry in schema_entry.tables.scan(transaction) {
            if entry.internal {
                continue;
            }
            match entry.kind {
                EntryKind::Table(_) => tables.push(entry),
                EntryKind::View(_) => views.push(entry),
                _ => {}
            }
        }
        for table in reorder_tables(tables) {
            entries.push((schema_name.clone(), table));
        }
        for view in views {
            entries.push((schema_name.clone(), view));
        }
        for function in schema_entry.functions.scan(transaction) {
            if !function.internal && matches!(function.kind, EntryKind::Macro(_)) {
                entries.push((schema_name.clone(), function));
            }
        }
        for function in schema_entry.table_functions.scan(transaction) {
            if !function.internal && matches!(function.kind, EntryKind::TableMacro(_)) {
                entries.push((schema_name.clone(), function));
            }
        }
        for index in schema_entry.indexes.scan(transaction) {
            entries.push((schema_name.clone(), index));
        }
    }

    // The tables with indexes are locked before commits continue.
    let indexed_tables: Vec<Arc<DataTable>> = entries
        .iter()
        .filter_map(|(_, entry)| match &entry.kind {
            EntryKind::Table(table) => table.storage.clone(),
            _ => None,
        })
        .filter(|table| table.has_indexes())
        .collect();
    let mut table_locks: Vec<Option<ExclusiveCheckpointGuard>> = Vec::new();
    let checkpoint_id = on_start(meta_block, &mut || {
        table_locks = indexed_tables
            .iter()
            .map(|table| Some(table.info().checkpoint_lock().exclusive()))
            .collect();
    })?;
    let sleep_ms = storage.checkpoint_config().sleep_ms;
    if sleep_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
    }

    let mut serializer = BinarySerializer::new(options.clone());
    serializer.begin();
    {
        let serializer: &mut dyn Serializer = &mut serializer;
        serializer.on_property_begin(100, "catalog_entries");
        serializer.on_list_begin(entries.len());
        for (schema, entry) in &entries {
            serializer.on_object_begin();
            match &entry.kind {
                EntryKind::Schema(_) => {
                    serializer.write_property(99, "catalog_type", &AstCatalogType::SchemaEntry)?;
                    // `SchemaCatalogEntry::GetInfo`: the schema name and comment, no catalog.
                    let mut info = base_info("", entry.name(), false, CreateInfoKind::Schema);
                    info.comment.clone_from(&entry.comment);
                    write_create_info(serializer, 100, "schema", &info)?;
                }
                EntryKind::Type(ty) => {
                    serializer.write_property(99, "catalog_type", &AstCatalogType::TypeEntry)?;
                    write_create_info(
                        serializer,
                        100,
                        "type",
                        &type_create_info(&catalog_name, schema, entry, ty),
                    )?;
                }
                EntryKind::Macro(_) | EntryKind::TableMacro(_) => {
                    let info = macro_create_info(&catalog_name, schema, entry)
                        .ok_or_else(|| Error::internal("expected a macro"))?;
                    let (ty, tag) = if matches!(entry.kind, EntryKind::Macro(_)) {
                        (AstCatalogType::MacroEntry, "macro")
                    } else {
                        (AstCatalogType::TableMacroEntry, "table_macro")
                    };
                    serializer.write_property(99, "catalog_type", &ty)?;
                    write_create_info(serializer, 100, tag, &info)?;
                }
                EntryKind::Sequence(sequence) => {
                    serializer.write_property(99, "catalog_type", &AstCatalogType::SequenceEntry)?;
                    write_create_info(
                        serializer,
                        100,
                        "sequence",
                        &sequence_create_info(&catalog_name, schema, entry, sequence),
                    )?;
                }
                EntryKind::Table(table) => {
                    serializer.write_property(99, "catalog_type", &AstCatalogType::TableEntry)?;
                    write_create_info(
                        serializer,
                        100,
                        "table",
                        &table_create_info(&catalog_name, schema, entry, table),
                    )?;
                    let storage_table = table
                        .storage
                        .as_ref()
                        .ok_or_else(|| Error::internal("checkpoint of a table without storage"))?;
                    // Commits appending to the table wait while it is written
                    // (`DataTable::GetCheckpointLock`).
                    let table_lock = match indexed_tables
                        .iter()
                        .position(|indexed| Arc::ptr_eq(indexed, storage_table))
                    {
                        Some(position) => table_locks.get_mut(position).and_then(Option::take),
                        None => Some(storage_table.info().checkpoint_lock().exclusive()),
                    };
                    let mut data = write_table_data(
                        storage_table,
                        metadata,
                        &mut table_writer,
                        &mut blocks,
                        &options,
                        checkpoint_id,
                        table_settings,
                    )?;
                    serializer.write_object_property(101, "table_pointer", |s| {
                        serialize_meta_block_pointer(s, data.table_pointer)
                    })?;
                    serializer.write_property(102, "total_rows", &data.total_rows)?;
                    let index_infos = serialize_indexes(
                        storage_table,
                        data.rebuilt_indexes.as_deref_mut(),
                        storage,
                        &options,
                        &mut blocks,
                    )?;
                    persisted.push((Arc::clone(storage_table), data));
                    drop(table_lock);
                    // Empty block pointers for forwards compatibility.
                    serializer.on_property_begin(103, "index_pointers");
                    serializer.on_list_begin(0);
                    serializer.on_list_end();
                    serializer.on_property_end();
                    serializer.write_list_property(
                        104,
                        "index_storage_infos",
                        index_infos.len(),
                        |s, index| {
                            s.on_object_begin();
                            rustdb_storage::checkpoint::pointers::serialize_index_storage_info(
                                s,
                                &index_infos[index],
                            )?;
                            s.on_object_end();
                            Ok(())
                        },
                    )?;
                }
                EntryKind::View(view) => {
                    serializer.write_property(99, "catalog_type", &AstCatalogType::ViewEntry)?;
                    write_create_info(
                        serializer,
                        100,
                        "view",
                        &view_create_info(&catalog_name, schema, entry, view),
                    )?;
                }
                EntryKind::Index(index) => {
                    serializer.write_property(99, "catalog_type", &AstCatalogType::IndexEntry)?;
                    let info = entry_info(
                        &catalog_name,
                        schema,
                        entry,
                        entry.temporary,
                        CreateInfoKind::Index(Box::new(index.info.clone())),
                    );
                    write_create_info(serializer, 100, "index", &info)?;
                }
                _ => {}
            }
            serializer.on_object_end();
        }
        serializer.on_list_end();
        serializer.on_property_end();
    }
    serializer.end();
    writer.write_data(serializer.bytes())?;
    writer.flush()?;
    table_writer.flush()?;
    blocks.flush()?;
    let abort = storage.checkpoint_config().abort;
    abort.check(CheckpointAbort::BeforeHeader)?;
    abort.check(CheckpointAbort::BeforeHeaderNonFatal)?;
    let block_manager = storage.block_manager();
    let header = DatabaseHeader {
        iteration: 0,
        meta_block: meta_block.block_pointer,
        free_list: 0,
        block_count: 0,
        block_alloc_size: rustdb_buffer::BlockManager::block_alloc_size(block_manager.as_ref()) as u64,
        vector_size: STANDARD_VECTOR_SIZE,
        serialization_compatibility: block_manager.storage_version(),
    };
    block_manager.write_header(metadata, header, &blocks.used_blocks, abort)?;
    abort.check(CheckpointAbort::BeforeTruncate)?;
    block_manager.truncate()?;
    abort.check(CheckpointAbort::BeforeWalFinish)?;
    for (table, data) in persisted {
        finish_table_checkpoint(&table, data, replace_in_memory)?;
    }
    Ok(())
}

/// Checkpoints an in-memory database (DuckDB's `InMemoryCheckpointer::CreateCheckpoint`):
/// the data of every committed table is vacuumed (by a `full_checkpoint`) and, unless
/// `vacuum_only` (`CheckpointType::VACUUM_ONLY`), compressed into in-memory blocks; then
/// the estimated size of the changes is reset. The caller guarantees that no transaction
/// writes to the database, and for a full checkpoint that none needs an older version of
/// its data.
pub fn checkpoint_in_memory(
    catalog: &Catalog,
    storage: &InMemoryStorageManager,
    full_checkpoint: bool,
    vacuum_only: bool,
    lowest_active_start: u64,
) -> Result<()> {
    let transaction = Transaction::system();
    let mut tables = Vec::new();
    for schema in catalog.schemas().scan(&transaction) {
        let Some(schema_entry) = schema.as_schema() else {
            continue;
        };
        for entry in schema_entry.tables.scan(&transaction) {
            if let EntryKind::Table(table) = &entry.kind
                && let Some(storage_table) = &table.storage
            {
                tables.push(Arc::clone(storage_table));
            }
        }
    }
    let settings = storage.table_write_settings(full_checkpoint, lowest_active_start);
    let abort = storage.checkpoint_config().abort;
    for table in tables {
        checkpoint_in_memory_table(&table, storage.compression_config(), settings, vacuum_only)?;
        abort.check(CheckpointAbort::InMemoryCheckpoint)?;
    }
    storage.set_wal_size(0);
    Ok(())
}

/// Writes the data of every index of `table` (`TableIndexList::SerializeToDisk`). Each
/// index fills its own partial blocks.
fn serialize_indexes(
    table: &DataTable,
    mut rebuilt: Option<&mut [rustdb_storage::index::art::Art]>,
    storage: &StorageManager,
    options: &rustdb_serde::SerializationOptions,
    blocks: &mut BlockWriter,
) -> Result<Vec<rustdb_storage::index::art::ArtStorageInfo>> {
    let mut index_options = CaseInsensitiveMap::default();
    if options.compatibility.serialization_version >= 3 {
        index_options.insert("v1_0_0_storage", Value::boolean(false));
    }
    let mut infos = Vec::new();
    // The position of the next bound index in `rebuilt`.
    let mut bound = 0;
    for slot in table.info().indexes().slots() {
        let mut index_blocks = BlockWriter::new(Arc::clone(storage.block_manager()));
        // Unchanged buffers of a bound index and an unbound index (written as it was
        // stored) keep their blocks, which stay in use.
        let mut kept = Vec::new();
        let info = match &slot {
            // Indexes rebuilt by a vacuum are written instead of the table's current ones,
            // which the table replaces by them once the checkpoint is complete.
            IndexSlot::Bound(_)
                if let Some(index) = rebuilt.as_deref_mut().and_then(|rebuilt| rebuilt.get_mut(bound)) =>
            {
                bound += 1;
                index.serialize_to_disk(&index_options, &mut index_blocks, &mut |block| kept.push(block))?
            }
            IndexSlot::Bound(entry) => {
                bound += 1;
                entry
                    .lock()
                    .index
                    .serialize_to_disk(&index_options, &mut index_blocks, &mut |block| kept.push(block))?
            }
            IndexSlot::Unbound(index) => {
                if index.has_buffered_replays() {
                    return Err(Error::internal(
                        "checkpoint requires binding pending index replays",
                    ));
                }
                index.serialize_to_disk(&mut index_blocks, &mut |block| kept.push(block))?
            }
        };
        for block in kept {
            *blocks.used_blocks.entry(block).or_insert(0) += 1;
        }
        for (block, count) in index_blocks.used_blocks {
            *blocks.used_blocks.entry(block).or_insert(0) += count;
        }
        infos.push(info);
    }
    Ok(infos)
}

/// The physical key columns of a UNIQUE constraint.
fn unique_keys(table: &TableEntry, unique: &rustdb_ast::UniqueConstraint) -> Result<Vec<usize>> {
    if unique.index.0 != u64::MAX {
        return table
            .logical_to_physical(unique.index.0 as usize)
            .map(|p| vec![p])
            .ok_or_else(|| Error::internal("UNIQUE constraint on a generated column"));
    }
    unique
        .columns
        .iter()
        .map(|name| {
            table
                .column(name)
                .and_then(|(logical, _)| table.logical_to_physical(logical))
                .ok_or_else(|| Error::internal("UNIQUE constraint on an unknown column"))
        })
        .collect()
}

/// Reads a `unique_ptr<CreateInfo>` property (`ReadCreateInfo`).
fn read_create_info(
    deserializer: &mut dyn Deserializer,
    expected: AstCatalogType,
    tag: &'static str,
) -> Result<CreateInfo> {
    read_create_info_property(deserializer, 100, expected, tag)
}

/// Reads the `unique_ptr<CreateInfo>` property `field` of an entry of type `expected`.
pub fn read_create_info_property(
    deserializer: &mut dyn Deserializer,
    field: rustdb_serde::FieldId,
    expected: AstCatalogType,
    tag: &'static str,
) -> Result<CreateInfo> {
    deserializer.on_property_begin(field, tag)?;
    if !deserializer.on_nullable_begin()? {
        return Err(Error::new_bytes(
            rustdb_common::ErrorKind::Io,
            rustdb_common::byte_format!("corrupt database file - {tag} entry without create info"),
        ));
    }
    let info: CreateInfo = deserializer.read_object()?;
    deserializer.on_nullable_end()?;
    deserializer.on_property_end()?;
    if info.catalog_type() != expected {
        return Err(Error::new_bytes(
            rustdb_common::ErrorKind::Io,
            rustdb_common::byte_format!("corrupt database file - catalog entry type mismatch for {tag}"),
        ));
    }
    Ok(info)
}

/// Loads the catalog of the last checkpoint (`SingleFileCheckpointReader::LoadFromStorage`).
/// Entries are created as committed system entries.
pub fn load_checkpoint(catalog: &Catalog, storage: &StorageManager) -> Result<()> {
    let meta_block = storage.meta_block();
    if !meta_block.is_valid() {
        return Ok(());
    }
    let metadata = storage.metadata();
    let transaction = Transaction::system();
    // The stored index data per table, taken by the index entries that follow
    // (`DataTableInfo::ExtractIndexStorageInfo`).
    let mut index_storage: Vec<(
        rustdb_common::ByteString,
        rustdb_common::ByteString,
        Vec<ArtStorageInfo>,
    )> = Vec::new();
    let mut reader = MetadataReader::new(metadata, meta_block, true)?;
    let mut deserializer = StreamDeserializer::new(&mut reader);
    deserializer.begin()?;
    let d: &mut dyn Deserializer = &mut deserializer;
    d.on_property_begin(100, "catalog_entries")?;
    let count = d.on_list_begin()?;
    for _ in 0..count {
        d.on_object_begin()?;
        let ty: AstCatalogType = d.read_property(99, "type")?;
        match ty {
            AstCatalogType::SchemaEntry => {
                let info = read_create_info(d, ty, "schema")?;
                catalog.create_schema_committed(&info.schema)?;
            }
            AstCatalogType::TypeEntry | AstCatalogType::MacroEntry | AstCatalogType::TableMacroEntry => {
                let tag = match ty {
                    AstCatalogType::TypeEntry => "type",
                    AstCatalogType::MacroEntry => "macro",
                    _ => "table_macro",
                };
                let info = read_create_info(d, ty, tag)?;
                catalog.create_entry_committed(&info.schema, entry_from_create_info(catalog, &info)?)?;
            }
            AstCatalogType::SequenceEntry => {
                let info = read_create_info(d, ty, "sequence")?;
                let CreateInfoKind::Sequence(sequence) = &info.kind else {
                    return Err(Error::internal("expected a sequence"));
                };
                let mut entry = catalog.new_entry(
                    sequence.name.clone(),
                    EntryKind::Sequence(SequenceEntry::new(sequence)),
                );
                entry.temporary = info.temporary;
                entry.comment.clone_from(&info.comment);
                catalog.create_entry_committed(&info.schema, entry)?;
            }
            AstCatalogType::TableEntry => {
                let info = read_create_info(d, ty, "table")?;
                let table_pointer: MetaBlockPointer =
                    d.read_object_property(101, "table_pointer", deserialize_meta_block_pointer)?;
                let total_rows: u64 = d.read_property(102, "total_rows")?;
                // Files of DuckDB versions before v0.10 point to each index's root
                // (`index_pointers`, with deprecated storage). Decode their allocator
                // metadata so the buffers can still be loaded lazily.
                let legacy_pointers = read_block_pointer_list(d, 103, "index_pointers")?.unwrap_or_default();
                let mut index_versions = std::collections::HashMap::new();
                let mut stored_indexes = legacy_pointers
                    .into_iter()
                    .filter(rustdb_storage::checkpoint::pointers::BlockPointer::is_valid)
                    .map(|pointer| rustdb_storage::index::art::read_legacy_index_storage(metadata, pointer))
                    .collect::<Result<Vec<_>>>()?;
                if d.on_optional_property_begin(104, "index_storage_infos")? {
                    // Modern checkpoints embed the allocator metadata directly.
                    let stored = d.on_list_begin()?;
                    for _ in 0..stored {
                        d.on_object_begin()?;
                        let info = deserialize_index_storage_info(d)?;
                        d.on_object_end()?;
                        // The storage version the index was created with, recorded
                        // since v1.5.0.
                        let version = info
                            .options
                            .get("storage_version")
                            .and_then(rustdb_types::Value::as_u64);
                        index_versions.insert(info.name.clone(), version);
                        stored_indexes.push(info);
                    }
                    d.on_list_end()?;
                    d.on_optional_property_end(true)?;
                }
                let CreateInfoKind::Table(create) = &info.kind else {
                    return Err(Error::internal("expected a table"));
                };
                let mut entry = TableEntry::from_create_info(create);
                let columns: Vec<StorageColumn> = entry
                    .physical_columns()
                    .map(|(_, _, column)| {
                        StorageColumn::new(column.name.clone(), column.logical_type.clone())
                            .with_compression(column.storage_compression())
                    })
                    .collect();
                let types: Vec<LogicalType> = columns.iter().map(|c| c.ty.clone()).collect();
                let data = read_table_data(metadata, table_pointer, &types, total_rows)?;
                let collection = load_row_groups(storage.io(), metadata, &types, data)?;
                let table = DataTable::from_collection(
                    Arc::clone(storage.io()),
                    info.schema.clone(),
                    create.table.clone(),
                    columns,
                    collection,
                );
                table.info().set_stored_index_versions(index_versions);
                for (index, constraint) in entry.constraints.iter().enumerate() {
                    match constraint {
                        Constraint::Unique(unique) => {
                            let kind = if unique.is_primary_key {
                                IndexConstraintType::Primary
                            } else {
                                IndexConstraintType::Unique
                            };
                            restore_constraint_index(
                                &table,
                                kind,
                                &unique_keys(&entry, unique)?,
                                index,
                                &mut stored_indexes,
                            )?;
                        }
                        Constraint::ForeignKey(fk)
                            if matches!(
                                fk.fk_type,
                                ForeignKeyType::FkTypeForeignKeyTable
                                    | ForeignKeyType::FkTypeSelfReferenceTable
                            ) =>
                        {
                            let keys: Vec<usize> = fk.fk_keys.iter().map(|k| k.0 as usize).collect();
                            restore_constraint_index(
                                &table,
                                IndexConstraintType::Foreign,
                                &keys,
                                index,
                                &mut stored_indexes,
                            )?;
                        }
                        _ => {}
                    }
                }
                entry.storage = Some(table);
                if !stored_indexes.is_empty() {
                    index_storage.push((info.schema.clone(), create.table.clone(), stored_indexes));
                }
                let mut catalog_entry = catalog.new_entry(create.table.clone(), EntryKind::Table(entry));
                catalog_entry.temporary = info.temporary;
                catalog_entry.dependencies.clone_from(&info.dependencies.set);
                catalog_entry.comment.clone_from(&info.comment);
                catalog.create_entry_committed(&info.schema, catalog_entry)?;
            }
            AstCatalogType::ViewEntry => {
                let info = read_create_info(d, ty, "view")?;
                let CreateInfoKind::View(view) = &info.kind else {
                    return Err(Error::internal("expected a view"));
                };
                let mut entry = catalog.new_entry(
                    view.view_name.clone(),
                    EntryKind::View(ViewEntry {
                        query: (*view.query).clone(),
                        aliases: view.aliases.clone(),
                        types: view.types.clone(),
                        names: view.names.clone(),
                        column_comments: view.column_comments.clone(),
                    }),
                );
                entry.temporary = info.temporary;
                entry.dependencies.clone_from(&info.dependencies.set);
                entry.comment.clone_from(&info.comment);
                catalog.create_entry_committed(&info.schema, entry)?;
            }
            AstCatalogType::IndexEntry => {
                let info = read_create_info(d, ty, "index")?;
                // Files of DuckDB versions before v0.10 store the index behind a root
                // block pointer (`ReadIndex`).
                let root = if d.on_optional_property_begin(101, "root_block_pointer")? {
                    d.on_object_begin()?;
                    let pointer = rustdb_storage::checkpoint::pointers::BlockPointer::deserialize_fields(d)?;
                    d.on_object_end()?;
                    d.on_optional_property_end(true)?;
                    pointer.is_valid().then_some(pointer)
                } else {
                    None
                };
                let stored = match root {
                    Some(pointer) => UnboundIndexStorage::Disk(
                        rustdb_storage::index::art::read_legacy_index_storage(metadata, pointer)?,
                    ),
                    None => UnboundIndexStorage::Disk(extract_index_storage(&mut index_storage, &info)),
                };
                load_index(catalog, &transaction, &info, stored)?;
            }
            other => {
                let _ = other;
                return Err(Error::new_bytes(
                    rustdb_common::ErrorKind::Io,
                    b"corrupt database file - unrecognized catalog type in checkpoint".as_slice(),
                ));
            }
        }
        d.on_object_end()?;
    }
    d.on_list_end()?;
    d.on_property_end()?;
    deserializer.end()?;
    Ok(())
}

/// Reads an optional list of `BlockPointer` objects.
fn read_block_pointer_list(
    deserializer: &mut dyn Deserializer,
    field: u16,
    tag: &'static str,
) -> Result<Option<Vec<rustdb_storage::checkpoint::pointers::BlockPointer>>> {
    if !deserializer.on_optional_property_begin(field, tag)? {
        return Ok(None);
    }
    let mut result = Vec::new();
    deserializer.read_list(|d, _| {
        d.on_object_begin()?;
        result.push(rustdb_storage::checkpoint::pointers::BlockPointer::deserialize_fields(d)?);
        d.on_object_end()
    })?;
    deserializer.on_optional_property_end(true)?;
    Ok(Some(result))
}

/// Takes the stored data of index `info` from those read with its table
/// (`DataTableInfo::ExtractIndexStorageInfo`); an index without one gets an empty
/// description.
fn extract_index_storage(
    index_storage: &mut [(
        rustdb_common::ByteString,
        rustdb_common::ByteString,
        Vec<ArtStorageInfo>,
    )],
    info: &CreateInfo,
) -> ArtStorageInfo {
    let CreateInfoKind::Index(index) = &info.kind else {
        return ArtStorageInfo::default();
    };
    index_storage
        .iter_mut()
        .filter(|(schema, table, _)| {
            schema.eq_ignore_ascii_case(&info.schema) && table.eq_ignore_ascii_case(&index.table)
        })
        .find_map(|(_, _, stored)| {
            let position = stored.iter().position(|stored| stored.name == index.name)?;
            Some(stored.remove(position))
        })
        .unwrap_or_else(|| ArtStorageInfo {
            name: index.name.clone(),
            ..ArtStorageInfo::default()
        })
}

/// Constraint buffers precede explicit indexes, in constraint order, as in C++.
/// ALTER/rename can preserve names different from the current generated ordinal.
fn restore_constraint_index(
    table: &DataTable,
    kind: IndexConstraintType,
    columns: &[usize],
    index: usize,
    stored: &mut Vec<ArtStorageInfo>,
) -> Result<()> {
    let name = rustdb_common::byte_format!("{}_{}_{index}", kind.name(), table.name());
    let position = stored
        .iter()
        .position(|info| info.name.eq_ignore_ascii_case(&name))
        .or_else(|| stored.iter().position(|info| info.name.is_empty()))
        .or_else(|| (!stored.is_empty()).then_some(0));
    if let Some(position) = position {
        let info = stored.remove(position);
        if table.restore_constraint_index(kind, columns, index, &info)? {
            return Ok(());
        }
    }
    table.load_constraint_index(kind, columns, index)
}

/// Creates an index entry read from a checkpoint (`CheckpointReader::ReadIndex`).
fn load_index(
    catalog: &Catalog,
    transaction: &Transaction,
    info: &CreateInfo,
    stored: UnboundIndexStorage,
) -> Result<()> {
    let (mut entry, _) = build_index_entry(catalog, transaction, info, stored)?;
    entry.comment.clone_from(&info.comment);
    catalog.create_entry_committed(&info.schema, entry)
}

/// Adds the index `info` to its table and returns the index's catalog entry and the
/// table. An index over plain columns restores its persisted buffers when available;
/// an index over expressions waits unbound until a connection binds it (`UnboundIndex`), keeping its
/// `stored` data meanwhile.
fn build_index_entry(
    catalog: &Catalog,
    transaction: &Transaction,
    info: &CreateInfo,
    stored: UnboundIndexStorage,
) -> Result<(CatalogEntry, Arc<DataTable>)> {
    let CreateInfoKind::Index(index) = &info.kind else {
        return Err(Error::internal("expected an index"));
    };
    let table_entry = catalog
        .get_entry(transaction, CatalogType::Table, &info.schema, &index.table)
        .ok_or_else(|| {
            Error::new_bytes(
                rustdb_common::ErrorKind::Io,
                b"corrupt database file - index entry without table entry".as_slice(),
            )
        })?;
    let EntryKind::Table(table) = &table_entry.kind else {
        return Err(Error::internal("index on a non-table"));
    };
    let storage = table
        .storage
        .clone()
        .ok_or_else(|| Error::internal("index on a table without storage"))?;
    let plain_columns: Option<Vec<usize>> = index
        .parsed_expressions
        .iter()
        .map(|expression| match &expression.kind {
            ParsedExpressionKind::ColumnRef(column) => column
                .column_names
                .last()
                .and_then(|name| table.column(name))
                .and_then(|(logical, _)| table.logical_to_physical(logical)),
            _ => None,
        })
        .collect();
    let constraint = match index.constraint_type {
        AstIndexConstraintType::Unique => IndexConstraintType::Unique,
        AstIndexConstraintType::Primary => IndexConstraintType::Primary,
        AstIndexConstraintType::Foreign => IndexConstraintType::Foreign,
        AstIndexConstraintType::None => IndexConstraintType::None,
    };
    match plain_columns {
        Some(columns) if !columns.is_empty() => {
            let mut art = storage.new_art(&index.name, constraint, &columns)?;
            let restored = match &stored {
                UnboundIndexStorage::Disk(info) => storage.restore_index(&mut art, info)?,
                UnboundIndexStorage::Wal(info, buffers) => {
                    storage.restore_wal_index(&mut art, info, buffers)?
                }
                UnboundIndexStorage::Deprecated => false,
            };
            if !restored {
                storage.build_index(transaction, &mut art, false)?;
            }
            storage.add_index(art)?;
        }
        _ => {
            let column_ids = index
                .column_ids
                .iter()
                .map(|&column| {
                    usize::try_from(column).map_err(|_| Error::serialization("invalid index column"))
                })
                .collect::<Result<Vec<_>>>()?;
            let index_type = if index.index_type_name.is_empty() {
                b"ART".as_slice()
            } else {
                index.index_type_name.as_bytes()
            };
            storage.add_unbound_index(UnboundIndex::new(
                index.name.clone(),
                index_type,
                constraint,
                column_ids,
                Arc::new(info.clone()),
                stored,
            ))?;
        }
    }
    let mut entry = catalog.new_entry(
        index.name.clone(),
        EntryKind::Index(IndexEntry {
            info: (**index).clone(),
            schema: info.schema.clone(),
            storage: Some(Arc::clone(storage.info())),
        }),
    );
    entry.temporary = info.temporary;
    Ok((entry, storage))
}

/// Recreates the catalog entry but returns its native index staged until commit.
/// The stored tree already includes rows inserted by its creating transaction.
pub fn replay_create_index(
    catalog: &Catalog,
    transaction: &Transaction,
    info: &CreateInfo,
    stored: UnboundIndexStorage,
) -> Result<(Arc<rustdb_storage::DataTableInfo>, IndexSlot)> {
    let (entry, storage) = build_index_entry(catalog, transaction, info, stored)?;
    let name = entry.name().to_owned();
    let index = storage
        .indexes()
        .take_for_replay(&name)
        .ok_or_else(|| Error::internal("replayed index missing from table"))?;
    catalog.create_entry(
        transaction,
        &info.schema,
        entry,
        OnCreateConflict::ErrorOnConflict,
    )?;
    Ok((Arc::clone(storage.info()), index))
}
