//! Block handles and pins (DuckDB's `BlockHandle`, `BlockMemory` and `BufferHandle`).
//!
//! A [`BlockHandle`] names a block of memory that may be loaded or evicted. Pinning it
//! yields a [`BufferHandle`], an RAII guard that keeps the block loaded at a fixed address
//! until it is dropped.
//!
//! Pinning is lock-free while a block already has readers: the reader count is bumped with
//! a compare-and-swap, which cannot race with eviction because a block is only unloaded
//! under its lock with zero readers. Only the first pin (which may have to load the
//! block) and the last unpin (which queues the block for eviction) take the block's lock.

use std::ptr::NonNull;
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicI64, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering,
};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_common::Result;

use crate::MAXIMUM_BLOCK;
use crate::block_manager::BlockManager;
use crate::file_buffer::{FileBuffer, FileBufferType};
use crate::manager::ManagerShared;
use crate::memory::MemoryReservation;
use crate::memory_tag::MemoryTag;

/// Identifier of a block: persistent blocks of a database file are below
/// [`MAXIMUM_BLOCK`], temporary (in-memory) blocks are at or above it.
pub type BlockId = i64;

/// When the data of a temporary block may be thrown away (DuckDB's `DestroyBufferUpon`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DestroyBufferUpon {
    /// Only when the block handle is destroyed: eviction writes it to a temporary file.
    Block = 0,
    /// On eviction: the data is dropped instead of written out.
    Eviction = 1,
    /// As soon as the block is unpinned (it never enters an eviction queue).
    Unpin = 2,
}

impl DestroyBufferUpon {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Eviction,
            2 => Self::Unpin,
            _ => Self::Block,
        }
    }
}

/// Whether a block's data is in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockState {
    Unloaded,
    Loaded,
}

/// State of a block guarded by its lock.
pub(crate) struct BlockInner {
    pub(crate) buffer: Option<FileBuffer>,
    /// Memory charged for the loaded buffer.
    pub(crate) charge: MemoryReservation,
    /// Whether the block's latest eviction queue entry is still in the queue.
    pub(crate) has_queue_entry: bool,
}

const NO_QUEUE_INDEX: usize = usize::MAX;

/// The memory of a block, shared with eviction queue entries (through `Weak`).
pub(crate) struct BlockMemory {
    pub(crate) shared: Arc<ManagerShared>,
    block_id: BlockId,
    tag: MemoryTag,
    buffer_type: FileBufferType,
    loaded: AtomicBool,
    readers: AtomicI32,
    /// Start and length of the loaded user data; valid while `readers > 0`.
    data: AtomicPtr<u8>,
    data_len: AtomicUsize,
    eviction_sequence: AtomicU64,
    lru_timestamp_ms: AtomicI64,
    destroy_upon: AtomicU8,
    /// Bytes to reserve when (re)loading the block.
    memory_usage: AtomicU64,
    eviction_queue_index: AtomicUsize,
    /// Whether the block was read (and its checksum verified) before.
    read_before: AtomicBool,
    inner: Mutex<BlockInner>,
}

impl BlockMemory {
    /// An unloaded block (persistent blocks start like this).
    pub(crate) fn unloaded(
        shared: Arc<ManagerShared>,
        block_id: BlockId,
        tag: MemoryTag,
        alloc_size: usize,
    ) -> Self {
        let charge = MemoryReservation::empty(Arc::clone(shared.pool()), tag);
        Self {
            shared,
            block_id,
            tag,
            buffer_type: FileBufferType::Block,
            loaded: AtomicBool::new(false),
            readers: AtomicI32::new(0),
            data: AtomicPtr::new(std::ptr::null_mut()),
            data_len: AtomicUsize::new(0),
            eviction_sequence: AtomicU64::new(0),
            lru_timestamp_ms: AtomicI64::new(0),
            destroy_upon: AtomicU8::new(DestroyBufferUpon::Block as u8),
            memory_usage: AtomicU64::new(alloc_size as u64),
            eviction_queue_index: AtomicUsize::new(NO_QUEUE_INDEX),
            read_before: AtomicBool::new(false),
            inner: Mutex::new(BlockInner {
                buffer: None,
                charge,
                has_queue_entry: false,
            }),
        }
    }

