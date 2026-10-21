//! Replaying the write-ahead log when a database file is opened (DuckDB's
//! `WriteAheadLogReplayer` and `WriteAheadLogDeserializer`).
//!
//! The WAL is read twice. The first pass only looks at the entries: it finds the end of
//! the last complete commit and a checkpoint marker, if a checkpoint was interrupted. If
//! that checkpoint completed, the WAL is obsolete. Otherwise the second pass applies the
//! entries, committing at every `WAL_FLUSH`; a torn tail (a serialization error) ends the
//! replay and is cut off before the next write.

use std::sync::Arc;

use rustdb_ast::{
    AlterInfo, AlterInfoKind, AlterTableInfo, CatalogType as AstCatalogType, CreateInfo, DropInfo,
    OnEntryNotFound, SqlStatement, SqlStatementKind,
};
use rustdb_catalog::checkpoint::{read_create_info_property, replay_create_index};
use rustdb_catalog::{Catalog, CatalogType, EntryKind, Transaction};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_execution::operators::{
    CatalogTableCreator, CreateEntryAction, CreateSchemaAction, CreateSequenceAction, CreateViewAction,
    DdlAction, DropAction, TableCreator,
};
use rustdb_serde::Deserializer;
use rustdb_storage::DataTable;
use rustdb_storage::checkpoint::pointers::{deserialize_index_storage_info, deserialize_meta_block_pointer};
use rustdb_storage::checkpoint::vector_serde::deserialize_chunk;
use rustdb_storage::index::UnboundIndexStorage;
use rustdb_storage::metadata::MetaBlockPointer;
use rustdb_storage::storage_manager::StorageManager;
use rustdb_storage::wal::{WAL_VERSION_NUMBER, WalReader, WalType, checkpoint_wal_path};
use rustdb_types::DataChunk;

use crate::attached::AttachedDatabase;
use crate::{Connection, Database};

fn io_error(message: impl Into<std::borrow::Cow<'static, str>>) -> Error {
    Error::new(ErrorKind::Io, message)
}

fn remove_file(path: &str) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(format!("Could not remove file \"{path}\": {error}"))),
    }
}

/// `WriteAheadLogDeserializer::ThrowVersionError`.
fn version_error(checkpoint_iteration: u64, expected: u64) -> Error {
    let relation = if checkpoint_iteration < expected {
        "an older"
    } else {
        "a newer"
    };
    io_error(format!(
        "This WAL was created for this database file, but the WAL checkpoint iteration does not match the \
         database file. That means the WAL was created for {relation} version of this database. File checkpoint \
         iteration: {expected}, WAL checkpoint iteration: {checkpoint_iteration}"
    ))
}

/// Replays the WAL of the database's file, if any, and sets up the WAL that commits are
/// logged to from now on (`WriteAheadLog::Replay`).
pub(crate) fn replay(database: &Database, attached: &Arc<AttachedDatabase>) -> Result<()> {
    let Some(storage) = attached.storage.file().cloned() else {
        return Ok(());
    };
    // `SingleFileStorageManager::LoadDatabase` times the replay.
    let _timer =
        rustdb_common::profiling::start_timer(rustdb_common::profiling::MetricType::AttachReplayWalLatency);
    let (size, requires_truncate) = match WalReader::open(&storage.wal_path())? {
        None => (0, false),
        Some(reader) => replay_log(database, attached, &storage, reader)?,
    };
    let wal = storage.init_wal(size, requires_truncate)?;
    if !storage.is_read_only() {
        attached.catalog.set_wal(wal)?;
    }
    Ok(())
}

/// What the first pass found.
#[derive(Debug, Default)]
struct ScanState {
    /// The root of an interrupted checkpoint, with where its marker ends.
    checkpoint: Option<(MetaBlockPointer, u64)>,
    /// The end of the last complete commit before the checkpoint marker.
    checkpoint_truncate_offset: u64,
    /// The start of the last entry read.
    current_position: u64,
    /// The WAL belongs to the previous checkpoint iteration: a checkpoint must have
    /// completed (`expected_checkpoint_id`).
    expected_checkpoint: Option<u64>,
}

