use std::alloc::{self, Layout};
use std::ptr::NonNull;
use std::sync::Arc;

/// Alignment of every vector data buffer; enough for 128-bit integers and SIMD loads.
const ALIGN: usize = 16;

/// A zero-initialised, 16-byte aligned, fixed-size byte allocation.
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
    pub(crate) fn zeroed(len: usize) -> Self {
        if len == 0 {
            // A dangling but aligned pointer is valid for zero-length slices.
            let ptr = NonNull::new(ALIGN as *mut u8).unwrap_or(NonNull::dangling());
            return Self { ptr, len };
        }
        let layout = Self::layout(len);
        // SAFETY: `layout` has non-zero size.
        let ptr = unsafe { alloc::alloc_zeroed(layout) };
        let Some(ptr) = NonNull::new(ptr) else {
            alloc::handle_alloc_error(layout)
        };
        Self { ptr, len }
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
        let mut copy = Self::zeroed(self.len);
        // SAFETY: both allocations are `len` bytes and do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(self.as_ptr(), copy.as_mut_ptr(), self.len) };
        copy
    }
}

impl Drop for RawBuffer {
    fn drop(&mut self) {
        if self.len != 0 {
            // SAFETY: allocated in `zeroed` with this exact layout.
            unsafe { alloc::dealloc(self.ptr.as_ptr(), Self::layout(self.len)) };
        }
    }
}

/// A shared, copy-on-write data buffer.
pub(crate) type SharedBuffer = Arc<RawBuffer>;
