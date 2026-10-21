//! The buffer manager of a database (DuckDB's `StandardBufferManager`).

use std::alloc::Layout;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use rustdb_common::{Error, Result};

use crate::allocator::BlockAllocator;
use crate::block::{BlockHandle, BlockId, BlockMemory, BufferHandle, DestroyBufferUpon};
use crate::block_manager::BlockManager;
use crate::bytes::format_bytes;
use crate::error::{io_error, out_of_memory};
use crate::file_buffer::{FileBuffer, FileBufferType, aligned_alloc_size};
use crate::memory::MemoryReservation;
use crate::memory_tag::{MEMORY_TAG_COUNT, MemoryTag};
use crate::os;
use crate::pool::BufferPool;
use crate::temp_file::{TemporaryDirectoryHandle, TemporaryFileInformation, read_exact_at, write_all_at};
use crate::temporary_memory::{TemporaryMemoryContext, TemporaryMemoryManager};
use crate::{DEFAULT_BLOCK_ALLOC_SIZE, MAXIMUM_BLOCK, TEMPORARY_BLOCK_HEADER_SIZE};

/// Memory and spilling configuration of a buffer manager.
#[derive(Clone, Debug, Default)]
pub struct BufferManagerConfig {
    /// `memory_limit`; `None` uses DuckDB's default, 80% of the physical memory.
    pub memory_limit: Option<u64>,
    /// `temp_directory`; `None` disables spilling to disk.
    pub temp_directory: Option<PathBuf>,
    /// `max_temp_directory_size`; `None` means 90% of the free disk space.
    pub max_temp_directory_size: Option<u64>,
    /// Record when blocks were last unpinned (for age-based eviction).
    pub track_eviction_timestamps: bool,
    /// Bytes of freed blocks the allocator keeps for reuse; `None` picks a default.
    pub allocator_retention: Option<usize>,
}

/// The default memory limit: 80% of physical memory (`DBConfig::SetDefaultMaxMemory`).
pub fn default_memory_limit() -> u64 {
    // DuckDB falls back to 8 GiB when the physical memory is unknown.
    os::physical_memory().map_or(8 << 30, |memory| memory / 10 * 8)
}

/// Memory used by one tag (a row of `duckdb_memory()`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryInformation {
    pub tag: MemoryTag,
    /// Bytes in memory.
    pub size: u64,
    /// Bytes written to temporary storage.
    pub evicted_data: u64,
}

const IN_MEMORY_WARNING: &str = "\nDatabase is launched in in-memory mode and no temporary directory is specified.\
                                 \nUnused blocks cannot be offloaded to disk.\
                                 \n\nLaunch the database with a persistent storage back-end\
                                 \nOr set SET temp_directory='/path/to/tmp.tmp'";

/// The temporary directory: configured lazily and created on first use.
struct TemporaryDirectory {
    path: RwLock<String>,
    size_on_disk: Arc<AtomicU64>,
    handle: Mutex<Option<Arc<TemporaryDirectoryHandle>>>,
    max_swap_space: Mutex<Option<u64>>,
}

