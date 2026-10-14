//! Block managers: where persistent blocks are read from and written to (DuckDB's
//! `BlockManager`).
//!
//! The buffer manager only needs a block manager to load and store whole blocks; the
//! single-file storage format (free lists, checkpoints, metadata) plugs in later by
//! implementing [`BlockManager`]. [`BlockRegistry`] provides the shared bookkeeping of
//! which persistent blocks currently have a handle.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use rustdb_common::{Error, Result};

use crate::block::{BlockHandle, BlockId};
use crate::file_buffer::FileBuffer;
use crate::{DEFAULT_BLOCK_ALLOC_SIZE, DEFAULT_BLOCK_HEADER_STORAGE_SIZE};

/// Reads and writes persistent blocks.
///
/// Implementations must be thread-safe: blocks are loaded on demand from any thread.
pub trait BlockManager: Send + Sync + std::fmt::Debug {
    /// Allocation size of one block, header included.
    fn block_alloc_size(&self) -> usize;

    /// Size of the per-block header (the checksum).
    fn block_header_size(&self) -> usize;

    /// Usable size of one block.
    fn block_size(&self) -> usize {
        self.block_alloc_size() - self.block_header_size()
    }

    /// Whether this manager keeps everything in memory (no file).
    fn in_memory(&self) -> bool;

    /// Fills `buffer` (header included) with the contents of block `block_id`.
    fn read(&self, block_id: BlockId, buffer: &mut FileBuffer) -> Result<()>;

    /// Writes `buffer` (header included) as block `block_id`.
    fn write(&self, block_id: BlockId, buffer: &FileBuffer) -> Result<()>;

    /// The registry of live handles of this manager's blocks.
    fn registry(&self) -> &BlockRegistry;

    /// Forgets the handle of a persistent block (called when the handle is dropped).
    fn unregister_block(&self, block_id: BlockId) {
        self.registry().unregister(block_id);
    }
}

/// Map from persistent block id to its live handle (DuckDB's `BlockManager::blocks`), so a
/// block registered twice yields the same handle.
#[derive(Debug, Default)]
pub struct BlockRegistry {
    blocks: Mutex<HashMap<BlockId, Weak<BlockHandle>>>,
}

impl BlockRegistry {
    /// The live handle of `block_id`, or a new one created by `create` and registered.
    pub(crate) fn get_or_register(
        &self,
        block_id: BlockId,
        create: impl FnOnce() -> Arc<BlockHandle>,
    ) -> Arc<BlockHandle> {
        let mut blocks = self.blocks.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = blocks.get(&block_id).and_then(Weak::upgrade) {
            return existing;
        }
        let handle = create();
        blocks.insert(block_id, Arc::downgrade(&handle));
        handle
    }

    /// The live handle of `block_id`, if any (`BlockManager::TryGetBlock`).
    pub fn get(&self, block_id: BlockId) -> Option<Arc<BlockHandle>> {
        let blocks = self.blocks.lock().unwrap_or_else(PoisonError::into_inner);
        blocks.get(&block_id).and_then(Weak::upgrade)
    }

    /// Whether `block_id` has a live handle (`BlockManager::BlockIsRegistered`).
    pub fn is_registered(&self, block_id: BlockId) -> bool {
        self.get(block_id).is_some()
    }

    pub(crate) fn unregister(&self, block_id: BlockId) {
        let mut blocks = self.blocks.lock().unwrap_or_else(PoisonError::into_inner);
        // A handle registered after the dropped one must stay.
        if blocks.get(&block_id).is_some_and(|weak| weak.strong_count() == 0) {
            blocks.remove(&block_id);
        }
    }
}

/// The block manager of an in-memory database: it has no file, so it cannot read or write
/// blocks (DuckDB's `InMemoryBlockManager`).
#[derive(Debug)]
pub struct InMemoryBlockManager {
    block_alloc_size: usize,
    block_header_size: usize,
    registry: BlockRegistry,
}

impl Default for InMemoryBlockManager {
    fn default() -> Self {
        Self::new(DEFAULT_BLOCK_ALLOC_SIZE, DEFAULT_BLOCK_HEADER_STORAGE_SIZE)
    }
}

impl InMemoryBlockManager {
    pub fn new(block_alloc_size: usize, block_header_size: usize) -> Self {
        Self {
            block_alloc_size,
            block_header_size,
            registry: BlockRegistry::default(),
        }
    }
}

impl BlockManager for InMemoryBlockManager {
    fn block_alloc_size(&self) -> usize {
        self.block_alloc_size
    }

    fn block_header_size(&self) -> usize {
        self.block_header_size
    }

    fn in_memory(&self) -> bool {
        true
    }

    fn read(&self, _block_id: BlockId, _buffer: &mut FileBuffer) -> Result<()> {
        Err(Error::internal("Cannot perform IO in in-memory database - Read!"))
    }

    fn write(&self, _block_id: BlockId, _buffer: &FileBuffer) -> Result<()> {
        Err(Error::internal(
            "Cannot perform IO in in-memory database - Write!",
        ))
    }

    fn registry(&self) -> &BlockRegistry {
        &self.registry
    }
}
