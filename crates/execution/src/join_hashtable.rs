//! The hash table of the hash join (DuckDB's `JoinHashTable`, `execution/join_hashtable.cpp`).
//!
//! Build rows are appended to a radix-partitioned [`PartitionedTupleData`] laid out as
//! `[keys, payload, (found flag), hash]`. Finalizing combines the partitions and inserts
//! every row into a pointer table of salted 64-bit entries ([`HtEntry`]): rows with equal
//! keys are chained through the hash column, which is overwritten with the next pointer.
//! Probing finds the chain of each probe row with linear probing, resolves any
//! non-equality conditions with a [`RowMatcher`] and walks the chains in lockstep
//! ([`ScanStructure`]), exactly like DuckDB, so results come out in DuckDB's order.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_buffer::{AllocatedData, BufferManager, MemoryTag};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_row::hash::{combine_hash_vector, hash_columns, hash_vector};
use rustdb_row::{
    ChunkFormat, ChunkIterator, HeapKeepAlive, MatchPredicate, PartitionedAppendState, PartitionedTupleData,
    PinProperties, RadixPartitioning, RowMatcher, RowPtr, ScanState, TupleDataCollection, TupleDataLayout,
    ValidityType,
};
use rustdb_types::{DataChunk, LogicalType, PhysicalType, Value, ValidityMask, Vector, VectorKind};

use crate::aggregate_hashtable::{AggregateObject, GroupedAggregateHashTable, initial_capacity};
use crate::operators::join::{self, propagates_build_side};

/// The error of a scalar subquery (SINGLE join) that finds more than one row.
pub(crate) const SCALAR_SUBQUERY_ERROR: &str = "More than one row returned by a subquery used as an expression - \
scalar subqueries can only return a single row.\n\nUse \"SET scalar_subquery_error_on_multiple_rows=false\" to \
revert to previous behavior of returning a random row.";

/// Upper 16 bits of an entry: the salt.
const SALT_MASK: u64 = 0xFFFF_0000_0000_0000;
/// Lower 48 bits of an entry: the row pointer.
const POINTER_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// Salts are only compared for tables that do not fit in the CPU cache.
const USE_SALT_THRESHOLD: usize = 8192;

/// One slot of the pointer table: a row pointer in the low 48 bits and the upper 16 bits
/// of the row's hash (DuckDB's `ht_entry_t`). Zero means empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HtEntry(u64);

impl HtEntry {
    fn new(salt: u64, row: RowPtr) -> Self {
        Self((row.as_ptr() as u64 & POINTER_MASK) | (salt & SALT_MASK))
    }

    const fn is_occupied(self) -> bool {
        self.0 != 0
    }

    fn pointer(self) -> RowPtr {
        RowPtr::from_ptr(std::ptr::with_exposed_provenance_mut(
            (self.0 & POINTER_MASK) as usize,
        ))
    }

    /// The salt of a hash with all pointer bits set (`ExtractSalt`).
    const fn extract_salt(hash: u64) -> u64 {
        hash | POINTER_MASK
    }

    const fn salt(self) -> u64 {
        Self::extract_salt(self.0)
    }

    const fn salt_with_nulls(self) -> u64 {
        self.0 & SALT_MASK
    }
}

/// `++offset &= mask` (`IncrementAndWrap`).
#[inline]
const fn increment_and_wrap(offset: u64, mask: u64) -> u64 {
    (offset + 1) & mask
}

/// Reads the pointer stored at `row + offset`.
///
/// # Safety
/// `row + offset` must be readable for 8 bytes.
#[inline]
unsafe fn load_pointer(row: RowPtr, offset: usize) -> RowPtr {
    // SAFETY: guaranteed by the caller.
    let address = unsafe { row.as_ptr().add(offset).cast::<u64>().read_unaligned() };
    RowPtr::from_ptr(std::ptr::with_exposed_provenance_mut(address as usize))
}

/// Writes `pointer` at `row + offset`.
///
/// # Safety
/// `row + offset` must be writable for 8 bytes and not read concurrently.
#[inline]
unsafe fn store_pointer(pointer: RowPtr, row: RowPtr, offset: usize) {
    let address = pointer.as_ptr().expose_provenance() as u64;
    // SAFETY: guaranteed by the caller.
    unsafe { row.as_ptr().add(offset).cast::<u64>().write_unaligned(address) };
}

/// Reads the hash stored at `row + offset` before finalization.
///
/// # Safety
/// `row + offset` must be readable for 8 bytes.
#[inline]
unsafe fn load_hash(row: RowPtr, offset: usize) -> u64 {
    // SAFETY: guaranteed by the caller.
    unsafe { row.as_ptr().add(offset).cast::<u64>().read_unaligned() }
}

/// The "found a match" flag of a build row (RIGHT/FULL/RIGHT SEMI/RIGHT ANTI joins).
/// Probe threads may set it concurrently, so it is accessed atomically.
///
/// # Safety
/// `row + offset` must point to the flag byte of a pinned row.
#[inline]
unsafe fn found_flag<'a>(row: RowPtr, offset: usize) -> &'a AtomicU8 {
    // SAFETY: guaranteed by the caller; `AtomicU8` has the size and alignment of `u8`.
    unsafe { AtomicU8::from_ptr(row.as_ptr().add(offset)) }
}

