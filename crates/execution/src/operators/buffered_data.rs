//! The chunks a streaming result collector buffers for the client (DuckDB's
//! `BufferedData`, `SimpleBufferedData` and `BatchedBufferedData`, `main/buffered_data`).
//!
//! The collector's sink appends chunks until `streaming_buffer_size` bytes are buffered,
//! then blocks: its task is descheduled with its chunk until a fetch has taken enough
//! chunks out (`UnblockSinks`), so a query whose result is read slowly does not run ahead
//! of the reader.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_types::DataChunk;

use crate::parallel::InterruptState;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The buffer of a streaming result whose rows arrive in order or need none
/// (`SimpleBufferedData`, filled by `PhysicalBufferedCollector`).
#[derive(Debug)]
pub struct SimpleBufferedData {
    state: Mutex<SimpleState>,
    /// The bytes of the buffered chunks (`buffered_count`).
    buffered_count: AtomicUsize,
    /// `streaming_buffer_size`.
    buffer_size: usize,
}

#[derive(Debug, Default)]
struct SimpleState {
    chunks: VecDeque<DataChunk>,
    /// The tasks whose sink blocked (`blocked_sinks`).
    blocked_sinks: VecDeque<InterruptState>,
}

impl SimpleBufferedData {
    fn new(buffer_size: usize) -> Self {
        Self {
            state: Mutex::default(),
            buffered_count: AtomicUsize::new(0),
            buffer_size,
        }
    }

    /// `BufferIsFull`.
    fn buffer_is_full(&self) -> bool {
        self.buffered_count.load(Ordering::Acquire) >= self.buffer_size
    }

    /// Reschedules enough blocked sinks to fill the buffer again (`UnblockSinks`).
    fn unblock_sinks(&self) {
        if self.buffer_is_full() {
            return;
        }
        let mut state = lock(&self.state);
        while let Some(blocked_sink) = state.blocked_sinks.front() {
            if self.buffer_is_full() {
                break;
            }
            blocked_sink.callback();
            state.blocked_sinks.pop_front();
        }
    }

    /// `Scan`: the oldest buffered chunk.
    fn scan(&self) -> Option<DataChunk> {
        let chunk = lock(&self.state).chunks.pop_front()?;
        self.buffered_count
            .fetch_sub(chunk.allocation_size(), Ordering::AcqRel);
        Some(chunk)
    }

    /// The sink's step (`PhysicalBufferedCollector::Sink`): appends a copy of `chunk`, or
    /// blocks while the buffer is full.
    pub fn sink(&self, chunk: &DataChunk, interrupt_state: &InterruptState) -> bool {
        let mut state = lock(&self.state);
        if self.buffer_is_full() && interrupt_state.can_block() {
            state.blocked_sinks.push_back(interrupt_state.clone());
            return false;
        }
        let chunk = chunk.compacted();
        let was_full = self.buffer_is_full();
        self.buffered_count
            .fetch_add(chunk.allocation_size(), Ordering::AcqRel);
        state.chunks.push_back(chunk);
        if !was_full && self.buffer_is_full() {
            // A fetch waiting for the buffer to fill can go on.
            interrupt_state.wake_query_thread();
        }
        true
    }
}

/// A batch whose chunks wait until every earlier batch was read (`InProgressBatch`).
#[derive(Debug, Default)]
struct InProgressBatch {
    chunks: VecDeque<DataChunk>,
    /// Whether the sink moved past the batch (`NextBatch` was called).
    completed: bool,
}

/// The buffer of a streaming result whose rows keep the order of their batch indices,
/// collected by several threads (`BatchedBufferedData`, filled by
/// `PhysicalBufferedBatchCollector`): the minimum batch goes to the read queue, the
/// others wait in the buffer.
#[derive(Debug)]
pub struct BatchedBufferedData {
    state: Mutex<BatchedState>,
    /// 60% of `streaming_buffer_size`.
    read_queue_capacity: usize,
    /// 40% of `streaming_buffer_size`.
    buffer_capacity: usize,
}

#[derive(Debug, Default)]
struct BatchedState {
    /// The chunks of batches above the minimum (`buffer`).
    buffer: BTreeMap<u64, InProgressBatch>,
    buffer_byte_count: usize,
    /// The chunks that can be read (`read_queue`).
    read_queue: VecDeque<DataChunk>,
    read_queue_byte_count: usize,
    blocked_sinks: BTreeMap<u64, InterruptState>,
    min_batch: u64,
    /// Verifies that the order is kept.
    lowest_moved_batch: u64,
}

impl BatchedBufferedData {
    fn new(total_buffer_size: usize) -> Self {
        Self {
            state: Mutex::default(),
            read_queue_capacity: (total_buffer_size as f64 * 0.6) as usize,
            buffer_capacity: (total_buffer_size as f64 * 0.4) as usize,
        }
    }

    /// `ShouldBlockBatch`.
    fn should_block_batch(&self, state: &BatchedState, batch: u64) -> bool {
        if state.min_batch == batch {
            // The minimum batch goes on while the read queue has room.
            return state.read_queue_byte_count >= self.read_queue_capacity;
        }
        state.buffer_byte_count >= self.buffer_capacity
    }

    /// `BufferIsEmpty`: whether the read queue is empty.
    fn buffer_is_empty(&self) -> bool {
        lock(&self.state).read_queue.is_empty()
    }

    /// Reschedules the blocked sinks that have room again (`UnblockSinks`).
    fn unblock_sinks(&self) {
        let mut state = lock(&self.state);
        let unblocked: Vec<u64> = state
            .blocked_sinks
            .keys()
            .copied()
            .filter(|&batch| !self.should_block_batch(&state, batch))
            .collect();
        for batch in unblocked {
            if let Some(blocked_sink) = state.blocked_sinks.remove(&batch) {
                blocked_sink.callback();
            }
        }
    }

