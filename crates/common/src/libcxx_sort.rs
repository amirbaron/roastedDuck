//! `std::sort` as libc++ implements it (`__algorithm/sort.h` of the toolchain DuckDB's
//! reference build uses): an unstable introsort, ported so that elements the comparison
//! deems equal (in the shell: colors of the same hue, tables of the same height) end up
//! in DuckDB's order. This is the variant for non-arithmetic elements (no branchless
//! sorting networks or bitset partitioning).

/// `std::sort(first, last, comp)` with `less` as the comparison.
pub fn sort<T: Clone>(v: &mut [T], less: &mut impl FnMut(&T, &T) -> bool) {
    if v.is_empty() {
        return;
    }
    // `2 * __bit_log2(len)`.
    let depth_limit = 2 * (usize::BITS - 1 - v.len().leading_zeros()) as usize;
    introsort(v, 0, v.len(), less, depth_limit, true);
}

/// `__sort3`: orders three elements; returns whether it swapped.
fn sort3<T>(v: &mut [T], x: usize, y: usize, z: usize, c: &mut impl FnMut(&T, &T) -> bool) -> bool {
    if !c(&v[y], &v[x]) {
        if !c(&v[z], &v[y]) {
            return false;
        }
        v.swap(y, z);
        if c(&v[y], &v[x]) {
            v.swap(x, y);
        }
        return true;
    }
    if c(&v[z], &v[y]) {
        v.swap(x, z);
        return true;
    }
    v.swap(x, y);
    if c(&v[z], &v[y]) {
        v.swap(y, z);
    }
    true
}

/// `__sort4`.
fn sort4<T>(v: &mut [T], x1: usize, x2: usize, x3: usize, x4: usize, c: &mut impl FnMut(&T, &T) -> bool) {
    sort3(v, x1, x2, x3, c);
    if c(&v[x4], &v[x3]) {
        v.swap(x3, x4);
        if c(&v[x3], &v[x2]) {
            v.swap(x2, x3);
            if c(&v[x2], &v[x1]) {
                v.swap(x1, x2);
            }
        }
    }
}

/// `__sort5`.
fn sort5<T>(v: &mut [T], x: [usize; 5], c: &mut impl FnMut(&T, &T) -> bool) {
    let [x1, x2, x3, x4, x5] = x;
    sort4(v, x1, x2, x3, x4, c);
    if c(&v[x5], &v[x4]) {
        v.swap(x4, x5);
        if c(&v[x4], &v[x3]) {
            v.swap(x3, x4);
            if c(&v[x3], &v[x2]) {
                v.swap(x2, x3);
                if c(&v[x2], &v[x1]) {
                    v.swap(x1, x2);
                }
            }
        }
    }
}

/// `__insertion_sort`.
fn insertion_sort<T: Clone>(v: &mut [T], first: usize, last: usize, c: &mut impl FnMut(&T, &T) -> bool) {
    if first == last {
        return;
    }
    for i in first + 1..last {
        let mut j = i - 1;
        if c(&v[i], &v[j]) {
            let t = v[i].clone();
            let mut k = j;
            j = i;
            loop {
                v[j] = v[k].clone();
                j = k;
                if j == first {
                    break;
                }
                k -= 1;
                if !c(&t, &v[k]) {
                    break;
                }
            }
            v[j] = t;
        }
    }
}

/// `__insertion_sort_unguarded`: the element before `first` is not greater than any in
/// the range.
fn insertion_sort_unguarded<T: Clone>(
    v: &mut [T],
    first: usize,
    last: usize,
    c: &mut impl FnMut(&T, &T) -> bool,
) {
    if first == last {
        return;
    }
    for i in first + 1..last {
        let mut j = i - 1;
        if c(&v[i], &v[j]) {
            let t = v[i].clone();
            let mut k = j;
            j = i;
            loop {
                v[j] = v[k].clone();
                j = k;
                k -= 1;
                if !c(&t, &v[k]) {
                    break;
                }
            }
            v[j] = t;
        }
    }
}

/// `__insertion_sort_incomplete`: sorts small ranges, gives up after 8 moves; returns
/// whether the range is sorted.
fn insertion_sort_incomplete<T: Clone>(
    v: &mut [T],
    first: usize,
    last: usize,
    c: &mut impl FnMut(&T, &T) -> bool,
) -> bool {
    match last - first {
        0 | 1 => return true,
        2 => {
            if c(&v[last - 1], &v[first]) {
                v.swap(first, last - 1);
            }
            return true;
        }
        3 => {
            sort3(v, first, first + 1, last - 1, c);
            return true;
        }
        4 => {
            sort4(v, first, first + 1, first + 2, last - 1, c);
            return true;
        }
        5 => {
            sort5(v, [first, first + 1, first + 2, first + 3, last - 1], c);
            return true;
        }
        _ => {}
    }
    let mut j = first + 2;
    sort3(v, first, first + 1, j, c);
    const LIMIT: u32 = 8;
    let mut count = 0;
    let mut i = j + 1;
    while i != last {
        if c(&v[i], &v[j]) {
            let t = v[i].clone();
            let mut k = j;
            j = i;
            loop {
                v[j] = v[k].clone();
                j = k;
                if j == first {
                    break;
                }
                k -= 1;
                if !c(&t, &v[k]) {
                    break;
                }
            }
            v[j] = t;
            count += 1;
            if count == LIMIT {
                return i + 1 == last;
            }
        }
        j = i;
        i += 1;
    }
    true
}

