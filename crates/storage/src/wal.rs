//! The write-ahead log (DuckDB's `WriteAheadLog`, the writing half of `WALWriteState`
//! and the entry framing of `WriteAheadLogDeserializer`).
//!
//! # Format
//! The WAL of `db.duckdb` is `db.duckdb.wal`. It starts with an unframed header object
//! (`WAL_VERSION`, the WAL version and, for files of version 67 and up, the identity and
//! checkpoint iteration of the database file it belongs to). Every following entry is
//! framed as `[u64 size][u64 checksum][payload]`; the payload is one object whose field
//! 100 is the [`WalType`]. A commit appends its entries followed by a `WAL_FLUSH` entry
//! and syncs the file; replay applies the entries up to the last complete flush.
//!
//! # Checkpoints
//! A checkpoint appends a `CHECKPOINT` entry with the root it will write, and commits go
//! to `db.duckdb.wal.checkpoint` while it runs. Once the checkpoint wrote its header, that
//! file replaces the WAL (or the WAL is removed if no commit wrote to it).

use std::fs::{File, OpenOptions};
use std::io::{BufReader, ErrorKind as IoErrorKind, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_serde::binary::BinarySerializer;
use rustdb_serde::{Deserializer, SerializationOptions, Serializer};
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::checkpoint::pointers::{PersistentCollectionData, serialize_meta_block_pointer};
use crate::checkpoint::stream::{ReadStream, StreamDeserializer};
use crate::checkpoint::vector_serde::serialize_chunk;
use crate::data_table::{DataTable, DataTableInfo};
use crate::metadata::MetaBlockPointer;
use crate::single_file_block_manager::SingleFileBlockManager;
use crate::storage_info::{DB_IDENTIFIER_LEN, checksum};

/// The WAL version written (`WAL_VERSION_NUMBER`); 3 is the encrypted WAL.
pub const WAL_VERSION_NUMBER: u64 = 2;

/// The kind of a WAL entry (DuckDB's `WALType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WalType {
    CreateTable = 1,
    DropTable = 2,
    CreateSchema = 3,
    DropSchema = 4,
    CreateView = 5,
    DropView = 6,
    CreateSequence = 8,
    DropSequence = 9,
    SequenceValue = 10,
    CreateMacro = 11,
    DropMacro = 12,
    CreateType = 13,
    DropType = 14,
    AlterInfo = 20,
    CreateTableMacro = 21,
    DropTableMacro = 22,
    CreateIndex = 23,
    DropIndex = 24,
    UseTable = 25,
    InsertTuple = 26,
    DeleteTuple = 27,
    UpdateTuple = 28,
    RowGroupData = 29,
    WalVersion = 98,
    Checkpoint = 99,
    WalFlush = 100,
}

impl WalType {
    /// The entry type stored as `value`.
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::CreateTable,
            2 => Self::DropTable,
            3 => Self::CreateSchema,
            4 => Self::DropSchema,
            5 => Self::CreateView,
            6 => Self::DropView,
            8 => Self::CreateSequence,
            9 => Self::DropSequence,
            10 => Self::SequenceValue,
            11 => Self::CreateMacro,
            12 => Self::DropMacro,
            13 => Self::CreateType,
            14 => Self::DropType,
            20 => Self::AlterInfo,
            21 => Self::CreateTableMacro,
            22 => Self::DropTableMacro,
            23 => Self::CreateIndex,
            24 => Self::DropIndex,
            25 => Self::UseTable,
            26 => Self::InsertTuple,
            27 => Self::DeleteTuple,
            28 => Self::UpdateTuple,
            29 => Self::RowGroupData,
            98 => Self::WalVersion,
            99 => Self::Checkpoint,
            100 => Self::WalFlush,
            _ => return None,
        })
    }
}

fn io_error(message: impl Into<std::borrow::Cow<'static, str>>) -> Error {
    Error::new(ErrorKind::Io, message)
}

