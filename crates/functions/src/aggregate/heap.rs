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

/// Heaps up to this size (e.g. `arg_max(x, y, 2)` per group) keep their entries in the
/// state itself: a hash aggregate has one heap per group, and allocating (and, on
/// another thread, freeing) each of them dominated such queries.
const INLINE_HEAP: usize = 2;

/// The entries of a [`TopN`].
#[derive(Clone, Debug)]
enum Entries<T> {
    /// The first `len` slots are entries.
    Inline { len: usize, slots: [T; INLINE_HEAP] },
    Allocated(Vec<T>),
}

impl<T: Default> Entries<T> {
    fn as_slice(&self) -> &[T] {
        match self {
            Self::Inline { len, slots } => &slots[..*len],
            Self::Allocated(entries) => entries,
        }
    }

    fn as_mut_slice(&mut self) -> &mut [T] {
        match self {
            Self::Inline { len, slots } => &mut slots[..*len],
            Self::Allocated(entries) => entries,
        }
    }

    /// Appends an entry; a heap of at most `capacity` entries has room for it.
    fn push(&mut self, entry: T, capacity: usize) {
        match self {
            Self::Inline { len, slots } => {
                slots[*len] = entry;
                *len += 1;
            }
            Self::Allocated(entries) => {
                if entries.capacity() == 0 {
                    // Small heaps allocate just their size rather than `Vec`'s first
                    // growth step.
                    entries.reserve_exact(capacity.min(SMALL_HEAP));
                }
                entries.push(entry);
            }
        }
    }
}

/// A heap keeping the `capacity` best entries by `less` (the best are the smallest).
#[derive(Clone, Debug)]
pub(crate) struct TopN<T> {
    capacity: usize,
    entries: Entries<T>,
}

impl<T: Default> TopN<T> {
    pub(crate) fn new(capacity: usize) -> Self {
        let entries = if capacity <= INLINE_HEAP {
            Entries::Inline {
                len: 0,
                slots: std::array::from_fn(|_| T::default()),
            }
        } else {
            Entries::Allocated(Vec::new())
        };
        Self { capacity, entries }
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.as_slice().is_empty()
    }

    pub(crate) fn entries(&self) -> &[T] {
        self.entries.as_slice()
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
        let len = self.entries.as_slice().len();
        if len < self.capacity {
            self.entries.push(make(), self.capacity);
            sift_up(self.entries.as_mut_slice(), len + 1, less);
        } else if self.entries.as_slice().first().is_some_and(is_better_than_top) {
            let entries = self.entries.as_mut_slice();
            pop_heap(entries, len, less);
            replace(&mut entries[len - 1]);
            sift_up(entries, len, less);
        }
    }

    /// The entries in ascending order of `less` (libc++'s `sort_heap`).
    pub(crate) fn sorted(&mut self, less: &impl Fn(&T, &T) -> bool) -> &[T] {
        let entries = self.entries.as_mut_slice();
        let mut len = entries.len();
        while len > 1 {
            pop_heap(entries, len, less);
            len -= 1;
        }
        entries
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