/// `__partition_with_equals_on_right`: partitions around the first element; returns
/// its position and whether the range was already partitioned.
fn partition_with_equals_on_right<T: Clone>(
    v: &mut [T],
    begin: usize,
    end: usize,
    c: &mut impl FnMut(&T, &T) -> bool,
) -> (usize, bool) {
    let pivot = v[begin].clone();
    let mut first = begin;
    let mut last = end;
    loop {
        first += 1;
        if !c(&v[first], &pivot) {
            break;
        }
    }
    if begin == first - 1 {
        while first < last {
            last -= 1;
            if c(&v[last], &pivot) {
                break;
            }
        }
    } else {
        loop {
            last -= 1;
            if c(&v[last], &pivot) {
                break;
            }
        }
    }
    let already_partitioned = first >= last;
    while first < last {
        v.swap(first, last);
        loop {
            first += 1;
            if !c(&v[first], &pivot) {
                break;
            }
        }
        loop {
            last -= 1;
            if c(&v[last], &pivot) {
                break;
            }
        }
    }
    let pivot_pos = first - 1;
    if begin != pivot_pos {
        v[begin] = v[pivot_pos].clone();
    }
    v[pivot_pos] = pivot;
    (pivot_pos, already_partitioned)
}

/// `__partition_with_equals_on_left`: elements equal to the pivot go left; returns where
/// the greater elements start.
fn partition_with_equals_on_left<T: Clone>(
    v: &mut [T],
    begin: usize,
    end: usize,
    c: &mut impl FnMut(&T, &T) -> bool,
) -> usize {
    let pivot = v[begin].clone();
    let mut first = begin;
    let mut last = end;
    if c(&pivot, &v[last - 1]) {
        loop {
            first += 1;
            if c(&pivot, &v[first]) {
                break;
            }
        }
    } else {
        loop {
            first += 1;
            if !(first < last && !c(&pivot, &v[first])) {
                break;
            }
        }
    }
    if first < last {
        loop {
            last -= 1;
            if !c(&pivot, &v[last]) {
                break;
            }
        }
    }
    while first < last {
        v.swap(first, last);
        loop {
            first += 1;
            if c(&pivot, &v[first]) {
                break;
            }
        }
        loop {
            last -= 1;
            if !c(&pivot, &v[last]) {
                break;
            }
        }
    }
    let pivot_pos = first - 1;
    if begin != pivot_pos {
        v[begin] = v[pivot_pos].clone();
    }
    v[pivot_pos] = pivot;
    first
}

/// `__sift_down`.
fn sift_down<T: Clone>(
    v: &mut [T],
    first: usize,
    c: &mut impl FnMut(&T, &T) -> bool,
    len: usize,
    mut start: usize,
) {
    let mut child = start;
    if len < 2 || (len - 2) / 2 < child {
        return;
    }
    child = 2 * child + 1;
    if child + 1 < len && c(&v[first + child], &v[first + child + 1]) {
        child += 1;
    }
    if c(&v[first + child], &v[first + start]) {
        return;
    }
    let top = v[first + start].clone();
    loop {
        v[first + start] = v[first + child].clone();
        start = child;
        if (len - 2) / 2 < child {
            break;
        }
        child = 2 * child + 1;
        if child + 1 < len && c(&v[first + child], &v[first + child + 1]) {
            child += 1;
        }
        if c(&v[first + child], &top) {
            break;
        }
    }
    v[first + start] = top;
}

/// `__floyd_sift_down`: moves the hole at `first` down to a leaf; returns its position.
fn floyd_sift_down<T: Clone>(
    v: &mut [T],
    first: usize,
    c: &mut impl FnMut(&T, &T) -> bool,
    len: usize,
) -> usize {
    let mut hole = first;
    let mut child_i = first;
    let mut child = 0;
    loop {
        child_i += child + 1;
        child = 2 * child + 1;
        if child + 1 < len && c(&v[child_i], &v[child_i + 1]) {
            child_i += 1;
            child += 1;
        }
        v[hole] = v[child_i].clone();
        hole = child_i;
        if child > (len - 2) / 2 {
            return hole;
        }
    }
}

/// `__sift_up`.
fn sift_up<T: Clone>(
    v: &mut [T],
    first: usize,
    mut last: usize,
    c: &mut impl FnMut(&T, &T) -> bool,
    mut len: usize,
) {
    if len > 1 {
        len = (len - 2) / 2;
        let mut ptr = first + len;
        last -= 1;
        if c(&v[ptr], &v[last]) {
            let t = v[last].clone();
            loop {
                v[last] = v[ptr].clone();
                last = ptr;
                if len == 0 {
                    break;
                }
                len = (len - 1) / 2;
                ptr = first + len;
                if !c(&v[ptr], &t) {
                    break;
                }
            }
            v[last] = t;
        }
    }
}

