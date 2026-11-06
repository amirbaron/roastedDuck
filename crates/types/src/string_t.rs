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

    /// An inline string of the first `len` (at most [`Self::INLINE_LENGTH`]) bytes of
    /// `bits` read as little-endian bytes, the inverse of [`StringT::inlined_bits`]:
    /// built with masks rather than a copy of `len` bytes.
    #[inline]
    pub fn inline_bits(bits: u128, len: usize) -> Self {
        debug_assert!(len <= Self::INLINE_LENGTH);
        let mask = (1u128 << (8 * len.min(Self::INLINE_LENGTH))) - 1;
        let bytes = (bits & mask).to_le_bytes();
        let mut data = [0; 12];
        data.copy_from_slice(&bytes[..12]);
        Self {
            len: len as u32,
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

    /// Equality decided from the 16 bytes of the two values alone (DuckDB's
    /// `string_t::StringComparisonOperators::Equals` fast path): different lengths or
    /// prefixes are unequal, inlined strings compare their bytes. `None` when the heap
    /// bytes of two long strings must be compared.
    #[inline]
    pub fn quick_eq(&self, other: &Self) -> Option<bool> {
        if self.len != other.len {
            return Some(false);
        }
        let len = self.len as usize;
        let word = |s: &Self, at: usize| {
            let mut bytes = [0u8; 8];
            bytes[..8.min(12 - at)].copy_from_slice(&s.data[at..at + 8.min(12 - at)]);
            u64::from_le_bytes(bytes)
        };
        if len > Self::INLINE_LENGTH {
            return if self.prefix() == other.prefix() {
                None
            } else {
                Some(false)
            };
        }
        // Only the inlined bytes count, not whatever follows them.
        let low = word(self, 0) ^ word(other, 0);
        let high = word(self, 8) ^ word(other, 8);
        let low_mask = if len >= 8 {
            u64::MAX
        } else {
            (1u64 << (len * 8)) - 1
        };
        let high_mask = if len <= 8 {
            0
        } else {
            (1u64 << ((len - 8) * 8)) - 1
        };
        Some(low & low_mask == 0 && high & high_mask == 0)
    }

    /// The bytes of an inlined string as a little-endian number: byte `i` of the string
    /// in bits `8i..8i + 8`, zero above the string's length. Only meaningful for inlined
    /// strings ([`StringT::is_inlined`]).
    #[inline]
    pub fn inlined_bits(&self) -> u128 {
        let mut bytes = [0u8; 16];
        bytes[..12].copy_from_slice(&self.data);
        // Bytes after an inlined string are not necessarily zero.
        let mask = u128::MAX.checked_shr(128 - 8 * self.len.min(12)).unwrap_or(0);
        u128::from_le_bytes(bytes) & mask
    }

    /// The first (up to) four bytes; equal prefixes are needed for equal strings.
    pub fn prefix(&self) -> [u8; 4] {
        [self.data[0], self.data[1], self.data[2], self.data[3]]
    }

    /// A pointer to the string's bytes: inside `*this` for an inlined string, otherwise
    /// the pointer it holds (DuckDB's `string_t::GetData`, as the C API hands it out).
    ///
    /// # Safety
    /// `this` must point to a valid `StringT`; the result is only valid as long as that
    /// value (inlined) or its heap (not inlined) is.
    pub unsafe fn data_ptr(this: *const Self) -> *const u8 {
        // SAFETY: guaranteed by the caller.
        let value = unsafe { &*this };
        if value.is_inlined() {
            return value.data.as_ptr();
        }
        let mut ptr = [0u8; 8];
        ptr.copy_from_slice(&value.data[4..]);
        u64::from_ne_bytes(ptr) as usize as *const u8
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

#[cfg(test)]
mod tests {
    use super::StringT;

    #[test]
    fn quick_eq_ignores_padding_and_defers_long_strings() {
        let words: [&[u8]; 8] = [
            b"",
            b"a",
            b"abcd",
            b"abcde",
            b"abcdefgh",
            b"abcdefghi",
            b"abcdefghijkl",
            b"abcdefghijkx",
        ];
        for a in words {
            for b in words {
                let (x, y) = (StringT::inline(a), StringT::inline(b));
                assert_eq!(x.quick_eq(&y), Some(a == b), "{a:?} {b:?}");
            }
        }
        let mut padded = StringT::inline(b"abc");
        padded.data[7] = 0xff;
        assert_eq!(padded.quick_eq(&StringT::inline(b"abc")), Some(true));
        let long: &[u8] = b"abcdefghijklmnop";
        let other: &[u8] = b"abcdefghijklmnoq";
        let third: &[u8] = b"xbcdefghijklmnop";
        // SAFETY: the slices outlive the values.
        let (x, y, z) = unsafe {
            (
                StringT::from_raw(long),
                StringT::from_raw(other),
                StringT::from_raw(third),
            )
        };
        assert_eq!(x.quick_eq(&y), None);
        assert_eq!(x.quick_eq(&z), Some(false));
    }
}
