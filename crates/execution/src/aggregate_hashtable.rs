//! The grouped aggregate hash table (DuckDB's `GroupedAggregateHashTable`).
//!
//! Groups and their aggregate states live as rows in a radix-partitioned
//! [`PartitionedTupleData`]; a separate open-addressing table of 64-bit entries points at
//! them. Each entry packs a 48-bit row pointer with the upper 16 bits of the group's hash
//! (the "salt"), so most probes are resolved without touching the row. Probing is linear
//! with a salt-dependent odd stride, exactly as in DuckDB.

use std::sync::Arc;

use rustdb_buffer::{BufferManager, MemoryTag};
use rustdb_common::{Error, Result};
use rustdb_function::AggregateFunction;
use rustdb_function::FunctionData;
use rustdb_row::{
    ChunkFormat, MatchPredicate, PartitionedAppendState, PartitionedTupleData, PinProperties, RowAggregate,
    RowMatcher, RowOperationsState, RowPtr, TupleDataCollection, TupleDataLayout, ValidityType,
    combine_states, destroy_states, finalize_states, initialize_states, update_states,
};
use rustdb_types::{DataChunk, LogicalType, STANDARD_VECTOR_SIZE, SelectionVector, Vector};

/// Upper 16 bits of an entry: the salt.
const SALT_MASK: u64 = 0xFFFF_0000_0000_0000;
/// Lower 48 bits of an entry: the row pointer.
const POINTER_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
/// Entries per group the table keeps free before resizing (`LOAD_FACTOR`).
const LOAD_FACTOR: f64 = 1.5;

/// The salt of a hash, with all pointer bits set (`ht_entry_t::ExtractSalt`).
const fn extract_salt(hash: u64) -> u64 {
    hash | POINTER_MASK
}

/// Advances a probe position by an odd, salt-derived stride (`SaltIncrementAndWrap`).
const fn next_offset(offset: u64, salt: u64, bitmask: u64) -> u64 {
    const INCREMENT_BITS: u32 = 5;
    (offset + ((salt >> (64 - INCREMENT_BITS)) | 1)) & bitmask
}

/// The number of table entries for `count` groups (`GetCapacityForCount`).
pub fn capacity_for_count(count: usize) -> usize {
    let count = count.max(initial_capacity());
    ((count as f64 * LOAD_FACTOR) as usize).next_power_of_two()
}

/// `InitialCapacity`
pub const fn initial_capacity() -> usize {
    STANDARD_VECTOR_SIZE * 2
}

/// An aggregate to compute per group: its function, bind data and how many input
/// columns it consumes.
#[derive(Clone, Debug)]
pub struct AggregateObject {
    pub function: AggregateFunction,
    pub bind_data: Option<Arc<dyn FunctionData>>,
    pub child_count: usize,
}

/// A hash table mapping groups to aggregate states.
#[derive(Debug)]
pub struct GroupedAggregateHashTable {
    layout: Arc<TupleDataLayout>,
    group_types: Vec<LogicalType>,
    partitioned: PartitionedTupleData,
    append: PartitionedAppendState,
    entries: Vec<u64>,
    bitmask: u64,
    count: usize,
    matcher: RowMatcher,
    row_state: RowOperationsState,
    /// Row of the group of each input row of the last `find_or_create_groups`.
    addresses: Vec<RowPtr>,
    ht_offsets: Vec<u64>,
    salts: Vec<u64>,
    /// Whether rows still hold aggregate states that need destroying.
    states_live: bool,
}

