//! The block manager of a database file (DuckDB's `SingleFileBlockManager`): headers,
//! checksummed blocks and the free list.
//!
//! Blocks freed by a checkpoint only become reusable after the next checkpoint wrote a
//! header that no longer references them (`modified_blocks`); blocks allocated since the
//! last checkpoint (`newly_used_blocks`) are still free as far as the file on disk is
//! concerned and are written into the free list as such.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rustdb_buffer::{BlockId, BlockManager, BlockRegistry, FileBuffer};
use rustdb_common::profiling::{MetricType, add_to_counter};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::logging::FileSystemLogger;

use crate::compression::CompressionConfig;
use crate::metadata::{MetaBlockPointer, MetadataManager, MetadataReader, MetadataWriter};
use crate::storage_info::{
    BLOCK_START, DB_IDENTIFIER_LEN, DEFAULT_BLOCK_HEADER_SIZE, DEPRECATED_VERSION_NUMBER, DatabaseHeader,
    FILE_HEADER_SIZE, INVALID_BLOCK_INDEX, MAGIC_BYTE_OFFSET, MAGIC_BYTES, MainHeader, STANDARD_VECTOR_SIZE,
    VERSION_NUMBER, checksum, unsupported_version_error,
};

fn io_error(message: impl Into<std::borrow::Cow<'static, str>>) -> Error {
    Error::new(ErrorKind::Io, message)
}

/// Positional file I/O (DuckDB's `FileHandle::Read/Write` at a location).
#[derive(Debug)]
struct FileHandle {
    file: File,
    path: String,
    /// The `FileSystem` log of the connection that opened the file.
    logger: Option<FileSystemLogger>,
}

impl FileHandle {
    /// A handle of `file`, opened now at `path`.
    fn new(file: File, path: &str) -> Self {
        Self {
            file,
            path: path.to_owned(),
            logger: FileSystemLogger::open(path),
        }
    }

    /// Reads `buffer.len()` bytes at `location`; the running query counts them
    /// (`FileHandle::Read`'s `TOTAL_BYTES_READ`).
    fn read_at(&self, buffer: &mut [u8], location: u64) -> Result<()> {
        add_to_counter(MetricType::TotalBytesRead, buffer.len() as u64);
        if let Some(logger) = &self.logger {
            logger.read(buffer.len() as u64, location);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file
                .read_exact_at(buffer, location)
                .map_err(|error| self.error("read from", location, &error))
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            let mut done = 0;
            while done < buffer.len() {
                let read = self
                    .file
                    .seek_read(&mut buffer[done..], location + done as u64)
                    .map_err(|error| self.error("read from", location, &error))?;
                if read == 0 {
                    return Err(io_error(format!(
                        "Could not read enough bytes from file \"{}\"",
                        self.path
                    )));
                }
                done += read;
            }
            Ok(())
        }
    }

    /// Writes `buffer` at `location`; the running query counts the bytes
    /// (`FileHandle::Write`'s `TOTAL_BYTES_WRITTEN`).
    fn write_at(&self, buffer: &[u8], location: u64) -> Result<()> {
        self.write_parts_at(&[], buffer, location)
    }