/// Checks the header against the database file (`ReplayVersion`).
fn check_header(reader: &mut WalReader, storage: &StorageManager, state: &mut ScanState) -> Result<()> {
    let header = reader.read_header()?;
    if !matches!(header.version, 1 | WAL_VERSION_NUMBER) {
        return Err(io_error(format!(
            "Failed to read WAL of version {} - can only read version 1, 2 and 3 (encrypted)",
            header.version
        )));
    }
    let (Some(identifier), Some(iteration)) = (header.db_identifier, header.checkpoint_iteration) else {
        return Ok(());
    };
    let block_manager = storage.block_manager();
    if identifier != block_manager.db_identifier() {
        return Err(io_error("WAL does not match database file."));
    }
    let expected = block_manager.checkpoint_iteration();
    if expected != iteration {
        if iteration + 1 == expected {
            // The WAL was checkpointed, but not removed before the process stopped.
            state.expected_checkpoint = Some(expected);
            return Ok(());
        }
        return Err(version_error(iteration, expected));
    }
    Ok(())
}

/// Opens an entry and reads its type.
fn entry_type(d: &mut dyn Deserializer) -> Result<WalType> {
    d.on_object_begin()?;
    let value: u8 = d.read_property(100, "wal_type")?;
    WalType::from_u8(value).ok_or_else(|| Error::internal("Invalid WAL entry type!"))
}

/// What the first pass needs of an entry.
enum ScannedEntry {
    Flush,
    /// A checkpoint marker with the root it announces.
    Checkpoint(MetaBlockPointer),
    Other,
}

/// Reads an entry for the first pass. A framed entry is read only up to what the pass
/// needs (its frame skips the rest); an unframed one (`whole`, WAL version 1) is read to
/// its end.
fn scan_entry(d: &mut dyn Deserializer, whole: bool) -> Result<ScannedEntry> {
    if whole {
        return Ok(match parse_entry(d, "")? {
            WalEntry::Flush => ScannedEntry::Flush,
            WalEntry::Checkpoint(meta_block) => ScannedEntry::Checkpoint(meta_block),
            _ => ScannedEntry::Other,
        });
    }
    Ok(match entry_type(d)? {
        WalType::WalFlush => ScannedEntry::Flush,
        WalType::Checkpoint => ScannedEntry::Checkpoint(d.read_object_property(
            101,
            "meta_block",
            deserialize_meta_block_pointer,
        )?),
        _ => ScannedEntry::Other,
    })
}

/// The first pass (`ReplayLog` with `deserialize_only`).
fn scan_log(reader: &mut WalReader, storage: &StorageManager) -> Result<ScanState> {
    let mut state = ScanState::default();
    let mut last_flush_end = 0;
    // DuckDB's loop also counts the version entry `check_header` reads.
    let mut replay_entry_count = 1;
    let outcome = (|| -> Result<()> {
        check_header(reader, storage, &mut state)?;
        loop {
            replay_entry_count += 1;
            state.current_position = reader.offset();
            let whole = reader.version() == 1;
            match reader.read_entry(|d| scan_entry(d, whole))? {
                ScannedEntry::Checkpoint(meta_block) => {
                    if state.checkpoint.is_some() {
                        return Err(io_error("WAL cannot contain more than one checkpoint marker"));
                    }
                    state.checkpoint = Some((meta_block, reader.offset()));
                    state.checkpoint_truncate_offset = last_flush_end;
                }
                ScannedEntry::Flush => {
                    last_flush_end = reader.offset();
                    if reader.finished() {
                        rustdb_common::profiling::add_to_counter(
                            rustdb_common::profiling::MetricType::WalReplayEntryCount,
                            replay_entry_count,
                        );
                        return Ok(());
                    }
                }
                ScannedEntry::Other => {}
            }
        }
    })();
    match outcome {
        Ok(()) => Ok(state),
        // A torn tail: everything before it is replayed.
        Err(error) if error.kind() == ErrorKind::Serialization => Ok(state),
        Err(error) => Err(error),
    }
}

