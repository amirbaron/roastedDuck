//! Pattern-defeating quicksort, as vendored by DuckDB (`third_party/pdqsort/pdqsort.h`,
//! Orson Peters, zlib license), ported operation for operation so that the order of equal
//! keys matches DuckDB's. DuckDB's copy has no heapsort fallback.

use super::seq::KeySeq;

const INSERTION_SORT_THRESHOLD: usize = 24;
const NINTHER_THRESHOLD: usize = 128;
const PARTIAL_INSERTION_SORT_LIMIT: usize = 8;
const BLOCK_SIZE: usize = 64;

/// Sorts `keys[begin..end]` with `less` (`pdqsort_branchless`).
pub(crate) fn pdqsort_branchless<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    begin: usize,
    end: usize,
    less: &impl Fn(&K, &K) -> bool,
) {
    if begin == end {
        return;
    }
    pdqsort_loop(keys, begin, end, less, true);
}

/// `insertion_sort`.
fn insertion_sort<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    begin: usize,
    end: usize,
    less: &impl Fn(&K, &K) -> bool,
) {
    if begin == end {
        return;
    }
    for cur in begin + 1..end {
        let mut sift = cur;
        let mut sift_1 = cur - 1;
        if less(&keys.get(sift), &keys.get(sift_1)) {
            let tmp = keys.get(sift);
            loop {
                keys.set(sift, keys.get(sift_1));
                sift -= 1;
                if sift == begin {
                    break;
                }
                sift_1 -= 1;
                if !less(&tmp, &keys.get(sift_1)) {
                    break;
                }
            }
            keys.set(sift, tmp);
        }
    }
}

/// `unguarded_insertion_sort`: the element before `begin` is a sentinel no larger than any
/// element of the range.
fn unguarded_insertion_sort<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    begin: usize,
    end: usize,
    less: &impl Fn(&K, &K) -> bool,
) {
    if begin == end {
        return;
    }
    for cur in begin + 1..end {
        let mut sift = cur;
        let mut sift_1 = cur - 1;
        if less(&keys.get(sift), &keys.get(sift_1)) {
            let tmp = keys.get(sift);
            loop {
                keys.set(sift, keys.get(sift_1));
                sift -= 1;
                sift_1 = sift_1.wrapping_sub(1);
                if !less(&tmp, &keys.get(sift_1)) {
                    break;
                }
            }
            keys.set(sift, tmp);
        }
    }
}

/// `partial_insertion_sort`: insertion sort that gives up after moving more than
/// `PARTIAL_INSERTION_SORT_LIMIT` elements; returns whether the range is sorted.
fn partial_insertion_sort<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    begin: usize,
    end: usize,
    less: &impl Fn(&K, &K) -> bool,
) -> bool {
    if begin == end {
        return true;
    }
    let mut limit = 0;
    for cur in begin + 1..end {
        let mut sift = cur;
        let mut sift_1 = cur - 1;
        if less(&keys.get(sift), &keys.get(sift_1)) {
            let tmp = keys.get(sift);
            loop {
                keys.set(sift, keys.get(sift_1));
                sift -= 1;
                if sift == begin {
                    break;
                }
                sift_1 -= 1;
                if !less(&tmp, &keys.get(sift_1)) {
                    break;
                }
            }
            keys.set(sift, tmp);
            limit += cur - sift;
        }
        if limit > PARTIAL_INSERTION_SORT_LIMIT {
            return false;
        }
    }
    true
}

fn sort2<K: Copy, S: KeySeq<K> + ?Sized>(keys: &mut S, a: usize, b: usize, less: &impl Fn(&K, &K) -> bool) {
    if less(&keys.get(b), &keys.get(a)) {
        keys.swap(a, b);
    }
}

fn sort3<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    a: usize,
    b: usize,
    c: usize,
    less: &impl Fn(&K, &K) -> bool,
) {
    sort2(keys, a, b, less);
    sort2(keys, b, c, less);
    sort2(keys, a, b, less);
}

