//! Merging sorted runs in parallel (DuckDB's `SortedRunMerger`).
//!
//! The output is cut into partitions of `partition_size` rows. For each partition the
//! boundaries within every run are found with a k-way merge-path search, then the keys of
//! those ranges are copied into a buffer and sorted (the ranges are sorted already, so this
//! is a merge of a few runs), and finally scanned: key columns are decoded from the keys and
//! payload columns gathered from the payload rows the keys point to.
//!
//! In-memory runs are read directly from their pinned blocks. Runs of an external sort are
//! read chunk by chunk: keys and payload are stored in the same order, so chunk `c` of keys
//! belongs to chunk `c` of payload, and the payload pointers are set while copying keys.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_buffer::{AllocatedData, BufferHandle};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{DataChunk, PhysicalType, Vector};

use super::algorithm::KeyBlocks;
use super::encode::{decode_fixed_into, decode_sort_key};
use super::key::{SortKey, dispatch_sort_key};
use super::pdqsort::pdqsort_branchless;
use super::run::SortedRun;
use super::vergesort::vergesort;
use super::{ColumnSource, SortLayouts};
use crate::gather::HeapKeepAlive;
use crate::row::RowPtr;
use crate::state::{PinProperties, ScanState};

/// DuckDB's `DEFAULT_ROW_GROUP_SIZE`: rows per merge partition.
pub const DEFAULT_ROW_GROUP_SIZE: usize = 122_880;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `[begin, end)` of a partition within one run.
#[derive(Clone, Copy, Debug, Default)]
struct Boundary {
    begin: usize,
    end: usize,
}

#[derive(Debug)]
struct PartitionState {
    boundaries: Mutex<Vec<Boundary>>,
    begin_computed: AtomicBool,
    scanned: AtomicBool,
}

/// Block pointers of an in-memory run's keys.
#[derive(Debug)]
struct RunBlocks {
    blocks: Vec<RowPtr>,
    per_block: usize,
    len: usize,
}

/// What a local merger state is working on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Task {
    ComputeBoundaries,
    AcquireBoundaries,
    MergePartition,
    ScanPartition,
    Finished,
}

/// Merges finalized runs; shared by all threads of the source phase (DuckDB's merger and
/// its global source state).
pub struct SortedRunMerger {
    pub(crate) layouts: Arc<SortLayouts>,
    pub(crate) runs: Vec<SortedRun>,
    pub(crate) total_count: usize,
    pub(crate) partition_size: usize,
    pub(crate) external: bool,
    pub(crate) index_creation: bool,
    threads: usize,
    num_partitions: usize,
    partitions: Vec<PartitionState>,
    next_partition: Mutex<usize>,
    total_scanned: AtomicUsize,
    destroy_partition: Mutex<usize>,
    materialized: Mutex<Vec<Option<SortedRun>>>,
    run_blocks: Vec<RunBlocks>,
    payload_keep_alive: Option<HeapKeepAlive>,
}

impl std::fmt::Debug for SortedRunMerger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SortedRunMerger")
            .field("runs", &self.runs.len())
            .field("total_count", &self.total_count)
            .field("partitions", &self.num_partitions)
            .field("external", &self.external)
            .finish_non_exhaustive()
    }
}

/// One thread's position in the merge.
pub struct MergerLocalState {
    task: Task,
    partition: Option<usize>,
    boundaries: Vec<Boundary>,
    external_runs: Vec<ExternalRun>,
    merged: Option<AllocatedData>,
    merged_count: usize,
    merged_index: usize,
    /// Pins of everything the merged keys point to (external sorts).
    pins: Vec<Arc<BufferHandle>>,
    decode_buffer: Vec<u8>,
}

impl std::fmt::Debug for MergerLocalState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergerLocalState")
            .field("task", &self.task)
            .field("partition", &self.partition)
            .finish_non_exhaustive()
    }
}

impl MergerLocalState {
    /// The partition being produced (the output batch index).
    pub fn partition_index(&self) -> Option<usize> {
        self.partition
    }
}

/// Chunk-wise access to the keys of an external run.
struct ExternalRun {
    keys: ScanState,
    payload: ScanState,
    chunk: Option<usize>,
    /// Copies of the current chunk's keys, with payload pointers set.
    local: Vec<u64>,
    keep_pinned: bool,
    pin_payload: bool,
}