/// Hints the CPU to start loading the cache line at `address` (no effect on
/// correctness; probes issue these for rows they are about to compare).
#[inline]
fn prefetch_read(address: *const u8) {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `prfm` is a hint: it never faults, even for invalid addresses, and has no
    // architectural side effects.
    unsafe {
        std::arch::asm!("prfm pldl1keep, [{0}]", in(reg) address, options(nostack, readonly, preserves_flags));
    }
    #[cfg(target_arch = "x86_64")]
    // SAFETY: `prefetcht0` is a hint that never faults.
    unsafe {
        std::arch::x86_64::_mm_prefetch::<{ std::arch::x86_64::_MM_HINT_T0 }>(address.cast::<i8>());
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    let _ = address;
}

/// The row matcher predicate of a join comparison.
fn match_predicate(comparison: ExpressionType) -> Result<MatchPredicate> {
    Ok(match comparison {
        ExpressionType::CompareEqual => MatchPredicate::Equal,
        ExpressionType::CompareNotDistinctFrom => MatchPredicate::NotDistinctFrom,
        ExpressionType::CompareNotequal => MatchPredicate::NotEqual,
        ExpressionType::CompareDistinctFrom => MatchPredicate::DistinctFrom,
        ExpressionType::CompareGreaterthan => MatchPredicate::GreaterThan,
        ExpressionType::CompareGreaterthanorequalto => MatchPredicate::GreaterThanOrEqual,
        ExpressionType::CompareLessthan => MatchPredicate::LessThan,
        ExpressionType::CompareLessthanorequalto => MatchPredicate::LessThanOrEqual,
        other => {
            return Err(Error::internal(format!(
                "Unsupported join comparison {} in JoinHashTable",
                other.name()
            )));
        }
    })
}

/// Hashes the equality keys (the first `equality_count` columns of `keys`) of all rows
/// (`JoinHashTable::Hash`). Rows filtered out for NULL keys get hashes too; they are
/// never looked at.
fn hash_keys(keys: &DataChunk, equality_count: usize) -> Result<Vector> {
    let mut hashes = Vector::flat(LogicalType::UBIGINT, keys.len().max(1));
    hash_vector(keys.column(0), keys.len(), &mut hashes)?;
    for column in &keys.columns()[1..equality_count] {
        combine_hash_vector(&mut hashes, column, keys.len())?;
    }
    Ok(hashes)
}

/// Flattens sequence vectors, which the row format cannot read.
pub(crate) fn flatten_sequences(chunk: &mut DataChunk) {
    let len = chunk.len();
    for index in 0..chunk.column_count() {
        if chunk.column(index).kind() == VectorKind::Sequence {
            chunk.column_mut(index).flatten(len);
        }
    }
}

/// The rows of `vector` (among `sel[..count]`, or `0..count`) that are not NULL
/// (`FilterNullValues`).
fn filter_null_values(vector: &Vector, sel: Option<&[u32]>, count: usize, result: &mut Vec<u32>) {
    let validity = vector.validity();
    let slots = vector.sel();
    let mut filtered = Vec::with_capacity(count);
    for i in 0..count {
        let row = sel.map_or(i as u32, |sel| sel[i]);
        if validity.is_valid(slots.slot(row as usize)) {
            filtered.push(row);
        }
    }
    *result = filtered;
}

/// The pointer table: `capacity` atomic entries in memory charged to the buffer manager.
#[derive(Debug)]
struct PointerTable {
    memory: AllocatedData,
    capacity: usize,
}

impl PointerTable {
    fn entries(&self) -> &[AtomicU64] {
        // SAFETY: `memory` is 16-byte aligned, holds `capacity * 8` bytes that were
        // initialised (zeroed) after allocation, and lives as long as `self`. `AtomicU64`
        // has the size and (here satisfied) alignment of `u64`.
        unsafe { std::slice::from_raw_parts(self.memory.as_ptr().cast::<AtomicU64>(), self.capacity) }
    }
}

/// COUNT(*) and COUNT(key) per group of correlated values, for MARK joins of correlated
/// `ANY` subqueries (DuckDB's `correlated_mark_join_info`). An empty group makes the mark
/// false, a group with NULLs makes a false mark NULL.
#[derive(Debug)]
struct CorrelatedMarkJoinInfo {
    correlated_types: Vec<LogicalType>,
    correlated_counts: Mutex<GroupedAggregateHashTable>,
}

impl CorrelatedMarkJoinInfo {
    fn new(buffer_manager: BufferManager, correlated_types: Vec<LogicalType>) -> Result<Self> {
        use rustdb_functions::aggregate::distributive::count::{count_function, count_star_function};
        let aggregates = [
            AggregateObject {
                function: count_star_function(),
                bind_data: None,
                child_count: 0,
            },
            AggregateObject {
                function: count_function(),
                bind_data: None,
                child_count: 1,
            },
        ];
        let correlated_counts = GroupedAggregateHashTable::new(
            buffer_manager,
            correlated_types.clone(),
            &aggregates,
            initial_capacity(),
            0,
        )?;
        Ok(Self {
            correlated_types,
            correlated_counts: Mutex::new(correlated_counts),
        })
    }

    /// The correlated columns of `keys` and their hashes.
    fn groups(&self, keys: &DataChunk) -> Result<(DataChunk, Vector)> {
        let columns = keys.columns()[..self.correlated_types.len()].to_vec();
        let groups = DataChunk::new(columns, keys.len());
        let group_refs: Vec<&Vector> = groups.columns().iter().collect();
        let hashes = hash_columns(&group_refs, keys.len())?;
        Ok((groups, hashes))
    }
}

/// The join comparisons of a hash table, split into equalities (the keys) and others.
#[derive(Clone, Debug)]
pub struct JoinHashTableConditions {
    /// The comparison of each condition, equalities first.
    pub comparisons: Vec<ExpressionType>,
    /// The type of each condition's operands.
    pub types: Vec<LogicalType>,
}

/// The join hash table; see the module documentation.
#[derive(Debug)]
pub struct JoinHashTable {
    buffer_manager: BufferManager,
    pub(crate) join_type: JoinType,
    /// Number of leading equality conditions (`equality_predicate_columns`).
    equality_count: usize,
    condition_types: Vec<LogicalType>,
    /// Layout columns emitted for the build side (`output_columns`).
    output_columns: Vec<usize>,
    layout: Arc<TupleDataLayout>,
    /// Compares equality keys of build rows while inserting (`row_matcher_build`).
    row_matcher_build: RowMatcher,
    /// Resolves the non-equality conditions while probing (`row_matcher_probe`).
    row_matcher_probe: Option<RowMatcher>,
    /// Whether some key has more than one row, so chains must be followed.
    chains_longer_than_one: bool,
    /// Offset of the found flag (or of the hash if there is none) in a row (`tuple_size`).
    tuple_size: usize,
    /// Offset of the hash / next pointer in a row (`pointer_offset`).
    pointer_offset: usize,
    pub(crate) finalized: bool,
    /// Whether a build key was NULL (a false MARK becomes NULL).
    pub(crate) has_null: bool,
    bitmask: u64,
    single_join_error_on_multiple_rows: bool,
    /// Whether rows with duplicate keys are kept (not for single-condition SEMI, ANTI and
    /// MARK joins, which only need to know a key exists).
    insert_duplicate_keys: bool,
    null_values_are_equal: Vec<bool>,
    correlated: Option<CorrelatedMarkJoinInfo>,
    sink_collection: PartitionedTupleData,
    data_collection: TupleDataCollection,
    pointer_table: Option<PointerTable>,
    /// A zeroed row that ends chains (`dead_end`).
    dead_end: Box<[u64]>,
    heap_keep_alive: Option<HeapKeepAlive>,
    radix_bits: usize,
    /// Pointer table entries per row (`load_factor`).
    pub(crate) load_factor: f64,
    /// Partitions in the current external round (`current_partitions`).
    current_partitions: Vec<bool>,
    /// Partitions already built (`completed_partitions`).
    completed_partitions: Vec<bool>,
}

impl JoinHashTable {
    /// Partitions of the build side, 2^4 (`INITIAL_RADIX_BITS`).
    pub const INITIAL_RADIX_BITS: usize = 4;
    /// The table is 25-50% full (`DEFAULT_LOAD_FACTOR`).
    pub const DEFAULT_LOAD_FACTOR: f64 = 2.0;
    /// The table is 33-67% full (`EXTERNAL_LOAD_FACTOR`).
    pub const EXTERNAL_LOAD_FACTOR: f64 = 1.5;
    /// The smallest pointer table (`MINIMUM_CAPACITY`).
    pub const MINIMUM_CAPACITY: usize = 16384;

    /// A table for a join of `join_type` on `conditions` (equalities first) storing
    /// `build_types` payload columns and emitting layout columns `output_columns`.
    pub fn new(
        buffer_manager: BufferManager,
        conditions: &JoinHashTableConditions,
        build_types: Vec<LogicalType>,
        join_type: JoinType,
        output_columns: Vec<usize>,
        single_join_error_on_multiple_rows: bool,
    ) -> Result<Self> {
        let mut equality_predicates = Vec::new();
        let mut non_equality_predicates = Vec::new();
        let mut non_equality_columns = Vec::new();
        let mut null_values_are_equal = Vec::with_capacity(conditions.comparisons.len());
        for (index, &comparison) in conditions.comparisons.iter().enumerate() {
            if join::is_equality(comparison) {
                if !non_equality_predicates.is_empty() {
                    return Err(Error::internal(
                        "JoinHashTable: equality conditions must precede the others",
                    ));
                }
                equality_predicates.push(match_predicate(comparison)?);
            } else {
                non_equality_predicates.push(match_predicate(comparison)?);
                non_equality_columns.push(index);
            }
            null_values_are_equal.push(matches!(
                comparison,
                ExpressionType::CompareDistinctFrom | ExpressionType::CompareNotDistinctFrom
            ));
        }
        if equality_predicates.is_empty() {
            return Err(Error::internal("JoinHashTable requires an equality condition"));
        }
        let condition_types = conditions.types.clone();

        let mut layout_types = condition_types.clone();
        layout_types.extend(build_types.iter().cloned());
        if propagates_build_side(join_type) {
            layout_types.push(LogicalType::BOOLEAN);
        }
        layout_types.push(LogicalType::UBIGINT);
        let layout = Arc::new(TupleDataLayout::new(layout_types, ValidityType::CanHaveNull)?);

        let row_matcher_probe = if non_equality_predicates.is_empty() {
            None
        } else {
            Some(RowMatcher::new(
                &layout,
                &non_equality_predicates,
                Some(non_equality_columns.clone()),
            )?)
        };
        let row_matcher_build = RowMatcher::new(&layout, &equality_predicates, None)?;

        let offsets = layout.offsets();
        let tuple_size = offsets[condition_types.len() + build_types.len()];
        let pointer_offset = offsets[offsets.len() - 1];
        let hash_column = layout.column_count() - 1;
        let data_collection =
            TupleDataCollection::new(buffer_manager.clone(), Arc::clone(&layout), MemoryTag::HashTable)?;
        let sink_collection = PartitionedTupleData::new(
            buffer_manager.clone(),
            Arc::clone(&layout),
            MemoryTag::HashTable,
            Self::INITIAL_RADIX_BITS,
            hash_column,
        )?;
        let dead_end = vec![0u64; layout.row_width().div_ceil(8)].into_boxed_slice();
        let insert_duplicate_keys = !(conditions.comparisons.len() == 1
            && matches!(join_type, JoinType::Semi | JoinType::Anti | JoinType::Mark));
        let partitions = RadixPartitioning::number_of_partitions(Self::INITIAL_RADIX_BITS);
        Ok(Self {
            buffer_manager,
            join_type,
            equality_count: equality_predicates.len(),
            condition_types,
            output_columns,
            layout,
            row_matcher_build,
            row_matcher_probe,
            chains_longer_than_one: false,
            tuple_size,
            pointer_offset,
            finalized: false,
            has_null: false,
            bitmask: 0,
            single_join_error_on_multiple_rows: join_type != JoinType::Single || single_join_error_on_multiple_rows,
            insert_duplicate_keys,
            null_values_are_equal,
            correlated: None,
            sink_collection,
            data_collection,
            pointer_table: None,
            dead_end,
            heap_keep_alive: None,
            radix_bits: Self::INITIAL_RADIX_BITS,
            load_factor: Self::DEFAULT_LOAD_FACTOR,
            current_partitions: vec![false; partitions],
            completed_partitions: vec![false; partitions],
        })
    }

    /// Turns this into the table of a correlated MARK join whose first
    /// `correlated_types.len()` conditions compare the correlated columns
    /// (`PhysicalHashJoin::InitializeHashTable`).
    pub fn enable_correlated_mark_join(&mut self, correlated_types: Vec<LogicalType>) -> Result<()> {
        self.correlated = Some(CorrelatedMarkJoinInfo::new(
            self.buffer_manager.clone(),
            correlated_types,
        )?);
        Ok(())
    }

    /// The row layout.
    pub fn layout(&self) -> &TupleDataLayout {
        &self.layout
    }

    /// Number of build rows (`Count`).
    pub fn count(&self) -> usize {
        self.data_collection.count()
    }

    /// Bytes of build rows (`SizeInBytes`).
    pub fn size_in_bytes(&self) -> usize {
        self.data_collection.size_in_bytes()
    }

    /// The finalized (or currently built) rows.
    pub fn data_collection(&self) -> &TupleDataCollection {
        &self.data_collection
    }

    /// The rows sunk so far, partitioned (`GetSinkCollection`).
    pub fn sink_collection_mut(&mut self) -> &mut PartitionedTupleData {
        &mut self.sink_collection
    }

    /// The rows sunk so far, partitioned.
    pub fn sink_collection(&self) -> &PartitionedTupleData {
        &self.sink_collection
    }

    /// Number of radix bits the build side is partitioned on.
    pub fn radix_bits(&self) -> usize {
        self.radix_bits
    }

    /// The types of the condition operands.
    pub fn condition_types(&self) -> &[LogicalType] {
        &self.condition_types
    }

    /// Whether NULLs match NULLs in condition `column` (`NullValuesAreEqual`).
    pub fn null_values_are_equal(&self, column: usize) -> bool {
        self.null_values_are_equal[column]
    }

    /// A state for appending build rows from one thread.
    pub fn initialize_append_state(&mut self) -> PartitionedAppendState {
        self.sink_collection
            .initialize_append_state(PinProperties::UnpinAfterDone)
    }

    /// Entries of the pointer table for `count` rows (`PointerTableCapacity`).
    pub fn pointer_table_capacity(&self, count: usize) -> usize {
        let capacity = ((count as f64 * self.load_factor) as usize).next_power_of_two();
        capacity.max(Self::MINIMUM_CAPACITY)
    }

    /// Bytes of the pointer table for `count` rows (`PointerTableSize`).
    pub fn pointer_table_size(&self, count: usize) -> usize {
        self.pointer_table_capacity(count) * size_of::<u64>()
    }

    // ---------------------------------------------------------------------------------
    // Build
    // ---------------------------------------------------------------------------------

    /// Adds build rows: `keys` holds the condition columns, `payload` the build columns
    /// (`Build`). Rows with NULL keys are dropped unless NULLs compare equal or the build
    /// side is propagated.
    pub fn build(
        &mut self,
        append_state: &mut PartitionedAppendState,
        keys: &DataChunk,
        payload: &DataChunk,
    ) -> Result<()> {
        let count = keys.len();
        if count == 0 {
            return Ok(());
        }
        if self.join_type == JoinType::Mark
            && let Some(info) = &mut self.correlated
        {
            // Count rows and non-NULL keys per correlated group.
            let (groups, hashes) = info.groups(keys)?;
            let payload_column = keys.column(info.correlated_types.len()).clone();
            info.correlated_counts
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner)
                .add_chunk(&groups, &hashes, &[Vec::new(), vec![payload_column]], &[])?;
        }

        let (sel, added_count) = self.prepare_keys(keys, true);
        if added_count < count {
            self.has_null = true;
        }
        if added_count == 0 {
            return Ok(());
        }
        let hashes = hash_keys(keys, self.equality_count)?;

        // [keys, payload, (found flag), hash]
        let mut columns: Vec<Vector> = keys.columns().to_vec();
        columns.extend(payload.columns().iter().cloned());
        if propagates_build_side(self.join_type) {
            columns.push(Vector::constant(&Value::boolean(false))?);
        }
        columns.push(hashes);
        let formats = ChunkFormat::from_columns(&columns, count)?;
        self.sink_collection
            .append_formatted(append_state, &formats, sel.as_deref(), added_count)
    }

    /// The rows whose keys may match (`PrepareKeys`): `None` for all rows, else the rows
    /// without NULLs in NULL-rejecting key columns. Returns the selection and its count.
    fn prepare_keys(&self, keys: &DataChunk, build_side: bool) -> (Option<Vec<u32>>, usize) {
        let mut count = keys.len();
        if build_side && propagates_build_side(self.join_type) {
            // RIGHT/FULL joins keep NULL keys: those rows are emitted as unmatched.
            return (None, count);
        }
        if self.join_type == JoinType::Mark && self.correlated.is_some() {
            // Correlated MARK joins count NULL keys per group.
            return (None, count);
        }
        let mut sel: Option<Vec<u32>> = None;
        for (column, &nulls_equal) in self.null_values_are_equal.iter().enumerate() {
            let vector = keys.column(column);
            if nulls_equal || vector.validity().is_all_valid() {
                continue;
            }
            let mut filtered = Vec::new();
            filter_null_values(vector, sel.as_deref(), count, &mut filtered);
            count = filtered.len();
            sel = Some(filtered);
        }
        (sel, count)
    }

    /// Moves the rows of a thread-local table into this one (`Merge`).
    pub fn merge(&mut self, other: JoinHashTable) -> Result<()> {
        self.data_collection.combine(other.data_collection)?;
        if self.join_type == JoinType::Mark {
            self.has_null |= other.has_null;
            if let (Some(info), Some(other_info)) = (&mut self.correlated, other.correlated) {
                let mut other_counts = other_info
                    .correlated_counts
                    .into_inner()
                    .unwrap_or_else(PoisonError::into_inner);
                let partitioned = other_counts.take_partitioned_data()?;
                let counts = info
                    .correlated_counts
                    .get_mut()
                    .unwrap_or_else(PoisonError::into_inner);
                for partition in partitioned.partitions() {
                    counts.combine(partition)?;
                }
            }
        }
        self.sink_collection.combine(other.sink_collection)
    }

    /// Collects all partitions into the data collection (`Unpartition`).
    pub fn unpartition(&mut self) -> Result<()> {
        self.data_collection = self.sink_collection.take_unpartitioned()?;
        Ok(())
    }

    /// Allocates the pointer table for the current rows (`AllocatePointerTable`).
    pub fn allocate_pointer_table(&mut self) -> Result<()> {
        let mut capacity = self.pointer_table_capacity(self.count());
        const MAX_HASHTABLE_CAPACITY: usize = (1 << 48) - 1;
        if capacity >= MAX_HASHTABLE_CAPACITY {
            return Err(Error::internal("Hashtable capacity exceeds 48-bit limit (2^48 - 1)"));
        }
        match &self.pointer_table {
            Some(table) if table.capacity >= capacity => capacity = table.capacity,
            _ => {
                // Drop the old table first so its memory can be reused.
                self.pointer_table = None;
                let memory = self.buffer_manager.allocate_data(capacity * size_of::<u64>())?;
                self.pointer_table = Some(PointerTable { memory, capacity });
            }
        }
        self.bitmask = capacity as u64 - 1;
        Ok(())
    }

    /// Clears pointer table entries `from..to` (`InitializePointerTable`).
    pub fn initialize_pointer_table(&mut self, from: usize, to: usize) {
        if let Some(table) = &self.pointer_table {
            // SAFETY: the table holds `capacity >= to` entries of plain `u64`s; zero is a
            // valid (empty) entry, and nothing reads the table during initialization.
            unsafe {
                std::ptr::write_bytes(
                    table.memory.as_ptr().cast::<u64>().add(from),
                    0,
                    to.saturating_sub(from),
                );
            }
        }
    }

    /// Number of pointer table entries (`capacity`).
    pub fn capacity(&self) -> usize {
        self.pointer_table.as_ref().map_or(0, |table| table.capacity)
    }

    /// Inserts the rows of data collection chunks `chunks` into the pointer table
    /// (`Finalize`). Entries are updated atomically when `parallel`, so several threads
    /// may insert disjoint chunk ranges.
    pub fn finalize(&mut self, chunks: std::ops::Range<usize>, parallel: bool) -> Result<()> {
        let table = self
            .pointer_table
            .as_ref()
            .ok_or_else(|| Error::internal("JoinHashTable::Finalize without a pointer table"))?;
        let mut inserter = Inserter::new(self);
        let mut iterator = ChunkIterator::range(
            &self.data_collection,
            PinProperties::KeepEverythingPinned,
            chunks,
            false,
        )?;
        let mut hashes = Vec::with_capacity(STANDARD_VECTOR_SIZE);
        while !iterator.done() {
            let rows = iterator.row_locations();
            hashes.clear();
            // SAFETY: the rows are pinned rows of this layout whose hash column holds a u64.
            hashes.extend(rows.iter().map(|&row| unsafe { load_hash(row, self.pointer_offset) }));
            if parallel {
                inserter.insert_hashes::<true>(table.entries(), rows, &hashes)?;
            } else {
                inserter.insert_hashes::<false>(table.entries(), rows, &hashes)?;
            }
            iterator.next_chunk()?;
        }
        drop(iterator);
        if inserter.chains_longer_than_one {
            self.chains_longer_than_one = true;
        }
        Ok(())
    }

    /// Marks the table ready for probing.
    pub fn mark_finalized(&mut self) {
        self.heap_keep_alive = Some(self.data_collection.heap_keep_alive());
        self.finalized = true;
    }

    fn use_salt(&self) -> bool {
        self.capacity() > USE_SALT_THRESHOLD
    }

    // ---------------------------------------------------------------------------------
    // Probe
    // ---------------------------------------------------------------------------------

    /// Prepares `scan` for the probe `keys` and finds the chain of every probe row whose
    /// equality keys exist (`Probe`). `precomputed_hashes` are the keys' hashes, if known.
    pub fn probe(
        &self,
        scan: &mut ScanStructure,
        keys: &DataChunk,
        state: &mut ProbeState,
        precomputed_hashes: Option<&Vector>,
    ) -> Result<()> {
        let current_sel = self.initialize_scan_structure(scan, keys);
        if scan.count == 0 {
            return Ok(());
        }
        let computed;
        let hashes = if let Some(hashes) = precomputed_hashes {
            hashes
        } else {
            computed = hash_keys(keys, self.equality_count)?;
            &computed
        };
        let has_sel = scan.has_null_value_filter;
        self.get_row_pointers(keys, state, hashes, current_sel.as_deref(), has_sel, scan)
    }

    /// Resets `scan` for a new probe chunk and filters NULL keys
    /// (`InitializeScanStructure`). Returns the selection of candidate rows.
    fn initialize_scan_structure(&self, scan: &mut ScanStructure, keys: &DataChunk) -> Option<Vec<u32>> {
        scan.is_null = false;
        scan.finished = false;
        scan.last_match_count = 0;
        scan.found_match.clear();
        scan.found_match.resize(keys.len(), false);
        let (sel, count) = self.prepare_keys(keys, false);
        scan.count = count;
        scan.has_null_value_filter = count < keys.len();
        sel
    }

    /// Finds the chain head of every candidate row by linear probing, comparing keys of
    /// occupied entries (`GetRowPointers`). Afterwards `scan.sel_vector[..scan.count]`
    /// holds the rows with a matching key, in the order they were resolved.
    fn get_row_pointers(
        &self,
        keys: &DataChunk,
        state: &mut ProbeState,
        hashes: &Vector,
        row_sel: Option<&[u32]>,
        has_row_sel: bool,
        scan: &mut ScanStructure,
    ) -> Result<()> {
        let table = self
            .pointer_table
            .as_ref()
            .ok_or_else(|| Error::internal("probing a hash table without a pointer table"))?;
        let entries = table.entries();
        let count = scan.count;
        let formats = ChunkFormat::new(keys)?;

        // Densify the hashes of the candidate rows.
        state.hashes_dense.clear();
        {
            let unified = hashes.unified::<u64>();
            if has_row_sel {
                let row_sel = row_sel.unwrap_or_default();
                state
                    .hashes_dense
                    .extend(row_sel[..count].iter().map(|&row| unified.values[unified.slot(row as usize)]));
            } else {
                state
                    .hashes_dense
                    .extend((0..count).map(|row| unified.values[unified.slot(row)]));
            }
        }
        state.ht_offsets_and_salts.resize(keys.len().max(STANDARD_VECTOR_SIZE), 0);
        scan.pointers.resize(keys.len().max(STANDARD_VECTOR_SIZE), RowPtr::NULL);

        let use_salts = self.use_salt();
        let mut match_count = 0;
        scan.sel_vector.clear();
        // The rows still probing: `None` = `0..count`.
        let mut probing: Option<Vec<u32>> = if has_row_sel {
            Some(row_sel.unwrap_or_default()[..count].to_vec())
        } else {
            None
        };
        let mut elements = count;
        loop {
            // Load the first entry of every row in one pass: the loads are independent,
            // so their cache misses overlap.
            state.first_entries.clear();
            state.first_entries.extend(
                state.hashes_dense[..elements]
                    .iter()
                    .map(|&hash| entries[(hash & self.bitmask) as usize].load(Ordering::Relaxed)),
            );
            // Start loading the rows the entries point to, so the key comparisons hit.
            for &entry in &state.first_entries {
                if entry != 0 {
                    prefetch_read(HtEntry(entry).pointer().as_ptr());
                }
            }
            state.keys_to_compare.clear();
            for i in 0..elements {
                let row_hash = state.hashes_dense[i];
                let mut offset = row_hash & self.bitmask;
                let row_index = probing.as_ref().map_or(i as u32, |sel| sel[i]);
                let mut entry = HtEntry(state.first_entries[i]);
                if use_salts {
                    let row_salt = HtEntry::extract_salt(row_hash);
                    loop {
                        if !entry.is_occupied() {
                            break;
                        }
                        if entry.salt() == row_salt {
                            state.add_pointer_to_compare(scan, entry, offset, row_index);
                            break;
                        }
                        offset = increment_and_wrap(offset, self.bitmask);
                        entry = HtEntry(entries[offset as usize].load(Ordering::Relaxed));
                    }
                } else if entry.is_occupied() {
                    state.add_pointer_to_compare(scan, entry, offset, row_index);
                }
            }
            let compare_count = state.keys_to_compare.len();
            if compare_count == 0 {
                break;
            }
            state.keys_no_match.clear();
            // SAFETY: every compared pointer is a pinned row of this table's layout.
            let matched = unsafe {
                self.row_matcher_build.match_rows(
                    &self.layout,
                    &formats,
                    &mut state.keys_to_compare,
                    compare_count,
                    &scan.pointers,
                    Some(&mut state.keys_no_match),
                )?
            };
            scan.sel_vector.extend_from_slice(&state.keys_to_compare[..matched]);
            match_count += matched;
            // Keep probing the rows whose keys differ, one entry further.
            let no_match = state.keys_no_match.len();
            for i in 0..no_match {
                let row_index = state.keys_no_match[i] as usize;
                let offset_and_salt = state.ht_offsets_and_salts[row_index];
                state.hashes_dense[i] = increment_and_wrap(offset_and_salt, self.bitmask | SALT_MASK);
            }
            elements = no_match;
            if no_match == 0 {
                break;
            }
            probing = Some(std::mem::take(&mut state.keys_no_match));
        }
        scan.count = match_count;
        Ok(())
    }

    /// Emits the build rows of a RIGHT/FULL join that found no match, or the matched
    /// (RIGHT SEMI) / unmatched (RIGHT ANTI) ones, continuing at `state`
    /// (`ScanFullOuter`). `result_types` are the output types; leaves `result` empty when
    /// the scan is done.
    pub fn scan_full_outer(
        &self,
        state: &mut FullOuterScanState,
        result_types: &[LogicalType],
        result: &mut DataChunk,
    ) -> Result<()> {
        let wanted = u8::from(self.join_type == JoinType::RightSemi);
        let mut found: Vec<RowPtr> = Vec::with_capacity(STANDARD_VECTOR_SIZE);
        while state.chunk < state.chunk_end && found.len() < STANDARD_VECTOR_SIZE {
            let count = self
                .data_collection
                .fetch_chunk(&mut state.scan, state.chunk, false)?;
            let rows = &state.scan.chunk_state.row_locations()[..count];
            let mut i = state.offset_in_chunk;
            while i < count {
                let row = rows[i];
                i += 1;
                // SAFETY: rows of a finalized table are pinned and carry a found flag.
                if unsafe { found_flag(row, self.tuple_size) }.load(Ordering::Relaxed) == wanted {
                    found.push(row);
                    if found.len() == STANDARD_VECTOR_SIZE {
                        break;
                    }
                }
            }
            if i < count {
                state.offset_in_chunk = i;
            } else {
                state.offset_in_chunk = 0;
                state.chunk += 1;
            }
        }
        if found.is_empty() {
            return Ok(());
        }
        let count = found.len();
        let left_column_count = if matches!(self.join_type, JoinType::RightSemi | JoinType::RightAnti) {
            0
        } else {
            result_types.len() - self.output_columns.len()
        };
        let mut columns = join::null_columns(&result_types[..left_column_count]);
        for (index, &column) in self.output_columns.iter().enumerate() {
            let mut vector = Vector::flat(result_types[left_column_count + index].clone(), count);
            // SAFETY: the rows are pinned rows of this table.
            unsafe { self.gather(&found, None, count, column, &mut vector, None)? };
            columns.push(vector);
        }
        result.set_columns(columns, count);
        Ok(())
    }

    /// A scan of data collection chunks `chunks` for [`Self::scan_full_outer`].
    pub fn full_outer_scan_state(&self, chunks: std::ops::Range<usize>) -> FullOuterScanState {
        FullOuterScanState {
            scan: self
                .data_collection
                .initialize_scan_columns(Vec::new(), PinProperties::AlreadyPinned),
            chunk: chunks.start,
            chunk_end: chunks.end,
            offset_in_chunk: 0,
        }
    }

    /// Gathers layout column `column` of `rows[scan_sel[i]]` into `target[target_sel[i]]`.
    ///
    /// # Safety
    /// The selected rows must be pinned rows of this table.
    unsafe fn gather(
        &self,
        rows: &[RowPtr],
        scan_sel: Option<&[u32]>,
        count: usize,
        column: usize,
        target: &mut Vector,
        target_sel: Option<&[u32]>,
    ) -> Result<()> {
        // SAFETY: forwarded from the caller; the heap keep-alive owns the heap pins.
        unsafe {
            self.data_collection.gather(
                rows,
                scan_sel,
                count,
                &[column],
                std::slice::from_mut(target),
                target_sel,
                self.heap_keep_alive.as_ref(),
            )
        }
    }

    // ---------------------------------------------------------------------------------
    // External join
    // ---------------------------------------------------------------------------------

    /// Total bytes of the table if every partition of `local_tables` were built, with the
    /// largest partition's size and row count (`GetTotalSize`).
    pub fn total_size_of(&self, local_tables: &[JoinHashTable]) -> (usize, usize, usize) {
        let partitions = RadixPartitioning::number_of_partitions(self.radix_bits);
        let mut sizes = vec![0; partitions];
        let mut counts = vec![0; partitions];
        for table in local_tables {
            table.sink_collection.add_sizes_and_counts(&mut sizes, &mut counts);
        }
        self.total_size(&sizes, &counts)
    }

    /// `GetTotalSize` over per-partition sizes and counts: (total, largest partition
    /// size, largest partition count).
    pub fn total_size(&self, sizes: &[usize], counts: &[usize]) -> (usize, usize, usize) {
        let mut total_size = 0;
        let mut total_count = 0;
        let mut max_ht_size = 0;
        let mut max_partition_size = 0;
        let mut max_partition_count = 0;
        for (&size, &count) in sizes.iter().zip(counts) {
            total_size += size;
            total_count += count;
            let partition_size = size + self.pointer_table_size(count);
            if partition_size > max_ht_size {
                max_ht_size = partition_size;
                max_partition_size = size;
                max_partition_count = count;
            }
        }
        if total_count == 0 {
            return (0, max_partition_size, max_partition_count);
        }
        (
            total_size + self.pointer_table_size(total_count),
            max_partition_size,
            max_partition_count,
        )
    }

    /// Bytes of the partitions not built yet (`GetRemainingSize`).
    pub fn remaining_size(&self) -> usize {
        let mut count = 0;
        let mut size = 0;
        for (partition, &completed) in self.sink_collection.partitions().iter().zip(&self.completed_partitions) {
            if !completed {
                count += partition.count();
                size += partition.size_in_bytes();
            }
        }
        size + self.pointer_table_size(count)
    }

    /// Increases the radix bits so that partitions fit in `max_ht_size`
    /// (`SetRepartitionRadixBits`).
    pub fn set_repartition_radix_bits(
        &mut self,
        max_ht_size: usize,
        max_partition_size: usize,
        max_partition_count: usize,
    ) -> Result<()> {
        let max_added_bits = RadixPartitioning::MAX_RADIX_BITS - self.radix_bits;
        let mut added_bits = 1;
        while added_bits < max_added_bits {
            let multiplier = RadixPartitioning::number_of_partitions(added_bits) as f64;
            let new_size = max_partition_size as f64 / multiplier;
            let new_count = max_partition_count as f64 / multiplier;
            let new_ht_size = new_size + self.pointer_table_size(new_count as usize) as f64;
            if new_ht_size <= max_ht_size as f64 / 4.0 {
                break;
            }
            added_bits += 1;
        }
        self.radix_bits += added_bits;
        self.sink_collection = self.sink_collection.with_radix_bits(self.radix_bits)?;
        self.initialize_partition_masks();
        Ok(())
    }

    fn initialize_partition_masks(&mut self) {
        let partitions = RadixPartitioning::number_of_partitions(self.radix_bits);
        self.current_partitions = vec![false; partitions];
        self.completed_partitions = vec![false; partitions];
    }

    /// Partitions in the current round (`CurrentPartitionCount`).
    pub fn current_partition_count(&self) -> usize {
        self.current_partitions.iter().filter(|&&current| current).count()
    }

    /// Partitions finished before the current round (`FinishedPartitionCount`).
    pub fn finished_partition_count(&self) -> usize {
        self.completed_partitions
            .iter()
            .filter(|&&completed| completed)
            .count()
            - self.current_partition_count()
    }

    /// Repartitions this thread-local table to `global`'s radix bits and merges it
    /// (`Repartition`).
    pub fn repartition_into(mut self, global: &mut JoinHashTable) -> Result<()> {
        let mut target = self.sink_collection.with_radix_bits(global.radix_bits)?;
        self.sink_collection.repartition(&mut target)?;
        self.sink_collection = target;
        self.radix_bits = global.radix_bits;
        global.merge(self)
    }

    /// Drops the current round's rows and pointer table (`Reset`).
    pub fn reset(&mut self) {
        self.data_collection.reset();
        self.pointer_table = None;
        self.heap_keep_alive = None;
        self.current_partitions.fill(false);
        self.finalized = false;
    }

    /// Moves the next partitions that fit in `max_ht_size` (at least one) into the data
    /// collection, smallest first; returns `false` when all partitions are done
    /// (`PrepareExternalFinalize`).
    pub fn prepare_external_finalize(&mut self, max_ht_size: usize) -> Result<bool> {
        if self.finalized {
            self.reset();
        }
        if self.completed_partitions.iter().all(|&completed| completed) {
            return Ok(false);
        }
        let sizes: Vec<(usize, usize)> = self
            .sink_collection
            .partitions()
            .iter()
            .map(|partition| (partition.size_in_bytes(), partition.count()))
            .collect();
        let ht_size = |index: usize| sizes[index].0 + self.pointer_table_size(sizes[index].1);
        let mut indices: Vec<usize> = (0..sizes.len())
            .filter(|&index| !self.completed_partitions[index])
            .collect();
        let min_partition_size = indices.iter().map(|&index| ht_size(index)).min().unwrap_or(1).max(1);
        // Sort by size, rounded down to multiples of the smallest partition, keeping the
        // original order otherwise (partition order determines eviction order).
        indices.sort_by_key(|&index| ht_size(index) / min_partition_size);

        let mut count = 0;
        let mut data_size = 0;
        for index in indices {
            let incl_count = count + sizes[index].1;
            let incl_data_size = data_size + sizes[index].0;
            let incl_ht_size = incl_data_size + self.pointer_table_size(incl_count);
            if count > 0 && incl_ht_size > max_ht_size {
                break;
            }
            count = incl_count;
            data_size = incl_data_size;
            self.current_partitions[index] = true;
            let partition = &mut self.sink_collection.partitions_mut()[index];
            let rows = std::mem::replace(partition, partition.empty_like());
            self.data_collection.combine(rows)?;
            self.completed_partitions[index] = true;
        }
        Ok(true)
    }

    /// Whether the partition of `hash` is being built in the current round.
    pub fn hash_in_current_partitions(&self, hash: u64) -> bool {
        self.current_partitions[RadixPartitioning::partition_index(hash, self.radix_bits)]
    }

    /// The partitions of the current round.
    pub fn current_partitions(&self) -> &[bool] {
        &self.current_partitions
    }

    /// The hashes of the probe keys (`Hash`), for spilling them with the probe rows.
    pub fn hash_probe_keys(&self, keys: &DataChunk) -> Result<Vector> {
        hash_keys(keys, self.equality_count)
    }
}