    /// Moves the batches up to the minimum to the read queue, in order
    /// (`MoveCompletedBatches`).
    fn move_completed_batches(state: &mut BatchedState) -> Result<()> {
        let movable: Vec<u64> = state
            .buffer
            .keys()
            .copied()
            .take_while(|&batch| batch <= state.min_batch)
            .collect();
        for batch_index in movable {
            let Some(in_progress) = state.buffer.remove(&batch_index) else {
                continue;
            };
            let mut batch_allocation_size = 0;
            for chunk in in_progress.chunks {
                batch_allocation_size += chunk.allocation_size();
                state.read_queue.push_back(chunk);
            }
            if state.lowest_moved_batch > batch_index {
                return Err(Error::internal(format!(
                    "Lowest moved batch is {}, attempted to move {batch_index} afterwards\nAttempted to move chunks, of {batch_allocation_size} bytes in total\nmin_batch is {}",
                    state.lowest_moved_batch, state.min_batch
                )));
            }
            state.lowest_moved_batch = batch_index;
            state.buffer_byte_count -= batch_allocation_size;
            state.read_queue_byte_count += batch_allocation_size;
        }
        Ok(())
    }

    /// `UpdateMinBatchIndex`.
    pub fn update_min_batch_index(
        &self,
        min_batch_index: u64,
        interrupt_state: &InterruptState,
    ) -> Result<()> {
        let mut state = lock(&self.state);
        let new_min_batch = state.min_batch.max(min_batch_index);
        if new_min_batch == state.min_batch {
            return Ok(());
        }
        state.min_batch = new_min_batch;
        let was_empty = state.read_queue.is_empty();
        Self::move_completed_batches(&mut state)?;
        if was_empty && !state.read_queue.is_empty() {
            interrupt_state.wake_query_thread();
        }
        Ok(())
    }

    /// `CompleteBatch`.
    pub fn complete_batch(&self, batch: u64) {
        if let Some(in_progress) = lock(&self.state).buffer.get_mut(&batch) {
            in_progress.completed = true;
        }
    }

    /// `Scan`: the next chunk of the read queue.
    fn scan(&self) -> Option<DataChunk> {
        let mut state = lock(&self.state);
        let chunk = state.read_queue.pop_front()?;
        state.read_queue_byte_count -= chunk.allocation_size();
        Some(chunk)
    }

    /// The sink's step (`PhysicalBufferedBatchCollector::Sink`) for a chunk of `batch`:
    /// appends a copy of it, or blocks while its queue is full.
    pub fn sink(&self, chunk: &DataChunk, batch: u64, interrupt_state: &InterruptState) -> Result<bool> {
        let mut state = lock(&self.state);
        if self.should_block_batch(&state, batch) && interrupt_state.can_block() {
            state.blocked_sinks.insert(batch, interrupt_state.clone());
            return Ok(false);
        }
        // `Append`.
        let chunk = chunk.compacted();
        let allocation_size = chunk.allocation_size();
        if state.min_batch == batch {
            if state
                .buffer
                .keys()
                .next()
                .is_some_and(|&first| first < state.min_batch)
            {
                return Err(Error::internal("Batches remaining in buffer"));
            }
            let was_empty = state.read_queue.is_empty();
            state.read_queue.push_back(chunk);
            state.read_queue_byte_count += allocation_size;
            if was_empty {
                // A fetch waiting for a chunk to read can go on.
                interrupt_state.wake_query_thread();
            }
        } else {
            let in_progress = state.buffer.entry(batch).or_default();
            in_progress.completed = false;
            in_progress.chunks.push_back(chunk);
            state.buffer_byte_count += allocation_size;
        }
        Ok(true)
    }
}

/// The buffer a streaming result reads from (`BufferedData`).
#[derive(Debug)]
pub enum BufferedData {
    Simple(SimpleBufferedData),
    Batched(BatchedBufferedData),
}

impl BufferedData {
    /// A buffer for a collector that keeps arrival order (`SimpleBufferedData`).
    pub fn simple(buffer_size: usize) -> Self {
        Self::Simple(SimpleBufferedData::new(buffer_size))
    }

    /// A buffer for a collector that orders batches (`BatchedBufferedData`).
    pub fn batched(buffer_size: usize) -> Self {
        Self::Batched(BatchedBufferedData::new(buffer_size))
    }

    /// Whether a chunk can be fetched without running the query
    /// (`ExecuteTaskInternal`'s first check): the simple buffer is full, the batched read
    /// queue is not empty.
    pub fn chunk_ready_before_execution(&self) -> bool {
        match self {
            Self::Simple(data) => data.buffer_is_full(),
            Self::Batched(data) => !data.buffer_is_empty(),
        }
    }

    /// Whether a chunk can be fetched after running the query for a step
    /// (`ExecuteTaskInternal`'s second check).
    pub fn chunk_ready_after_execution(&self) -> bool {
        match self {
            Self::Simple(data) => data.buffer_is_full(),
            Self::Batched(data) => !data.buffer_is_empty(),
        }
    }

    /// Reschedules blocked sinks that have room again (`UnblockSinks`).
    pub fn unblock_sinks(&self) {
        match self {
            Self::Simple(data) => data.unblock_sinks(),
            Self::Batched(data) => data.unblock_sinks(),
        }
    }

    /// The next chunk for the client, `None` when nothing is buffered (`Scan`).
    pub fn scan(&self) -> Option<DataChunk> {
        match self {
            Self::Simple(data) => data.scan(),
            Self::Batched(data) => data.scan(),
        }
    }
}