impl ExternalRun {
    fn new() -> Self {
        Self {
            keys: ScanState {
                pin_state: crate::state::PinState::new(PinProperties::UnpinAfterDone),
                ..ScanState::default()
            },
            payload: ScanState {
                pin_state: crate::state::PinState::new(PinProperties::UnpinAfterDone),
                ..ScanState::default()
            },
            chunk: None,
            local: Vec::new(),
            keep_pinned: false,
            pin_payload: false,
        }
    }

    /// Switches between boundary search (nothing kept) and merging (keys and payload of
    /// the partition stay pinned).
    fn set_mode(&mut self, keep_pinned: bool, pin_payload: bool) {
        self.keep_pinned = keep_pinned;
        self.pin_payload = pin_payload;
        self.keys.pin_state.clear();
        self.payload.pin_state.clear();
        self.keys.pinned_segment = None;
        self.payload.pinned_segment = None;
        self.chunk = None;
    }

    /// Key `index` of `run`, loading its chunk if needed.
    fn key<K: SortKey>(
        &mut self,
        run: &SortedRun,
        index: usize,
        pins: &mut Vec<Arc<BufferHandle>>,
    ) -> Result<K> {
        let chunk = index / STANDARD_VECTOR_SIZE;
        if self.chunk != Some(chunk) {
            if self.keep_pinned {
                self.keys.pin_state.take_handles(pins);
                self.payload.pin_state.take_handles(pins);
            }
            let count = run.key_data.fetch_chunk(&mut self.keys, chunk, false)?;
            let words = size_of::<K>() / 8;
            self.local.clear();
            self.local.resize(count * words, 0);
            let local = typed_mut::<K>(&mut self.local);
            for (i, key) in local.iter_mut().enumerate().take(count) {
                // SAFETY: the chunk's key rows were just pinned and hold `K`s.
                *key = unsafe { self.keys.chunk_state.row_locations[i].as_ptr().cast::<K>().read() };
            }
            if self.pin_payload
                && let Some(payload) = &run.payload_data
            {
                payload.fetch_chunk(&mut self.payload, chunk, false)?;
                for (key, row) in local
                    .iter_mut()
                    .zip(&self.payload.chunk_state.row_locations)
                    .take(count)
                {
                    key.set_payload(row.as_ptr());
                }
            }
            self.chunk = Some(chunk);
        }
        Ok(typed_mut::<K>(&mut self.local)[index % STANDARD_VECTOR_SIZE])
    }
}

/// `words` as keys (key structs are `#[repr(C)]` runs of `u64`s).
fn typed_mut<K: SortKey>(words: &mut [u64]) -> &mut [K] {
    let len = words.len() * 8 / size_of::<K>();
    // SAFETY: `K` consists of `u64`s (size a multiple of 8, alignment 8) and every bit
    // pattern is a valid key value.
    unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<K>(), len) }
}

impl SortedRunMerger {
    pub(crate) fn new(
        layouts: Arc<SortLayouts>,
        runs: Vec<SortedRun>,
        partition_size: usize,
        external: bool,
        index_creation: bool,
        threads: usize,
    ) -> Result<Self> {
        let total_count: usize = runs.iter().map(SortedRun::count).sum();
        let partition_size = partition_size.max(1);
        let num_partitions = total_count.div_ceil(partition_size);
        let partitions = (0..num_partitions)
            .map(|index| {
                let maximum_end = (index + 1) * partition_size;
                PartitionState {
                    boundaries: Mutex::new(
                        runs.iter()
                            .map(|run| Boundary {
                                begin: 0,
                                end: run.count().min(maximum_end),
                            })
                            .collect(),
                    ),
                    begin_computed: AtomicBool::new(index == 0),
                    scanned: AtomicBool::new(false),
                }
            })
            .collect();
        let mut run_blocks = Vec::new();
        let mut payload_keep_alive = None;
        if !external {
            for run in &runs {
                run_blocks.push(RunBlocks {
                    blocks: run
                        .key_data
                        .row_block_pointers()?
                        .into_iter()
                        .map(RowPtr::from_ptr)
                        .collect(),
                    per_block: run.key_data.tuples_per_block(),
                    len: run.count(),
                });
            }
            let keep_alive: Vec<HeapKeepAlive> = runs
                .iter()
                .filter_map(|run| {
                    run.payload_data
                        .as_ref()
                        .map(crate::TupleDataCollection::heap_keep_alive)
                })
                .collect();
            payload_keep_alive = Some(Arc::new(keep_alive) as HeapKeepAlive);
        }
        Ok(Self {
            layouts,
            runs,
            total_count,
            partition_size,
            external,
            index_creation,
            threads: threads.max(1),
            num_partitions,
            partitions,
            next_partition: Mutex::new(0),
            total_scanned: AtomicUsize::new(0),
            destroy_partition: Mutex::new(0),
            materialized: Mutex::new(Vec::new()),
            run_blocks,
            payload_keep_alive,
        })
    }

