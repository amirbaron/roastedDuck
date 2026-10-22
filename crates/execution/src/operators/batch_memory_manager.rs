//! Throttling batch sinks against the memory limit (DuckDB's `BatchMemoryManager`,
//! `execution/operator/persistent/batch_memory_manager.hpp`).
//!
//! A batch sink buffers the batches above the lowest one still in progress until they can
//! be flushed in order. The manager tracks how much those unflushed batches take, asks the
//! temporary memory manager for room for them, and lets a sink that is ahead of the lowest
//! batch block when they take all of it; the sink of the lowest batch never blocks, and
//! the blocked ones are rescheduled when the lowest batch moves on.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rustdb_buffer::{BufferManager, TemporaryMemoryContext, TemporaryMemoryState};
use rustdb_common::{Error, Result};

use crate::SinkResult;
use crate::parallel::InterruptState;

/// The tasks a sink blocked (`StateWithBlockableTasks`).
#[derive(Debug)]
pub struct BlockableTasks {
    can_block: bool,
    blocked_tasks: Vec<InterruptState>,
}

impl Default for BlockableTasks {
    fn default() -> Self {
        Self {
            can_block: true,
            blocked_tasks: Vec::new(),
        }
    }
}

impl BlockableTasks {
    /// Keeps the task's interrupt state and blocks it, or finishes the sink once blocking
    /// was prevented (`BlockSink`).
    pub fn block_sink(&mut self, interrupt_state: &InterruptState) -> SinkResult {
        if self.can_block && interrupt_state.can_block() {
            self.blocked_tasks.push(interrupt_state.clone());
            SinkResult::Blocked
        } else {
            SinkResult::Finished
        }
    }

    /// Reschedules every blocked task; whether there was one (`UnblockTasks`).
    pub fn unblock_tasks(&mut self) -> bool {
        if self.blocked_tasks.is_empty() {
            return false;
        }
        for task in self.blocked_tasks.drain(..) {
            task.callback();
        }
        true
    }
}

/// See the module documentation.
#[derive(Debug)]
pub struct BatchMemoryManager {
    buffer_manager: BufferManager,
    threads: u64,
    /// The claim on temporary memory (`memory_state`).
    memory_state: TemporaryMemoryState,
    /// The bytes of the unflushed batches (`unflushed_memory_usage`).
    unflushed_memory_usage: AtomicU64,
    /// The lowest batch still in progress (`min_batch_index`).
    min_batch_index: AtomicU64,
    /// The bytes the unflushed batches may take (`available_memory`).
    available_memory: AtomicU64,
    /// Whether the manager may ask for more memory, and the blocked tasks.
    state: Mutex<ManagerState>,
}

