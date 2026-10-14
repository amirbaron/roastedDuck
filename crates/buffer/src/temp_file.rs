//! Temporary files for evicted buffers (DuckDB's `TemporaryFileManager`).
//!
//! Buffers of the default block size are written into shared files named
//! `duckdb_temp_storage_<SIZE>-<index>.tmp`, one slot per buffer. Freed slots are reused,
//! and files shrink (truncate) when their last slots are freed and disappear when empty.
//! Every growth is checked against `max_temp_directory_size`.
//!
//! DuckDB additionally zstd-compresses buffers into smaller size classes (`S32K` ...
//! `S224K`); without a pure-Rust zstd encoder buffers are always stored uncompressed in
//! the `DEFAULT` class (the format is internal to one process).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_common::{Error, Result};

use crate::DEFAULT_BLOCK_ALLOC_SIZE;
use crate::allocator::BlockAllocator;
use crate::block::BlockId;
use crate::bytes::format_bytes;
use crate::error::{io_error, out_of_memory};
use crate::file_buffer::{FileBuffer, FileBufferType};
use crate::os;

/// Slot size classes of temporary files (DuckDB's `TemporaryBufferSize`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TemporaryBufferSize {
    S32K,
    S64K,
    S96K,
    S128K,
    S160K,
    S192K,
    S224K,
    Default,
}

impl TemporaryBufferSize {
    /// Slot size in bytes.
    pub const fn bytes(self) -> u64 {
        match self {
            Self::S32K => 32 << 10,
            Self::S64K => 64 << 10,
            Self::S96K => 96 << 10,
            Self::S128K => 128 << 10,
            Self::S160K => 160 << 10,
            Self::S192K => 192 << 10,
            Self::S224K => 224 << 10,
            Self::Default => DEFAULT_BLOCK_ALLOC_SIZE as u64,
        }
    }

    /// The name used in file names (`EnumUtil::ToString`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::S32K => "S32K",
            Self::S64K => "S64K",
            Self::S96K => "S96K",
            Self::S128K => "S128K",
            Self::S160K => "S160K",
            Self::S192K => "S192K",
            Self::S224K => "S224K",
            Self::Default => "DEFAULT",
        }
    }
}

/// Name and size of one temporary file (`duckdb_temporary_files()`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporaryFileInformation {
    pub path: String,
    pub size: u64,
}

/// Disk usage of a temporary directory and its limit.
#[derive(Debug)]
pub(crate) struct DiskUsage {
    size_on_disk: Arc<AtomicU64>,
    max_swap_space: AtomicU64,
}

impl DiskUsage {
    pub(crate) fn used(&self) -> u64 {
        self.size_on_disk.load(Ordering::Relaxed)
    }

    pub(crate) fn max(&self) -> u64 {
        self.max_swap_space.load(Ordering::Relaxed)
    }

    /// Records `bytes` more on disk, failing if that exceeds `max_temp_directory_size`.
    pub(crate) fn increase(&self, bytes: u64) -> Result<()> {
        let current = self.used();
        let max = self.max();
        if current.saturating_add(bytes) > max {
            return Err(out_of_memory(format!(
                "failed to offload data block of size {} ({}/{} used).\n\
                 This limit was set by the 'max_temp_directory_size' setting.\n\
                 By default, this setting utilizes the available disk space on the drive where the 'temp_directory' \
                 is located.\n\
                 You can adjust this setting, by using (for example) PRAGMA max_temp_directory_size='10GiB'",
                format_bytes(bytes),
                format_bytes(current),
                format_bytes(max)
            )));
        }
        self.size_on_disk.fetch_add(bytes, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) fn decrease(&self, bytes: u64) {
        self.size_on_disk.fetch_sub(bytes, Ordering::Relaxed);
    }
}

/// Allocates slot indexes within a file (or file indexes within a size class), always
/// handing out the lowest free index (DuckDB's `BlockIndexManager`).
#[derive(Debug, Default)]
struct BlockIndexManager {
    max_index: u64,
    free_indexes: BTreeSet<u64>,
    indexes_in_use: BTreeSet<u64>,
}

impl BlockIndexManager {
    /// A new index; `disk` (if any) is charged for file growth of `slot_size` per index.
    fn new_index(&mut self, slot_size: u64, disk: Option<&DiskUsage>) -> Result<u64> {
        let index = if let Some(&index) = self.free_indexes.first() {
            self.free_indexes.remove(&index);
            index
        } else {
            let index = self.max_index;
            self.set_max_index(index + 1, slot_size, disk)?;
            index
        };
        self.indexes_in_use.insert(index);
        Ok(index)
    }