    /// A loaded block owning `buffer`, charged by `charge`.
    pub(crate) fn loaded(
        shared: Arc<ManagerShared>,
        block_id: BlockId,
        tag: MemoryTag,
        buffer: FileBuffer,
        destroy_upon: DestroyBufferUpon,
        memory_usage: u64,
        charge: MemoryReservation,
    ) -> Self {
        let data = buffer.data_ptr();
        let len = buffer.size();
        Self {
            shared,
            block_id,
            tag,
            buffer_type: buffer.kind(),
            loaded: AtomicBool::new(true),
            readers: AtomicI32::new(0),
            data: AtomicPtr::new(data),
            data_len: AtomicUsize::new(len),
            eviction_sequence: AtomicU64::new(0),
            lru_timestamp_ms: AtomicI64::new(0),
            destroy_upon: AtomicU8::new(destroy_upon as u8),
            memory_usage: AtomicU64::new(memory_usage),
            eviction_queue_index: AtomicUsize::new(NO_QUEUE_INDEX),
            read_before: AtomicBool::new(false),
            inner: Mutex::new(BlockInner {
                buffer: Some(buffer),
                charge,
                has_queue_entry: false,
            }),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, BlockInner> {
        // A panic while holding the lock leaves the block in a consistent state (every
        // mutation is a single assignment), so a poisoned lock is still usable.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn tag(&self) -> MemoryTag {
        self.tag
    }

    pub(crate) fn buffer_type(&self) -> FileBufferType {
        self.buffer_type
    }

    pub(crate) fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::Acquire)
    }

    pub(crate) fn readers(&self) -> i32 {
        self.readers.load(Ordering::Acquire)
    }

    pub(crate) fn memory_usage(&self) -> u64 {
        self.memory_usage.load(Ordering::Relaxed)
    }

    pub(crate) fn eviction_sequence(&self) -> u64 {
        self.eviction_sequence.load(Ordering::Acquire)
    }

    pub(crate) fn next_eviction_sequence(&self) -> u64 {
        self.eviction_sequence.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub(crate) fn lru_timestamp(&self) -> i64 {
        self.lru_timestamp_ms.load(Ordering::Relaxed)
    }

    pub(crate) fn set_lru_timestamp(&self, timestamp: i64) {
        self.lru_timestamp_ms.store(timestamp, Ordering::Relaxed);
    }

    pub(crate) fn eviction_queue_index(&self) -> usize {
        self.eviction_queue_index.load(Ordering::Relaxed)
    }

    pub(crate) fn destroy_upon(&self) -> DestroyBufferUpon {
        DestroyBufferUpon::from_u8(self.destroy_upon.load(Ordering::Relaxed))
    }

    fn is_temporary(&self) -> bool {
        self.block_id >= MAXIMUM_BLOCK
    }

    /// Whether unpinned data must go to the eviction queue (rather than be dropped).
    pub(crate) fn must_add_to_eviction_queue(&self) -> bool {
        self.destroy_upon() != DestroyBufferUpon::Unpin
    }

    /// Whether eviction must write the data to a temporary file to keep it.
    pub(crate) fn must_write_to_temporary_file(&self) -> bool {
        self.destroy_upon() == DestroyBufferUpon::Block
    }

    /// Whether the block can be unloaded right now (call with the lock held for a
    /// stable answer).
    pub(crate) fn can_unload(&self) -> bool {
        if !self.is_loaded() || self.readers() > 0 {
            return false;
        }
        // Without a temporary directory, data that must survive eviction cannot be
        // written out, so the block cannot be unloaded.
        !(self.is_temporary()
            && self.must_write_to_temporary_file()
            && !self.shared.has_temporary_directory())
    }

    /// Marks the block loaded with `buffer` (the caller holds the lock).
    pub(crate) fn install(&self, inner: &mut BlockInner, buffer: FileBuffer) {
        self.data.store(buffer.data_ptr(), Ordering::Relaxed);
        self.data_len.store(buffer.size(), Ordering::Relaxed);
        inner.buffer = Some(buffer);
        self.loaded.store(true, Ordering::Release);
    }

    /// Unloads the block, writing it to a temporary file if its data must be kept, and
    /// returns the buffer (`BlockMemory::UnloadAndTakeBlock`).
    pub(crate) fn unload_and_take(&self, inner: &mut BlockInner) -> Result<Option<FileBuffer>> {
        if !self.is_loaded() {
            return Ok(None);
        }
        if self.is_temporary()
            && self.must_write_to_temporary_file()
            && let Some(buffer) = &inner.buffer
        {
            self.shared
                .write_temporary_buffer(self.tag, self.block_id, buffer)?;
        }
        inner.charge.resize(0);
        self.loaded.store(false, Ordering::Release);
        self.data.store(std::ptr::null_mut(), Ordering::Relaxed);
        Ok(inner.buffer.take())
    }

    /// Unloads the block and frees its buffer.
    pub(crate) fn unload(&self, inner: &mut BlockInner) -> Result<()> {
        self.unload_and_take(inner).map(drop)
    }

    /// Adds a reader without taking the lock if the block already has readers.
    fn try_pin_fast(&self) -> Option<(NonNull<u8>, usize)> {
        let mut readers = self.readers.load(Ordering::Acquire);
        while readers > 0 {
            match self.readers.compare_exchange_weak(
                readers,
                readers + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let data = NonNull::new(self.data.load(Ordering::Acquire))?;
                    return Some((data, self.data_len.load(Ordering::Relaxed)));
                }
                Err(current) => readers = current,
            }
        }
        None
    }