#[derive(Debug)]
struct ManagerState {
    can_increase_memory: bool,
    tasks: BlockableTasks,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl BatchMemoryManager {
    /// Registers with the temporary memory manager and asks for `initial_memory_request`.
    pub fn new(buffer_manager: BufferManager, threads: usize, initial_memory_request: u64) -> Self {
        let threads = threads.max(1) as u64;
        let context = buffer_manager.temporary_memory_context(threads, 1, false);
        let memory_state = buffer_manager.temporary_memory_manager().register(&context);
        let manager = Self {
            buffer_manager,
            threads,
            memory_state,
            unflushed_memory_usage: AtomicU64::new(0),
            min_batch_index: AtomicU64::new(0),
            available_memory: AtomicU64::new(0),
            state: Mutex::new(ManagerState {
                can_increase_memory: true,
                tasks: BlockableTasks::default(),
            }),
        };
        manager.set_memory_size(&mut lock(&manager.state), initial_memory_request);
        manager
    }

    fn context(&self) -> TemporaryMemoryContext {
        self.buffer_manager
            .temporary_memory_context(self.threads, 1, false)
    }

    /// Asks for `size` bytes, at most a quarter of the query's memory (`SetMemorySize`).
    fn set_memory_size(&self, state: &mut ManagerState, size: u64) {
        let request_cap = self.buffer_manager.query_max_memory() / 4;
        let size = size.min(request_cap);
        let available = self.available_memory.load(Ordering::Acquire);
        if size <= available {
            return;
        }
        self.memory_state
            .set_remaining_size_and_update_reservation(&self.context(), size);
        let next_reservation = self.memory_state.reservation();
        if available >= next_reservation {
            // Asked for more and declined: stop asking.
            state.can_increase_memory = false;
        }
        self.available_memory
            .store(next_reservation, Ordering::Release);
    }

    /// `SetMemorySize` from outside (`MaxThreads`).
    pub fn request_memory(&self, size: u64) {
        self.set_memory_size(&mut lock(&self.state), size);
    }

    /// Doubles the memory asked for, unless asking was declined (`IncreaseMemory`).
    fn increase_memory(&self, state: &mut ManagerState) {
        if !state.can_increase_memory {
            return;
        }
        let doubled = self.available_memory.load(Ordering::Acquire) * 2;
        self.set_memory_size(state, doubled);
    }

    /// Whether the unflushed batches take all the memory and the sink of `batch_index`,
    /// not the lowest batch, should wait (`OutOfMemory`).
    pub fn out_of_memory(&self, batch_index: u64) -> bool {
        if self.unflushed_memory_usage.load(Ordering::Acquire) >= self.available_memory.load(Ordering::Acquire) {
            let mut state = lock(&self.state);
            if batch_index > self.min_batch_index.load(Ordering::Acquire) {
                // Out of memory and not the lowest batch: try to get more.
                self.increase_memory(&mut state);
                if self.unflushed_memory_usage.load(Ordering::Acquire)
                    >= self.available_memory.load(Ordering::Acquire)
                {
                    return true;
                }
            }
        }
        false
    }

    /// Moves the lowest batch on, rescheduling the blocked tasks (`UpdateMinBatchIndex`).
    pub fn update_min_batch_index(&self, current_min_batch_index: u64) {
        if self.min_batch_index.load(Ordering::Acquire) >= current_min_batch_index {
            return;
        }
        let mut state = lock(&self.state);
        let new_batch_index = self
            .min_batch_index
            .load(Ordering::Acquire)
            .max(current_min_batch_index);
        if new_batch_index != self.min_batch_index.load(Ordering::Acquire) {
            self.min_batch_index
                .store(new_batch_index, Ordering::Release);
            state.tasks.unblock_tasks();
        }
    }

    /// The bytes the unflushed batches may take (`AvailableMemory`).
    pub fn available_memory(&self) -> u64 {
        self.available_memory.load(Ordering::Acquire)
    }

    /// `IncreaseUnflushedMemory`.
    pub fn increase_unflushed_memory(&self, memory_increase: u64) {
        self.unflushed_memory_usage
            .fetch_add(memory_increase, Ordering::AcqRel);
    }

    /// `ReduceUnflushedMemory`.
    pub fn reduce_unflushed_memory(&self, memory_reduction: u64) -> Result<()> {
        self.unflushed_memory_usage
            .try_update(Ordering::AcqRel, Ordering::Acquire, |usage| {
                usage.checked_sub(memory_reduction)
            })
            .map(|_| ())
            .map_err(|_| Error::internal("Reducing unflushed memory usage below zero!?"))
    }

    /// Whether `batch_index` is the lowest batch in progress (`IsMinimumBatchIndex`).
    pub fn is_minimum_batch_index(&self, batch_index: u64) -> bool {
        batch_index <= self.min_batch_index.load(Ordering::Acquire)
    }

    /// Blocks the sink of `batch_index` unless the lowest batch reached it meanwhile
    /// (the locked part of `PhysicalBatchInsert::Sink`); `None` if it may go on.
    pub fn block_unless_minimum(&self, batch_index: u64, interrupt_state: &InterruptState) -> Option<SinkResult> {
        let mut state = lock(&self.state);
        if self.is_minimum_batch_index(batch_index) {
            return None;
        }
        Some(state.tasks.block_sink(interrupt_state))
    }

    /// Reschedules every blocked task; whether there was one (`UnblockTasks`).
    pub fn unblock_tasks(&self) -> bool {
        lock(&self.state).tasks.unblock_tasks()
    }

    /// The unflushed memory must be zero once every batch was flushed (`FinalCheck`).
    pub fn final_check(&self) -> Result<()> {
        let usage = self.unflushed_memory_usage.load(Ordering::Acquire);
        if usage != 0 {
            return Err(Error::internal(format!(
                "Unflushed memory usage is not zero at finalize but {usage}"
            )));
        }
        Ok(())
    }
}
