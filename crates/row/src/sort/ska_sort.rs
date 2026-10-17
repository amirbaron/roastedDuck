//! In-place radix sort on the first eight key bytes, as vendored by DuckDB
//! (`third_party/ska_sort/ska_sort.hpp`, Malte Skarupke, Boost license, with DuckDB's
//! changes): American flag sort for small partitions, ska byte sort for large ones, and
//! vergesort + pdqsort on the full key for partitions below 128 keys and for keys that tie
//! on all radix bytes. Ported operation for operation so that equal keys end up in
//! DuckDB's order.

use super::pdqsort::pdqsort_branchless;
use super::seq::KeySeq;
use super::vergesort::vergesort;

/// Partitions below this size are comparison sorted (`StdSortThreshold`).
const STD_SORT_THRESHOLD: usize = 128;
/// Partitions below this size use American flag sort (`AmericanFlagSortThreshold`).
const AMERICAN_FLAG_SORT_THRESHOLD: usize = 1024;
/// The radix key has eight bytes (`UnsignedInplaceSorter<…, 8>`).
const NUM_BYTES: usize = 8;

/// How the radix sort treats a key layout (DuckDB's `SkaExtractKey`).
#[derive(Clone, Debug)]
pub(crate) struct SkaKey {
    /// Whether keys that tie on the radix bytes must be compared in full.
    pub requires_next_sort: bool,
    /// The number of leading key bytes to radix sort on (at most 8).
    pub ska_sort_width: usize,
    /// Byte offsets that are identical in every key.
    pub sort_skippable_bytes: Vec<usize>,
}

impl SkaKey {
    fn byte_is_skippable(&self, offset: usize) -> bool {
        self.sort_skippable_bytes.contains(&offset)
    }
}

/// `ska_sort(begin, end, extract_key)` over `keys[begin..end]`: `radix_key` gives a key's
/// first eight bytes as a big-endian integer, `less` the full order.
pub(crate) fn ska_sort<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    begin: usize,
    end: usize,
    extract: &SkaKey,
    radix_key: &impl Fn(&K) -> u64,
    less: &impl Fn(&K, &K) -> bool,
) {
    // `SortStarter::sort`.
    let num_elements = end - begin;
    if std_sort_if_less_than_threshold(keys, begin, end, num_elements, less) {
        return;
    }
    let sorter = Sorter {
        extract,
        radix_key,
        less,
        next_sort: extract.requires_next_sort,
    };
    sorter.sort(keys, begin, end, num_elements, 0);
}

/// `StdSortFallback`: vergesort with pdqsort on the full key.
fn std_sort_fallback<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    begin: usize,
    end: usize,
    less: &impl Fn(&K, &K) -> bool,
) {
    vergesort(keys, begin, end, less, &mut |keys: &mut S, lo, hi| {
        pdqsort_branchless(keys, lo, hi, less);
    });
}

/// `StdSortIfLessThanThreshold`: comparison sorts small ranges; returns whether it did.
fn std_sort_if_less_than_threshold<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    begin: usize,
    end: usize,
    num_elements: usize,
    less: &impl Fn(&K, &K) -> bool,
) -> bool {
    if num_elements <= 1 {
        return true;
    }
    if num_elements >= STD_SORT_THRESHOLD {
        return false;
    }
    std_sort_fallback(keys, begin, end, less);
    true
}

/// A partition's bounds while sorting on one byte (`PartitionInfo`; `offset` doubles as
/// the count while counting).
#[derive(Clone, Copy, Default)]
struct PartitionInfo {
    offset: usize,
    next_offset: usize,
}

/// `UnsignedInplaceSorter<…, 8, Offset>` with the byte offset as a parameter.
struct Sorter<'a, R, L> {
    extract: &'a SkaKey,
    radix_key: &'a R,
    less: &'a L,
    /// Whether ties on all radix bytes are sorted on the full key (`next_sort`).
    next_sort: bool,
}

