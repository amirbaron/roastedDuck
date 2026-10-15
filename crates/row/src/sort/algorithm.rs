//! In-place sorting of sort keys spread over buffer-managed blocks (DuckDB sorts with
//! vergesort + `ska_sort` over a block iterator).
//!
//! Keys are radix sorted most-significant byte first on their first eight bytes
//! (American flag sort, in place). As soon as a bucket lies within one block it is sorted
//! as a contiguous slice; small buckets use insertion sort; buckets that tie on all eight
//! bytes are finished with a comparison sort when the keys are longer than that. Already
//! sorted (or reverse sorted) input is detected up front.

use std::cmp::Ordering;

use super::key::SortKey;

/// Below this many keys, insertion sort.
const INSERTION_THRESHOLD: usize = 24;

/// How the radix sort treats a key layout.
#[derive(Clone, Debug)]
pub(crate) struct RadixPlan {
    /// Number of leading key bytes worth radix sorting (at most 8).
    pub(crate) radix_bytes: usize,
    /// Bytes known to be identical in every key (always-valid validity bytes).
    pub(crate) skippable: [bool; 8],
    /// Whether keys longer than eight bytes need a comparison sort after radix sorting.
    pub(crate) requires_next_sort: bool,
}

impl RadixPlan {
    pub(crate) fn new<K: SortKey>(sort_width: Option<usize>, skippable_bytes: &[usize]) -> Self {
        let mut skippable = [false; 8];
        for &byte in skippable_bytes {
            if byte < 8 {
                skippable[byte] = true;
            }
        }
        Self {
            radix_bytes: sort_width.unwrap_or(8).min(8),
            skippable,
            requires_next_sort: !K::TYPE.is_constant_size() || K::INLINE_LENGTH != 8,
        }
    }

    /// The first byte at or after `byte` worth sorting on, if any.
    fn next_byte(&self, mut byte: usize) -> Option<usize> {
        while byte < self.radix_bytes {
            if !self.skippable[byte] {
                return Some(byte);
            }
            byte += 1;
        }
        None
    }
}

