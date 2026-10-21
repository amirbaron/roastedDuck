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

use std::fs::{File, OpenOptions};
use std::io::{BufReader, ErrorKind as IoErrorKind, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_serde::binary::BinarySerializer;
use rustdb_serde::{Deserializer, SerializationOptions, Serializer};
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::checkpoint::pointers::serialize_meta_block_pointer;
use crate::checkpoint::stream::{ReadStream, StreamDeserializer};
use crate::checkpoint::vector_serde::serialize_chunk;
use crate::data_table::DataTableInfo;
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
    /// The size of the WAL (`StorageManager::GetWALSize`).
    size: u64,
    /// Entries written since the WAL was last emptied (`GetWALEntriesCount`).
    entries: u64,
    /// The entries of the last commit.
    last_commit_entries: u64,
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
    block_manager: Arc<SingleFileBlockManager>,
    options: SerializationOptions,
    state: Mutex<WalState>,
}

impl WriteAheadLog {
    /// The WAL at `path` whose existing contents are `size` bytes; with
    /// `requires_truncate` anything after them is cut off before the next write.
    pub fn new(
        path: String,
        block_manager: Arc<SingleFileBlockManager>,
        options: SerializationOptions,
        size: u64,
        requires_truncate: bool,
    ) -> Self {
        Self {
            path,
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

    /// A batch for the entries of one commit.
    pub fn batch(&self) -> WalBatch {
        WalBatch::new(self.options.clone())
    }

    /// Takes the WAL lock (`GetWALLock`): commits hold it while they write and publish
    /// their changes, checkpoints for their whole duration.
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

    /// The header object (`WriteHeader`); it is not framed.
    fn header(&self) -> Result<Vec<u8>> {
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
                s.write_property(
                    103,
                    "checkpoint_iteration",
                    &self.block_manager.checkpoint_iteration(),
                )?;
            }
        }
        serializer.end();
        Ok(serializer.into_bytes())
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

    /// Opens the file for appending (`Initialize`).
    fn file(&mut self) -> Result<&mut File> {
        if self.state.file.is_none() {
            let path = &self.wal.path;
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
                .map_err(|error| io_error(format!("Cannot open file \"{path}\": {error}")))?;
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
        let path = self.wal.path.clone();
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
        let header = self.wal.header()?;
        self.append(&header)
    }

    /// Appends the entries of a commit followed by a `WAL_FLUSH` entry and syncs the file
    /// (`WALWriteState` + `WriteAheadLog::Flush`). On failure the WAL is cut back to its
    /// previous size (`StorageCommitState::RevertCommit`).
    pub fn commit(&mut self, mut batch: WalBatch) -> Result<()> {
        self.state.last_commit_entries = 0;
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
        }
        result
    }

    fn sync(&mut self) -> Result<()> {
        let path = self.wal.path.clone();
        self.file()?
            .sync_data()
            .map_err(|error| io_error(format!("Could not fsync file \"{path}\": {error}")))
    }

    /// Cuts the WAL to `size` bytes (`WriteAheadLog::Truncate`).
    pub fn truncate(&mut self, size: u64) -> Result<()> {
        let path = self.wal.path.clone();
        let file = self.file()?;
        file.set_len(size)
            .and_then(|()| file.seek(SeekFrom::Start(size)).map(|_| ()))
            .map_err(|error| io_error(format!("Could not truncate file \"{path}\": {error}")))?;
        self.state.size = size;
        Ok(())
    }

    /// Records that a checkpoint with root `meta_block` started (`WALStartCheckpoint`):
    /// if the process dies before the WAL is removed, replay sees whether the checkpoint
    /// completed. Returns whether the WAL had entries.
    pub fn start_checkpoint(&mut self, meta_block: MetaBlockPointer) -> Result<bool> {
        if self.state.size == 0 {
            return Ok(false);
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
        Ok(true)
    }

    /// Removes the WAL after a completed checkpoint (`WALFinishCheckpoint`).
    pub fn finish_checkpoint(&mut self) -> Result<()> {
        self.state.file = None;
        self.state.size = 0;
        self.state.entries = 0;
        self.state.last_commit_entries = 0;
        self.state.requires_truncate = false;
        match std::fs::remove_file(&self.wal.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == IoErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(format!(
                "Could not remove file \"{}\": {error}",
                self.wal.path
            ))),
        }
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
            Err(error) => return Err(io_error(format!("Cannot open file \"{path}\": {error}"))),
        };
        let file_size = file
            .metadata()
            .map_err(|error| io_error(format!("Cannot open file \"{path}\": {error}")))?
            .len();
        Ok(Some(Self {
            path: path.to_owned(),
            reader: BufReader::new(file),
            offset: 0,
            file_size,
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
            return Err(Error::not_implemented(
                "replaying a WAL without a version entry (written by DuckDB before v0.10)",
            ));
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
        Ok(WalHeader {
            version,
            db_identifier: has_identifier.then_some(identifier),
            checkpoint_iteration,
        })
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