/// Replays the WAL read by `reader`; returns the size of the WAL to keep and whether the
/// rest must be cut off (`WriteAheadLogReplayer::ReplayLog`).
fn replay_log(
    database: &Database,
    attached: &AttachedDatabase,
    storage: &Arc<StorageManager>,
    mut reader: WalReader,
) -> Result<(u64, bool)> {
    let path = reader.path().to_owned();
    let read_only = storage.is_read_only();
    if reader.finished() {
        // An empty WAL.
        if !read_only {
            remove_file(&path)?;
        }
        return Ok((0, false));
    }
    let prefix = |error: Error| {
        Error::new(
            error.kind(),
            format!(
                "Failure while replaying WAL file \"{path}\": {}",
                error.raw_message()
            ),
        )
    };
    let scan = scan_log(&mut reader, storage).map_err(prefix)?;
    let mut truncate_failed_checkpoint = false;
    if let Some((meta_block, checkpoint_end)) = scan.checkpoint {
        if scan.current_position != checkpoint_end {
            return Err(io_error("WAL checkpoint marker must be at the end of the WAL"));
        }
        if std::path::Path::new(&checkpoint_wal_path(storage.path())).exists() {
            return Err(Error::not_implemented(
                "recovering the WAL of a checkpoint that ran concurrently with commits",
            ));
        }
        // `IsCheckpointClean`: the header points at the checkpoint the marker announced.
        if storage.meta_block().block_pointer == meta_block.block_pointer {
            if !read_only {
                remove_file(&path)?;
            }
            return Ok((0, false));
        }
        truncate_failed_checkpoint = !read_only;
    }
    if let Some(expected) = scan.expected_checkpoint {
        return Err(version_error(expected - 1, expected));
    }
    if truncate_failed_checkpoint {
        // The marker of the failed checkpoint ends the WAL; cut it off.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|error| io_error(format!("Cannot open file \"{path}\": {error}")))?;
        file.set_len(scan.checkpoint_truncate_offset)
            .and_then(|()| file.sync_all())
            .map_err(|error| io_error(format!("Could not truncate file \"{path}\": {error}")))?;
        if scan.checkpoint_truncate_offset == 0 {
            return Ok((0, false));
        }
        reader = WalReader::open(&path)?.ok_or_else(|| io_error(format!("Cannot open file \"{path}\"")))?;
    }
    reader.reset()?;
    let mut connection = database.connect()?;
    let mut replayer = Replayer {
        catalog: Arc::clone(&attached.catalog),
        storage: Arc::clone(storage),
        current_table: None,
    };
    let mut successful_offset = 0;
    let mut all_succeeded = false;
    connection.query("BEGIN TRANSACTION")?;
    let outcome = (|| -> Result<()> {
        reader.read_header()?;
        loop {
            if reader.read_entry(|d| replayer.replay_entry(&mut connection, d))? {
                connection.query("COMMIT")?;
                successful_offset = reader.offset();
                if reader.finished() {
                    all_succeeded = true;
                    return Ok(());
                }
                connection.query("BEGIN TRANSACTION")?;
            }
        }
    })();
    if let Err(error) = outcome {
        let _ = connection.query("ROLLBACK");
        // A serialization error is a torn tail (`abort_on_wal_failure` is off).
        if error.kind() != ErrorKind::Serialization {
            return Err(prefix(error));
        }
    }
    Ok((successful_offset, !all_succeeded))
}

/// A WAL entry as read, before it is applied (the reading half of DuckDB's `Replay*`
/// functions, which their `deserialize_only` mode runs alone).
enum WalEntry {
    Flush,
    CreateTable(CreateInfo),
    CreateView(CreateInfo),
    CreateSequence(CreateInfo),
    CreateSchema(String),
    CreateIndex(CreateInfo, UnboundIndexStorage),
    /// Macros, table macros and types.
    CreateEntry(CreateInfo),
    Drop {
        catalog_type: AstCatalogType,
        schema: String,
        name: String,
    },
    Alter(Box<AlterInfo>),
    SequenceValue {
        schema: String,
        name: String,
        usage_count: u64,
        counter: i64,
    },
    UseTable {
        schema: String,
        name: String,
    },
    Insert(DataChunk),
    Delete(DataChunk),
    Update {
        column_path: Vec<u64>,
        chunk: DataChunk,
    },
    Checkpoint(MetaBlockPointer),
}