    /// Partitions that can be produced in parallel.
    pub fn max_threads(&self) -> usize {
        self.num_partitions.max(1)
    }

    /// Rows scanned so far (progress).
    pub fn scanned(&self) -> usize {
        self.total_scanned.load(Ordering::Relaxed)
    }

    pub fn local_state(&self) -> MergerLocalState {
        MergerLocalState {
            task: Task::Finished,
            partition: None,
            boundaries: vec![Boundary::default(); self.runs.len()],
            external_runs: if self.external {
                self.runs.iter().map(|_| ExternalRun::new()).collect()
            } else {
                Vec::new()
            },
            merged: None,
            merged_count: 0,
            merged_index: 0,
            pins: Vec::new(),
            decode_buffer: Vec::new(),
        }
    }

    fn assign_task(&self, local: &mut MergerLocalState) -> bool {
        let mut next = lock(&self.next_partition);
        if *next == self.num_partitions {
            return false;
        }
        local.partition = Some(*next);
        local.task = Task::ComputeBoundaries;
        *next += 1;
        true
    }

    /// Produces the next chunk of sorted output into `chunk`; an empty chunk means this
    /// thread is done (`GetData`).
    pub fn get_data(&self, chunk: &mut DataChunk, local: &mut MergerLocalState) -> Result<()> {
        chunk.set_columns(Vec::new(), 0);
        while chunk.is_empty() {
            if local.task == Task::Finished && !self.assign_task(local) {
                break;
            }
            self.execute_task(local, Some(&mut *chunk))?;
        }
        if chunk.is_empty() {
            local.clear();
        }
        Ok(())
    }

    /// Merges every partition this thread can claim into sorted runs
    /// (`MaterializeSortedRun`); collect them with [`Self::take_sorted_run`].
    pub fn materialize(&self, local: &mut MergerLocalState) -> Result<()> {
        loop {
            if local.task == Task::Finished && !self.assign_task(local) {
                break;
            }
            self.execute_task(local, None)?;
        }
        local.clear();
        Ok(())
    }

    /// All materialized partitions combined into one run, in order (`GetSortedRun`).
    pub fn take_sorted_run(&self) -> Result<Option<SortedRun>> {
        let mut materialized = lock(&self.materialized);
        let mut partitions = std::mem::take(&mut *materialized).into_iter().flatten();
        let Some(mut result) = partitions.next() else {
            return Ok(None);
        };
        for partition in partitions {
            result.combine(partition)?;
        }
        Ok(Some(result))
    }

    fn execute_task(&self, local: &mut MergerLocalState, chunk: Option<&mut DataChunk>) -> Result<()> {
        let Some(partition) = local.partition else {
            return Err(Error::internal("merger task without a partition"));
        };
        match local.task {
            Task::ComputeBoundaries => {
                self.compute_partition_boundaries(local, partition)?;
                local.task = Task::AcquireBoundaries;
            }
            Task::AcquireBoundaries => {
                self.acquire_partition_boundaries(local, partition)?;
                local.task = Task::MergePartition;
            }
            Task::MergePartition => {
                let key_type = self.layouts.key_type;
                dispatch_sort_key!(key_type, K => self.merge_partition::<K>(local))?;
                local.task = Task::ScanPartition;
            }
            Task::ScanPartition => {
                let key_type = self.layouts.key_type;
                let done = if let Some(chunk) = chunk {
                    dispatch_sort_key!(key_type, K => self.scan_partition::<K>(local, chunk))?;
                    chunk.is_empty()
                } else {
                    dispatch_sort_key!(key_type, K => self.materialize_partition::<K>(local, partition))?;
                    true
                };
                if done {
                    self.destroy_scanned_data();
                    self.partitions[partition].scanned.store(true, Ordering::Release);
                    self.total_scanned
                        .fetch_add(local.merged_count, Ordering::Relaxed);
                    local.partition = None;
                    local.task = Task::Finished;
                    local.pins.clear();
                }
            }
            Task::Finished => {}
        }
        Ok(())
    }