/// The path of the WAL of the database file at `path` (`StorageManager::GetWALPath`).
pub fn wal_path(path: &str) -> String {
    format!("{path}.wal")
}

/// The path of the WAL written while a checkpoint runs (`GetCheckpointWALPath`).
pub fn checkpoint_wal_path(path: &str) -> String {
    format!("{path}.wal.checkpoint")
}

/// The path the WAL and the checkpoint WAL of a failed checkpoint are merged into before
/// they are replayed (`GetRecoveryWALPath`).
pub fn recovery_wal_path(path: &str) -> String {
    format!("{path}.wal.recovery")
}

// ------------------------------------------------------------------------------------
// Writing
// ------------------------------------------------------------------------------------

/// The entries of one commit, serialized and framed but not written yet (DuckDB writes
/// them through the WAL's buffered writer as `WALWriteState` visits the undo buffer).
#[derive(Debug)]
pub struct WalBatch {
    options: SerializationOptions,
    buffer: Vec<u8>,
    entries: u64,
    /// The table the last data entry referred to (`WALWriteState::current_table_info`).
    current_table: Option<Arc<DataTableInfo>>,
}

impl WalBatch {
    pub fn new(options: SerializationOptions) -> Self {
        Self {
            options,
            buffer: Vec::new(),
            entries: 0,
            current_table: None,
        }
    }

    /// The serialization options of the entries.
    pub fn options(&self) -> &SerializationOptions {
        &self.options
    }

    /// Whether no entry was added.
    pub fn is_empty(&self) -> bool {
        self.entries == 0
    }

    /// Adds an entry of type `wal_type` whose remaining properties `write` writes
    /// (`WriteAheadLogSerializer`).
    pub fn entry(
        &mut self,
        wal_type: WalType,
        write: impl FnOnce(&mut dyn Serializer) -> Result<()>,
    ) -> Result<()> {
        let payload = serialize_entry(&self.options, wal_type, write)?;
        frame_entry(&mut self.buffer, &payload);
        self.entries += 1;
        Ok(())
    }

    /// Makes `table` the target of the following data entries, writing `USE_TABLE` if it
    /// is not already (`WALWriteState::SwitchTable`).
    pub fn switch_table(&mut self, table: &Arc<DataTableInfo>) -> Result<()> {
        if self
            .current_table
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, table))
        {
            return Ok(());
        }
        self.use_table(table)
    }

    /// Writes `USE_TABLE` for `table` (`WriteSetTable`).
    pub fn use_table(&mut self, table: &Arc<DataTableInfo>) -> Result<()> {
        let schema = table.schema_name();
        let name = table.table_name();
        self.entry(WalType::UseTable, |s| {
            s.write_property(101, "schema", &schema)?;
            s.write_property(102, "table", &name)
        })?;
        self.current_table = Some(Arc::clone(table));
        Ok(())
    }

    /// `INSERT_TUPLE` of `chunk` (`WriteInsert`).
    pub fn insert(&mut self, chunk: &DataChunk) -> Result<()> {
        self.entry(WalType::InsertTuple, |s| {
            s.write_object_property(101, "chunk", |s| serialize_chunk(s, chunk))
        })
    }

    /// `ROW_GROUP_DATA` of row groups an optimistic write stored (`WriteRowGroupData`).
    pub fn row_group_data(&mut self, data: &PersistentCollectionData) -> Result<()> {
        if data.row_group_data.is_empty() {
            return Err(Error::internal("ROW_GROUP_DATA without row groups"));
        }
        self.entry(WalType::RowGroupData, |s| {
            s.write_object_property(101, "row_group_data", |s| data.serialize_fields(s))
        })
    }

    /// `DELETE_TUPLE` of the rows `row_ids` (`WriteDelete`).
    pub fn delete(&mut self, row_ids: &[i64]) -> Result<()> {
        let mut ids = Vector::flat(LogicalType::BIGINT, row_ids.len());
        ids.values_mut::<i64>()[..row_ids.len()].copy_from_slice(row_ids);
        let chunk = DataChunk::new(vec![ids], row_ids.len());
        self.entry(WalType::DeleteTuple, |s| {
            s.write_object_property(101, "chunk", |s| serialize_chunk(s, &chunk))
        })
    }

    /// `UPDATE_TUPLE` of the column at `column_path`; `chunk` holds the new values and the
    /// row ids (`WriteUpdate`).
    pub fn update(&mut self, column_path: &[u64], chunk: &DataChunk) -> Result<()> {
        self.entry(WalType::UpdateTuple, |s| {
            s.write_list_property(101, "column_indexes", column_path.len(), |s, index| {
                s.write_u64(column_path[index]);
                Ok(())
            })?;
            s.write_object_property(102, "chunk", |s| serialize_chunk(s, chunk))
        })
    }

    /// `SEQUENCE_VALUE` (`WriteSequenceValue`).
    pub fn sequence_value(&mut self, schema: &str, name: &str, usage_count: u64, counter: i64) -> Result<()> {
        self.entry(WalType::SequenceValue, |s| {
            s.write_property(101, "schema", schema)?;
            s.write_property(102, "name", name)?;
            s.write_property(103, "usage_count", &usage_count)?;
            s.write_property(104, "counter", &counter)
        })
    }
}

