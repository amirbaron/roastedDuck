//! Checkpointing the catalog to a database file and loading it back (DuckDB's
//! `SingleFileCheckpointWriter` and `CheckpointReader`).
//!
//! The catalog is written as one list of entries: per schema its sequences, tables
//! (referenced tables before referencing ones), views and indexes. Each entry is its
//! `CreateInfo`; tables are followed by the pointer to their data.

use std::collections::BTreeSet;
use std::sync::Arc;

use rustdb_ast::{
    CatalogType as AstCatalogType, ColumnDefinition, ColumnList, CompressionType, Constraint,
    CreateIndexInfo, CreateInfo, CreateInfoKind, CreateMacroInfo, CreateSequenceInfo, CreateTableInfo,
    CreateTypeInfo, CreateViewInfo, ForeignKeyType, IndexConstraintType as AstIndexConstraintType,
    LogicalDependencyList, OnCreateConflict, ParsedExpressionKind, TableColumnType,
};
use rustdb_buffer::BlockId;
use rustdb_common::{CaseInsensitiveMap, Error, OrderedMap, Result};
use rustdb_serde::binary::BinarySerializer;
use rustdb_serde::{Deserializer, Serializer};
use rustdb_storage::checkpoint::pointers::{deserialize_meta_block_pointer, serialize_meta_block_pointer};
use rustdb_storage::checkpoint::stream::StreamDeserializer;
use rustdb_storage::checkpoint::table_reader::{load_row_groups, read_table_data};
use rustdb_storage::checkpoint::table_writer::{BlockWriter, write_table_data};
use rustdb_storage::index::IndexConstraintType;
use rustdb_storage::metadata::{MetaBlockPointer, MetadataReader, MetadataWriter};
use rustdb_storage::storage_info::{DatabaseHeader, STANDARD_VECTOR_SIZE};
use rustdb_storage::storage_manager::StorageManager;
use rustdb_storage::{ColumnDefinition as StorageColumn, DataTable};
use rustdb_types::{LogicalType, Value};

use crate::{
    Catalog, CatalogEntry, CatalogType, EntryKind, IndexEntry, MacroEntry, SequenceEntry, TableEntry,
    Transaction, TypeEntry, ViewEntry,
};

/// Builds the ART of an index whose keys are expressions (they need the binder).
pub trait IndexExpressionLoader {
    /// Builds and adds the index described by `info` to `table`.
    fn load_expression_index(&self, table: &Arc<DataTable>, info: &CreateIndexInfo) -> Result<()>;
}

pub(crate) fn base_info(catalog: &str, schema: &str, temporary: bool, kind: CreateInfoKind) -> CreateInfo {
    CreateInfo {
        catalog: catalog.to_owned(),
        schema: schema.to_owned(),
        temporary,
        internal: false,
        on_conflict: OnCreateConflict::ErrorOnConflict,
        sql: String::new(),
        comment: Value::default(),
        tags: CaseInsensitiveMap::default(),
        dependencies: LogicalDependencyList::default(),
        kind,
    }
}

/// The `CreateInfo` of schema `schema` (`SchemaCatalogEntry::GetInfo`).
pub fn schema_create_info(catalog: &str, schema: &str) -> CreateInfo {
    base_info(catalog, schema, false, CreateInfoKind::Schema)
}

/// `TableCatalogEntry::GetInfo`.
pub(crate) fn table_create_info(
    catalog: &str,
    schema: &str,
    entry: &CatalogEntry,
    table: &TableEntry,
) -> CreateInfo {
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
                compression_type: CompressionType::CompressionAuto,
                comment: Value::default(),
                tags: OrderedMap::default(),
            }
        })
        .collect();
    base_info(
        catalog,
        schema,
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
    catalog: &str,
    schema: &str,
    entry: &CatalogEntry,
    view: &ViewEntry,
) -> CreateInfo {
    base_info(
        catalog,
        schema,
        entry.temporary,
        CreateInfoKind::View(Box::new(CreateViewInfo {
            view_name: entry.name().to_owned(),
            aliases: view.aliases.clone(),
            types: view.types.clone(),
            query: Box::new(view.query.clone()),
            names: view.names.clone(),
            column_comments: OrderedMap::default(),
        })),
    )
}

