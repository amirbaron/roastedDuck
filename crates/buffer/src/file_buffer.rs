//! An owned, page-aligned buffer that can be written to and read from disk (DuckDB's
//! `FileBuffer`).

use std::alloc::Layout;
use std::ptr::NonNull;
use std::sync::Arc;

use roastedduck_common::{Error, Result};

use crate::SECTOR_SIZE;
use crate::allocator::BlockAllocator;

/// The kind of a buffer, which decides how it is evicted (DuckDB's `FileBufferType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FileBufferType {
    /// A block of a database file: eviction just frees it (it can be re-read).
    Block = 1,
    /// Temporary memory: eviction writes it to a temporary file unless it may be
    /// destroyed.
    ManagedBuffer = 2,
    /// Small memory that is never evicted or written to disk.
    TinyBuffer = 3,
    /// A cached block of an external file.
    ExternalFile = 4,
}

enum Memory {
    /// From the block allocator (all buffers that may touch disk).
    Mapped(Arc<BlockAllocator>),
    /// From the global allocator (tiny buffers).
    Heap(Layout),
}

/// A buffer of `alloc_size` bytes whose first `header_size` bytes are a header
/// (the checksum of on-disk blocks); the rest is the user data.
pub struct FileBuffer {
    ptr: NonNull<u8>,
    alloc_size: usize,
    header_size: usize,
    kind: FileBufferType,
    memory: Memory,
}

// SAFETY: a `FileBuffer` uniquely owns its allocation; access to the bytes goes through
// `&self`/`&mut self` or through pins whose synchronisation is the buffer manager's job.
unsafe impl Send for FileBuffer {}
// SAFETY: see above; shared references only expose raw pointers.
unsafe impl Sync for FileBuffer {}

impl std::fmt::Debug for FileBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileBuffer")
            .field("kind", &self.kind)
            .field("alloc_size", &self.alloc_size)
            .field("header_size", &self.header_size)
            .finish_non_exhaustive()
    }
}

/// The allocation size DuckDB uses for `user_size` bytes plus a header: sector-aligned.
pub(crate) const fn aligned_alloc_size(user_size: usize, header_size: usize) -> usize {
    (user_size + header_size).next_multiple_of(SECTOR_SIZE)
}

fn allocation_failure(size: usize) -> Error {
    Error::new(
        roastedduck_common::ErrorKind::OutOfMemory,
        format!("Failed to allocate block of {size} bytes (bad allocation)"),
    )
}

impl FileBuffer {
    /// Allocates a buffer with room for `user_size` bytes after a `header_size` header.
    pub(crate) fn allocate(
        allocator: &Arc<BlockAllocator>,
        kind: FileBufferType,
        user_size: usize,
        header_size: usize,
    ) -> Result<Self> {
        if kind == FileBufferType::TinyBuffer {
            // Tiny buffers never touch disk, so they need neither a header nor alignment.
            let layout =
                Layout::from_size_align(user_size.max(1), 16).map_err(|_| allocation_failure(user_size))?;
            // SAFETY: `layout` has non-zero size.
            let ptr = NonNull::new(unsafe { std::alloc::alloc(layout) })
                .ok_or_else(|| allocation_failure(user_size))?;
            return Ok(Self {
                ptr,
                alloc_size: user_size,
                header_size: 0,
                kind,
                memory: Memory::Heap(layout),
            });
        }
        let alloc_size = aligned_alloc_size(user_size, header_size);
        let ptr = allocator
            .allocate(alloc_size)
            .ok_or_else(|| allocation_failure(alloc_size))?;
        Ok(Self {
            ptr,
            alloc_size,
            header_size,
            kind,
            memory: Memory::Mapped(Arc::clone(allocator)),
        })
    }

    /// Takes over the memory of `source` as a buffer of another kind and header size
    /// (reusing an evicted buffer of the right size).
    pub(crate) fn reuse(mut source: Self, kind: FileBufferType, header_size: usize) -> Self {
        source.kind = kind;
        source.header_size = header_size;
        source
    }

    pub fn kind(&self) -> FileBufferType {
        self.kind
    }

    /// Size of the whole allocation, header included (what is read/written on disk).
    pub fn alloc_size(&self) -> usize {
        self.alloc_size
    }

    pub fn header_size(&self) -> usize {
        self.header_size
    }

    /// Size of the user data.
    pub fn size(&self) -> usize {
        self.alloc_size - self.header_size
    }

    /// Start of the user data.
    pub fn data_ptr(&self) -> *mut u8 {
        // SAFETY: `header_size <= alloc_size`, so the result is within the allocation.
        unsafe { self.ptr.as_ptr().add(self.header_size) }
    }

    /// The whole allocation, header included.
    ///
    /// # Safety
    /// No one may write the buffer through a pin while the slice is alive.
    pub unsafe fn internal(&self) -> &[u8] {
        // SAFETY: the allocation holds `alloc_size` bytes; absence of writers is
        // guaranteed by the caller.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.alloc_size) }
    }

    /// The whole allocation, header included, for filling it from disk.
    pub fn internal_mut(&mut self) -> &mut [u8] {
        // SAFETY: `&mut self` means the buffer is not shared (it is not loaded into a
        // block yet, or the block's lock is held with no readers).
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.alloc_size) }
    }
}

impl Drop for FileBuffer {
    fn drop(&mut self) {
        match &self.memory {
            // SAFETY: allocated by `allocator.allocate(alloc_size)` and dropped once.
            Memory::Mapped(allocator) => unsafe { allocator.free(self.ptr, self.alloc_size) },
            // SAFETY: allocated with `layout` by the global allocator.
            Memory::Heap(layout) => unsafe { std::alloc::dealloc(self.ptr.as_ptr(), *layout) },
        }
    }
}