/// Row groups of one table an optimistic write stored and a commit moved into the table
/// (DuckDB's `OptimisticallyWrittenRowGroupData`).
#[derive(Debug)]
struct OptimisticallyWrittenRowGroupData {
    table: Arc<DataTable>,
    /// The row id of the first row.
    start: u64,
    count: u64,
    data: PersistentCollectionData,
}

/// What a commit logs differently because of optimistic writes (DuckDB's
/// `SingleFileStorageCommitState`): the row groups that moved into tables as they are,
/// logged as `ROW_GROUP_DATA` instead of their rows.
#[derive(Debug, Default)]
pub struct StorageCommitState {
    row_group_data: Vec<OptimisticallyWrittenRowGroupData>,
}

impl StorageCommitState {
    /// Records that `count` rows from row id `start` of `table` are the stored row groups
    /// `data` (`AddRowGroupData`).
    pub(crate) fn add_row_group_data(
        &mut self,
        table: &Arc<DataTable>,
        start: u64,
        count: u64,
        data: PersistentCollectionData,
    ) {
        self.row_group_data.push(OptimisticallyWrittenRowGroupData {
            table: Arc::clone(table),
            start,
            count,
            data,
        });
    }

    /// The stored row groups at row id `start` of `table`, with their row count
    /// (`GetRowGroupData`).
    pub fn row_group_data(
        &self,
        table: &Arc<DataTable>,
        start: u64,
    ) -> Option<(u64, &PersistentCollectionData)> {
        self.row_group_data
            .iter()
            .find(|entry| Arc::ptr_eq(&entry.table, table) && entry.start == start)
            .map(|entry| (entry.count, &entry.data))
    }

    /// Whether stored row groups were moved into tables (`HasRowGroupData`).
    pub fn has_row_group_data(&self) -> bool {
        !self.row_group_data.is_empty()
    }
}

/// Serializes one entry object.
fn serialize_entry(
    options: &SerializationOptions,
    wal_type: WalType,
    write: impl FnOnce(&mut dyn Serializer) -> Result<()>,
) -> Result<Vec<u8>> {
    let mut serializer = BinarySerializer::new(options.clone());
    serializer.begin();
    {
        let s: &mut dyn Serializer = &mut serializer;
        s.write_property(100, "wal_type", &(wal_type as u8))?;
        write(s)?;
    }
    serializer.end();
    Ok(serializer.into_bytes())
}