/// Inserts rows into the pointer table (DuckDB's `InsertHashes` with its `InsertState`).
struct Inserter<'a> {
    ht: &'a JoinHashTable,
    chains_longer_than_one: bool,
    ht_offsets: Vec<u64>,
    salts: Vec<u64>,
    remaining: Vec<u32>,
    keys_to_compare: Vec<u32>,
    rhs_rows: Vec<RowPtr>,
    key_match: Vec<u32>,
    keys_no_match: Vec<u32>,
    equality_columns: Vec<usize>,
}

impl<'a> Inserter<'a> {
    fn new(ht: &'a JoinHashTable) -> Self {
        Self {
            ht,
            chains_longer_than_one: false,
            ht_offsets: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            salts: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            remaining: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            keys_to_compare: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            rhs_rows: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            key_match: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            keys_no_match: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            equality_columns: (0..ht.equality_count).collect(),
        }
    }

    /// Inserts the given (pinned) rows with their hashes (`InsertHashesLoop`).
    fn insert_hashes<const PARALLEL: bool>(
        &mut self,
        entries: &[AtomicU64],
        rows: &[RowPtr],
        hashes: &[u64],
    ) -> Result<()> {
        let ht = self.ht;
        let count = rows.len();
        self.ht_offsets.clear();
        self.salts.clear();
        for &hash in hashes {
            self.salts.push(HtEntry::extract_salt(hash));
            self.ht_offsets.push(hash & ht.bitmask);
        }
        self.remaining.clear();
        self.remaining.extend(0..count as u32);
        if propagates_build_side(ht.join_type) {
            // NULL keys were kept for the outer scan; they never match, so do not insert
            // them unless NULLs compare equal.
            let column_count = ht.layout.column_count();
            for column in 0..ht.null_values_are_equal.len() {
                if ht.null_values_are_equal[column] || ht.layout.all_valid() {
                    continue;
                }
                self.remaining.retain(|&row| {
                    // SAFETY: the row is a pinned row with `column_count` validity bits.
                    unsafe { row_is_valid(rows[row as usize], column, column_count) }
                });
            }
        }
        let capacity_mask = ht.bitmask | SALT_MASK;
        while !self.remaining.is_empty() {
            self.keys_to_compare.clear();
            self.rhs_rows.clear();
            for &row_index in &self.remaining {
                let row_index = row_index as usize;
                let salt = self.salts[row_index];
                let mut offset = self.ht_offsets[row_index];
                let (entry, occupied) = loop {
                    let entry = HtEntry(entries[offset as usize].load(Ordering::Relaxed));
                    if !entry.is_occupied() || entry.salt() == salt {
                        break (entry, entry.is_occupied());
                    }
                    offset = increment_and_wrap(offset, capacity_mask);
                };
                self.ht_offsets[row_index] = offset;
                if occupied {
                    self.keys_to_compare.push(row_index as u32);
                    self.rhs_rows.push(entry.pointer());
                    continue;
                }
                let row = rows[row_index];
                let collided = insert_row_to_entry::<PARALLEL, true>(
                    &entries[offset as usize],
                    row,
                    salt,
                    ht.pointer_offset,
                );
                if PARALLEL && let Some(collided) = collided {
                    // Another thread took the entry: compare with its row.
                    self.keys_to_compare.push(row_index as u32);
                    self.rhs_rows.push(collided);
                }
            }
            self.keys_no_match.clear();
            if !self.keys_to_compare.is_empty() {
                self.perform_key_comparison(rows)?;
                self.insert_matches_and_increment_misses::<PARALLEL>(entries, rows, capacity_mask);
            }
            std::mem::swap(&mut self.remaining, &mut self.keys_no_match);
        }
        Ok(())
    }

