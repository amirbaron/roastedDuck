//! Bit masks over the rows of a hash group (DuckDB uses `ValidityMask` for these): the
//! partition and peer boundary masks, and the NULL masks used by `IGNORE NULLS`.

use rustdb_types::ValidityMask;

/// Bits per mask entry (`ValidityMask::BITS_PER_VALUE`).
pub(crate) const BITS_PER_ENTRY: usize = 64;

/// A row bit mask; a set bit means "valid" (a boundary, or a non-NULL value).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowMask {
    /// `None` when every row is valid.
    words: Option<Vec<u64>>,
}

impl WindowMask {
    /// A mask with every row valid.
    pub(crate) const fn all_valid() -> Self {
        Self { words: None }
    }

    /// A mask of `count` rows, all invalid.
    pub(crate) fn all_invalid(count: usize) -> Self {
        Self {
            words: Some(vec![0; count.div_ceil(BITS_PER_ENTRY)]),
        }
    }

    /// A mask with row `i` valid where `flags[i]` is set.
    pub(crate) fn from_flags(flags: &[bool]) -> Self {
        let words = flags
            .chunks(BITS_PER_ENTRY)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .fold(0u64, |word, (bit, &flag)| word | (u64::from(flag) << bit))
            })
            .collect();
        Self { words: Some(words) }
    }

    /// The mask of the first `count` rows of a vector's validity.
    pub(crate) fn from_validity(validity: &ValidityMask, count: usize) -> Self {
        let Some(words) = validity.words() else {
            return Self::all_valid();
        };
        let needed = count.div_ceil(BITS_PER_ENTRY);
        let mut owned = words[..needed.min(words.len())].to_vec();
        owned.resize(needed, u64::MAX);
        Self { words: Some(owned) }
    }

    pub(crate) fn is_all_valid(&self) -> bool {
        self.words.is_none()
    }

    #[inline]
    pub(crate) fn is_valid(&self, row: usize) -> bool {
        self.words.as_ref().is_none_or(|words| {
            words
                .get(row / BITS_PER_ENTRY)
                .is_none_or(|word| word >> (row % BITS_PER_ENTRY) & 1 == 1)
        })
    }

    /// Marks `row` valid (no-op on an all-valid mask).
    #[inline]
    pub(crate) fn set_valid(&mut self, row: usize) {
        if let Some(words) = &mut self.words {
            words[row / BITS_PER_ENTRY] |= 1 << (row % BITS_PER_ENTRY);
        }
    }

    /// The 64 bits of entry `index` (all set past the end or on an all-valid mask).
    #[inline]
    pub(crate) fn entry(&self, index: usize) -> u64 {
        self.words
            .as_ref()
            .and_then(|words| words.get(index).copied())
            .unwrap_or(u64::MAX)
    }

    /// Number of valid rows in `[begin, end)`.
    pub(crate) fn count_valid(&self, begin: usize, end: usize) -> usize {
        if begin >= end {
            return 0;
        }
        let Some(words) = &self.words else {
            return end - begin;
        };
        let (first, last) = (begin / BITS_PER_ENTRY, (end - 1) / BITS_PER_ENTRY);
        let mut count = 0;
        for index in first..=last {
            let mut word = words.get(index).copied().unwrap_or(u64::MAX);
            if index == first {
                word &= u64::MAX << (begin % BITS_PER_ENTRY);
            }
            if index == last && !end.is_multiple_of(BITS_PER_ENTRY) {
                word &= u64::MAX >> (BITS_PER_ENTRY - end % BITS_PER_ENTRY);
            }
            count += word.count_ones() as usize;
        }
        count
    }
}

