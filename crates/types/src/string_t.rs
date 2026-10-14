use std::fmt;

/// DuckDB's 16-byte `string_t`.
///
/// Strings of up to [`StringT::INLINE_LENGTH`] bytes are stored inline. Longer strings keep
/// a 4-byte prefix inline and point to the full bytes, which live in a heap owned by the
/// vector (or storage block) the value came from.
#[derive(Clone, Copy, Default)]
#[repr(C, align(8))]
pub struct StringT {
    len: u32,
    /// Inline bytes, or `prefix[4] ++ pointer[8]` for long strings.
    data: [u8; 12],
}

const _: () = assert!(size_of::<StringT>() == 16);

impl StringT {
    pub const INLINE_LENGTH: usize = 12;
    pub const PREFIX_LENGTH: usize = 4;

    /// An inline string; `bytes.len()` must be at most [`Self::INLINE_LENGTH`].
    pub fn inline(bytes: &[u8]) -> Self {
        debug_assert!(bytes.len() <= Self::INLINE_LENGTH);
        let mut data = [0; 12];
        data[..bytes.len()].copy_from_slice(bytes);
        Self {
            len: bytes.len() as u32,
            data,
        }
    }

    /// A string pointing at `bytes`.
    ///
    /// # Safety
    /// The memory behind `bytes` must outlive every read of the returned value; callers keep
    /// the owning heap alive (see `Vector::add_heap`).
    pub unsafe fn from_raw(bytes: &[u8]) -> Self {
        if bytes.len() <= Self::INLINE_LENGTH {
            return Self::inline(bytes);
        }
        let mut data = [0; 12];
        data[..4].copy_from_slice(&bytes[..4]);
        data[4..].copy_from_slice(&(bytes.as_ptr() as usize as u64).to_ne_bytes());
        Self {
            len: bytes.len() as u32,
            data,
        }
    }

    pub const fn len(&self) -> usize {
        self.len as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn is_inlined(&self) -> bool {
        self.len as usize <= Self::INLINE_LENGTH
    }

    /// The first (up to) four bytes; equal prefixes are needed for equal strings.
    pub fn prefix(&self) -> [u8; 4] {
        [self.data[0], self.data[1], self.data[2], self.data[3]]
    }

    /// The string bytes.
    ///
    /// # Safety
    /// For non-inlined strings the heap the value points into must still be alive.
    pub unsafe fn as_bytes(&self) -> &[u8] {
        if self.is_inlined() {
            &self.data[..self.len as usize]
        } else {
            let mut ptr = [0u8; 8];
            ptr.copy_from_slice(&self.data[4..]);
            let ptr = u64::from_ne_bytes(ptr) as usize as *const u8;
            // SAFETY: guaranteed by the caller; `len` bytes were valid when created.
            unsafe { std::slice::from_raw_parts(ptr, self.len as usize) }
        }
    }
}

impl fmt::Debug for StringT {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The pointer half of `data` is meaningless without its heap, so only the length and
        // prefix are shown.
        write!(f, "StringT {{ len: {}, prefix: {:?} }}", self.len, self.prefix())
    }
}
