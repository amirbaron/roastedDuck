//! Blocks of aggregate states (DuckDB's `WindowAggregateStates` and the per-chunk result
//! states of the aggregators).

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::ptr::NonNull;
use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_function::aggregate::AggregateInput;
use roastedduck_function::{AggregateOps, StatePtr};
use roastedduck_types::Vector;

/// Aligned memory for a number of states of one aggregate; states are not initialised.
struct StateMemory {
    memory: Option<(NonNull<u8>, Layout)>,
    states: Vec<StatePtr>,
}

impl StateMemory {
    fn new(ops: &dyn AggregateOps, count: usize) -> Result<Self> {
        let layout = ops.state_layout().pad_to_align();
        let stride = layout.size();
        let size = stride
            .checked_mul(count)
            .ok_or_else(|| Error::internal("window aggregate states exceed the address space"))?;
        let mut memory = None;
        let base = if size == 0 {
            NonNull::<u8>::dangling()
        } else {
            let layout = Layout::from_size_align(size, layout.align())
                .map_err(|_| Error::internal("window aggregate states exceed the address space"))?;
            // SAFETY: the layout has a non-zero size.
            let pointer =
                NonNull::new(unsafe { alloc_zeroed(layout) }).unwrap_or_else(|| handle_alloc_error(layout));
            memory = Some((pointer, layout));
            pointer
        };
        let states = (0..count)
            // SAFETY: `index * stride < size` (or the state is zero-sized) and the stride
            // is a multiple of the state's alignment.
            .map(|index| unsafe { StatePtr::new(base.add(index * stride)) })
            .collect();
        Ok(Self { memory, states })
    }
}

impl Drop for StateMemory {
    fn drop(&mut self) {
        if let Some((memory, layout)) = self.memory.take() {
            // SAFETY: allocated in `new` with this layout.
            unsafe { dealloc(memory.as_ptr(), layout) };
        }
    }
}

/// `count` states that are initialised together and live until dropped (DuckDB's
/// `WindowAggregateStates`).
pub(crate) struct WindowAggregateStates {
    ops: Arc<dyn AggregateOps>,
    memory: StateMemory,
}

// SAFETY: the allocation is uniquely owned; states are mutated only through the owner,
// which hands out pointers to one thread at a time (sink/finalize of a hash group) and
// only reads them afterwards.
unsafe impl Send for WindowAggregateStates {}
// SAFETY: see above; shared access only reads the states.
unsafe impl Sync for WindowAggregateStates {}

impl std::fmt::Debug for WindowAggregateStates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowAggregateStates")
            .field("count", &self.memory.states.len())
            .finish_non_exhaustive()
    }
}

impl WindowAggregateStates {
    /// Allocates and initialises `count` states.
    pub(crate) fn new(ops: Arc<dyn AggregateOps>, count: usize) -> Result<Self> {
        let memory = StateMemory::new(ops.as_ref(), count)?;
        for &state in &memory.states {
            // SAFETY: the memory is reserved for this state and not yet initialised.
            unsafe { ops.initialize(state) };
        }
        Ok(Self { ops, memory })
    }

    /// The states.
    pub(crate) fn states(&self) -> &[StatePtr] {
        &self.memory.states
    }

    /// State `index`.
    pub(crate) fn state(&self, index: usize) -> StatePtr {
        self.memory.states[index]
    }

    /// Writes the result of state `i` into row `i` of `result`.
    pub(crate) fn finalize(&self, input: AggregateInput<'_>, result: &mut Vector) -> Result<()> {
        // SAFETY: the states are initialised.
        unsafe { self.ops.finalize(self.states(), input, result, 0) }
    }
}

impl Drop for WindowAggregateStates {
    fn drop(&mut self) {
        if !self.memory.states.is_empty() && self.ops.needs_destructor() {
            // SAFETY: the states are initialised and destroyed exactly once.
            unsafe { self.ops.destroy(&self.memory.states) };
        }
    }
}

/// Result states for one chunk of rows, initialised for each chunk and destroyed after
/// finalizing (the `statef` buffers of DuckDB's aggregators).
pub(crate) struct ScratchStates {
    ops: Arc<dyn AggregateOps>,
    memory: StateMemory,
    /// How many leading states are initialised.
    live: usize,
}

// SAFETY: as for `WindowAggregateStates`; scratch states belong to one local state.
unsafe impl Send for ScratchStates {}

impl std::fmt::Debug for ScratchStates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScratchStates")
            .field("capacity", &self.memory.states.len())
            .field("live", &self.live)
            .finish_non_exhaustive()
    }
}

impl ScratchStates {
    /// Room for `capacity` states.
    pub(crate) fn new(ops: Arc<dyn AggregateOps>, capacity: usize) -> Result<Self> {
        let memory = StateMemory::new(ops.as_ref(), capacity)?;
        Ok(Self { ops, memory, live: 0 })
    }

    /// Initialises the first `count` states (destroying the previous ones).
    pub(crate) fn begin(&mut self, count: usize) -> Result<()> {
        self.destroy();
        if count > self.memory.states.len() {
            return Err(Error::internal("too many window result states"));
        }
        for &state in &self.memory.states[..count] {
            // SAFETY: the memory is reserved for this state and not initialised.
            unsafe { self.ops.initialize(state) };
        }
        self.live = count;
        Ok(())
    }

    /// The live states.
    pub(crate) fn states(&self) -> &[StatePtr] {
        &self.memory.states[..self.live]
    }

    /// Live state `index`.
    pub(crate) fn state(&self, index: usize) -> StatePtr {
        self.states()[index]
    }

    /// Combines `other`'s live states into these, pairwise.
    pub(crate) fn combine_from(&self, other: &Self, input: AggregateInput<'_>) -> Result<()> {
        let count = self.live.min(other.live);
        // SAFETY: both hold initialised states of the same function in distinct blocks.
        unsafe {
            self.ops
                .combine(&other.states()[..count], &self.states()[..count], input)
        }
    }

    /// Finalizes the live states into rows `0..live` of `result` and destroys them.
    pub(crate) fn finish(&mut self, input: AggregateInput<'_>, result: &mut Vector) -> Result<()> {
        // SAFETY: the states are initialised.
        let finalized = unsafe { self.ops.finalize(self.states(), input, result, 0) };
        self.destroy();
        finalized
    }

    /// Destroys the live states.
    pub(crate) fn destroy(&mut self) {
        if self.live > 0 && self.ops.needs_destructor() {
            // SAFETY: the live states are initialised and destroyed exactly once.
            unsafe { self.ops.destroy(&self.memory.states[..self.live]) };
        }
        self.live = 0;
    }
}

impl Drop for ScratchStates {
    fn drop(&mut self) {
        self.destroy();
    }
}
