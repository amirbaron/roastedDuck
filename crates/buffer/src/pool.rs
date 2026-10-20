//! The buffer pool: memory limit, usage accounting and eviction (DuckDB's `BufferPool`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use rustdb_common::Result;

use crate::allocator::BlockAllocator;
use crate::block::{BlockInner, BlockMemory};
use crate::error::out_of_memory;
use crate::eviction::{EvictionNode, EvictionQueue};
use crate::file_buffer::{FileBuffer, FileBufferType};
use crate::memory::{MemoryReservation, MemoryUsage};
use crate::memory_tag::MemoryTag;
use crate::temporary_memory::TemporaryMemoryManager;

/// Queues per buffer type, in eviction priority order: database blocks and external
/// file blocks are cheapest to evict (just drop them), managed buffers must be written
/// out, and tiny buffers are evicted last. Managed buffers get several queues so
/// partitioned operators can evict their largest partitions first.
const QUEUE_SIZES: [usize; 3] = [1, 6, 1];

fn queue_type_index(kind: FileBufferType) -> usize {
    match kind {
        FileBufferType::Block | FileBufferType::ExternalFile => 0,
        FileBufferType::ManagedBuffer => 1,
        FileBufferType::TinyBuffer => 2,
    }
}

/// Outcome of trying to make room for an allocation.
pub(crate) struct Eviction {
    /// Whether used memory plus the request fits the limit.
    pub(crate) success: bool,
    /// The memory charged for the request (empty on failure).
    pub(crate) reservation: MemoryReservation,
    /// An evicted buffer of exactly the requested size, if one could be reused.
    pub(crate) reusable: Option<FileBuffer>,
}

/// Memory management shared by one or more databases: the memory limit, usage counters
/// per tag and the eviction queues.
pub struct BufferPool {
    maximum_memory: AtomicU64,
    limit_lock: Mutex<()>,
    usage: MemoryUsage,
    queues: Vec<EvictionQueue>,
    track_eviction_timestamps: bool,
    epoch: Instant,
    allocator: Arc<BlockAllocator>,
    bulk_deallocation_flush_threshold: AtomicU64,
    temporary_memory: TemporaryMemoryManager,
    /// Bytes of unpinned table data kept loaded (see [`BufferPool::table_cache_limit`]).
    cached_table_data: AtomicU64,
}

impl std::fmt::Debug for BufferPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferPool")
            .field("maximum_memory", &self.max_memory())
            .field("used_memory", &self.used_memory(false))
            .finish_non_exhaustive()
    }
}

/// The size of the table cache, at most an eighth of the memory limit
/// ([`BufferPool::table_cache_limit`]).
const TABLE_CACHE_LIMIT: u64 = 96 << 20;

/// DuckDB's default `allocator_bulk_deallocation_flush_threshold` (512 MiB).
pub const DEFAULT_BULK_DEALLOCATION_FLUSH_THRESHOLD: u64 = 512 << 20;

impl BufferPool {
    /// A pool with the given limit that allocates buffers from `allocator`.
    pub fn new(
        maximum_memory: u64,
        allocator: Arc<BlockAllocator>,
        track_eviction_timestamps: bool,
    ) -> Arc<Self> {
        let queues = QUEUE_SIZES
            .iter()
            .flat_map(|&count| (0..count).map(|_| EvictionQueue::new()))
            .collect();
        Arc::new(Self {
            maximum_memory: AtomicU64::new(maximum_memory),
            limit_lock: Mutex::new(()),
            usage: MemoryUsage::new(),
            queues,
            track_eviction_timestamps,
            epoch: Instant::now(),
            allocator,
            bulk_deallocation_flush_threshold: AtomicU64::new(DEFAULT_BULK_DEALLOCATION_FLUSH_THRESHOLD),
            temporary_memory: TemporaryMemoryManager::new(),
            cached_table_data: AtomicU64::new(0),
        })
    }

    /// The most unpinned table data (persistent blocks released on unpin) kept loaded:
    /// DuckDB keeps every block it read until memory runs short, which makes repeated
    /// scans skip reading the file but keeps the whole scanned data resident. A bounded
    /// cache keeps the first blocks read (it does not replace them, so a scan larger than
    /// the cache still finds part of its data) while every other block is read again.
    pub fn table_cache_limit(&self) -> u64 {
        TABLE_CACHE_LIMIT.min(self.max_memory() / 8)
    }

    /// Counts `size` bytes of table data as kept loaded, if they fit the table cache and
    /// the pool's whole usage (operators' data too) is below the cache size: queries that
    /// need memory for their hash tables or sorts do not keep table data as well.
    pub(crate) fn try_cache_table_data(&self, size: u64) -> bool {
        let limit = self.table_cache_limit();
        if self.used_memory(false) + size > limit {
            return false;
        }
        self.cached_table_data
            .try_update(Ordering::AcqRel, Ordering::Acquire, |cached| {
                (cached + size <= limit).then_some(cached + size)
            })
            .is_ok()
    }

