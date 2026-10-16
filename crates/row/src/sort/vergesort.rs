//! Vergesort for random-access sequences, as vendored by DuckDB
//! (`third_party/vergesort/vergesort.h`, Morwenn, MIT license): finds big ascending or
//! descending runs, sorts the parts between them with a fallback sort, then merges the
//! runs pairwise. Ported operation for operation so that equal keys end up in DuckDB's
//! order.

use std::collections::VecDeque;

use super::seq::{KeySeq, inplace_merge, log2, reverse};

/// Sorts `keys[first..last]` with `less`, sorting unstructured parts with `fallback`
/// (`duckdb_vergesort::vergesort` for random-access iterators).
pub(crate) fn vergesort<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    first: usize,
    last: usize,
    less: &impl Fn(&K, &K) -> bool,
    fallback: &mut impl FnMut(&mut S, usize, usize),
) {
    let dist = last - first;
    if dist < 128 {
        // Vergesort is inefficient for small collections.
        fallback(keys, first, last);
        return;
    }
    // Below this run length a part is sorted by the fallback.
    let unstable_limit = dist / log2(dist);
    // The end of each run, in order.
    let mut runs: VecDeque<usize> = VecDeque::new();
    // The beginning of an unstable part, or `last` if the previous part is a run.
    let mut begin_unstable = last;
    let mut current = first;
    let mut next = first + 1;
    loop {
        let begin_range = current;
        // A short tail is unstable.
        if last - next <= unstable_limit {
            if begin_unstable == last {
                begin_unstable = begin_range;
            }
            break;
        }
        current += unstable_limit;
        next += unstable_limit;
        let mut current2 = current;
        let mut next2 = next;
        let descending = less(&keys.get(next), &keys.get(current));
        if descending {
            // Extend the decreasing sequence backwards and forwards.
            loop {
                current -= 1;
                next -= 1;
                if less(&keys.get(current), &keys.get(next)) {
                    break;
                }
                if current == begin_range {
                    break;
                }
            }
            if less(&keys.get(current), &keys.get(next)) {
                current += 1;
            }
            current2 += 1;
            next2 += 1;
            while next2 != last {
                if less(&keys.get(current2), &keys.get(next2)) {
                    break;
                }
                current2 += 1;
                next2 += 1;
            }
        } else {
            // Extend the increasing sequence backwards and forwards.
            loop {
                current -= 1;
                next -= 1;
                if less(&keys.get(next), &keys.get(current)) {
                    break;
                }
                if current == begin_range {
                    break;
                }
            }
            if less(&keys.get(next), &keys.get(current)) {
                current += 1;
            }
            current2 += 1;
            next2 += 1;
            while next2 != last {
                if less(&keys.get(next2), &keys.get(current2)) {
                    break;
                }
                current2 += 1;
                next2 += 1;
            }
        }
        if next2 - current >= unstable_limit {
            // A big enough sorted sequence.
            if descending {
                reverse(keys, current, next2);
            }
            if current - begin_range > 0 && begin_unstable == last {
                begin_unstable = begin_range;
            }
            if begin_unstable != last {
                fallback(keys, begin_unstable, current);
                runs.push_back(current);
                begin_unstable = last;
            }
            runs.push_back(next2);
        } else if begin_unstable == last {
            // Remember the beginning of the unsorted sequence.
            begin_unstable = begin_range;
        }
        if next2 == last {
            break;
        }
        current = current2 + 1;
        next = next2 + 1;
    }
    if begin_unstable != last {
        // Sort what is left unsorted.
        runs.push_back(last);
        fallback(keys, begin_unstable, last);
    }
    if runs.len() < 2 {
        return;
    }
    // Merge runs pairwise until one is left.
    while runs.len() > 1 {
        let mut begin = first;
        let mut merged = VecDeque::with_capacity(runs.len().div_ceil(2));
        let mut index = 0;
        while index + 1 < runs.len() {
            inplace_merge(keys, begin, runs[index], runs[index + 1], less);
            merged.push_back(runs[index + 1]);
            begin = runs[index + 1];
            index += 2;
        }
        if index < runs.len() {
            merged.push_back(runs[index]);
        }
        runs = merged;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sort::pdqsort::pdqsort_branchless;

    #[test]
    fn sorts_runs_and_noise() {
        let mut state = 0x9e37_79b9_u64;
        let mut values: Vec<u32> = (0..2000).collect();
        values.extend((0..1500).rev());
        values.extend((0..700).map(|_| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (state >> 40) as u32 % 1000
        }));
        values.extend(500..3000);
        let mut expected = values.clone();
        expected.sort_unstable();
        let len = values.len();
        let less = |a: &u32, b: &u32| a < b;
        vergesort(
            values.as_mut_slice(),
            0,
            len,
            &less,
            &mut |keys: &mut [u32], begin, end| {
                pdqsort_branchless(keys, begin, end, &less);
            },
        );
        assert_eq!(values, expected);
    }
}
