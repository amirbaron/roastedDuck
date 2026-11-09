//! The blocks of a column data collection (DuckDB's `ColumnDataAllocator` with the
//! `BUFFER_MANAGER_ALLOCATOR`).
//!
//! Vectors and string heaps are carved out of buffer-managed blocks one after the other;
//! a block is only left behind when the next allocation does not fit. The blocks cannot
//! be destroyed when evicted: they are spilled to the temporary directory, and an
//! allocation fails with an out-of-memory error when they cannot be.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roastedduck_buffer::{
    BlockHandle, BufferHandle, BufferManager, DEFAULT_BLOCK_HEADER_STORAGE_SIZE, DestroyBufferUpon, MemoryTag,
};
use roastedduck_common::{Error, Result};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A block and how much of it is in use (`BlockMetaData`).
#[derive(Debug)]
struct BlockMetaData {
    handle: Arc<BlockHandle>,
    size: u32,
    capacity: u32,
}

impl BlockMetaData {
    /// The bytes still free (`BlockMetaData::Capacity`).
    fn remaining(&self) -> u32 {
        self.capacity - self.size
    }
}

/// The pins a reader or appender holds on the blocks of the current chunk
/// (`ChunkManagementState`). Vectors read from the chunk share the pins of the string
/// heaps they point into.
#[derive(Debug, Default)]
pub struct ChunkManagementState {
    handles: Vec<(u32, Arc<BufferHandle>)>,
}

impl ChunkManagementState {
    pub(crate) fn find(&self, block_id: u32) -> Option<&Arc<BufferHandle>> {
        self.handles
            .iter()
            .find(|(id, _)| *id == block_id)
            .map(|(_, handle)| handle)
    }

    fn insert(&mut self, block_id: u32, handle: BufferHandle) {
        self.handles.retain(|(id, _)| *id != block_id);
        self.handles.push((block_id, Arc::new(handle)));
    }

    /// Releases every pin.
    pub fn clear(&mut self) {
        self.handles.clear();
    }
}

/// Allocates the vectors and string heaps of one or more collection segments in
/// buffer-managed blocks (`ColumnDataAllocator`).
#[derive(Debug)]
pub struct ColumnDataAllocator {
    buffer_manager: BufferManager,
    /// The tag the blocks are charged to (DuckDB's is always `COLUMN_DATA`; the window
    /// operator's hash groups charge theirs to the operator's).
    tag: MemoryTag,
    /// The usable size of a block (`BufferManager::GetBlockSize`: the allocation size
    /// without the block header).
    block_size: usize,
    blocks: Mutex<Vec<BlockMetaData>>,
    partition_index: Option<usize>,
}

impl ColumnDataAllocator {
    pub fn new(buffer_manager: BufferManager, tag: MemoryTag) -> Self {
        Self {
            block_size: buffer_manager.block_alloc_size() - DEFAULT_BLOCK_HEADER_STORAGE_SIZE,
            buffer_manager,
            tag,
            blocks: Mutex::new(Vec::new()),
            partition_index: None,
        }
    }

    pub fn set_partition_index(&mut self, index: usize) -> Result<()> {
        if self.partition_index.is_some() || !lock(&self.blocks).is_empty() {
            return Err(Error::internal(
                "ColumnDataAllocator partition must be set once before allocating",
            ));
        }
        self.partition_index = Some(index);
        Ok(())
    }

    /// The usable size of a block.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// The number of blocks.
    #[cfg(test)]
    pub fn block_count(&self) -> usize {
        lock(&self.blocks).len()
    }

    /// The bytes of the blocks in use (`SizeInBytes`).
    pub fn size_in_bytes(&self) -> usize {
        lock(&self.blocks).iter().map(|block| block.size as usize).sum()
    }

    /// The bytes allocated for the blocks (`AllocationSize`).
    pub fn allocation_size(&self) -> usize {
        lock(&self.blocks)
            .iter()
            .map(|block| block.capacity as usize)
            .sum()
    }

