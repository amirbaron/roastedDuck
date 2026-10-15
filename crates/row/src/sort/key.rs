//! Fixed-width sort keys (DuckDB's `SortKey<SortKeyType>`).
//!
//! A sort key holds a row's normalized key (see [`super::encode`]): byte-comparable, but
//! stored as big-endian-decoded `u64` parts so keys compare with integer comparisons.
//! Keys longer than the inline part keep a pointer to the full key in the heap. Keys of
//! sorts with a payload end with a pointer to the payload row.

use std::cmp::Ordering;

/// The shape of a sort key (DuckDB's `SortKeyType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SortKeyType {
    NoPayloadFixed8,
    NoPayloadFixed16,
    NoPayloadFixed24,
    NoPayloadFixed32,
    NoPayloadVariable32,
    PayloadFixed16,
    PayloadFixed24,
    PayloadFixed32,
    PayloadVariable32,
}

impl SortKeyType {
    /// Bytes per key.
    pub const fn width(self) -> usize {
        match self {
            Self::NoPayloadFixed8 => 8,
            Self::NoPayloadFixed16 | Self::PayloadFixed16 => 16,
            Self::NoPayloadFixed24 | Self::PayloadFixed24 => 24,
            Self::NoPayloadFixed32
            | Self::PayloadFixed32
            | Self::NoPayloadVariable32
            | Self::PayloadVariable32 => 32,
        }
    }

    /// Normalized key bytes stored inline.
    pub const fn inline_length(self) -> usize {
        match self {
            Self::NoPayloadFixed8 | Self::PayloadFixed16 | Self::PayloadVariable32 => 8,
            Self::NoPayloadFixed16 | Self::PayloadFixed24 | Self::NoPayloadVariable32 => 16,
            Self::NoPayloadFixed24 | Self::PayloadFixed32 => 24,
            Self::NoPayloadFixed32 => 32,
        }
    }

    /// Whether every key fits inline (no heap).
    pub const fn is_constant_size(self) -> bool {
        !matches!(self, Self::NoPayloadVariable32 | Self::PayloadVariable32)
    }

    pub const fn has_payload(self) -> bool {
        matches!(
            self,
            Self::PayloadFixed16 | Self::PayloadFixed24 | Self::PayloadFixed32 | Self::PayloadVariable32
        )
    }

    /// Offset of the key size (the heap size) in variable-size keys.
    pub const fn heap_size_offset(self) -> usize {
        match self {
            Self::NoPayloadVariable32 => 16,
            _ => 8,
        }
    }

    /// Offset of the payload pointer.
    pub const fn payload_offset(self) -> Option<usize> {
        match self {
            Self::PayloadFixed16 => Some(8),
            Self::PayloadFixed24 => Some(16),
            Self::PayloadFixed32 | Self::PayloadVariable32 => Some(24),
            _ => None,
        }
    }
}

/// Operations on one concrete key type; implemented by the `#[repr(C)]` key structs.
pub(crate) trait SortKey: Copy + Default + Send + Sync + 'static {
    const TYPE: SortKeyType;
    const INLINE_LENGTH: usize = Self::TYPE.inline_length();

    /// The inline parts.
    fn parts(&self) -> &[u64];
    fn parts_mut(&mut self) -> &mut [u64];

    /// The first eight key bytes as an integer (radix sorting).
    #[inline]
    fn part0(&self) -> u64 {
        self.parts()[0]
    }

    /// The order of the normalized keys.
    fn compare(&self, other: &Self) -> Ordering;

    #[inline]
    fn less(&self, other: &Self) -> bool {
        self.compare(other) == Ordering::Less
    }

    /// Full key length if stored in the heap, else 0.
    fn heap_size(&self) -> usize {
        0
    }
    fn heap_data(&self) -> *mut u8 {
        std::ptr::null_mut()
    }
    fn set_heap(&mut self, _size: usize, _data: *mut u8) {}

    fn payload(&self) -> *mut u8 {
        std::ptr::null_mut()
    }
    fn set_payload(&mut self, _payload: *mut u8) {}

    /// Builds the key from normalized key bytes; keys longer than the inline part are
    /// copied to `*heap`, which is advanced.
    ///
    /// # Safety
    /// For long keys, `*heap` must be writable for `key.len()` bytes.
    unsafe fn construct(&mut self, key: &[u8], heap: &mut *mut u8) {
        *self = Self::default();
        let inline = key.len().min(Self::INLINE_LENGTH);
        let parts = self.parts_mut();
        for (part, chunk) in parts.iter_mut().zip(key[..inline].chunks(8)) {
            let mut bytes = [0u8; 8];
            bytes[..chunk.len()].copy_from_slice(chunk);
            *part = u64::from_be_bytes(bytes);
        }
        if key.len() > Self::INLINE_LENGTH {
            // SAFETY: the caller reserved `key.len()` heap bytes.
            unsafe {
                std::ptr::copy_nonoverlapping(key.as_ptr(), *heap, key.len());
                self.set_heap(key.len(), *heap);
                *heap = heap.add(key.len());
            }
        }
    }

    /// Appends the normalized key bytes (inline keys are zero-padded) to `out`.
    ///
    /// # Safety
    /// The heap data of a long key must still be valid.
    unsafe fn decode_into(&self, out: &mut Vec<u8>) {
        let size = self.heap_size();
        if size > Self::INLINE_LENGTH {
            // SAFETY: guaranteed by the caller.
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(self.heap_data(), size) });
        } else {
            for part in self.parts() {
                out.extend_from_slice(&part.to_be_bytes());
            }
        }
    }
}