/// State shared by a buffer manager and all of its blocks.
pub(crate) struct ManagerShared {
    pool: Arc<BufferPool>,
    temporary_directory: TemporaryDirectory,
    temporary_id: AtomicI64,
    evicted_data_per_tag: [AtomicU64; MEMORY_TAG_COUNT],
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ManagerShared {
    pub(crate) fn pool(&self) -> &Arc<BufferPool> {
        &self.pool
    }

    fn temp_path(&self) -> String {
        self.temporary_directory
            .path
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn has_temporary_directory(&self) -> bool {
        !self.temp_path().is_empty()
    }

    fn in_memory_warning(&self) -> &'static str {
        if self.has_temporary_directory() {
            ""
        } else {
            IN_MEMORY_WARNING
        }
    }

    fn used_memory(&self) -> u64 {
        self.pool.used_memory(true)
    }

    /// Evicts blocks until `size` more bytes fit, or fails with DuckDB's out-of-memory
    /// error (`EvictBlocksOrThrow`). `describe` renders the start of the message.
    fn evict_or_throw(
        &self,
        tag: MemoryTag,
        size: u64,
        reuse: bool,
        describe: impl FnOnce() -> String,
    ) -> Result<(MemoryReservation, Option<FileBuffer>)> {
        let eviction = self.pool.evict_blocks(tag, size, self.pool.max_memory(), reuse)?;
        if !eviction.success {
            return Err(out_of_memory(format!(
                "{} ({}/{} used){}",
                describe(),
                format_bytes(self.used_memory()),
                format_bytes(self.pool.max_memory()),
                self.in_memory_warning()
            )));
        }
        Ok((eviction.reservation, eviction.reusable))
    }

    /// A managed buffer for `size` user bytes, reusing `reusable` when given
    /// (`ConstructManagedBuffer`).
    fn construct_managed_buffer(
        &self,
        size: usize,
        header_size: usize,
        reusable: Option<FileBuffer>,
        kind: FileBufferType,
    ) -> Result<FileBuffer> {
        match reusable {
            Some(buffer) if buffer.alloc_size() == aligned_alloc_size(size, header_size) => {
                Ok(FileBuffer::reuse(buffer, kind, header_size))
            }
            _ => FileBuffer::allocate(self.pool.allocator(), kind, size, header_size),
        }
    }

    /// A buffer to load a persistent block of `block_manager` into.
    pub(crate) fn create_block_buffer(
        &self,
        block_manager: &dyn BlockManager,
        reusable: Option<FileBuffer>,
    ) -> Result<FileBuffer> {
        self.construct_managed_buffer(
            block_manager.block_size(),
            block_manager.block_header_size(),
            reusable,
            FileBufferType::Block,
        )
    }

    /// The temporary directory handle, creating the directory on first use
    /// (`RequireTemporaryDirectory`).
    fn require_temporary_directory(&self) -> Result<Arc<TemporaryDirectoryHandle>> {
        let path = self.temp_path();
        if path.is_empty() {
            return Err(Error::invalid_input(
                "Out-of-memory: cannot write buffer because no temporary directory is specified!\nTo enable \
                 temporary buffer eviction set a temporary directory using PRAGMA temp_directory='/path/to/tmp.tmp'",
            ));
        }
        let mut handle = lock(&self.temporary_directory.handle);
        if let Some(handle) = &*handle {
            return Ok(Arc::clone(handle));
        }
        let max_swap = *lock(&self.temporary_directory.max_swap_space);
        let created = Arc::new(TemporaryDirectoryHandle::new(
            PathBuf::from(path),
            Arc::clone(&self.temporary_directory.size_on_disk),
            max_swap,
        )?);
        *handle = Some(Arc::clone(&created));
        Ok(created)
    }

    fn temporary_handle(&self) -> Option<Arc<TemporaryDirectoryHandle>> {
        lock(&self.temporary_directory.handle).clone()
    }

    fn temporary_block_path(&self, block_id: BlockId) -> PathBuf {
        Path::new(&self.temp_path()).join(format!("duckdb_temp_block-{block_id}.block"))
    }

    fn evicted(&self, tag: MemoryTag) -> &AtomicU64 {
        &self.evicted_data_per_tag[tag.index()]
    }

    /// Writes an evicted buffer to temporary storage (`WriteTemporaryBuffer`).
    pub(crate) fn write_temporary_buffer(
        &self,
        tag: MemoryTag,
        block_id: BlockId,
        buffer: &FileBuffer,
    ) -> Result<()> {
        let handle = self.require_temporary_directory()?;
        if buffer.alloc_size() == DEFAULT_BLOCK_ALLOC_SIZE {
            let size = handle.manager.write_buffer(block_id, buffer)?;
            self.evicted(tag).fetch_add(size, Ordering::Relaxed);
            return Ok(());
        }
        // Other sizes go into a file of their own: [size][header size][buffer]. As in
        // DuckDB, the file is created before the disk space is reserved: when the
        // reservation fails, the empty file stays until the block is destroyed.
        let path = self.temporary_block_path(block_id);
        let file_size = buffer.alloc_size() as u64 + 16;
        self.evicted(tag)
            .fetch_add(buffer.alloc_size() as u64, Ordering::Relaxed);
        let write_error = |err: std::io::Error| io_error(format!("Could not write file \"{}\": {err}", path.display()));
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(write_error)?;
        handle.manager.disk.increase(file_size)?;
        let write = || -> std::io::Result<()> {
            write_all_at(&file, &(buffer.size() as u64).to_le_bytes(), 0)?;
            write_all_at(&file, &(buffer.header_size() as u64).to_le_bytes(), 8)?;
            // SAFETY: the buffer is being evicted: no readers, lock held.
            write_all_at(&file, unsafe { buffer.internal() }, 16)
        };
        if let Err(err) = write() {
            handle.manager.disk.decrease(file_size);
            self.evicted(tag)
                .fetch_sub(buffer.alloc_size() as u64, Ordering::Relaxed);
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(write_error(err));
        }
        Ok(())
    }

    /// Reads an evicted buffer back and frees its temporary storage
    /// (`ReadTemporaryBuffer`).
    pub(crate) fn read_temporary_buffer(
        &self,
        tag: MemoryTag,
        block_id: BlockId,
        reusable: Option<FileBuffer>,
    ) -> Result<FileBuffer> {
        let Some(handle) = self.temporary_handle() else {
            return Err(Error::internal(
                "ReadTemporaryBuffer called but temporary directory has not been instantiated yet",
            ));
        };
        if handle.manager.has_buffer(block_id) {
            let (buffer, size) = handle
                .manager
                .read_buffer(block_id, self.pool.allocator(), reusable)?;
            self.evicted(tag).fetch_sub(size, Ordering::Relaxed);
            return Ok(buffer);
        }
        let path = self.temporary_block_path(block_id);
        let read_error =
            |err: std::io::Error| io_error(format!("Could not read from file \"{}\": {err}", path.display()));
        let file = OpenOptions::new().read(true).open(&path).map_err(read_error)?;
        let mut header = [0u8; 16];
        read_exact_at(&file, &mut header, 0).map_err(read_error)?;
        let (size_bytes, header_bytes) = header.split_at(8);
        let block_size = u64::from_le_bytes(size_bytes.try_into().unwrap_or_default());
        let block_header_size = u64::from_le_bytes(header_bytes.try_into().unwrap_or_default());
        let file_size = file.metadata().map_err(read_error)?.len();
        if block_header_size > file_size || block_size > file_size {
            return Err(io_error(format!(
                "Corrupt temporary file \"{}\": block header size ({block_header_size}) or block size ({block_size}) \
                 exceeds file size {file_size}",
                path.display()
            )));
        }
        let expected = 16 + aligned_alloc_size(block_size as usize, block_header_size as usize) as u64;
        if expected != file_size {
            return Err(io_error(format!(
                "Corrupt temporary file \"{}\": header describes {expected} bytes but file is {file_size}",
                path.display()
            )));
        }
        let mut buffer = self.construct_managed_buffer(
            block_size as usize,
            block_header_size as usize,
            reusable,
            FileBufferType::ManagedBuffer,
        )?;
        read_exact_at(&file, buffer.internal_mut(), 16).map_err(read_error)?;
        drop(file);
        self.delete_temporary_file(block_id, tag, buffer.alloc_size() as u64)?;
        Ok(buffer)
    }

    /// Deletes the temporary storage of an evicted block (`DeleteTemporaryFile`).
    pub(crate) fn delete_temporary_file(
        &self,
        block_id: BlockId,
        tag: MemoryTag,
        memory_usage: u64,
    ) -> Result<()> {
        if !self.has_temporary_directory() {
            return Ok(());
        }
        let Some(handle) = self.temporary_handle() else {
            return Ok(());
        };
        if handle.manager.has_buffer(block_id) {
            let size = handle.manager.delete_buffer(block_id)?;
            self.evicted(tag).fetch_sub(size, Ordering::Relaxed);
            return Ok(());
        }
        let path = self.temporary_block_path(block_id);
        if let Ok(metadata) = std::fs::metadata(&path) {
            self.evicted(tag).fetch_sub(memory_usage, Ordering::Relaxed);
            std::fs::remove_file(&path)
                .map_err(|err| io_error(format!("Could not remove file \"{}\": {err}", path.display())))?;
            handle.manager.disk.decrease(metadata.len());
        }
        Ok(())
    }

    /// The slow path of unpinning: the last reader may queue the block for eviction.
    pub(crate) fn unpin(&self, block: &Arc<BlockHandle>) {
        let memory = &block.memory;
        let purge = {
            let mut inner = memory.lock();
            if inner.buffer.is_none() || memory.buffer_type() == FileBufferType::TinyBuffer {
                return;
            }
            if memory.remove_reader_locked() != 0 {
                return;
            }
            if memory.releases_on_unpin() {
                // Persistent data: reloaded from the file when pinned again.
                let _ = memory.unload(&mut inner);
                return;
            }
            if memory.must_add_to_eviction_queue() {
                self.pool.add_to_eviction_queue(memory, &mut inner)
            } else {
                // Destroyed on unpin: nothing is written, so unloading cannot fail.
                let _ = memory.unload(&mut inner);
                false
            }
        };
        if purge {
            self.pool.purge_queue(memory);
        }
    }
}

/// The buffer manager of a database: allocates, pins and evicts buffer-managed memory.
/// Cheap to clone; clones share the same state.
#[derive(Clone)]
pub struct BufferManager {
    shared: Arc<ManagerShared>,
}

impl std::fmt::Debug for BufferManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferManager")
            .field("used_memory", &self.used_memory())
            .field("max_memory", &self.max_memory())
            .field("temp_directory", &self.temporary_directory())
            .finish()
    }
}

