//! The memory source for buffer-managed blocks (DuckDB's `BlockAllocator`).
//!
//! Blocks are page-aligned OS mappings. Freed blocks of the default size are kept in a
//! small sharded pool so the next allocation skips the system call and the page faults;
//! everything above the retention limit, and the whole pool on [`BlockAllocator::flush`],
//! is unmapped so the memory really goes back to the OS. This replaces the role jemalloc
//! plays for DuckDB without keeping allocator slack around after a pipeline finishes.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use rustdb_common::{Error, Result};

use crate::os;

/// How long a freed large mapping stays mapped for reuse: the decay time DuckDB gives its
/// allocator (`DUCKDB_JEMALLOC_DECAY`, jemalloc's `dirty_decay_ms`).
const RETAIN_LARGE_MAPPINGS: Duration = Duration::from_secs(5);

/// A freed large mapping kept for reuse.
struct RetainedMapping {
    ptr: NonNull<u8>,
    size: usize,
    freed: Instant,
}

// SAFETY: a retained mapping is unused memory owned by the list; it carries no thread
// affinity.
unsafe impl Send for RetainedMapping {}

/// Large mappings freed in the last [`RETAIN_LARGE_MAPPINGS`], as DuckDB's allocator keeps
/// freed memory until its decay time: a query that allocates the same hash tables again
/// (every run of a repeated query) reuses their pages instead of faulting in new ones.
static RETAINED_MAPPINGS: Mutex<Vec<RetainedMapping>> = Mutex::new(Vec::new());

/// Maps `size` bytes (a multiple of the page size) for a large allocation, reusing a
/// retained mapping of exactly that size. Returns the memory and whether it is zeroed
/// (fresh mappings are, reused ones are not); `None` if the OS refuses the mapping. A
/// request no retained mapping fits unmaps all of them first, so that they do not add
/// to the memory the new one takes.
pub(crate) fn map_large(size: usize) -> Option<(NonNull<u8>, bool)> {
    let stale = {
        let mut retained = RETAINED_MAPPINGS.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let fresh = |mapping: &RetainedMapping| now.duration_since(mapping.freed) < RETAIN_LARGE_MAPPINGS;
        let reusable = retained
            .iter()
            .position(|mapping| mapping.size == size && fresh(mapping));
        if let Some(index) = reusable {
            let mapping = retained.swap_remove(index);
            return Some((mapping.ptr, false));
        }
        std::mem::take(&mut *retained)
    };
    for mapping in stale {
        // SAFETY: retained mappings are unused mappings of `size` bytes.
        unsafe { os::unmap(mapping.ptr, mapping.size) };
    }
    os::map(size).map(|ptr| (ptr, true))
}

/// Frees a mapping from [`map_large`]: it stays mapped for reuse for a while, and the
/// mappings freed longer ago are unmapped.
///
/// # Safety
/// `ptr` and `size` must come from one `map_large` call, and the memory must not be used
/// afterwards.
pub(crate) unsafe fn unmap_large(ptr: NonNull<u8>, size: usize) {
    let now = Instant::now();
    let expired: Vec<RetainedMapping> = {
        let mut retained = RETAINED_MAPPINGS.lock().unwrap_or_else(PoisonError::into_inner);
        retained.push(RetainedMapping {
            ptr,
            size,
            freed: now,
        });
        let (expired, kept) = std::mem::take(&mut *retained)
            .into_iter()
            .partition(|mapping| now.duration_since(mapping.freed) >= RETAIN_LARGE_MAPPINGS);
        *retained = kept;
        expired
    };
    for mapping in expired {
        // SAFETY: retained mappings are unused mappings of `size` bytes.
        unsafe { os::unmap(mapping.ptr, mapping.size) };
    }
}

/// A pooled block address.
struct PooledBlock(NonNull<u8>);

// SAFETY: a pooled block is unused memory owned by the pool; it carries no thread affinity.
unsafe impl Send for PooledBlock {}