    /// Adds a reader to a loaded block (the caller holds the lock).
    pub(crate) fn add_reader_locked(&self) -> Option<(NonNull<u8>, usize)> {
        debug_assert!(self.is_loaded());
        let data = NonNull::new(self.data.load(Ordering::Relaxed))?;
        self.readers.fetch_add(1, Ordering::AcqRel);
        Some((data, self.data_len.load(Ordering::Relaxed)))
    }

    /// Removes a reader without the lock if others remain; returns `false` if this may
    /// be the last reader (then the caller takes the slow path).
    fn try_unpin_fast(&self) -> bool {
        let mut readers = self.readers.load(Ordering::Acquire);
        while readers > 1 {
            match self.readers.compare_exchange_weak(
                readers,
                readers - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(current) => readers = current,
            }
        }
        false
    }

    /// Removes a reader with the lock held; returns the remaining count.
    pub(crate) fn remove_reader_locked(&self) -> i32 {
        self.readers.fetch_sub(1, Ordering::AcqRel) - 1
    }

    /// Changes the memory reserved for reloading (the caller holds the lock).
    pub(crate) fn set_memory_usage(&self, inner: &mut BlockInner, usage: u64) {
        self.memory_usage.store(usage, Ordering::Relaxed);
        inner.charge.resize(usage);
    }
}

impl Drop for BlockMemory {
    fn drop(&mut self) {
        let has_queue_entry = self
            .inner
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .has_queue_entry;
        if has_queue_entry && self.buffer_type != FileBufferType::TinyBuffer {
            // The entry in the eviction queue is dead now.
            self.shared.pool().increment_dead_nodes(self);
        }
        let inner = self.inner.get_mut().unwrap_or_else(PoisonError::into_inner);
        inner.buffer = None;
        inner.charge.resize(0);
        let loaded = *self.loaded.get_mut();
        if self.block_id >= MAXIMUM_BLOCK && !loaded {
            // The data may live in a temporary file; failing to delete it only leaks disk
            // space until the temporary directory is cleaned up.
            let _ = self
                .shared
                .delete_temporary_file(self.block_id, self.tag, *self.memory_usage.get_mut());
        }
    }
}

/// A block of buffer-managed memory that can be pinned (DuckDB's `BlockHandle`).
pub struct BlockHandle {
    block_id: BlockId,
    block_manager: Option<Arc<dyn BlockManager>>,
    pub(crate) memory: Arc<BlockMemory>,
}

impl std::fmt::Debug for BlockHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockHandle")
            .field("block_id", &self.block_id)
            .field("tag", &self.memory.tag())
            .field("state", &self.state())
            .field("readers", &self.readers())
            .finish_non_exhaustive()
    }
}

impl BlockHandle {
    pub(crate) fn new(
        block_id: BlockId,
        block_manager: Option<Arc<dyn BlockManager>>,
        memory: BlockMemory,
    ) -> Self {
        Self {
            block_id,
            block_manager,
            memory: Arc::new(memory),
        }
    }

    pub fn block_id(&self) -> BlockId {
        self.block_id
    }

    pub fn tag(&self) -> MemoryTag {
        self.memory.tag()
    }

    pub fn buffer_type(&self) -> FileBufferType {
        self.memory.buffer_type()
    }

    pub fn state(&self) -> BlockState {
        if self.memory.is_loaded() {
            BlockState::Loaded
        } else {
            BlockState::Unloaded
        }
    }

    /// Number of live pins.
    pub fn readers(&self) -> i32 {
        self.memory.readers()
    }

    /// Bytes the block occupies when loaded.
    pub fn memory_usage(&self) -> u64 {
        self.memory.memory_usage()
    }

    /// The block manager of a persistent block.
    pub fn block_manager(&self) -> Option<&Arc<dyn BlockManager>> {
        self.block_manager.as_ref()
    }