/// Payload pointer accessors for key structs with a `payload` field.
macro_rules! payload_methods {
    () => {
        #[inline]
        fn payload(&self) -> *mut u8 {
            self.payload as usize as *mut u8
        }

        #[inline]
        fn set_payload(&mut self, payload: *mut u8) {
            self.payload = payload as usize as u64;
        }
    };
}

/// Inline part accessors.
macro_rules! part_methods {
    () => {
        #[inline]
        fn parts(&self) -> &[u64] {
            &self.parts
        }

        #[inline]
        fn parts_mut(&mut self) -> &mut [u64] {
            &mut self.parts
        }
    };
}

/// Heap accessors and ordering of variable-size keys.
macro_rules! variable_methods {
    () => {
        #[inline]
        fn compare(&self, other: &Self) -> Ordering {
            self.parts.cmp(&other.parts).then_with(|| {
                compare_variable_tail(
                    self.size,
                    self.data,
                    other.size,
                    other.data,
                    Self::INLINE_LENGTH,
                )
            })
        }

        fn heap_size(&self) -> usize {
            self.size as usize
        }

        fn heap_data(&self) -> *mut u8 {
            self.data as usize as *mut u8
        }

        fn set_heap(&mut self, size: usize, data: *mut u8) {
            self.size = size as u64;
            self.data = data as usize as u64;
        }
    };
}

/// Compares variable keys after their inline parts were found equal.
#[inline]
fn compare_variable_tail(
    lhs_size: u64,
    lhs_data: u64,
    rhs_size: u64,
    rhs_data: u64,
    inline_length: usize,
) -> Ordering {
    if lhs_size as usize <= inline_length || rhs_size as usize <= inline_length {
        // An inline key is shorter than any key with the same prefix in the heap.
        return lhs_size.cmp(&rhs_size);
    }
    // SAFETY: keys longer than the inline part point to their full bytes in a pinned
    // heap block.
    let (lhs, rhs) = unsafe {
        (
            std::slice::from_raw_parts(lhs_data as usize as *const u8, lhs_size as usize),
            std::slice::from_raw_parts(rhs_data as usize as *const u8, rhs_size as usize),
        )
    };
    lhs.cmp(rhs)
}

macro_rules! sort_key {
    (fixed $name:ident, $parts:literal) => {
        /// A fixed-size sort key without payload.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        #[repr(C)]
        pub(crate) struct $name {
            parts: [u64; $parts],
        }

        impl SortKey for $name {
            const TYPE: SortKeyType = SortKeyType::$name;
            part_methods!();

            #[inline]
            fn compare(&self, other: &Self) -> Ordering {
                self.parts.cmp(&other.parts)
            }
        }
    };
    (fixed_payload $name:ident, $parts:literal) => {
        /// A fixed-size sort key pointing to its payload row.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        #[repr(C)]
        pub(crate) struct $name {
            parts: [u64; $parts],
            payload: u64,
        }

        impl SortKey for $name {
            const TYPE: SortKeyType = SortKeyType::$name;
            part_methods!();
            payload_methods!();

            #[inline]
            fn compare(&self, other: &Self) -> Ordering {
                self.parts.cmp(&other.parts)
            }
        }
    };
    (variable $name:ident, $parts:literal) => {
        /// A variable-size sort key: inline prefix plus the full key in the heap.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        #[repr(C)]
        pub(crate) struct $name {
            parts: [u64; $parts],
            size: u64,
            data: u64,
        }

        impl SortKey for $name {
            const TYPE: SortKeyType = SortKeyType::$name;
            part_methods!();
            variable_methods!();
        }
    };
    (variable_payload $name:ident, $parts:literal) => {
        /// A variable-size sort key pointing to its payload row.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        #[repr(C)]
        pub(crate) struct $name {
            parts: [u64; $parts],
            size: u64,
            data: u64,
            payload: u64,
        }

        impl SortKey for $name {
            const TYPE: SortKeyType = SortKeyType::$name;
            part_methods!();
            variable_methods!();
            payload_methods!();
        }
    };
}

