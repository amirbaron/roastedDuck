//! Random access to a sequence of sort keys, so that the sort algorithms run both on a
//! contiguous slice and in place over the blocks of a sorted run (DuckDB's
//! `block_iterator_t`).

/// A mutable sequence of `Copy` keys addressed by index.
pub(crate) trait KeySeq<K: Copy> {
    fn len(&self) -> usize;
    fn get(&self, index: usize) -> K;
    fn set(&mut self, index: usize, key: K);
    #[inline]
    fn swap(&mut self, a: usize, b: usize) {
        let key = self.get(a);
        self.set(a, self.get(b));
        self.set(b, key);
    }
}

impl<K: Copy> KeySeq<K> for [K] {
    #[inline]
    fn len(&self) -> usize {
        <[K]>::len(self)
    }
    #[inline]
    fn get(&self, index: usize) -> K {
        self[index]
    }
    #[inline]
    fn set(&mut self, index: usize, key: K) {
        self[index] = key;
    }
    #[inline]
    fn swap(&mut self, a: usize, b: usize) {
        <[K]>::swap(self, a, b);
    }
}

/// `std::reverse` of `keys[first..last]`.
pub(crate) fn reverse<K: Copy, S: KeySeq<K> + ?Sized>(keys: &mut S, first: usize, last: usize) {
    let (mut lo, mut hi) = (first, last);
    while lo + 1 < hi {
        hi -= 1;
        keys.swap(lo, hi);
        lo += 1;
    }
}

/// `std::inplace_merge`: stably merges the sorted ranges `first..middle` and
/// `middle..last` (equal keys keep the left range first). A stable merge has exactly one
/// result, so any implementation matches libc++'s.
pub(crate) fn inplace_merge<K: Copy, S: KeySeq<K> + ?Sized>(
    keys: &mut S,
    first: usize,
    middle: usize,
    last: usize,
    less: &impl Fn(&K, &K) -> bool,
) {
    if first == middle || middle == last {
        return;
    }
    // Already in order: nothing moves.
    if !less(&keys.get(middle), &keys.get(middle - 1)) {
        return;
    }
    let left: Vec<K> = (first..middle).map(|index| keys.get(index)).collect();
    let (mut l, mut r, mut out) = (0, middle, first);
    while l < left.len() && r < last {
        let right = keys.get(r);
        if less(&right, &left[l]) {
            keys.set(out, right);
            r += 1;
        } else {
            keys.set(out, left[l]);
            l += 1;
        }
        out += 1;
    }
    while l < left.len() {
        keys.set(out, left[l]);
        l += 1;
        out += 1;
    }
}

/// `floor(log2(n))` for `n > 0` (`detail::log2`).
pub(crate) fn log2(mut n: usize) -> usize {
    let mut log = 0;
    while {
        n >>= 1;
        n != 0
    } {
        log += 1;
    }
    log
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_stably() {
        let mut keys = [(1, 'a'), (3, 'a'), (5, 'a'), (1, 'b'), (3, 'b'), (4, 'b')];
        inplace_merge(
            keys.as_mut_slice(),
            0,
            3,
            6,
            &|a: &(i32, char), b: &(i32, char)| a.0 < b.0,
        );
        assert_eq!(keys, [(1, 'a'), (1, 'b'), (3, 'a'), (3, 'b'), (4, 'b'), (5, 'a')]);
        let mut keys = [1, 2, 3, 4];
        reverse(keys.as_mut_slice(), 0, 4);
        assert_eq!(keys, [4, 3, 2, 1]);
        assert_eq!(
            (log2(1), log2(2), log2(3), log2(1024), log2(1025)),
            (0, 1, 1, 10, 10)
        );
    }
}
