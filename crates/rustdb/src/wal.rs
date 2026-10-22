//! Replaying the write-ahead log when a database file is opened (DuckDB's
//! `WriteAheadLogReplayer` and `WriteAheadLogDeserializer`).
//!
//! The WAL is read twice. The first pass only looks at the entries: it finds the end of
//! the last complete commit and a checkpoint marker, if a checkpoint was interrupted. If
//! that checkpoint completed, the WAL is obsolete, except for the commits made while it
//! ran, which are in the checkpoint WAL (`.wal.checkpoint`). Otherwise the second pass
//! applies the entries, committing at every `WAL_FLUSH`; a torn tail (a serialization
//! error) ends the replay and is cut off before the next write.

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
use rustdb_storage::checkpoint::pointers::{
    PersistentCollectionData, deserialize_index_storage_info, deserialize_meta_block_pointer,
};
use rustdb_storage::checkpoint::vector_serde::deserialize_chunk;
use rustdb_storage::index::UnboundIndexStorage;
use rustdb_storage::metadata::MetaBlockPointer;
use rustdb_storage::storage_manager::{CheckpointAbort, StorageManager};
use rustdb_storage::wal::{WAL_ENCRYPTED_VERSION_NUMBER, WAL_VERSION_NUMBER, WalReader, WalType};
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

/// Which WAL a replay reads (DuckDB's `WALReplayState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WalFile {
    /// The WAL of the database file.
    Main,
    /// The checkpoint WAL of a checkpoint that ran concurrently with commits, or the WAL
    /// it was moved or merged into: it may belong to the next checkpoint iteration.
    Checkpoint,
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
    let replayed = match WalReader::open_for(&storage.wal_path(), storage.block_manager())? {
        None => None,
        Some(reader) => replay_log(database, attached, &storage, reader, WalFile::Main)?,
    };
    let (size, requires_truncate) = if let Some(kept) = replayed {
        kept
    } else {
        // Nothing in the WAL is needed any more.
        if !storage.is_read_only() {
            remove_file(&storage.wal_path())?;
        }
        (0, false)
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
    /// The root of an interrupted checkpoint, with where its marker starts and ends.
    checkpoint: Option<(MetaBlockPointer, u64, u64)>,
    /// The end of the last complete commit before the checkpoint marker.
    checkpoint_truncate_offset: u64,
    /// The start of the last entry read.
    current_position: u64,
    /// The WAL belongs to the previous checkpoint iteration: a checkpoint must have
    /// completed (`expected_checkpoint_id`).
    expected_checkpoint: Option<u64>,
}

/// Checks the header against the database file (`ReplayVersion`); returns the WAL's
/// version.
fn check_header(
    reader: &mut WalReader,
    storage: &StorageManager,
    file: WalFile,
    state: &mut ScanState,
) -> Result<u64> {
    let header = reader.read_header()?;
    if !matches!(
        header.version,
        1 | WAL_VERSION_NUMBER | WAL_ENCRYPTED_VERSION_NUMBER
    ) {
        return Err(io_error(format!(
            "Failed to read WAL of version {} - can only read version 1, 2 and 3 (encrypted)",
            header.version
        )));
    }
    let (Some(identifier), Some(iteration)) = (header.db_identifier, header.checkpoint_iteration) else {
        return Ok(header.version);
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
            return Ok(header.version);
        }
        // The checkpoint WAL of a checkpoint that did not write its header yet.
        if file == WalFile::Checkpoint && iteration == expected + 1 {
            return Ok(header.version);
        }
        return Err(version_error(iteration, expected));
    }
    Ok(header.version)
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
    /// Row groups an optimistic write stored, whose blocks are in use.
    RowGroupData(PersistentCollectionData),
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
            WalEntry::RowGroupData(data) => ScannedEntry::RowGroupData(data),
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
        WalType::RowGroupData => ScannedEntry::RowGroupData(read_row_group_data(d)?),
        _ => ScannedEntry::Other,
    })
}

/// Reads the row groups of a `ROW_GROUP_DATA` entry.
fn read_row_group_data(d: &mut dyn Deserializer) -> Result<PersistentCollectionData> {
    d.read_object_property(
        101,
        "row_group_data",
        PersistentCollectionData::deserialize_fields,
    )
}

