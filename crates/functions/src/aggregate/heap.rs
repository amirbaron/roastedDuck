//! Bounded heaps for the top-N aggregates (`min(x, n)`, `arg_max(x, y, n)`, ...; DuckDB's
//! `UnaryAggregateHeap`/`BinaryAggregateHeap` in `minmax_n_helpers.hpp`).
//!
//! DuckDB builds them on `std::push_heap`/`pop_heap`/`sort_heap`; the order of entries with
//! equal keys in the final result depends on those algorithms, so they are ported from
//! libc++ (the reference build's standard library) rather than replaced by `BinaryHeap`.

use std::cmp::Ordering;

/// The largest `n` accepted by the top-N aggregates (`MIN_MAX_N_MAX_VALUE`).
pub(crate) const MIN_MAX_N_MAX_VALUE: i64 = 1_000_000;

/// libc++'s `__sift_up` over `heap[..len]`: restores the heap after appending the last
/// element. `less(a, b)` is the heap's comparator.
fn sift_up<T>(heap: &mut [T], len: usize, less: &impl Fn(&T, &T) -> bool) {
    if len <= 1 {
        return;
    }
    let mut parent = (len - 2) / 2;
    let mut last = len - 1;
    if !less(&heap[parent], &heap[last]) {
        return;
    }
    loop {
        heap.swap(last, parent);
        last = parent;
        if parent == 0 {
            break;
        }
        parent = (parent - 1) / 2;
        if !less(&heap[parent], &heap[last]) {
            break;
        }
    }
}

/// libc++'s `__floyd_sift_down`: moves the hole at the root down to a leaf, always
/// following the larger child; returns the final hole position.
fn floyd_sift_down<T>(heap: &mut [T], len: usize, less: &impl Fn(&T, &T) -> bool) -> usize {
    let mut hole = 0;
    let mut child = 0;
    loop {
        let mut child_index = 2 * child + 1;
        child = child_index;
        if child + 1 < len && less(&heap[child_index], &heap[child_index + 1]) {
            child_index += 1;
            child += 1;
        }
        heap.swap(hole, child_index);
        hole = child_index;
        if child > (len - 2) / 2 {
            return hole;
        }
    }
}

/// libc++'s `__pop_heap` over `heap[..len]`: moves the root to `heap[len - 1]`.
fn pop_heap<T>(heap: &mut [T], len: usize, less: &impl Fn(&T, &T) -> bool) {
    if len <= 1 {
        return;
    }
    // The root travels down as the "hole"; it ends at `hole` and is swapped with the last.
    let hole = floyd_sift_down(heap, len, less);
    let last = len - 1;
    if hole == last {
        return;
    }
    heap.swap(hole, last);
    sift_up(heap, hole + 1, less);
}

/// Heaps up to this size allocate exactly their size up front.
const SMALL_HEAP: usize = 16;

/// A heap keeping the `capacity` best entries by `less` (the best are the smallest).
#[derive(Clone, Debug)]
pub(crate) struct TopN<T> {
    capacity: usize,
    entries: Vec<T>,
}

impl<T> TopN<T> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Vec::new(),
        }
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn entries(&self) -> &[T] {
        &self.entries
    }

    /// Offers an entry; `make` builds it only when it is kept, `replace` overwrites the
    /// evicted entry in place (reusing its allocations).
    pub(crate) fn insert_with(
        &mut self,
        less: &impl Fn(&T, &T) -> bool,
        is_better_than_top: impl FnOnce(&T) -> bool,
        make: impl FnOnce() -> T,
        replace: impl FnOnce(&mut T),
    ) {
        if self.entries.len() < self.capacity {
            if self.entries.capacity() == 0 {
                // Small heaps (e.g. `arg_max(x, y, 2)` per group) allocate just their
                // size rather than `Vec`'s first growth step.
                self.entries.reserve_exact(self.capacity.min(SMALL_HEAP));
            }
            self.entries.push(make());
            let len = self.entries.len();
            sift_up(&mut self.entries, len, less);
        } else if self.entries.first().is_some_and(is_better_than_top) {
            let len = self.entries.len();
            pop_heap(&mut self.entries, len, less);
            replace(&mut self.entries[len - 1]);
            sift_up(&mut self.entries, len, less);
        }
    }

    /// The entries in ascending order of `less` (libc++'s `sort_heap`).
    pub(crate) fn sorted(&mut self, less: &impl Fn(&T, &T) -> bool) -> &[T] {
        let mut len = self.entries.len();
        while len > 1 {
            pop_heap(&mut self.entries, len, less);
            len -= 1;
        }
        &self.entries
    }
}

/// Orders by `cmp`, as the heap comparator `LessThan` (or `GreaterThan` if `greater`).
pub(crate) fn comparator<T>(greater: bool, cmp: impl Fn(&T, &T) -> Ordering) -> impl Fn(&T, &T) -> bool {
    move |a, b| {
        let ordering = cmp(a, b);
        if greater {
            ordering == Ordering::Greater
        } else {
            ordering == Ordering::Less
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_smallest_entries_in_order() {
        let less = |a: &i32, b: &i32| a < b;
        let mut heap = TopN::new(3);
        for value in [5, 1, 9, 3, 7, 2, 8] {
            heap.insert_with(&less, |top| value < *top, || value, |slot| *slot = value);
        }
        assert_eq!(heap.sorted(&less), &[1, 2, 3]);
    }
}