    /// Computes where partition `p` ends in every run (`ComputePartitionBoundaries`).
    fn compute_partition_boundaries(&self, local: &mut MergerLocalState, p: usize) -> Result<()> {
        let current = &self.partitions[p];
        let begin_computed = current.begin_computed.load(Ordering::Acquire);
        local.boundaries.clone_from(&lock(&current.boundaries));
        if !begin_computed {
            // Start from the closest earlier partition whose begin is known.
            for previous in (2..=p).rev() {
                let partition = &self.partitions[previous - 1];
                if !partition.begin_computed.load(Ordering::Acquire) {
                    continue;
                }
                let boundaries = lock(&partition.boundaries);
                for (boundary, known) in local.boundaries.iter_mut().zip(boundaries.iter()) {
                    boundary.begin = known.begin;
                }
                break;
            }
        }
        let key_type = self.layouts.key_type;
        dispatch_sort_key!(key_type, K => self.compute_boundaries_typed::<K>(local, p))?;
        if p + 1 != self.num_partitions {
            let next = &self.partitions[p + 1];
            if !next.begin_computed.load(Ordering::Acquire) {
                let mut boundaries = lock(&next.boundaries);
                if !next.begin_computed.load(Ordering::Acquire) {
                    for (boundary, computed) in boundaries.iter_mut().zip(&local.boundaries) {
                        boundary.begin = computed.begin;
                    }
                    next.begin_computed.store(true, Ordering::Release);
                }
            }
        }
        let mut boundaries = lock(&current.boundaries);
        for (boundary, computed) in boundaries.iter_mut().zip(&local.boundaries) {
            boundary.end = computed.end;
        }
        Ok(())
    }

    fn compute_boundaries_typed<K: SortKey>(&self, local: &mut MergerLocalState, p: usize) -> Result<()> {
        if p + 1 == self.num_partitions {
            for (boundary, run) in local.boundaries.iter_mut().zip(&self.runs) {
                boundary.begin = run.count();
            }
            return Ok(());
        }
        let mut total_remaining = (p + 1) * self.partition_size;
        let mut active = Vec::with_capacity(self.runs.len());
        for (index, boundary) in local.boundaries.iter().enumerate() {
            if boundary.begin != boundary.end {
                active.push(index);
            }
            total_remaining -= boundary.begin;
        }
        let in_memory = self.in_memory_blocks::<K>();
        for run in &mut local.external_runs {
            run.set_mode(false, false);
        }
        while total_remaining != 0 {
            let k = active.len();
            if k == 0 {
                return Err(Error::internal("merge boundary search ran out of runs"));
            }
            let mut min_index = active[0];
            let mut min_delta = step(total_remaining, k, local.boundaries[min_index]);
            let mut min_key: K = self.key_at(
                &in_memory,
                local,
                min_index,
                local.boundaries[min_index].begin + min_delta - 1,
            )?;
            for &run in &active[1..] {
                let boundary = local.boundaries[run];
                let delta = step(total_remaining, k, boundary);
                let key: K = self.key_at(&in_memory, local, run, boundary.begin + delta - 1)?;
                if key.less(&min_key) {
                    min_index = run;
                    min_delta = delta;
                    min_key = key;
                }
            }
            let boundary = &mut local.boundaries[min_index];
            boundary.begin += min_delta;
            if boundary.begin == boundary.end {
                active.retain(|&run| run != min_index);
            }
            total_remaining -= min_delta;
        }
        for boundary in &mut local.boundaries {
            boundary.end = boundary.begin;
        }
        Ok(())
    }

    /// The keys of in-memory runs.
    fn in_memory_blocks<K: SortKey>(&self) -> Vec<KeyBlocks<K>> {
        self.run_blocks
            .iter()
            .map(|run| {
                let blocks = run.blocks.iter().map(|block| block.as_ptr()).collect();
                // SAFETY: in-memory runs keep every key block pinned until the merger is
                // dropped; the keys are only read.
                unsafe { KeyBlocks::new(blocks, run.per_block, run.len) }
            })
            .collect()
    }