#[inline]
fn digit<K: SortKey>(key: &K, byte: usize) -> usize {
    ((key.part0() >> (56 - 8 * byte)) & 0xFF) as usize
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

    /// The keys `lo..hi` as a slice, if they lie in one block.
    fn slice(&mut self, lo: usize, hi: usize) -> Option<&mut [K]> {
        let (block, offset) = self.locate(lo);
        if offset + (hi - lo) > self.per_block {
            return None;
        }
        // SAFETY: the range is within one block (checked) and used exclusively.
        Some(unsafe { std::slice::from_raw_parts_mut(self.blocks[block].add(offset), hi - lo) })
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

/// Sorts every key of `blocks` (`TemplatedSort`).
pub(crate) fn sort_blocks<K: SortKey>(blocks: &mut KeyBlocks<K>, plan: &RadixPlan) {
    let len = blocks.len();
    if len < 2 || presorted(blocks) {
        return;
    }
    sort_block_range(blocks, 0, len, 0, plan);
}

/// Sorts a slice of keys.
pub(crate) fn sort_slice<K: SortKey>(keys: &mut [K], plan: &RadixPlan) {
    if keys.len() < 2 {
        return;
    }
    if keys.is_sorted_by(|a, b| !b.less(a)) {
        return;
    }
    if keys.windows(2).all(|pair| pair[1].less(&pair[0])) {
        keys.reverse();
        return;
    }
    sort_slice_from(keys, 0, plan);
}

/// Detects sorted and strictly descending input (reversing the latter).
fn presorted<K: SortKey>(blocks: &mut KeyBlocks<K>) -> bool {
    let len = blocks.len();
    let mut previous = blocks.get(0);
    let mut ascending = true;
    let mut descending = true;
    for index in 1..len {
        let key = blocks.get(index);
        match key.compare(&previous) {
            Ordering::Less => ascending = false,
            Ordering::Greater => descending = false,
            // Reversing a non-increasing sequence still sorts it.
            Ordering::Equal => {}
        }
        if !ascending && !descending {
            return false;
        }
        previous = key;
    }
    if !ascending {
        let (mut lo, mut hi) = (0, len - 1);
        while lo < hi {
            let (a, b) = (blocks.get(lo), blocks.get(hi));
            blocks.set(lo, b);
            blocks.set(hi, a);
            lo += 1;
            hi -= 1;
        }
    }
    true
}

fn sort_block_range<K: SortKey>(
    blocks: &mut KeyBlocks<K>,
    lo: usize,
    hi: usize,
    byte: usize,
    plan: &RadixPlan,
) {
    let n = hi - lo;
    if n < 2 {
        return;
    }
    if let Some(slice) = blocks.slice(lo, hi) {
        sort_slice_from(slice, byte, plan);
        return;
    }
    if n <= INSERTION_THRESHOLD {
        let mut keys: Vec<K> = (lo..hi).map(|index| blocks.get(index)).collect();
        insertion_sort(&mut keys);
        for (index, key) in (lo..hi).zip(keys) {
            blocks.set(index, key);
        }
        return;
    }
    let Some(byte) = plan.next_byte(byte) else {
        if plan.requires_next_sort {
            // Keys tie on the radix bytes: finish with a comparison sort.
            let mut keys: Vec<K> = (lo..hi).map(|index| blocks.get(index)).collect();
            keys.sort_unstable_by(SortKey::compare);
            for (index, key) in (lo..hi).zip(keys) {
                blocks.set(index, key);
            }
        }
        return;
    };
    let mut counts = [0usize; 256];
    for index in lo..hi {
        counts[digit(&blocks.get(index), byte)] += 1;
    }
    if counts.contains(&n) {
        sort_block_range(blocks, lo, hi, byte + 1, plan);
        return;
    }
    let (mut heads, tails) = bucket_bounds(&counts, lo);
    for bucket in 0..256 {
        while heads[bucket] < tails[bucket] {
            let mut key = blocks.get(heads[bucket]);
            let mut target = digit(&key, byte);
            while target != bucket {
                let position = heads[target];
                heads[target] += 1;
                let displaced = blocks.get(position);
                blocks.set(position, key);
                key = displaced;
                target = digit(&key, byte);
            }
            blocks.set(heads[bucket], key);
            heads[bucket] += 1;
        }
    }
    let mut start = lo;
    for &count in &counts {
        if count > 1 {
            sort_block_range(blocks, start, start + count, byte + 1, plan);
        }
        start += count;
    }
}

/// `(heads, tails)` of the buckets of `counts`, starting at `base`.
fn bucket_bounds(counts: &[usize; 256], base: usize) -> ([usize; 256], [usize; 256]) {
    let mut heads = [0; 256];
    let mut tails = [0; 256];
    let mut offset = base;
    for bucket in 0..256 {
        heads[bucket] = offset;
        offset += counts[bucket];
        tails[bucket] = offset;
    }
    (heads, tails)
}

fn sort_slice_from<K: SortKey>(keys: &mut [K], byte: usize, plan: &RadixPlan) {
    let n = keys.len();
    if n <= INSERTION_THRESHOLD {
        insertion_sort(keys);
        return;
    }
    let Some(byte) = plan.next_byte(byte) else {
        if plan.requires_next_sort {
            keys.sort_unstable_by(SortKey::compare);
        }
        return;
    };
    let mut counts = [0usize; 256];
    for key in keys.iter() {
        counts[digit(key, byte)] += 1;
    }
    if counts.contains(&n) {
        sort_slice_from(keys, byte + 1, plan);
        return;
    }
    let (mut heads, tails) = bucket_bounds(&counts, 0);
    for bucket in 0..256 {
        while heads[bucket] < tails[bucket] {
            let mut key = keys[heads[bucket]];
            let mut target = digit(&key, byte);
            while target != bucket {
                let position = heads[target];
                heads[target] += 1;
                std::mem::swap(&mut keys[position], &mut key);
                target = digit(&key, byte);
            }
            keys[heads[bucket]] = key;
            heads[bucket] += 1;
        }
    }
    let mut start = 0;
    for &count in &counts {
        if count > 1 {
            sort_slice_from(&mut keys[start..start + count], byte + 1, plan);
        }
        start += count;
    }
}

fn insertion_sort<K: SortKey>(keys: &mut [K]) {
    for i in 1..keys.len() {
        let key = keys[i];
        let mut j = i;
        while j > 0 && key.less(&keys[j - 1]) {
            keys[j] = keys[j - 1];
            j -= 1;
        }
        keys[j] = key;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sort::key::{NoPayloadFixed8, NoPayloadFixed16};

    fn keys<K: SortKey>(values: &[u64]) -> Vec<K> {
        values
            .iter()
            .map(|value| {
                let mut key = K::default();
                let bytes = value.to_be_bytes();
                let mut heap = std::ptr::null_mut();
                // SAFETY: keys of at most 16 bytes never use the heap.
                unsafe { key.construct(&bytes, &mut heap) };
                key
            })
            .collect()
    }

    fn rng(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    #[test]
    fn fast_division_is_exact() {
        for divisor in [1, 3, 7, 10_922, 16_384, 32_768] {
            let fast = FastDiv::new(divisor);
            for n in [
                0,
                1,
                divisor - 1,
                divisor,
                divisor + 1,
                123_456_789,
                u64::from(u32::MAX),
            ] {
                assert_eq!(fast.div(n), n / divisor, "{n} / {divisor}");
            }
        }
    }

    #[test]
    fn sorts_across_blocks() {
        let mut seed = 0x1234_5678;
        for (len, per_block) in [(10_000usize, 1000usize), (5000, 37), (100, 7), (65_536, 10_922)] {
            let values: Vec<u64> = (0..len).map(|_| rng(&mut seed) % 5000).collect();
            let mut storage: Vec<NoPayloadFixed8> = keys(&values);
            let blocks: Vec<*mut u8> = storage
                .chunks_mut(per_block)
                .map(|chunk| chunk.as_mut_ptr().cast::<u8>())
                .collect();
            // SAFETY: the blocks are disjoint chunks of `storage`, used only here.
            let mut paged = unsafe { KeyBlocks::<NoPayloadFixed8>::new(blocks, per_block, len) };
            sort_blocks(&mut paged, &RadixPlan::new::<NoPayloadFixed8>(Some(8), &[]));
            let mut expected = values.clone();
            expected.sort_unstable();
            let sorted: Vec<u64> = storage.iter().map(SortKey::part0).collect();
            assert_eq!(sorted, expected, "len {len}, per block {per_block}");
        }
    }

    #[test]
    fn ties_on_the_first_eight_bytes_use_the_whole_key() {
        let mut seed = 99;
        let mut storage: Vec<NoPayloadFixed16> = (0..3000)
            .map(|_| {
                let mut bytes = [0u8; 16];
                bytes[..8].copy_from_slice(&(rng(&mut seed) % 3).to_be_bytes());
                bytes[8..].copy_from_slice(&rng(&mut seed).to_be_bytes());
                let mut key = NoPayloadFixed16::default();
                let mut heap = std::ptr::null_mut();
                // SAFETY: inline key.
                unsafe { key.construct(&bytes, &mut heap) };
                key
            })
            .collect();
        let mut expected = storage.clone();
        expected.sort_by(SortKey::compare);
        let blocks: Vec<*mut u8> = storage
            .chunks_mut(256)
            .map(|chunk| chunk.as_mut_ptr().cast::<u8>())
            .collect();
        // SAFETY: disjoint chunks of `storage`.
        let mut paged = unsafe { KeyBlocks::<NoPayloadFixed16>::new(blocks, 256, 3000) };
        sort_blocks(&mut paged, &RadixPlan::new::<NoPayloadFixed16>(Some(16), &[]));
        assert_eq!(storage, expected);
    }

    #[test]
    fn presorted_and_reversed_inputs() {
        let mut ascending: Vec<NoPayloadFixed8> = keys(&(0..1000).collect::<Vec<_>>());
        let expected = ascending.clone();
        let mut descending = ascending.clone();
        descending.reverse();
        sort_slice(&mut ascending, &RadixPlan::new::<NoPayloadFixed8>(Some(8), &[]));
        sort_slice(&mut descending, &RadixPlan::new::<NoPayloadFixed8>(Some(8), &[]));
        assert_eq!(ascending, expected);
        assert_eq!(descending, expected);
    }
}
