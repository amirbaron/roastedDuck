//! Metadata blocks (DuckDB's `MetadataManager`, `MetadataReader` and `MetadataWriter`).
//!
//! Metadata (the catalog, table pointers, statistics, the free list) is written in
//! chains of small blocks: every file block holding metadata is split into 64 equal
//! metadata blocks. A metadata block starts with the pointer to the next block of its
//! chain (`-1` at the end). A [`MetaBlockPointer`] packs the file block id with the
//! metadata block index in its top byte, plus a byte offset.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roastedduck_buffer::{
    BlockHandle, BlockId, BlockManager, BufferHandle, BufferManager, MAXIMUM_BLOCK, MemoryTag,
};
use roastedduck_common::{Error, ErrorKind, Result};

use crate::single_file_block_manager::SingleFileBlockManager;

/// The number of metadata blocks per file block (`METADATA_BLOCK_COUNT`).
pub const METADATA_BLOCK_COUNT: usize = 64;

/// A pointer into metadata (DuckDB's `MetaBlockPointer`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MetaBlockPointer {
    /// The file block id in the low 56 bits, the metadata block index in the top 8.
    pub block_pointer: u64,
    pub offset: u32,
}

impl MetaBlockPointer {
    pub const fn new(block_pointer: u64, offset: u32) -> Self {
        Self {
            block_pointer,
            offset,
        }
    }

    /// No pointer.
    pub const fn invalid() -> Self {
        Self::new(u64::MAX, 0)
    }

    pub const fn is_valid(&self) -> bool {
        self.block_pointer != u64::MAX
    }

    /// The file block id (`GetBlockId`).
    pub const fn block_id(&self) -> BlockId {
        (self.block_pointer & !(0xFF << 56)) as BlockId
    }

    /// The metadata block index (`GetBlockIndex`).
    pub const fn block_index(&self) -> u32 {
        (self.block_pointer >> 56) as u32
    }
}

/// A metadata block in memory (DuckDB's `MetadataPointer`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetadataPointer {
    pub block_index: BlockId,
    pub index: u8,
}

/// A pinned metadata block (DuckDB's `MetadataHandle`).
#[derive(Debug)]
pub struct MetadataHandle {
    pub pointer: MetadataPointer,
    handle: BufferHandle,
}

/// One file block of metadata (DuckDB's `MetadataBlock`).
#[derive(Debug)]
struct MetadataBlock {
    block: Option<Arc<BlockHandle>>,
    block_id: BlockId,
    /// Free metadata block indexes, the next one to use last.
    free_blocks: Vec<u8>,
    dirty: bool,
}

impl MetadataBlock {
    /// `FreeBlocksToInteger`.
    fn free_blocks_to_integer(&self) -> u64 {
        self.free_blocks
            .iter()
            .fold(0, |result, &index| result | (1 << index))
    }

    /// `FreeBlocksFromInteger`.
    fn set_free_blocks(&mut self, free_list: u64) {
        self.free_blocks = (0..64u8)
            .rev()
            .filter(|&index| free_list & (1 << index) != 0)
            .collect();
    }
}

/// One file block of metadata as `pragma_metadata_info` shows it (`MetadataBlockInfo`).
#[derive(Clone, Debug)]
pub struct MetadataBlockInfo {
    pub block_id: BlockId,
    pub total_blocks: u64,
    /// The free metadata block indexes, ascending.
    pub free_list: Vec<u64>,
}

/// Allocates, pins and flushes metadata blocks (DuckDB's `MetadataManager`).
#[derive(Debug)]
pub struct MetadataManager {
    block_manager: Arc<SingleFileBlockManager>,
    buffer_manager: BufferManager,
    blocks: Mutex<BTreeMap<BlockId, MetadataBlock>>,
    /// For every block, the metadata blocks in use at the last checkpoint: freed when
    /// the next checkpoint no longer uses them.
    modified_blocks: Mutex<BTreeMap<BlockId, u64>>,
}