    /// Writes `header` followed by `data` at `location` as one write, as DuckDB writes a
    /// block together with the checksum in front of it.
    fn write_parts_at(&self, header: &[u8], data: &[u8], location: u64) -> Result<()> {
        let total = header.len() + data.len();
        add_to_counter(MetricType::TotalBytesWritten, total as u64);
        if let Some(logger) = &self.logger {
            logger.write(total as u64, location);
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut written = 0;
            while written < total {
                // The parts not written yet.
                let (header_rest, data_rest) = if written < header.len() {
                    (&header[written..], data)
                } else {
                    (&[][..], &data[written - header.len()..])
                };
                let parts = [
                    libc::iovec {
                        iov_base: header_rest.as_ptr().cast_mut().cast(),
                        iov_len: header_rest.len(),
                    },
                    libc::iovec {
                        iov_base: data_rest.as_ptr().cast_mut().cast(),
                        iov_len: data_rest.len(),
                    },
                ];
                let skip = usize::from(header_rest.is_empty());
                let offset = libc::off_t::try_from(location + written as u64).map_err(|_| {
                    io_error(format!(
                        "Could not write to file \"{}\": offset too large",
                        self.path
                    ))
                })?;
                // SAFETY: the iovecs point into `header` and `data`, which outlive the call.
                let result = unsafe {
                    libc::pwritev(
                        self.file.as_raw_fd(),
                        parts[skip..].as_ptr(),
                        (parts.len() - skip) as libc::c_int,
                        offset,
                    )
                };
                match result {
                    0 => {
                        let error = std::io::Error::from(std::io::ErrorKind::WriteZero);
                        return Err(self.error("write to", location, &error));
                    }
                    written_now if written_now > 0 => written += written_now.unsigned_abs(),
                    _ => {
                        let error = std::io::Error::last_os_error();
                        if error.kind() != std::io::ErrorKind::Interrupted {
                            return Err(self.error("write to", location, &error));
                        }
                    }
                }
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            let buffer = [header, data].concat();
            let mut done = 0;
            while done < buffer.len() {
                done += self
                    .file
                    .seek_write(&buffer[done..], location + done as u64)
                    .map_err(|error| self.error("write to", location, &error))?;
            }
            Ok(())
        }
    }

    fn error(&self, action: &str, location: u64, error: &std::io::Error) -> Error {
        io_error(format!(
            "Could not {action} file \"{}\" at location {location}: {error}",
            self.path
        ))
    }

    fn size(&self) -> Result<u64> {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|error| io_error(format!("Could not get size of file \"{}\": {error}", self.path)))
    }

    fn sync(&self) -> Result<()> {
        self.file
            .sync_data()
            .map_err(|error| io_error(format!("Could not fsync file \"{}\": {error}", self.path)))
    }

    fn truncate(&self, size: u64) -> Result<()> {
        self.file
            .set_len(size)
            .map_err(|error| io_error(format!("Could not truncate file \"{}\": {error}", self.path)))
    }
}

impl Drop for FileHandle {
    fn drop(&mut self) {
        if let Some(logger) = &self.logger {
            logger.close();
        }
    }
}

/// The free-list bookkeeping (the members DuckDB guards with `single_file_block_lock`).
#[derive(Debug, Default)]
struct BlockState {
    /// Blocks that can be allocated.
    free_list: BTreeSet<BlockId>,
    /// Free blocks that still have a live handle; they join the free list on unregister.
    free_blocks_in_use: BTreeSet<BlockId>,
    /// Blocks referenced more than once, with their reference count.
    multi_use_blocks: BTreeMap<BlockId, u32>,
    /// Blocks freed since the last checkpoint: reusable once the next header is written.
    modified_blocks: BTreeSet<BlockId>,
    /// Blocks allocated since the last checkpoint.
    newly_used_blocks: BTreeSet<BlockId>,
    /// The number of blocks in the file.
    max_block: BlockId,
    iteration_count: u64,
    /// Which database header (0 or 1) is active.
    active_header: u8,
    /// The metadata pointer of the catalog.
    meta_block: u64,
    /// The metadata pointer of the free list.
    free_list_id: u64,
    /// The data blocks the checkpoint on disk references (they become free once a
    /// newer checkpoint no longer does).
    checkpoint_blocks: BTreeSet<BlockId>,
    /// While a checkpoint runs, the blocks of optimistic writes commits logged since it
    /// started: the checkpoint does not write their rows, but they stay in use.
    committed_during_checkpoint: Option<BTreeSet<BlockId>>,
}

/// How a block id is taken (`FreeBlockType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FreeBlockType {
    NewlyUsedBlock,
    CheckpointedBlock,
}

/// Options of a database file (DuckDB's `StorageManagerOptions`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockManagerOptions {
    pub read_only: bool,
    pub block_alloc_size: usize,
    /// The serialization version (`storage_version`); `None` keeps the file's.
    pub storage_version: Option<u64>,
}

/// The block manager of a database file.
#[derive(Debug)]
pub struct SingleFileBlockManager {
    handle: FileHandle,
    block_alloc_size: usize,
    read_only: bool,
    /// `MainHeader::version_number`; a checkpoint upgrading the file from 64 raises it.
    version_number: AtomicU64,
    storage_version: u64,
    /// The random identity of the file, matched by its WAL (`MainHeader::db_identifier`).
    db_identifier: [u8; DB_IDENTIFIER_LEN],
    registry: BlockRegistry,
    state: Mutex<BlockState>,
    /// How checkpoints compress segments (`force_compression` and related settings).
    compression: Mutex<CompressionConfig>,
}

