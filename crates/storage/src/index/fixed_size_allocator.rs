//! Fixed-size allocators: the memory of an index, carved into segments of one size
//! (DuckDB's `FixedSizeAllocator` and `FixedSizeBuffer`).
//!
//! Each buffer is one block (`block_size` bytes). It starts with a bitmask of `validity_t`
//! words, one bit per segment (set = free), followed by the segments. The byte layout of a
//! buffer is DuckDB's, so buffers are written to and read from storage as they are.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use rustdb_buffer::{BufferManager, MemoryReservation, MemoryTag};
use rustdb_common::{Error, Result};

use super::IndexPointer;

/// Bits per bitmask word (`sizeof(validity_t) * 8`).
const BITS_PER_WORD: usize = 64;
/// Bytes per bitmask word.
const WORD_SIZE: usize = 8;
/// The share of free memory, in percent, from which a vacuum empties buffers
/// (`VACUUM_THRESHOLD`).
const VACUUM_THRESHOLD: u8 = 10;

/// Where a buffer lives on disk: a block and the offset of the buffer in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferPointer {
    pub block_id: i64,
    pub offset: u32,
}

/// Loads the bytes of a buffer persisted at a [`BufferPointer`].
pub trait BufferLoader: Send + Sync + std::fmt::Debug {
    /// Returns `allocation_size` bytes stored at `pointer`.
    fn load(&self, pointer: BufferPointer, allocation_size: usize) -> Result<Vec<u8>>;
}

/// One buffer of an allocator (DuckDB's `FixedSizeBuffer`).
#[derive(Debug)]
pub(crate) struct FixedSizeBuffer {
    /// The in-memory bytes, loaded lazily for buffers read from storage.
    memory: OnceLock<Box<[u8]>>,
    /// Memory charged to the buffer manager for `memory`.
    reservation: OnceLock<MemoryReservation>,
    /// Number of allocated segments.
    pub(crate) segment_count: usize,
    /// Bytes of the buffer that hold data (bitmask up to the last allocated segment).
    pub(crate) allocation_size: usize,
    /// Location on disk, if the buffer was persisted.
    pub(crate) disk: Option<BufferPointer>,
    /// Whether the in-memory bytes differ from the persisted ones.
    pub(crate) dirty: bool,
}

impl FixedSizeBuffer {
    fn in_memory(bytes: Box<[u8]>, reservation: MemoryReservation) -> Self {
        Self {
            memory: OnceLock::from(bytes),
            reservation: OnceLock::from(reservation),
            segment_count: 0,
            allocation_size: 0,
            disk: None,
            dirty: true,
        }
    }

    fn on_disk(pointer: BufferPointer, segment_count: usize, allocation_size: usize) -> Self {
        Self {
            memory: OnceLock::new(),
            reservation: OnceLock::new(),
            segment_count,
            allocation_size,
            disk: Some(pointer),
            dirty: false,
        }
    }

    pub(crate) fn is_loaded(&self) -> bool {
        self.memory.get().is_some()
    }
}

/// Hands out segments of `segment_size` bytes from block-sized buffers.
#[derive(Debug)]
pub struct FixedSizeAllocator {
    buffer_manager: BufferManager,
    loader: Option<std::sync::Arc<dyn BufferLoader>>,
    block_size: usize,
    segment_size: usize,
    bitmask_count: usize,
    bitmask_offset: usize,
    available_segments_per_buffer: usize,
    total_segment_count: usize,
    pub(crate) buffers: BTreeMap<u64, FixedSizeBuffer>,
    buffers_with_free_space: BTreeSet<u64>,
    buffer_with_free_space: Option<u64>,
    /// The buffers a running vacuum empties (`vacuum_buffers`).
    vacuum_buffers: BTreeSet<u64>,
}