impl BufferManager {
    /// A buffer manager with its own buffer pool.
    pub fn new(config: BufferManagerConfig) -> Result<Self> {
        let memory_limit = config.memory_limit.unwrap_or_else(default_memory_limit);
        let retention = config
            .allocator_retention
            .unwrap_or_else(|| (memory_limit / 16).clamp(4 << 20, 64 << 20) as usize);
        let allocator = Arc::new(BlockAllocator::new(DEFAULT_BLOCK_ALLOC_SIZE, retention));
        let pool = BufferPool::new(memory_limit, allocator, config.track_eviction_timestamps);
        let manager = Self::with_pool(pool, config.temp_directory);
        if config.max_temp_directory_size.is_some() {
            manager.set_swap_limit(config.max_temp_directory_size)?;
        }
        Ok(manager)
    }

    /// A buffer manager sharing `pool` with other databases.
    pub fn with_pool(pool: Arc<BufferPool>, temp_directory: Option<PathBuf>) -> Self {
        let path = temp_directory
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        Self {
            shared: Arc::new(ManagerShared {
                pool,
                temporary_directory: TemporaryDirectory {
                    path: RwLock::new(path),
                    size_on_disk: Arc::new(AtomicU64::new(0)),
                    handle: Mutex::new(None),
                    max_swap_space: Mutex::new(None),
                },
                temporary_id: AtomicI64::new(MAXIMUM_BLOCK),
                evicted_data_per_tag: std::array::from_fn(|_| AtomicU64::new(0)),
            }),
        }
    }

