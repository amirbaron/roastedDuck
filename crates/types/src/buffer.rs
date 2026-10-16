use std::alloc::{self, Layout};
use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::Arc;

/// Alignment of every vector data buffer; enough for 128-bit integers and SIMD loads.
const ALIGN: usize = 16;

/// A 16-byte aligned, fixed-size byte allocation whose bytes are always initialised.
///
/// Freed buffers of common vector sizes go to a small per-thread free list and are handed
/// out again by [`RawBuffer::recycled`] without clearing them, the way DuckDB's vectors
/// reuse their `VectorCache` buffer chunk after chunk instead of allocating.
#[derive(Debug)]
pub(crate) struct RawBuffer {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: `RawBuffer` uniquely owns its allocation; shared access is read-only via `&self`.
unsafe impl Send for RawBuffer {}
// SAFETY: see above.
unsafe impl Sync for RawBuffer {}

impl RawBuffer {
    /// A buffer of `len` zero bytes.
    pub(crate) fn zeroed(len: usize) -> Self {
        if let Some(mut buffer) = Self::take_pooled(len) {
            // SAFETY: the buffer is `len` bytes long and uniquely owned.
            unsafe { std::ptr::write_bytes(buffer.as_mut_ptr(), 0, len) };
            return buffer;
        }
        Self::allocate(len, true)
    }

    /// A buffer of `len` initialised bytes with unspecified contents (zeros or the values of
    /// an earlier vector), for callers that overwrite the slots they read.
    pub(crate) fn recycled(len: usize) -> Self {
        Self::take_pooled(len).unwrap_or_else(|| Self::allocate(len, true))
    }

    /// A buffer whose bytes are all overwritten by `fill` before anyone can read them.
    fn filled(len: usize, fill: impl FnOnce(*mut u8)) -> Self {
        let mut buffer = Self::take_pooled(len).unwrap_or_else(|| Self::allocate(len, false));
        fill(buffer.as_mut_ptr());
        buffer
    }

    fn allocate(len: usize, zero: bool) -> Self {
        if len == 0 {
            // A dangling but aligned pointer is valid for zero-length slices.
            let ptr = NonNull::new(ALIGN as *mut u8).unwrap_or(NonNull::dangling());
            return Self { ptr, len };
        }
        let layout = Self::layout(len);
        // SAFETY: `layout` has non-zero size.
        let ptr = unsafe {
            if zero {
                alloc::alloc_zeroed(layout)
            } else {
                alloc::alloc(layout)
            }
        };
        let Some(ptr) = NonNull::new(ptr) else {
            alloc::handle_alloc_error(layout)
        };
        Self { ptr, len }
    }

    fn take_pooled(len: usize) -> Option<Self> {
        if !Pool::accepts(len) {
            return None;
        }
        POOL.try_with(|pool| pool.borrow_mut().take(len))
            .ok()
            .flatten()
            .map(|ptr| Self { ptr, len })
    }

    fn layout(len: usize) -> Layout {
        match Layout::from_size_align(len, ALIGN) {
            Ok(layout) => layout,
            Err(_) => alloc::handle_alloc_error(Layout::new::<u8>()),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn as_ptr(&self) -> *const u8 {
        self.ptr.as_ptr()
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr.as_ptr()
    }
}

impl Clone for RawBuffer {
    fn clone(&self) -> Self {
        Self::filled(self.len, |target| {
            // SAFETY: both allocations are `len` bytes and do not overlap; the copy
            // initialises every byte of the new buffer.
            unsafe { std::ptr::copy_nonoverlapping(self.as_ptr(), target, self.len) };
        })
    }
}

impl Drop for RawBuffer {
    fn drop(&mut self) {
        if self.len == 0 {
            return;
        }
        let ptr = self.ptr;
        let len = self.len;
        let pooled = Pool::accepts(len)
            && POOL
                .try_with(|pool| pool.borrow_mut().put(ptr, len))
                .unwrap_or(false);
        if !pooled {
            // SAFETY: allocated in `allocate` with this exact layout.
            unsafe { alloc::dealloc(ptr.as_ptr(), Self::layout(len)) };
        }
    }
}

/// Free buffers of one byte length.
#[derive(Default)]
struct SizeClass {
    len: usize,
    free: Vec<NonNull<u8>>,
}

/// Per-thread free lists of recently dropped vector buffers.
#[derive(Default)]
struct Pool {
    classes: Vec<SizeClass>,
}

impl Pool {
    /// Largest buffer kept: one vector of 16-byte values (`STANDARD_VECTOR_SIZE` × 16).
    const MAX_LEN: usize = 2048 * 16;
    /// Distinct buffer lengths kept per thread.
    const MAX_CLASSES: usize = 8;
    /// Free buffers kept per length.
    const MAX_FREE: usize = 16;

    fn accepts(len: usize) -> bool {
        len != 0 && len <= Self::MAX_LEN
    }

    fn take(&mut self, len: usize) -> Option<NonNull<u8>> {
        self.classes
            .iter_mut()
            .find(|class| class.len == len)
            .and_then(|class| class.free.pop())
    }

    /// Keeps `ptr` for reuse; `false` if the pool is full.
    fn put(&mut self, ptr: NonNull<u8>, len: usize) -> bool {
        let index = match self.classes.iter().position(|class| class.len == len) {
            Some(index) => index,
            None if self.classes.len() < Self::MAX_CLASSES => {
                self.classes.push(SizeClass {
                    len,
                    free: Vec::new(),
                });
                self.classes.len() - 1
            }
            None => return false,
        };
        let class = &mut self.classes[index];
        if class.free.len() >= Self::MAX_FREE {
            return false;
        }
        class.free.push(ptr);
        true
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        for class in &mut self.classes {
            for ptr in class.free.drain(..) {
                // SAFETY: every pooled pointer came from `RawBuffer::allocate` with
                // `class.len` bytes and is no longer referenced by any `RawBuffer`.
                unsafe { alloc::dealloc(ptr.as_ptr(), RawBuffer::layout(class.len)) };
            }
        }
    }
}

thread_local! {
    static POOL: RefCell<Pool> = RefCell::new(Pool::default());
}

/// A shared, copy-on-write data buffer.
pub(crate) type SharedBuffer = Arc<RawBuffer>;

#[cfg(test)]
mod tests {
    use super::RawBuffer;

    #[test]
    fn zeroed_buffers_are_zero_after_reuse() {
        let mut first = RawBuffer::zeroed(4096);
        // SAFETY: the buffer is 4096 bytes long.
        unsafe { std::ptr::write_bytes(first.as_mut_ptr(), 0xAB, 4096) };
        drop(first);
        let second = RawBuffer::zeroed(4096);
        // SAFETY: the buffer is 4096 initialised bytes.
        let bytes = unsafe { std::slice::from_raw_parts(second.as_ptr(), 4096) };
        assert!(bytes.iter().all(|&b| b == 0));
    }

    #[test]
    fn clones_copy_every_byte() {
        let mut source = RawBuffer::recycled(100);
        // SAFETY: the buffer is 100 bytes long.
        unsafe { std::ptr::write_bytes(source.as_mut_ptr(), 7, 100) };
        let copy = source.clone();
        // SAFETY: the buffer is 100 initialised bytes.
        let bytes = unsafe { std::slice::from_raw_parts(copy.as_ptr(), 100) };
        assert!(bytes.iter().all(|&b| b == 7));
    }
}