    /// Compares the equality keys of the rows to insert (`keys_to_compare`) with the rows
    /// already in their entries (`rhs_rows`), splitting them into `key_match` and
    /// `keys_no_match` positions (`PerformKeyComparison`).
    fn perform_key_comparison(&mut self, rows: &[RowPtr]) -> Result<()> {
        let ht = self.ht;
        let count = self.keys_to_compare.len();
        let mut lhs: Vec<Vector> = self
            .equality_columns
            .iter()
            .map(|&column| Vector::flat(ht.layout.types()[column].clone(), count))
            .collect();
        // SAFETY: the rows being inserted are pinned rows of this table.
        unsafe {
            ht.data_collection.gather(
                rows,
                Some(&self.keys_to_compare),
                count,
                &self.equality_columns,
                &mut lhs,
                None,
                None,
            )?;
        }
        let formats = ChunkFormat::from_columns(&lhs, count)?;
        self.key_match.clear();
        self.key_match.extend(0..count as u32);
        let mut no_match = Vec::new();
        // SAFETY: `rhs_rows[i]` are pinned rows of this table.
        let matched = unsafe {
            ht.row_matcher_build.match_rows(
                &ht.layout,
                &formats,
                &mut self.key_match,
                count,
                &self.rhs_rows,
                Some(&mut no_match),
            )?
        };
        self.key_match.truncate(matched);
        // Positions in `keys_to_compare`; translated by the caller.
        self.keys_no_match = no_match;
        Ok(())
    }