    pub fn pool(&self) -> &Arc<BufferPool> {
        &self.shared.pool
    }

    /// Allocation size of a temporary block.
    pub fn block_alloc_size(&self) -> usize {
        DEFAULT_BLOCK_ALLOC_SIZE
    }

    /// Usable size of a temporary block.
    pub fn block_size(&self) -> usize {
        DEFAULT_BLOCK_ALLOC_SIZE - TEMPORARY_BLOCK_HEADER_SIZE
    }

    /// Bytes currently charged (exact).
    pub fn used_memory(&self) -> u64 {
        self.shared.used_memory()
    }

    /// Bytes currently charged, without folding in the per-thread caches (DuckDB's
    /// `BufferPool::GetUsedMemory(false)`).
    pub fn used_memory_estimate(&self) -> u64 {
        self.shared.pool.used_memory(false)
    }

    /// The memory limit.
    pub fn max_memory(&self) -> u64 {
        self.shared.pool.max_memory()
    }

    /// The per-query memory limit.
    pub fn query_max_memory(&self) -> u64 {
        self.shared.pool.query_max_memory()
    }

    /// Bytes currently used in the temporary directory.
    pub fn used_swap(&self) -> u64 {
        self.shared
            .temporary_directory
            .size_on_disk
            .load(Ordering::Relaxed)
    }

    /// The effective `max_temp_directory_size`, once the temporary directory is in use.
    pub fn max_swap(&self) -> Option<u64> {
        self.shared
            .temporary_handle()
            .map(|handle| handle.manager.disk.max())
    }

    /// Changes the memory limit, evicting as needed (`SET memory_limit`).
    pub fn set_memory_limit(&self, limit: u64) -> Result<()> {
        self.shared.pool.set_limit(limit, self.shared.in_memory_warning())
    }

    /// Changes `max_temp_directory_size`; `None` restores the default (90% of free disk).
    pub fn set_swap_limit(&self, limit: Option<u64>) -> Result<()> {
        let handle = lock(&self.shared.temporary_directory.handle);
        if let Some(handle) = &*handle {
            return handle.manager.set_max_swap_space(limit);
        }
        *lock(&self.shared.temporary_directory.max_swap_space) = limit;
        Ok(())
    }

    /// The temporary directory (empty when spilling is disabled).
    pub fn temporary_directory(&self) -> String {
        self.shared.temp_path()
    }