impl GroupedAggregateHashTable {
    /// A table for groups of `group_types`, partitioned into `2^radix_bits` partitions.
    pub fn new(
        buffer_manager: BufferManager,
        group_types: Vec<LogicalType>,
        aggregates: &[AggregateObject],
        capacity: usize,
        radix_bits: usize,
    ) -> Result<Self> {
        let mut layout_types = group_types.clone();
        layout_types.push(LogicalType::UBIGINT); // the group hash
        let row_aggregates = aggregates
            .iter()
            .map(|aggregate| {
                let state = aggregate.function.ops.state_layout();
                RowAggregate {
                    function: aggregate.function.clone(),
                    bind_data: aggregate.bind_data.clone(),
                    child_count: aggregate.child_count,
                    payload_size: state.size().next_multiple_of(state.align().max(1)),
                    alignment: state.align(),
                }
            })
            .collect();
        let layout = Arc::new(TupleDataLayout::with_aggregates(
            layout_types,
            row_aggregates,
            ValidityType::CanHaveNull,
        )?);
        let hash_column = group_types.len();
        let mut partitioned = PartitionedTupleData::new(
            buffer_manager,
            Arc::clone(&layout),
            MemoryTag::HashTable,
            radix_bits,
            hash_column,
        )?;
        let append = partitioned.initialize_append_state(PinProperties::KeepEverythingPinned);
        let predicate = if layout.all_valid() {
            MatchPredicate::Equal
        } else {
            MatchPredicate::NotDistinctFrom
        };
        let matcher = RowMatcher::new(&layout, &vec![predicate; group_types.len()], None)?;
        let mut table = Self {
            layout,
            group_types,
            partitioned,
            append,
            entries: Vec::new(),
            bitmask: 0,
            count: 0,
            matcher,
            row_state: RowOperationsState::default(),
            addresses: Vec::new(),
            ht_offsets: Vec::new(),
            salts: Vec::new(),
            states_live: false,
        };
        table.resize(capacity.max(initial_capacity()).next_power_of_two())?;
        Ok(table)
    }

    pub fn layout(&self) -> &Arc<TupleDataLayout> {
        &self.layout
    }

    /// Number of groups.
    pub fn count(&self) -> usize {
        self.count
    }

    pub fn capacity(&self) -> usize {
        self.entries.len()
    }

    fn resize_threshold(&self) -> usize {
        (self.capacity() as f64 / LOAD_FACTOR) as usize
    }

    /// Grows the pointer table to `capacity` entries and reinserts every group (`Resize`).
    fn resize(&mut self, capacity: usize) -> Result<()> {
        if self.count != 0 && capacity < self.capacity() {
            return Err(Error::internal("Cannot downsize a non-empty hash table!"));
        }
        self.entries = vec![0; capacity];
        self.bitmask = capacity as u64 - 1;
        if self.count != 0 {
            self.reinsert_rows()?;
        }
        Ok(())
    }

    fn reinsert_rows(&mut self) -> Result<()> {
        let hash_offset = self.layout.offsets()[self.group_types.len()];
        for partition in self.partitioned.partitions() {
            let mut scan = partition.initialize_scan_columns(Vec::new(), PinProperties::AlreadyPinned);
            let mut chunk = DataChunk::default();
            while partition.scan(&mut scan, &mut chunk)? {
                for &row in &scan.chunk_state.row_locations()[..chunk.len()] {
                    // SAFETY: rows of this table are pinned (KeepEverythingPinned) and the
                    // hash column holds a u64 at `hash_offset`.
                    let hash = unsafe { row.as_ptr().add(hash_offset).cast::<u64>().read_unaligned() };
                    let salt = extract_salt(hash);
                    let mut offset = hash & self.bitmask;
                    while self.entries[offset as usize] != 0 {
                        offset = next_offset(offset, salt, self.bitmask);
                    }
                    self.entries[offset as usize] = (salt & SALT_MASK) | (row.as_ptr() as u64 & POINTER_MASK);
                }
            }
        }
        Ok(())
    }