    /// Frees `index`; returns whether the highest index in use dropped (the file can be
    /// truncated).
    fn remove_index(&mut self, index: u64, slot_size: u64, disk: Option<&DiskUsage>) -> Result<bool> {
        if !self.indexes_in_use.remove(&index) {
            return Err(Error::internal(format!(
                "RemoveIndex - index {index} not found in indexes_in_use"
            )));
        }
        self.free_indexes.insert(index);
        let max_in_use = self.indexes_in_use.last().map_or(0, |&max| max + 1);
        if max_in_use < self.max_index {
            self.set_max_index(max_in_use, slot_size, disk)?;
            self.free_indexes.retain(|&free| free < max_in_use);
            return Ok(true);
        }
        Ok(false)
    }

    fn set_max_index(&mut self, new_index: u64, slot_size: u64, disk: Option<&DiskUsage>) -> Result<()> {
        if let Some(disk) = disk {
            if new_index < self.max_index {
                disk.decrease((self.max_index - new_index) * slot_size);
            } else if new_index > self.max_index {
                // Growth may fail, so only update the index afterwards.
                disk.increase((new_index - self.max_index) * slot_size)?;
            }
        }
        self.max_index = new_index;
        Ok(())
    }

    fn has_free_blocks(&self) -> bool {
        !self.free_indexes.is_empty()
    }
}

/// Identifies a temporary file: its size class and index within the class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FileIdentifier {
    size: TemporaryBufferSize,
    file_index: u64,
}

/// Where a buffer lives: file and slot.
#[derive(Clone, Copy, Debug)]
struct SlotLocation {
    file: FileIdentifier,
    slot: u64,
    header_size: usize,
}

/// One temporary file (DuckDB's `TemporaryFileHandle`).
struct TemporaryFile {
    path: PathBuf,
    /// Files fill up to this many slots before a new file is started; later files may
    /// grow larger (`(1 << file_count) * 4000`).
    max_allowed_index: u64,
    slot_size: u64,
    state: Mutex<FileState>,
}

struct FileState {
    file: Option<File>,
    indexes: BlockIndexManager,
}

const MAX_ALLOWED_INDEX_BASE: u64 = 4000;

impl TemporaryFile {
    fn lock(&self) -> MutexGuard<'_, FileState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reserves a slot, or `None` if the file is full.
    fn try_reserve_slot(&self, disk: &DiskUsage) -> Result<Option<u64>> {
        let mut state = self.lock();
        if state.indexes.max_index >= self.max_allowed_index && !state.indexes.has_free_blocks() {
            return Ok(None);
        }
        if state.file.is_none() {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&self.path)
                .map_err(|err| io_error(format!("Cannot open file \"{}\": {err}", self.path.display())))?;
            state.file = Some(file);
        }
        state.indexes.new_index(self.slot_size, Some(disk)).map(Some)
    }

    fn position(&self, slot: u64) -> u64 {
        slot * self.slot_size
    }

    fn write_slot(&self, slot: u64, data: &[u8]) -> Result<()> {
        let state = self.lock();
        let file = state
            .file
            .as_ref()
            .ok_or_else(|| Error::internal("temporary file is not open"))?;
        write_all_at(file, data, self.position(slot))
            .map_err(|err| io_error(format!("Could not write file \"{}\": {err}", self.path.display())))
    }

    fn read_slot(&self, slot: u64, data: &mut [u8]) -> Result<()> {
        let state = self.lock();
        let file = state
            .file
            .as_ref()
            .ok_or_else(|| Error::internal("temporary file is not open"))?;
        read_exact_at(file, data, self.position(slot)).map_err(|err| {
            io_error(format!(
                "Could not read from file \"{}\": {err}",
                self.path.display()
            ))
        })
    }

    /// Frees a slot, truncating the file if its tail became free.
    fn free_slot(&self, slot: u64, disk: &DiskUsage) -> Result<()> {
        let mut state = self.lock();
        if state.indexes.remove_index(slot, self.slot_size, Some(disk))? {
            let length = self.position(state.indexes.max_index);
            if let Some(file) = &state.file {
                file.set_len(length).map_err(|err| {
                    io_error(format!(
                        "Could not truncate file \"{}\": {err}",
                        self.path.display()
                    ))
                })?;
            }
        }
        Ok(())
    }

    /// Deletes the file if no slot is in use; returns whether it did.
    fn delete_if_empty(&self) -> Result<bool> {
        let mut state = self.lock();
        if state.indexes.max_index > 0 {
            return Ok(false);
        }
        state.file = None;
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(err) => Err(io_error(format!(
                "Could not remove file \"{}\": {err}",
                self.path.display()
            ))),
        }
    }

    fn information(&self) -> TemporaryFileInformation {
        let state = self.lock();
        TemporaryFileInformation {
            path: self.path.display().to_string(),
            size: self.position(state.indexes.indexes_in_use.len() as u64),
        }
    }
}