    /// Changes the temporary directory; not possible once it has been used.
    pub fn set_temporary_directory(&self, path: &str) -> Result<()> {
        let handle = lock(&self.shared.temporary_directory.handle);
        if handle.is_some() {
            return Err(Error::not_implemented(
                "Cannot switch temporary directory after the current one has been used",
            ));
        }
        path.clone_into(
            &mut self
                .shared
                .temporary_directory
                .path
                .write()
                .unwrap_or_else(PoisonError::into_inner),
        );
        Ok(())
    }

    pub fn has_temporary_directory(&self) -> bool {
        self.shared.has_temporary_directory()
    }

    /// Whether the temporary directory contains spilled data files.
    pub fn has_files_in_temporary_directory(&self) -> bool {
        let path = self.temporary_directory();
        if path.is_empty() {
            return false;
        }
        std::fs::read_dir(path).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                entry.file_type().is_ok_and(|kind| kind.is_file())
                    && (name.ends_with(".block") || name.ends_with(".tmp"))
            })
        })
    }

    /// Memory and spilled bytes per tag (`duckdb_memory()`).
    pub fn memory_usage_info(&self) -> Vec<MemoryInformation> {
        MemoryTag::ALL
            .iter()
            .map(|&tag| MemoryInformation {
                tag,
                size: self.shared.pool.used_memory_by_tag(tag),
                evicted_data: self.shared.evicted(tag).load(Ordering::Relaxed),
            })
            .collect()
    }

    /// The temporary files in use (`duckdb_temporary_files()`).
    pub fn temporary_files(&self) -> Vec<TemporaryFileInformation> {
        let path = self.temporary_directory();
        if path.is_empty() {
            return Vec::new();
        }
        let mut result = self
            .shared
            .temporary_handle()
            .map(|handle| handle.manager.temporary_files())
            .unwrap_or_default();
        if let Ok(entries) = std::fs::read_dir(&path) {
            for entry in entries.flatten() {
                let is_block = entry.file_name().to_string_lossy().ends_with(".block");
                if let (true, Ok(metadata)) = (is_block, entry.metadata())
                    && metadata.is_file()
                {
                    result.push(TemporaryFileInformation {
                        path: entry.path().display().to_string(),
                        size: metadata.len(),
                    });
                }
            }
        }
        result
    }

    /// Allocates a temporary block of `size` bytes and pins it (`Allocate`).
    ///
    /// With `can_destroy`, the data is dropped instead of spilled when the block is
    /// evicted while unpinned (the next pin then fails).
    pub fn allocate(&self, tag: MemoryTag, size: usize, can_destroy: bool) -> Result<BufferHandle> {
        let block = self.register_memory(tag, size, TEMPORARY_BLOCK_HEADER_SIZE, can_destroy)?;
        self.pin(&block)
    }

    /// Allocates an unpinned temporary block of `size` bytes (`AllocateTemporaryMemory`).
    pub fn allocate_temporary_memory(
        &self,
        tag: MemoryTag,
        size: usize,
        can_destroy: bool,
    ) -> Result<Arc<BlockHandle>> {
        self.register_memory(tag, size, TEMPORARY_BLOCK_HEADER_SIZE, can_destroy)
    }

    /// Allocates and pins a temporary block shaped like `block_manager`'s blocks, so it
    /// can later be converted into a persistent block (`Allocate(tag, block_manager)`).
    pub fn allocate_for(
        &self,
        tag: MemoryTag,
        block_manager: &dyn BlockManager,
        can_destroy: bool,
    ) -> Result<BufferHandle> {
        let block = self.register_memory(
            tag,
            block_manager.block_size(),
            block_manager.block_header_size(),
            can_destroy,
        )?;
        self.pin(&block)
    }

    /// `RegisterMemory`: reserves memory (evicting if needed) and creates a loaded block.
    fn register_memory(
        &self,
        tag: MemoryTag,
        size: usize,
        header_size: usize,
        can_destroy: bool,
    ) -> Result<Arc<BlockHandle>> {
        let alloc_size = aligned_alloc_size(size, header_size);
        let (reservation, reusable) = self.shared.evict_or_throw(tag, alloc_size as u64, true, || {
            format!(
                "could not allocate block of size {}",
                format_bytes(alloc_size as u64)
            )
        })?;
        let kind = if tag == MemoryTag::ExternalFileCache {
            FileBufferType::ExternalFile
        } else {
            FileBufferType::ManagedBuffer
        };
        let buffer = self
            .shared
            .construct_managed_buffer(size, header_size, reusable, kind)?;
        let destroy_upon = if can_destroy {
            DestroyBufferUpon::Eviction
        } else {
            DestroyBufferUpon::Block
        };
        let block_id = self.next_temporary_id();
        let memory = BlockMemory::loaded(
            Arc::clone(&self.shared),
            block_id,
            tag,
            buffer,
            destroy_upon,
            alloc_size as u64,
            reservation,
        );
        Ok(Arc::new(BlockHandle::new(block_id, None, memory)))
    }

    /// Registers memory smaller than a block that is never evicted; pinning and unpinning
    /// it is free (`RegisterSmallMemory`).
    pub fn register_small_memory(&self, tag: MemoryTag, size: usize) -> Result<Arc<BlockHandle>> {
        let (reservation, _) = self.shared.evict_or_throw(tag, size as u64, false, || {
            format!("could not allocate block of size {}", format_bytes(size as u64))
        })?;
        let buffer = FileBuffer::allocate(self.shared.pool.allocator(), FileBufferType::TinyBuffer, size, 0)?;
        let block_id = self.next_temporary_id();
        let memory = BlockMemory::loaded(
            Arc::clone(&self.shared),
            block_id,
            tag,
            buffer,
            DestroyBufferUpon::Block,
            size as u64,
            reservation,
        );
        Ok(Arc::new(BlockHandle::new(block_id, None, memory)))
    }

    fn next_temporary_id(&self) -> BlockId {
        self.shared.temporary_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Pins a block, loading it if necessary (`Pin`). Fails if the block's data was
    /// destroyed on eviction (see [`BufferManager::try_pin`]).
    pub fn pin(&self, block: &Arc<BlockHandle>) -> Result<BufferHandle> {
        self.try_pin(block)?.ok_or_else(|| {
            Error::internal(format!(
                "Pinning block {} failed: its data was destroyed on eviction",
                block.block_id()
            ))
        })
    }

    /// Pins a block; `None` if it was evicted and its data destroyed (blocks allocated
    /// with `can_destroy`).
    pub fn try_pin(&self, block: &Arc<BlockHandle>) -> Result<Option<BufferHandle>> {
        if let Some(handle) = BufferHandle::try_pin_fast(block) {
            return Ok(Some(handle));
        }
        let memory = &block.memory;
        let required = {
            let inner = memory.lock();
            if memory.is_loaded() {
                let pinned = memory.add_reader_locked();
                drop(inner);
                return Ok(pinned.map(|(ptr, len)| BufferHandle::new(Arc::clone(block), ptr, len)));
            }
            memory.memory_usage()
        };
        let (reservation, reusable) = self.shared.evict_or_throw(memory.tag(), required, true, || {
            format!("failed to pin block of size {}", format_bytes(required))
        })?;
        let mut inner = memory.lock();
        if memory.is_loaded() {
            // Someone else loaded it in the meantime.
            drop(reservation);
            let pinned = memory.add_reader_locked();
            drop(inner);
            return Ok(pinned.map(|(ptr, len)| BufferHandle::new(Arc::clone(block), ptr, len)));
        }
        let Some((ptr, len)) = block.load(&mut inner, reusable)? else {
            return Ok(None);
        };
        inner.charge = reservation;
        // Variable-size blocks may load into a smaller buffer than reserved.
        let alloc_size = inner
            .buffer
            .as_ref()
            .map_or(required, |buffer| buffer.alloc_size() as u64);
        if alloc_size != required {
            memory.set_memory_usage(&mut inner, alloc_size);
        }
        drop(inner);
        Ok(Some(BufferHandle::new(Arc::clone(block), ptr, len)))
    }

    /// Loads unloaded persistent blocks ahead of use (`Prefetch`); a hint only.
    pub fn prefetch(&self, blocks: &[Arc<BlockHandle>]) -> Result<()> {
        for block in blocks {
            if block.block_manager().is_some() && !block.memory.is_loaded() {
                drop(self.pin(block)?);
            }
        }
        Ok(())
    }

    /// Charges `size` bytes to the `EXTENSION` tag without allocating, evicting if needed
    /// (`ReserveMemory`). Release with [`BufferManager::free_reserved_memory`].
    pub fn reserve_memory(&self, size: u64) -> Result<()> {
        if size == 0 {
            return Ok(());
        }
        let (reservation, _) = self
            .shared
            .evict_or_throw(MemoryTag::Extension, size, false, || {
                format!("failed to reserve memory data of size {}", format_bytes(size))
            })?;
        reservation.keep_charged();
        Ok(())
    }

    /// Charges `size` bytes to `tag` for memory owned outside the buffer pool (such as the
    /// buffers of an index's fixed-size allocators), evicting if needed. The charge is
    /// released when the returned reservation is dropped.
    pub fn reserve_tagged(&self, tag: MemoryTag, size: u64) -> Result<MemoryReservation> {
        let (reservation, _) = self.shared.evict_or_throw(tag, size, false, || {
            format!("failed to allocate data of size {}", format_bytes(size))
        })?;
        Ok(reservation)
    }

    /// Releases memory charged with [`BufferManager::reserve_memory`].
    pub fn free_reserved_memory(&self, size: u64) {
        if size != 0 {
            self.shared
                .pool
                .update_used_memory(MemoryTag::Extension, -(size as i64));
        }
    }

    /// Allocates `size` bytes, charged to the `ALLOCATOR` tag (DuckDB's buffer allocator).
    /// Fails with an out-of-memory error if they don't fit.
    pub fn allocate_data(&self, size: usize) -> Result<AllocatedData> {
        let (reservation, _) =
            self.shared
                .evict_or_throw(MemoryTag::Allocator, size as u64, false, || {
                    format!("failed to allocate data of size {}", format_bytes(size as u64))
                })?;
        AllocatedData::allocate(size, Some(reservation), false)
    }

    /// The manager assigning memory to concurrently running operators.
    pub fn temporary_memory_manager(&self) -> &TemporaryMemoryManager {
        self.shared.pool.temporary_memory_manager()
    }

    /// A [`TemporaryMemoryContext`] describing this manager, for the given parallelism.
    pub fn temporary_memory_context(
        &self,
        threads: u64,
        connections: u64,
        force_external: bool,
    ) -> TemporaryMemoryContext {
        TemporaryMemoryContext {
            memory_limit: self.max_memory(),
            query_max_memory: self.query_max_memory(),
            has_temporary_directory: self.has_temporary_directory(),
            threads,
            connections,
            force_external,
        }
    }

    /// Returns memory kept by the block allocator for reuse to the OS (call when a
    /// pipeline finishes).
    pub fn flush_allocator(&self) {
        self.shared.pool.allocator().flush();
    }

    /// The handle of persistent block `block_id` of `block_manager`, creating an unloaded
    /// one if it has none (`BlockManager::RegisterBlock`).
    pub fn register_block(
        &self,
        block_manager: &Arc<dyn BlockManager>,
        block_id: BlockId,
    ) -> Arc<BlockHandle> {
        block_manager.registry().get_or_register(block_id, || {
            let memory = BlockMemory::unloaded(
                Arc::clone(&self.shared),
                block_id,
                MemoryTag::BaseTable,
                block_manager.block_alloc_size(),
            );
            Arc::new(BlockHandle::new(
                block_id,
                Some(Arc::clone(block_manager)),
                memory,
            ))
        })
    }

    /// Turns a pinned temporary block into persistent block `block_id`: its buffer is
    /// written through `block_manager` and moved into the new handle, without copying
    /// (`BlockManager::ConvertToPersistent`, destructive mode). The old block must have no
    /// other pins.
    pub fn convert_to_persistent(
        &self,
        block_manager: &Arc<dyn BlockManager>,
        block_id: BlockId,
        old_block: Arc<BlockHandle>,
        old_pin: BufferHandle,
    ) -> Result<Arc<BlockHandle>> {
        let new_block = self.register_block(block_manager, block_id);
        {
            let old_memory = &old_block.memory;
            let mut old_inner = old_memory.lock();
            if old_memory.readers() > 1 {
                return Err(Error::internal(format!(
                    "BlockManager::ConvertToPersistent in destructive mode - cannot be called for block {block_id} \
                     as old_block has multiple readers active"
                )));
            }
            let Some(buffer) = old_inner.buffer.take() else {
                return Err(Error::internal("ConvertToPersistent called on an unloaded block"));
            };
            if buffer.alloc_size() > block_manager.block_alloc_size() {
                old_inner.buffer = Some(buffer);
                return Err(Error::internal(
                    "temporary buffer is larger than a persistent block",
                ));
            }
            let buffer = FileBuffer::reuse(buffer, FileBufferType::Block, block_manager.block_header_size());
            if let Err(error) = block_manager.write(block_id, &buffer) {
                old_inner.buffer = Some(buffer);
                return Err(error);
            }
            // Move buffer and charge to the new block.
            let mut charge = std::mem::replace(
                &mut old_inner.charge,
                MemoryReservation::empty(Arc::clone(&self.shared.pool), old_memory.tag()),
            );
            charge.retag(new_block.tag());
            let usage = charge.size();
            old_memory.set_memory_usage(&mut old_inner, 0);
            let mut unloaded = old_inner;
            let _ = old_memory.unload(&mut unloaded);
            let new_memory = &new_block.memory;
            let mut new_inner = new_memory.lock();
            new_memory.install(&mut new_inner, buffer);
            new_inner.charge = charge;
            new_memory.set_memory_usage(&mut new_inner, usage);
        }
        // The old pin refers to the moved buffer; release it before anyone pins anew.
        drop(old_pin);
        drop(old_block);
        let purge = {
            let mut inner = new_block.memory.lock();
            self.shared
                .pool
                .add_to_eviction_queue(&new_block.memory, &mut inner)
        };
        if purge {
            self.shared.pool.purge_queue(&new_block.memory);
        }
        Ok(new_block)
    }
}

