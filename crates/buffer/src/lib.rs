//! Buffer management (DuckDB's `storage/buffer`, `BufferManager`, `BufferPool`,
//! temporary files and memory reservations).
//!
//! Every large allocation of the engine goes through a [`BufferManager`]: it is charged
//! against the memory limit under a [`MemoryTag`], and when the limit is reached unpinned
//! blocks are evicted — dropped if they can be re-read or recreated, otherwise spilled to
//! the temporary directory and read back on the next [`BufferManager::pin`].
//!
//! # Structure
//! - [`BlockHandle`]: a block that may be loaded or evicted; [`BufferHandle`]: an RAII pin
//!   keeping it loaded at a fixed address.
//! - [`BufferPool`]: memory limit, per-tag usage counters, eviction queues. One pool can
//!   be shared by several databases.
//! - [`BlockManager`]: reads and writes persistent blocks (the database file).
//! - [`TemporaryMemoryManager`]: divides memory between concurrently running operators.
//! - [`ArenaAllocator`]: bump allocation for small objects, charged to the buffer manager.
//!
//! # Differences from DuckDB (internal only)
//! - Temporary (in-memory) blocks have no header, so their full 256 KiB are usable and the
//!   data is page-aligned. Persistent blocks keep the 8-byte checksum header.
//! - Spilled buffers are stored uncompressed (no zstd).
//! - Block memory comes straight from the OS and is returned to it when freed beyond a
//!   small reuse pool, instead of being retained by a general-purpose allocator.

mod allocator;
mod arena;
mod block;
mod block_manager;
mod bytes;
mod error;
mod eviction;
mod file_buffer;
mod manager;
mod memory;
mod memory_tag;
mod os;
mod pool;
mod temp_file;
mod temporary_memory;

pub use allocator::BlockAllocator;
pub use arena::{ARENA_ALLOCATOR_INITIAL_CAPACITY, ARENA_ALLOCATOR_MAX_CAPACITY, ArenaAllocator};
pub use block::{BlockHandle, BlockId, BlockState, BufferHandle, DestroyBufferUpon};
pub use block_manager::{BlockManager, BlockRegistry, InMemoryBlockManager};
pub use bytes::{format_bytes, parse_memory_limit, try_parse_formatted_bytes};
pub use error::out_of_memory;
pub use file_buffer::{FileBuffer, FileBufferType};
pub use manager::{
    AllocatedData, BufferManager, BufferManagerConfig, MemoryInformation, default_memory_limit,
};
pub use memory::MemoryReservation;
pub use memory_tag::{MEMORY_TAG_COUNT, MemoryTag};
pub use os::{available_disk_space, physical_memory};
pub use pool::{BufferPool, DEFAULT_BULK_DEALLOCATION_FLUSH_THRESHOLD};
pub use temp_file::{TemporaryBufferSize, TemporaryFileInformation};
pub use temporary_memory::{TemporaryMemoryContext, TemporaryMemoryManager, TemporaryMemoryState};

/// Allocation size of a default block (256 KiB).
pub const DEFAULT_BLOCK_ALLOC_SIZE: usize = 262_144;
/// Header size of persistent blocks (the checksum).
pub const DEFAULT_BLOCK_HEADER_STORAGE_SIZE: usize = 8;
/// Header size of temporary blocks (see the crate documentation).
pub const TEMPORARY_BLOCK_HEADER_SIZE: usize = 0;
/// Buffers that may be written to disk are allocated in multiples of this.
pub const SECTOR_SIZE: usize = 4096;
/// Persistent block ids are below this; temporary block ids are at or above it.
pub const MAXIMUM_BLOCK: BlockId = 4_611_686_018_427_388_000;
/// The id of no block.
pub const INVALID_BLOCK: BlockId = -1;