impl SingleFileBlockManager {
    fn lock(&self) -> MutexGuard<'_, BlockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A header block: an 8-byte checksum followed by `content` zero-padded to 4 KiB
    /// (`ChecksumAndWrite` of the header buffer).
    fn write_header_block(&self, content: &[u8], location: u64) -> Result<()> {
        let mut block = vec![0u8; FILE_HEADER_SIZE];
        block[DEFAULT_BLOCK_HEADER_SIZE..DEFAULT_BLOCK_HEADER_SIZE + content.len()].copy_from_slice(content);
        let sum = checksum(&block[DEFAULT_BLOCK_HEADER_SIZE..]);
        block[..DEFAULT_BLOCK_HEADER_SIZE].copy_from_slice(&sum.to_le_bytes());
        self.handle.write_at(&block, location)
    }

    /// Reads a header block and verifies its checksum (`ReadAndChecksum`); returns the
    /// bytes after the checksum.
    fn read_header_block(&self, location: u64) -> Result<Vec<u8>> {
        let mut block = vec![0u8; FILE_HEADER_SIZE];
        self.handle.read_at(&mut block, location)?;
        verify_checksum(&block, location)?;
        Ok(block.split_off(DEFAULT_BLOCK_HEADER_SIZE))
    }

    /// Creates a new database file (`CreateNewDatabase`).
    pub fn create_new(path: &str, options: BlockManagerOptions) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|error| {
                io_error(format!(
                    "Cannot open file \"{path}\": {}",
                    rustdb_common::os_error_text(&error)
                ))
            })?;
        let storage_version = options
            .storage_version
            .unwrap_or(crate::storage_info::DEFAULT_SERIALIZATION_VERSION);
        let version_number = if storage_version < 4 {
            VERSION_NUMBER
        } else {
            crate::storage_info::storage_version(&crate::storage_info::storage_version_name(
                storage_version,
                false,
            ))
            .unwrap_or(VERSION_NUMBER)
        };
        let mut main_header = MainHeader::new(version_number);
        if version_number >= 67 {
            main_header.db_identifier = random_identifier();
        }
        let manager = Self {
            handle: FileHandle::new(file, path),
            block_alloc_size: options.block_alloc_size,
            read_only: false,
            version_number: AtomicU64::new(version_number),
            storage_version,
            db_identifier: main_header.db_identifier,
            registry: BlockRegistry::default(),
            compression: Mutex::default(),
            state: Mutex::new(BlockState {
                active_header: 1,
                meta_block: INVALID_BLOCK_INDEX,
                free_list_id: INVALID_BLOCK_INDEX,
                ..BlockState::default()
            }),
        };
        let mut content = Vec::with_capacity(FILE_HEADER_SIZE);
        main_header.write(&mut content);
        manager.write_header_block(&content, 0)?;
        // The database headers are serialized over the main header's buffer, as DuckDB
        // does, so the files are identical.
        let header = DatabaseHeader {
            iteration: 0,
            meta_block: INVALID_BLOCK_INDEX,
            free_list: INVALID_BLOCK_INDEX,
            block_count: 0,
            block_alloc_size: options.block_alloc_size as u64,
            vector_size: STANDARD_VECTOR_SIZE,
            serialization_compatibility: storage_version,
        };
        let mut header_bytes = Vec::new();
        header.write(&mut header_bytes);
        content[..header_bytes.len()].copy_from_slice(&header_bytes);
        manager.write_header_block(&content, FILE_HEADER_SIZE as u64)?;
        manager.write_header_block(&content, 2 * FILE_HEADER_SIZE as u64)?;
        manager.handle.sync()?;
        Ok(manager)
    }

    /// Opens an existing database file (`LoadExistingDatabase`); the free list is read
    /// with [`SingleFileBlockManager::load_free_list`] once the metadata manager exists.
    pub fn load_existing(path: &str, options: BlockManagerOptions) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(!options.read_only)
            .open(path)
            .map_err(|error| {
                if options.read_only && error.kind() == std::io::ErrorKind::NotFound {
                    io_error(format!(
                        "Cannot open database \"{path}\" in read-only mode: database does not exist"
                    ))
                } else {
                    io_error(format!(
                        "Cannot open file \"{path}\": {}",
                        rustdb_common::os_error_text(&error)
                    ))
                }
            })?;
        let handle = FileHandle::new(file, path);
        // `MainHeader::CheckMagicBytes`.
        let not_a_database = || {
            io_error(format!(
                "The file \"{path}\" exists, but it is not a valid DuckDB database file!"
            ))
        };
        if handle.size()? < (MAGIC_BYTE_OFFSET + MAGIC_BYTES.len()) as u64 {
            return Err(not_a_database());
        }
        let mut magic = [0u8; 4];
        handle.read_at(&mut magic, MAGIC_BYTE_OFFSET as u64)?;
        if &magic != MAGIC_BYTES {
            return Err(not_a_database());
        }
        let mut manager = Self {
            handle,
            block_alloc_size: options.block_alloc_size,
            read_only: options.read_only,
            version_number: AtomicU64::new(VERSION_NUMBER),
            storage_version: 1,
            db_identifier: [0; DB_IDENTIFIER_LEN],
            registry: BlockRegistry::default(),
            compression: Mutex::default(),
            state: Mutex::new(BlockState::default()),
        };
        let main_bytes = manager.read_header_block(0)?;
        let main_header = MainHeader::read(&main_bytes)?;
        if main_header.is_encrypted() {
            return Err(Error::catalog(format!(
                "Cannot open encrypted database \"{path}\" without a key"
            )));
        }
        *manager.version_number.get_mut() = main_header.version_number;
        manager.db_identifier = main_header.db_identifier;
        let h1 = DatabaseHeader::read(&main_header, &manager.read_header_block(FILE_HEADER_SIZE as u64)?)?;
        let h2 = DatabaseHeader::read(
            &main_header,
            &manager.read_header_block(2 * FILE_HEADER_SIZE as u64)?,
        )?;
        if main_header.version_number == DEPRECATED_VERSION_NUMBER {
            let active = if h1.iteration > h2.iteration { h1 } else { h2 };
            return Err(unsupported_version_error(active.serialization_compatibility));
        }
        let (active_header, header) = if h1.iteration > h2.iteration {
            (0, h1)
        } else {
            (1, h2)
        };
        manager.initialize(&header, options)?;
        manager.lock().active_header = active_header;
        Ok(manager)
    }

    /// `Initialize`: adopts the active header.
    fn initialize(&mut self, header: &DatabaseHeader, options: BlockManagerOptions) -> Result<()> {
        let path = &self.handle.path;
        if let Some(requested) = options.storage_version {
            if requested < header.serialization_compatibility {
                return Err(Error::invalid_input(format!(
                    "Error opening \"{path}\": cannot initialize database with storage version {requested} - which \
                     is lower than what the database itself uses ({}). The storage version of an existing database \
                     cannot be lowered.",
                    header.serialization_compatibility
                )));
            }
            self.storage_version = requested;
        } else {
            self.storage_version = header.serialization_compatibility;
        }
        if header.serialization_compatibility > crate::storage_info::LATEST_SERIALIZATION_VERSION {
            return Err(Error::invalid_input(format!(
                "Error opening \"{path}\": file was written with a storage version greater than the latest version \
                 supported by this DuckDB instance. Try opening the file with a newer version of DuckDB."
            )));
        }
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        state.free_list_id = header.free_list;
        state.meta_block = header.meta_block;
        state.iteration_count = header.iteration;
        state.max_block = i64::try_from(header.block_count).map_err(|_| io_error("invalid block count"))?;
        self.block_alloc_size =
            usize::try_from(header.block_alloc_size).map_err(|_| io_error("invalid block size"))?;
        Ok(())
    }

    pub fn path(&self) -> &str {
        &self.handle.path
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// The file's version number (`MainHeader::version_number`).
    pub fn version_number(&self) -> u64 {
        self.version_number.load(Ordering::Relaxed)
    }

    /// The serialization version (`StorageManager::GetStorageVersion`).
    pub fn storage_version(&self) -> u64 {
        self.storage_version
    }

    /// How checkpoints compress segments.
    pub fn compression_config(&self) -> CompressionConfig {
        *self.compression.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets how checkpoints compress segments (`force_compression`).
    pub fn set_compression_config(&self, config: CompressionConfig) {
        *self.compression.lock().unwrap_or_else(PoisonError::into_inner) = config;
    }

    /// The identity of the file (`GetDBIdentifier`).
    pub fn db_identifier(&self) -> [u8; DB_IDENTIFIER_LEN] {
        self.db_identifier
    }

    /// The iteration of the active database header (`GetCheckpointIteration`).
    pub fn checkpoint_iteration(&self) -> u64 {
        self.lock().iteration_count
    }

    /// The metadata pointer of the catalog (`GetMetaBlock`).
    pub fn meta_block(&self) -> u64 {
        self.lock().meta_block
    }

    /// The number of blocks in the file (`TotalBlocks`).
    pub fn total_blocks(&self) -> u64 {
        u64::try_from(self.lock().max_block).unwrap_or(0)
    }

    /// The number of free blocks (`FreeBlocks`).
    pub fn free_blocks(&self) -> u64 {
        self.lock().free_list.len() as u64
    }

    /// Reads the free list and the metadata block list (`LoadFreeList`). Every other block
    /// of the file holds data the checkpoint on disk references.
    pub fn load_free_list(&self, metadata: &MetadataManager) -> Result<()> {
        let free_pointer = MetaBlockPointer::new(self.lock().free_list_id, 0);
        if !free_pointer.is_valid() {
            let mut state = self.lock();
            state.checkpoint_blocks = (0..state.max_block).collect();
            return Ok(());
        }
        let mut reader = MetadataReader::new(metadata, free_pointer, true)?;
        let free_count = reader.read_u64()?;
        let mut free_list = BTreeSet::new();
        for _ in 0..free_count {
            free_list.insert(reader.read_i64()?);
        }
        let multi_use_count = reader.read_u64()?;
        let mut multi_use = BTreeMap::new();
        for _ in 0..multi_use_count {
            let block = reader.read_i64()?;
            let count = reader.read_u32()?;
            multi_use.insert(block, count);
        }
        metadata.read_block_list(&mut reader)?;
        let metadata_blocks: BTreeSet<BlockId> = metadata
            .metadata_info()
            .iter()
            .map(|block| block.block_id)
            .collect();
        {
            let mut state = self.lock();
            state.checkpoint_blocks = (0..state.max_block)
                .filter(|block| !free_list.contains(block) && !metadata_blocks.contains(block))
                .collect();
            state.free_list = free_list;
            state.multi_use_blocks = multi_use;
        }
        metadata.mark_blocks_as_modified();
        Ok(())
    }

    fn free_block_id_internal(&self, kind: FreeBlockType) -> Result<BlockId> {
        let mut state = self.lock();
        let block_id = if let Some(&first) = state.free_list.iter().next() {
            state.free_list.remove(&first);
            first
        } else {
            let id = state.max_block;
            state.max_block += 1;
            id
        };
        if kind == FreeBlockType::NewlyUsedBlock {
            state.newly_used_blocks.insert(block_id);
        }
        drop(state);
        if self.registry.is_registered(block_id) {
            return Err(Error::internal(format!(
                "Free block {block_id} is already registered"
            )));
        }
        Ok(block_id)
    }

    /// A block id to write new data to (`GetFreeBlockId`).
    pub fn get_free_block_id(&self) -> Result<BlockId> {
        self.free_block_id_internal(FreeBlockType::NewlyUsedBlock)
    }

    /// A block id for checkpoint data (`GetFreeBlockIdForCheckpoint`).
    pub fn get_free_block_id_for_checkpoint(&self) -> Result<BlockId> {
        self.free_block_id_internal(FreeBlockType::CheckpointedBlock)
    }

    /// The block id the next allocation returns (`PeekFreeBlockId`).
    pub fn peek_free_block_id(&self) -> BlockId {
        let state = self.lock();
        state.free_list.iter().next().copied().unwrap_or(state.max_block)
    }

    /// Marks a block written by an optimistic write as part of the persisted data once a
    /// commit logged it to the WAL (`MarkBlockAsCheckpointed`): it is no longer free in
    /// the free list the next checkpoint writes, and is freed once a checkpoint no longer
    /// references it, like the blocks of the checkpoint on disk.
    pub fn mark_block_as_checkpointed(&self, block_id: BlockId) {
        let mut state = self.lock();
        state.newly_used_blocks.remove(&block_id);
        state.checkpoint_blocks.insert(block_id);
        if let Some(committed) = &mut state.committed_during_checkpoint {
            committed.insert(block_id);
        }
    }

    /// Records that a checkpoint started while commits continue: the blocks they log
    /// until it writes its header stay in use, although the checkpoint does not reference
    /// them.
    pub fn start_checkpoint(&self) {
        self.lock().committed_during_checkpoint = Some(BTreeSet::new());
    }

    /// Marks a block read from the file as in use (`MarkBlockAsUsed`).
    pub fn mark_block_as_used(&self, block_id: BlockId) {
        let mut state = self.lock();
        if state.max_block <= block_id {
            while state.max_block < block_id {
                let free = state.max_block;
                state.free_list.insert(free);
                state.max_block += 1;
            }
            state.max_block += 1;
        } else if state.free_list.remove(&block_id) {
        } else {
            Self::increase_reference_count(&mut state, block_id);
        }
    }

    fn increase_reference_count(state: &mut BlockState, block_id: BlockId) {
        *state.multi_use_blocks.entry(block_id).or_insert(1) += 1;
    }

    /// `IncreaseBlockReferenceCount`.
    pub fn increase_block_reference_count(&self, block_id: BlockId) {
        Self::increase_reference_count(&mut self.lock(), block_id);
    }

    /// Marks a block as no longer needed (`MarkBlockAsModified`): blocks of this
    /// checkpoint are freed at once, blocks on disk after the next checkpoint.
    pub fn mark_block_as_modified(&self, block_id: BlockId) -> Result<()> {
        let mut state = self.lock();
        if let Some(count) = state.multi_use_blocks.get_mut(&block_id) {
            *count -= 1;
            if *count <= 1 {
                state.multi_use_blocks.remove(&block_id);
            }
            return Ok(());
        }
        if state.modified_blocks.contains(&block_id) {
            return Err(Error::internal(format!(
                "MarkBlockAsModified called with already modified block id {block_id}"
            )));
        }
        if state.free_list.contains(&block_id) {
            return Err(Error::internal(format!(
                "MarkBlockAsModified called with already freed block id {block_id}"
            )));
        }
        if state.newly_used_blocks.remove(&block_id) {
            self.add_free_block(&mut state, block_id);
        } else {
            state.modified_blocks.insert(block_id);
        }
        Ok(())
    }

    /// `AddFreeBlock`: a block with a live handle is freed when the handle goes away.
    fn add_free_block(&self, state: &mut BlockState, block_id: BlockId) -> bool {
        if self.registry.is_registered(block_id) {
            state.free_blocks_in_use.insert(block_id);
            false
        } else {
            state.free_list.insert(block_id);
            true
        }
    }

    /// Writes the free list and the database header of a checkpoint (`WriteHeader`).
    /// `data_blocks` are the data blocks the new checkpoint references; those of the
    /// previous checkpoint it no longer uses are freed, except blocks commits logged
    /// while it ran.
    pub fn write_header(
        &self,
        metadata: &MetadataManager,
        mut header: DatabaseHeader,
        data_blocks: &BTreeMap<BlockId, u32>,
        abort: crate::storage_manager::CheckpointAbort,
    ) -> Result<()> {
        let released: Vec<BlockId> = {
            let mut state = self.lock();
            let mut new_blocks: BTreeSet<BlockId> = data_blocks.keys().copied().collect();
            new_blocks.extend(state.committed_during_checkpoint.take().unwrap_or_default());
            let previous = std::mem::replace(&mut state.checkpoint_blocks, new_blocks);
            let released: Vec<BlockId> = previous.difference(&state.checkpoint_blocks).copied().collect();
            for block in &released {
                state.multi_use_blocks.remove(block);
            }
            // Blocks optimistic writes added that the checkpoint keeps are in use now
            // (`CheckpointBlockIdMarker`).
            for block in data_blocks.keys() {
                state.newly_used_blocks.remove(block);
            }
            // Blocks shared by several segments are multi-use blocks (`IncreaseBlockReferenceCount`).
            for (&block, &uses) in data_blocks {
                if uses > 1 {
                    state.multi_use_blocks.insert(block, uses);
                }
            }
            released
        };
        for block in released {
            self.mark_block_as_modified(block)?;
        }
        let free_list_blocks = self.free_list_blocks(metadata)?;
        metadata.mark_blocks_as_modified();
        let (all_free_blocks, written_multi_use) = {
            let mut state = self.lock();
            state.iteration_count += 1;
            header.iteration = state.iteration_count;
            let mut all_free = state.free_list.clone();
            let modified: Vec<BlockId> = state.modified_blocks.iter().copied().collect();
            for block in modified {
                all_free.insert(block);
                self.add_free_block(&mut state, block);
            }
            let mut written_multi_use = state.multi_use_blocks.clone();
            let newly_used: Vec<BlockId> = state.newly_used_blocks.iter().copied().collect();
            for block in newly_used {
                all_free.insert(block);
                written_multi_use.remove(&block);
            }
            state.modified_blocks.clear();
            (all_free, written_multi_use)
        };
        if free_list_blocks.is_empty() {
            header.free_list = INVALID_BLOCK_INDEX;
        } else {
            let mut writer = MetadataWriter::with_handles(metadata, free_list_blocks);
            header.free_list = writer.meta_block_pointer()?.block_pointer;
            writer.write_u64(all_free_blocks.len() as u64)?;
            for block in &all_free_blocks {
                writer.write_i64(*block)?;
            }
            writer.write_u64(written_multi_use.len() as u64)?;
            for (block, count) in &written_multi_use {
                writer.write_i64(*block)?;
                writer.write_u32(*count)?;
            }
            metadata.write_block_list(&mut writer)?;
            writer.flush()?;
        }
        metadata.flush()?;
        header.block_count = self.total_blocks();
        header.serialization_compatibility = self.storage_version;
        abort.check(crate::storage_manager::CheckpointAbort::AfterFreeListWrite)?;
        self.handle.sync()?;
        // A file of version 64 upgraded to a storage version that stores its
        // compatibility in the database header gets a version 65 main header.
        if self.version_number() == 64 && self.storage_version >= 4 {
            self.version_number.store(65, Ordering::Relaxed);
            let mut main_header = Vec::with_capacity(FILE_HEADER_SIZE);
            MainHeader::new(65).write(&mut main_header);
            self.write_header_block(&main_header, 0)?;
        }
        let mut content = Vec::with_capacity(FILE_HEADER_SIZE);
        header.write(&mut content);
        let location = {
            let state = self.lock();
            if state.active_header == 1 {
                FILE_HEADER_SIZE as u64
            } else {
                2 * FILE_HEADER_SIZE as u64
            }
        };
        self.write_header_block(&content, location)?;
        {
            let mut state = self.lock();
            state.active_header = 1 - state.active_header;
            state.meta_block = header.meta_block;
            state.free_list_id = header.free_list;
        }
        self.handle.sync()
    }

    /// Reserves the metadata blocks the free list is written to (`GetFreeListBlocks`).
    fn free_list_blocks(&self, metadata: &MetadataManager) -> Result<Vec<crate::metadata::MetadataHandle>> {
        let block_size = metadata.metadata_block_size() - 8;
        let mut blocks = Vec::new();
        let mut allocated = 0;
        loop {
            let (free_count, multi_use_count) = {
                let state = self.lock();
                (
                    state.free_list.len()
                        + state.modified_blocks.len()
                        + state.free_blocks_in_use.len()
                        + state.newly_used_blocks.len(),
                    state.multi_use_blocks.len(),
                )
            };
            let free_list_size = 8 + 8 * free_count;
            let multi_use_size = 8 + 12 * multi_use_count;
            let metadata_blocks = 8 + 16 * metadata.block_count();
            let total = free_list_size + multi_use_size + metadata_blocks;
            if total < allocated {
                break;
            }
            blocks.push(metadata.allocate_handle()?);
            allocated += block_size;
        }
        Ok(blocks)
    }

    /// Syncs the database file (`FileSync`): the blocks optimistic writes added must be
    /// durable before a WAL entry refers to them.
    pub fn file_sync(&self) -> Result<()> {
        self.handle.sync()
    }

    /// Cuts free blocks off the end of the file (`Truncate`).
    pub fn truncate(&self) -> Result<()> {
        let mut state = self.lock();
        let mut truncated = false;
        while let Some(&last) = state.free_list.iter().next_back() {
            if last + 1 != state.max_block {
                break;
            }
            state.free_list.remove(&last);
            state.max_block -= 1;
            truncated = true;
        }
        if !truncated {
            return Ok(());
        }
        let size = BLOCK_START + u64::try_from(state.max_block).unwrap_or(0) * self.block_alloc_size as u64;
        drop(state);
        self.handle.truncate(size)
    }

    /// Writes the data of block `block_id` behind its checksum (`ChecksumAndWrite`).
    pub fn write_block(&self, block_id: BlockId, data: &[u8]) -> Result<()> {
        if self.read_only {
            return Err(io_error("Cannot write to a database opened in read-only mode"));
        }
        let location = self.block_location(block_id)?;
        let sum = checksum(data);
        self.handle.write_parts_at(&sum.to_le_bytes(), data, location)
    }

    fn block_location(&self, block_id: BlockId) -> Result<u64> {
        let id =
            u64::try_from(block_id).map_err(|_| Error::internal(format!("invalid block id {block_id}")))?;
        Ok(BLOCK_START + id * self.block_alloc_size as u64)
    }
}

/// Verifies the checksum in the first 8 bytes of `block` (`CheckChecksum`).
fn verify_checksum(block: &[u8], location: u64) -> Result<()> {
    let mut stored = [0u8; 8];
    stored.copy_from_slice(&block[..DEFAULT_BLOCK_HEADER_SIZE]);
    let stored = u64::from_le_bytes(stored);
    let computed = checksum(&block[DEFAULT_BLOCK_HEADER_SIZE..]);
    if stored != computed {
        return Err(io_error(format!(
            "Corrupt database file: computed checksum {computed} does not match stored checksum {stored} in block \
             at location {location}"
        )));
    }
    Ok(())
}

/// A random database identifier (`GenerateDBIdentifier`).
fn random_identifier() -> [u8; crate::storage_info::DB_IDENTIFIER_LEN] {
    use std::hash::{BuildHasher, Hasher};
    let mut result = [0u8; crate::storage_info::DB_IDENTIFIER_LEN];
    for chunk in result.chunks_mut(8) {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64),
        );
        let bytes = hasher.finish().to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
    result
}