/// Reads one entry; `CREATE` entries go to the catalog `catalog` whatever the catalog
/// name they were logged with.
fn parse_entry(d: &mut dyn Deserializer, catalog: &str) -> Result<WalEntry> {
    let wal_type = entry_type(d)?;
    let create_info = |d: &mut dyn Deserializer, expected: AstCatalogType, tag: &'static str| {
        let mut info = read_create_info_property(d, 101, expected, tag)?;
        catalog.clone_into(&mut info.catalog);
        Ok::<_, Error>(info)
    };
    let entry = match wal_type {
        WalType::WalFlush => WalEntry::Flush,
        WalType::CreateTable => WalEntry::CreateTable(create_info(d, AstCatalogType::TableEntry, "table")?),
        WalType::CreateView => WalEntry::CreateView(create_info(d, AstCatalogType::ViewEntry, "view")?),
        WalType::CreateSequence => {
            WalEntry::CreateSequence(create_info(d, AstCatalogType::SequenceEntry, "sequence")?)
        }
        WalType::CreateSchema => WalEntry::CreateSchema(d.read_property(101, "schema")?),
        WalType::CreateIndex => {
            let info = create_info(d, AstCatalogType::IndexEntry, "index_catalog_entry")?;
            WalEntry::CreateIndex(info, read_index_storage(d)?)
        }
        // `ReplayCreateMacro`, `ReplayCreateTableMacro` and `ReplayCreateType`.
        WalType::CreateMacro => WalEntry::CreateEntry(create_info(d, AstCatalogType::MacroEntry, "macro")?),
        WalType::CreateTableMacro => {
            WalEntry::CreateEntry(create_info(d, AstCatalogType::TableMacroEntry, "table")?)
        }
        WalType::CreateType => WalEntry::CreateEntry(create_info(d, AstCatalogType::TypeEntry, "type")?),
        WalType::DropTable
        | WalType::DropView
        | WalType::DropSequence
        | WalType::DropIndex
        | WalType::DropMacro
        | WalType::DropTableMacro
        | WalType::DropType => {
            let schema: String = d.read_property(101, "schema")?;
            let name: String = d.read_property(102, "name")?;
            let catalog_type = match wal_type {
                WalType::DropTable => AstCatalogType::TableEntry,
                WalType::DropView => AstCatalogType::ViewEntry,
                WalType::DropSequence => AstCatalogType::SequenceEntry,
                WalType::DropIndex => AstCatalogType::IndexEntry,
                WalType::DropMacro => AstCatalogType::MacroEntry,
                WalType::DropTableMacro => AstCatalogType::TableMacroEntry,
                _ => AstCatalogType::TypeEntry,
            };
            WalEntry::Drop {
                catalog_type,
                schema,
                name,
            }
        }
        WalType::DropSchema => WalEntry::Drop {
            catalog_type: AstCatalogType::SchemaEntry,
            schema: String::new(),
            name: d.read_property(101, "schema")?,
        },
        WalType::AlterInfo => {
            d.on_property_begin(101, "info")?;
            if !d.on_nullable_begin()? {
                return Err(Error::serialization("ALTER entry without info"));
            }
            let info: rustdb_ast::ParseInfo = d.read_object()?;
            d.on_nullable_end()?;
            d.on_property_end()?;
            let rustdb_ast::ParseInfo::Alter(alter) = info else {
                return Err(Error::serialization("ALTER entry with another statement"));
            };
            if is_add_primary_key(&alter) {
                skip_index_storage(d)?;
            }
            WalEntry::Alter(alter)
        }
        WalType::SequenceValue => WalEntry::SequenceValue {
            schema: d.read_property(101, "schema")?,
            name: d.read_property(102, "name")?,
            usage_count: d.read_property(103, "usage_count")?,
            counter: d.read_property(104, "counter")?,
        },
        WalType::UseTable => WalEntry::UseTable {
            schema: d.read_property(101, "schema")?,
            name: d.read_property(102, "table")?,
        },
        WalType::InsertTuple => WalEntry::Insert(d.read_object_property(101, "chunk", deserialize_chunk)?),
        WalType::DeleteTuple => WalEntry::Delete(d.read_object_property(101, "chunk", deserialize_chunk)?),
        WalType::UpdateTuple => WalEntry::Update {
            column_path: d.read_property(101, "column_indexes")?,
            chunk: d.read_object_property(102, "chunk", deserialize_chunk)?,
        },
        WalType::Checkpoint => {
            WalEntry::Checkpoint(d.read_object_property(101, "meta_block", deserialize_meta_block_pointer)?)
        }
        WalType::RowGroupData => {
            return Err(Error::not_implemented("replaying ROW_GROUP_DATA WAL entries"));
        }
        WalType::WalVersion => return Err(Error::internal("Invalid WAL entry type!")),
    };
    d.on_object_end()?;
    Ok(entry)
}

