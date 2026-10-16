//! The perfect hash join (DuckDB's `PerfectHashJoinExecutor`,
//! `perfect_hash_join_executor.cpp`): when the build keys of an INNER join on one integer
//! equality are unique and span a small range, the build columns are laid out in arrays
//! indexed by `key - min` and probes look their rows up directly instead of walking the
//! hash table.
//!
//! DuckDB decides with the build key range from its statistics (or the min/max its join
//! filter pushdown aggregates while sinking). rustdb has neither, so the hash join tracks
//! the exact min/max of the build keys while sinking ([`KeyRange`]) and treats them as the
//! statistics; duplicate keys still fall back to the hash table, as in DuckDB.

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_common::{Error, Result};
use rustdb_row::RowPtr;
use rustdb_types::{DataChunk, LogicalType, NativeType, PhysicalType, SelectionVector, ValidityMask, Vector};

use crate::join_hashtable::JoinHashTable;

/// The largest build key range a perfect hash table is built for (`MAX_BUILD_SIZE`).
const MAX_BUILD_SIZE: u128 = 1_048_576;

/// The integer key types of perfect hash joins (DuckDB's `TypeIsInteger`).
trait PerfectKey: NativeType + PartialOrd {
    /// The key as a 128-bit signed integer, or `None` if it does not fit
    /// (`ExtractNumericValue`).
    fn to_i128(self) -> Option<i128>;
}

macro_rules! perfect_keys {
    ($($ty:ty),*) => {
        $(
            impl PerfectKey for $ty {
                #[inline]
                fn to_i128(self) -> Option<i128> {
                    i128::try_from(self).ok()
                }
            }
        )*
    };
}

perfect_keys!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128);

/// Calls `$body` with `$ty` bound to the native type of the integer physical type
/// `$physical`, or evaluates `$otherwise` for other types.
macro_rules! dispatch_integer {
    ($physical:expr, $ty:ident => $body:expr, $otherwise:expr) => {
        match $physical {
            PhysicalType::Int8 => {
                type $ty = i8;
                $body
            }
            PhysicalType::Int16 => {
                type $ty = i16;
                $body
            }
            PhysicalType::Int32 => {
                type $ty = i32;
                $body
            }
            PhysicalType::Int64 => {
                type $ty = i64;
                $body
            }
            PhysicalType::Int128 => {
                type $ty = i128;
                $body
            }
            PhysicalType::UInt8 => {
                type $ty = u8;
                $body
            }
            PhysicalType::UInt16 => {
                type $ty = u16;
                $body
            }
            PhysicalType::UInt32 => {
                type $ty = u32;
                $body
            }
            PhysicalType::UInt64 => {
                type $ty = u64;
                $body
            }
            PhysicalType::UInt128 => {
                type $ty = u128;
                $body
            }
            _ => $otherwise,
        }
    };
}

/// Whether `physical` is an integer type (`TypeIsInteger`).
fn is_integer(physical: PhysicalType) -> bool {
    matches!(
        physical,
        PhysicalType::Int8
            | PhysicalType::Int16
            | PhysicalType::Int32
            | PhysicalType::Int64
            | PhysicalType::Int128
            | PhysicalType::UInt8
            | PhysicalType::UInt16
            | PhysicalType::UInt32
            | PhysicalType::UInt64
            | PhysicalType::UInt128
    )
}

/// Whether a join could use a perfect hash table, before looking at the data: an INNER
/// join on a single integer equality whose right input has no nested columns (the static
/// part of `CanDoPerfectHashJoin`).
pub(super) fn is_perfect_hash_join_candidate(
    join_type: JoinType,
    comparisons: &[ExpressionType],
    key_type: &LogicalType,
    right_types: &[LogicalType],
) -> bool {
    join_type == JoinType::Inner
        && comparisons == [ExpressionType::CompareEqual]
        && is_integer(key_type.physical_type())
        && !right_types.iter().any(|ty| {
            matches!(
                ty.physical_type(),
                PhysicalType::Struct | PhysicalType::List | PhysicalType::Array
            )
        })
}

/// The minimum and maximum of the non-NULL build keys seen so far (the min/max that
/// DuckDB's join filter pushdown computes while sinking).
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyRange {
    /// `(min, max)`, if any non-NULL key was seen.
    bounds: Option<(i128, i128)>,
    /// A key did not fit an `i128`: the range is unusable.
    overflow: bool,
}

impl KeyRange {
    /// Extends the range by the first `count` rows of `keys`.
    pub(super) fn update(&mut self, keys: &Vector, count: usize) {
        if self.overflow {
            return;
        }
        dispatch_integer!(keys.logical_type().physical_type(), T => self.update_typed::<T>(keys, count), self.overflow = true);
    }

