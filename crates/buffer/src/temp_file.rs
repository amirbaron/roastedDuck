//! Temporary files for evicted buffers (DuckDB's `TemporaryFileManager`).
//!
//! Buffers of the default block size are written into shared files named
//! `duckdb_temp_storage_<SIZE>-<index>.tmp`, one slot per buffer. Freed slots are reused,
//! and files shrink (truncate) when their last slots are freed and disappear when empty.
//! Every growth is checked against `max_temp_directory_size`.
//!
//! Like DuckDB, buffers are zstd-compressed into smaller size classes (`S32K` ...
//! `S224K`) when that pays off: an adaptive choice per write compares how long
//! uncompressed writes and compressed writes at each of the levels -5, -3, -1, 1, 3 and 5
//! took (`TemporaryFileCompressionAdaptivity`). A compressed slot holds the compressed
//! length (8 bytes, little endian) followed by the zstd frame.
//!
//! With `temp_file_encryption`, the files created are encrypted: every slot is preceded
//! by the nonce and tag of its AES-GCM encryption under the database's temporary key
//! (`DEFAULT_ENCRYPTED_BUFFER_HEADER_SIZE` bytes).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use roastedduck_common::{Error, Result};
use roastedduck_crypto::{ENCRYPTED_BUFFER_HEADER_SIZE, EncryptionKeyManager};

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
    /// The size class of a compressed buffer of `bytes` bytes, rounded up to the 32 KiB
    /// granularity; `None` if it is larger than the largest compressed class
    /// (`RoundUpSizeToTemporaryBufferSize`).
    fn for_compressed(bytes: usize) -> Option<Self> {
        Some(match bytes.div_ceil(32 << 10) {
            0 | 1 => Self::S32K,
            2 => Self::S64K,
            3 => Self::S96K,
            4 => Self::S128K,
            5 => Self::S160K,
            6 => Self::S192K,
            7 => Self::S224K,
            _ => return None,
        })
    }

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

/// Whether new temporary files are encrypted (`temp_file_encryption`), and the database's
/// keys, whose temporary key encrypts them (`TemporaryFileManager::IsEncrypted`).
#[derive(Debug, Default)]
pub(crate) struct TemporaryFileEncryption {
    pub(crate) enabled: AtomicBool,
    pub(crate) keys: Arc<EncryptionKeyManager>,
}

impl TemporaryFileEncryption {
    pub(crate) fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Encrypts `data` into `[nonce and tag][ciphertext]` (`EncryptTemporaryBuffer`).
    pub(crate) fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        let key = self.keys.temp_key()?;
        let mut encrypted = vec![0u8; ENCRYPTED_BUFFER_HEADER_SIZE + data.len()];
        let (metadata, payload) = encrypted.split_at_mut(ENCRYPTED_BUFFER_HEADER_SIZE);
        payload.copy_from_slice(data);
        let metadata: &mut [u8; ENCRYPTED_BUFFER_HEADER_SIZE] = metadata
            .try_into()
            .map_err(|_| Error::internal("encryption metadata size"))?;
        roastedduck_crypto::encrypt_temporary_buffer(&key, payload, metadata)?;
        Ok(encrypted)
    }

    /// Decrypts `data` in place with the nonce and tag of `metadata`
    /// (`DecryptTemporaryBuffer`).
    pub(crate) fn decrypt(
        &self,
        metadata: &[u8; ENCRYPTED_BUFFER_HEADER_SIZE],
        data: &mut [u8],
    ) -> Result<()> {
        let key = self.keys.temp_key()?;
        roastedduck_crypto::decrypt_temporary_buffer(&key, data, metadata)
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
    /// Whether the slots are encrypted (`TemporaryFileIdentifier::encrypted`), decided
    /// when the file is created.
    encrypted: bool,
    state: Mutex<FileState>,
}