    /// Prepends the rows with equal keys to their chains and moves the others on by one
    /// entry (`InsertMatchesAndIncrementMisses`). Leaves the rows still to insert in
    /// `keys_no_match`.
    fn insert_matches_and_increment_misses<const PARALLEL: bool>(
        &mut self,
        entries: &[AtomicU64],
        rows: &[RowPtr],
        capacity_mask: u64,
    ) {
        let ht = self.ht;
        if !self.key_match.is_empty() {
            self.chains_longer_than_one = true;
        }
        if ht.insert_duplicate_keys {
            for &position in &self.key_match {
                let row_index = self.keys_to_compare[position as usize] as usize;
                let entry = &entries[self.ht_offsets[row_index] as usize];
                insert_row_to_entry::<PARALLEL, false>(entry, rows[row_index], self.salts[row_index], ht.pointer_offset);
            }
        }
        for position in &mut self.keys_no_match {
            let row_index = self.keys_to_compare[*position as usize] as usize;
            self.ht_offsets[row_index] = increment_and_wrap(self.ht_offsets[row_index], capacity_mask);
            *position = row_index as u32;
        }
    }
}

/// Whether column `column` of the row is valid.
///
/// # Safety
/// `row` must be a pinned row with `column_count` validity bits.
#[inline]
unsafe fn row_is_valid(row: RowPtr, column: usize, column_count: usize) -> bool {
    debug_assert!(column < column_count);
    // SAFETY: guaranteed by the caller.
    unsafe { (*row.as_ptr().add(column / 8) >> (column % 8)) & 1 == 1 }
}