impl FixedSizeAllocator {
    /// An empty allocator of `segment_size`-byte segments in `block_size`-byte buffers.
    pub fn new(segment_size: usize, block_size: usize, buffer_manager: BufferManager) -> Result<Self> {
        if segment_size > block_size - WORD_SIZE {
            return Err(Error::internal(format!(
                "The maximum segment size of fixed-size allocators is {}",
                block_size - WORD_SIZE
            )));
        }
        // Interleave bitmask words and segments until the block is full.
        let mut byte_count = 0;
        let mut bitmask_count = 0;
        let mut available = 0;
        while byte_count < block_size {
            if bitmask_count == 0 || (bitmask_count * BITS_PER_WORD).is_multiple_of(available) {
                bitmask_count += 1;
                byte_count += WORD_SIZE;
            }
            let remaining_bytes = block_size - byte_count;
            let remaining_segments = (remaining_bytes / segment_size).min(BITS_PER_WORD);
            if remaining_segments == 0 {
                break;
            }
            available += remaining_segments;
            byte_count += remaining_segments * segment_size;
        }
        Ok(Self {
            buffer_manager,
            loader: None,
            block_size,
            segment_size,
            bitmask_count,
            bitmask_offset: bitmask_count * WORD_SIZE,
            available_segments_per_buffer: available,
            total_segment_count: 0,
            buffers: BTreeMap::new(),
            buffers_with_free_space: BTreeSet::new(),
            buffer_with_free_space: None,
            vacuum_buffers: BTreeSet::new(),
        })
    }

    pub fn segment_size(&self) -> usize {
        self.segment_size
    }

    /// Number of allocated segments.
    pub fn segment_count(&self) -> usize {
        self.total_segment_count
    }

    pub fn is_empty(&self) -> bool {
        self.total_segment_count == 0
    }

    /// Sets how persisted buffers are loaded.
    pub fn set_loader(&mut self, loader: std::sync::Arc<dyn BufferLoader>) {
        self.loader = Some(loader);
    }

    /// Allocates a zeroed segment (`New`).
    pub fn new_segment(&mut self) -> Result<IndexPointer> {
        let buffer_id = if let Some(id) = self.buffer_with_free_space {
            id
        } else {
            {
                let id = self.available_buffer_id();
                let reservation = self
                    .buffer_manager
                    .reserve_tagged(MemoryTag::ArtIndex, buffer_memory_size(self.block_size))?;
                let mut bytes = vec![0u8; self.block_size].into_boxed_slice();
                // All segments start free.
                set_all_valid(&mut bytes, self.available_segments_per_buffer);
                self.buffers
                    .insert(id, FixedSizeBuffer::in_memory(bytes, reservation));
                self.buffers_with_free_space.insert(id);
                self.buffer_with_free_space = Some(id);
                id
            }
        };
        let available = self.available_segments_per_buffer;
        let bitmask_count = self.bitmask_count;
        let segment_size = self.segment_size;
        let bitmask_offset = self.bitmask_offset;
        let loader = self.loader.clone();
        let buffer = self
            .buffers
            .get_mut(&buffer_id)
            .ok_or_else(|| Error::internal("allocator buffer with free space is missing"))?;
        let segment_count = buffer.segment_count;
        let bytes = buffer_bytes_mut(buffer, loader.as_deref(), &self.buffer_manager, self.block_size)?;
        let offset = take_free_segment(bytes, segment_count, bitmask_count, available)?;
        let start = offset * segment_size + bitmask_offset;
        bytes[start..start + segment_size].fill(0);
        buffer.dirty = true;
        buffer.segment_count += 1;
        self.total_segment_count += 1;
        if buffer.segment_count == available {
            self.buffers_with_free_space.remove(&buffer_id);
            self.next_buffer_with_free_space();
        }
        let buffer_id = u32::try_from(buffer_id).map_err(|_| Error::internal("index buffer id overflow"))?;
        let offset = u32::try_from(offset).map_err(|_| Error::internal("index segment offset overflow"))?;
        Ok(IndexPointer::new(buffer_id, offset))
    }