    /// `size` bytes of cached table data were unloaded.
    pub(crate) fn uncache_table_data(&self, size: u64) {
        self.cached_table_data.fetch_sub(size, Ordering::AcqRel);
    }

    /// The allocator buffers are allocated from.
    pub fn allocator(&self) -> &Arc<BlockAllocator> {
        &self.allocator
    }

    /// Bytes currently in use; `flush` folds the per-thread caches in first (exact).
    pub fn used_memory(&self, flush: bool) -> u64 {
        self.usage.total(flush)
    }

    /// Bytes in use for one tag (exact).
    pub fn used_memory_by_tag(&self, tag: MemoryTag) -> u64 {
        self.usage.tag(tag, true)
    }

    /// The memory limit.
    pub fn max_memory(&self) -> u64 {
        self.maximum_memory.load(Ordering::Relaxed)
    }

    /// The memory limit of a single query (currently the global limit, like DuckDB).
    pub fn query_max_memory(&self) -> u64 {
        self.max_memory()
    }

    pub fn temporary_memory_manager(&self) -> &TemporaryMemoryManager {
        &self.temporary_memory
    }

    pub fn set_bulk_deallocation_flush_threshold(&self, threshold: u64) {
        self.bulk_deallocation_flush_threshold
            .store(threshold, Ordering::Relaxed);
    }

    pub fn bulk_deallocation_flush_threshold(&self) -> u64 {
        self.bulk_deallocation_flush_threshold.load(Ordering::Relaxed)
    }

    pub(crate) fn update_used_memory(&self, tag: MemoryTag, delta: i64) {
        self.usage.update(tag, delta);
    }

    /// Milliseconds since the pool was created, for LRU timestamps.
    fn now_ms(&self) -> i64 {
        self.epoch.elapsed().as_millis() as i64
    }

    /// Changes the memory limit, evicting blocks to get under it
    /// (`BufferPool::SetLimit`). `postscript` is appended to the error message.
    pub fn set_limit(self: &Arc<Self>, limit: u64, postscript: &str) -> Result<()> {
        let _guard = self.limit_lock.lock().unwrap_or_else(PoisonError::into_inner);
        let error = || {
            out_of_memory(format!(
                "Failed to change memory limit to {limit}: could not free up enough memory for the new limit{postscript}"
            ))
        };
        if !self.evict_blocks(MemoryTag::Extension, 0, limit, false)?.success {
            return Err(error());
        }
        let old_limit = self.maximum_memory.swap(limit, Ordering::Relaxed);
        if !self.evict_blocks(MemoryTag::Extension, 0, limit, false)?.success {
            self.maximum_memory.store(old_limit, Ordering::Relaxed);
            return Err(error());
        }
        self.allocator.flush();
        Ok(())
    }

    fn queue_for(&self, memory: &BlockMemory) -> &EvictionQueue {
        let type_index = queue_type_index(memory.buffer_type());
        let mut index: usize = QUEUE_SIZES[..type_index].iter().sum();
        let queue_size = QUEUE_SIZES[type_index];
        // A set eviction queue index places the block in an earlier (evicted sooner)
        // queue: index 0 is the back, larger indices move towards the front.
        let requested = memory.eviction_queue_index();
        if requested < queue_size {
            index += queue_size - requested - 1;
        }
        &self.queues[index]
    }

    /// Adds an unpinned block to its eviction queue; requires the block's lock. Returns
    /// whether the queue is due for a purge.
    pub(crate) fn add_to_eviction_queue(
        &self,
        memory: &Arc<BlockMemory>,
        inner: &mut MutexGuard<'_, BlockInner>,
    ) -> bool {
        let queue = self.queue_for(memory);
        if inner.has_queue_entry {
            // Count the previous entry as dead before bumping the sequence number, so a
            // concurrent purge never sees it stale before it is counted.
            queue.increment_dead_nodes();
        }
        let sequence = memory.next_eviction_sequence();
        if self.track_eviction_timestamps {
            memory.set_lru_timestamp(self.now_ms());
        }
        inner.has_queue_entry = true;
        queue.push(EvictionNode {
            memory: Arc::downgrade(memory),
            sequence,
        })
    }

    pub(crate) fn increment_dead_nodes(&self, memory: &BlockMemory) {
        self.queue_for(memory).increment_dead_nodes();
    }

    /// Garbage-collects the eviction queue of `memory`.
    pub(crate) fn purge_queue(&self, memory: &BlockMemory) {
        self.queue_for(memory).purge();
    }