    /// Finds the group of every row of `groups` (with precomputed `hashes`), creating
    /// missing groups with fresh aggregate states. Afterwards `addresses()[i]` is the
    /// row of input row `i`'s group. Returns the number of new groups
    /// (`FindOrCreateGroupsInternal`).
    pub fn find_or_create_groups(&mut self, groups: &DataChunk, hashes: &Vector) -> Result<usize> {
        let count = groups.len();
        if count == 0 {
            return Ok(0);
        }
        if self.count + count > self.capacity() || self.count + count > self.resize_threshold() {
            self.resize(self.capacity() * 2)?;
        }
        let mut columns = groups.columns().to_vec();
        columns.push(hashes.clone());
        let group_chunk = DataChunk::new(columns, count);
        let formats = ChunkFormat::new(&group_chunk)?;

        let hash_values = hashes.unified::<u64>();
        self.ht_offsets.clear();
        self.salts.clear();
        for row in 0..count {
            let hash = hash_values.values[hash_values.slot(row)];
            self.ht_offsets.push(hash & self.bitmask);
            self.salts.push(extract_salt(hash));
        }
        self.addresses.clear();
        self.addresses.resize(count, RowPtr::NULL);

        let mut remaining: Vec<u32> = (0..count as u32).collect();
        let mut new_groups = 0;
        let mut empty = Vec::with_capacity(count);
        let mut compare = Vec::with_capacity(count);
        let mut no_match = Vec::new();
        let mut iterations = 0;
        while !remaining.is_empty() {
            iterations += 1;
            if iterations > self.capacity() {
                return Err(Error::internal(
                    "Maximum outer iteration count reached in GroupedAggregateHashTable",
                ));
            }
            empty.clear();
            compare.clear();
            for &index in &remaining {
                let index = index as usize;
                let salt = self.salts[index];
                let mut offset = self.ht_offsets[index];
                loop {
                    let entry = &mut self.entries[offset as usize];
                    if *entry == 0 {
                        // Unoccupied: claim it with the salt; the pointer follows below.
                        *entry = salt;
                        empty.push(index as u32);
                        break;
                    }
                    if (*entry | POINTER_MASK) == salt {
                        compare.push(index as u32);
                        break;
                    }
                    offset = next_offset(offset, salt, self.bitmask);
                }
                self.ht_offsets[index] = offset;
            }
            if !empty.is_empty() {
                self.append_new_groups(&formats, &empty)?;
                new_groups += empty.len();
            }
            no_match.clear();
            if !compare.is_empty() {
                for &index in &compare {
                    let entry = self.entries[self.ht_offsets[index as usize] as usize];
                    self.addresses[index as usize] = RowPtr::from_ptr((entry & POINTER_MASK) as *mut u8);
                }
                let compare_count = compare.len();
                // SAFETY: every compared address is a pinned row of this table's layout.
                unsafe {
                    self.matcher.match_rows(
                        &self.layout,
                        &formats,
                        &mut compare,
                        compare_count,
                        &self.addresses,
                        Some(&mut no_match),
                    )?;
                }
            }
            for &index in &no_match {
                let index = index as usize;
                self.ht_offsets[index] = next_offset(self.ht_offsets[index], self.salts[index], self.bitmask);
            }
            std::mem::swap(&mut remaining, &mut no_match);
        }
        self.count += new_groups;
        Ok(new_groups)
    }

    /// Appends the groups of input rows `rows` and points their claimed entries at them.
    fn append_new_groups(&mut self, formats: &ChunkFormat<'_>, rows: &[u32]) -> Result<()> {
        self.partitioned
            .append_formatted(&mut self.append, formats, Some(rows), rows.len())?;
        let locations = self.append.chunk_state.row_locations();
        // SAFETY: the rows were just appended and are pinned; their states are fresh.
        unsafe { initialize_states(&self.layout, locations, None, rows.len())? };
        self.states_live = true;
        let reverse = self.append.reverse_partition_sel();
        for &index in rows {
            let location = locations[reverse[index as usize] as usize];
            let entry = &mut self.entries[self.ht_offsets[index as usize] as usize];
            *entry = (*entry & SALT_MASK) | (location.as_ptr() as u64 & POINTER_MASK);
            self.addresses[index as usize] = location;
        }
        Ok(())
    }

    /// Adds a chunk: finds or creates the groups, then updates aggregate `i` with its
    /// `inputs[i]` columns for the rows selected by `selections[i]` (all rows when `None`,
    /// e.g. without a `FILTER`). Returns the number of new groups (`AddChunk`).
    pub fn add_chunk(
        &mut self,
        groups: &DataChunk,
        hashes: &Vector,
        inputs: &[Vec<Vector>],
        selections: &[Option<Vec<u32>>],
    ) -> Result<usize> {
        let new_groups = self.find_or_create_groups(groups, hashes)?;
        for (index, columns) in inputs.iter().enumerate() {
            match selections.get(index).and_then(Option::as_ref) {
                None => {
                    // SAFETY: the addresses are pinned rows of this layout with live states.
                    unsafe {
                        update_states(
                            &mut self.row_state,
                            &self.layout,
                            index,
                            &self.addresses,
                            columns,
                            groups.len(),
                        )?;
                    }
                }
                Some(selection) if !selection.is_empty() => {
                    let rows: Vec<RowPtr> = selection.iter().map(|&i| self.addresses[i as usize]).collect();
                    let sel = SelectionVector::new(selection.clone());
                    let sliced: Vec<Vector> = columns.iter().map(|column| column.slice(&sel)).collect();
                    // SAFETY: as above.
                    unsafe {
                        update_states(
                            &mut self.row_state,
                            &self.layout,
                            index,
                            &rows,
                            &sliced,
                            rows.len(),
                        )?;
                    }
                }
                Some(_) => {}
            }
        }
        Ok(new_groups)
    }