    /// Frees the segment at `ptr` (`Free`).
    pub fn free(&mut self, ptr: IndexPointer) -> Result<()> {
        let buffer_id = ptr.buffer_id();
        let offset = ptr.offset() as usize;
        let loader = self.loader.clone();
        let buffer = self.buffers.get_mut(&buffer_id).ok_or_else(|| {
            Error::internal(format!("freeing a segment of unknown index buffer {buffer_id}"))
        })?;
        let bytes = buffer_bytes_mut(buffer, loader.as_deref(), &self.buffer_manager, self.block_size)?;
        let word = offset / BITS_PER_WORD;
        let bit = offset % BITS_PER_WORD;
        let mut value = read_word(bytes, word);
        if value & (1 << bit) != 0 {
            return Err(Error::internal("double free of an index segment"));
        }
        value |= 1 << bit;
        write_word(bytes, word, value);
        buffer.dirty = true;
        if buffer.segment_count == 0 || self.total_segment_count == 0 {
            return Err(Error::internal("index allocator segment count underflow"));
        }
        buffer.segment_count -= 1;
        self.total_segment_count -= 1;
        if buffer.segment_count != 0 {
            self.buffers_with_free_space.insert(buffer_id);
            if self.buffer_with_free_space.is_none() {
                self.buffer_with_free_space = Some(buffer_id);
            }
            return Ok(());
        }
        // Keep the last empty buffer around for the next allocation.
        if self.buffers_with_free_space.len() == 1 {
            return Ok(());
        }
        self.buffers_with_free_space.remove(&buffer_id);
        self.buffers.remove(&buffer_id);
        if self.buffer_with_free_space == Some(buffer_id) {
            self.next_buffer_with_free_space();
        }
        Ok(())
    }

    /// Frees all memory (`Reset`).
    pub fn reset(&mut self) {
        self.buffers.clear();
        self.buffers_with_free_space.clear();
        self.buffer_with_free_space = None;
        self.total_segment_count = 0;
    }

    /// The bytes of the segment at `ptr`.
    pub fn segment(&self, ptr: IndexPointer) -> Result<&[u8]> {
        let buffer = self.buffer(ptr.buffer_id())?;
        let bytes = buffer_bytes(
            buffer,
            self.loader.as_deref(),
            &self.buffer_manager,
            self.block_size,
        )?;
        let start = ptr.offset() as usize * self.segment_size + self.bitmask_offset;
        bytes
            .get(start..start + self.segment_size)
            .ok_or_else(|| Error::internal("index segment out of range"))
    }

    /// The bytes of the segment at `ptr`, for modification.
    pub fn segment_mut(&mut self, ptr: IndexPointer) -> Result<&mut [u8]> {
        let loader = self.loader.clone();
        let buffer_id = ptr.buffer_id();
        let buffer = self
            .buffers
            .get_mut(&buffer_id)
            .ok_or_else(|| Error::internal(format!("unknown index buffer {buffer_id}")))?;
        buffer.dirty = true;
        let bytes = buffer_bytes_mut(buffer, loader.as_deref(), &self.buffer_manager, self.block_size)?;
        let start = ptr.offset() as usize * self.segment_size + self.bitmask_offset;
        bytes
            .get_mut(start..start + self.segment_size)
            .ok_or_else(|| Error::internal("index segment out of range"))
    }

    fn buffer(&self, buffer_id: u64) -> Result<&FixedSizeBuffer> {
        self.buffers
            .get(&buffer_id)
            .ok_or_else(|| Error::internal(format!("unknown index buffer {buffer_id}")))
    }

    /// Whether the segment at `ptr` is in memory without loading it (`LoadedFromStorage`'s
    /// counterpart).
    pub fn is_loaded(&self, ptr: IndexPointer) -> bool {
        self.buffers
            .get(&ptr.buffer_id())
            .is_some_and(FixedSizeBuffer::is_loaded)
    }

    /// Bytes of memory held by loaded buffers (`GetInMemorySize`).
    pub fn in_memory_size(&self) -> usize {
        self.buffers.values().filter(|b| b.is_loaded()).count() * self.block_size
    }

