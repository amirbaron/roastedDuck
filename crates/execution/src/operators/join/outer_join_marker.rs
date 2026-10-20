//! Tracks which rows of one join side found a partner (DuckDB's `OuterJoinMarker`).

use std::sync::atomic::{AtomicBool, Ordering};

use rustdb_types::{DataChunk, LogicalType, STANDARD_VECTOR_SIZE};

use super::{null_columns, slice_columns};

/// One flag per row of a join side, set when the row finds a match; disabled markers
/// ignore everything. Flags are atomic so probe threads can share a right-side marker.
#[derive(Debug, Default)]
pub struct OuterJoinMarker {
    enabled: bool,
    found_match: Vec<AtomicBool>,
}

impl OuterJoinMarker {
    /// A marker that is only active when `enabled` (an outer join on this side).
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            found_match: Vec::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Sizes the marker for `count` rows, all unmatched (`Initialize`).
    pub fn initialize(&mut self, count: usize) {
        if !self.enabled {
            return;
        }
        self.found_match = (0..count).map(|_| AtomicBool::new(false)).collect();
    }

    /// How many threads can scan the unmatched rows (`MaxThreads`).
    pub fn max_threads(&self) -> usize {
        self.found_match.len() / (STANDARD_VECTOR_SIZE * 10)
    }

    /// Marks every row unmatched (`Reset`).
    pub fn reset(&self) {
        for flag in &self.found_match {
            flag.store(false, Ordering::Relaxed);
        }
    }

    /// Marks row `position` matched (`SetMatch`).
    pub fn set_match(&self, position: usize) {
        if self.enabled {
            self.found_match[position].store(true, Ordering::Relaxed);
        }
    }

    /// Marks rows `base + sel[i]` matched (`SetMatches`).
    pub fn set_matches(&self, sel: &[u32], base: usize) {
        if !self.enabled {
            return;
        }
        for &index in sel {
            self.found_match[base + index as usize].store(true, Ordering::Relaxed);
        }
    }

    /// Whether row `position` found a match.
    pub fn found(&self, position: usize) -> bool {
        self.found_match[position].load(Ordering::Relaxed)
    }

    /// Number of rows tracked.
    pub fn count(&self) -> usize {
        self.found_match.len()
    }

    /// The rows of `left` that found no match, with NULL right columns of
    /// `right_types` (`ConstructLeftJoinResult`). Leaves `result` empty if there are none.
    pub fn construct_left_join_result(
        &self,
        left: &DataChunk,
        right_types: &[LogicalType],
        result: &mut DataChunk,
    ) {
        if !self.enabled {
            return;
        }
        let remaining: Vec<u32> = (0..left.len() as u32)
            .filter(|&row| !self.found(row as usize))
            .collect();
        if remaining.is_empty() {
            return;
        }
        let len = remaining.len();
        let mut columns = slice_columns(left, &remaining);
        columns.extend(null_columns(right_types));
        result.set_columns(columns, len);
    }

    /// The unmatched rows of `chunk`, which holds rows `base..base + chunk.len()` of the
    /// marked side, after NULL columns of `left_types` (one step of `OuterJoinMarker::Scan`).
    /// Leaves `result` empty if all rows matched.
    pub fn construct_right_join_result(
        &self,
        chunk: &DataChunk,
        base: usize,
        left_types: &[LogicalType],
        result: &mut DataChunk,
    ) {
        let unmatched: Vec<u32> = (0..chunk.len() as u32)
            .filter(|&row| !self.found(base + row as usize))
            .collect();
        if unmatched.is_empty() {
            return;
        }
        let len = unmatched.len();
        let mut columns = null_columns(left_types);
        columns.extend(slice_columns(chunk, &unmatched));
        result.set_columns(columns, len);
    }
}