sort_key!(fixed NoPayloadFixed8, 1);
sort_key!(fixed NoPayloadFixed16, 2);
sort_key!(fixed NoPayloadFixed24, 3);
sort_key!(fixed NoPayloadFixed32, 4);
sort_key!(variable NoPayloadVariable32, 2);
sort_key!(fixed_payload PayloadFixed16, 1);
sort_key!(fixed_payload PayloadFixed24, 2);
sort_key!(fixed_payload PayloadFixed32, 3);
sort_key!(variable_payload PayloadVariable32, 1);

const _: () = {
    assert!(size_of::<NoPayloadFixed8>() == 8);
    assert!(size_of::<NoPayloadFixed16>() == 16);
    assert!(size_of::<NoPayloadFixed24>() == 24);
    assert!(size_of::<NoPayloadFixed32>() == 32);
    assert!(size_of::<NoPayloadVariable32>() == 32);
    assert!(size_of::<PayloadFixed16>() == 16);
    assert!(size_of::<PayloadFixed24>() == 24);
    assert!(size_of::<PayloadFixed32>() == 32);
    assert!(size_of::<PayloadVariable32>() == 32);
};

/// Calls `$body` with `$K` bound to the key struct of `$key_type`.
macro_rules! dispatch_sort_key {
    ($key_type:expr, $K:ident => $body:expr) => {{
        use $crate::sort::key::SortKeyType as T;
        match $key_type {
            T::NoPayloadFixed8 => {
                type $K = $crate::sort::key::NoPayloadFixed8;
                $body
            }
            T::NoPayloadFixed16 => {
                type $K = $crate::sort::key::NoPayloadFixed16;
                $body
            }
            T::NoPayloadFixed24 => {
                type $K = $crate::sort::key::NoPayloadFixed24;
                $body
            }
            T::NoPayloadFixed32 => {
                type $K = $crate::sort::key::NoPayloadFixed32;
                $body
            }
            T::NoPayloadVariable32 => {
                type $K = $crate::sort::key::NoPayloadVariable32;
                $body
            }
            T::PayloadFixed16 => {
                type $K = $crate::sort::key::PayloadFixed16;
                $body
            }
            T::PayloadFixed24 => {
                type $K = $crate::sort::key::PayloadFixed24;
                $body
            }
            T::PayloadFixed32 => {
                type $K = $crate::sort::key::PayloadFixed32;
                $body
            }
            T::PayloadVariable32 => {
                type $K = $crate::sort::key::PayloadVariable32;
                $body
            }
        }
    }};
}
pub(crate) use dispatch_sort_key;

#[cfg(test)]
mod tests {
    use super::*;

    fn key<K: SortKey>(bytes: &[u8], heap: &mut Vec<u8>) -> K {
        let mut key = K::default();
        let mut ptr = heap.as_mut_ptr();
        // SAFETY: `heap` has room for any key used in the tests.
        unsafe { key.construct(bytes, &mut ptr) };
        key
    }

    #[test]
    fn fixed_keys_compare_bytewise() {
        let mut heap = vec![0; 64];
        let a: NoPayloadFixed16 = key(&[1, 2, 3, 4, 5, 6, 7, 8, 9], &mut heap);
        let b: NoPayloadFixed16 = key(&[1, 2, 3, 4, 5, 6, 7, 8, 10], &mut heap);
        let c: NoPayloadFixed16 = key(&[2], &mut heap);
        assert!(a.less(&b) && b.less(&c));
        let mut decoded = Vec::new();
        // SAFETY: inline key.
        unsafe { a.decode_into(&mut decoded) };
        assert_eq!(&decoded[..9], &[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert!(decoded[9..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn variable_keys_fall_back_to_the_heap() {
        let mut heap_a = vec![0; 64];
        let mut heap_b = vec![0; 64];
        let long_a: Vec<u8> = (0..30).collect();
        let mut long_b = long_a.clone();
        long_b[29] = 100;
        let a: NoPayloadVariable32 = key(&long_a, &mut heap_a);
        let b: NoPayloadVariable32 = key(&long_b, &mut heap_b);
        let short: NoPayloadVariable32 = key(&long_a[..16], &mut vec![0; 8]);
        assert_eq!(a.heap_size(), 30);
        assert!(a.less(&b));
        assert!(
            short.less(&a),
            "an inline key is shorter than a longer key with its prefix"
        );
        assert_eq!(a.compare(&a), Ordering::Equal);
        let mut decoded = Vec::new();
        // SAFETY: the heap is alive.
        unsafe { b.decode_into(&mut decoded) };
        assert_eq!(decoded, long_b);
    }
}
