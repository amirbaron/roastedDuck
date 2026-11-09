//! A single aggregate state in its own allocation, for code that runs an aggregate outside
//! of an aggregate operator (sorted aggregates, `list_aggregate`, ...).

use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::fmt;
use std::ptr::NonNull;
use std::sync::Arc;

use roastedduck_common::Result;
use roastedduck_types::Vector;

use super::{AggregateFunction, AggregateInput, AggregateOps, StatePtr};

/// An initialised state of one aggregate, destroyed and freed on drop.
pub struct AggregateState {
    ops: Arc<dyn AggregateOps>,
    memory: NonNull<u8>,
    layout: Layout,
}

// SAFETY: the state is only reachable through `&mut self` (or `&self` for read-only use in
// `combine_into`), and aggregate states are `Send` by the `AggregateOps` contract.
unsafe impl Send for AggregateState {}

impl fmt::Debug for AggregateState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AggregateState")
            .field("layout", &self.layout)
            .finish_non_exhaustive()
    }
}

impl AggregateState {
    /// A fresh (initialised) state of `function`.
    pub fn new(function: &AggregateFunction) -> Self {
        let ops = Arc::clone(&function.ops);
        // Zero-sized states still get a distinct, aligned allocation.
        let layout = ops.state_layout();
        let layout = Layout::from_size_align(layout.size().max(1), layout.align())
            .unwrap_or_else(|_| Layout::new::<u8>());
        // SAFETY: the layout has a non-zero size.
        let memory = NonNull::new(unsafe { alloc(layout) }).unwrap_or_else(|| handle_alloc_error(layout));
        let state = Self { ops, memory, layout };
        // SAFETY: freshly allocated memory of the state's layout.
        unsafe { state.ops.initialize(state.pointer()) };
        state
    }

    fn pointer(&self) -> StatePtr {
        // SAFETY: the allocation has the aggregate's state layout.
        unsafe { StatePtr::new(self.memory) }
    }

    /// Adds `count` rows of `inputs`.
    pub fn update(&mut self, inputs: &[Vector], input: AggregateInput<'_>, count: usize) -> Result<()> {
        // SAFETY: the state is initialised and exclusively borrowed.
        unsafe { self.ops.simple_update(inputs, input, self.pointer(), count) }
    }

    /// Merges this state into `target`, a state of the same aggregate.
    pub fn combine_into(&self, target: &mut Self, input: AggregateInput<'_>) -> Result<()> {
        if !Arc::ptr_eq(&self.ops, &target.ops) {
            return Err(roastedduck_common::Error::internal(
                "combining states of different aggregates",
            ));
        }
        // SAFETY: both states are initialised states of the same aggregate (checked above)
        // and distinct (`target` is borrowed mutably).
        unsafe { self.ops.combine(&[self.pointer()], &[target.pointer()], input) }
    }

    /// Writes the result into row `row` of `result`.
    pub fn finalize(&mut self, input: AggregateInput<'_>, result: &mut Vector, row: usize) -> Result<()> {
        // SAFETY: the state is initialised and exclusively borrowed.
        unsafe { self.ops.finalize(&[self.pointer()], input, result, row) }
    }
}

impl Drop for AggregateState {
    fn drop(&mut self) {
        // SAFETY: initialised in `new`, destroyed and freed exactly once here.
        unsafe {
            if self.ops.needs_destructor() {
                self.ops.destroy(&[self.pointer()]);
            }
            dealloc(self.memory.as_ptr(), self.layout);
        }
    }
}