/// The state of the second pass (`ReplayState`).
struct Replayer {
    catalog: Arc<Catalog>,
    storage: Arc<StorageManager>,
    /// The table of the last `USE_TABLE`.
    current_table: Option<Arc<DataTable>>,
}

impl Replayer {
    /// Applies one entry; returns whether it was a `WAL_FLUSH` (`ReplayEntry`).
    fn replay_entry(&mut self, connection: &mut Connection, d: &mut dyn Deserializer) -> Result<bool> {
        let entry = parse_entry(d, self.catalog.name())?;
        self.apply(connection, entry)
    }

    /// Applies a parsed entry; returns whether it was a `WAL_FLUSH`.
    fn apply(&mut self, connection: &mut Connection, entry: WalEntry) -> Result<bool> {
        let transaction = connection.current_transaction();
        match entry {
            WalEntry::Flush => return Ok(true),
            WalEntry::CreateTable(info) => {
                let creator = CatalogTableCreator {
                    catalog: Arc::clone(&self.catalog),
                    schema: info.schema.clone(),
                    info,
                    io: Arc::clone(self.storage.io()),
                };
                creator.create(&transaction)?;
            }
            WalEntry::CreateView(info) => {
                CreateViewAction {
                    catalog: Arc::clone(&self.catalog),
                    info,
                }
                .execute(&transaction)?;
            }
            WalEntry::CreateSequence(info) => {
                CreateSequenceAction {
                    catalog: Arc::clone(&self.catalog),
                    info,
                }
                .execute(&transaction)?;
            }
            WalEntry::CreateSchema(schema) => {
                let mut info = rustdb_catalog::checkpoint::schema_create_info(self.catalog.name(), &schema);
                info.on_conflict = rustdb_ast::OnCreateConflict::ErrorOnConflict;
                CreateSchemaAction {
                    catalog: Arc::clone(&self.catalog),
                    info,
                }
                .execute(&transaction)?;
            }
            WalEntry::CreateIndex(info, stored) => {
                replay_create_index(&self.catalog, &transaction, &info, stored)?;
            }
            WalEntry::CreateEntry(info) => {
                CreateEntryAction {
                    catalog: Arc::clone(&self.catalog),
                    info,
                }
                .execute(&transaction)?;
            }
            WalEntry::Drop {
                catalog_type,
                schema,
                name,
            } => {
                if catalog_type == AstCatalogType::TableEntry
                    && self
                        .current_table
                        .as_ref()
                        .is_some_and(|table| table.info().table_name().eq_ignore_ascii_case(&name))
                {
                    self.current_table = None;
                }
                self.drop(&transaction, catalog_type, schema, name)?;
            }
            WalEntry::Alter(mut alter) => self.alter(connection, &mut alter)?,
            WalEntry::SequenceValue {
                schema,
                name,
                usage_count,
                counter,
            } => {
                let entry = self
                    .catalog
                    .get_entry(&transaction, CatalogType::Sequence, &schema, &name)
                    .ok_or_else(|| Error::catalog(format!("Sequence with name {name} does not exist!")))?;
                let EntryKind::Sequence(sequence) = &entry.kind else {
                    return Err(Error::internal("expected a sequence"));
                };
                sequence.replay_value(usage_count, counter);
            }
            WalEntry::UseTable { schema, name } => {
                let entry = self
                    .catalog
                    .get_entry(&transaction, CatalogType::Table, &schema, &name)
                    .ok_or_else(|| Error::catalog(format!("Table with name {name} does not exist!")))?;
                let EntryKind::Table(table) = &entry.kind else {
                    return Err(Error::catalog(format!("{name} is not a table")));
                };
                self.current_table.clone_from(&table.storage);
            }
            WalEntry::Insert(chunk) => {
                let table = self.current_table("insert")?;
                // Appended without verifying constraints (`LocalWALAppend`).
                table.append_chunk(&transaction, &chunk, rustdb_storage::TableConstraints::default())?;
            }
            WalEntry::Delete(chunk) => {
                let table = self.current_table("delete")?;
                let ids = chunk.column(0).flattened(chunk.len());
                let row_ids = &ids.values::<i64>()[..chunk.len()];
                let total_rows = table.total_rows();
                if row_ids.iter().any(|&id| id < 0 || id as u64 >= total_rows) {
                    return Err(Error::serialization("invalid row ID delete in WAL"));
                }
                table.delete(&transaction, row_ids)?;
            }
            WalEntry::Update { column_path, chunk } => {
                let table = self.current_table("update")?;
                if column_path
                    .first()
                    .is_none_or(|&column| column as usize >= table.columns().len())
                {
                    return Err(Error::internal(
                        "Corrupt WAL: column index for update out of bounds",
                    ));
                }
                let mut columns = chunk.columns().to_vec();
                let ids = columns
                    .pop()
                    .ok_or_else(|| Error::internal("Corrupt WAL: update without row ids"))?
                    .flattened(chunk.len());
                let values = columns
                    .pop()
                    .ok_or_else(|| Error::internal("Corrupt WAL: update without values"))?;
                let path: Vec<usize> = column_path.iter().map(|&index| index as usize).collect();
                table.update_column(&transaction, &ids.values::<i64>()[..chunk.len()], &path, &values)?;
            }
            WalEntry::Checkpoint(_) => {}
        }
        Ok(false)
    }