/// Number of pool shards; threads use the shard of their slot to avoid contention.
const SHARDS: usize = 16;

/// Hands out page-aligned memory and recycles default-size blocks.
pub struct BlockAllocator {
    block_size: usize,
    shards: Box<[Mutex<Vec<PooledBlock>>]>,
    pooled: AtomicUsize,
    retain_blocks: AtomicUsize,
    /// The memory set aside for blocks that is never given back (the
    /// `block_allocator_memory` setting, DuckDB's `physical_memory_size`): freed blocks
    /// up to this size stay pooled whatever the retention limit.
    physical_memory_size: AtomicUsize,
    mapped_bytes: AtomicUsize,
}

impl std::fmt::Debug for BlockAllocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockAllocator")
            .field("block_size", &self.block_size)
            .field("pooled", &self.pooled.load(Ordering::Relaxed))
            .field("mapped_bytes", &self.mapped_bytes.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl BlockAllocator {
    /// An allocator recycling blocks of `block_size` bytes, keeping at most `retain_bytes`
    /// of freed blocks for reuse.
    pub fn new(block_size: usize, retain_bytes: usize) -> Self {
        Self {
            block_size,
            shards: (0..SHARDS).map(|_| Mutex::new(Vec::new())).collect(),
            pooled: AtomicUsize::new(0),
            retain_blocks: AtomicUsize::new(retain_bytes / block_size.max(1)),
            physical_memory_size: AtomicUsize::new(0),
            mapped_bytes: AtomicUsize::new(0),
        }
    }

    /// The memory set aside for blocks (`block_allocator_memory`).
    pub fn physical_memory_size(&self) -> usize {
        self.physical_memory_size.load(Ordering::Relaxed)
    }

    /// Sets the memory set aside for blocks (DuckDB's `BlockAllocator::Resize`): it can
    /// only grow, and only up to the virtual memory DuckDB reserves for blocks, 80% of
    /// the system's memory.
    pub fn resize(&self, size: usize) -> Result<()> {
        let virtual_memory_size =
            (crate::default_memory_limit() as usize).next_multiple_of(self.block_size.max(1));
        self.physical_memory_size
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                (size >= current && size <= virtual_memory_size).then_some(size)
            })
            .map(|_| ())
            .map_err(|current| {
                Error::invalid_input(if size < current {
                    format!("The \"block_allocator_size\" setting cannot be reduced (current: {current})")
                } else {
                    format!(
                        "The \"block_allocator_size\" setting cannot be greater than the virtual memory size (virtual memory size: {virtual_memory_size})"
                    )
                })
            })
    }

    /// How many freed blocks are kept for reuse.
    fn retained_blocks(&self) -> usize {
        let set_aside = self.physical_memory_size.load(Ordering::Relaxed) / self.block_size.max(1);
        self.retain_blocks.load(Ordering::Relaxed).max(set_aside)
    }

    /// The size of recycled blocks.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// Bytes currently mapped (in use or pooled).
    pub fn mapped_bytes(&self) -> usize {
        self.mapped_bytes.load(Ordering::Relaxed)
    }

    /// Bytes held in the reuse pool.
    pub fn pooled_bytes(&self) -> usize {
        self.pooled.load(Ordering::Relaxed) * self.block_size
    }

    /// Changes how many bytes of freed blocks are kept for reuse, releasing the excess.
    pub fn set_retention(&self, retain_bytes: usize) {
        self.retain_blocks
            .store(retain_bytes / self.block_size.max(1), Ordering::Relaxed);
        let excess = self
            .pooled
            .load(Ordering::Relaxed)
            .saturating_sub(self.retained_blocks());
        self.release_blocks(excess);
    }

    /// Allocates `size` bytes (a multiple of the page size), page-aligned. Memory is not
    /// necessarily zeroed. Returns `None` if the OS refuses the mapping.
    pub(crate) fn allocate(&self, size: usize) -> Option<NonNull<u8>> {
        if size == self.block_size
            && let Some(block) = self.take_pooled()
        {
            return Some(block);
        }
        let ptr = os::map(size)?;
        self.mapped_bytes.fetch_add(size, Ordering::Relaxed);
        Some(ptr)
    }

    /// Frees memory from [`BlockAllocator::allocate`].
    ///
    /// # Safety
    /// `ptr` and `size` must come from one `allocate` call on this allocator, and the
    /// memory must not be used afterwards.
    pub(crate) unsafe fn free(&self, ptr: NonNull<u8>, size: usize) {
        if size == self.block_size && self.pooled.load(Ordering::Relaxed) < self.retained_blocks() {
            let shard = os::thread_slot() % SHARDS;
            if let Ok(mut blocks) = self.shards[shard].lock() {
                blocks.push(PooledBlock(ptr));
                self.pooled.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        // SAFETY: guaranteed by the caller.
        unsafe { os::unmap(ptr, size) };
        self.mapped_bytes.fetch_sub(size, Ordering::Relaxed);
    }

    /// Returns the pooled blocks to the OS, except those of the memory set aside for
    /// blocks (`block_allocator_memory`).
    pub fn flush(&self) {
        let set_aside = self.physical_memory_size.load(Ordering::Relaxed) / self.block_size.max(1);
        self.release_blocks(self.pooled.load(Ordering::Relaxed).saturating_sub(set_aside));
    }

    /// Returns pooled blocks covering at least `bytes` to the OS (DuckDB's
    /// `FlushAll(extra_memory)`), so a large reservation is backed by real free memory.
    pub fn release(&self, bytes: usize) {
        self.release_blocks(bytes.div_ceil(self.block_size.max(1)));
    }

    fn take_pooled(&self) -> Option<NonNull<u8>> {
        if self.pooled.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let start = os::thread_slot();
        for i in 0..SHARDS {
            let Ok(mut blocks) = self.shards[(start + i) % SHARDS].lock() else {
                continue;
            };
            if let Some(PooledBlock(ptr)) = blocks.pop() {
                self.pooled.fetch_sub(1, Ordering::Relaxed);
                return Some(ptr);
            }
        }
        None
    }

    fn release_blocks(&self, mut count: usize) {
        for shard in &*self.shards {
            if count == 0 {
                return;
            }
            let Ok(mut blocks) = shard.lock() else {
                continue;
            };
            while count > 0 {
                let Some(PooledBlock(ptr)) = blocks.pop() else {
                    break;
                };
                self.pooled.fetch_sub(1, Ordering::Relaxed);
                // SAFETY: pooled blocks are unused mappings of `block_size` bytes.
                unsafe { os::unmap(ptr, self.block_size) };
                self.mapped_bytes.fetch_sub(self.block_size, Ordering::Relaxed);
                count -= 1;
            }
        }
    }
}

impl Drop for BlockAllocator {
    fn drop(&mut self) {
        self.release_blocks(usize::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recycles_default_blocks_and_releases_the_rest() {
        let allocator = BlockAllocator::new(1 << 16, 2 << 16);
        let a = allocator.allocate(1 << 16).unwrap();
        let b = allocator.allocate(1 << 16).unwrap();
        let c = allocator.allocate(1 << 16).unwrap();
        assert_eq!(allocator.mapped_bytes(), 3 << 16);
        // SAFETY: each block is freed once and not used afterwards.
        unsafe {
            allocator.free(a, 1 << 16);
            allocator.free(b, 1 << 16);
            allocator.free(c, 1 << 16);
        }
        // Only two blocks fit the retention limit.
        assert_eq!(allocator.pooled_bytes(), 2 << 16);
        assert_eq!(allocator.mapped_bytes(), 2 << 16);
        let d = allocator.allocate(1 << 16).unwrap();
        assert_eq!(allocator.pooled_bytes(), 1 << 16);
        // SAFETY: freed once.
        unsafe { allocator.free(d, 1 << 16) };
        allocator.flush();
        assert_eq!(allocator.mapped_bytes(), 0);
    }
}
