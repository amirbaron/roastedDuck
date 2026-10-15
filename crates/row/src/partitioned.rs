//! Rows radix-partitioned on a hash column (DuckDB's `PartitionedTupleData` /
//! `RadixPartitionedTupleData`).
//!
//! Each partition is its own [`TupleDataCollection`]. An append computes the partition of
//! every row, orders the rows by partition with one selection vector, reserves space in
//! each touched partition and scatters everything in one pass.

use std::sync::Arc;

use rustdb_buffer::{BufferManager, MemoryTag};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{DataChunk, PhysicalType};

use crate::collection::{ChunkIterator, TupleDataCollection};
use crate::format::ChunkFormat;
use crate::layout::TupleDataLayout;
use crate::radix::RadixPartitioning;
use crate::row::load;
use crate::scatter::compute_heap_sizes;
use crate::state::{ChunkState, PinProperties, PinState};

/// Per-thread state of appends to a [`PartitionedTupleData`]
/// (`PartitionedTupleDataAppendState`).
#[derive(Debug)]
pub struct PartitionedAppendState {
    partition_indices: Vec<u32>,
    /// Rows ordered by partition: position `k` holds input row `partition_sel[k]`.
    partition_sel: Vec<u32>,
    /// Position of input row `i` among the partition-ordered rows (indexed by input row).
    reverse_partition_sel: Vec<u32>,
    /// Rows per partition in the current append.
    counts: Vec<u32>,
    /// Partitions with rows in the current append, ascending.
    touched: Vec<u32>,
    /// Scratch: next position of each partition's run.
    offsets: Vec<u32>,
    pin_states: Vec<PinState>,
    /// Row locations of the last append, in partition order.
    pub chunk_state: ChunkState,
}

impl PartitionedAppendState {
    /// Where input row `i` of the last append ended up in `chunk_state`.
    pub fn reverse_partition_sel(&self) -> &[u32] {
        &self.reverse_partition_sel
    }

    /// The input rows of the last append in partition order.
    pub fn partition_sel(&self) -> &[u32] {
        &self.partition_sel
    }
}

/// Radix-partitioned rows; see the module documentation.
#[derive(Debug)]
pub struct PartitionedTupleData {
    buffer_manager: BufferManager,
    layout: Arc<TupleDataLayout>,
    tag: MemoryTag,
    radix_bits: usize,
    hash_column: usize,
    partitions: Vec<TupleDataCollection>,
    count: usize,
}

impl PartitionedTupleData {
    /// `2^radix_bits` partitions of rows of `layout`, partitioned on the `UBIGINT` hash in
    /// column `hash_column`.
    pub fn new(
        buffer_manager: BufferManager,
        layout: Arc<TupleDataLayout>,
        tag: MemoryTag,
        radix_bits: usize,
        hash_column: usize,
    ) -> Result<Self> {
        if radix_bits > RadixPartitioning::MAX_RADIX_BITS {
            return Err(Error::internal(
                "radix_bits higher than RadixPartitioning::MAX_RADIX_BITS encountered in RadixBitsSwitch",
            ));
        }
        if layout
            .types()
            .get(hash_column)
            .map(rustdb_types::LogicalType::physical_type)
            != Some(PhysicalType::UInt64)
        {
            return Err(Error::internal(
                "the hash column of a partitioned collection must be UBIGINT",
            ));
        }
        let mut partitions = Vec::with_capacity(RadixPartitioning::number_of_partitions(radix_bits));
        for index in 0..RadixPartitioning::number_of_partitions(radix_bits) {
            let mut partition = TupleDataCollection::new(buffer_manager.clone(), Arc::clone(&layout), tag)?;
            partition.set_partition_index(index);
            partitions.push(partition);
        }
        Ok(Self {
            buffer_manager,
            layout,
            tag,
            radix_bits,
            hash_column,
            partitions,
            count: 0,
        })
    }