    /// Drops buffers without segments (`RemoveEmptyBuffers`).
    pub fn remove_empty_buffers(&mut self) {
        let empty: Vec<u64> = self
            .buffers
            .iter()
            .filter(|(_, buffer)| buffer.segment_count == 0)
            .map(|(&id, _)| id)
            .collect();
        for id in empty {
            self.buffers.remove(&id);
            self.buffers_with_free_space.remove(&id);
        }
        self.next_buffer_with_free_space();
    }

    /// Chooses the buffers a vacuum empties; returns whether there are any
    /// (`InitializeVacuum`). An empty allocator frees all its memory instead. Buffers not
    /// in memory are not vacuumed.
    pub fn initialize_vacuum(&mut self) -> bool {
        if self.total_segment_count == 0 {
            self.reset();
            return false;
        }
        self.remove_empty_buffers();
        // (free segments, buffer id) of every buffer in memory, least full last.
        let mut candidates: Vec<(usize, u64)> = Vec::new();
        let mut available_in_memory = 0;
        for (&id, buffer) in &self.buffers {
            if buffer.is_loaded() {
                let available = self.available_segments_per_buffer - buffer.segment_count;
                available_in_memory += available;
                candidates.push((available, id));
            }
        }
        if candidates.is_empty() {
            return false;
        }
        let excess_buffer_count = available_in_memory / self.available_segments_per_buffer;
        // The threshold is adaptive: vacuum once the excess is VACUUM_THRESHOLD percent.
        let memory_usage = self.in_memory_size();
        let excess_memory_usage = excess_buffer_count * self.block_size;
        let excess_percentage = excess_memory_usage as f64 / memory_usage as f64;
        if excess_percentage < f64::from(VACUUM_THRESHOLD) / 100.0 {
            return false;
        }
        // Vacuum the buffers with the most free space (`multimap` order: the
        // smallest keys are erased first, the same key in insertion order).
        candidates.sort_by_key(|&(available, _)| available);
        let skip = candidates.len() - excess_buffer_count;
        for &(_, id) in &candidates[skip..] {
            self.buffers_with_free_space.remove(&id);
            self.vacuum_buffers.insert(id);
        }
        self.next_buffer_with_free_space();
        true
    }

    /// Whether the segment at `ptr` lives in a buffer being vacuumed (`NeedsVacuum`).
    pub fn needs_vacuum(&self, ptr: IndexPointer) -> bool {
        self.vacuum_buffers.contains(&ptr.buffer_id())
    }

    /// Moves the segment at `ptr` to a buffer that stays and returns its new pointer
    /// (`VacuumPointer`). The old buffer's bitmask is left as is: the whole buffer is
    /// freed by [`FixedSizeAllocator::finalize_vacuum`].
    pub fn vacuum_pointer(&mut self, ptr: IndexPointer) -> Result<IndexPointer> {
        let new_ptr = self.new_segment()?;
        // `new_segment` counted the segment, which is only moved.
        self.total_segment_count -= 1;
        let bytes = self.segment(ptr)?.to_vec();
        self.segment_mut(new_ptr)?.copy_from_slice(&bytes);
        Ok(new_ptr)
    }

    /// Frees the vacuumed buffers (`FinalizeVacuum`).
    pub fn finalize_vacuum(&mut self) {
        for id in std::mem::take(&mut self.vacuum_buffers) {
            self.buffers.remove(&id);
        }
    }

    /// The smallest id not used by a buffer, searching down from the buffer count
    /// (`GetAvailableBufferId`).
    fn available_buffer_id(&self) -> u64 {
        let mut id = self.buffers.len() as u64;
        while self.buffers.contains_key(&id) {
            id -= 1;
        }
        id
    }

    fn next_buffer_with_free_space(&mut self) {
        self.buffer_with_free_space = self.buffers_with_free_space.iter().next().copied();
    }

