//! Sorting keys in place (DuckDB's `TemplatedSort`): vergesort over the whole run, with
//! `ska_sort` (a radix sort on the first eight key bytes) as the fallback for unsorted
//! parts, over a contiguous copy or directly over the run's key blocks.

use super::key::SortKey;
use super::seq::KeySeq;
use super::ska_sort::{SkaKey, ska_sort};
use super::vergesort::vergesort;

/// The radix sort parameters of a key layout (DuckDB's `SkaExtractKey`): the radix sort
/// covers the first `sort_width` (at most 8) bytes, skipping always-identical bytes, and
/// unless this is an (approximate) index sort, keys that are not fully covered by the
/// first eight bytes are finished with a comparison sort.
pub(crate) fn ska_key<K: SortKey>(
    sort_width: Option<usize>,
    skippable_bytes: &[usize],
    is_index_sort: bool,
) -> SkaKey {
    SkaKey {
        requires_next_sort: !is_index_sort && (!K::TYPE.is_constant_size() || K::INLINE_LENGTH != 8),
        ska_sort_width: sort_width.unwrap_or(8).min(8),
        sort_skippable_bytes: skippable_bytes.to_vec(),
    }
}

/// Sorts every key of `keys` (`TemplatedSort`).
pub(crate) fn sort_keys<K: SortKey, S: KeySeq<K> + ?Sized>(keys: &mut S, extract: &SkaKey) {
    let len = keys.len();
    let less = |a: &K, b: &K| a.less(b);
    let radix_key = |key: &K| key.part0();
    vergesort(keys, 0, len, &less, &mut |keys: &mut S, begin, end| {
        ska_sort(keys, begin, end, extract, &radix_key, &less);
    });
}

/// Keys stored in equally sized blocks, addressed by a global index.
pub(crate) struct KeyBlocks<K> {
    blocks: Vec<*mut K>,
    per_block: usize,
    divider: FastDiv,
    len: usize,
}

impl<K: SortKey> KeyBlocks<K> {
    /// # Safety
    /// Each pointer must address a block holding `per_block` keys of type `K` (the last
    /// one at least the rest of `len`), aligned for `K` and valid while this value lives.
    /// Sorting (writing) requires that no one else uses the keys meanwhile.
    pub(crate) unsafe fn new(blocks: Vec<*mut u8>, per_block: usize, len: usize) -> Self {
        Self {
            blocks: blocks.into_iter().map(<*mut u8>::cast::<K>).collect(),
            per_block,
            divider: FastDiv::new(per_block as u64),
            len,
        }
    }

    #[inline]
    fn locate(&self, index: usize) -> (usize, usize) {
        let block = self.divider.div(index as u64) as usize;
        (block, index - block * self.per_block)
    }

    /// The address of key `index`.
    #[inline]
    pub(crate) fn ptr_at(&self, index: usize) -> *mut K {
        let (block, offset) = self.locate(index);
        // SAFETY: `index < len`, so the key lies within its block (constructor contract).
        unsafe { self.blocks[block].add(offset) }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// The key at `index`.
    #[inline]
    pub(crate) fn get(&self, index: usize) -> K {
        debug_assert!(index < self.len);
        // SAFETY: valid, aligned key (constructor contract).
        unsafe { self.ptr_at(index).read() }
    }

    #[inline]
    fn set(&mut self, index: usize, key: K) {
        debug_assert!(index < self.len);
        // SAFETY: valid, aligned key used exclusively (constructor contract).
        unsafe { self.ptr_at(index).write(key) };
    }

    /// Copies every key to `out`, which must have room for [`Self::len`] keys.
    ///
    /// # Safety
    /// `out` must be valid for writing `len` keys and not overlap the blocks.
    pub(crate) unsafe fn copy_to(&self, out: *mut K) {
        let mut copied = 0;
        for &block in &self.blocks {
            let n = self.per_block.min(self.len - copied);
            if n == 0 {
                break;
            }
            // SAFETY: the block holds at least `n` keys (constructor contract) and `out`
            // has room for `len` keys (caller contract).
            unsafe { std::ptr::copy_nonoverlapping(block, out.add(copied), n) };
            copied += n;
        }
    }

    /// Overwrites every key with `keys` (in order).
    pub(crate) fn copy_from(&mut self, keys: &[K]) {
        for (&block, chunk) in self.blocks.iter().zip(keys[..self.len].chunks(self.per_block)) {
            // SAFETY: the block holds `chunk.len()` keys, used exclusively (constructor
            // contract); `keys` is a distinct allocation.
            unsafe { std::ptr::copy_nonoverlapping(chunk.as_ptr(), block, chunk.len()) };
        }
    }
}

/// Division by a constant through multiplication (Lemire's fastdiv), exact for numerators
/// below 2^32.
#[derive(Clone, Copy, Debug)]
struct FastDiv {
    divisor: u64,
    multiplier: u64,
}

impl FastDiv {
    fn new(divisor: u64) -> Self {
        let divisor = divisor.max(1);
        Self {
            divisor,
            multiplier: (u64::MAX / divisor).wrapping_add(1),
        }
    }

    #[inline]
    fn div(self, n: u64) -> u64 {
        if self.divisor == 1 {
            return n;
        }
        if n >> 32 != 0 {
            return n / self.divisor;
        }
        ((u128::from(self.multiplier) * u128::from(n)) >> 64) as u64
    }
}

impl<K: SortKey> KeySeq<K> for KeyBlocks<K> {
    #[inline]
    fn len(&self) -> usize {
        self.len
    }
    #[inline]
    fn get(&self, index: usize) -> K {
        KeyBlocks::get(self, index)
    }
    #[inline]
    fn set(&mut self, index: usize, key: K) {
        KeyBlocks::set(self, index, key);
    }
}