    /// An empty partitioned collection like this one but with `radix_bits` bits.
    pub fn with_radix_bits(&self, radix_bits: usize) -> Result<Self> {
        Self::new(
            self.buffer_manager.clone(),
            Arc::clone(&self.layout),
            self.tag,
            radix_bits,
            self.hash_column,
        )
    }

    pub fn layout(&self) -> &TupleDataLayout {
        &self.layout
    }

    pub fn radix_bits(&self) -> usize {
        self.radix_bits
    }

    pub fn partition_count(&self) -> usize {
        self.partitions.len()
    }

    pub fn partitions(&self) -> &[TupleDataCollection] {
        &self.partitions
    }

    pub fn partitions_mut(&mut self) -> &mut [TupleDataCollection] {
        &mut self.partitions
    }

    /// Takes the partitions out, leaving empty ones behind.
    pub fn take_partitions(&mut self) -> Vec<TupleDataCollection> {
        let empty = self
            .partitions
            .iter()
            .enumerate()
            .map(|(index, partition)| {
                let mut empty = partition.empty_like();
                empty.set_partition_index(index);
                empty
            })
            .collect();
        self.count = 0;
        std::mem::replace(&mut self.partitions, empty)
    }

    /// Total rows.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Bytes held by all partitions.
    pub fn size_in_bytes(&self) -> usize {
        self.partitions
            .iter()
            .map(TupleDataCollection::size_in_bytes)
            .sum()
    }

    /// Adds every partition's size and row count to `sizes` / `counts`.
    pub fn add_sizes_and_counts(&self, sizes: &mut [usize], counts: &mut [usize]) {
        for ((partition, size), count) in self.partitions.iter().zip(sizes).zip(counts) {
            *size += partition.size_in_bytes();
            *count += partition.count();
        }
    }

    /// A state for appending from one thread.
    pub fn initialize_append_state(&mut self, properties: PinProperties) -> PartitionedAppendState {
        let partitions = self.partitions.len();
        let column_ids = (0..self.layout.column_count()).collect();
        PartitionedAppendState {
            partition_indices: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            partition_sel: Vec::with_capacity(STANDARD_VECTOR_SIZE),
            reverse_partition_sel: vec![0; STANDARD_VECTOR_SIZE],
            counts: vec![0; partitions],
            touched: Vec::new(),
            offsets: vec![0; partitions],
            pin_states: (0..partitions).map(|_| PinState::new(properties)).collect(),
            chunk_state: ChunkState::new(column_ids),
        }
    }

    /// Appends all rows of `chunk`.
    pub fn append(&mut self, state: &mut PartitionedAppendState, chunk: &DataChunk) -> Result<()> {
        let formats = ChunkFormat::new(chunk)?;
        self.append_formatted(state, &formats, None, chunk.len())
    }