impl<R, L> Sorter<'_, R, L> {
    /// `current_byte`: byte `offset` (most significant first) of the radix key.
    #[inline]
    fn current_byte<K>(&self, key: &K, offset: usize) -> usize
    where
        R: Fn(&K) -> u64,
    {
        ((self.radix_key)(key) >> ((NUM_BYTES - 1 - offset) * 8)) as u8 as usize
    }

    /// `UnsignedInplaceSorter::sort`.
    fn sort<K: Copy, S: KeySeq<K> + ?Sized>(
        &self,
        keys: &mut S,
        begin: usize,
        end: usize,
        num_elements: usize,
        offset: usize,
    ) where
        R: Fn(&K) -> u64,
        L: Fn(&K, &K) -> bool,
    {
        if offset == NUM_BYTES {
            // All radix bytes tie: `next_sort`.
            if self.next_sort {
                std_sort_fallback(keys, begin, end, self.less);
            }
        } else if self.extract.byte_is_skippable(offset) {
            self.sort(keys, begin, end, num_elements, offset + 1);
        } else if num_elements < AMERICAN_FLAG_SORT_THRESHOLD {
            self.american_flag_sort(keys, begin, end, offset);
        } else {
            self.ska_byte_sort(keys, begin, end, offset);
        }
    }

    /// Whether partitions must be sorted on the next byte (`Offset + 1 !=
    /// ska_sort_width || next_sort`).
    fn recurses(&self, offset: usize) -> bool {
        offset + 1 != self.extract.ska_sort_width || self.next_sort
    }

    /// Sorts one partition on the next byte, or by comparison if it is small.
    fn sort_partition<K: Copy, S: KeySeq<K> + ?Sized>(
        &self,
        keys: &mut S,
        begin: usize,
        end: usize,
        offset: usize,
    ) where
        R: Fn(&K) -> u64,
        L: Fn(&K, &K) -> bool,
    {
        let num_elements = end - begin;
        if !std_sort_if_less_than_threshold(keys, begin, end, num_elements, self.less) {
            self.sort(keys, begin, end, num_elements, offset + 1);
        }
    }

    /// Counts the keys per byte value and lays the partitions out.
    fn count_partitions<K: Copy, S: KeySeq<K> + ?Sized>(
        &self,
        keys: &S,
        begin: usize,
        end: usize,
        offset: usize,
    ) -> [PartitionInfo; 256]
    where
        R: Fn(&K) -> u64,
    {
        let mut partitions = [PartitionInfo::default(); 256];
        for index in begin..end {
            partitions[self.current_byte(&keys.get(index), offset)].offset += 1;
        }
        partitions
    }

    /// `american_flag_sort`.
    fn american_flag_sort<K: Copy, S: KeySeq<K> + ?Sized>(
        &self,
        keys: &mut S,
        begin: usize,
        end: usize,
        offset: usize,
    ) where
        R: Fn(&K) -> u64,
        L: Fn(&K, &K) -> bool,
    {
        let mut partitions = self.count_partitions(keys, begin, end, offset);
        let mut total = 0;
        let mut remaining_partitions = [0u8; 256];
        let mut num_partitions = 0;
        for (byte, partition) in partitions.iter_mut().enumerate() {
            let count = partition.offset;
            if count == 0 {
                continue;
            }
            partition.offset = total;
            total += count;
            partition.next_offset = total;
            remaining_partitions[num_partitions] = byte as u8;
            num_partitions += 1;
        }
        if num_partitions > 1 {
            let mut current_block_ptr = 0;
            let mut current_block = usize::from(remaining_partitions[current_block_ptr]);
            let last_block = num_partitions - 1;
            let mut it = begin;
            let mut block_end = begin + partitions[current_block].next_offset;
            let last_element = end - 1;
            'swap: loop {
                let block = self.current_byte(&keys.get(it), offset);
                if block == current_block {
                    it += 1;
                    if it == last_element {
                        break;
                    } else if it == block_end {
                        loop {
                            current_block_ptr += 1;
                            if current_block_ptr == last_block {
                                break 'swap;
                            }
                            current_block = usize::from(remaining_partitions[current_block_ptr]);
                            if partitions[current_block].offset != partitions[current_block].next_offset {
                                break;
                            }
                        }
                        it = begin + partitions[current_block].offset;
                        block_end = begin + partitions[current_block].next_offset;
                    }
                } else {
                    let target = partitions[block].offset;
                    partitions[block].offset += 1;
                    keys.swap(it, begin + target);
                }
            }
        }
        if self.recurses(offset) {
            let mut start_offset = 0;
            for &byte in &remaining_partitions[..num_partitions] {
                let end_offset = partitions[usize::from(byte)].next_offset;
                self.sort_partition(keys, begin + start_offset, begin + end_offset, offset);
                start_offset = end_offset;
            }
        }
    }

    /// `ska_byte_sort`.
    fn ska_byte_sort<K: Copy, S: KeySeq<K> + ?Sized>(
        &self,
        keys: &mut S,
        begin: usize,
        end: usize,
        offset: usize,
    ) where
        R: Fn(&K) -> u64,
        L: Fn(&K, &K) -> bool,
    {
        let mut partitions = self.count_partitions(keys, begin, end, offset);
        let mut remaining_partitions = [0u8; 256];
        let mut total = 0;
        let mut num_partitions = 0;
        for (byte, partition) in partitions.iter_mut().enumerate() {
            let count = partition.offset;
            if count != 0 {
                partition.offset = total;
                total += count;
                remaining_partitions[num_partitions] = byte as u8;
                num_partitions += 1;
            }
            partition.next_offset = total;
        }
        let mut last_remaining = num_partitions;
        while last_remaining > 1 {
            // `custom_std_partition` over the remaining partitions, keeping those that still
            // have keys out of place; the swapping updates the partitions in place (the
            // `partitions_copy` of the original is a pointer to the same array).
            let mut keep = |partition: u8, keys: &mut S| -> bool {
                let partition = usize::from(partition);
                if partitions[partition].offset == partitions[partition].next_offset {
                    return false;
                }
                let from = partitions[partition].offset;
                let to = partitions[partition].next_offset;
                for index in begin + from..begin + to {
                    let this_partition = self.current_byte(&keys.get(index), offset);
                    let target = partitions[this_partition].offset;
                    partitions[this_partition].offset += 1;
                    keys.swap(index, begin + target);
                }
                partitions[partition].offset != partitions[partition].next_offset
            };
            let mut first = 0;
            loop {
                if first == last_remaining {
                    break;
                }
                if !keep(remaining_partitions[first], keys) {
                    break;
                }
                first += 1;
            }
            if first != last_remaining {
                let mut it = first + 1;
                while it != last_remaining {
                    if keep(remaining_partitions[it], keys) {
                        remaining_partitions.swap(first, it);
                        first += 1;
                    }
                    it += 1;
                }
            }
            last_remaining = first;
        }
        if self.recurses(offset) {
            for &byte in remaining_partitions[..num_partitions].iter().rev() {
                let partition = usize::from(byte);
                let start_offset = if partition == 0 {
                    0
                } else {
                    partitions[partition - 1].next_offset
                };
                let end_offset = partitions[partition].next_offset;
                self.sort_partition(keys, begin + start_offset, begin + end_offset, offset);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_on_the_radix_key_then_the_full_key() {
        let mut state = 0xdead_beef_u64;
        for len in [5usize, 127, 128, 1023, 1024, 5000, 20000] {
            let mut keys: Vec<(u64, u32)> = (0..len)
                .map(|_| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    // Few distinct leading bytes, so partitions tie and recurse.
                    (
                        ((state >> 60) << 56) | ((state >> 20) % 1000),
                        (state >> 33) as u32 % 50,
                    )
                })
                .collect();
            let mut expected = keys.clone();
            expected.sort_unstable();
            let extract = SkaKey {
                requires_next_sort: true,
                ska_sort_width: 8,
                sort_skippable_bytes: vec![1],
            };
            let len = keys.len();
            ska_sort(
                keys.as_mut_slice(),
                0,
                len,
                &extract,
                &|key: &(u64, u32)| key.0,
                &|a: &(u64, u32), b: &(u64, u32)| a < b,
            );
            assert_eq!(keys, expected, "len {len}");
        }
    }
}