/// Makes `entry` point to `row`, chaining the previous row behind it
/// (`InsertRowToEntry`). With `EXPECT_EMPTY` in parallel mode, returns the row that
/// took the entry first instead of inserting.
#[inline]
fn insert_row_to_entry<const PARALLEL: bool, const EXPECT_EMPTY: bool>(
    entry: &AtomicU64,
    row: RowPtr,
    salt: u64,
    pointer_offset: usize,
) -> Option<RowPtr> {
    let desired = HtEntry::new(salt, row);
    if PARALLEL {
        if EXPECT_EMPTY {
            // SAFETY: `row` is a pinned row being inserted by this thread only.
            unsafe { store_pointer(RowPtr::NULL, row, pointer_offset) };
            return match entry.compare_exchange(0, desired.0, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) => None,
                Err(current) => Some(HtEntry(current).pointer()),
            };
        }
        let mut expected = entry.load(Ordering::Relaxed);
        loop {
            // SAFETY: as above.
            unsafe { store_pointer(HtEntry(expected).pointer(), row, pointer_offset) };
            match entry.compare_exchange_weak(expected, desired.0, Ordering::Release, Ordering::Relaxed) {
                Ok(_) => return None,
                Err(current) => expected = current,
            }
        }
    }
    let current = HtEntry(entry.load(Ordering::Relaxed));
    let next = if current.is_occupied() {
        current.pointer()
    } else {
        RowPtr::NULL
    };
    // SAFETY: `row` is a pinned row being inserted; nothing else accesses it.
    unsafe { store_pointer(next, row, pointer_offset) };
    entry.store(desired.0, Ordering::Relaxed);
    None
}

/// Per-thread scratch space of probes (DuckDB's `ProbeState`).
#[derive(Debug, Default)]
pub struct ProbeState {
    hashes_dense: Vec<u64>,
    /// The entry each row's probe starts at.
    first_entries: Vec<u64>,
    ht_offsets_and_salts: Vec<u64>,
    keys_to_compare: Vec<u32>,
    keys_no_match: Vec<u32>,
}

impl ProbeState {
    /// Queues probe row `row_index` for a key comparison with `entry`'s row
    /// (`AddPointerToCompare`).
    #[inline]
    fn add_pointer_to_compare(&mut self, scan: &mut ScanStructure, entry: HtEntry, offset: u64, row_index: u32) {
        self.keys_to_compare.push(row_index);
        scan.pointers[row_index as usize] = entry.pointer();
        self.ht_offsets_and_salts[row_index as usize] = offset | entry.salt_with_nulls();
    }
}

/// Position of a scan over a finalized table's rows (DuckDB's `JoinHTScanState`).
#[derive(Debug)]
pub struct FullOuterScanState {
    scan: ScanState,
    chunk: usize,
    chunk_end: usize,
    offset_in_chunk: usize,
}