/// The first pass (`ReplayLog` with `deserialize_only`).
fn scan_log(reader: &mut WalReader, storage: &StorageManager, file: WalFile) -> Result<ScanState> {
    let mut state = ScanState::default();
    let mut last_flush_end = 0;
    // DuckDB's loop also counts the version entry `check_header` reads.
    let mut replay_entry_count = 1;
    let outcome = (|| -> Result<()> {
        check_header(reader, storage, file, &mut state)?;
        loop {
            replay_entry_count += 1;
            state.current_position = reader.offset();
            let whole = reader.version() == 1;
            match reader.read_entry(|d| scan_entry(d, whole))? {
                ScannedEntry::Checkpoint(meta_block) => {
                    if state.checkpoint.is_some() {
                        return Err(io_error("WAL cannot contain more than one checkpoint marker"));
                    }
                    state.checkpoint = Some((meta_block, state.current_position, reader.offset()));
                    state.checkpoint_truncate_offset = last_flush_end;
                }
                ScannedEntry::RowGroupData(data) => {
                    // The blocks are in use once the WAL is replayed; mark them now, so
                    // that nothing the replay writes reuses them (`ReplayRowGroupData` in
                    // `deserialize_only` mode).
                    let block_manager = storage.block_manager();
                    for block in data.block_ids() {
                        block_manager.mark_block_as_used(block);
                        block_manager.mark_block_as_checkpointed(block);
                    }
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

/// Opens the WAL at `path` of `storage`'s file, which exists.
fn open_wal(storage: &StorageManager, path: &str) -> Result<WalReader> {
    WalReader::open_for(path, storage.block_manager())?
        .ok_or_else(|| io_error(format!("Cannot open file \"{path}\"")))
}

/// Replays the WAL read by `reader`; returns the size of the WAL to keep and whether the
/// rest must be cut off, or `None` when the WAL is not needed any more
/// (`WriteAheadLogReplayer::ReplayLog`).
///
/// A checkpoint marker shows a checkpoint that did not finish. If commits ran
/// concurrently with it, their entries are in the checkpoint WAL: when the checkpoint
/// wrote its header, only the checkpoint WAL is replayed (it becomes the WAL); otherwise
/// the WAL up to the marker and the checkpoint WAL are merged into the WAL and replayed.
/// A read-only database replays the files as they are.
fn replay_log(
    database: &Database,
    attached: &AttachedDatabase,
    storage: &Arc<StorageManager>,
    mut reader: WalReader,
    file: WalFile,
) -> Result<Option<(u64, bool)>> {
    let path = reader.path().to_owned();
    let read_only = storage.is_read_only();
    if reader.finished() {
        // An empty WAL.
        return Ok(None);
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
    let scan = scan_log(&mut reader, storage, file).map_err(prefix)?;
    let mut truncate_failed_checkpoint = false;
    // The checkpoint WAL a read-only database replays after this one.
    let mut checkpoint_wal = None;
    if let Some((meta_block, checkpoint_position, checkpoint_end)) = scan.checkpoint {
        if file == WalFile::Checkpoint {
            return Err(Error::invalid_input(format!(
                "Failure while replaying checkpoint WAL file \"{path}\": checkpoint WAL cannot contain a \
                 checkpoint marker"
            )));
        }
        if scan.current_position != checkpoint_end {
            return Err(io_error("WAL checkpoint marker must be at the end of the WAL"));
        }
        // `IsCheckpointClean`: the header points at the checkpoint the marker announced.
        let checkpoint_succeeded = storage.meta_block().block_pointer == meta_block.block_pointer;
        let checkpoint_wal_path = storage.checkpoint_wal_path();
        if !std::path::Path::new(&checkpoint_wal_path).exists() {
            if checkpoint_succeeded {
                return Ok(None);
            }
            truncate_failed_checkpoint = !read_only;
        } else if checkpoint_succeeded {
            // Only the commits made while the checkpoint ran are not in the file.
            if read_only {
                return replay_log(
                    database,
                    attached,
                    storage,
                    open_wal(storage, &checkpoint_wal_path)?,
                    WalFile::Checkpoint,
                );
            }
            drop(reader);
            std::fs::rename(&checkpoint_wal_path, &path).map_err(|error| {
                io_error(format!(
                    "Could not move file \"{checkpoint_wal_path}\" to \"{path}\": {error}"
                ))
            })?;
            return replay_log(
                database,
                attached,
                storage,
                open_wal(storage, &path)?,
                WalFile::Checkpoint,
            );
        } else if read_only {
            checkpoint_wal = Some(checkpoint_wal_path);
        } else {
            drop(reader);
            merge_into_recovery_wal(storage, &path, checkpoint_position, &checkpoint_wal_path)?;
            return replay_log(
                database,
                attached,
                storage,
                open_wal(storage, &path)?,
                WalFile::Checkpoint,
            );
        }
    }
    if let Some(expected) = scan.expected_checkpoint {
        return Err(version_error(expected - 1, expected));
    }
    if truncate_failed_checkpoint {
        // The marker of the failed checkpoint ends the WAL; cut it off.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|error| {
                io_error(format!(
                    "Cannot open file \"{path}\": {}",
                    rustdb_common::os_error_text(&error)
                ))
            })?;
        file.set_len(scan.checkpoint_truncate_offset)
            .and_then(|()| file.sync_all())
            .map_err(|error| io_error(format!("Could not truncate file \"{path}\": {error}")))?;
        if scan.checkpoint_truncate_offset == 0 {
            return Ok(Some((0, false)));
        }
        reader = open_wal(storage, &path)?;
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
    if all_succeeded && let Some(checkpoint_wal) = checkpoint_wal {
        return replay_log(
            database,
            attached,
            storage,
            open_wal(storage, &checkpoint_wal)?,
            WalFile::Checkpoint,
        );
    }
    Ok(Some((successful_offset, !all_succeeded)))
}

/// Copies `count` bytes from the position of `source` to the end of `target`
/// (`CopyOverWAL`).
fn copy_wal(
    source: &mut std::fs::File,
    source_path: &str,
    count: u64,
    target: &mut std::fs::File,
    target_path: &str,
) -> Result<()> {
    use std::io::Read;
    let copied = std::io::copy(&mut Read::by_ref(source).take(count), target).map_err(|error| {
        io_error(format!(
            "Could not copy \"{source_path}\" to \"{target_path}\": {error}"
        ))
    })?;
    if copied != count {
        return Err(io_error(format!(
            "Could not read file \"{source_path}\": unexpected end of file"
        )));
    }
    Ok(())
}

/// Merges the WAL at `wal_path` up to its checkpoint marker at `checkpoint_position` and
/// the entries of the checkpoint WAL into the recovery WAL, which then replaces the WAL;
/// the checkpoint WAL is removed (`MergeIntoRecoveryWAL`).
fn merge_into_recovery_wal(
    storage: &StorageManager,
    wal_path: &str,
    checkpoint_position: u64,
    checkpoint_wal_path: &str,
) -> Result<()> {
    let recovery_path = storage.recovery_wal_path();
    let open = |path: &str| {
        std::fs::File::open(path).map_err(|error| {
            io_error(format!(
                "Cannot open file \"{path}\": {}",
                rustdb_common::os_error_text(&error)
            ))
        })
    };
    let mut recovery = std::fs::File::create(&recovery_path)
        .map_err(|error| io_error(format!("Cannot open file \"{recovery_path}\": {error}")))?;
    let mut main = open(wal_path)?;
    copy_wal(
        &mut main,
        wal_path,
        checkpoint_position,
        &mut recovery,
        &recovery_path,
    )?;
    drop(main);
    let mut checkpoint_reader = open_wal(storage, checkpoint_wal_path)?;
    if !checkpoint_reader.finished() {
        // The checkpoint WAL's version entry is not copied; the versions must match.
        let mut state = ScanState::default();
        let version = check_header(&mut checkpoint_reader, storage, WalFile::Checkpoint, &mut state)?;
        let main_version = open_wal(storage, wal_path)?.read_header()?.version;
        if version != main_version {
            return Err(Error::invalid_input(format!(
                "Failure while replaying checkpoint WAL file \"{checkpoint_wal_path}\": checkpoint WAL version is \
                 different from main WAL version"
            )));
        }
        let header_end = checkpoint_reader.offset();
        let mut checkpoint = open(checkpoint_wal_path)?;
        let size = checkpoint
            .metadata()
            .map_err(|error| io_error(format!("Cannot open file \"{checkpoint_wal_path}\": {error}")))?
            .len();
        std::io::Seek::seek(&mut checkpoint, std::io::SeekFrom::Start(header_end)).map_err(|error| {
            io_error(format!(
                "Could not seek in file \"{checkpoint_wal_path}\": {error}"
            ))
        })?;
        copy_wal(
            &mut checkpoint,
            checkpoint_wal_path,
            size - header_end,
            &mut recovery,
            &recovery_path,
        )?;
    }
    recovery
        .sync_all()
        .map_err(|error| io_error(format!("Could not fsync file \"{recovery_path}\": {error}")))?;
    drop(recovery);
    let abort = storage.checkpoint_config().abort;
    abort.check(CheckpointAbort::BeforeMovingRecovery)?;
    std::fs::rename(&recovery_path, wal_path).map_err(|error| {
        io_error(format!(
            "Could not move file \"{recovery_path}\" to \"{wal_path}\": {error}"
        ))
    })?;
    abort.check(CheckpointAbort::BeforeDeletingCheckpointWal)?;
    std::fs::remove_file(checkpoint_wal_path).map_err(|error| {
        io_error(format!(
            "Could not remove file \"{checkpoint_wal_path}\": {error}"
        ))
    })
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
    RowGroupData(PersistentCollectionData),
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
        WalType::RowGroupData => WalEntry::RowGroupData(read_row_group_data(d)?),
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
        let entry = parse_entry(d, &self.catalog.name())?;
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
                let mut info = rustdb_catalog::checkpoint::schema_create_info(&self.catalog.name(), &schema);
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
            WalEntry::RowGroupData(data) => {
                // The stored row groups join the table directly (`ReplayRowGroupData`).
                self.current_table("insert")?.replay_row_group_data(data)?;
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
                catalog: self.catalog.name().to_string(),
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
        info.catalog = self.catalog.name().to_string();
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