/// `__partial_sort(first, last, last)`: heap sort.
fn heap_sort<T: Clone>(v: &mut [T], first: usize, last: usize, c: &mut impl FnMut(&T, &T) -> bool) {
    // `__make_heap`.
    let n = last - first;
    if n > 1 {
        for start in (0..=(n - 2) / 2).rev() {
            sift_down(v, first, c, n, start);
        }
    }
    // `__sort_heap` with `__pop_heap`.
    let mut end = last;
    let mut len = n;
    while len > 1 {
        let top = v[first].clone();
        let mut hole = floyd_sift_down(v, first, c, len);
        end -= 1;
        if hole == end {
            v[hole] = top;
        } else {
            v[hole] = v[end].clone();
            hole += 1;
            v[end] = top;
            sift_up(v, first, hole, c, hole - first);
        }
        len -= 1;
    }
}

/// `__introsort`.
fn introsort<T: Clone>(
    v: &mut [T],
    mut first: usize,
    mut last: usize,
    c: &mut impl FnMut(&T, &T) -> bool,
    mut depth: usize,
    mut leftmost: bool,
) {
    const LIMIT: usize = 24;
    const NINTHER_THRESHOLD: usize = 128;
    loop {
        let len = last - first;
        match len {
            0 | 1 => return,
            2 => {
                if c(&v[last - 1], &v[first]) {
                    v.swap(first, last - 1);
                }
                return;
            }
            3 => {
                sort3(v, first, first + 1, last - 1, c);
                return;
            }
            4 => {
                sort4(v, first, first + 1, first + 2, last - 1, c);
                return;
            }
            5 => {
                sort5(v, [first, first + 1, first + 2, first + 3, last - 1], c);
                return;
            }
            _ => {}
        }
        if len < LIMIT {
            if leftmost {
                insertion_sort(v, first, last, c);
            } else {
                insertion_sort_unguarded(v, first, last, c);
            }
            return;
        }
        if depth == 0 {
            heap_sort(v, first, last, c);
            return;
        }
        depth -= 1;
        let half_len = len / 2;
        if len > NINTHER_THRESHOLD {
            sort3(v, first, first + half_len, last - 1, c);
            sort3(v, first + 1, first + (half_len - 1), last - 2, c);
            sort3(v, first + 2, first + (half_len + 1), last - 3, c);
            sort3(
                v,
                first + (half_len - 1),
                first + half_len,
                first + (half_len + 1),
                c,
            );
            v.swap(first, first + half_len);
        } else {
            sort3(v, first + half_len, first, last - 1, c);
        }
        if !leftmost && !c(&v[first - 1], &v[first]) {
            first = partition_with_equals_on_left(v, first, last, c);
            continue;
        }
        let (mut i, already_partitioned) = partition_with_equals_on_right(v, first, last, c);
        if already_partitioned {
            let fs = insertion_sort_incomplete(v, first, i, c);
            if insertion_sort_incomplete(v, i + 1, last, c) {
                if fs {
                    return;
                }
                last = i;
                continue;
            } else if fs {
                i += 1;
                first = i;
                continue;
            }
        }
        introsort(v, first, i, c, depth, leftmost);
        leftmost = false;
        first = i + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts() {
        for n in [0usize, 1, 2, 3, 5, 10, 23, 24, 30, 129, 300, 1000] {
            let mut v: Vec<u64> = (0..n as u64).map(|i| (i * 7919 + 13) % 101).collect();
            let mut expected = v.clone();
            expected.sort_unstable();
            sort(&mut v, &mut |a, b| a < b);
            assert_eq!(v, expected);
        }
    }

    #[test]
    fn matches_libcxx() {
        // Sorting (key, index) pairs with many equal keys by key; the FNV-1a hash of the
        // indices in the order libc++ 21's std::sort leaves them (one line per input).
        let mut state: u64 = 12345;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u32
        };
        let mut output = String::new();
        for n in [6, 7, 23, 24, 25, 50, 128, 129, 200, 256, 1000, 3000] {
            for keys in [2, 5, 50] {
                let mut v: Vec<(u32, u32)> = (0..n).map(|i| (next() % keys, i)).collect();
                sort(&mut v, &mut |a, b| a.0 < b.0);
                for (_, index) in v {
                    output += &format!("{index} ");
                }
                output.push('\n');
            }
        }
        let hash = output.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
        assert_eq!((hash, output.len()), (0x4664_1f72_7e6f_a2e0, 61668));
    }

    #[test]
    fn equal_elements_order() {
        // Keys with ties: libc++ orders the ties of this input as below.
        let mut v: Vec<(u32, u32)> = (0..8).map(|i| (i % 2, i)).collect();
        sort(&mut v, &mut |a, b| a.0 < b.0);
        assert_eq!(
            v.iter().map(|e| e.1).collect::<Vec<_>>(),
            [0, 2, 4, 6, 1, 3, 5, 7]
        );
    }
}