/// The state of probing one input chunk, which may produce several output chunks
/// (DuckDB's `JoinHashTable::ScanStructure`).
#[derive(Debug)]
pub struct ScanStructure {
    /// The current chain row of every probe row.
    pointers: Vec<RowPtr>,
    /// Number of probe rows still walking a chain.
    count: usize,
    /// The probe rows still walking a chain.
    sel_vector: Vec<u32>,
    chain_match_sel: Vec<u32>,
    chain_no_match_sel: Vec<u32>,
    found_match: Vec<bool>,
    finished: bool,
    /// Whether no probe is in progress.
    pub is_null: bool,
    has_null_value_filter: bool,
    /// Build rows of the buffered matches (the compaction buffer).
    rhs_pointers: Vec<RowPtr>,
    /// Probe rows of the buffered matches.
    lhs_sel: Vec<u32>,
    /// Matches found but not emitted because the output was full.
    last_match_count: usize,
    last_sel: Vec<u32>,
    /// Reused buffer for advancing chains.
    advance_scratch: Vec<u32>,
}

impl Default for ScanStructure {
    fn default() -> Self {
        Self {
            pointers: vec![RowPtr::NULL; STANDARD_VECTOR_SIZE],
            count: 0,
            sel_vector: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            chain_match_sel: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            chain_no_match_sel: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            found_match: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            finished: false,
            is_null: true,
            has_null_value_filter: false,
            rhs_pointers: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            lhs_sel: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            last_match_count: 0,
            last_sel: Vec::new(),
            advance_scratch: Vec::with_capacity(STANDARD_VECTOR_SIZE),
        }
    }
}

impl ScanStructure {
    /// Whether every chain is exhausted (`PointersExhausted`).
    pub fn pointers_exhausted(&self) -> bool {
        self.count == 0
    }