impl MetadataManager {
    pub fn new(block_manager: Arc<SingleFileBlockManager>, buffer_manager: BufferManager) -> Self {
        Self {
            block_manager,
            buffer_manager,
            blocks: Mutex::default(),
            modified_blocks: Mutex::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<BlockId, MetadataBlock>> {
        self.blocks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn dyn_manager(&self) -> Arc<dyn BlockManager> {
        Arc::clone(&self.block_manager) as Arc<dyn BlockManager>
    }

    pub fn block_manager(&self) -> &Arc<SingleFileBlockManager> {
        &self.block_manager
    }

    /// The size of one metadata block (`GetMetadataBlockSize`).
    pub fn metadata_block_size(&self) -> usize {
        (self.block_manager.block_size() / METADATA_BLOCK_COUNT) & !7
    }

    /// Every file block holding metadata with its free metadata blocks, by block id
    /// (`GetMetadataInfo`).
    pub fn metadata_info(&self) -> Vec<MetadataBlockInfo> {
        self.lock()
            .values()
            .map(|block| {
                let mut free_list: Vec<u64> =
                    block.free_blocks.iter().map(|&index| u64::from(index)).collect();
                free_list.sort_unstable();
                MetadataBlockInfo {
                    block_id: block.block_id,
                    total_blocks: METADATA_BLOCK_COUNT as u64,
                    free_list,
                }
            })
            .collect()
    }

    /// The number of file blocks holding metadata (`BlockCount`).
    pub fn block_count(&self) -> usize {
        self.lock().len()
    }

    /// A free metadata block, pinned for writing (`AllocateHandle`).
    pub fn allocate_handle(&self) -> Result<MetadataHandle> {
        let existing = self
            .lock()
            .iter()
            .find(|(_, block)| !block.free_blocks.is_empty())
            .map(|(&id, _)| id);
        let block_id = match existing {
            Some(id) if id <= self.block_manager.peek_free_block_id() => id,
            _ => self.allocate_new_block()?,
        };
        let mut blocks = self.lock();
        let block = blocks
            .get_mut(&block_id)
            .ok_or_else(|| Error::internal("metadata block disappeared"))?;
        block.dirty = true;
        let is_persistent = block
            .block
            .as_ref()
            .is_some_and(|handle| handle.block_id() < MAXIMUM_BLOCK);
        if is_persistent {
            self.convert_to_transient(block)?;
        }
        let index = block
            .free_blocks
            .pop()
            .ok_or_else(|| Error::internal("metadata block without free space"))?;
        drop(blocks);
        self.pin(MetadataPointer {
            block_index: block_id,
            index,
        })
    }

    /// Pins a metadata block (`Pin`).
    pub fn pin(&self, pointer: MetadataPointer) -> Result<MetadataHandle> {
        let block = {
            let blocks = self.lock();
            let entry = blocks.get(&pointer.block_index).ok_or_else(|| {
                Error::internal(format!(
                    "Trying to pin block {} - but the block did not exist",
                    pointer.block_index
                ))
            })?;
            entry
                .block
                .clone()
                .ok_or_else(|| Error::internal("metadata block without a handle"))?
        };
        Ok(MetadataHandle {
            pointer,
            handle: self.buffer_manager.pin(&block)?,
        })
    }

    /// Copies a block read from disk into a new in-memory block before it is written to
    /// (`ConvertToTransient`).
    fn convert_to_transient(&self, block: &mut MetadataBlock) -> Result<()> {
        let old = block
            .block
            .clone()
            .ok_or_else(|| Error::internal("metadata block without a handle"))?;
        let old_pin = self.buffer_manager.pin(&old)?;
        let new_pin =
            self.buffer_manager
                .allocate_for(MemoryTag::Metadata, self.block_manager.as_ref(), false)?;
        let size = self.block_manager.block_size();
        // SAFETY: the old block is only read here; the new block is not shared yet.
        unsafe {
            std::ptr::copy_nonoverlapping(old_pin.as_ptr(), new_pin.as_ptr(), size);
        }
        drop(old_pin);
        self.block_manager.unregister_block(block.block_id);
        block.block = Some(Arc::clone(new_pin.block()));
        block.dirty = true;
        Ok(())
    }

    /// Adds a new file block of metadata (`AllocateNewBlock`).
    fn allocate_new_block(&self) -> Result<BlockId> {
        let block_id = self.block_manager.get_free_block_id_for_checkpoint()?;
        let pin =
            self.buffer_manager
                .allocate_for(MemoryTag::Metadata, self.block_manager.as_ref(), false)?;
        // SAFETY: the block was just allocated and is not shared.
        unsafe {
            std::ptr::write_bytes(pin.as_ptr(), 0, self.block_manager.block_size());
        }
        let block = MetadataBlock {
            block: Some(Arc::clone(pin.block())),
            block_id,
            free_blocks: (0..METADATA_BLOCK_COUNT as u8).rev().collect(),
            dirty: true,
        };
        let mut blocks = self.lock();
        if blocks.contains_key(&block_id) {
            return Err(Error::internal(format!(
                "Block id with id {block_id} already exists"
            )));
        }
        blocks.insert(block_id, block);
        Ok(block_id)
    }

    /// Registers a file block of metadata found on disk (`AddAndRegisterBlock`).
    fn add_and_register_block(
        &self,
        blocks: &mut BTreeMap<BlockId, MetadataBlock>,
        mut block: MetadataBlock,
    ) {
        if blocks.contains_key(&block.block_id) {
            return;
        }
        let handle = self
            .buffer_manager
            .register_block(&self.dyn_manager(), block.block_id);
        // Blocks read from the file are never written in place (`convert_to_transient`
        // copies them first), so they are read again (when a row group's column is first
        // used) instead of staying resident.
        handle.release_on_unpin();
        block.block = Some(handle);
        blocks.insert(block.block_id, block);
    }

    /// The disk pointer of `pointer` at `offset` (`GetDiskPointer`).
    pub fn disk_pointer(pointer: MetadataPointer, offset: u32) -> MetaBlockPointer {
        let block = u64::try_from(pointer.block_index).unwrap_or(u64::MAX >> 8);
        MetaBlockPointer::new(block | (u64::from(pointer.index) << 56), offset)
    }

    /// The in-memory pointer of a disk pointer to a known block (`FromDiskPointer`).
    pub fn from_disk_pointer(&self, pointer: MetaBlockPointer) -> Result<MetadataPointer> {
        let block_id = pointer.block_id();
        if !self.lock().contains_key(&block_id) {
            return Err(Error::internal(format!(
                "Failed to load metadata pointer (id {block_id}, idx {}, ptr {})\n",
                pointer.block_index(),
                pointer.block_pointer
            )));
        }
        Ok(MetadataPointer {
            block_index: block_id,
            index: pointer.block_index() as u8,
        })
    }

    /// Registers the block of a pointer read from disk (`RegisterDiskPointer`).
    pub fn register_disk_pointer(&self, pointer: MetaBlockPointer) -> Result<MetadataPointer> {
        {
            let mut blocks = self.lock();
            let block = MetadataBlock {
                block: None,
                block_id: pointer.block_id(),
                free_blocks: Vec::new(),
                dirty: false,
            };
            self.add_and_register_block(&mut blocks, block);
        }
        self.from_disk_pointer(pointer)
    }

    /// Writes dirty metadata blocks to the file (`Flush`).
    pub fn flush(&self) -> Result<()> {
        let total_metadata_size = self.metadata_block_size() * METADATA_BLOCK_COUNT;
        let block_size = self.block_manager.block_size();
        let ids: Vec<BlockId> = self.lock().keys().copied().collect();
        for id in ids {
            let (handle, dirty) = {
                let blocks = self.lock();
                let Some(block) = blocks.get(&id) else { continue };
                (block.block.clone(), block.dirty)
            };
            let Some(handle) = handle else { continue };
            if !dirty {
                if handle.block_id() >= MAXIMUM_BLOCK {
                    return Err(Error::internal("Transient blocks must always be marked as dirty"));
                }
                continue;
            }
            let pin = self.buffer_manager.pin(&handle)?;
            // SAFETY: metadata blocks are only written by the checkpointing thread.
            unsafe {
                std::ptr::write_bytes(
                    pin.as_ptr().add(total_metadata_size),
                    0,
                    block_size - total_metadata_size,
                );
            }
            let new_handle = if handle.block_id() >= MAXIMUM_BLOCK {
                // Readers can still pin this metadata snapshot during a checkpoint.
                // Match C++'s THREAD_SAFE conversion instead of moving their buffer.
                self.buffer_manager
                    .convert_to_persistent_thread_safe(&self.dyn_manager(), id, handle, pin)?
            } else {
                self.write_pinned(id, &pin)?;
                handle
            };
            let mut blocks = self.lock();
            if let Some(block) = blocks.get_mut(&id) {
                block.block = Some(new_handle);
                block.dirty = false;
            }
        }
        Ok(())
    }

    /// Writes a pinned persistent block through the block manager.
    fn write_pinned(&self, block_id: BlockId, pin: &BufferHandle) -> Result<()> {
        let size = self.block_manager.block_size();
        // SAFETY: the pin keeps `size` bytes alive; only the checkpoint writes metadata.
        let data = unsafe { std::slice::from_raw_parts(pin.as_ptr(), size) };
        self.block_manager.write_block(block_id, data)
    }

    /// Writes the list of metadata blocks and their free metadata blocks (`Write`).
    pub fn write_block_list(&self, writer: &mut MetadataWriter<'_>) -> Result<()> {
        let entries: Vec<(BlockId, u64)> = self
            .lock()
            .values()
            .map(|block| (block.block_id, block.free_blocks_to_integer()))
            .collect();
        writer.write_u64(entries.len() as u64)?;
        for (block_id, free) in entries {
            writer.write_i64(block_id)?;
            writer.write_u64(free)?;
        }
        Ok(())
    }

    /// Reads the list written by [`MetadataManager::write_block_list`] (`Read`).
    pub fn read_block_list(&self, reader: &mut MetadataReader<'_>) -> Result<()> {
        let count = reader.read_u64()?;
        for _ in 0..count {
            let block_id = reader.read_i64()?;
            let free = reader.read_u64()?;
            let mut blocks = self.lock();
            if let Some(existing) = blocks.get_mut(&block_id) {
                existing.set_free_blocks(free);
            } else {
                let mut block = MetadataBlock {
                    block: None,
                    block_id,
                    free_blocks: Vec::new(),
                    dirty: false,
                };
                block.set_free_blocks(free);
                self.add_and_register_block(&mut blocks, block);
            }
        }
        Ok(())
    }

    /// Frees the metadata blocks the previous checkpoint used and remembers the ones in
    /// use now (`MarkBlocksAsModified`).
    pub fn mark_blocks_as_modified(&self) {
        let mut modified = self
            .modified_blocks
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut blocks = self.lock();
        let mut fully_freed = Vec::new();
        for (&block_id, &modified_list) in modified.iter() {
            let Some(block) = blocks.get_mut(&block_id) else {
                continue;
            };
            let new_free = block.free_blocks_to_integer() | modified_list;
            if new_free == u64::MAX {
                blocks.remove(&block_id);
                fully_freed.push(block_id);
            } else {
                block.set_free_blocks(new_free);
            }
        }
        modified.clear();
        for block in blocks.values() {
            modified.insert(block.block_id, !block.free_blocks_to_integer());
        }
        drop(blocks);
        drop(modified);
        for block_id in fully_freed {
            // A block the free list does not know is an internal inconsistency we
            // cannot recover from here; DuckDB treats it as an internal error too.
            let _ = self.block_manager.mark_block_as_modified(block_id);
        }
    }

    /// Keeps metadata blocks that are still referenced (`ClearModifiedBlocks`).
    pub fn clear_modified_blocks(&self, pointers: &[MetaBlockPointer]) -> Result<()> {
        let mut modified = self
            .modified_blocks
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for pointer in pointers {
            let entry = modified.get_mut(&pointer.block_id()).ok_or_else(|| {
                Error::internal(format!(
                    "ClearModifiedBlocks - Block id {} not found in modified_blocks",
                    pointer.block_id()
                ))
            })?;
            *entry &= !(1u64 << pointer.block_index());
        }
        Ok(())
    }

    /// The handles of all metadata blocks (`GetBlocks`).
    pub fn blocks(&self) -> Vec<Arc<BlockHandle>> {
        self.lock()
            .values()
            .filter_map(|block| block.block.clone())
            .collect()
    }
}

/// Reads a chain of metadata blocks (DuckDB's `MetadataReader`).
#[derive(Debug)]
pub struct MetadataReader<'a> {
    manager: &'a MetadataManager,
    /// Whether the blocks are found on disk now (`BlockReaderType::REGISTER_BLOCKS`).
    register_blocks: bool,
    next_pointer: MetaBlockPointer,
    has_next_block: bool,
    block: Option<MetadataHandle>,
    offset: usize,
    next_offset: usize,
    capacity: usize,
    /// The pointers of the blocks read so far, if tracked.
    read_pointers: Option<Vec<MetaBlockPointer>>,
}

impl<'a> MetadataReader<'a> {
    pub fn new(
        manager: &'a MetadataManager,
        pointer: MetaBlockPointer,
        register_blocks: bool,
    ) -> Result<Self> {
        Ok(Self {
            manager,
            register_blocks,
            next_pointer: pointer,
            has_next_block: true,
            block: None,
            offset: 0,
            next_offset: pointer.offset as usize,
            capacity: 0,
            read_pointers: None,
        })
    }

    /// Records the pointers of the blocks read from now on.
    pub fn track_read_pointers(&mut self) {
        self.read_pointers = Some(Vec::new());
    }

    /// The pointers of the blocks read since tracking started.
    pub fn take_read_pointers(&mut self) -> Vec<MetaBlockPointer> {
        self.read_pointers.take().unwrap_or_default()
    }

    fn read_next_block(&mut self) -> Result<()> {
        if !self.has_next_block {
            return Err(Error::new(
                ErrorKind::Io,
                "No more data remaining in MetadataReader",
            ));
        }
        if let Some(pointers) = &mut self.read_pointers {
            pointers.push(self.next_pointer);
        }
        let pointer = if self.register_blocks {
            self.manager.register_disk_pointer(self.next_pointer)?
        } else {
            self.manager.from_disk_pointer(self.next_pointer)?
        };
        let handle = self.manager.pin(pointer)?;
        let base = usize::from(pointer.index) * self.manager.metadata_block_size();
        // SAFETY: metadata blocks being read are not written concurrently.
        let next = unsafe { read_u64_at(&handle, base) };
        self.block = Some(handle);
        if next == u64::MAX {
            self.has_next_block = false;
        } else {
            self.next_pointer = MetaBlockPointer::new(next, 0);
        }
        self.next_offset = self.next_offset.max(8);
        if self.next_offset > self.manager.metadata_block_size() {
            return Err(Error::internal("next_offset cannot be bigger than block size"));
        }
        self.offset = self.next_offset;
        self.next_offset = 8;
        self.capacity = self.manager.metadata_block_size();
        Ok(())
    }

    /// Fills `buffer` with the next bytes (`ReadData`).
    pub fn read_data(&mut self, buffer: &mut [u8]) -> Result<()> {
        let mut done = 0;
        while self.offset + (buffer.len() - done) > self.capacity {
            let available = self.capacity - self.offset;
            if available > 0 {
                self.copy_out(&mut buffer[done..done + available]);
                done += available;
            }
            self.read_next_block()?;
        }
        let rest = buffer.len() - done;
        self.copy_out(&mut buffer[done..]);
        self.offset += rest;
        Ok(())
    }

    fn copy_out(&self, target: &mut [u8]) {
        let Some(block) = &self.block else { return };
        let base = usize::from(block.pointer.index) * self.manager.metadata_block_size() + self.offset;
        // SAFETY: in bounds of the pinned block (offset + len <= capacity); metadata is
        // not written while it is read.
        unsafe {
            std::ptr::copy_nonoverlapping(block.handle.as_ptr().add(base), target.as_mut_ptr(), target.len());
        }
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        let mut bytes = [0u8; 1];
        self.read_data(&mut bytes)?;
        Ok(bytes[0])
    }

    pub fn read_u32(&mut self) -> Result<u32> {
        let mut bytes = [0u8; 4];
        self.read_data(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    pub fn read_u64(&mut self) -> Result<u64> {
        let mut bytes = [0u8; 8];
        self.read_data(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    pub fn read_i64(&mut self) -> Result<i64> {
        let mut bytes = [0u8; 8];
        self.read_data(&mut bytes)?;
        Ok(i64::from_le_bytes(bytes))
    }

    /// The current position (`GetMetaBlockPointer`).
    pub fn meta_block_pointer(&self) -> Result<MetaBlockPointer> {
        let block =
            self.block.as_ref().filter(|_| self.capacity > 0).ok_or_else(|| {
                Error::internal("GetMetaBlockPointer called but there is no active pointer")
            })?;
        Ok(MetadataManager::disk_pointer(block.pointer, self.offset as u32))
    }
}

/// Reads the little-endian `u64` at byte `offset` of a pinned block.
///
/// # Safety
/// No one may write these bytes concurrently.
unsafe fn read_u64_at(handle: &MetadataHandle, offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    // SAFETY: metadata blocks hold `METADATA_BLOCK_COUNT` blocks of at least 8 bytes;
    // the caller guarantees no concurrent writers.
    unsafe {
        std::ptr::copy_nonoverlapping(handle.handle.as_ptr().add(offset), bytes.as_mut_ptr(), 8);
    }
    u64::from_le_bytes(bytes)
}

/// Where a writer gets its next metadata block.
#[derive(Debug)]
enum HandleSource {
    /// Allocated from the metadata manager.
    Allocate,
    /// Reserved up front (`FreeListBlockWriter`).
    Reserved(std::vec::IntoIter<MetadataHandle>),
}

/// Writes a chain of metadata blocks (DuckDB's `MetadataWriter`).
#[derive(Debug)]
pub struct MetadataWriter<'a> {
    manager: &'a MetadataManager,
    source: HandleSource,
    block: Option<MetadataHandle>,
    offset: usize,
    capacity: usize,
    /// The pointers of the blocks written, if tracked.
    written_pointers: Option<Vec<MetaBlockPointer>>,
}

impl<'a> MetadataWriter<'a> {
    pub fn new(manager: &'a MetadataManager) -> Self {
        Self {
            manager,
            source: HandleSource::Allocate,
            block: None,
            offset: 0,
            capacity: 0,
            written_pointers: None,
        }
    }

    /// A writer over blocks reserved in advance (`FreeListBlockWriter`).
    pub fn with_handles(manager: &'a MetadataManager, handles: Vec<MetadataHandle>) -> Self {
        Self {
            source: HandleSource::Reserved(handles.into_iter()),
            ..Self::new(manager)
        }
    }

    /// Records the pointers of all blocks written from now on.
    pub fn track_written_pointers(&mut self) {
        let mut pointers = Vec::new();
        if let Some(block) = &self.block
            && self.capacity > 0
            && self.offset < self.capacity
        {
            pointers.push(MetadataManager::disk_pointer(block.pointer, 0));
        }
        self.written_pointers = Some(pointers);
    }

    /// The pointers of the blocks written since tracking started.
    pub fn take_written_pointers(&mut self) -> Vec<MetaBlockPointer> {
        self.written_pointers.take().unwrap_or_default()
    }

    fn next_handle(&mut self) -> Result<MetadataHandle> {
        match &mut self.source {
            HandleSource::Allocate => self.manager.allocate_handle(),
            HandleSource::Reserved(handles) => handles.next().ok_or_else(|| {
                Error::internal(
                    "Free List Block Writer ran out of blocks, this means not enough blocks were allocated up front",
                )
            }),
        }
    }

    fn base(&self) -> usize {
        self.block.as_ref().map_or(0, |block| {
            usize::from(block.pointer.index) * self.manager.metadata_block_size()
        })
    }

    fn store_u64(&self, offset: usize, value: u64) {
        if let Some(block) = &self.block {
            // SAFETY: in bounds (metadata blocks are at least 8 bytes); the writer owns
            // the metadata block it writes.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    value.to_le_bytes().as_ptr(),
                    block.handle.as_ptr().add(self.base() + offset),
                    8,
                );
            }
        }
    }

    /// Moves to a new metadata block, linking it from the current one (`NextBlock`).
    fn next_block(&mut self) -> Result<()> {
        let new_handle = self.next_handle()?;
        if self.capacity > 0 {
            let pointer = MetadataManager::disk_pointer(new_handle.pointer, 0);
            self.store_u64(0, pointer.block_pointer);
        }
        self.block = Some(new_handle);
        self.offset = 8;
        self.capacity = self.manager.metadata_block_size();
        self.store_u64(0, u64::MAX);
        if let (Some(pointers), Some(block)) = (&mut self.written_pointers, &self.block) {
            pointers.push(MetadataManager::disk_pointer(block.pointer, 0));
        }
        Ok(())
    }

    /// The position the next byte goes to (`GetMetaBlockPointer`).
    pub fn meta_block_pointer(&mut self) -> Result<MetaBlockPointer> {
        if self.offset >= self.capacity {
            self.next_block()?;
        }
        let block = self
            .block
            .as_ref()
            .ok_or_else(|| Error::internal("metadata writer without a block"))?;
        Ok(MetadataManager::disk_pointer(block.pointer, self.offset as u32))
    }

    /// Appends bytes, continuing in new blocks as needed (`WriteData`).
    pub fn write_data(&mut self, mut data: &[u8]) -> Result<()> {
        while self.offset + data.len() > self.capacity {
            let amount = self.capacity - self.offset;
            if amount > 0 {
                self.copy_in(&data[..amount]);
                self.offset += amount;
                data = &data[amount..];
            }
            self.next_block()?;
        }
        self.copy_in(data);
        self.offset += data.len();
        Ok(())
    }

    fn copy_in(&self, data: &[u8]) {
        if let Some(block) = &self.block {
            // SAFETY: in bounds (offset + len <= capacity); the writer owns the block.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    block.handle.as_ptr().add(self.base() + self.offset),
                    data.len(),
                );
            }
        }
    }

    pub fn write_u8(&mut self, value: u8) -> Result<()> {
        self.write_data(&[value])
    }

    pub fn write_u32(&mut self, value: u32) -> Result<()> {
        self.write_data(&value.to_le_bytes())
    }

    pub fn write_u64(&mut self, value: u64) -> Result<()> {
        self.write_data(&value.to_le_bytes())
    }

    pub fn write_i64(&mut self, value: i64) -> Result<()> {
        self.write_data(&value.to_le_bytes())
    }

    /// Zeroes the rest of the current block and releases it (`Flush`).
    pub fn flush(&mut self) -> Result<()> {
        if self.offset < self.capacity
            && let Some(block) = &self.block
        {
            // SAFETY: in bounds of the current metadata block; the writer owns it.
            unsafe {
                std::ptr::write_bytes(
                    block.handle.as_ptr().add(self.base() + self.offset),
                    0,
                    self.capacity - self.offset,
                );
            }
        }
        self.block = None;
        self.capacity = 0;
        self.offset = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::single_file_block_manager::BlockManagerOptions;
    use roastedduck_buffer::{BufferManagerConfig, DEFAULT_BLOCK_ALLOC_SIZE};
    use roastedduck_crypto::EncryptionKeyManager;

    #[test]
    fn flush_preserves_pinned_metadata_readers() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "roastedduck-metadata-reader-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let buffers = BufferManager::new(BufferManagerConfig {
            memory_limit: Some((8 * DEFAULT_BLOCK_ALLOC_SIZE) as u64),
            ..BufferManagerConfig::default()
        })?;
        let blocks = Arc::new(SingleFileBlockManager::create_new(
            path.as_os_str().as_encoded_bytes(),
            BlockManagerOptions {
                read_only: false,
                block_alloc_size: DEFAULT_BLOCK_ALLOC_SIZE,
                storage_version: None,
            },
            None,
            &EncryptionKeyManager::default(),
        )?);
        let metadata = MetadataManager::new(blocks, buffers.clone());
        let mut writer = MetadataWriter::new(&metadata);
        let pointer = writer.meta_block_pointer()?;
        writer.write_u64(11)?;
        writer.write_u64(22)?;
        writer.flush()?;
        drop(writer);

        let mut reader = MetadataReader::new(&metadata, pointer, false)?;
        assert_eq!(reader.read_u64()?, 11);
        metadata.flush()?;
        // The reader keeps its original transient buffer across publication.
        assert_eq!(reader.read_u64()?, 22);
        let mut persisted = MetadataReader::new(&metadata, pointer, false)?;
        assert_eq!(persisted.read_u64()?, 11);
        assert_eq!(persisted.read_u64()?, 22);
        assert!(buffers.used_memory() <= (2 * DEFAULT_BLOCK_ALLOC_SIZE) as u64);
        drop(reader);
        assert!(buffers.used_memory() <= DEFAULT_BLOCK_ALLOC_SIZE as u64);
        drop(persisted);
        drop(metadata);
        drop(buffers);
        std::fs::remove_file(path)?;
        Ok(())
    }
}