/// `SequenceCatalogEntry::GetInfo`: the counter is stored as the start value.
pub(crate) fn sequence_create_info(
    catalog: &str,
    schema: &str,
    entry: &CatalogEntry,
    sequence: &SequenceEntry,
) -> CreateInfo {
    let data = sequence.data();
    base_info(
        catalog,
        schema,
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
    catalog: &str,
    schema: &str,
    entry: &CatalogEntry,
    ty: &TypeEntry,
) -> CreateInfo {
    base_info(
        catalog,
        schema,
        entry.temporary,
        CreateInfoKind::Type(CreateTypeInfo {
            name: entry.name().to_owned(),
            logical_type: ty.user_type.clone(),
            query: None,
        }),
    )
}

/// `MacroCatalogEntry::GetInfo` (the entry's temporary flag is not part of it).
pub(crate) fn macro_create_info(catalog: &str, schema: &str, entry: &CatalogEntry) -> Option<CreateInfo> {
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
    Some(base_info(catalog, schema, false, kind))
}

/// The catalog entry a `CREATE TYPE` or `CREATE MACRO` info describes (DuckDB's
/// `TypeCatalogEntry` / `MacroCatalogEntry` constructors).
pub fn entry_from_create_info(info: &CreateInfo) -> Result<CatalogEntry> {
    let mut entry = match &info.kind {
        CreateInfoKind::Type(ty) => CatalogEntry::new(
            ty.name.clone(),
            EntryKind::Type(TypeEntry {
                user_type: ty.logical_type.clone(),
            }),
        ),
        CreateInfoKind::Macro(function) => CatalogEntry::new(
            function.name.clone(),
            EntryKind::Macro(MacroEntry {
                macros: function.macros.clone(),
            }),
        ),
        CreateInfoKind::TableMacro(function) => CatalogEntry::new(
            function.name.clone(),
            EntryKind::TableMacro(MacroEntry {
                macros: function.macros.clone(),
            }),
        ),
        _ => return Err(Error::internal("expected a type or macro")),
    };
    entry.temporary = info.temporary;
    entry.internal = info.internal;
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

/// Writes a checkpoint: the catalog visible to `transaction` and all table data
/// (`SingleFileCheckpointWriter::CreateCheckpoint`).
///
/// `on_start` is called with the root the checkpoint will have before anything is
/// written; the WAL records it (`WALStartCheckpoint`).
pub fn write_checkpoint(
    catalog: &Catalog,
    storage: &StorageManager,
    transaction: &Transaction,
    on_start: &mut dyn FnMut(MetaBlockPointer) -> Result<()>,
    replace_in_memory: bool,
) -> Result<()> {
    if storage.is_read_only() {
        return Ok(());
    }
    let metadata = storage.metadata();
    let options = storage.serialization_options();
    let mut writer = MetadataWriter::new(metadata);
    let mut table_writer = MetadataWriter::new(metadata);
    let meta_block = writer.meta_block_pointer()?;
    on_start(meta_block)?;
    let mut blocks = BlockWriter::new(Arc::clone(storage.block_manager()));
    let catalog_name = catalog.name().to_owned();
    let mut persisted = Vec::new();

    // `GetCatalogEntries`.
    let mut entries: Vec<(String, Arc<CatalogEntry>)> = Vec::new();
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
                    let info = base_info(&catalog_name, entry.name(), false, CreateInfoKind::Schema);
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
                    let data = write_table_data(
                        storage_table,
                        metadata,
                        &mut table_writer,
                        &mut blocks,
                        &options,
                        u64::MAX,
                    )?;
                    serializer.write_object_property(101, "table_pointer", |s| {
                        serialize_meta_block_pointer(s, data.table_pointer)
                    })?;
                    serializer.write_property(102, "total_rows", &data.total_rows)?;
                    persisted.push((Arc::clone(storage_table), data.persisted));
                    let index_infos = serialize_indexes(storage_table, storage, &options, &mut blocks)?;
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
                    let info = base_info(
                        &catalog_name,
                        schema,
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
    block_manager.write_header(metadata, header, &blocks.used_blocks)?;
    block_manager.truncate()?;
    if replace_in_memory {
        for (table, row_groups) in persisted {
            rustdb_storage::checkpoint::table_writer::replace_with_checkpointed(&table, row_groups)?;
        }
    }
    Ok(())
}

/// Writes the data of every index of `table` (`TableIndexList::SerializeToDisk`). Each
/// index fills its own partial blocks.
fn serialize_indexes(
    table: &DataTable,
    storage: &StorageManager,
    options: &rustdb_serde::SerializationOptions,
    blocks: &mut BlockWriter,
) -> Result<Vec<rustdb_storage::index::art::ArtStorageInfo>> {
    let mut index_options = CaseInsensitiveMap::default();
    if options.compatibility.serialization_version >= 3 {
        index_options.insert("v1_0_0_storage", Value::boolean(false));
    }
    let storage_version = storage.block_manager().storage_version();
    let mut infos = Vec::new();
    for entry in table.info().indexes().entries() {
        let mut index_blocks = BlockWriter::new(Arc::clone(storage.block_manager()));
        let mut state = entry.lock();
        let info = state
            .index
            .serialize_to_disk(&index_options, storage_version, &mut |bytes| {
                let pointer = index_blocks.write_segment(bytes)?;
                Ok(rustdb_storage::index::fixed_size_allocator::BufferPointer {
                    block_id: pointer.block_id,
                    offset: pointer.offset,
                })
            })?;
        index_blocks.flush()?;
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
        return Err(Error::new(
            rustdb_common::ErrorKind::Io,
            format!("corrupt database file - {tag} entry without create info"),
        ));
    }
    let info: CreateInfo = deserializer.read_object()?;
    deserializer.on_nullable_end()?;
    deserializer.on_property_end()?;
    if info.catalog_type() != expected {
        return Err(Error::new(
            rustdb_common::ErrorKind::Io,
            format!("corrupt database file - catalog entry type mismatch for {tag}"),
        ));
    }
    Ok(info)
}

/// Loads the catalog of the last checkpoint (`SingleFileCheckpointReader::LoadFromStorage`).
/// Entries are created as committed system entries.
pub fn load_checkpoint(
    catalog: &Catalog,
    storage: &StorageManager,
    index_loader: &dyn IndexExpressionLoader,
) -> Result<()> {
    let meta_block = storage.meta_block();
    if !meta_block.is_valid() {
        return Ok(());
    }
    let metadata = storage.metadata();
    let transaction = Transaction::system();
    let mut data_blocks: Vec<BlockId> = Vec::new();
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
                catalog.create_entry_committed(&info.schema, entry_from_create_info(&info)?)?;
            }
            AstCatalogType::SequenceEntry => {
                let info = read_create_info(d, ty, "sequence")?;
                let CreateInfoKind::Sequence(sequence) = &info.kind else {
                    return Err(Error::internal("expected a sequence"));
                };
                let mut entry = CatalogEntry::new(
                    sequence.name.clone(),
                    EntryKind::Sequence(SequenceEntry::new(sequence)),
                );
                entry.temporary = info.temporary;
                catalog.create_entry_committed(&info.schema, entry)?;
            }
            AstCatalogType::TableEntry => {
                let info = read_create_info(d, ty, "table")?;
                let table_pointer: MetaBlockPointer =
                    d.read_object_property(101, "table_pointer", deserialize_meta_block_pointer)?;
                let total_rows: u64 = d.read_property(102, "total_rows")?;
                let index_pointers: Option<Vec<rustdb_storage::checkpoint::pointers::BlockPointer>> =
                    read_block_pointer_list(d, 103, "index_pointers")?;
                if index_pointers.is_some_and(|pointers| !pointers.is_empty()) {
                    return Err(Error::not_implemented(
                        "reading indexes of database files written by old DuckDB versions",
                    ));
                }
                if d.on_optional_property_begin(104, "index_storage_infos")? {
                    // The indexes are rebuilt from the table below; their stored data
                    // only keeps its blocks in use until the next checkpoint.
                    let stored = d.on_list_begin()?;
                    for _ in 0..stored {
                        d.on_object_begin()?;
                        let info =
                            rustdb_storage::checkpoint::pointers::IndexStorageInfo::deserialize_fields(d)?;
                        d.on_object_end()?;
                        data_blocks.extend(info.block_ids());
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
                    })
                    .collect();
                let types: Vec<LogicalType> = columns.iter().map(|c| c.ty.clone()).collect();
                let data = read_table_data(metadata, table_pointer, &types, total_rows)?;
                let collection = load_row_groups(storage.io(), metadata, &types, data, &mut data_blocks)?;
                let table = DataTable::from_collection(
                    Arc::clone(storage.io()),
                    info.schema.clone(),
                    create.table.clone(),
                    columns,
                    collection,
                );
                for (index, constraint) in entry.constraints.iter().enumerate() {
                    match constraint {
                        Constraint::Unique(unique) => {
                            let kind = if unique.is_primary_key {
                                IndexConstraintType::Primary
                            } else {
                                IndexConstraintType::Unique
                            };
                            table.load_constraint_index(kind, &unique_keys(&entry, unique)?, index)?;
                        }
                        Constraint::ForeignKey(fk)
                            if matches!(
                                fk.fk_type,
                                ForeignKeyType::FkTypeForeignKeyTable
                                    | ForeignKeyType::FkTypeSelfReferenceTable
                            ) =>
                        {
                            let keys: Vec<usize> = fk.fk_keys.iter().map(|k| k.0 as usize).collect();
                            table.load_constraint_index(IndexConstraintType::Foreign, &keys, index)?;
                        }
                        _ => {}
                    }
                }
                entry.storage = Some(table);
                let mut catalog_entry = CatalogEntry::new(create.table.clone(), EntryKind::Table(entry));
                catalog_entry.temporary = info.temporary;
                catalog.create_entry_committed(&info.schema, catalog_entry)?;
            }
            AstCatalogType::ViewEntry => {
                let info = read_create_info(d, ty, "view")?;
                let CreateInfoKind::View(view) = &info.kind else {
                    return Err(Error::internal("expected a view"));
                };
                let mut entry = CatalogEntry::new(
                    view.view_name.clone(),
                    EntryKind::View(ViewEntry {
                        query: (*view.query).clone(),
                        aliases: view.aliases.clone(),
                        types: view.types.clone(),
                        names: view.names.clone(),
                    }),
                );
                entry.temporary = info.temporary;
                catalog.create_entry_committed(&info.schema, entry)?;
            }
            AstCatalogType::IndexEntry => {
                let info = read_create_info(d, ty, "index")?;
                let root: Option<Vec<u8>> = if d.on_optional_property_begin(101, "root_block_pointer")? {
                    d.on_object_begin()?;
                    let pointer = rustdb_storage::checkpoint::pointers::BlockPointer::deserialize_fields(d)?;
                    d.on_object_end()?;
                    d.on_optional_property_end(true)?;
                    pointer.is_valid().then(Vec::new)
                } else {
                    None
                };
                if root.is_some() {
                    return Err(Error::not_implemented(
                        "reading indexes of database files written by old DuckDB versions",
                    ));
                }
                load_index(catalog, &transaction, &info, index_loader)?;
            }
            other => {
                let _ = other;
                return Err(Error::new(
                    rustdb_common::ErrorKind::Io,
                    "corrupt database file - unrecognized catalog type in checkpoint",
                ));
            }
        }
        d.on_object_end()?;
    }
    d.on_list_end()?;
    d.on_property_end()?;
    deserializer.end()?;
    let blocks: BTreeSet<BlockId> = data_blocks.into_iter().collect();
    storage.block_manager().set_checkpoint_blocks(blocks);
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

/// Creates an index entry read from a checkpoint and builds its ART from the table's rows.
fn load_index(
    catalog: &Catalog,
    transaction: &Transaction,
    info: &CreateInfo,
    index_loader: &dyn IndexExpressionLoader,
) -> Result<()> {
    let (entry, _) = build_index_entry(catalog, transaction, info, index_loader)?;
    catalog.create_entry_committed(&info.schema, entry)
}

/// Builds the ART of the index `info` from its table's rows, adds it to the table and
/// returns the index's catalog entry and the table.
fn build_index_entry(
    catalog: &Catalog,
    transaction: &Transaction,
    info: &CreateInfo,
    index_loader: &dyn IndexExpressionLoader,
) -> Result<(CatalogEntry, Arc<DataTable>)> {
    let CreateInfoKind::Index(index) = &info.kind else {
        return Err(Error::internal("expected an index"));
    };
    let table_entry = catalog
        .get_entry(transaction, CatalogType::Table, &info.schema, &index.table)
        .ok_or_else(|| {
            Error::new(
                rustdb_common::ErrorKind::Io,
                "corrupt database file - index entry without table entry",
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
            storage.build_index(transaction, &mut art, false)?;
            storage.add_index(art)?;
        }
        _ => index_loader.load_expression_index(&storage, index)?,
    }
    let mut entry = CatalogEntry::new(
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

/// Recreates an index of a WAL in `transaction` (`ReplayCreateIndex`): the index is
/// rebuilt from its table.
pub fn replay_create_index(
    catalog: &Catalog,
    transaction: &Transaction,
    info: &CreateInfo,
    index_loader: &dyn IndexExpressionLoader,
) -> Result<()> {
    let (entry, storage) = build_index_entry(catalog, transaction, info, index_loader)?;
    let name = entry.name().to_owned();
    match catalog.create_entry(
        transaction,
        &info.schema,
        entry,
        OnCreateConflict::ErrorOnConflict,
    ) {
        Ok(_) => Ok(()),
        Err(error) => {
            storage.indexes().remove(&name);
            Err(error)
        }
    }
}