    /// Evicts blocks until the used memory plus `extra_memory` fits in `memory_limit`
    /// (`BufferPool::EvictBlocks`). On success the returned reservation charges
    /// `extra_memory` to `tag`. With `reuse`, an evicted buffer whose allocation size is
    /// exactly `extra_memory` is handed back for reuse instead of being freed.
    pub(crate) fn evict_blocks(
        self: &Arc<Self>,
        tag: MemoryTag,
        extra_memory: u64,
        memory_limit: u64,
        reuse: bool,
    ) -> Result<Eviction> {
        for queue in &self.queues {
            let eviction = self.evict_from_queue(queue, tag, extra_memory, memory_limit, reuse)?;
            if eviction.success {
                return Ok(eviction);
            }
        }
        // DuckDB also evicts its object cache here; there is none yet.
        Ok(Eviction {
            success: false,
            reservation: MemoryReservation::empty(Arc::clone(self), tag),
            reusable: None,
        })
    }

    fn evict_from_queue(
        self: &Arc<Self>,
        queue: &EvictionQueue,
        tag: MemoryTag,
        extra_memory: u64,
        memory_limit: u64,
        reuse: bool,
    ) -> Result<Eviction> {
        let mut reservation = MemoryReservation::new(Arc::clone(self), tag, extra_memory);
        let mut eviction = Eviction {
            success: false,
            reservation: MemoryReservation::empty(Arc::clone(self), tag),
            reusable: None,
        };
        if self.usage.total(false) <= memory_limit {
            self.flush_bulk(extra_memory);
            eviction.success = true;
            eviction.reservation = reservation;
            return Ok(eviction);
        }
        self.iterate_unloadable_blocks(queue, |memory, inner| {
            if reuse
                && let Some(buffer) = &inner.buffer
                && buffer.alloc_size() as u64 == extra_memory
            {
                eviction.reusable = memory.unload_and_take(inner)?;
                eviction.success = true;
                return Ok(false);
            }
            memory.unload(inner)?;
            if self.usage.total(false) <= memory_limit {
                eviction.success = true;
                return Ok(false);
            }
            Ok(true)
        })?;
        if eviction.success {
            self.flush_bulk(extra_memory);
            eviction.reservation = reservation;
        } else {
            reservation.resize(0);
        }
        Ok(eviction)
    }

    /// Releases pooled allocator memory after a large reservation so it is backed by
    /// memory the OS can actually hand out.
    fn flush_bulk(&self, extra_memory: u64) {
        if extra_memory > self.bulk_deallocation_flush_threshold() {
            self.allocator.release(extra_memory as usize);
        }
    }

    /// Dequeues entries until `f` returns `false`, calling it for every block that can be
    /// unloaded right now, with the block's lock held.
    fn iterate_unloadable_blocks(
        &self,
        queue: &EvictionQueue,
        mut f: impl FnMut(&Arc<BlockMemory>, &mut MutexGuard<'_, BlockInner>) -> Result<bool>,
    ) -> Result<()> {
        while let Some(node) = queue.pop() {
            let Some(memory) = node.memory.upgrade() else {
                queue.decrement_dead_nodes();
                continue;
            };
            let mut inner = memory.lock();
            if node.sequence != memory.eviction_sequence() {
                // Superseded by a newer entry, which counted this one as dead.
                queue.decrement_dead_nodes();
                continue;
            }
            // The live entry was just dequeued.
            inner.has_queue_entry = false;
            if !memory.can_unload() {
                // Pinned again (or otherwise not unloadable); it is re-queued on unpin.
                continue;
            }
            match f(&memory, &mut inner) {
                Ok(true) => {}
                Ok(false) => return Ok(()),
                Err(error) => {
                    // Keep the block evictable later.
                    self.add_to_eviction_queue(&memory, &mut inner);
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// Unloads every unpinned block that was not used in the last `max_age_sec` seconds
    /// (`BufferPool::PurgeAgedBlocks`). Requires timestamp tracking; returns bytes freed.
    pub fn purge_aged_blocks(&self, max_age_sec: u32) -> Result<u64> {
        let now = self.now_ms();
        let limit = now - i64::from(max_age_sec) * 1000;
        let mut purged = 0;
        for queue in &self.queues {
            self.iterate_unloadable_blocks(queue, |memory, inner| {
                let timestamp = memory.lru_timestamp();
                let fresh = timestamp >= limit && timestamp <= now;
                purged += memory.memory_usage();
                memory.unload(inner)?;
                Ok(!fresh)
            })?;
        }
        Ok(purged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eviction_queue_index_moves_blocks_towards_the_front() {
        // Managed buffers occupy queues 1..=6; index 0 is the last of them.
        assert_eq!(QUEUE_SIZES.iter().sum::<usize>(), 8);
        assert_eq!(queue_type_index(FileBufferType::ManagedBuffer), 1);
        assert_eq!(queue_type_index(FileBufferType::ExternalFile), 0);
    }
}