/// `swap_offsets`: swaps the out-of-place elements found by a block partition.
fn swap_offsets<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    first: usize,
    last: usize,
    offsets_l: &[u8],
    offsets_r: &[u8],
    num: usize,
    use_swaps: bool,
) {
    if use_swaps {
        for i in 0..num {
            keys.swap(
                first + usize::from(offsets_l[i]),
                last - usize::from(offsets_r[i]),
            );
        }
    } else if num > 0 {
        let mut l = first + usize::from(offsets_l[0]);
        let mut r = last - usize::from(offsets_r[0]);
        let tmp = keys.get(l);
        keys.set(l, keys.get(r));
        for i in 1..num {
            l = first + usize::from(offsets_l[i]);
            keys.set(r, keys.get(l));
            r = last - usize::from(offsets_r[i]);
            keys.set(l, keys.get(r));
        }
        keys.set(r, tmp);
    }
}

/// `partition_right_branchless`: partitions around `keys[begin]` with block partitioning;
/// elements equal to the pivot go right. Returns the pivot's final position and whether
/// the range was already partitioned.
fn partition_right_branchless<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    begin: usize,
    end: usize,
    less: &impl Fn(&K, &K) -> bool,
) -> (usize, bool) {
    let pivot = keys.get(begin);
    let mut first = begin;
    let mut last = end;
    loop {
        first += 1;
        if !less(&keys.get(first), &pivot) {
            break;
        }
    }
    if first - 1 == begin {
        while first < last {
            last -= 1;
            if less(&keys.get(last), &pivot) {
                break;
            }
        }
    } else {
        loop {
            last -= 1;
            if less(&keys.get(last), &pivot) {
                break;
            }
        }
    }
    let already_partitioned = first >= last;
    if !already_partitioned {
        keys.swap(first, last);
        first += 1;
        let mut offsets_l = [0u8; BLOCK_SIZE];
        let mut offsets_r = [0u8; BLOCK_SIZE];
        let mut offsets_l_base = first;
        let mut offsets_r_base = last;
        let (mut num_l, mut num_r, mut start_l, mut start_r) = (0usize, 0usize, 0usize, 0usize);
        while first < last {
            let num_unknown = last - first;
            let left_split = if num_l == 0 {
                if num_r == 0 { num_unknown / 2 } else { num_unknown }
            } else {
                0
            };
            let right_split = if num_r == 0 { num_unknown - left_split } else { 0 };
            if left_split >= BLOCK_SIZE {
                for i in 0..BLOCK_SIZE {
                    offsets_l[num_l] = i as u8;
                    num_l += usize::from(!less(&keys.get(first), &pivot));
                    first += 1;
                }
            } else {
                for i in 0..left_split {
                    offsets_l[num_l] = i as u8;
                    num_l += usize::from(!less(&keys.get(first), &pivot));
                    first += 1;
                }
            }
            if right_split >= BLOCK_SIZE {
                for i in 0..BLOCK_SIZE {
                    offsets_r[num_r] = (i + 1) as u8;
                    last -= 1;
                    num_r += usize::from(less(&keys.get(last), &pivot));
                }
            } else {
                for i in 0..right_split {
                    offsets_r[num_r] = (i + 1) as u8;
                    last -= 1;
                    num_r += usize::from(less(&keys.get(last), &pivot));
                }
            }
            let num = num_l.min(num_r);
            swap_offsets(
                keys,
                offsets_l_base,
                offsets_r_base,
                &offsets_l[start_l..],
                &offsets_r[start_r..],
                num,
                num_l == num_r,
            );
            num_l -= num;
            num_r -= num;
            start_l += num;
            start_r += num;
            if num_l == 0 {
                start_l = 0;
                offsets_l_base = first;
            }
            if num_r == 0 {
                start_r = 0;
                offsets_r_base = last;
            }
        }
        if num_l > 0 {
            while num_l > 0 {
                num_l -= 1;
                last -= 1;
                keys.swap(offsets_l_base + usize::from(offsets_l[start_l + num_l]), last);
            }
            first = last;
        }
        if num_r > 0 {
            while num_r > 0 {
                num_r -= 1;
                keys.swap(offsets_r_base - usize::from(offsets_r[start_r + num_r]), first);
                first += 1;
            }
            last = first;
        }
        let _ = last;
    }
    let pivot_pos = first - 1;
    keys.set(begin, keys.get(pivot_pos));
    keys.set(pivot_pos, pivot);
    (pivot_pos, already_partitioned)
}