    /// Appends rows `sel[..count]` (or the first `count`) of formatted input
    /// (`AppendUnified`).
    pub fn append_formatted(
        &mut self,
        state: &mut PartitionedAppendState,
        formats: &ChunkFormat<'_>,
        sel: Option<&[u32]>,
        count: usize,
    ) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let hashes = formats.column(self.hash_column);
        state.partition_indices.clear();
        for i in 0..count {
            let row = sel.map_or(i, |sel| sel[i] as usize);
            let slot = hashes.slot(row);
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(hashes.slot_bytes(slot, 8));
            let hash = u64::from_ne_bytes(bytes);
            state
                .partition_indices
                .push(RadixPartitioning::partition_index(hash, self.radix_bits) as u32);
        }
        let single = build_partition_sel(state, sel, count);
        if let Some(partition_index) = single {
            let partition = &mut self.partitions[partition_index];
            let pin_state = &mut state.pin_states[partition_index];
            let mut append = crate::state::AppendState {
                pin_state: std::mem::take(pin_state),
                chunk_state: std::mem::take(&mut state.chunk_state),
            };
            let result = partition.append_formatted(&mut append, formats, sel, count);
            *pin_state = append.pin_state;
            state.chunk_state = append.chunk_state;
            result?;
        } else {
            if !self.layout.all_constant() {
                compute_heap_sizes(
                    &self.layout,
                    formats,
                    Some(&state.partition_sel),
                    count,
                    &mut state.chunk_state.heap_sizes,
                )?;
            }
            self.build_buffer_space(state)?;
            self.partitions[0].scatter(&mut state.chunk_state, formats, Some(&state.partition_sel), count);
        }
        self.count += count;
        Ok(())
    }

    /// Appends rows found by an iterator over another collection with the same layout
    /// (used for repartitioning).
    fn append_rows(
        &mut self,
        state: &mut PartitionedAppendState,
        input: &ChunkState,
        count: usize,
    ) -> Result<()> {
        let offset = self.layout.offsets()[self.hash_column];
        state.partition_indices.clear();
        for row in &input.row_locations[..count] {
            // SAFETY: the input rows are pinned rows of this layout; the hash column holds
            // a `u64`.
            let hash = unsafe { load::<u64>(row.as_ptr().add(offset)) };
            state
                .partition_indices
                .push(RadixPartitioning::partition_index(hash, self.radix_bits) as u32);
        }
        let single = build_partition_sel(state, None, count);
        if let Some(partition_index) = single {
            state.chunk_state.heap_sizes[..count].copy_from_slice(&input.heap_sizes[..count]);
            let partition = &mut self.partitions[partition_index];
            partition.build(
                &mut state.pin_states[partition_index],
                &mut state.chunk_state,
                0,
                count,
            )?;
            partition.copy_rows(&state.chunk_state, input, None, count);
        } else {
            for (target, &source) in state.chunk_state.heap_sizes.iter_mut().zip(&state.partition_sel) {
                *target = input.heap_sizes[source as usize];
            }
            self.build_buffer_space(state)?;
            self.partitions[0].copy_rows(&state.chunk_state, input, Some(&state.partition_sel), count);
        }
        self.count += count;
        Ok(())
    }

    /// Reserves each touched partition's rows at its range of the partition-ordered chunk.
    fn build_buffer_space(&mut self, state: &mut PartitionedAppendState) -> Result<()> {
        let mut offset = 0;
        for &partition_index in &state.touched {
            let length = state.counts[partition_index as usize] as usize;
            let partition = &mut self.partitions[partition_index as usize];
            partition.build(
                &mut state.pin_states[partition_index as usize],
                &mut state.chunk_state,
                offset,
                length,
            )?;
            offset += length;
        }
        Ok(())
    }

    /// Releases (or stores) every pin of `state` (`FlushAppendState`).
    pub fn flush_append_state(&mut self, state: &mut PartitionedAppendState) {
        for (partition, pin_state) in self.partitions.iter().zip(&mut state.pin_states) {
            partition.finalize_pin_state(pin_state);
        }
    }

    /// Moves every row of `other` (same radix bits) into this collection (`Combine`).
    pub fn combine(&mut self, mut other: PartitionedTupleData) -> Result<()> {
        if other.count == 0 {
            return Ok(());
        }
        if other.partitions.len() != self.partitions.len() {
            return Err(Error::internal(
                "combining partitioned data with different partition counts",
            ));
        }
        for (target, source) in self.partitions.iter_mut().zip(other.partitions.drain(..)) {
            target.combine(source)?;
        }
        self.count += other.count;
        Ok(())
    }

    /// Removes all rows.
    pub fn reset(&mut self) {
        for partition in &mut self.partitions {
            partition.reset();
        }
        self.count = 0;
    }

    /// Releases every stored pin.
    pub fn unpin(&self) {
        for partition in &self.partitions {
            partition.unpin();
        }
    }

    /// Moves every row into `target`, which has at least as many radix bits
    /// (`Repartition`). Partitions are consumed one by one, so memory is freed as it goes.
    pub fn repartition(&mut self, target: &mut PartitionedTupleData) -> Result<()> {
        if self.layout.types() != target.layout.types() {
            return Err(Error::internal("repartitioning into a different layout"));
        }
        if self.partitions.len() == target.partitions.len() {
            let partitions = self.take_partitions();
            let count: usize = partitions.iter().map(TupleDataCollection::count).sum();
            for (into, from) in target.partitions.iter_mut().zip(partitions) {
                into.combine(from)?;
            }
            target.count += count;
            return Ok(());
        }
        if self.partitions.len() > target.partitions.len() {
            return Err(Error::internal(format!(
                "PartitionedTupleData::Repartition into fewer partitions ({} -> {})",
                self.partitions.len(),
                target.partitions.len()
            )));
        }
        let mut state = target.initialize_append_state(PinProperties::UnpinAfterDone);
        let multiplier = 1 << (target.radix_bits - self.radix_bits);
        for partition_index in 0..self.partitions.len() {
            let partition = &self.partitions[partition_index];
            if partition.count() > 0 {
                let mut iterator = ChunkIterator::new(partition, PinProperties::DestroyAfterDone, true)?;
                loop {
                    let count = iterator.current_chunk_count();
                    target.append_rows(&mut state, iterator.chunk_state(), count)?;
                    if !iterator.next_chunk()? {
                        break;
                    }
                }
                // Rows of this partition only go to these target partitions: unpin them.
                let from = partition_index * multiplier;
                for index in from..from + multiplier {
                    target.partitions[index].finalize_pin_state(&mut state.pin_states[index]);
                }
            }
            self.partitions[partition_index].reset();
        }
        target.flush_append_state(&mut state);
        self.count = 0;
        Ok(())
    }

    /// All rows as a single collection (`GetUnpartitioned`).
    pub fn take_unpartitioned(&mut self) -> Result<TupleDataCollection> {
        let mut partitions = self.take_partitions().into_iter();
        let Some(mut result) = partitions.next() else {
            return Err(Error::internal("partitioned data without partitions"));
        };
        for partition in partitions {
            result.combine(partition)?;
        }
        Ok(result)
    }
}

