//! Memory handed to C callers: `duckdb_malloc`, `duckdb_free` and C strings.
//!
//! Every buffer this library gives out carries its allocation size in a header word in
//! front of the returned pointer, so `duckdb_free` can release any of them uniformly.

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::ffi::{c_char, c_void};

const HEADER: usize = 16; // keeps the returned pointer 16-byte aligned

fn layout(total: usize) -> Layout {
    Layout::from_size_align(total, HEADER).unwrap_or_else(|_| handle_alloc_error(Layout::new::<u8>()))
}

/// Allocates `size` zeroed bytes freeable with `duckdb_free`.
pub(crate) fn allocate(size: usize) -> *mut u8 {
    let total = size.saturating_add(HEADER);
    let layout = layout(total);
    // SAFETY: `layout` has non-zero size (at least `HEADER`).
    let base = unsafe { alloc_zeroed(layout) };
    if base.is_null() {
        handle_alloc_error(layout);
    }
    // SAFETY: `base` points to `total >= HEADER` bytes.
    unsafe {
        base.copy_from_nonoverlapping(total.to_ne_bytes().as_ptr(), size_of::<usize>());
        base.add(HEADER)
    }
}

/// Copies `text` into a NUL-terminated buffer freeable with `duckdb_free`.
/// Interior NUL bytes are kept; C readers will stop at the first one.
pub(crate) fn c_string(text: &str) -> *mut c_char {
    let ptr = allocate(text.len() + 1);
    // SAFETY: `ptr` has room for `len + 1` bytes; the last byte stays zero.
    unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), ptr, text.len()) };
    ptr.cast()
}

/// `void *duckdb_malloc(size_t size)`
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_malloc(size: usize) -> *mut c_void {
    allocate(size).cast()
}

/// `void duckdb_free(void *ptr)`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr` was returned by `allocate`, so the header precedes it.
    unsafe {
        let base = ptr.cast::<u8>().sub(HEADER);
        let mut total = [0u8; size_of::<usize>()];
        total.as_mut_ptr().copy_from_nonoverlapping(base, total.len());
        let total = usize::from_ne_bytes(total);
        dealloc(base, layout(total));
    }
}