    /// Writes every buffer with `write`, which returns where the bytes went, and records
    /// the locations (`SerializeBuffers`). A checkpoint rewrites all index data, so
    /// buffers persisted by an earlier checkpoint are written again.
    pub(crate) fn serialize_buffers(
        &mut self,
        write: &mut dyn FnMut(&[u8]) -> Result<BufferPointer>,
        keep: &mut dyn FnMut(rustdb_buffer::BlockId),
    ) -> Result<()> {
        self.update_allocation_sizes();
        let ids: Vec<u64> = self.buffers.keys().copied().collect();
        for id in ids {
            let pointer = {
                let buffer = self
                    .buffers
                    .get(&id)
                    .ok_or_else(|| Error::internal("index buffer disappeared"))?;
                // `FixedSizeBuffer::Serialize`: a buffer on disk that did not change keeps
                // its block.
                if let (false, Some(disk)) = (buffer.dirty, buffer.disk) {
                    keep(disk.block_id);
                    continue;
                }
                let bytes = buffer_bytes(
                    buffer,
                    self.loader.as_deref(),
                    &self.buffer_manager,
                    self.block_size,
                )?;
                write(&bytes[..buffer.allocation_size.min(bytes.len())])?
            };
            self.set_persisted(id, pointer);
        }
        Ok(())
    }

    /// The bytes of every buffer, in the order of [`FixedSizeAllocator::info`], for a
    /// WAL entry (`InitSerializationToWAL`).
    pub(crate) fn wal_buffers(&mut self) -> Result<Vec<Vec<u8>>> {
        self.update_allocation_sizes();
        self.buffers
            .values()
            .map(|buffer| {
                let bytes = buffer_bytes(
                    buffer,
                    self.loader.as_deref(),
                    &self.buffer_manager,
                    self.block_size,
                )?;
                Ok(bytes[..buffer.allocation_size.min(bytes.len())].to_vec())
            })
            .collect()
    }

    /// Recomputes the allocation size of every dirty buffer (`SetAllocationSize`).
    pub(crate) fn update_allocation_sizes(&mut self) {
        let available = self.available_segments_per_buffer;
        let segment_size = self.segment_size;
        let bitmask_offset = self.bitmask_offset;
        for buffer in self.buffers.values_mut() {
            if !buffer.dirty {
                continue;
            }
            let Some(bytes) = buffer.memory.get() else {
                continue;
            };
            let mut max_offset = available;
            for i in (1..=available).rev() {
                if !bit_is_set(bytes, i - 1) {
                    max_offset = i;
                    break;
                }
            }
            buffer.allocation_size = max_offset * segment_size + bitmask_offset;
        }
    }

    /// The serialization information of this allocator (`GetInfo`).
    pub fn info(&self) -> FixedSizeAllocatorInfo {
        let mut info = FixedSizeAllocatorInfo {
            segment_size: self.segment_size as u64,
            ..FixedSizeAllocatorInfo::default()
        };
        for (&id, buffer) in &self.buffers {
            info.buffer_ids.push(id);
            info.block_pointers.push(buffer.disk.unwrap_or(BufferPointer {
                block_id: -1,
                offset: 0,
            }));
            info.segment_counts.push(buffer.segment_count as u64);
            info.allocation_sizes.push(buffer.allocation_size as u64);
        }
        info.buffers_with_free_space = self.buffers_with_free_space.iter().copied().collect();
        info
    }

    /// Registers the persisted buffers described by `info` (`Init`); they are loaded on
    /// first access.
    pub fn init(&mut self, info: &FixedSizeAllocatorInfo) -> Result<()> {
        self.segment_size = usize::try_from(info.segment_size)
            .map_err(|_| Error::serialization("invalid segment size in index storage information"))?;
        self.total_segment_count = 0;
        for (i, &id) in info.buffer_ids.iter().enumerate() {
            let pointer = *info
                .block_pointers
                .get(i)
                .ok_or_else(|| Error::serialization("missing block pointer in index storage information"))?;
            if pointer.block_id < 0 || pointer.block_id >= rustdb_buffer::MAXIMUM_BLOCK {
                return Err(Error::serialization(
                    "invalid block ID in index storage information",
                ));
            }
            let segment_count = info.segment_counts.get(i).copied().unwrap_or(0) as usize;
            let allocation_size = info.allocation_sizes.get(i).copied().unwrap_or(0) as usize;
            self.buffers.insert(
                id,
                FixedSizeBuffer::on_disk(pointer, segment_count, allocation_size),
            );
            self.total_segment_count += segment_count;
        }
        self.buffers_with_free_space = info.buffers_with_free_space.iter().copied().collect();
        self.next_buffer_with_free_space();
        Ok(())
    }