    /// Pins block `block_id` (`Pin`).
    pub fn pin(&self, block_id: u32) -> Result<BufferHandle> {
        let handle = {
            let blocks = lock(&self.blocks);
            let block = blocks
                .get(block_id as usize)
                .ok_or_else(|| Error::internal("ColumnDataAllocator: block out of range"))?;
            Arc::clone(&block.handle)
        };
        self.buffer_manager.pin(&handle)
    }

    /// Allocates a block of at least `size` bytes and returns it pinned (`AllocateBlock`).
    fn allocate_block(&self, blocks: &mut Vec<BlockMetaData>, size: usize) -> Result<BufferHandle> {
        let max_size = size.max(self.block_size());
        let pin = self.buffer_manager.allocate(self.tag, max_size, false)?;
        if let Some(index) = self.partition_index {
            pin.block().set_eviction_queue_index(index.bit_width() as usize);
        }
        let capacity = u32::try_from(max_size)
            .map_err(|_| Error::internal("ColumnDataAllocator: block larger than 4 GiB"))?;
        blocks.push(BlockMetaData {
            handle: Arc::clone(pin.block()),
            size: 0,
            capacity,
        });
        Ok(pin)
    }

    /// Reserves `size` bytes (`AllocateData` / `AllocateBuffer`): at the end of the last
    /// block, or in a new block when they do not fit. The block is pinned in `state`, if
    /// given. Returns `(block_id, offset)`.
    pub fn allocate_data(&self, size: usize, state: Option<&mut ChunkManagementState>) -> Result<(u32, u32)> {
        let mut blocks = lock(&self.blocks);
        let mut new_pin = None;
        if blocks
            .last()
            .is_none_or(|block| (block.remaining() as usize) < size)
        {
            new_pin = Some(self.allocate_block(&mut blocks, size)?);
        }
        let block_id = u32::try_from(blocks.len() - 1)
            .map_err(|_| Error::internal("ColumnDataAllocator: too many blocks"))?;
        let Some(block) = blocks.last_mut() else {
            unreachable!("a block was just ensured")
        };
        let offset = block.size;
        block.size +=
            u32::try_from(size).map_err(|_| Error::internal("ColumnDataAllocator: allocation too large"))?;
        if let Some(state) = state {
            if let Some(pin) = new_pin {
                state.insert(block_id, pin);
            } else if state.find(block_id).is_none() {
                // Not necessarily pinned by this appender yet.
                let handle = Arc::clone(&block.handle);
                drop(blocks);
                state.insert(block_id, self.buffer_manager.pin(&handle)?);
            }
        }
        Ok((block_id, offset))
    }

    /// The address of `offset` in block `block_id`, which `state` pins
    /// (`GetDataPointer`).
    pub fn data_pointer(state: &ChunkManagementState, block_id: u32, offset: u32) -> Result<*mut u8> {
        let handle = state
            .find(block_id)
            .ok_or_else(|| Error::internal("ColumnDataAllocator: block of the chunk is not pinned"))?;
        // SAFETY: the allocation at `offset` lies inside the pinned block.
        Ok(unsafe { handle.as_ptr().add(offset as usize) })
    }

    /// Pins the blocks of a chunk and releases the pins of blocks it does not use
    /// (`InitializeChunkState`).
    pub fn initialize_chunk_state(&self, state: &mut ChunkManagementState, block_ids: &[u32]) -> Result<()> {
        state.handles.retain(|(id, _)| block_ids.contains(id));
        for &block_id in block_ids {
            if state.find(block_id).is_none() {
                let handle = self.pin(block_id)?;
                state.handles.push((block_id, Arc::new(handle)));
            }
        }
        Ok(())
    }
}

impl Drop for ColumnDataAllocator {
    fn drop(&mut self) {
        // The blocks are freed with their handles; nothing is spilled on the way.
        for block in lock(&self.blocks).iter() {
            block.handle.set_destroy_buffer_upon(DestroyBufferUpon::Unpin);
        }
    }
}
