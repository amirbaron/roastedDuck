//! The grouped aggregate hash table (DuckDB's `GroupedAggregateHashTable`).
//!
//! Groups and their aggregate states live as rows in a radix-partitioned
//! [`PartitionedTupleData`]; a separate open-addressing table of 64-bit entries points at
//! them. Each entry packs a 48-bit row pointer with the upper 16 bits of the group's hash
//! (the "salt"), so most probes are resolved without touching the row. Probing is linear
//! with a salt-dependent odd stride, exactly as in DuckDB.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use rustdb_buffer::{AllocatedData, BufferManager, MemoryTag};
use rustdb_common::{Error, Result};
use rustdb_function::AggregateFunction;
use rustdb_function::FunctionData;
use rustdb_row::{
    ChunkFormat, GroupedUpdate, MatchPredicate, PartitionedAppendState, PartitionedTupleData, PinProperties,
    RowAggregate, RowMatcher, RowOperationsState, RowPtr, TupleDataCollection, TupleDataLayout, ValidityType,
    combine_states, destroy_states, finalize_states, initialize_states, supports_update_selection,
    update_states,
};
use rustdb_storage::statistics::{HLL_REGISTERS, HyperLogLog};
use rustdb_types::{
    DataChunk, DictionaryInfo, LogicalType, STANDARD_VECTOR_SIZE, Sel, SelectionVector, Vector,
};

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

/// Finds the entry of each row in `remaining` (DuckDB's
/// `GroupedAggregateHashTableInnerLoop`): rows whose probe reaches an empty entry claim it
/// with their salt and go to `empty`; rows reaching an entry with their salt go to
/// `compare`. `offsets[row]` is left at the entry found.
///
/// The slices are locals so that their lengths stay in registers, and every probe position
/// is masked, which lets the compiler drop the bounds checks.
#[inline]
fn probe(
    entries: &mut [u64],
    bitmask: u64,
    salts: &[u64],
    offsets: &mut [u64],
    remaining: &[u32],
    empty: &mut Vec<u32>,
    compare: &mut Vec<u32>,
) {
    let mask = (entries.len() - 1) & usize::try_from(bitmask).unwrap_or(usize::MAX);
    // Written by index like DuckDB's selection vectors, into the spare capacity.
    empty.clear();
    compare.clear();
    empty.reserve(remaining.len());
    compare.reserve(remaining.len());
    let empty_out = &mut empty.spare_capacity_mut()[..remaining.len()];
    let compare_out = &mut compare.spare_capacity_mut()[..remaining.len()];
    let (mut empty_count, mut compare_count) = (0, 0);
    for &index in remaining {
        let row = index as usize;
        let salt = salts[row];
        let mut offset = offsets[row] as usize & mask;
        loop {
            let entry = &mut entries[offset & mask];
            if *entry == 0 {
                // Unoccupied: claim it with the salt; the pointer follows later.
                *entry = salt;
                empty_out[empty_count].write(index);
                empty_count += 1;
                break;
            }
            if (*entry | POINTER_MASK) == salt {
                compare_out[compare_count].write(index);
                compare_count += 1;
                break;
            }
            offset = next_offset(offset as u64, salt, mask as u64) as usize;
        }
        offsets[row] = offset as u64;
    }
    // SAFETY: every row goes to exactly one output, at its count, so the first
    // `empty_count` / `compare_count` spare slots were written.
    unsafe {
        empty.set_len(empty_count);
        compare.set_len(compare_count);
    }
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

/// The number of groups a pointer table of `capacity` entries takes before it grows
/// (`ResizeThreshold`).
pub fn resize_threshold(capacity: usize) -> usize {
    (capacity as f64 / LOAD_FACTOR) as usize
}

/// An aggregate to compute per group: its function, bind data and how many input
/// columns it consumes.
#[derive(Clone, Debug)]
pub struct AggregateObject {
    pub function: AggregateFunction,
    pub bind_data: Option<Arc<dyn FunctionData>>,
    pub child_count: usize,
}

/// Storage dictionaries larger than this are aggregated row by row
/// (`MAX_DICTIONARY_SIZE_THRESHOLD`).
const MAX_DICTIONARY_SIZE: usize = 20_000;

/// The groups of the entries of the storage dictionary seen last (DuckDB's
/// `AggregateDictionaryState`): a dictionary group column is looked up once per distinct
/// entry, not once per row.
#[derive(Debug, Default)]
struct DictionaryGroups {
    /// The dictionary (kept alive, so that its address identifies it).
    dictionary: Option<Arc<DictionaryInfo>>,
    /// The group row of every entry looked up so far (`RowPtr::NULL` otherwise).
    groups: Vec<RowPtr>,
}

/// The entries of a pointer table, zeroed (DuckDB's `hash_map`). A large table is a
/// mapping of its own, so it goes back to the OS when a bigger one replaces it (the process
/// allocator kept every outgrown table). DuckDB charges it to the buffer pool; here, as
/// before, the sinks only count it in their memory estimate (`size_in_bytes`): charging
/// it too takes TPC-H q9 over a 100 MB limit (`external_file_cache_low_mem.test_slow`).
#[derive(Debug, Default)]
struct Entries {
    memory: Option<AllocatedData>,
    len: usize,
}

impl Entries {
    fn zeroed(len: usize) -> Result<Self> {
        let memory = AllocatedData::untracked_zeroed(len * size_of::<u64>())?;
        Ok(Self {
            memory: Some(memory),
            len,
        })
    }
}

impl Deref for Entries {
    type Target = [u64];

    #[allow(
        clippy::cast_ptr_alignment,
        reason = "AllocatedData is 16-byte aligned, more than u64 needs"
    )]
    fn deref(&self) -> &[u64] {
        match &self.memory {
            // SAFETY: the memory holds `len` initialised u64s and lives as long as `self`.
            Some(memory) => unsafe { std::slice::from_raw_parts(memory.as_ptr().cast(), self.len) },
            None => &[],
        }
    }
}