    /// Adds a buffer whose bytes are given directly (WAL replay of an index).
    pub fn insert_buffer(&mut self, id: u64, data: &[u8], segment_count: usize) -> Result<()> {
        let reservation = self
            .buffer_manager
            .reserve_tagged(MemoryTag::ArtIndex, buffer_memory_size(self.block_size))?;
        let mut bytes = vec![0u8; self.block_size].into_boxed_slice();
        let len = data.len().min(self.block_size);
        bytes[..len].copy_from_slice(&data[..len]);
        let mut buffer = FixedSizeBuffer::in_memory(bytes, reservation);
        buffer.segment_count = segment_count;
        buffer.allocation_size = len;
        self.buffers.insert(id, buffer);
        self.total_segment_count += segment_count;
        Ok(())
    }

    /// Records where a buffer was written.
    pub(crate) fn set_persisted(&mut self, id: u64, pointer: BufferPointer) {
        if let Some(buffer) = self.buffers.get_mut(&id) {
            buffer.disk = Some(pointer);
            buffer.dirty = false;
        }
    }

    #[allow(dead_code, reason = "used by checkpointing")]
    pub(crate) fn bitmask_offset(&self) -> usize {
        self.bitmask_offset
    }

    #[allow(dead_code, reason = "used by checkpointing")]
    pub(crate) fn available_segments_per_buffer(&self) -> usize {
        self.available_segments_per_buffer
    }
}

/// Serialization information of an allocator (DuckDB's `FixedSizeAllocatorInfo`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FixedSizeAllocatorInfo {
    pub segment_size: u64,
    pub buffer_ids: Vec<u64>,
    pub block_pointers: Vec<BufferPointer>,
    pub segment_counts: Vec<u64>,
    pub allocation_sizes: Vec<u64>,
    pub buffers_with_free_space: Vec<u64>,
}

fn load_buffer(
    buffer: &FixedSizeBuffer,
    loader: Option<&dyn BufferLoader>,
    buffer_manager: &BufferManager,
    block_size: usize,
) -> Result<()> {
    if buffer.memory.get().is_some() {
        return Ok(());
    }
    let pointer = buffer
        .disk
        .ok_or_else(|| Error::internal("invalid or missing buffer in FixedSizeAllocator"))?;
    let loader = loader.ok_or_else(|| Error::internal("no loader for a persisted index buffer"))?;
    let data = loader.load(pointer, buffer.allocation_size)?;
    let reservation = buffer_manager.reserve_tagged(MemoryTag::ArtIndex, buffer_memory_size(block_size))?;
    let mut bytes = vec![0u8; block_size].into_boxed_slice();
    let len = data.len().min(block_size);
    bytes[..len].copy_from_slice(&data[..len]);
    // A concurrent load of the same buffer is impossible: index access is serialised by
    // the index lock, so losing the race here only drops a duplicate.
    let _ = buffer.memory.set(bytes);
    let _ = buffer.reservation.set(reservation);
    Ok(())
}

/// The memory a buffer of `block_size` bytes is accounted with: a block with its header,
/// like the blocks DuckDB allocates for it (`BufferManager::Allocate` with the block
/// manager's block size).
fn buffer_memory_size(block_size: usize) -> u64 {
    (block_size + crate::storage_info::DEFAULT_BLOCK_HEADER_SIZE) as u64
}

fn buffer_bytes<'a>(
    buffer: &'a FixedSizeBuffer,
    loader: Option<&dyn BufferLoader>,
    buffer_manager: &BufferManager,
    block_size: usize,
) -> Result<&'a [u8]> {
    load_buffer(buffer, loader, buffer_manager, block_size)?;
    buffer
        .memory
        .get()
        .map(|m| &m[..])
        .ok_or_else(|| Error::internal("index buffer failed to load"))
}