#[derive(Default)]
struct ManagerState {
    files: HashMap<TemporaryBufferSize, BTreeMap<u64, Arc<TemporaryFile>>>,
    used_blocks: HashMap<BlockId, SlotLocation>,
    file_indexes: HashMap<TemporaryBufferSize, BlockIndexManager>,
}

/// Owns the shared temporary files of one directory (DuckDB's `TemporaryFileManager`).
pub(crate) struct TemporaryFileManager {
    directory: PathBuf,
    state: Mutex<ManagerState>,
    pub(crate) disk: DiskUsage,
}

impl TemporaryFileManager {
    fn new(directory: PathBuf, size_on_disk: Arc<AtomicU64>) -> Self {
        Self {
            directory,
            state: Mutex::new(ManagerState::default()),
            disk: DiskUsage {
                size_on_disk,
                max_swap_space: AtomicU64::new(0),
            },
        }
    }

    fn lock(&self) -> MutexGuard<'_, ManagerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn file_path(&self, file: FileIdentifier) -> PathBuf {
        self.directory.join(format!(
            "duckdb_temp_storage_{}-{}.tmp",
            file.size.name(),
            file.file_index
        ))
    }

    /// Writes `buffer` (a default-size buffer) and returns the bytes it occupies on disk.
    pub(crate) fn write_buffer(&self, block_id: BlockId, buffer: &FileBuffer) -> Result<u64> {
        let size = TemporaryBufferSize::Default;
        let (file, location) = {
            let mut state = self.lock();
            let mut target = None;
            if let Some(files) = state.files.get(&size) {
                for (&file_index, file) in files {
                    if let Some(slot) = file.try_reserve_slot(&self.disk)? {
                        target = Some((Arc::clone(file), file_index, slot));
                        break;
                    }
                }
            }
            let (file, file_index, slot) = if let Some(target) = target {
                target
            } else {
                let file_index = state.file_indexes.entry(size).or_default().new_index(0, None)?;
                let identifier = FileIdentifier { size, file_index };
                let files = state.files.entry(size).or_default();
                let file = Arc::new(TemporaryFile {
                    path: self.file_path(identifier),
                    max_allowed_index: (1u64 << files.len().min(32)) * MAX_ALLOWED_INDEX_BASE,
                    slot_size: size.bytes(),
                    state: Mutex::new(FileState {
                        file: None,
                        indexes: BlockIndexManager::default(),
                    }),
                });
                files.insert(file_index, Arc::clone(&file));
                let Some(slot) = file.try_reserve_slot(&self.disk)? else {
                    return Err(Error::internal("a new temporary file has no free slot"));
                };
                (file, file_index, slot)
            };
            let location = SlotLocation {
                file: FileIdentifier { size, file_index },
                slot,
                header_size: buffer.header_size(),
            };
            state.used_blocks.insert(block_id, location);
            (file, location)
        };
        // SAFETY: the buffer is being evicted: it has no readers and its lock is held.
        let data = unsafe { buffer.internal() };
        if let Err(error) = file.write_slot(location.slot, data) {
            let mut state = self.lock();
            self.erase_used_block(&mut state, block_id)?;
            return Err(error);
        }
        Ok(size.bytes())
    }

    pub(crate) fn has_buffer(&self, block_id: BlockId) -> bool {
        self.lock().used_blocks.contains_key(&block_id)
    }

    /// Reads back and frees the slot of `block_id`. Returns the buffer and the bytes the
    /// slot occupied.
    pub(crate) fn read_buffer(
        &self,
        block_id: BlockId,
        allocator: &Arc<BlockAllocator>,
        reusable: Option<FileBuffer>,
    ) -> Result<(FileBuffer, u64)> {
        let (file, location) = {
            let state = self.lock();
            let location = *state
                .used_blocks
                .get(&block_id)
                .ok_or_else(|| Error::internal(format!("temporary block {block_id} not found")))?;
            let file = state
                .files
                .get(&location.file.size)
                .and_then(|files| files.get(&location.file.file_index))
                .cloned()
                .ok_or_else(|| Error::internal("temporary file not found"))?;
            (file, location)
        };
        let alloc_size = location.file.size.bytes() as usize;
        let mut buffer = match reusable {
            Some(buffer) if buffer.alloc_size() == alloc_size => {
                FileBuffer::reuse(buffer, FileBufferType::ManagedBuffer, location.header_size)
            }
            _ => FileBuffer::allocate(
                allocator,
                FileBufferType::ManagedBuffer,
                alloc_size - location.header_size,
                location.header_size,
            )?,
        };
        file.read_slot(location.slot, buffer.internal_mut())?;
        let mut state = self.lock();
        self.erase_used_block(&mut state, block_id)?;
        Ok((buffer, location.file.size.bytes()))
    }

    /// Frees the slot of `block_id` without reading it; returns the bytes it occupied.
    pub(crate) fn delete_buffer(&self, block_id: BlockId) -> Result<u64> {
        let mut state = self.lock();
        let size = state
            .used_blocks
            .get(&block_id)
            .map_or(0, |location| location.file.size.bytes());
        self.erase_used_block(&mut state, block_id)?;
        Ok(size)
    }

    fn erase_used_block(&self, state: &mut ManagerState, block_id: BlockId) -> Result<()> {
        let location = state.used_blocks.remove(&block_id).ok_or_else(|| {
            Error::internal(format!(
                "EraseUsedBlock - Block {block_id} not found in used blocks"
            ))
        })?;
        let Some(file) = state
            .files
            .get(&location.file.size)
            .and_then(|files| files.get(&location.file.file_index))
            .cloned()
        else {
            return Ok(());
        };
        file.free_slot(location.slot, &self.disk)?;
        if file.delete_if_empty()? {
            if let Some(files) = state.files.get_mut(&location.file.size) {
                files.remove(&location.file.file_index);
            }
            if let Some(indexes) = state.file_indexes.get_mut(&location.file.size) {
                indexes.remove_index(location.file.file_index, 0, None)?;
            }
        }
        Ok(())
    }

    pub(crate) fn temporary_files(&self) -> Vec<TemporaryFileInformation> {
        let state = self.lock();
        let mut sizes: Vec<_> = state.files.keys().copied().collect();
        sizes.sort();
        sizes
            .into_iter()
            .flat_map(|size| {
                state.files[&size]
                    .values()
                    .map(|file| file.information())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Sets `max_temp_directory_size`; `None` means 90% of the free disk space.
    pub(crate) fn set_max_swap_space(&self, limit: Option<u64>) -> Result<()> {
        let new_limit = limit.unwrap_or_else(|| default_max_swap(&self.directory));
        let used = self.disk.used();
        if used > new_limit {
            return Err(out_of_memory(format!(
                "failed to adjust the 'max_temp_directory_size', currently used space ({}) exceeds the new limit \
                 ({})\nPlease increase the limit or destroy the buffers stored in the temp directory by e.g removing \
                 temporary tables.\nTo get usage information of the temp_directory, use 'CALL \
                 duckdb_temporary_files();'\n\t\t",
                format_bytes(used),
                format_bytes(new_limit)
            )));
        }
        self.disk.max_swap_space.store(new_limit, Ordering::Relaxed);
        Ok(())
    }
}

/// 90% of the free space at `path`, or "unlimited" if it cannot be determined.
fn default_max_swap(path: &Path) -> u64 {
    match os::available_disk_space(path) {
        Some(space) => (space as f64 * 0.9) as u64,
        None => u64::MAX - 1,
    }
}

/// A temporary directory in use (DuckDB's `TemporaryDirectoryHandle`): creates the
/// directory on first use and cleans it up when dropped.
pub(crate) struct TemporaryDirectoryHandle {
    path: PathBuf,
    created_directory: bool,
    pub(crate) manager: TemporaryFileManager,
}

impl TemporaryDirectoryHandle {
    pub(crate) fn new(
        path: PathBuf,
        size_on_disk: Arc<AtomicU64>,
        max_swap_space: Option<u64>,
    ) -> Result<Self> {
        let created_directory = !path.is_dir();
        if created_directory {
            std::fs::create_dir_all(&path).map_err(|err| {
                io_error(format!(
                    "Failed to create directory \"{}\": {err}",
                    path.display()
                ))
            })?;
        }
        let manager = TemporaryFileManager::new(path.clone(), size_on_disk);
        manager.set_max_swap_space(max_swap_space)?;
        Ok(Self {
            path,
            created_directory,
            manager,
        })
    }
}

impl Drop for TemporaryDirectoryHandle {
    fn drop(&mut self) {
        // Cleanup failures leave stray files behind but must not abort shutdown.
        if self.created_directory {
            let _ = std::fs::remove_dir_all(&self.path);
            return;
        }
        let Ok(entries) = std::fs::read_dir(&self.path) else {
            return;
        };
        for entry in entries.flatten() {
            let is_file = entry.file_type().is_ok_and(|kind| kind.is_file());
            if is_file && entry.file_name().to_string_lossy().starts_with("duckdb_temp_") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(unix)]
pub(crate) fn write_all_at(file: &File, data: &[u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(data, offset)
}

#[cfg(unix)]
pub(crate) fn read_exact_at(file: &File, data: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(data, offset)
}

#[cfg(windows)]
pub(crate) fn write_all_at(file: &File, mut data: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !data.is_empty() {
        let written = file.seek_write(data, offset)?;
        data = &data[written..];
        offset += written as u64;
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn read_exact_at(file: &File, mut data: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !data.is_empty() {
        let read = file.seek_read(data, offset)?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        data = &mut data[read..];
        offset += read as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_manager_reuses_lowest_free_index_and_shrinks() {
        let disk = DiskUsage {
            size_on_disk: Arc::new(AtomicU64::new(0)),
            max_swap_space: AtomicU64::new(u64::MAX),
        };
        let mut indexes = BlockIndexManager::default();
        for expected in 0..4 {
            assert_eq!(indexes.new_index(10, Some(&disk)).unwrap(), expected);
        }
        assert_eq!(disk.used(), 40);
        assert!(!indexes.remove_index(1, 10, Some(&disk)).unwrap());
        assert_eq!(indexes.new_index(10, Some(&disk)).unwrap(), 1);
        assert!(indexes.remove_index(3, 10, Some(&disk)).unwrap());
        assert_eq!(disk.used(), 30);
        assert!(indexes.remove_index(2, 10, Some(&disk)).unwrap());
        assert_eq!(indexes.max_index, 2);
        assert!(indexes.free_indexes.is_empty());
    }

    #[test]
    fn disk_limit_errors_like_duckdb() {
        let disk = DiskUsage {
            size_on_disk: Arc::new(AtomicU64::new(983_040)),
            max_swap_space: AtomicU64::new(1_000_000),
        };
        let error = disk.increase(262_144).unwrap_err();
        assert!(error.to_string().starts_with(
            "Out of Memory Error: failed to offload data block of size 256.0 KiB (960.0 KiB/976.5 KiB used).\n\
             This limit was set by the 'max_temp_directory_size' setting."
        ));
    }
}