struct FileState {
    /// The open file; reads and writes clone it and run without the lock, like DuckDB's
    /// (only the slot bookkeeping is under the file lock).
    file: Option<Arc<File>>,
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
            state.file = Some(Arc::new(file));
        }
        state.indexes.new_index(self.slot_size, Some(disk)).map(Some)
    }

    /// Where slot `slot` starts (`GetPositionInFile`): encrypted slots have the nonce and
    /// tag in front.
    fn position(&self, slot: u64) -> u64 {
        let metadata = if self.encrypted {
            ENCRYPTED_BUFFER_HEADER_SIZE as u64
        } else {
            0
        };
        slot * (self.slot_size + metadata)
    }

    /// The open file, for I/O outside the lock.
    fn handle(&self) -> Result<Arc<File>> {
        self.lock()
            .file
            .clone()
            .ok_or_else(|| Error::internal("temporary file is not open"))
    }

    fn write_slot(&self, slot: u64, data: &[u8], encryption: &TemporaryFileEncryption) -> Result<()> {
        let file = self.handle()?;
        let encrypted;
        let data = if self.encrypted {
            encrypted = encryption.encrypt(data)?;
            &encrypted
        } else {
            data
        };
        write_all_at(&file, data, self.position(slot))
            .map_err(|err| io_error(format!("Could not write file \"{}\": {err}", self.path.display())))
    }

    fn read_slot(&self, slot: u64, data: &mut [u8], encryption: &TemporaryFileEncryption) -> Result<()> {
        let file = self.handle()?;
        let read_error = |err: std::io::Error| {
            io_error(format!(
                "Could not read from file \"{}\": {err}",
                self.path.display()
            ))
        };
        let mut position = self.position(slot);
        let mut metadata = [0u8; ENCRYPTED_BUFFER_HEADER_SIZE];
        if self.encrypted {
            read_exact_at(&file, &mut metadata, position).map_err(read_error)?;
            position += ENCRYPTED_BUFFER_HEADER_SIZE as u64;
        }
        read_exact_at(&file, data, position).map_err(read_error)?;
        if self.encrypted {
            encryption.decrypt(&metadata, data)?;
        }
        Ok(())
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

/// Which zstd level writes are the fastest, and whether they are fast enough, compared to
/// uncompressed writes, to compress at all (DuckDB's `TemporaryFileCompressionAdaptivity`).
#[derive(Debug)]
struct CompressionAdaptivity {
    /// Moving averages of the duration of uncompressed writes and of compressed writes per
    /// level.
    uncompressed_ns: u64,
    compressed_ns: [u64; Self::LEVELS],
    /// State of the random deviations.
    random: u64,
}

impl CompressionAdaptivity {
    /// `INITIAL_NS`
    const INITIAL_NS: u64 = 50_000;
    /// The levels -5, -3, -1, 1, 3 and 5 (`LEVELS`).
    const LEVELS: usize = 6;
    /// Compress while compressed writes take less than this many times as long
    /// (`DURATION_RATIO_THRESHOLD`).
    const DURATION_RATIO_THRESHOLD: f64 = 2.0;
    /// How often to try another choice, to keep the averages current
    /// (`COMPRESSION_DEVIATION`).
    const COMPRESSION_DEVIATION: f64 = 0.5;
    /// Weight of the old average (`WEIGHT`).
    const WEIGHT: u64 = 16;

    fn new(seed: u64) -> Self {
        Self {
            uncompressed_ns: Self::INITIAL_NS,
            compressed_ns: [Self::INITIAL_NS; Self::LEVELS],
            random: seed | 1,
        }
    }

    /// `IndexToLevel`.
    fn level(index: usize) -> i32 {
        index as i32 * 2 - 5
    }

    fn next_random(&mut self) -> f64 {
        // xorshift64*
        self.random ^= self.random >> 12;
        self.random ^= self.random << 25;
        self.random ^= self.random >> 27;
        (self.random.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// The zstd level of the next write, `None` to write it uncompressed
    /// (`GetCompressionLevel`).
    fn compression_level(&mut self) -> Option<i32> {
        let mut min_index = 0;
        for index in 1..Self::LEVELS {
            if self.compressed_ns[index] < self.compressed_ns[min_index] {
                min_index = index;
            }
        }
        let ratio = self.compressed_ns[min_index] as f64 / self.uncompressed_ns as f64;
        let should_compress = ratio < Self::DURATION_RATIO_THRESHOLD;
        let should_deviate = self.next_random() < Self::COMPRESSION_DEVIATION;
        let deviate_uncompressed = self.next_random() < 0.5;
        if !should_deviate {
            should_compress.then(|| Self::level(min_index))
        } else if !should_compress {
            Some(Self::level(0))
        } else if deviate_uncompressed {
            None
        } else if min_index == Self::LEVELS - 1 {
            Some(Self::level(min_index - 1))
        } else if ratio < 1.0 {
            Some(Self::level(min_index + 1))
        } else if min_index == 0 {
            None
        } else {
            Some(Self::level(min_index - 1))
        }
    }

    /// Records the duration of a write at `level` (`Update`).
    fn update(&mut self, level: Option<i32>, started: Instant) {
        let duration = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let average = match level {
            None => &mut self.uncompressed_ns,
            Some(level) => &mut self.compressed_ns[((level + 5) >> 1) as usize],
        };
        *average = (*average * (Self::WEIGHT - 1) + duration) / Self::WEIGHT;
    }
}

/// The number of independent adaptivity states, picked by thread
/// (`COMPRESSION_ADAPTIVITIES`).
const COMPRESSION_ADAPTIVITIES: usize = 64;

/// The adaptivity state of the calling thread (`TaskScheduler::GetEstimatedCPUId`).
fn adaptivity_index() -> usize {
    use std::hash::{Hash, Hasher};
    let mut hasher = FixedHasher(0);
    std::thread::current().id().hash(&mut hasher);
    (hasher.finish() as usize) % COMPRESSION_ADAPTIVITIES
}

/// FNV-1a, a fixed hash for picking per-thread state.
struct FixedHasher(u64);

impl std::hash::Hasher for FixedHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = if self.0 == 0 {
            0xcbf2_9ce4_8422_2325
        } else {
            self.0
        };
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        self.0 = hash;
    }
}

/// `buffer`'s bytes zstd-compressed at `level` into a slot image (length word, then the
/// frame), or `None` if that does not fit a compressed size class (`CompressBuffer`).
fn compress_buffer(data: &[u8], level: i32) -> Option<(TemporaryBufferSize, Vec<u8>)> {
    if data.len() as u64 <= TemporaryBufferSize::S32K.bytes() {
        return None; // No point compressing.
    }
    let frame = roastedduck_zstd::compress(data, level);
    let size = TemporaryBufferSize::for_compressed(size_of::<u64>() + frame.len())?;
    let mut slot = Vec::with_capacity(size.bytes() as usize);
    slot.extend_from_slice(&(frame.len() as u64).to_le_bytes());
    slot.extend_from_slice(&frame);
    slot.resize(size.bytes() as usize, 0);
    Some((size, slot))
}

/// Decompresses a compressed slot image into `target`, which must receive exactly its
/// length in bytes.
fn decompress_slot(slot: &[u8], target: &mut [u8]) -> Result<()> {
    let corrupt = |message: String| io_error(format!("Corrupt temporary file: {message}"));
    let (length, frame) = slot
        .split_first_chunk::<8>()
        .ok_or_else(|| corrupt("compressed block without a length".to_owned()))?;
    let length = usize::try_from(u64::from_le_bytes(*length)).unwrap_or(usize::MAX);
    let frame = frame.get(..length).ok_or_else(|| {
        corrupt(format!(
            "compressed block claims {length} bytes but only {} are available",
            frame.len()
        ))
    })?;
    let size = roastedduck_zstd::decompress(target, frame)
        .map_err(|error| corrupt(format!("failed to decompress block ({error})")))?;
    if size != target.len() {
        return Err(corrupt(format!(
            "decompressed block is {size} bytes but expected {}",
            target.len()
        )));
    }
    Ok(())
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
    adaptivities: Box<[Mutex<CompressionAdaptivity>]>,
    pub(crate) encryption: Arc<TemporaryFileEncryption>,
}

impl TemporaryFileManager {
    fn new(
        directory: PathBuf,
        size_on_disk: Arc<AtomicU64>,
        encryption: Arc<TemporaryFileEncryption>,
    ) -> Self {
        Self {
            directory,
            encryption,
            state: Mutex::new(ManagerState::default()),
            disk: DiskUsage {
                size_on_disk,
                max_swap_space: AtomicU64::new(0),
            },
            adaptivities: (0..COMPRESSION_ADAPTIVITIES as u64)
                .map(|index| {
                    Mutex::new(CompressionAdaptivity::new(
                        index.wrapping_mul(0x9E37_79B9_7F4A_7C15),
                    ))
                })
                .collect(),
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
        // SAFETY: the buffer is being evicted: it has no readers and its lock is held.
        let data = unsafe { buffer.internal() };
        let adaptivity = &self.adaptivities[adaptivity_index()];
        let started = Instant::now();
        let level = adaptivity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .compression_level();
        let compressed = level.and_then(|level| compress_buffer(data, level));
        let size = compressed
            .as_ref()
            .map_or(TemporaryBufferSize::Default, |(size, _)| *size);
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
                    encrypted: self.encryption.enabled(),
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
        let slot_data = compressed.as_ref().map_or(data, |(_, slot)| slot.as_slice());
        if let Err(error) = file.write_slot(location.slot, slot_data, &self.encryption) {
            let mut state = self.lock();
            self.erase_used_block(&mut state, block_id)?;
            return Err(error);
        }
        adaptivity
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .update(level, started);
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
        let alloc_size = DEFAULT_BLOCK_ALLOC_SIZE;
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
        if location.file.size == TemporaryBufferSize::Default {
            file.read_slot(location.slot, buffer.internal_mut(), &self.encryption)?;
        } else {
            let mut slot = vec![0; location.file.size.bytes() as usize];
            file.read_slot(location.slot, &mut slot, &self.encryption)?;
            decompress_slot(&slot, buffer.internal_mut())?;
        }
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
        encryption: Arc<TemporaryFileEncryption>,
    ) -> Result<Self> {
        let created_directory = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => false,
            Ok(_) => {
                return Err(io_error(format!(
                    "Failed to create directory \"{}\": path exists but is not a directory!",
                    path.display()
                ))
                .with_extra_info("errno", "0"));
            }
            Err(_) => true,
        };
        if created_directory {
            // C++ creates one directory with mode 0755, subject to the process
            // umask. Missing ancestors must fail without creating parents.
            let mut directory = std::fs::DirBuilder::new();
            directory.recursive(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                directory.mode(0o755);
            }
            directory
                .create(&path)
                .or_else(|err| {
                    if err.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() {
                        Ok(())
                    } else {
                        Err(err)
                    }
                })
                .map_err(|err| {
                    let error = io_error(format!(
                        "Failed to create directory \"{}\": {}",
                        path.display(),
                        roastedduck_common::os_error_text(&err)
                    ));
                    match err.raw_os_error() {
                        Some(code) => error.with_extra_info("errno", code.to_string()),
                        None => error,
                    }
                })?;
        }
        let manager = TemporaryFileManager::new(path.clone(), size_on_disk, encryption);
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

    fn directory_fixture(label: &str) -> PathBuf {
        static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "roastedduck-temp-directory-{label}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create dedicated directory fixture");
        path
    }

    fn directory_handle(path: PathBuf) -> Result<TemporaryDirectoryHandle> {
        TemporaryDirectoryHandle::new(
            path,
            Arc::new(AtomicU64::new(0)),
            Some(1 << 20),
            Arc::new(TemporaryFileEncryption::default()),
        )
    }

    #[test]
    fn temporary_directory_missing_parent_does_not_create_ancestors() {
        let root = directory_fixture("missing-parent");
        let parent = root.join("missing");
        let path = parent.join("spill");
        let result = directory_handle(path.clone());
        let error = result.as_ref().err().map(ToString::to_string);
        #[cfg(unix)]
        let has_errno = result
            .as_ref()
            .err()
            .is_some_and(|error| error.extra_info().contains(&("errno", &b"2"[..])));
        let created_parent = parent.exists();
        drop(result);
        std::fs::remove_dir_all(&root).expect("remove owned fixture");
        let error = error.expect("missing parent must fail like C++ CreateDirectory");
        assert!(
            error.starts_with("IO Error: Failed to create directory"),
            "{error}"
        );
        assert!(!created_parent, "a failed request must not create ancestors");
        assert!(!error.contains("(os error "), "{error}");
        #[cfg(unix)]
        assert!(has_errno, "missing-parent errors must carry errno 2");
    }

    #[test]
    fn temporary_directory_creates_only_owned_leaf_and_removes_it() {
        let root = directory_fixture("owned-leaf");
        let path = root.join("spill");
        let handle = directory_handle(path.clone()).expect("existing parent permits one new leaf");
        assert!(path.is_dir());
        drop(handle);
        assert!(!path.exists());
        assert!(root.is_dir());
        std::fs::remove_dir(&root).expect("remove owned fixture parent");
    }

    #[test]
    fn temporary_directory_preserves_preexisting_directory_and_user_file() {
        let root = directory_fixture("preexisting");
        let path = root.join("spill");
        std::fs::create_dir(&path).unwrap();
        let user_file = path.join("keep.txt");
        std::fs::write(&user_file, b"unchanged").unwrap();
        let handle = directory_handle(path.clone()).expect("preexisting directory is valid");
        drop(handle);
        assert_eq!(std::fs::read(&user_file).unwrap(), b"unchanged");
        assert!(path.is_dir());
        let file_path = root.join("not-a-directory");
        std::fs::write(&file_path, b"also unchanged").unwrap();
        let error = directory_handle(file_path.clone())
            .err()
            .expect("a regular file is not a temporary directory");
        assert_eq!(
            error.raw_message(),
            format!(
                "Failed to create directory \"{}\": path exists but is not a directory!",
                file_path.display()
            )
        );
        assert!(error.extra_info().contains(&("errno", &b"0"[..])));
        assert_eq!(std::fs::read(file_path).unwrap(), b"also unchanged");
        std::fs::remove_dir_all(&root).expect("remove only owned fixtures");
    }

    #[cfg(unix)]
    #[test]
    fn temporary_directory_never_adds_group_or_other_write_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = directory_fixture("permissions");
        let path = root.join("spill");
        let handle = directory_handle(path.clone()).expect("create one temporary directory");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        drop(handle);
        std::fs::remove_dir(&root).expect("remove owned fixture parent");
        // Also run this binary under umask 000: the caller's permissive mask
        // must not add writes that C++'s explicit 0755 mode does not request.
        assert_eq!(mode & 0o022, 0);
    }

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
    fn compressed_slots_round_trip() {
        let data: Vec<u8> = (0..DEFAULT_BLOCK_ALLOC_SIZE).map(|i| (i % 7) as u8).collect();
        let (size, slot) = super::compress_buffer(&data, 1).expect("repetitive data compresses");
        assert_eq!(size, TemporaryBufferSize::S32K);
        assert_eq!(slot.len() as u64, size.bytes());
        let mut restored = vec![0; data.len()];
        super::decompress_slot(&slot, &mut restored).unwrap();
        assert_eq!(restored, data);

        // Incompressible data stays in the default class.
        let mut state = 0x1234_5678_u64;
        let noise: Vec<u8> = (0..DEFAULT_BLOCK_ALLOC_SIZE)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        assert!(super::compress_buffer(&noise, 1).is_none());
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