impl DerefMut for Entries {
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "AllocatedData is 16-byte aligned, more than u64 needs"
    )]
    fn deref_mut(&mut self) -> &mut [u64] {
        match &mut self.memory {
            // SAFETY: as in `deref`; `&mut self` makes the access unique.
            Some(memory) => unsafe { std::slice::from_raw_parts_mut(memory.as_ptr().cast(), self.len) },
            None => &mut [],
        }
    }
}

/// A hash table mapping groups to aggregate states.
#[derive(Debug)]
pub struct GroupedAggregateHashTable {
    layout: Arc<TupleDataLayout>,
    group_types: Vec<LogicalType>,
    partitioned: PartitionedTupleData,
    append: PartitionedAppendState,
    entries: Entries,
    bitmask: u64,
    count: usize,
    matcher: RowMatcher,
    row_state: RowOperationsState,
    /// Whether every aggregate can add a selection of rows to one state, so chunks with
    /// few groups are added one group at a time ([`GroupedUpdate`]).
    selection_update: bool,
    grouped_update: GroupedUpdate,
    /// Row of the group of each input row of the last `find_or_create_groups`.
    addresses: Vec<RowPtr>,
    /// Input rows of the last `find_or_create_groups` that created a group, in creation
    /// order (`new_groups_out`).
    new_group_rows: Vec<u32>,
    ht_offsets: Vec<u64>,
    salts: Vec<u64>,
    /// Scratch selections of `find_or_create_groups`: the rows still probing, those that
    /// claimed an empty entry, those to compare with an entry's group, and those that
    /// differed.
    probe_buffers: [Vec<u32>; 4],
    /// Whether rows still hold aggregate states that need destroying.
    states_live: bool,
    /// Rows added with `add_chunk` (`sink_count`).
    sink_count: usize,
    /// Whether new rows are appended without looking up their groups (`skip_lookups`);
    /// the duplicates are merged when the partitions are combined.
    skip_lookups: bool,
    /// Distinct group hashes seen while enabled (`hll` / `enable_hll`).
    hll: Option<HyperLogLog>,
    /// Cached groups of a storage dictionary group column (`dict_state`).
    dictionary_groups: DictionaryGroups,
    /// How often the pointer table started over while the rows were kept (`Abandon`).
    restarts: usize,
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
        Self::with_validity(
            buffer_manager,
            group_types,
            aggregates,
            capacity,
            radix_bits,
            ValidityType::CanHaveNull,
        )
    }

    /// Like [`GroupedAggregateHashTable::new`]; with `ValidityType::CannotHaveNull` the
    /// rows store no validity for the groups, which must then never be NULL, and groups
    /// compare with plain equality (DuckDB's `group_validity`).
    pub fn with_validity(
        buffer_manager: BufferManager,
        group_types: Vec<LogicalType>,
        aggregates: &[AggregateObject],
        capacity: usize,
        radix_bits: usize,
        group_validity: ValidityType,
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
            group_validity,
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
        let selection_update = supports_update_selection(&layout);
        let mut table = Self {
            layout,
            group_types,
            partitioned,
            append,
            entries: Entries::default(),
            bitmask: 0,
            count: 0,
            matcher,
            row_state: RowOperationsState::default(),
            selection_update,
            grouped_update: GroupedUpdate::default(),
            addresses: Vec::new(),
            new_group_rows: Vec::new(),
            ht_offsets: Vec::new(),
            salts: Vec::new(),
            probe_buffers: Default::default(),
            states_live: false,
            sink_count: 0,
            skip_lookups: false,
            hll: None,
            dictionary_groups: DictionaryGroups::default(),
            restarts: 0,
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

    /// Bytes held by the groups and the pointer table.
    pub fn size_in_bytes(&self) -> usize {
        self.partitioned.size_in_bytes() + self.entries.len() * size_of::<u64>()
    }

    /// The input rows of the last `find_or_create_groups` that created a new group.
    pub fn new_group_rows(&self) -> &[u32] {
        &self.new_group_rows
    }

    pub fn capacity(&self) -> usize {
        self.entries.len()
    }

    fn resize_threshold(&self) -> usize {
        resize_threshold(self.capacity())
    }

    /// Rows added with [`GroupedAggregateHashTable::add_chunk`] (`GetSinkCount`).
    pub fn sink_count(&self) -> usize {
        self.sink_count
    }

    /// Groups materialised in the table's rows, including those of earlier pointer tables
    /// (`GetMaterializedCount`).
    pub fn materialized_count(&self) -> usize {
        self.partitioned.count()
    }

    /// Radix bits of the table's partitions.
    pub fn radix_bits(&self) -> usize {
        self.partitioned.radix_bits()
    }

    /// From now on, appends rows without looking up their groups (`SkipLookups`).
    pub fn skip_lookups(&mut self) {
        self.skip_lookups = true;
    }

    /// Whether every group has a single row (no duplicates to merge when combining).
    pub fn is_deduplicated(&self) -> bool {
        !self.skip_lookups && self.restarts == 0
    }

    /// Starts or stops counting the distinct group hashes (`EnableHLL`).
    pub fn enable_hll(&mut self, enable: bool) {
        self.hll = enable.then(|| self.hll.unwrap_or_default());
    }

    /// An upper bound of the distinct groups seen while counting was enabled
    /// (`GetHLLUpperBound`).
    pub fn hll_upper_bound(&self) -> usize {
        // `HyperLogLog::GetErrorRate`: sqrt(pi / 2) / sqrt(M).
        let error_rate = (std::f64::consts::PI / 2.0).sqrt() / (HLL_REGISTERS as f64).sqrt();
        let count = self.hll.map_or(0, |hll| hll.count());
        ((1.0 + error_rate) * count as f64) as usize
    }

    /// Starts a new, empty pointer table while keeping the rows (`Abandon`): groups seen
    /// again get new rows, which are merged with the old ones when the partitions are
    /// combined.
    pub fn abandon(&mut self) {
        self.entries.fill(0);
        self.count = 0;
        self.restarts += 1;
        self.dictionary_groups = DictionaryGroups::default();
    }

    /// Moves the rows into `radix_bits` partitions (`SetRadixBits` + `Repartition`). The
    /// pointer table no longer points at them, so it starts over.
    pub fn repartition(&mut self, radix_bits: usize) -> Result<()> {
        self.partitioned.flush_append_state(&mut self.append);
        let mut target = self.partitioned.with_radix_bits(radix_bits)?;
        self.partitioned.repartition(&mut target)?;
        self.partitioned = target;
        self.append = self
            .partitioned
            .initialize_append_state(PinProperties::KeepEverythingPinned);
        self.abandon();
        Ok(())
    }

    /// Grows (or, when empty, sets) the pointer table to `capacity` entries (`Resize`).
    pub fn resize_to(&mut self, capacity: usize) -> Result<()> {
        self.resize(capacity.max(initial_capacity()).next_power_of_two())
    }

    /// Grows the pointer table to `capacity` entries and reinserts every group (`Resize`).
    fn resize(&mut self, capacity: usize) -> Result<()> {
        if self.count != 0 && capacity < self.capacity() {
            return Err(Error::internal("Cannot downsize a non-empty hash table!"));
        }
        // The groups are reinserted from the rows, so the old table goes before the new one
        // is allocated.
        self.entries = Entries::default();
        self.entries = Entries::zeroed(capacity)?;
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
        if let Some(hll) = &mut self.hll {
            for row in 0..count {
                hll.insert_element(hash_values.values[hash_values.slot(row)]);
            }
        }
        self.addresses.clear();
        self.addresses.resize(count, RowPtr::NULL);
        self.new_group_rows.clear();

        if self.skip_lookups {
            // Just appending: every row becomes a group of its own.
            let rows: Vec<u32> = (0..count as u32).collect();
            self.partitioned
                .append_formatted(&mut self.append, &formats, Some(&rows), count)?;
            let locations = self.append.chunk_state.row_locations();
            // SAFETY: the rows were just appended and are pinned; their states are fresh.
            unsafe { initialize_states(&self.layout, locations, None, count)? };
            self.states_live = true;
            let reverse = self.append.reverse_partition_sel();
            for row in 0..count {
                self.addresses[row] = locations[reverse[row] as usize];
            }
            self.new_group_rows.extend_from_slice(&rows);
            self.count += count;
            return Ok(count);
        }

        self.ht_offsets.resize(count, 0);
        self.salts.resize(count, 0);
        // Looking up every row's first entry in a branchless loop lets the cache misses
        // overlap before the branchy probing loop below (DuckDB's `occupied_count`).
        let mut occupied = 0usize;
        {
            let entries = &self.entries[..];
            let mask = (entries.len() - 1) & usize::try_from(self.bitmask).unwrap_or(usize::MAX);
            let offsets = self.ht_offsets[..count].iter_mut();
            let salts = self.salts[..count].iter_mut();
            let mut lookup = |offset: &mut u64, salt: &mut u64, hash: u64| {
                let position = hash as usize & mask;
                occupied += usize::from(entries[position] != 0);
                *offset = position as u64;
                *salt = extract_salt(hash);
            };
            if let Sel::Identity = hash_values.sel {
                for ((offset, salt), &hash) in offsets.zip(salts).zip(&hash_values.values[..count]) {
                    lookup(offset, salt, hash);
                }
            } else {
                for (row, (offset, salt)) in offsets.zip(salts).enumerate() {
                    lookup(offset, salt, hash_values.values[hash_values.slot(row)]);
                }
            }
        }
        std::hint::black_box(occupied);

        let [mut remaining, mut empty, mut compare, mut no_match] = std::mem::take(&mut self.probe_buffers);
        remaining.clear();
        remaining.extend(0..count as u32);
        let mut new_groups = 0;
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
            probe(
                &mut self.entries,
                self.bitmask,
                &self.salts,
                &mut self.ht_offsets,
                &remaining,
                &mut empty,
                &mut compare,
            );
            if !empty.is_empty() {
                self.append_new_groups(&formats, &empty)?;
                self.new_group_rows.extend_from_slice(&empty);
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
        self.probe_buffers = [remaining, empty, compare, no_match];
        self.count += new_groups;
        Ok(new_groups)
    }

    /// [`Self::find_or_create_groups`] for a single group column that is a dictionary
    /// vector over a storage dictionary (DuckDB's `TryAddDictionaryGroups`): the entries
    /// not looked up before are looked up once, then every row takes its entry's group.
    /// Returns `None` (doing nothing) for other groups. `new_group_rows` then refers to the
    /// looked-up entries, not to the rows.
    fn find_or_create_dictionary_groups(&mut self, groups: &DataChunk) -> Result<Option<usize>> {
        if self.skip_lookups || groups.column_count() != 1 {
            return Ok(None);
        }
        let Some((dictionary, sel, info)) = groups.column(0).storage_dictionary_parts() else {
            return Ok(None);
        };
        let size = info.size();
        if size >= MAX_DICTIONARY_SIZE {
            return Ok(None);
        }
        let count = groups.len();
        let rows = &sel.as_slice()[..count];
        if rows.iter().any(|&entry| entry as usize >= size) {
            return Ok(None);
        }
        let cached = &mut self.dictionary_groups;
        if !cached
            .dictionary
            .as_ref()
            .is_some_and(|known| Arc::ptr_eq(known, info))
        {
            cached.dictionary = Some(Arc::clone(info));
            cached.groups.clear();
            cached.groups.resize(size, RowPtr::NULL);
        }
        // The entries without a group yet, each once (DuckDB's `found_entry`).
        let mut unique: Vec<u32> = Vec::new();
        for &entry in rows {
            let group = &mut cached.groups[entry as usize];
            if group.is_null() {
                *group = RowPtr::from_ptr(std::ptr::dangling_mut());
                unique.push(entry);
            }
        }
        let mut new_groups = 0;
        if !unique.is_empty() {
            let values = Vector::storage_dictionary(
                Arc::clone(dictionary),
                SelectionVector::new(unique.clone()),
                Arc::clone(info),
            );
            let hashes = rustdb_row::hash::hash_columns(&[&values], unique.len())?;
            let chunk = DataChunk::new(vec![values], unique.len());
            new_groups = match self.find_or_create_groups(&chunk, &hashes) {
                Ok(new_groups) => new_groups,
                Err(error) => {
                    // The placeholders of the unique entries must not survive.
                    self.dictionary_groups = DictionaryGroups::default();
                    return Err(error);
                }
            };
            for (index, &entry) in unique.iter().enumerate() {
                self.dictionary_groups.groups[entry as usize] = self.addresses[index];
            }
        }
        let groups = &self.dictionary_groups.groups;
        self.addresses.clear();
        self.addresses
            .extend(rows.iter().map(|&entry| groups[entry as usize]));
        Ok(Some(new_groups))
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
        self.sink_count += groups.len();
        let new_groups = match self.find_or_create_dictionary_groups(groups)? {
            Some(new_groups) => new_groups,
            None => self.find_or_create_groups(groups, hashes)?,
        };
        if self.selection_update && selections.iter().all(Option::is_none) {
            // SAFETY: the addresses are pinned rows of this layout with live states.
            let added = unsafe {
                self.grouped_update
                    .update(&self.layout, &self.addresses[..groups.len()], inputs)?
            };
            if added {
                return Ok(new_groups);
            }
        }
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

    /// The finalized aggregates of the group of every row of `groups` (with precomputed
    /// `hashes`), one vector per aggregate (`FetchAggregates`). Like DuckDB, groups that
    /// do not exist yet are created.
    pub fn fetch_aggregates(&mut self, groups: &DataChunk, hashes: &Vector) -> Result<Vec<Vector>> {
        let count = groups.len();
        let mut results: Vec<Vector> = self
            .layout
            .aggregates()
            .iter()
            .map(|aggregate| Vector::flat(aggregate.function.return_type.clone(), count.max(1)))
            .collect();
        if count == 0 {
            return Ok(results);
        }
        self.find_or_create_groups(groups, hashes)?;
        // SAFETY: the addresses are pinned rows of this table with live states.
        unsafe {
            finalize_states(
                &mut self.row_state,
                &self.layout,
                &self.addresses,
                count,
                &mut results,
                0,
            )?;
        }
        Ok(results)
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
        self.dictionary_groups = DictionaryGroups::default();
        Ok(data)
    }

    /// Emits every group with its finalized aggregates: group columns first, then one
    /// column per aggregate. States are destroyed as they are finalized.
    pub fn finalize_into(&mut self, mut emit: impl FnMut(DataChunk) -> Result<()>) -> Result<()> {
        self.partitioned.flush_append_state(&mut self.append);
        let group_count = self.group_types.len();
        for partition in self.partitioned.partitions() {
            // SAFETY: the partitions hold this table's rows, pinned, with live states.
            unsafe {
                finalize_rows(
                    &mut self.row_state,
                    &self.layout,
                    group_count,
                    partition,
                    PinProperties::AlreadyPinned,
                    &mut emit,
                )?;
            }
        }
        self.states_live = false;
        Ok(())
    }

    /// Emits the groups of `collection` — rows of a table with this `layout` whose groups
    /// are all distinct, e.g. everything one thread aggregated into a single table — with
    /// their finalized aggregates, like [`GroupedAggregateHashTable::finalize_into`], and
    /// destroys their states.
    ///
    /// # Safety
    /// Every row of `collection` must hold live aggregate states of `layout` that nothing
    /// else finalizes or destroys.
    pub unsafe fn finalize_collection(
        layout: &TupleDataLayout,
        group_count: usize,
        collection: &TupleDataCollection,
        mut emit: impl FnMut(DataChunk) -> Result<()>,
    ) -> Result<()> {
        let mut row_state = RowOperationsState::default();
        // SAFETY: forwarded from the caller.
        unsafe {
            finalize_rows(
                &mut row_state,
                layout,
                group_count,
                collection,
                PinProperties::DestroyAfterDone,
                &mut emit,
            )
        }
    }
}

/// Finalizes (and destroys) the aggregate states of every row of `collection`, emitting
/// the group columns followed by one column per aggregate.
///
/// # Safety
/// Every row must hold live aggregate states of `layout`.
unsafe fn finalize_rows(
    row_state: &mut RowOperationsState,
    layout: &TupleDataLayout,
    group_count: usize,
    collection: &TupleDataCollection,
    pin: PinProperties,
    emit: &mut impl FnMut(DataChunk) -> Result<()>,
) -> Result<()> {
    let aggregate_types: Vec<LogicalType> = layout
        .aggregates()
        .iter()
        .map(|a| a.function.return_type.clone())
        .collect();
    let mut scan = collection.initialize_scan_columns((0..group_count).collect(), pin);
    let mut groups = DataChunk::default();
    while collection.scan(&mut scan, &mut groups)? {
        let count = groups.len();
        let rows = scan.chunk_state.row_locations();
        let mut results: Vec<Vector> = aggregate_types
            .iter()
            .map(|ty| Vector::flat(ty.clone(), count))
            .collect();
        // SAFETY: forwarded from the caller; the scan keeps the rows pinned.
        unsafe {
            finalize_states(row_state, layout, rows, count, &mut results, 0)?;
            if layout.has_destructor() {
                destroy_states(row_state, layout, rows, count)?;
            }
        }
        let mut columns = groups.columns().to_vec();
        columns.extend(results);
        emit(DataChunk::new(columns, count))?;
    }
    Ok(())
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