/// `partition_left`: partitions around `keys[begin]`; elements equal to the pivot go left.
/// Used when the pivot equals the element before the range.
fn partition_left<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    begin: usize,
    end: usize,
    less: &impl Fn(&K, &K) -> bool,
) -> usize {
    let pivot = keys.get(begin);
    let mut first = begin;
    let mut last = end;
    loop {
        last -= 1;
        if !less(&pivot, &keys.get(last)) {
            break;
        }
    }
    if last + 1 == end {
        while first < last {
            first += 1;
            if less(&pivot, &keys.get(first)) {
                break;
            }
        }
    } else {
        loop {
            first += 1;
            if less(&pivot, &keys.get(first)) {
                break;
            }
        }
    }
    while first < last {
        keys.swap(first, last);
        loop {
            last -= 1;
            if !less(&pivot, &keys.get(last)) {
                break;
            }
        }
        loop {
            first += 1;
            if less(&pivot, &keys.get(first)) {
                break;
            }
        }
    }
    let pivot_pos = last;
    keys.set(begin, keys.get(pivot_pos));
    keys.set(pivot_pos, pivot);
    pivot_pos
}

/// `pdqsort_loop` with branchless partitioning.
fn pdqsort_loop<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    mut begin: usize,
    end: usize,
    less: &impl Fn(&K, &K) -> bool,
    mut leftmost: bool,
) {
    loop {
        let size = end - begin;
        if size < INSERTION_SORT_THRESHOLD {
            if leftmost {
                insertion_sort(keys, begin, end, less);
            } else {
                unguarded_insertion_sort(keys, begin, end, less);
            }
            return;
        }
        let s2 = size / 2;
        if size > NINTHER_THRESHOLD {
            sort3(keys, begin, begin + s2, end - 1, less);
            sort3(keys, begin + 1, begin + (s2 - 1), end - 2, less);
            sort3(keys, begin + 2, begin + (s2 + 1), end - 3, less);
            sort3(keys, begin + (s2 - 1), begin + s2, begin + (s2 + 1), less);
            keys.swap(begin, begin + s2);
        } else {
            sort3(keys, begin + s2, begin, end - 1, less);
        }
        if !leftmost && !less(&keys.get(begin - 1), &keys.get(begin)) {
            begin = partition_left(keys, begin, end, less) + 1;
            continue;
        }
        let (pivot_pos, already_partitioned) = partition_right_branchless(keys, begin, end, less);
        let l_size = pivot_pos - begin;
        let r_size = end - (pivot_pos + 1);
        let highly_unbalanced = l_size < size / 8 || r_size < size / 8;
        if highly_unbalanced {
            if l_size >= INSERTION_SORT_THRESHOLD {
                keys.swap(begin, begin + l_size / 4);
                keys.swap(pivot_pos - 1, pivot_pos - l_size / 4);
                if l_size > NINTHER_THRESHOLD {
                    keys.swap(begin + 1, begin + (l_size / 4 + 1));
                    keys.swap(begin + 2, begin + (l_size / 4 + 2));
                    keys.swap(pivot_pos - 2, pivot_pos - (l_size / 4 + 1));
                    keys.swap(pivot_pos - 3, pivot_pos - (l_size / 4 + 2));
                }
            }
            if r_size >= INSERTION_SORT_THRESHOLD {
                keys.swap(pivot_pos + 1, pivot_pos + (1 + r_size / 4));
                keys.swap(end - 1, end - r_size / 4);
                if r_size > NINTHER_THRESHOLD {
                    keys.swap(pivot_pos + 2, pivot_pos + (2 + r_size / 4));
                    keys.swap(pivot_pos + 3, pivot_pos + (3 + r_size / 4));
                    keys.swap(end - 2, end - (1 + r_size / 4));
                    keys.swap(end - 3, end - (2 + r_size / 4));
                }
            }
        } else if already_partitioned
            && partial_insertion_sort(keys, begin, pivot_pos, less)
            && partial_insertion_sort(keys, pivot_pos + 1, end, less)
        {
            return;
        }
        pdqsort_loop(keys, begin, pivot_pos, less, leftmost);
        begin = pivot_pos + 1;
        leftmost = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_like_a_reference_sort() {
        let mut state = 0x1234_5678_9abc_def0_u64;
        for len in [0usize, 1, 2, 23, 24, 25, 127, 128, 129, 1000, 5000] {
            let mut values: Vec<u32> = (0..len)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (state % 97) as u32
                })
                .collect();
            let mut expected = values.clone();
            expected.sort_unstable();
            let len = values.len();
            pdqsort_branchless(values.as_mut_slice(), 0, len, &|a: &u32, b: &u32| a < b);
            assert_eq!(values, expected, "len {len}");
        }
    }
}