/// Appends `payload` with its size and checksum (`ChecksumWriter::Flush`).
fn frame_entry(buffer: &mut Vec<u8>, payload: &[u8]) {
    buffer.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    buffer.extend_from_slice(&checksum(payload).to_le_bytes());
    buffer.extend_from_slice(payload);
}

#[derive(Debug, Default)]
struct WalState {
    /// The open file, opened by the first write (`Initialize`).
    file: Option<File>,
    /// While a checkpoint runs, commits are written to the checkpoint WAL
    /// (`.wal.checkpoint`), which becomes the WAL once the checkpoint completes: the
    /// checkpoint iteration it belongs to, the one after the checkpoint's start
    /// (`WriteAheadLog::checkpoint_iteration`).
    checkpoint_iteration: Option<u64>,
    /// The size of the WAL (`StorageManager::GetWALSize`).
    size: u64,
    /// Entries written since the WAL was last emptied (`GetWALEntriesCount`).
    entries: u64,
    /// The entries of the last commit.
    last_commit_entries: u64,
    /// The bytes the last commit appended.
    last_commit_size: u64,
    /// The file is cut to `size` when it is opened (`UNINITIALIZED_REQUIRES_TRUNCATE`):
    /// replay stopped at a torn or failed entry.
    requires_truncate: bool,
    /// Set when a checkpoint failed: the database is invalidated and nothing more is
    /// written (the WAL keeps the checkpoint marker last).
    failed: Option<String>,
    /// `RECOVERY_MODE NO_WAL_WRITES`: commits are not logged, only their size counted so
    /// that checkpoints still happen (`AddWALSize`).
    no_writes: bool,
}

/// The WAL of a database file.
#[derive(Debug)]
pub struct WriteAheadLog {
    path: String,
    /// The WAL commits are written to while a checkpoint runs (`GetCheckpointWALPath`).
    checkpoint_path: String,
    block_manager: Arc<SingleFileBlockManager>,
    options: SerializationOptions,
    state: Mutex<WalState>,
}

impl WriteAheadLog {
    /// The WAL at `path` whose existing contents are `size` bytes; with
    /// `requires_truncate` anything after them is cut off before the next write.
    /// `checkpoint_path` is where commits go while a checkpoint runs.
    pub fn new(
        path: String,
        checkpoint_path: String,
        block_manager: Arc<SingleFileBlockManager>,
        options: SerializationOptions,
        size: u64,
        requires_truncate: bool,
    ) -> Self {
        Self {
            path,
            checkpoint_path,
            block_manager,
            options,
            state: Mutex::new(WalState {
                size,
                requires_truncate,
                ..WalState::default()
            }),
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// The block manager of the database file the WAL belongs to.
    pub fn block_manager(&self) -> &Arc<SingleFileBlockManager> {
        &self.block_manager
    }

    /// A batch for the entries of one commit.
    pub fn batch(&self) -> WalBatch {
        WalBatch::new(self.options.clone())
    }

    /// Takes the WAL lock (`GetWALLock`): commits hold it while they write and publish
    /// their changes; checkpoints while they switch to the checkpoint WAL and back.
    pub fn lock(&self) -> WalGuard<'_> {
        WalGuard {
            wal: self,
            state: self.state.lock().unwrap_or_else(PoisonError::into_inner),
        }
    }

    /// The size of the WAL (`GetWALSize`).
    pub fn size(&self) -> u64 {
        self.lock().state.size
    }

    /// The header object (`WriteHeader`); it is not framed. A checkpoint WAL names its
    /// `checkpoint_iteration`, the WAL the iteration of the database file.
    fn header(&self, checkpoint_iteration: Option<u64>) -> Result<Vec<u8>> {
        let mut serializer = BinarySerializer::new(self.options.clone());
        serializer.begin();
        {
            let s: &mut dyn Serializer = &mut serializer;
            s.write_property(100, "wal_type", &(WalType::WalVersion as u8))?;
            s.write_property(101, "version", &WAL_VERSION_NUMBER)?;
            if self.block_manager.version_number() > 66 {
                let identifier = self.block_manager.db_identifier();
                s.write_list_property(102, "db_identifier", DB_IDENTIFIER_LEN, |s, index| {
                    s.write_u8(identifier[index]);
                    Ok(())
                })?;
                let iteration =
                    checkpoint_iteration.unwrap_or_else(|| self.block_manager.checkpoint_iteration());
                s.write_property(103, "checkpoint_iteration", &iteration)?;
            }
        }
        serializer.end();
        Ok(serializer.into_bytes())
    }
}

/// Removes the file at `path` if it exists (`TryRemoveFile`).
fn remove_if_exists(path: &str) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == IoErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(format!("Could not remove file \"{path}\": {error}"))),
    }
}