    fn key_at<K: SortKey>(
        &self,
        in_memory: &[KeyBlocks<K>],
        local: &mut MergerLocalState,
        run: usize,
        index: usize,
    ) -> Result<K> {
        if self.external {
            local.external_runs[run].key::<K>(&self.runs[run], index, &mut local.pins)
        } else {
            Ok(in_memory[run].get(index))
        }
    }

    /// Takes the begin boundaries computed by the previous partition (or computes them).
    fn acquire_partition_boundaries(&self, local: &mut MergerLocalState, p: usize) -> Result<()> {
        let current = &self.partitions[p];
        if !current.begin_computed.load(Ordering::Acquire) {
            // Not computed by another thread yet: do it here.
            self.compute_partition_boundaries(local, p - 1)?;
        }
        local.boundaries.clone_from(&lock(&current.boundaries));
        Ok(())
    }

    /// Copies the partition's key ranges together and sorts them (`MergePartition`).
    fn merge_partition<K: SortKey>(&self, local: &mut MergerLocalState) -> Result<()> {
        let words = self.partition_size * size_of::<K>() / 8;
        if local
            .merged
            .as_ref()
            .is_none_or(|merged| merged.len() < words * 8)
        {
            let data = self.layouts.buffer_manager.allocate_data(words * 8)?;
            // SAFETY: freshly allocated, `words * 8` bytes; zero them once so every key slot
            // is initialised.
            unsafe { std::ptr::write_bytes(data.as_ptr(), 0, words * 8) };
            local.merged = Some(data);
        }
        local.merged_count = 0;
        local.merged_index = 0;
        local.pins.clear();
        for run in &mut local.external_runs {
            run.set_mode(true, true);
        }
        let in_memory = self.in_memory_blocks::<K>();
        let mut active_runs = 0;
        let mut merged_count = 0;
        let boundaries = local.boundaries.clone();
        for (run, boundary) in boundaries.iter().enumerate() {
            if boundary.begin == boundary.end {
                continue;
            }
            active_runs += 1;
            for index in boundary.begin..boundary.end {
                let key: K = self.key_at(&in_memory, local, run, index)?;
                merged_keys::<K>(local)[merged_count] = key;
                merged_count += 1;
            }
        }
        // Keep the pins of the last chunks, too.
        for run in &mut local.external_runs {
            run.keys.pin_state.take_handles(&mut local.pins);
            run.payload.pin_state.take_handles(&mut local.pins);
            run.chunk = None;
        }
        local.merged_count = merged_count;
        if active_runs > 1 && !self.index_creation {
            // Re-sorting beats merging: vergesort finds the sorted ranges and merges them,
            // pdqsort handles the rest (DuckDB's `TemplatedComputeMerge`).
            let keys = &mut merged_keys::<K>(local)[..merged_count];
            let less = |a: &K, b: &K| a.less(b);
            vergesort(keys, 0, merged_count, &less, &mut |keys: &mut [K], begin, end| {
                pdqsort_branchless(keys, begin, end, &less);
            });
        }
        Ok(())
    }

    /// Emits the next chunk of the merged partition (`ScanPartition`).
    fn scan_partition<K: SortKey>(&self, local: &mut MergerLocalState, chunk: &mut DataChunk) -> Result<()> {
        let count = (local.merged_count - local.merged_index).min(STANDARD_VECTOR_SIZE);
        if count == 0 {
            chunk.set_columns(Vec::new(), 0);
            return Ok(());
        }
        let start = local.merged_index;
        let keys: Vec<K> = merged_keys::<K>(local)[start..start + count].to_vec();
        local.merged_index += count;
        let keep_alive = if self.external {
            Some(Arc::new(local.pins.clone()) as HeapKeepAlive)
        } else {
            self.payload_keep_alive.clone()
        };
        self.layouts
            .scan_keys(&keys, &mut local.decode_buffer, keep_alive.as_ref(), chunk)
    }

    /// Writes the merged partition into a new run (`MaterializePartition`).
    fn materialize_partition<K: SortKey>(
        &self,
        local: &mut MergerLocalState,
        partition: usize,
    ) -> Result<()> {
        let mut run = SortedRun::for_materialization(&self.layouts)?;
        let count = local.merged_count;
        run.append_keys(&mut merged_keys::<K>(local)[..count])?;
        run.finish_materialization();
        let mut materialized = lock(&self.materialized);
        if materialized.len() <= partition {
            materialized.resize_with(partition + 1, || None);
        }
        materialized[partition] = Some(run);
        Ok(())
    }