fn buffer_bytes_mut<'a>(
    buffer: &'a mut FixedSizeBuffer,
    loader: Option<&dyn BufferLoader>,
    buffer_manager: &BufferManager,
    block_size: usize,
) -> Result<&'a mut [u8]> {
    load_buffer(buffer, loader, buffer_manager, block_size)?;
    buffer
        .memory
        .get_mut()
        .map(|m| &mut m[..])
        .ok_or_else(|| Error::internal("index buffer failed to load"))
}

fn read_word(bytes: &[u8], word: usize) -> u64 {
    let mut raw = [0u8; WORD_SIZE];
    raw.copy_from_slice(&bytes[word * WORD_SIZE..(word + 1) * WORD_SIZE]);
    u64::from_le_bytes(raw)
}

fn write_word(bytes: &mut [u8], word: usize, value: u64) {
    bytes[word * WORD_SIZE..(word + 1) * WORD_SIZE].copy_from_slice(&value.to_le_bytes());
}

fn bit_is_set(bytes: &[u8], index: usize) -> bool {
    read_word(bytes, index / BITS_PER_WORD) & (1 << (index % BITS_PER_WORD)) != 0
}

/// Marks the first `count` segments free (`ValidityMask::SetAllValid`).
fn set_all_valid(bytes: &mut [u8], count: usize) {
    let full_words = count / BITS_PER_WORD;
    for word in 0..full_words {
        write_word(bytes, word, u64::MAX);
    }
    let rest = count % BITS_PER_WORD;
    if rest != 0 {
        write_word(bytes, full_words, (1u64 << rest) - 1);
    }
}

/// Claims a free segment of a buffer: the segment at `segment_count` if free, else the
/// first free one (`FixedSizeBuffer::GetOffset`).
fn take_free_segment(
    bytes: &mut [u8],
    segment_count: usize,
    bitmask_count: usize,
    available: usize,
) -> Result<usize> {
    if segment_count < available && bit_is_set(bytes, segment_count) {
        let word = segment_count / BITS_PER_WORD;
        let value = read_word(bytes, word) & !(1 << (segment_count % BITS_PER_WORD));
        write_word(bytes, word, value);
        return Ok(segment_count);
    }
    for word in 0..bitmask_count {
        let value = read_word(bytes, word);
        if value == 0 {
            continue;
        }
        let bit = value.trailing_zeros() as usize;
        write_word(bytes, word, value & !(1 << bit));
        return Ok(word * BITS_PER_WORD + bit);
    }
    Err(Error::internal("Invalid bitmask for FixedSizeAllocator"))
}

#[cfg(test)]
mod tests {
    use rustdb_buffer::BufferManagerConfig;

    use super::*;

    fn allocator(segment_size: usize) -> FixedSizeAllocator {
        let manager = BufferManager::new(BufferManagerConfig::default()).unwrap();
        FixedSizeAllocator::new(segment_size, 262_136, manager).unwrap()
    }

    #[test]
    fn layout_matches_duckdb() {
        // Node4 segments (40 bytes) in a 256 KiB block.
        let a = allocator(40);
        assert_eq!(a.bitmask_count, 103);
        assert_eq!(a.available_segments_per_buffer, 6532);
        let a = allocator(2056);
        assert_eq!(a.available_segments_per_buffer, 127);
        assert_eq!(a.bitmask_offset, 16);
    }

    #[test]
    fn allocate_and_free() {
        let mut a = allocator(40);
        let first = a.new_segment().unwrap();
        let second = a.new_segment().unwrap();
        assert_eq!((first.buffer_id(), first.offset()), (0, 0));
        assert_eq!((second.buffer_id(), second.offset()), (0, 1));
        a.segment_mut(second).unwrap()[0] = 7;
        assert_eq!(a.segment(second).unwrap()[0], 7);
        a.free(first).unwrap();
        assert!(a.free(first).is_err());
        // the freed segment is reused
        let third = a.new_segment().unwrap();
        assert_eq!(third.offset(), 0);
        assert_eq!(a.segment_count(), 2);
    }
}