/// The WAL, locked.
#[derive(Debug)]
pub struct WalGuard<'a> {
    wal: &'a WriteAheadLog,
    state: MutexGuard<'a, WalState>,
}

impl WalGuard<'_> {
    /// The size of the WAL (`GetWALSize`).
    pub fn size(&self) -> u64 {
        self.state.size
    }

    /// The size of the WAL before the last commit: the size
    /// `SingleFileStorageManager::AutomaticCheckpoint` sees when that commit starts.
    pub fn size_before_last_commit(&self) -> u64 {
        self.state.size - self.state.last_commit_size
    }

    /// Entries written since the WAL was last emptied, before the last commit: the count
    /// `SingleFileStorageManager::AutomaticCheckpoint` sees when that commit starts.
    pub fn entries_before_last_commit(&self) -> u64 {
        self.state.entries - self.state.last_commit_entries
    }

    /// Whether commits are not logged.
    pub fn no_writes(&self) -> bool {
        self.state.no_writes
    }

    /// Stops logging commits (`RecoveryMode::NO_WAL_WRITES`).
    pub fn set_no_writes(&mut self) {
        self.state.no_writes = true;
    }

    /// Refuses every further commit with `message`, after a checkpoint failed and
    /// invalidated the database.
    pub fn invalidate(&mut self, message: String) {
        self.state.failed = Some(message);
    }

    /// The file commits are written to.
    fn path(&self) -> &str {
        if self.state.checkpoint_iteration.is_some() {
            &self.wal.checkpoint_path
        } else {
            &self.wal.path
        }
    }

    /// Opens the file for appending (`Initialize`).
    fn file(&mut self) -> Result<&mut File> {
        if self.state.file.is_none() {
            let path = self.path().to_owned();
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .map_err(|error| {
                    io_error(format!(
                        "Cannot open file \"{path}\": {}",
                        rustdb_common::os_error_text(&error)
                    ))
                })?;
            if self.state.requires_truncate {
                file.set_len(self.state.size)
                    .map_err(|error| io_error(format!("Could not truncate file \"{path}\": {error}")))?;
                self.state.requires_truncate = false;
            }
            let size = file
                .seek(SeekFrom::End(0))
                .map_err(|error| io_error(format!("Could not seek in file \"{path}\": {error}")))?;
            self.state.size = size;
            self.state.file = Some(file);
        }
        self.state
            .file
            .as_mut()
            .ok_or_else(|| Error::internal("WAL file not open"))
    }

    /// Writes `bytes` at the end of the WAL.
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        let path = self.path().to_owned();
        self.file()?
            .write_all(bytes)
            .map_err(|error| io_error(format!("Could not write file \"{path}\": {error}")))?;
        self.state.size += bytes.len() as u64;
        rustdb_common::profiling::add_to_counter(
            rustdb_common::profiling::MetricType::TotalBytesWritten,
            bytes.len() as u64,
        );
        Ok(())
    }

    /// Writes the header if the WAL is empty (`WriteHeader`).
    fn write_header(&mut self) -> Result<()> {
        self.file()?;
        if self.state.size > 0 {
            return Ok(());
        }
        let header = self.wal.header(self.state.checkpoint_iteration)?;
        self.append(&header)
    }

    /// Appends the entries of a commit followed by a `WAL_FLUSH` entry and syncs the file
    /// (`WALWriteState` + `WriteAheadLog::Flush`). On failure the WAL is cut back to its
    /// previous size (`StorageCommitState::RevertCommit`).
    pub fn commit(&mut self, mut batch: WalBatch) -> Result<()> {
        self.state.last_commit_entries = 0;
        self.state.last_commit_size = 0;
        if batch.is_empty() {
            return Ok(());
        }
        if let Some(message) = &self.state.failed {
            return Err(rustdb_common::Error::new(
                rustdb_common::ErrorKind::Io,
                message.clone(),
            ));
        }
        if self.state.no_writes {
            // `DuckTransaction::ShouldWriteToWAL` is false: only the size is counted.
            self.state.size += batch.buffer.len() as u64;
            self.state.last_commit_size = batch.buffer.len() as u64;
            return Ok(());
        }
        batch.entry(WalType::WalFlush, |_| Ok(()))?;
        let initial_size = self.state.size;
        let result = self.write_header().and_then(|()| {
            self.append(&batch.buffer)?;
            self.sync()
        });
        if result.is_err() {
            let _ = self.truncate(initial_size);
        } else {
            self.state.entries += batch.entries;
            self.state.last_commit_entries = batch.entries;
            self.state.last_commit_size = self.state.size - initial_size;
        }
        result
    }

    fn sync(&mut self) -> Result<()> {
        let path = self.path().to_owned();
        self.file()?
            .sync_data()
            .map_err(|error| io_error(format!("Could not fsync file \"{path}\": {error}")))
    }

    /// Cuts the WAL to `size` bytes (`WriteAheadLog::Truncate`).
    pub fn truncate(&mut self, size: u64) -> Result<()> {
        let path = self.path().to_owned();
        let file = self.file()?;
        file.set_len(size)
            .and_then(|()| file.seek(SeekFrom::Start(size)).map(|_| ()))
            .map_err(|error| io_error(format!("Could not truncate file \"{path}\": {error}")))?;
        self.state.size = size;
        self.state.last_commit_size = 0;
        Ok(())
    }

    /// Records that a checkpoint with root `meta_block` started and switches commits to
    /// the checkpoint WAL (`WALStartCheckpoint`): if the process dies before the
    /// checkpoint finishes, replay sees whether it completed, and replays the commits it
    /// did not include from the checkpoint WAL. Returns whether the WAL had entries;
    /// without, nothing changes.
    pub fn start_checkpoint(&mut self, meta_block: MetaBlockPointer) -> Result<bool> {
        if self.state.size == 0 {
            return Ok(false);
        }
        if self.state.checkpoint_iteration.is_some() {
            return Err(Error::internal(format!(
                "Current WAL path \"{}\" does not match base WAL path \"{}\" in WALStartCheckpoint",
                self.wal.checkpoint_path, self.wal.path
            )));
        }
        let mut batch = self.wal.batch();
        batch.entry(WalType::Checkpoint, |s| {
            s.write_object_property(101, "meta_block", |s| serialize_meta_block_pointer(s, meta_block))
        })?;
        // The marker is written even when commits are not (`WriteCheckpoint`).
        let no_writes = std::mem::take(&mut self.state.no_writes);
        let result = self.commit(batch);
        self.state.no_writes = no_writes;
        result?;
        // A checkpoint WAL left by an earlier checkpoint is obsolete; the new one is
        // created by the first commit.
        remove_if_exists(&self.wal.checkpoint_path)?;
        self.state.file = None;
        self.state.checkpoint_iteration = Some(self.wal.block_manager.checkpoint_iteration() + 1);
        self.reset_size();
        self.wal.block_manager.start_checkpoint();
        Ok(true)
    }

    /// Ends a checkpoint that switched to the checkpoint WAL (`WALFinishCheckpoint`):
    /// the checkpoint WAL, if commits wrote to it, replaces the WAL; otherwise the WAL is
    /// removed.
    pub fn finish_checkpoint(&mut self) -> Result<()> {
        self.state.file = None;
        self.state.checkpoint_iteration = None;
        self.reset_size();
        if !std::path::Path::new(&self.wal.checkpoint_path).exists() {
            return remove_if_exists(&self.wal.path);
        }
        std::fs::rename(&self.wal.checkpoint_path, &self.wal.path).map_err(|error| {
            io_error(format!(
                "Could not move file \"{}\" to \"{}\": {error}",
                self.wal.checkpoint_path, self.wal.path
            ))
        })?;
        // Open what is now the WAL again; it keeps the checkpoint WAL's entries.
        self.file().map(|_| ())
    }

    /// An empty WAL (`WriteAheadLog(storage_manager, path)` resets the size and entry
    /// count).
    fn reset_size(&mut self) {
        self.state.size = 0;
        self.state.entries = 0;
        self.state.last_commit_entries = 0;
        self.state.last_commit_size = 0;
        self.state.requires_truncate = false;
    }
}