    /// Frees blocks of partitions every thread is done with (external sorts only;
    /// `DestroyScannedData`).
    fn destroy_scanned_data(&self) {
        if !self.external {
            return;
        }
        let Ok(mut destroy_partition) = self.destroy_partition.try_lock() else {
            return;
        };
        let mut end = *destroy_partition;
        while end < self.num_partitions && self.partitions[end].scanned.load(Ordering::Acquire) {
            end += 1;
        }
        // Lag behind by one partition per thread so data in use is never destroyed.
        if end < self.threads {
            return;
        }
        end -= self.threads;
        if end <= *destroy_partition {
            return;
        }
        for (run_index, run) in self.runs.iter().enumerate() {
            let begin = if *destroy_partition == 0 {
                0
            } else {
                lock(&self.partitions[*destroy_partition].boundaries)[run_index].end
            };
            let end_row = if end == self.num_partitions {
                run.count()
            } else {
                lock(&self.partitions[end].boundaries)[run_index].end
            };
            run.destroy_data(begin, end_row);
        }
        *destroy_partition = end;
    }
}

/// `ceil(total_remaining / k)`, but at most what the run has left.
fn step(total_remaining: usize, k: usize, boundary: Boundary) -> usize {
    total_remaining.div_ceil(k).min(boundary.end - boundary.begin)
}

impl MergerLocalState {
    fn clear(&mut self) {
        self.external_runs
            .iter_mut()
            .for_each(|run| run.set_mode(false, false));
        self.merged = None;
        self.pins.clear();
    }
}

/// The merge buffer as keys.
fn merged_keys<K: SortKey>(local: &mut MergerLocalState) -> &mut [K] {
    match &local.merged {
        Some(data) => {
            let len = data.len() / size_of::<K>();
            // SAFETY: the buffer is 16-byte aligned, zero-initialised and owned by `local`;
            // `K` is plain data.
            unsafe { std::slice::from_raw_parts_mut(data.as_ptr().cast::<K>(), len) }
        }
        None => &mut [],
    }
}

/// Whether values of `physical` are encoded as exactly their width (no delimiters).
fn is_plain_fixed(physical: PhysicalType) -> bool {
    !matches!(
        physical,
        PhysicalType::Varchar
            | PhysicalType::List
            | PhysicalType::Array
            | PhysicalType::Struct
            | PhysicalType::Bit
            | PhysicalType::Unknown
            | PhysicalType::Invalid
    )
}

impl SortLayouts {
    /// Decodes inline keys of fixed-width columns without the generic decoder: each column
    /// is a validity byte followed by the value (absent for NULLs).
    fn decode_fixed_keys<K: SortKey>(&self, keys: &[K], decoded: &mut [Vector]) {
        struct Column {
            physical: PhysicalType,
            width: usize,
            null_byte: u8,
            flip: bool,
        }
        let columns: Vec<Column> = self
            .key_types
            .iter()
            .zip(&self.modifiers)
            .map(|(ty, modifiers)| {
                let physical = ty.physical_type();
                Column {
                    physical,
                    width: physical.size(),
                    null_byte: modifiers.validity_bytes().0,
                    flip: modifiers.flip(),
                }
            })
            .collect();
        let mut nulls: Vec<Vec<usize>> = vec![Vec::new(); columns.len()];
        {
            let mut targets: Vec<&mut [u8]> = decoded.iter_mut().map(Vector::data_bytes_mut).collect();
            let mut bytes = [0u8; 32];
            for (row, key) in keys.iter().enumerate() {
                for (part, chunk) in key.parts().iter().zip(bytes.chunks_mut(8)) {
                    chunk.copy_from_slice(&part.to_be_bytes());
                }
                let mut position = 0;
                for ((column, target), nulls) in columns.iter().zip(&mut targets).zip(&mut nulls) {
                    position += 1;
                    if bytes[position - 1] == column.null_byte {
                        nulls.push(row);
                        continue;
                    }
                    decode_fixed_into(
                        column.physical,
                        &bytes[position..position + column.width],
                        column.flip,
                        &mut target[row * column.width..(row + 1) * column.width],
                    );
                    position += column.width;
                }
            }
        }
        for (vector, nulls) in decoded.iter_mut().zip(nulls) {
            let capacity = vector.capacity();
            let validity = vector.validity_mut();
            for row in nulls {
                validity.set_invalid(row, capacity);
            }
        }
    }

