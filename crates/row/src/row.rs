//! Raw row access: row pointers, validity bits and unaligned loads/stores.
//!
//! Everything here is `unsafe` at the boundary: callers guarantee that row pointers
//! point into pinned blocks laid out per the [`crate::TupleDataLayout`] in use.

/// The address of a row in a pinned block (DuckDB's `data_ptr_t` row locations).
///
/// Row pointers are plain values; dereferencing them is up to the row-format code, which
/// keeps the blocks pinned while their rows are in use. Hash tables store them as-is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct RowPtr(*mut u8);

// SAFETY: a row pointer is an address; synchronisation of the memory behind it is the
// responsibility of the code that dereferences it (as with DuckDB's raw row pointers).
unsafe impl Send for RowPtr {}
// SAFETY: see above.
unsafe impl Sync for RowPtr {}

impl Default for RowPtr {
    fn default() -> Self {
        Self::NULL
    }
}

impl RowPtr {
    /// No row.
    pub const NULL: Self = Self(std::ptr::null_mut());

    pub fn from_ptr(ptr: *mut u8) -> Self {
        Self(ptr)
    }

    pub fn as_ptr(self) -> *mut u8 {
        self.0
    }

    pub fn is_null(self) -> bool {
        self.0.is_null()
    }

    /// The address `offset` bytes further.
    #[inline]
    #[must_use]
    pub fn offset(self, offset: usize) -> Self {
        Self(self.0.wrapping_add(offset))
    }
}

/// Loads a `T` from a possibly unaligned address.
///
/// # Safety
/// `ptr` must be valid for reading `size_of::<T>()` bytes holding a valid `T`.
#[inline]
pub(crate) unsafe fn load<T: Copy>(ptr: *const u8) -> T {
    // SAFETY: guaranteed by the caller.
    unsafe { ptr.cast::<T>().read_unaligned() }
}

/// Stores a `T` at a possibly unaligned address.
///
/// # Safety
/// `ptr` must be valid for writing `size_of::<T>()` bytes.
#[inline]
pub(crate) unsafe fn store<T: Copy>(value: T, ptr: *mut u8) {
    // SAFETY: guaranteed by the caller.
    unsafe { ptr.cast::<T>().write_unaligned(value) };
}

/// Whether column `col` of the row at `row` is valid (bit `col` of the validity bytes).
///
/// # Safety
/// `row` must point to a row with at least `col + 1` validity bits.
#[inline]
pub(crate) unsafe fn is_valid(row: *const u8, col: usize) -> bool {
    // SAFETY: guaranteed by the caller.
    unsafe { (*row.add(col / 8) >> (col % 8)) & 1 == 1 }
}

/// Marks column `col` of the row at `row` NULL.
///
/// # Safety
/// `row` must point to a writable row with at least `col + 1` validity bits.
#[inline]
pub(crate) unsafe fn set_invalid(row: *mut u8, col: usize) {
    // SAFETY: guaranteed by the caller.
    unsafe { *row.add(col / 8) &= !(1 << (col % 8)) };
}

/// Marks the first `bytes * 8` columns of the row at `row` valid.
///
/// # Safety
/// `row` must be writable for `bytes` bytes.
#[inline]
pub(crate) unsafe fn set_all_valid(row: *mut u8, bytes: usize) {
    // SAFETY: guaranteed by the caller.
    unsafe { std::ptr::write_bytes(row, 0xFF, bytes) };
}

/// Copies `len` bytes.
///
/// # Safety
/// Both ranges must be valid and must not overlap.
#[inline]
pub(crate) unsafe fn copy(source: *const u8, target: *mut u8, len: usize) {
    // SAFETY: guaranteed by the caller.
    unsafe { std::ptr::copy_nonoverlapping(source, target, len) };
}