/// Memory charged to the `ALLOCATOR` tag (DuckDB's `AllocatedData` from the buffer
/// allocator). Freed and released when dropped.
///
/// Allocations of at least [`MAP_THRESHOLD`] bytes are page mappings of their own: the
/// process allocator would keep them resident after they are freed (a hash table that
/// doubled its pointer table a few times held all the smaller tables as well).
pub struct AllocatedData {
    ptr: NonNull<u8>,
    layout: Layout,
    _reservation: Option<MemoryReservation>,
}

/// Allocations from this size on are mapped from the OS directly.
const MAP_THRESHOLD: usize = 1 << 20;

// SAFETY: `AllocatedData` uniquely owns its allocation.
unsafe impl Send for AllocatedData {}
// SAFETY: shared access only exposes a raw pointer.
unsafe impl Sync for AllocatedData {}

impl std::fmt::Debug for AllocatedData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllocatedData")
            .field("len", &self.layout.size())
            .finish_non_exhaustive()
    }
}

impl AllocatedData {
    fn allocate(size: usize, reservation: Option<MemoryReservation>, zeroed: bool) -> Result<Self> {
        let error = || {
            out_of_memory(format!(
                "failed to allocate data of size {}",
                format_bytes(size as u64)
            ))
        };
        if size >= MAP_THRESHOLD {
            // Mappings start zeroed.
            let layout = Layout::from_size_align(size.next_multiple_of(os::PAGE_SIZE), os::PAGE_SIZE)
                .map_err(|_| error())?;
            let ptr = os::map(layout.size()).ok_or_else(error)?;
            return Ok(Self {
                ptr,
                layout,
                _reservation: reservation,
            });
        }
        let layout = Layout::from_size_align(size.max(1), 16).map_err(|_| error())?;
        // SAFETY: `layout` has non-zero size.
        let ptr = NonNull::new(unsafe {
            if zeroed {
                std::alloc::alloc_zeroed(layout)
            } else {
                std::alloc::alloc(layout)
            }
        })
        .ok_or_else(|| {
            out_of_memory(format!(
                "failed to allocate data of size {}",
                format_bytes(size as u64)
            ))
        })?;
        Ok(Self {
            ptr,
            layout,
            _reservation: reservation,
        })
    }

    /// Memory that is not charged to any buffer manager.
    pub fn untracked(size: usize) -> Result<Self> {
        Self::allocate(size, None, false)
    }

    /// Zeroed memory that is not charged to any buffer manager.
    pub fn untracked_zeroed(size: usize) -> Result<Self> {
        Self::allocate(size, None, true)
    }

    /// Start of the memory (uninitialised unless allocated zeroed), 16-byte aligned.
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    pub fn len(&self) -> usize {
        self.layout.size()
    }

    pub fn is_empty(&self) -> bool {
        self.layout.size() == 0
    }
}

impl Drop for AllocatedData {
    fn drop(&mut self) {
        if self.layout.size() >= MAP_THRESHOLD {
            // SAFETY: mapped in `allocate` with this size; nothing uses it any more.
            unsafe { os::unmap(self.ptr, self.layout.size()) };
        } else {
            // SAFETY: allocated in `allocate` with `self.layout`.
            unsafe { std::alloc::dealloc(self.ptr.as_ptr(), self.layout) };
        }
    }
}
