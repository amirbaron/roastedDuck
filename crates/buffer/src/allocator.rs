//! The memory source for buffer-managed blocks (DuckDB's `BlockAllocator`).
//!
//! Blocks are page-aligned OS mappings. Freed blocks of the default size are kept in a
//! small sharded pool so the next allocation skips the system call and the page faults;
//! everything above the retention limit, and the whole pool on [`BlockAllocator::flush`],
//! is unmapped so the memory really goes back to the OS. This replaces the role jemalloc
//! plays for DuckDB without keeping allocator slack around after a pipeline finishes.

use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::os;

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
            mapped_bytes: AtomicUsize::new(0),
        }
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
            .saturating_sub(self.retain_blocks.load(Ordering::Relaxed));
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
        if size == self.block_size
            && self.pooled.load(Ordering::Relaxed) < self.retain_blocks.load(Ordering::Relaxed)
        {
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

    /// Returns every pooled block to the OS.
    pub fn flush(&self) {
        self.release_blocks(usize::MAX);
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
        self.flush();
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