    /// Merges every group of `other` (a collection with this table's layout) into this
    /// table, consuming `other`'s states (`Combine`).
    pub fn combine(&mut self, other: &TupleDataCollection) -> Result<()> {
        if other.count() == 0 {
            return Ok(());
        }
        let group_count = self.group_types.len();
        let columns: Vec<usize> = (0..=group_count).collect();
        let mut scan = other.initialize_scan_columns(columns, PinProperties::DestroyAfterDone);
        let mut chunk = DataChunk::default();
        while other.scan(&mut scan, &mut chunk)? {
            let count = chunk.len();
            let mut columns = chunk.columns().to_vec();
            let hashes = columns
                .pop()
                .ok_or_else(|| Error::internal("combine scan without hash column"))?;
            let groups = DataChunk::new(columns, count);
            self.find_or_create_groups(&groups, &hashes)?;
            let sources = scan.chunk_state.row_locations();
            // SAFETY: sources are pinned rows of `other` (same layout) with live states;
            // targets are pinned rows of this table.
            unsafe {
                combine_states(&mut self.row_state, &self.layout, sources, &self.addresses, count)?;
                if self.layout.has_destructor() {
                    destroy_states(&mut self.row_state, &self.layout, sources, count)?;
                }
            }
        }
        Ok(())
    }

    /// Takes the partitioned groups (with their states) out of the table, leaving it empty.
    pub fn take_partitioned_data(&mut self) -> Result<PartitionedTupleData> {
        self.partitioned.flush_append_state(&mut self.append);
        let empty = self.partitioned.with_radix_bits(self.partitioned.radix_bits())?;
        let data = std::mem::replace(&mut self.partitioned, empty);
        self.append = self
            .partitioned
            .initialize_append_state(PinProperties::KeepEverythingPinned);
        self.entries.fill(0);
        self.count = 0;
        self.states_live = false;
        Ok(data)
    }

    /// Emits every group with its finalized aggregates: group columns first, then one
    /// column per aggregate. States are destroyed as they are finalized.
    pub fn finalize_into(&mut self, mut emit: impl FnMut(DataChunk) -> Result<()>) -> Result<()> {
        self.partitioned.flush_append_state(&mut self.append);
        let group_count = self.group_types.len();
        let aggregate_types: Vec<LogicalType> = self
            .layout
            .aggregates()
            .iter()
            .map(|a| a.function.return_type.clone())
            .collect();
        for partition in self.partitioned.partitions() {
            let mut scan =
                partition.initialize_scan_columns((0..group_count).collect(), PinProperties::AlreadyPinned);
            let mut groups = DataChunk::default();
            while partition.scan(&mut scan, &mut groups)? {
                let count = groups.len();
                let rows = scan.chunk_state.row_locations();
                let mut results: Vec<Vector> = aggregate_types
                    .iter()
                    .map(|ty| Vector::flat(ty.clone(), count))
                    .collect();
                // SAFETY: the rows are pinned rows of this table with live states.
                unsafe {
                    finalize_states(&mut self.row_state, &self.layout, rows, count, &mut results, 0)?;
                    if self.layout.has_destructor() {
                        destroy_states(&mut self.row_state, &self.layout, rows, count)?;
                    }
                }
                let mut columns = groups.columns().to_vec();
                columns.extend(results);
                emit(DataChunk::new(columns, count))?;
            }
        }
        self.states_live = false;
        Ok(())
    }
}

impl Drop for GroupedAggregateHashTable {
    fn drop(&mut self) {
        if !self.states_live || !self.layout.has_destructor() {
            return;
        }
        for partition in self.partitioned.partitions() {
            let mut scan = partition.initialize_scan_columns(Vec::new(), PinProperties::AlreadyPinned);
            let mut chunk = DataChunk::default();
            while let Ok(true) = partition.scan(&mut scan, &mut chunk) {
                let rows = &scan.chunk_state.row_locations()[..chunk.len()];
                // SAFETY: the rows hold states that were initialised and not yet destroyed.
                let _ = unsafe { destroy_states(&mut self.row_state, &self.layout, rows, rows.len()) };
            }
        }
    }
}