    fn update_typed<T: PerfectKey>(&mut self, keys: &Vector, count: usize) {
        let values = keys.unified::<T>();
        let mut chunk_min: Option<T> = None;
        let mut chunk_max: Option<T> = None;
        for row in 0..count {
            let slot = values.slot(row);
            if !values.validity.is_valid(slot) {
                continue;
            }
            let value = values.values[slot];
            if chunk_min.is_none_or(|min| value < min) {
                chunk_min = Some(value);
            }
            if chunk_max.is_none_or(|max| value > max) {
                chunk_max = Some(value);
            }
        }
        let (Some(min), Some(max)) = (chunk_min, chunk_max) else {
            return;
        };
        match (min.to_i128(), max.to_i128()) {
            (Some(min), Some(max)) => self.merge(Self {
                bounds: Some((min, max)),
                overflow: false,
            }),
            _ => self.overflow = true,
        }
    }

    /// Extends the range by `other`.
    pub(super) fn merge(&mut self, other: Self) {
        self.overflow |= other.overflow;
        self.bounds = match (self.bounds, other.bounds) {
            (Some((min, max)), Some((other_min, other_max))) => {
                Some((min.min(other_min), max.max(other_max)))
            }
            (bounds, None) | (None, bounds) => bounds,
        };
    }
}

/// A perfect hash table built from a finalized join hash table
/// (`PerfectHashJoinExecutor`).
#[derive(Debug)]
pub struct PerfectHashJoinExecutor {
    /// The smallest build key (`build_min`).
    build_min: i128,
    /// `max - min` (`build_range`).
    build_range: usize,
    /// Every key of the range has a build row and no build key was NULL
    /// (`is_build_dense`).
    is_build_dense: bool,
    /// One vector per build output column, holding the row of key `min + i` at slot `i`
    /// (NULL where no row has that key).
    perfect_hash_table: Vec<Vector>,
    /// Which slots have a build row (`bitmap_build_idx`).
    bitmap_build_idx: ValidityMask,
}

/// The per-thread buffers of perfect hash probes (`PerfectHashJoinState`).
#[derive(Debug, Default)]
pub struct PerfectHashJoinState {
    build_sel_vec: Vec<u32>,
    probe_sel_vec: Vec<u32>,
}

impl PerfectHashJoinExecutor {
    /// Builds the perfect hash table of `ht`, whose build keys span `range`, if the range
    /// is small enough and the keys are unique (`CanDoPerfectHashJoin` +
    /// `BuildPerfectHashTable`). `None` means: use the regular hash table.
    pub(super) fn try_build(
        ht: &JoinHashTable,
        range: KeyRange,
        output_types: &[LogicalType],
    ) -> Result<Option<Self>> {
        if range.overflow {
            return Ok(None);
        }
        let Some((min, max)) = range.bounds else {
            return Ok(None);
        };
        let Some(build_range) = max.checked_sub(min) else {
            return Ok(None);
        };
        if build_range < 0 || build_range.unsigned_abs() > MAX_BUILD_SIZE {
            return Ok(None);
        }
        let build_range =
            usize::try_from(build_range).map_err(|_| Error::internal("perfect hash range overflow"))?;
        let build_size = build_range + 1;
        // More rows than slots: some key repeats, which `FullScanHashTable` would find.
        if ht.count() > build_size {
            return Ok(None);
        }
        let mut executor = Self {
            build_min: min,
            build_range,
            is_build_dense: false,
            perfect_hash_table: Vec::new(),
            bitmap_build_idx: ValidityMask::all_invalid(build_size),
        };
        if !executor.full_scan_hash_table(ht, output_types)? {
            return Ok(None);
        }
        Ok(Some(executor))
    }

    /// Places every build row at its key's slot, failing on a duplicate key
    /// (`FullScanHashTable`).
    fn full_scan_hash_table(&mut self, ht: &JoinHashTable, output_types: &[LogicalType]) -> Result<bool> {
        let key_type = ht.condition_types()[0].clone();
        let build_size = self.build_range + 1;
        let mut tuples: Vec<RowPtr> = Vec::with_capacity(ht.count());
        let mut sel_build: Vec<u32> = Vec::with_capacity(ht.count());
        let mut unique_keys = 0;
        let success = ht.scan_key_column(&key_type, |rows, keys| {
            dispatch_integer!(
                key_type.physical_type(),
                T => Ok(self.fill_selection_vector_build::<T>(rows, keys, &mut tuples, &mut sel_build, &mut unique_keys)),
                Err(Error::not_implemented("Type not supported for perfect hash join"))
            )
        })?;
        if !success {
            return Ok(false);
        }
        if unique_keys == build_size && !ht.has_null {
            self.is_build_dense = true;
            self.bitmap_build_idx = ValidityMask::all_valid();
        }
        // Gather the build output columns into their slots.
        let key_count = tuples.len();
        for (index, ty) in output_types.iter().enumerate() {
            let mut vector = Vector::flat(ty.clone(), build_size);
            // SAFETY: the rows were just scanned from the table, whose blocks stay pinned.
            unsafe { ht.gather_output_column(&tuples, key_count, index, &mut vector, &sel_build)? };
            // Empty slots are NULL, so that the emitted dictionaries make sense.
            if !self.is_build_dense {
                let validity = vector.validity_mut();
                for slot in (0..build_size).filter(|&slot| !self.bitmap_build_idx.is_valid(slot)) {
                    validity.set_invalid(slot, build_size);
                }
            }
            self.perfect_hash_table.push(vector);
        }
        Ok(true)
    }