/// Moves forward from `l` to find the `n`th valid row before `r`; returns `r` if there are
/// not enough. `n` is decremented by the valid rows passed (DuckDB's
/// `WindowBoundariesState::FindNextStart`).
pub(crate) fn find_next_start(mask: &WindowMask, mut l: usize, r: usize, n: &mut usize) -> usize {
    if mask.is_all_valid() {
        // Unsigned wrap-around as in DuckDB: an inverted range (`l > r`) consumes `n`.
        let start = (l + *n).wrapping_sub(1).min(r);
        *n -= (*n).min(r.wrapping_sub(l));
        return start;
    }
    while l < r {
        let (entry, mut shift) = (l / BITS_PER_ENTRY, l % BITS_PER_ENTRY);
        let block = mask.entry(entry);
        // Skip a blank, aligned block in one step.
        if block == 0 && shift == 0 {
            l += BITS_PER_ENTRY;
            continue;
        }
        while shift < BITS_PER_ENTRY && l < r {
            if block >> shift & 1 == 1 {
                *n -= 1;
                if *n == 0 {
                    return l.min(r);
                }
            }
            shift += 1;
            l += 1;
        }
    }
    r
}

/// Moves backward from `r` to find the `n`th valid row at or after `l`; returns `l` if
/// there are not enough (DuckDB's `WindowBoundariesState::FindPrevStart`).
pub(crate) fn find_prev_start(mask: &WindowMask, l: usize, mut r: usize, n: &mut usize) -> usize {
    if mask.is_all_valid() {
        let start = if r <= l + *n { l } else { r - *n };
        *n = n.wrapping_sub(r.wrapping_sub(start));
        return start;
    }
    while l < r {
        let (entry, shift) = ((r - 1) / BITS_PER_ENTRY, (r - 1) % BITS_PER_ENTRY);
        let block = mask.entry(entry);
        // Skip a blank block that ends just before `r`.
        if block == 0 && shift + 1 == BITS_PER_ENTRY {
            r -= BITS_PER_ENTRY;
            continue;
        }
        // Probe r - 1 down to the start of the block.
        let mut shift = shift + 1;
        while shift > 0 && l < r {
            shift -= 1;
            if block >> shift & 1 == 1 {
                *n -= 1;
                if *n == 0 {
                    return l.max(r - 1);
                }
            }
            r -= 1;
        }
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask_with(count: usize, valid: &[usize]) -> WindowMask {
        let mut mask = WindowMask::all_invalid(count);
        for &row in valid {
            mask.set_valid(row);
        }
        mask
    }

    #[test]
    fn next_and_prev_start_skip_blank_blocks() {
        let mask = mask_with(300, &[0, 5, 200, 299]);
        let mut n = 1;
        assert_eq!(find_next_start(&mask, 1, 300, &mut n), 5);
        assert_eq!(n, 0);
        let mut n = 2;
        assert_eq!(find_next_start(&mask, 1, 300, &mut n), 200);
        let mut n = 5;
        assert_eq!(find_next_start(&mask, 1, 300, &mut n), 300);
        assert_eq!(n, 2);
        let mut n = 1;
        assert_eq!(find_prev_start(&mask, 0, 199, &mut n), 5);
        let mut n = 2;
        assert_eq!(find_prev_start(&mask, 0, 300, &mut n), 200);
        let mut n = 9;
        assert_eq!(find_prev_start(&mask, 1, 300, &mut n), 1);
    }

    #[test]
    fn all_valid_masks_count_arithmetically() {
        let mask = WindowMask::all_valid();
        let mut n = 3;
        assert_eq!(find_next_start(&mask, 10, 100, &mut n), 12);
        assert_eq!(n, 0);
        let mut n = 3;
        assert_eq!(find_prev_start(&mask, 10, 100, &mut n), 97);
        assert_eq!(n, 0);
        let mut n = 30;
        assert_eq!(find_prev_start(&mask, 10, 20, &mut n), 10);
        assert_eq!(n, 20);
    }

    #[test]
    fn count_valid_handles_ragged_ranges() {
        let mask = mask_with(200, &[1, 63, 64, 65, 130, 199]);
        assert_eq!(mask.count_valid(0, 200), 6);
        assert_eq!(mask.count_valid(2, 65), 2);
        assert_eq!(mask.count_valid(64, 64), 0);
        assert_eq!(mask.count_valid(66, 199), 1);
    }
}