    fn current_table(&self, action: &str) -> Result<&Arc<DataTable>> {
        self.current_table
            .as_ref()
            .ok_or_else(|| Error::internal(format!("Corrupt WAL: {action} without table")))
    }

    fn drop(
        &self,
        transaction: &Transaction,
        catalog_type: AstCatalogType,
        schema: String,
        name: String,
    ) -> Result<()> {
        DropAction {
            catalog: Arc::clone(&self.catalog),
            info: DropInfo {
                catalog_type,
                catalog: self.catalog.name().to_owned(),
                schema,
                name,
                if_not_found: OnEntryNotFound::ThrowException,
                cascade: false,
                allow_drop_internal: false,
                extra_drop_info: None,
            },
        }
        .execute(transaction)
    }

    /// Replays an `ALTER` through the binder (`Catalog::Alter`). Foreign key bookkeeping
    /// on the referenced table is skipped: replaying the referencing table's `CREATE` and
    /// `DROP` redoes it.
    fn alter(&self, connection: &mut Connection, info: &mut AlterInfo) -> Result<()> {
        if matches!(
            &info.kind,
            AlterInfoKind::AlterTable(AlterTableInfo::ForeignKeyConstraint(_))
        ) {
            return Ok(());
        }
        self.catalog.name().clone_into(&mut info.catalog);
        let statement = SqlStatement::new(SqlStatementKind::Alter(Box::new(info.clone())));
        connection.execute_statement("", statement)?;
        Ok(())
    }
}

/// Whether `info` is `ADD PRIMARY KEY` (`AlterInfo::IsAddPrimaryKey`).
fn is_add_primary_key(info: &AlterInfo) -> bool {
    matches!(
        &info.kind,
        AlterInfoKind::AlterTable(AlterTableInfo::AddConstraint(add))
            if matches!(add.constraint.as_ref(), rustdb_ast::Constraint::Unique(unique) if unique.is_primary_key)
    )
}

/// Reads the stored index data of an index entry: its description and the bytes of its
/// buffers per allocator (`ReplayIndexData`).
fn read_index_storage(d: &mut dyn Deserializer) -> Result<UnboundIndexStorage> {
    let info = d.read_object_property(102, "index_storage_info", deserialize_index_storage_info)?;
    d.on_property_begin(103, "index_storage")?;
    let count = d.on_list_begin()?;
    let mut buffers = Vec::with_capacity(count);
    for index in 0..count {
        let sizes = info
            .allocator_infos
            .get(index)
            .map(|allocator| allocator.allocation_sizes.as_slice())
            .unwrap_or_default();
        let mut allocator = Vec::with_capacity(sizes.len());
        for &size in sizes {
            let size = usize::try_from(size).map_err(|_| Error::serialization("index buffer too large"))?;
            allocator.push(d.read_data(size)?);
        }
        buffers.push(allocator);
    }
    d.on_list_end()?;
    d.on_property_end()?;
    Ok(UnboundIndexStorage::Wal(info, buffers))
}

/// Reads past the stored index data of an index entry (`ReplayIndexData`); rustdb
/// rebuilds the index from its table instead.
fn skip_index_storage(d: &mut dyn Deserializer) -> Result<()> {
    read_index_storage(d).map(drop)
}
