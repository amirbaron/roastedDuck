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
/// reuse their `VectorCache` buffer chunk after chunk instead of allocating. Such buffers
/// are allocated in power-of-two sizes, so vectors of any row count up to a full vector
/// share a few size classes.
#[derive(Debug)]
pub(crate) struct RawBuffer {
    ptr: NonNull<u8>,
    len: usize,
    /// The allocated bytes, at least `len`; all of them are initialised.
    capacity: usize,
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
        Self::allocate(len)
    }

    /// A buffer of `len` initialised bytes with unspecified contents (zeros or the values of
    /// an earlier vector), for callers that overwrite the slots they read.
    pub(crate) fn recycled(len: usize) -> Self {
        Self::take_pooled(len).unwrap_or_else(|| Self::allocate(len))
    }

    /// A buffer whose bytes are all overwritten by `fill` before anyone can read them.
    fn filled(len: usize, fill: impl FnOnce(*mut u8)) -> Self {
        let mut buffer = Self::take_pooled(len).unwrap_or_else(|| {
            if Pool::accepts(len) {
                // Pooled sizes keep every allocated byte initialised.
                Self::allocate(len)
            } else {
                Self::allocate_uninit(len)
            }
        });
        fill(buffer.as_mut_ptr());
        buffer
    }

    /// A zeroed buffer of `len` bytes, allocated in its pool size class.
    fn allocate(len: usize) -> Self {
        if len == 0 {
            return Self::empty();
        }
        let capacity = Pool::capacity(len);
        let layout = Self::layout(capacity);
        // SAFETY: `layout` has non-zero size.
        let ptr = unsafe { alloc::alloc_zeroed(layout) };
        let Some(ptr) = NonNull::new(ptr) else {
            alloc::handle_alloc_error(layout)
        };
        Self { ptr, len, capacity }
    }

    /// A buffer of `len` uninitialised bytes too large to pool, for [`RawBuffer::filled`].
    fn allocate_uninit(len: usize) -> Self {
        if len == 0 {
            return Self::empty();
        }
        let layout = Self::layout(len);
        // SAFETY: `layout` has non-zero size.
        let ptr = unsafe { alloc::alloc(layout) };
        let Some(ptr) = NonNull::new(ptr) else {
            alloc::handle_alloc_error(layout)
        };
        Self {
            ptr,
            len,
            capacity: len,
        }
    }

    fn empty() -> Self {
        // A dangling but aligned pointer is valid for zero-length slices.
        let ptr = NonNull::new(ALIGN as *mut u8).unwrap_or(NonNull::dangling());
        Self {
            ptr,
            len: 0,
            capacity: 0,
        }
    }

    fn take_pooled(len: usize) -> Option<Self> {
        if !Pool::accepts(len) {
            return None;
        }
        POOL.try_with(|pool| pool.borrow_mut().take(len))
            .ok()
            .flatten()
            .map(|ptr| Self {
                ptr,
                len,
                capacity: Pool::capacity(len),
            })
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
            // SAFETY: both allocations are at least `len` bytes and do not overlap; the
            // copy initialises the first `len` bytes of the new buffer (any others are).
            unsafe { std::ptr::copy_nonoverlapping(self.as_ptr(), target, self.len) };
        })
    }
}

impl Drop for RawBuffer {
    fn drop(&mut self) {
        if self.capacity == 0 {
            return;
        }
        let ptr = self.ptr;
        let len = self.len;
        let pooled = Pool::accepts(len)
            && POOL
                .try_with(|pool| pool.borrow_mut().put(ptr, len))
                .unwrap_or(false);
        if !pooled {
            // SAFETY: allocated with this exact layout in `allocate` or `allocate_uninit`.
            unsafe { alloc::dealloc(ptr.as_ptr(), Self::layout(self.capacity)) };
        }
    }
}

/// Per-thread free lists of recently dropped vector buffers, one per power-of-two size.
#[derive(Default)]
struct Pool {
    classes: [Vec<NonNull<u8>>; Pool::CLASSES],
}

impl Pool {
    /// Largest buffer kept: one vector of 16-byte values (`STANDARD_VECTOR_SIZE` × 16).
    const MAX_LEN: usize = 2048 * 16;
    /// The smallest size class: the alignment.
    const MIN_CAPACITY: usize = ALIGN;
    /// Size classes from `MIN_CAPACITY` to `MAX_LEN` bytes.
    const CLASSES: usize =
        (Self::MAX_LEN.trailing_zeros() - Self::MIN_CAPACITY.trailing_zeros() + 1) as usize;
    /// Free buffers kept per size class.
    const MAX_FREE: usize = 32;

    fn accepts(len: usize) -> bool {
        len != 0 && len <= Self::MAX_LEN
    }

    /// The bytes allocated for a buffer of `len` bytes: its size class if it is pooled.
    fn capacity(len: usize) -> usize {
        if Self::accepts(len) {
            len.next_power_of_two().max(Self::MIN_CAPACITY)
        } else {
            len
        }
    }

    fn class(len: usize) -> usize {
        (Self::capacity(len).trailing_zeros() - Self::MIN_CAPACITY.trailing_zeros()) as usize
    }

    fn take(&mut self, len: usize) -> Option<NonNull<u8>> {
        self.classes[Self::class(len)].pop()
    }

    /// Keeps the allocation of a `len`-byte [`RawBuffer`] for reuse; `false` if its size
    /// class is full.
    fn put(&mut self, ptr: NonNull<u8>, len: usize) -> bool {
        let class = &mut self.classes[Self::class(len)];
        if class.len() >= Self::MAX_FREE {
            return false;
        }
        class.push(ptr);
        true
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        for (index, class) in self.classes.iter_mut().enumerate() {
            let layout = RawBuffer::layout(Self::MIN_CAPACITY << index);
            for ptr in class.drain(..) {
                // SAFETY: every pooled pointer came from `RawBuffer::allocate` in this size
                // class and is no longer referenced by any `RawBuffer`.
                unsafe { alloc::dealloc(ptr.as_ptr(), layout) };
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
    fn buffers_of_one_size_class_are_reused() {
        let first = RawBuffer::recycled(1000);
        let ptr = first.as_ptr();
        drop(first);
        let second = RawBuffer::recycled(900);
        assert_eq!(second.as_ptr(), ptr);
        assert_eq!(second.len(), 900);
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
