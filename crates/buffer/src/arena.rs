//! A bump allocator for many small, short-lived allocations (DuckDB's `ArenaAllocator`).
//!
//! Chunks start at 2 KiB (by default) and double up to 16 MiB; everything is freed at once
//! by [`ArenaAllocator::reset`] or when the arena is dropped. With a buffer manager, chunks
//! are charged to the `ALLOCATOR` memory tag.

use std::ptr::NonNull;

use roastedduck_common::Result;

use crate::manager::{AllocatedData, BufferManager};

/// Default size of the first chunk.
pub const ARENA_ALLOCATOR_INITIAL_CAPACITY: usize = 2048;
/// Chunks stop doubling at this size.
pub const ARENA_ALLOCATOR_MAX_CAPACITY: usize = 1 << 24;
/// Alignment of [`ArenaAllocator::allocate_aligned`].
const ALIGNMENT: usize = 8;

struct ArenaChunk {
    data: AllocatedData,
    position: usize,
}

/// A bump allocator; see the module documentation.
pub struct ArenaAllocator {
    buffer_manager: Option<BufferManager>,
    initial_capacity: usize,
    /// Chunks in allocation order; the last one is the head being filled.
    chunks: Vec<ArenaChunk>,
    allocated_size: usize,
}

impl std::fmt::Debug for ArenaAllocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArenaAllocator")
            .field("chunks", &self.chunks.len())
            .field("allocated_size", &self.allocated_size)
            .finish_non_exhaustive()
    }
}

// SAFETY: the arena exclusively owns its chunks; allocations are handed out as raw
// pointers whose use is the caller's responsibility.
unsafe impl Send for ArenaAllocator {}

impl ArenaAllocator {
    /// An arena whose chunks are charged to `buffer_manager` (or untracked with `None`).
    pub fn new(buffer_manager: Option<BufferManager>) -> Self {
        Self::with_capacity(buffer_manager, ARENA_ALLOCATOR_INITIAL_CAPACITY)
    }

    /// An arena whose first chunk is `initial_capacity * 2` bytes (DuckDB doubles before
    /// the first allocation, too).
    pub fn with_capacity(buffer_manager: Option<BufferManager>, initial_capacity: usize) -> Self {
        Self {
            buffer_manager,
            initial_capacity: initial_capacity.max(1),
            chunks: Vec::new(),
            allocated_size: 0,
        }
    }

    /// Allocates `len` bytes (unaligned). The memory stays valid until [`Self::reset`],
    /// [`Self::destroy`] or drop.
    pub fn allocate(&mut self, len: usize) -> Result<NonNull<u8>> {
        let fits = self
            .chunks
            .last()
            .is_some_and(|head| head.position + len <= head.data.len());
        if !fits {
            self.allocate_new_chunk(len)?;
        }
        let Some(head) = self.chunks.last_mut() else {
            unreachable!("a chunk was just allocated")
        };
        // SAFETY: `position + len <= len()` of the chunk, so the result is in bounds.
        let ptr = unsafe { head.data.as_ptr().add(head.position) };
        head.position += len;
        // SAFETY: derived from a non-null chunk pointer.
        Ok(unsafe { NonNull::new_unchecked(ptr) })
    }

    /// Allocates `len` bytes aligned to 8 bytes.
    pub fn allocate_aligned(&mut self, len: usize) -> Result<NonNull<u8>> {
        self.align_next();
        self.allocate(len.next_multiple_of(ALIGNMENT))
    }

    /// Grows or shrinks the most recent allocation in place when possible, otherwise
    /// allocates anew and copies.
    ///
    /// # Safety
    /// `ptr` must come from this arena and hold at least `old_len` bytes.
    pub unsafe fn reallocate(
        &mut self,
        ptr: NonNull<u8>,
        old_len: usize,
        new_len: usize,
    ) -> Result<NonNull<u8>> {
        if old_len == new_len {
            return Ok(ptr);
        }
        if let Some(head) = self.chunks.last_mut()
            && head.position >= old_len
        {
            // SAFETY: `position - old_len` is within the chunk.
            let head_ptr = unsafe { head.data.as_ptr().add(head.position - old_len) };
            if head_ptr == ptr.as_ptr()
                && (new_len < old_len || head.position - old_len + new_len <= head.data.len())
            {
                head.position = head.position - old_len + new_len;
                return Ok(ptr);
            }
        }
        let result = self.allocate(new_len)?;
        // SAFETY: both regions are valid for `old_len.min(new_len)` bytes and distinct.
        unsafe { std::ptr::copy_nonoverlapping(ptr.as_ptr(), result.as_ptr(), old_len.min(new_len)) };
        Ok(result)
    }

    /// Makes the next allocation 8-byte aligned.
    pub fn align_next(&mut self) {
        if let Some(head) = self.chunks.last_mut() {
            head.position = head.position.next_multiple_of(ALIGNMENT).min(head.data.len());
        }
    }

    /// Releases the last `len` bytes allocated.
    pub fn shrink_head(&mut self, len: usize) {
        if let Some(head) = self.chunks.last_mut() {
            head.position = head.position.saturating_sub(len);
        }
    }

    /// Frees everything but the current chunk, which is emptied.
    pub fn reset(&mut self) {
        if let Some(head) = self.chunks.pop() {
            self.chunks.clear();
            self.chunks.push(ArenaChunk {
                data: head.data,
                position: 0,
            });
        }
        self.allocated_size = 0;
    }

    /// Frees everything.
    pub fn destroy(&mut self) {
        self.chunks.clear();
        self.allocated_size = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Bytes handed out (not counting chunk slack).
    pub fn size_in_bytes(&self) -> usize {
        self.chunks.iter().map(|chunk| chunk.position).sum()
    }

    /// Bytes allocated for chunks since the last reset.
    pub fn allocation_size(&self) -> usize {
        self.allocated_size
    }

    fn allocate_new_chunk(&mut self, min_size: usize) -> Result<()> {
        let mut capacity = self
            .chunks
            .last()
            .map_or(self.initial_capacity, |head| head.data.len())
            .min(ARENA_ALLOCATOR_MAX_CAPACITY);
        if capacity < ARENA_ALLOCATOR_MAX_CAPACITY {
            capacity *= 2;
        }
        while capacity < min_size {
            capacity *= 2;
        }
        let data = match &self.buffer_manager {
            Some(manager) => manager.allocate_data(capacity)?,
            None => AllocatedData::untracked(capacity)?,
        };
        self.chunks.push(ArenaChunk { data, position: 0 });
        self.allocated_size += capacity;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_allocates_doubles_and_resets() {
        let mut arena = ArenaAllocator::new(None);
        let a = arena.allocate(10).unwrap();
        let b = arena.allocate_aligned(3).unwrap();
        assert_eq!(b.as_ptr() as usize % 8, 0);
        assert_eq!(arena.allocation_size(), 4096);
        // A large allocation gets a chunk of its own.
        let _big = arena.allocate(100_000).unwrap();
        assert_eq!(arena.allocation_size(), 4096 + 131_072);
        // SAFETY: `a` came from this arena with 10 bytes.
        let moved = unsafe { arena.reallocate(a, 10, 20).unwrap() };
        assert_ne!(moved, a);
        arena.reset();
        assert_eq!(arena.size_in_bytes(), 0);
        assert_eq!(arena.allocation_size(), 0);
        assert!(!arena.is_empty());
    }
}