    /// Changes when the block's data may be destroyed.
    pub fn set_destroy_buffer_upon(&self, destroy_upon: DestroyBufferUpon) {
        self.memory
            .destroy_upon
            .store(destroy_upon as u8, Ordering::Relaxed);
    }

    pub fn destroy_buffer_upon(&self) -> DestroyBufferUpon {
        self.memory.destroy_upon()
    }

    /// Places the block in an earlier eviction queue (managed buffers only): larger
    /// indices are evicted sooner. Partitioned operators use the partition's radix bits so
    /// that bigger partitions spill first. Can be set once.
    pub fn set_eviction_queue_index(&self, index: usize) {
        let _ = self.memory.eviction_queue_index.compare_exchange(
            NO_QUEUE_INDEX,
            index,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    /// Loads the block's data (the caller holds the lock and has reserved memory).
    /// Returns `None` if the data was destroyed on eviction.
    pub(crate) fn load(
        &self,
        inner: &mut BlockInner,
        reusable: Option<FileBuffer>,
    ) -> Result<Option<(NonNull<u8>, usize)>> {
        let memory = &self.memory;
        if memory.is_loaded() {
            return Ok(memory.add_reader_locked());
        }
        let buffer = if let Some(block_manager) = &self.block_manager {
            let mut buffer = memory
                .shared
                .create_block_buffer(block_manager.as_ref(), reusable)?;
            if memory.read_before.load(Ordering::Relaxed) {
                block_manager.reread(self.block_id, &mut buffer)?;
            } else {
                block_manager.read(self.block_id, &mut buffer)?;
                memory.read_before.store(true, Ordering::Relaxed);
            }
            buffer
        } else {
            if !memory.must_write_to_temporary_file() {
                return Ok(None);
            }
            memory
                .shared
                .read_temporary_buffer(memory.tag(), self.block_id, reusable)?
        };
        memory.install(inner, buffer);
        Ok(memory.add_reader_locked())
    }
}

impl Drop for BlockHandle {
    fn drop(&mut self) {
        if let Some(block_manager) = &self.block_manager {
            block_manager.unregister_block(self.block_id);
        }
    }
}

/// A pin on a block: while it lives, the block stays loaded at [`BufferHandle::as_ptr`]
/// (DuckDB's `BufferHandle`). Dropping it unpins the block.
///
/// Pins do not synchronise access to the bytes: several pins (on several threads) may
/// read the same block, and writers must coordinate among themselves, as in DuckDB.
pub struct BufferHandle {
    block: Arc<BlockHandle>,
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: the pin owns a reference to the block and keeps it loaded; the pointer is only
// exposed as a raw pointer or through `unsafe` accessors with documented contracts.
unsafe impl Send for BufferHandle {}
// SAFETY: see above.
unsafe impl Sync for BufferHandle {}

impl std::fmt::Debug for BufferHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferHandle")
            .field("block_id", &self.block.block_id())
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl BufferHandle {
    pub(crate) fn new(block: Arc<BlockHandle>, ptr: NonNull<u8>, len: usize) -> Self {
        Self { block, ptr, len }
    }

    /// Pins `block` without the lock if it already has readers.
    pub(crate) fn try_pin_fast(block: &Arc<BlockHandle>) -> Option<Self> {
        let (ptr, len) = block.memory.try_pin_fast()?;
        Some(Self::new(Arc::clone(block), ptr, len))
    }

    /// The pinned block.
    pub fn block(&self) -> &Arc<BlockHandle> {
        &self.block
    }

    /// Start of the block's data.
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    /// Size of the block's data in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The block's data.
    ///
    /// # Safety
    /// No one may write the bytes (through any pin of this block) while the slice lives.
    pub unsafe fn as_slice(&self) -> &[u8] {
        // SAFETY: the pin keeps `len` bytes at `ptr` alive; no writers per the contract.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// The block's data for writing.
    ///
    /// # Safety
    /// No one else may access the bytes (through any pin of this block) while the slice
    /// lives.
    #[expect(clippy::mut_from_ref, reason = "pins are shared handles to raw block memory")]
    pub unsafe fn as_mut_slice(&self) -> &mut [u8] {
        // SAFETY: the pin keeps `len` bytes at `ptr` alive; exclusivity per the contract.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for BufferHandle {
    fn drop(&mut self) {
        if self.block.memory.buffer_type() == FileBufferType::TinyBuffer {
            // Tiny buffers are never evicted; pinning and unpinning them is a no-op.
            return;
        }
        if self.block.memory.try_unpin_fast() {
            return;
        }
        let shared = Arc::clone(&self.block.memory.shared);
        shared.unpin(&self.block);
    }
}
