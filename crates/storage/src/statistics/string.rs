//! Prefix min/max and length statistics of string columns (DuckDB's `StringStats`).

use std::cmp::Ordering;

use rustdb_types::compare::CompareOp;

use super::FilterPropagateResult;

/// Bytes of a string kept for min/max (`StringStatsData::MAX_STRING_MINMAX_SIZE`).
pub const MAX_STRING_MINMAX_SIZE: usize = 8;

/// String statistics: zero-padded 8-byte prefixes of the smallest and largest value,
/// whether any value contains non-ASCII bytes, and the longest value's length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StringStats {
    pub min: [u8; MAX_STRING_MINMAX_SIZE],
    pub max: [u8; MAX_STRING_MINMAX_SIZE],
    pub has_unicode: bool,
    pub has_max_string_length: bool,
    pub max_string_length: u32,
}

/// `target` is the value's first 8 bytes, zero padded (`ConstructValue`).
fn prefix(bytes: &[u8]) -> [u8; MAX_STRING_MINMAX_SIZE] {
    let mut target = [0; MAX_STRING_MINMAX_SIZE];
    let len = bytes.len().min(MAX_STRING_MINMAX_SIZE);
    target[..len].copy_from_slice(&bytes[..len]);
    target
}

impl StringStats {
    /// Statistics of no values (`StringStats::CreateEmpty`).
    pub const fn empty() -> Self {
        Self {
            min: [0xFF; MAX_STRING_MINMAX_SIZE],
            max: [0; MAX_STRING_MINMAX_SIZE],
            has_unicode: false,
            has_max_string_length: true,
            max_string_length: 0,
        }
    }

    /// Statistics that say nothing about the values (`StringStats::CreateUnknown`).
    pub const fn unknown() -> Self {
        Self {
            min: [0; MAX_STRING_MINMAX_SIZE],
            max: [0xFF; MAX_STRING_MINMAX_SIZE],
            has_unicode: true,
            has_max_string_length: false,
            max_string_length: 0,
        }
    }

    /// Accounts for one value. `check_unicode` is set for VARCHAR (not BLOB) columns.
    #[inline]
    pub fn update(&mut self, bytes: &[u8], check_unicode: bool) {
        let target = prefix(bytes);
        if target < self.min {
            self.min = target;
        }
        if target > self.max {
            self.max = target;
        }
        if bytes.len() > self.max_string_length as usize {
            self.max_string_length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        }
        if check_unicode && !self.has_unicode && !bytes.is_ascii() {
            self.has_unicode = true;
        }
    }

    pub fn merge(&mut self, other: &Self) {
        if other.min < self.min {
            self.min = other.min;
        }
        if other.max > self.max {
            self.max = other.max;
        }
        self.has_unicode |= other.has_unicode;
        self.has_max_string_length &= other.has_max_string_length;
        self.max_string_length = self.max_string_length.max(other.max_string_length);
    }

    /// The min prefix up to its first zero byte (`StringStats::Min`).
    pub fn min_bytes(&self) -> &[u8] {
        trim_zeros(&self.min)
    }

    /// The max prefix up to its first zero byte (`StringStats::Max`).
    pub fn max_bytes(&self) -> &[u8] {
        trim_zeros(&self.max)
    }

    /// Whether `x <op> constant` can hold (`StringStats::CheckZonemap`); several constants
    /// are OR-ed.
    pub fn check_zonemap(&self, op: CompareOp, constants: &[&[u8]]) -> FilterPropagateResult {
        for constant in constants {
            match check_prefixes(self.min, self.max, op, constant) {
                FilterPropagateResult::FilterAlwaysFalse => {}
                other => return other,
            }
        }
        FilterPropagateResult::FilterAlwaysFalse
    }
}

fn trim_zeros(data: &[u8]) -> &[u8] {
    let len = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    &data[..len]
}

/// Compares the first `min(len, 8)` bytes of `data` with `bound` (`StringValueComparison`).
fn compare_prefix(data: &[u8], bound: [u8; MAX_STRING_MINMAX_SIZE]) -> Ordering {
    let len = data.len().min(MAX_STRING_MINMAX_SIZE);
    data[..len].cmp(&bound[..len])
}

fn check_prefixes(
    min: [u8; MAX_STRING_MINMAX_SIZE],
    max: [u8; MAX_STRING_MINMAX_SIZE],
    op: CompareOp,
    constant: &[u8],
) -> FilterPropagateResult {
    use FilterPropagateResult::{FilterAlwaysFalse, FilterAlwaysTrue, NoPruningPossible};
    let min_cmp = compare_prefix(constant, min);
    let max_cmp = compare_prefix(constant, max);
    match op {
        CompareOp::Equal => {
            if min_cmp == Ordering::Less || max_cmp == Ordering::Greater {
                FilterAlwaysFalse
            } else {
                NoPruningPossible
            }
        }
        CompareOp::NotEqual => {
            if min_cmp == Ordering::Less || max_cmp == Ordering::Greater {
                FilterAlwaysTrue
            } else {
                NoPruningPossible
            }
        }
        CompareOp::GreaterThanOrEqual | CompareOp::GreaterThan => {
            if max_cmp == Ordering::Greater {
                FilterAlwaysFalse
            } else {
                NoPruningPossible
            }
        }
        CompareOp::LessThan | CompareOp::LessThanOrEqual => {
            if min_cmp == Ordering::Less {
                FilterAlwaysFalse
            } else {
                NoPruningPossible
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_and_lengths_are_tracked() {
        let mut stats = StringStats::empty();
        stats.update(b"hello world", true);
        stats.update(b"abc", true);
        assert_eq!(stats.min_bytes(), b"abc");
        assert_eq!(stats.max_bytes(), b"hello wo");
        assert_eq!(stats.max_string_length, 11);
        assert!(!stats.has_unicode);
        stats.update("ünï".as_bytes(), true);
        assert!(stats.has_unicode);
        assert_eq!(
            stats.check_zonemap(CompareOp::Equal, &[b"aaa"]),
            FilterPropagateResult::FilterAlwaysFalse
        );
        assert_eq!(
            stats.check_zonemap(CompareOp::Equal, &[b"b"]),
            FilterPropagateResult::NoPruningPossible
        );
    }
}