    /// Produces the next output chunk for the probe `keys` of `left` (`Next`). `result`
    /// is left empty when this probe has no more output.
    pub fn next(
        &mut self,
        ht: &JoinHashTable,
        keys: &DataChunk,
        left: &DataChunk,
        result_types: &[LogicalType],
        result: &mut DataChunk,
    ) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        let formats = ChunkFormat::new(keys)?;
        match ht.join_type {
            JoinType::Inner | JoinType::Right => self.next_inner_join(ht, &formats, left, result_types, result),
            JoinType::Semi => {
                self.scan_key_matches(ht, &formats)?;
                join::construct_semi_or_anti_join_result(left, result, &self.found_match, true);
                self.finished = true;
                Ok(())
            }
            JoinType::Anti => {
                self.scan_key_matches(ht, &formats)?;
                join::construct_semi_or_anti_join_result(left, result, &self.found_match, false);
                self.finished = true;
                Ok(())
            }
            JoinType::Mark => self.next_mark_join(ht, &formats, keys, left, result),
            JoinType::RightSemi | JoinType::RightAnti => self.next_right_semi_or_anti_join(ht, &formats),
            JoinType::Outer | JoinType::Left => self.next_left_join(ht, &formats, left, result_types, result),
            JoinType::Single => self.next_single_join(ht, &formats, left, result_types, result),
            JoinType::Invalid => Err(Error::internal("Unhandled join type in JoinHashTable")),
        }
    }

    /// Resolves the non-equality conditions for the current chain rows: their matches go
    /// to `chain_match_sel`, the rest (if `with_no_match`) to `chain_no_match_sel`
    /// (`ResolvePredicates`).
    fn resolve_predicates(
        &mut self,
        ht: &JoinHashTable,
        keys: &ChunkFormat<'_>,
        with_no_match: bool,
    ) -> Result<usize> {
        self.chain_match_sel.clear();
        self.chain_match_sel.extend_from_slice(&self.sel_vector[..self.count]);
        self.chain_no_match_sel.clear();
        let Some(matcher) = &ht.row_matcher_probe else {
            return Ok(self.count);
        };
        let no_match = if with_no_match {
            Some(&mut self.chain_no_match_sel)
        } else {
            None
        };
        // SAFETY: the chain pointers of the selected rows are pinned rows of `ht`.
        let matched = unsafe {
            matcher.match_rows(
                &ht.layout,
                keys,
                &mut self.chain_match_sel,
                self.count,
                &self.pointers,
                no_match,
            )?
        };
        self.chain_match_sel.truncate(matched);
        Ok(matched)
    }

    /// Finds the next chain rows with a match, advancing chains until some are found
    /// (`ScanInnerJoin`). The matches are in `chain_match_sel`.
    fn scan_inner_join(&mut self, ht: &JoinHashTable, keys: &ChunkFormat<'_>) -> Result<usize> {
        loop {
            let result_count = self.resolve_predicates(ht, keys, false)?;
            for &row in &self.chain_match_sel {
                self.found_match[row as usize] = true;
            }
            if result_count > 0 {
                return Ok(result_count);
            }
            self.advance_pointers(ht);
            if self.count == 0 {
                return Ok(0);
            }
        }
    }

    /// Moves the chains of `sel_vector[..count]` one row on (`AdvancePointers()`).
    fn advance_pointers(&mut self, ht: &JoinHashTable) {
        let rows = std::mem::take(&mut self.sel_vector);
        let count = self.count;
        self.advance_pointers_of(ht, &rows[..count]);
        self.advance_scratch = rows;
    }

    /// Moves the chains of `rows` one row on; those not at their end become the new
    /// `sel_vector` (`AdvancePointers(sel, count)`).
    fn advance_pointers_of(&mut self, ht: &JoinHashTable, rows: &[u32]) {
        if !ht.chains_longer_than_one {
            self.count = 0;
            return;
        }
        let mut next = std::mem::take(&mut self.advance_scratch);
        next.clear();
        for &row in rows {
            let pointer = &mut self.pointers[row as usize];
            // SAFETY: chain pointers are pinned rows of `ht` (or its dead end), whose
            // pointer column holds the next row of the chain.
            *pointer = unsafe { load_pointer(*pointer, ht.pointer_offset) };
            if !pointer.is_null() {
                next.push(row);
            }
        }
        self.count = next.len();
        self.advance_scratch = std::mem::replace(&mut self.sel_vector, next);
    }

    /// Gathers the build output columns for the probe rows `scan_sel` (or the buffered
    /// matches) into columns `left.column_count()..` of the result.
    fn gather_build_columns(
        &self,
        ht: &JoinHashTable,
        rows: &[RowPtr],
        scan_sel: Option<&[u32]>,
        count: usize,
        result_types: &[LogicalType],
        left_columns: usize,
        columns: &mut Vec<Vector>,
    ) -> Result<()> {
        for (index, &column) in ht.output_columns.iter().enumerate() {
            let mut vector = Vector::flat(result_types[left_columns + index].clone(), count);
            // SAFETY: the selected rows are pinned rows of `ht`.
            unsafe { ht.gather(rows, scan_sel, count, column, &mut vector, None)? };
            columns.push(vector);
        }
        Ok(())
    }

    /// INNER and RIGHT joins: emits matching pairs, one chain step for all probe rows at
    /// a time, buffering up to a full chunk (`NextInnerJoin`).
    fn next_inner_join(
        &mut self,
        ht: &JoinHashTable,
        keys: &ChunkFormat<'_>,
        left: &DataChunk,
        result_types: &[LogicalType],
        result: &mut DataChunk,
    ) -> Result<()> {
        let emits = !matches!(ht.join_type, JoinType::RightSemi | JoinType::RightAnti);
        let mut base_count = 0;
        self.lhs_sel.clear();
        self.rhs_pointers.clear();
        while self.count > 0 {
            let result_count = if self.last_match_count == 0 {
                self.scan_inner_join(ht, keys)?
            } else {
                std::mem::swap(&mut self.chain_match_sel, &mut self.last_sel);
                std::mem::replace(&mut self.last_match_count, 0)
            };
            if result_count > 0 {
                if base_count + result_count > STANDARD_VECTOR_SIZE {
                    // The output is full: keep these matches for the next call.
                    std::mem::swap(&mut self.last_sel, &mut self.chain_match_sel);
                    self.last_match_count = result_count;
                    break;
                }
                if propagates_build_side(ht.join_type) {
                    for &row in &self.chain_match_sel {
                        // SAFETY: matched chain rows are pinned rows with a found flag.
                        unsafe { found_flag(self.pointers[row as usize], ht.tuple_size) }
                            .store(1, Ordering::Relaxed);
                    }
                }
                if emits {
                    if !ht.chains_longer_than_one {
                        // Every key has one row: emit the matches directly.
                        let mut columns = join::slice_columns(left, &self.chain_match_sel);
                        self.gather_build_columns(
                            ht,
                            &self.pointers,
                            Some(&self.chain_match_sel),
                            result_count,
                            result_types,
                            left.column_count(),
                            &mut columns,
                        )?;
                        result.set_columns(columns, result_count);
                        self.advance_pointers(ht);
                        return Ok(());
                    }
                    // Buffer the matches (`UpdateCompactionBuffer`).
                    for &row in &self.chain_match_sel {
                        self.lhs_sel.push(row);
                        self.rhs_pointers.push(self.pointers[row as usize]);
                    }
                    base_count += result_count;
                }
            }
            self.advance_pointers(ht);
        }
        if base_count > 0 {
            let mut columns = join::slice_columns(left, &self.lhs_sel);
            self.gather_build_columns(
                ht,
                &self.rhs_pointers,
                None,
                base_count,
                result_types,
                left.column_count(),
                &mut columns,
            )?;
            result.set_columns(columns, base_count);
        }
        Ok(())
    }

    /// Walks the chains until every probe row found a match or ran out, setting
    /// `found_match` (`ScanKeyMatches`).
    fn scan_key_matches(&mut self, ht: &JoinHashTable, keys: &ChunkFormat<'_>) -> Result<()> {
        while self.count > 0 {
            let match_count = self.resolve_predicates(ht, keys, true)?;
            for &row in &self.chain_match_sel[..match_count] {
                self.found_match[row as usize] = true;
            }
            let no_match = std::mem::take(&mut self.chain_no_match_sel);
            self.advance_pointers_of(ht, &no_match);
            self.chain_no_match_sel = no_match;
        }
        Ok(())
    }

    /// RIGHT SEMI and RIGHT ANTI joins: marks the matched build rows; they are emitted by
    /// the final scan (`NextRightSemiOrAntiJoin`).
    fn next_right_semi_or_anti_join(&mut self, ht: &JoinHashTable, keys: &ChunkFormat<'_>) -> Result<()> {
        let dead_end = RowPtr::from_ptr(ht.dead_end.as_ptr().cast::<u8>().cast_mut());
        while !self.pointers_exhausted() {
            let result_count = self.resolve_predicates(ht, keys, false)?;
            if ht.row_matcher_probe.is_none() {
                // Equality only: the whole chain matches.
                for i in 0..result_count {
                    let row = self.chain_match_sel[i] as usize;
                    let mut pointer = self.pointers[row];
                    // SAFETY: chain rows are pinned rows of `ht` with a found flag and a
                    // next pointer.
                    unsafe {
                        if found_flag(pointer, ht.tuple_size).load(Ordering::Relaxed) != 0 {
                            // Already marked by an earlier probe: stop this chain.
                            self.pointers[row] = dead_end;
                            continue;
                        }
                        loop {
                            found_flag(pointer, ht.tuple_size).store(1, Ordering::Relaxed);
                            let next = load_pointer(pointer, ht.pointer_offset);
                            if next.is_null() {
                                break;
                            }
                            pointer = next;
                        }
                    }
                    self.pointers[row] = pointer;
                }
            } else {
                for &row in &self.chain_match_sel {
                    // SAFETY: matched chain rows are pinned rows with a found flag.
                    unsafe { found_flag(self.pointers[row as usize], ht.tuple_size) }
                        .store(1, Ordering::Relaxed);
                }
            }
            self.advance_pointers(ht);
        }
        self.finished = true;
        Ok(())
    }

    /// MARK joins: `left` with whether each row found a match
    /// (`NextMarkJoin` / `ConstructMarkJoinResult`).
    fn next_mark_join(
        &mut self,
        ht: &JoinHashTable,
        formats: &ChunkFormat<'_>,
        keys: &DataChunk,
        left: &DataChunk,
        result: &mut DataChunk,
    ) -> Result<()> {
        self.scan_key_matches(ht, formats)?;
        self.finished = true;
        let count = left.len();
        let Some(info) = &ht.correlated else {
            // NULL where a NULL-rejecting key is NULL; NULL instead of false if the build
            // side has NULLs.
            let mut validity = ValidityMask::all_valid();
            for (column, vector) in keys.columns().iter().enumerate() {
                if ht.null_values_are_equal[column] {
                    continue;
                }
                let keys_validity = vector.validity();
                if keys_validity.is_all_valid() {
                    continue;
                }
                let sel = vector.sel();
                for row in 0..keys.len() {
                    if !keys_validity.is_valid(sel.slot(row)) {
                        validity.set_invalid(row, count);
                    }
                }
            }
            let mut mark = Vector::flat(LogicalType::BOOLEAN, count.max(1));
            mark.values_mut::<bool>()[..count].copy_from_slice(&self.found_match[..count]);
            if ht.has_null {
                for row in 0..count {
                    if !self.found_match[row] {
                        validity.set_invalid(row, count);
                    }
                }
            }
            mark.set_validity(validity);
            let mut columns = left.columns().to_vec();
            columns.push(mark);
            result.set_columns(columns, count);
            return Ok(());
        };

        // Correlated: look up COUNT(*) and COUNT(key) of each row's group.
        let (groups, hashes) = info.groups(keys)?;
        let counts = info
            .correlated_counts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .fetch_aggregates(&groups, &hashes)?;
        let (count_star, count_key) = (counts[0].values::<i64>(), counts[1].values::<i64>());
        let last_key = keys.column(keys.column_count() - 1);
        let mut validity = ValidityMask::all_valid();
        {
            let key_validity = last_key.validity();
            if !key_validity.is_all_valid() {
                let sel = last_key.sel();
                for row in 0..count {
                    if !key_validity.is_valid(sel.slot(row)) {
                        validity.set_invalid(row, count);
                    }
                }
            }
        }
        let mut mark = Vector::flat(LogicalType::BOOLEAN, count.max(1));
        let values = mark.values_mut::<bool>();
        for row in 0..count {
            values[row] = self.found_match[row];
            if !values[row] && count_star[row] > count_key[row] {
                // The group has NULLs and no match: NULL.
                validity.set_invalid(row, count);
            }
            if count_star[row] == 0 {
                // An empty group: the result is false.
                validity.set_valid(row);
            }
        }
        mark.set_validity(validity);
        let mut columns = left.columns().to_vec();
        columns.push(mark);
        result.set_columns(columns, count);
        Ok(())
    }

    /// LEFT and FULL joins: the inner join matches, then the unmatched probe rows with
    /// NULLs (`NextLeftJoin`).
    fn next_left_join(
        &mut self,
        ht: &JoinHashTable,
        keys: &ChunkFormat<'_>,
        left: &DataChunk,
        result_types: &[LogicalType],
        result: &mut DataChunk,
    ) -> Result<()> {
        self.next_inner_join(ht, keys, left, result_types, result)?;
        if !result.is_empty() {
            return Ok(());
        }
        let remaining: Vec<u32> = (0..left.len() as u32)
            .filter(|&row| !self.found_match[row as usize])
            .collect();
        if !remaining.is_empty() {
            let len = remaining.len();
            let mut columns = join::slice_columns(left, &remaining);
            columns.extend(join::null_columns(&result_types[left.column_count()..]));
            result.set_columns(columns, len);
        }
        self.finished = true;
        Ok(())
    }

    /// SINGLE joins: every probe row with its match or NULLs; more than one match is an
    /// error unless disabled (`NextSingleJoin`).
    fn next_single_join(
        &mut self,
        ht: &JoinHashTable,
        keys: &ChunkFormat<'_>,
        left: &DataChunk,
        result_types: &[LogicalType],
        result: &mut DataChunk,
    ) -> Result<()> {
        let mut result_sel: Vec<u32> = Vec::with_capacity(STANDARD_VECTOR_SIZE);
        while self.count > 0 {
            let match_count = self.resolve_predicates(ht, keys, true)?;
            for &row in &self.chain_match_sel[..match_count] {
                self.found_match[row as usize] = true;
                result_sel.push(row);
            }
            let no_match = std::mem::take(&mut self.chain_no_match_sel);
            self.advance_pointers_of(ht, &no_match);
            self.chain_no_match_sel = no_match;
        }
        let count = left.len();
        let mut columns = left.columns().to_vec();
        for (index, &column) in ht.output_columns.iter().enumerate() {
            let mut vector = Vector::flat(result_types[left.column_count() + index].clone(), count.max(1));
            let mut validity = ValidityMask::all_valid();
            for row in 0..count {
                if !self.found_match[row] {
                    validity.set_invalid(row, count);
                }
            }
            vector.set_validity(validity);
            // SAFETY: the matched chain rows are pinned rows of `ht`.
            unsafe {
                ht.gather(
                    &self.pointers,
                    Some(&result_sel),
                    result_sel.len(),
                    column,
                    &mut vector,
                    Some(&result_sel),
                )?;
            }
            if matches!(vector.logical_type().physical_type(), PhysicalType::Struct | PhysicalType::Array) {
                // NULL nested values have NULL children (`FlatVector::SetNull`).
                let null = Value::null(vector.logical_type().clone());
                for row in (0..count).filter(|&row| !self.found_match[row]) {
                    vector.set_value(row, &null)?;
                }
            }
            columns.push(vector);
        }
        result.set_columns(columns, count);
        self.finished = true;

        if ht.single_join_error_on_multiple_rows && !result_sel.is_empty() {
            // A second match for any row is an error.
            self.advance_pointers_of(ht, &result_sel);
            if self.count > 0 && self.resolve_predicates(ht, keys, false)? > 0 {
                return Err(Error::invalid_input(SCALAR_SUBQUERY_ERROR));
            }
            self.count = 0;
        }
        Ok(())
    }
}