impl BlockManager for SingleFileBlockManager {
    fn block_alloc_size(&self) -> usize {
        self.block_alloc_size
    }

    fn total_blocks(&self) -> u64 {
        Self::total_blocks(self)
    }

    fn free_blocks(&self) -> u64 {
        Self::free_blocks(self)
    }

    fn block_header_size(&self) -> usize {
        DEFAULT_BLOCK_HEADER_SIZE
    }

    fn in_memory(&self) -> bool {
        false
    }

    fn read(&self, block_id: BlockId, buffer: &mut FileBuffer) -> Result<()> {
        let location = self.block_location(block_id)?;
        let bytes = buffer.internal_mut();
        self.handle.read_at(bytes, location)?;
        verify_checksum(bytes, location)
    }

    /// Blocks are immutable while their handle lives (a rewritten block gets a new
    /// handle), so the checksum verified by the first read is not computed again when a
    /// released table block is read back.
    fn reread(&self, block_id: BlockId, buffer: &mut FileBuffer) -> Result<()> {
        let location = self.block_location(block_id)?;
        self.handle.read_at(buffer.internal_mut(), location)
    }

    fn write(&self, block_id: BlockId, buffer: &FileBuffer) -> Result<()> {
        // SAFETY: blocks are written while no one modifies them (checkpoints write
        // blocks they own).
        let bytes = unsafe { buffer.internal() };
        self.write_block(block_id, &bytes[DEFAULT_BLOCK_HEADER_SIZE..])
    }

    fn registry(&self) -> &BlockRegistry {
        &self.registry
    }

    /// `UnregisterBlock`: a freed block whose handle went away becomes reusable.
    fn unregister_block(&self, block_id: BlockId) {
        self.registry().unregister(block_id);
        let mut state = self.lock();
        if state.free_blocks_in_use.remove(&block_id) {
            state.free_list.insert(block_id);
        }
    }
}

/// Whether `path` looks like a database file that exists.
pub fn database_exists(path: &str) -> bool {
    Path::new(path).exists()
}