    /// Records the slot of every key of one scanned chunk; `false` on a duplicate
    /// (`TemplatedFillSelectionVectorBuild`).
    fn fill_selection_vector_build<T: PerfectKey>(
        &mut self,
        rows: &[RowPtr],
        keys: &Vector,
        tuples: &mut Vec<RowPtr>,
        sel_build: &mut Vec<u32>,
        unique_keys: &mut usize,
    ) -> bool {
        let values = keys.unified::<T>();
        for (row_index, &row) in rows.iter().enumerate() {
            let Some(slot) = self.slot_of(values.values[values.slot(row_index)]) else {
                continue;
            };
            if self.bitmap_build_idx.is_valid(slot) {
                return false;
            }
            self.bitmap_build_idx.set_valid(slot);
            *unique_keys += 1;
            sel_build.push(slot as u32);
            tuples.push(row);
        }
        true
    }

    /// The slot of `key`, if it is in the build range.
    #[inline]
    fn slot_of<T: PerfectKey>(&self, key: T) -> Option<usize> {
        let offset = key.to_i128()?.checked_sub(self.build_min)?;
        usize::try_from(offset)
            .ok()
            .filter(|&slot| slot <= self.build_range)
    }

    /// Joins one probe chunk: the rows of `lhs` whose key has a build row, with that
    /// row's build columns (`ProbePerfectHashTable`).
    pub(super) fn probe(
        &self,
        keys: &DataChunk,
        lhs: &DataChunk,
        state: &mut PerfectHashJoinState,
        result: &mut DataChunk,
    ) -> Result<()> {
        let key = keys.column(0);
        let count = keys.len();
        state.build_sel_vec.clear();
        state.probe_sel_vec.clear();
        dispatch_integer!(
            key.logical_type().physical_type(),
            T => self.fill_selection_vector_probe::<T>(key, count, state),
            return Err(Error::not_implemented("Type not supported"))
        );
        let probe_sel_count = state.probe_sel_vec.len();
        let mut columns = if self.is_build_dense && probe_sel_count == count {
            // Every probe row matched: reference the probe columns.
            lhs.columns().to_vec()
        } else {
            super::slice_columns(lhs, &state.probe_sel_vec)
        };
        let build_sel = SelectionVector::new(state.build_sel_vec.clone());
        columns.extend(
            self.perfect_hash_table
                .iter()
                .map(|vector| vector.slice(&build_sel)),
        );
        result.set_columns(columns, probe_sel_count);
        Ok(())
    }

    /// Selects the probe rows with a build row and their slots
    /// (`TemplatedFillSelectionVectorProbe`).
    fn fill_selection_vector_probe<T: PerfectKey>(
        &self,
        key: &Vector,
        count: usize,
        state: &mut PerfectHashJoinState,
    ) {
        let values = key.unified::<T>();
        let all_valid = values.validity.is_all_valid();
        for row in 0..count {
            let slot = values.slot(row);
            if !all_valid && !values.validity.is_valid(slot) {
                continue;
            }
            if let Some(index) = self.slot_of(values.values[slot])
                && self.bitmap_build_idx.is_valid(index)
            {
                state.build_sel_vec.push(index as u32);
                state.probe_sel_vec.push(row as u32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_range_tracks_min_and_max_of_valid_keys() {
        let mut keys = Vector::flat(LogicalType::INTEGER, 4);
        keys.values_mut::<i32>().copy_from_slice(&[7, -3, 100, 5]);
        keys.validity_mut().set_invalid(2, 4);
        let mut range = KeyRange::default();
        range.update(&keys, 4);
        assert_eq!(range.bounds, Some((-3, 7)));
        let mut other = KeyRange::default();
        other.merge(range);
        other.merge(KeyRange::default());
        assert_eq!(other.bounds, Some((-3, 7)));
    }

    #[test]
    fn unsigned_keys_beyond_i128_overflow() {
        let mut keys = Vector::flat(LogicalType::UHUGEINT, 1);
        keys.values_mut::<u128>()[0] = u128::MAX;
        let mut range = KeyRange::default();
        range.update(&keys, 1);
        assert!(range.overflow);
    }
}