/// Orders the rows of an append by partition (`BuildPartitionSel`). Returns the partition
/// if all rows fall into one.
fn build_partition_sel(
    state: &mut PartitionedAppendState,
    sel: Option<&[u32]>,
    count: usize,
) -> Option<usize> {
    for &partition in &state.touched {
        state.counts[partition as usize] = 0;
    }
    state.touched.clear();
    for &partition in &state.partition_indices[..count] {
        let slot = &mut state.counts[partition as usize];
        if *slot == 0 {
            state.touched.push(partition);
        }
        *slot += 1;
    }
    let input = |i: usize| sel.map_or(i as u32, |sel| sel[i]);
    let max_input = (0..count).map(input).max().map_or(0, |max| max as usize + 1);
    if state.reverse_partition_sel.len() < max_input {
        state.reverse_partition_sel.resize(max_input, 0);
    }
    if let [partition] = state.touched[..] {
        state.partition_sel.clear();
        state.partition_sel.extend((0..count).map(input));
        for i in 0..count {
            state.reverse_partition_sel[input(i) as usize] = i as u32;
        }
        return Some(partition as usize);
    }
    state.touched.sort_unstable();
    let mut offset = 0;
    for &partition in &state.touched {
        state.offsets[partition as usize] = offset;
        offset += state.counts[partition as usize];
    }
    state.partition_sel.clear();
    state.partition_sel.resize(count, 0);
    for i in 0..count {
        let partition = state.partition_indices[i] as usize;
        let position = state.offsets[partition];
        state.offsets[partition] += 1;
        state.reverse_partition_sel[input(i) as usize] = position;
        state.partition_sel[position as usize] = input(i);
    }
    None
}