    /// Builds output columns for `keys`: key columns decoded, payload columns gathered
    /// (`SortedRunScanState::Scan`).
    pub(crate) fn scan_keys<K: SortKey>(
        &self,
        keys: &[K],
        buffer: &mut Vec<u8>,
        keep_alive: Option<&HeapKeepAlive>,
        chunk: &mut DataChunk,
    ) -> Result<()> {
        self.scan_key_columns(keys, None, buffer, keep_alive, chunk)
    }

    /// [`Self::scan_keys`] for output columns `columns` only (all if `None`), in that
    /// order.
    pub(crate) fn scan_key_columns<K: SortKey>(
        &self,
        keys: &[K],
        columns: Option<&[usize]>,
        buffer: &mut Vec<u8>,
        keep_alive: Option<&HeapKeepAlive>,
        chunk: &mut DataChunk,
    ) -> Result<()> {
        let count = keys.len();
        let wanted = |output: usize| columns.is_none_or(|columns| columns.contains(&output));
        let mut result: Vec<Option<Vector>> = vec![None; self.output_types.len()];
        let needs_keys = self
            .outputs
            .iter()
            .any(|output| matches!(output.source, ColumnSource::Key(_)) && wanted(output.output));
        if needs_keys {
            let mut decoded: Vec<Vector> = self
                .key_types
                .iter()
                .map(|ty| Vector::flat(ty.clone(), count.max(1)))
                .collect();
            if K::TYPE.is_constant_size()
                && self.key_types.iter().all(|ty| is_plain_fixed(ty.physical_type()))
            {
                self.decode_fixed_keys(keys, &mut decoded);
            } else {
                for (row, key) in keys.iter().enumerate() {
                    let size = key.heap_size();
                    if size > K::INLINE_LENGTH {
                        // SAFETY: the heaps of long keys are pinned while the merge partition
                        // lives; the key's heap data holds its `size` bytes.
                        let bytes = unsafe { std::slice::from_raw_parts(key.heap_data(), size) };
                        decode_sort_key(bytes, &mut decoded, row, &self.modifiers)?;
                    } else {
                        buffer.clear();
                        // SAFETY: inline key (no heap access).
                        unsafe { key.decode_into(buffer) };
                        decode_sort_key(buffer, &mut decoded, row, &self.modifiers)?;
                    }
                }
            }
            for output in &self.outputs {
                if let ColumnSource::Key(key) = output.source
                    && wanted(output.output)
                {
                    result[output.output] = Some(decoded[key].clone());
                }
            }
        }
        if let Some(layout) = self.payload_layout.as_deref() {
            let rows: Vec<RowPtr> = keys.iter().map(|key| RowPtr::from_ptr(key.payload())).collect();
            for output in &self.outputs {
                if let ColumnSource::Payload(column) = output.source
                    && wanted(output.output)
                {
                    let ty = self.output_types[output.output].clone();
                    let mut vector = Vector::flat(ty, count.max(1));
                    // SAFETY: the payload rows the keys point to are pinned (in-memory runs
                    // keep them pinned; external merges hold the pins in `keep_alive`).
                    unsafe {
                        crate::gather::gather_column(
                            layout,
                            column,
                            &rows,
                            None,
                            count,
                            0,
                            &mut vector,
                            None,
                        )?;
                    }
                    if let Some(keep_alive) = keep_alive
                        && crate::gather::has_heap_data(vector.logical_type())
                    {
                        vector.add_keep_alive(Arc::clone(keep_alive));
                    }
                    result[output.output] = Some(vector);
                }
            }
        }
        let missing = || Error::internal("sort output column was not produced");
        let columns = match columns {
            None => result
                .into_iter()
                .map(|column| column.ok_or_else(missing))
                .collect::<Result<Vec<_>>>()?,
            Some(columns) => columns
                .iter()
                .map(|&output| result.get(output).cloned().flatten().ok_or_else(missing))
                .collect::<Result<Vec<_>>>()?,
        };
        chunk.set_columns(columns, count);
        Ok(())
    }
}
