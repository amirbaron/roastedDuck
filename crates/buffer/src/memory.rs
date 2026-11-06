//! Memory accounting: per-tag usage counters and reservations against them.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::memory_tag::{MEMORY_TAG_COUNT, MemoryTag};
use crate::os;
use crate::pool::BufferPool;

/// Index of the all-tags total in a counter array.
const TOTAL: usize = MEMORY_TAG_COUNT;
/// Number of per-thread caches; usage reported without flushing can lag the truth by at
/// most `CACHE_COUNT * CACHE_THRESHOLD` (2 MiB), like DuckDB's.
const CACHE_COUNT: usize = 64;
/// Updates smaller than this go to a thread's cache first.
const CACHE_THRESHOLD: i64 = 32 << 10;

type Counters = [AtomicI64; MEMORY_TAG_COUNT + 1];

fn counters() -> Counters {
    std::array::from_fn(|_| AtomicI64::new(0))
}

/// Used-memory counters per tag (DuckDB's `BufferPool::MemoryUsage`).
///
/// Small updates are buffered in per-thread caches so concurrent allocations do not
/// all hammer the same cache line; reads with `flush` fold the caches into the totals.
pub(crate) struct MemoryUsage {
    totals: Counters,
    caches: Box<[Counters]>,
}

impl MemoryUsage {
    pub(crate) fn new() -> Self {
        Self {
            totals: counters(),
            caches: (0..CACHE_COUNT).map(|_| counters()).collect(),
        }
    }

    /// Bytes in use for `index` (a tag index or [`TOTAL`]).
    fn used(&self, index: usize, flush: bool) -> u64 {
        if !flush {
            return self.totals[index].load(Ordering::Relaxed).max(0) as u64;
        }
        let cached: i64 = self
            .caches
            .iter()
            .map(|cache| cache[index].swap(0, Ordering::Relaxed))
            .sum();
        let used = self.totals[index].fetch_add(cached, Ordering::Relaxed) + cached;
        used.max(0) as u64
    }

    /// Total bytes in use.
    pub(crate) fn total(&self, flush: bool) -> u64 {
        self.used(TOTAL, flush)
    }

    /// Bytes in use for one tag.
    pub(crate) fn tag(&self, tag: MemoryTag, flush: bool) -> u64 {
        self.used(tag.index(), flush)
    }

    pub(crate) fn update(&self, tag: MemoryTag, delta: i64) {
        let tag = tag.index();
        if delta.abs() >= CACHE_THRESHOLD {
            self.totals[tag].fetch_add(delta, Ordering::Relaxed);
            self.totals[TOTAL].fetch_add(delta, Ordering::Relaxed);
            return;
        }
        let cache = &self.caches[os::thread_slot() % CACHE_COUNT];
        for index in [tag, TOTAL] {
            let cached = cache[index].fetch_add(delta, Ordering::Relaxed) + delta;
            if cached.abs() >= CACHE_THRESHOLD {
                let flushed = cache[index].swap(0, Ordering::Relaxed);
                self.totals[index].fetch_add(flushed, Ordering::Relaxed);
            }
        }
    }
}

/// Bytes charged to a tag of a [`BufferPool`]; released when dropped (DuckDB's
/// `BufferPoolReservation` / `TempBufferPoolReservation`).
pub struct MemoryReservation {
    pool: Arc<BufferPool>,
    tag: MemoryTag,
    size: u64,
}

impl std::fmt::Debug for MemoryReservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryReservation")
            .field("tag", &self.tag)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl MemoryReservation {
    /// Charges `size` bytes to `tag`.
    pub(crate) fn new(pool: Arc<BufferPool>, tag: MemoryTag, size: u64) -> Self {
        let mut reservation = Self { pool, tag, size: 0 };
        reservation.resize(size);
        reservation
    }

    /// A reservation of zero bytes.
    pub(crate) fn empty(pool: Arc<BufferPool>, tag: MemoryTag) -> Self {
        Self { pool, tag, size: 0 }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn tag(&self) -> MemoryTag {
        self.tag
    }

    /// Changes the charged amount.
    pub fn resize(&mut self, new_size: u64) {
        let delta = new_size as i64 - self.size as i64;
        if delta != 0 {
            self.pool.update_used_memory(self.tag, delta);
        }
        self.size = new_size;
    }

    /// Moves the charge to another tag, keeping its size.
    pub(crate) fn retag(&mut self, tag: MemoryTag) {
        if tag == self.tag {
            return;
        }
        let size = self.size;
        self.resize(0);
        self.tag = tag;
        self.resize(size);
    }

    /// Ends the reservation without releasing the bytes; the caller takes over tracking
    /// them (DuckDB's `reservation.size = 0` idiom).
    pub(crate) fn keep_charged(mut self) -> u64 {
        std::mem::take(&mut self.size)
    }
}

impl Drop for MemoryReservation {
    fn drop(&mut self) {
        self.resize(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_updates_are_cached_until_flushed() {
        let usage = MemoryUsage::new();
        usage.update(MemoryTag::HashTable, 100);
        assert_eq!(usage.total(false), 0);
        assert_eq!(usage.total(true), 100);
        assert_eq!(usage.tag(MemoryTag::HashTable, true), 100);
        usage.update(MemoryTag::HashTable, 1 << 20);
        assert_eq!(usage.total(false), (1 << 20) + 100);
        usage.update(MemoryTag::HashTable, -(1 << 20) - 100);
        assert_eq!(usage.total(true), 0);
    }
}
