//! Linux reference introsort behavior for observable equal-key shell ordering.
pub fn sort<T: Clone>(values: &mut [T], less: &mut impl FnMut(&T, &T) -> bool) {
    if values.len() < 2 {
        return;
    }
    let depth = 2 * (usize::BITS - 1 - values.len().leading_zeros()) as usize;
    introsort(values, 0, values.len(), depth, less);
    let first = values.len().min(16);
    insertion(values, 0, first, less);
    for i in first..values.len() {
        let value = values[i].clone();
        let mut hole = i;
        while hole > 0 && less(&value, &values[hole - 1]) {
            values[hole] = values[hole - 1].clone();
            hole -= 1;
        }
        values[hole] = value;
    }
}
fn insertion<T: Clone>(v: &mut [T], begin: usize, end: usize, less: &mut impl FnMut(&T, &T) -> bool) {
    for i in begin + 1..end {
        let value = v[i].clone();
        let mut hole = i;
        while hole > begin && less(&value, &v[hole - 1]) {
            v[hole] = v[hole - 1].clone();
            hole -= 1;
        }
        v[hole] = value;
    }
}
fn introsort<T: Clone>(
    v: &mut [T],
    begin: usize,
    mut end: usize,
    mut depth: usize,
    less: &mut impl FnMut(&T, &T) -> bool,
) {
    while end - begin > 16 {
        if depth == 0 {
            heap_sort(&mut v[begin..end], less);
            return;
        }
        depth -= 1;
        let a = begin + 1;
        let b = begin + (end - begin) / 2;
        let c = end - 1;
        let median = if less(&v[a], &v[b]) {
            if less(&v[b], &v[c]) {
                b
            } else if less(&v[a], &v[c]) {
                c
            } else {
                a
            }
        } else if less(&v[a], &v[c]) {
            a
        } else if less(&v[b], &v[c]) {
            c
        } else {
            b
        };
        v.swap(begin, median);
        let pivot = v[begin].clone();
        let mut left = begin + 1;
        let mut right = end;
        loop {
            while less(&v[left], &pivot) {
                left += 1;
            }
            right -= 1;
            while less(&pivot, &v[right]) {
                right -= 1;
            }
            if left >= right {
                break;
            }
            v.swap(left, right);
            left += 1;
        }
        introsort(v, left, end, depth, less);
        end = left;
    }
}
fn heap_sort<T: Clone>(v: &mut [T], less: &mut impl FnMut(&T, &T) -> bool) {
    fn sift<T>(v: &mut [T], mut root: usize, end: usize, less: &mut impl FnMut(&T, &T) -> bool) {
        loop {
            let child = root * 2 + 1;
            if child >= end {
                return;
            }
            let child = if child + 1 < end && less(&v[child], &v[child + 1]) {
                child + 1
            } else {
                child
            };
            if !less(&v[root], &v[child]) {
                return;
            }
            v.swap(root, child);
            root = child;
        }
    }
    for i in (0..v.len() / 2).rev() {
        sift(v, i, v.len(), less);
    }
    for end in (1..v.len()).rev() {
        v.swap(0, end);
        sift(v, 0, end, less);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_gnu_sort_tie_fixtures_match() {
        for line in include_str!("../tests/data/libstdcxx_sort_order.tsv")
            .lines()
            .filter(|l| l.starts_with("S "))
        {
            let fields: Vec<usize> = line
                .split_whitespace()
                .skip(1)
                .map(|n| n.parse().expect("frozen numeric sort fixture"))
                .collect();
            let (count, mode) = (fields[0], fields[1]);
            let mut state = 0x1234_5678_9abc_def0_u64;
            let mut values: Vec<(usize, u64)> = (0..count)
                .map(|i| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    let key = match mode {
                        0 => i as u64,
                        1 => (count - i) as u64,
                        2 => 0,
                        _ => state % 17,
                    };
                    (i, key)
                })
                .collect();
            sort(&mut values, &mut |a, b| a.1 < b.1);
            assert_eq!(
                values.iter().map(|v| v.0).collect::<Vec<_>>(),
                fields[2..],
                "count={count} mode={mode}"
            );
        }
    }
}