// ------------------------------------------------------------------------------------
// Reading
// ------------------------------------------------------------------------------------

/// The header of a WAL (`ReplayVersion`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalHeader {
    pub version: u64,
    pub db_identifier: Option<[u8; DB_IDENTIFIER_LEN]>,
    pub checkpoint_iteration: Option<u64>,
}

/// Reads a WAL entry by entry (DuckDB's `BufferedFileReader` with the framing of
/// `GetEntryDeserializer`).
#[derive(Debug)]
pub struct WalReader {
    path: String,
    reader: BufReader<File>,
    offset: u64,
    file_size: u64,
    /// The WAL's version: 1 for WALs of DuckDB before v0.10, whose entries are neither
    /// sized nor checksummed (`ReplayState::wal_version`).
    version: u64,
}

impl ReadStream for WalReader {
    fn read_data(&mut self, buffer: &mut [u8]) -> Result<()> {
        if self.offset + buffer.len() as u64 > self.file_size {
            return Err(Error::serialization(
                "not enough data in file to deserialize result",
            ));
        }
        self.reader
            .read_exact(buffer)
            .map_err(|error| io_error(format!("Could not read file \"{}\": {error}", self.path)))?;
        self.offset += buffer.len() as u64;
        Ok(())
    }
}

impl WalReader {
    /// Opens the WAL at `path`; `None` if there is none.
    pub fn open(path: &str) -> Result<Option<Self>> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == IoErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(io_error(format!(
                    "Cannot open file \"{path}\": {}",
                    rustdb_common::os_error_text(&error)
                )));
            }
        };
        let file_size = file
            .metadata()
            .map_err(|error| {
                io_error(format!(
                    "Cannot open file \"{path}\": {}",
                    rustdb_common::os_error_text(&error)
                ))
            })?
            .len();
        Ok(Some(Self {
            path: path.to_owned(),
            reader: BufReader::new(file),
            offset: 0,
            file_size,
            version: WAL_VERSION_NUMBER,
        }))
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// The byte position of the next entry.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Whether everything was read.
    pub fn finished(&self) -> bool {
        self.offset >= self.file_size
    }

    /// Goes back to the start of the file.
    pub fn reset(&mut self) -> Result<()> {
        self.reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| io_error(format!("Could not seek in file \"{}\": {error}", self.path)))?;
        self.offset = 0;
        Ok(())
    }

    /// Reads the header object (the first, unframed entry).
    pub fn read_header(&mut self) -> Result<WalHeader> {
        let mut deserializer = StreamDeserializer::new(self);
        deserializer.begin()?;
        let d: &mut dyn Deserializer = &mut deserializer;
        let wal_type: u8 = d.read_property(100, "wal_type")?;
        if wal_type != WalType::WalVersion as u8 {
            // DuckDB before v0.10 wrote no version entry: the first entry is data.
            self.reset()?;
            self.version = 1;
            return Ok(WalHeader {
                version: 1,
                db_identifier: None,
                checkpoint_iteration: None,
            });
        }
        let version: u64 = d.read_property(101, "version")?;
        let mut identifier = [0u8; DB_IDENTIFIER_LEN];
        let mut has_identifier = false;
        if d.on_optional_property_begin(102, "db_identifier")? {
            let count = d.on_list_begin()?;
            for index in 0..count {
                let byte = d.read_u8()?;
                if let Some(slot) = identifier.get_mut(index) {
                    *slot = byte;
                }
            }
            d.on_list_end()?;
            d.on_optional_property_end(true)?;
            has_identifier = true;
        }
        let checkpoint_iteration: Option<u64> = d.read_optional(103, "checkpoint_iteration")?;
        deserializer.end()?;
        self.version = version;
        Ok(WalHeader {
            version,
            db_identifier: has_identifier.then_some(identifier),
            checkpoint_iteration,
        })
    }

    /// The WAL's version (1 for WALs written before v0.10).
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Reads the next entry with `read` (`GetEntryDeserializer`): from its sized and
    /// checksummed frame, or straight from the file for version 1.
    pub fn read_entry<R>(&mut self, read: impl FnOnce(&mut dyn Deserializer) -> Result<R>) -> Result<R> {
        if self.version == 1 {
            return read(&mut StreamDeserializer::new(self));
        }
        let payload = self.next_entry()?;
        read(&mut rustdb_serde::binary::BinaryDeserializer::new(&payload))
    }

    /// Reads the payload of the next entry and verifies its checksum. A torn entry is a
    /// serialization error, a checksum mismatch an I/O error.
    pub fn next_entry(&mut self) -> Result<Vec<u8>> {
        let mut size = [0u8; 8];
        self.read_data(&mut size)?;
        let mut stored = [0u8; 8];
        self.read_data(&mut stored)?;
        let size = u64::from_le_bytes(size);
        let stored = u64::from_le_bytes(stored);
        let offset = self.offset;
        if offset + size > self.file_size {
            return Err(Error::serialization(format!(
                "Corrupt WAL file: entry size exceeded remaining data in file at byte position {offset} (found \
                 entry with size {size} bytes, file size {} bytes)",
                self.file_size
            )));
        }
        let mut payload =
            vec![0u8; usize::try_from(size).map_err(|_| Error::serialization("WAL entry too large"))?];
        self.read_data(&mut payload)?;
        let computed = checksum(&payload);
        if computed != stored {
            return Err(io_error(format!(
                "Corrupt WAL file: entry at byte position {offset} computed checksum {computed} does not match \
                 stored checksum {stored}"
            )));
        }
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_serde::binary::BinaryDeserializer;

    #[test]
    fn entries_are_framed_with_their_checksum() {
        let mut batch = WalBatch::new(SerializationOptions::default());
        batch.delete(&[1, 5]).unwrap();
        assert_eq!(batch.entries, 1);
        let size = u64::from_le_bytes(batch.buffer[..8].try_into().unwrap()) as usize;
        let stored = u64::from_le_bytes(batch.buffer[8..16].try_into().unwrap());
        let payload = &batch.buffer[16..];
        assert_eq!(payload.len(), size);
        assert_eq!(stored, checksum(payload));
        let mut deserializer = BinaryDeserializer::new(payload);
        deserializer.begin().unwrap();
        let wal_type: u8 = (&mut deserializer as &mut dyn Deserializer)
            .read_property(100, "wal_type")
            .unwrap();
        assert_eq!(WalType::from_u8(wal_type), Some(WalType::DeleteTuple));
    }
}
