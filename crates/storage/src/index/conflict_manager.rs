//! Collects the outcome of constraint lookups (DuckDB's `ConflictManager`).
//!
//! In [`ConflictManagerMode::Throw`] the first hit is a violation. In
//! [`ConflictManagerMode::Scan`] hits are recorded per input row (with the row id they hit),
//! which foreign key checks and `ON CONFLICT` use; rows recorded during a scan are then
//! tolerated by a later throw-mode lookup.

use std::collections::HashMap;

use rustdb_common::ByteString;

use super::VerifyExistenceType;

/// Whether hits are violations or recorded (`ConflictManagerMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictManagerMode {
    Scan,
    Throw,
}

/// Recorded constraint hits of one chunk.
#[derive(Debug)]
pub struct ConflictManager {
    verify_type: VerifyExistenceType,
    chunk_size: usize,
    mode: ConflictManagerMode,
    /// Whether the conflict target names a single index (`ConflictInfo::SingleIndexTarget`).
    single_index_target: bool,
    finished: bool,
    /// Input rows in the order their first hit was recorded.
    order: Vec<usize>,
    /// The first and (for duplicate leaves) second row id hit by each input row.
    hits: HashMap<usize, (i64, Option<i64>)>,
    /// Names of the indexes matching the conflict target.
    matching_indexes: Vec<ByteString>,
}

impl ConflictManager {
    /// A manager that throws on the first hit.
    pub fn new(verify_type: VerifyExistenceType, chunk_size: usize) -> Self {
        Self {
            verify_type,
            chunk_size,
            mode: ConflictManagerMode::Throw,
            single_index_target: false,
            finished: false,
            order: Vec::new(),
            hits: HashMap::new(),
            matching_indexes: Vec::new(),
        }
    }

    /// A manager that records hits (foreign key checks and `ON CONFLICT`).
    pub fn scanning(verify_type: VerifyExistenceType, chunk_size: usize, single_index_target: bool) -> Self {
        let mut manager = Self::new(verify_type, chunk_size);
        manager.mode = ConflictManagerMode::Scan;
        manager.single_index_target = single_index_target;
        manager
    }

    pub fn verify_type(&self) -> VerifyExistenceType {
        self.verify_type
    }

    pub fn mode(&self) -> ConflictManagerMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: ConflictManagerMode) {
        self.mode = mode;
    }

    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    /// Records that index `name` matches the conflict target (`AddIndex`).
    pub fn add_index(&mut self, name: impl AsRef<[u8]>) {
        self.matching_indexes.push(name.as_ref().into());
    }

    /// Whether index `name` matches the conflict target (`IndexMatches`).
    pub fn index_matches(&self, name: impl AsRef<[u8]>) -> bool {
        self.matching_indexes
            .iter()
            .any(|n| n.eq_ignore_ascii_case(name.as_ref()))
    }

    fn should_throw(&self, row: usize) -> bool {
        self.mode == ConflictManagerMode::Throw && !self.hits.contains_key(&row)
    }

    /// Whether NULL keys are never conflicts (`IgnoreNulls`).
    fn ignore_nulls(&self) -> bool {
        !matches!(self.verify_type, VerifyExistenceType::AppendFk)
    }

    /// Records that input row `row` hit `row_id`; true if this is a violation
    /// (`AddHit`).
    pub fn add_hit(&mut self, row: usize, row_id: i64) -> bool {
        if self.should_throw(row) {
            return true;
        }
        if self.mode == ConflictManagerMode::Throw || self.finished {
            return false;
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = self.hits.entry(row) {
            entry.insert((row_id, None));
            self.order.push(row);
        }
        false
    }

    /// Records the second row id of a duplicate leaf (`AddSecondHit`).
    pub fn add_second_hit(&mut self, row: usize, row_id: i64) -> bool {
        if self.should_throw(row) {
            return true;
        }
        if self.mode == ConflictManagerMode::Throw || self.finished {
            return false;
        }
        if let Some(hit) = self.hits.get_mut(&row) {
            hit.1 = Some(row_id);
        }
        false
    }

    /// Records a NULL key of input row `row`; true if this is a violation (`AddNull`).
    pub fn add_null(&mut self, row: usize) -> bool {
        if self.ignore_nulls() {
            return false;
        }
        self.add_hit(row, i64::MAX)
    }

    /// `FinishLookup`: a single-index conflict target stops after the first index.
    pub fn finish_lookup(&mut self) {
        if self.mode == ConflictManagerMode::Throw || !self.single_index_target {
            return;
        }
        self.finished = self.has_conflicts();
    }

    pub fn has_conflicts(&self) -> bool {
        !self.hits.is_empty()
    }

    pub fn conflict_count(&self) -> usize {
        self.hits.len()
    }

    /// Whether input row `row` hit something (`GetFirstValidity().RowIsValid`).
    pub fn is_hit(&self, row: usize) -> bool {
        self.hits.contains_key(&row)
    }

    /// The first input row whose hit was recorded (`GetFirstIndex`).
    pub fn first_index(&self) -> Option<usize> {
        self.order.first().copied()
    }

    /// The first row below `count` that was hit, or with `negate` the first that was not
    /// (`GetFirstInvalidIndex`).
    pub fn first_invalid_index(&self, count: usize, negate: bool) -> Option<usize> {
        (0..count).find(|&row| self.is_hit(row) != negate)
    }

    /// The recorded hits as `(input row, row id)` in recording order.
    pub fn conflicts(&self) -> Vec<(usize, i64)> {
        self.order
            .iter()
            .filter_map(|row| self.hits.get(row).map(|hit| (*row, hit.0)))
            .collect()
    }

    /// For duplicate leaves, keeps the second row id where `visible` says it is the one
    /// the transaction sees (`ConflictManager::Finalize`).
    pub fn finalize(&mut self, mut visible: impl FnMut(i64) -> bool) {
        for hit in self.hits.values_mut() {
            if let Some(second) = hit.1.take()
                && visible(second)
            {
                hit.0 = second;
            }
        }
    }
}
