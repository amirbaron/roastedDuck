//! Aggregates with typed states and full access to their input vectors (the aggregates
//! DuckDB writes with custom `update`/`simple_update` callbacks: `count`, `first`, `list`,
//! `string_agg`, `arg_min`, `quantile`, ...).

use std::alloc::Layout;
use std::fmt;
use std::marker::PhantomData;

use rustdb_common::Result;
use rustdb_types::Vector;

use super::{AggregateInput, AggregateOps, StatePtr, state_mut};

/// The states an update writes to: input row `i` belongs to the state returned by
/// [`States::get`]`(i)`. Several rows may share a state; only one state is borrowed at a
/// time.
pub struct States<'a, S> {
    inner: Inner<'a, S>,
}

impl<S> fmt::Debug for States<'_, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("States")
            .field("single", &self.is_single())
            .finish_non_exhaustive()
    }
}

enum Inner<'a, S> {
    /// One state per row (a scatter update); entries may alias.
    Scattered(&'a [StatePtr], PhantomData<&'a mut S>),
    /// Every row updates the same state (a simple update).
    Single(&'a mut S),
}

impl<'a, S> States<'a, S> {
    /// Every row updates `state`.
    pub fn single(state: &'a mut S) -> Self {
        Self {
            inner: Inner::Single(state),
        }
    }

    /// # Safety
    /// Every pointer must reference an initialised `S` that nothing else accesses while
    /// the returned value is alive.
    unsafe fn scattered(states: &'a [StatePtr]) -> Self {
        Self {
            inner: Inner::Scattered(states, PhantomData),
        }
    }

    /// The state of input row `row`.
    #[inline]
    pub fn get(&mut self, row: usize) -> &mut S {
        match &mut self.inner {
            // SAFETY: guaranteed by `scattered`'s contract; the `&mut self` borrow ensures
            // only one state is borrowed at a time even if pointers alias.
            Inner::Scattered(states, _) => unsafe { state_mut(states[row]) },
            Inner::Single(state) => state,
        }
    }

    /// Whether every row updates the same state.
    pub fn is_single(&self) -> bool {
        matches!(self.inner, Inner::Single(_))
    }

    /// The single state, if every row updates the same one.
    pub fn as_single(&mut self) -> Option<&mut S> {
        match &mut self.inner {
            Inner::Single(state) => Some(state),
            Inner::Scattered(..) => None,
        }
    }
}

/// An aggregate over typed states.
pub trait Aggregate: Send + Sync + 'static {
    type State: Send + 'static;

    /// An empty state.
    fn initialize(&self) -> Self::State;

    /// Adds rows `0..count` of `inputs` to their states.
    fn update(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        states: &mut States<'_, Self::State>,
        count: usize,
    ) -> Result<()>;

    /// Whether [`Aggregate::update_selection`] is implemented.
    const SELECTION_UPDATE: bool = false;

    /// Adds rows `sel` of `inputs` (in order) to one state, leaving the state
    /// [`Aggregate::update`] would with those rows all pointing at it. Only called when
    /// [`Aggregate::SELECTION_UPDATE`] is set.
    fn update_selection(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        state: &mut Self::State,
        sel: &[u32],
    ) -> Result<()> {
        let _ = (inputs, input, state, sel);
        Err(rustdb_common::Error::internal(
            "aggregate without a selection update",
        ))
    }

    /// Merges `source` into `target`.
    fn combine(
        &self,
        source: &Self::State,
        target: &mut Self::State,
        input: AggregateInput<'_>,
    ) -> Result<()>;

    /// Writes the result of `state` into row `row` of `result`.
    fn finalize(
        &self,
        state: &mut Self::State,
        input: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()>;

    /// Writes the results of `count` distinct states into rows `offset..offset + count` of
    /// `result`; override to process a batch at once (e.g. one sort for all states).
    fn finalize_all(
        &self,
        states: &mut States<'_, Self::State>,
        count: usize,
        input: AggregateInput<'_>,
        result: &mut Vector,
        offset: usize,
    ) -> Result<()> {
        for index in 0..count {
            self.finalize(states.get(index), input, result, offset + index)?;
        }
        Ok(())
    }
}

/// Adapts an [`Aggregate`] to [`AggregateOps`].
pub struct Typed<A>(pub A);

impl<A> fmt::Debug for Typed<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(std::any::type_name::<A>())
    }
}

impl<A: Aggregate> AggregateOps for Typed<A> {
    fn state_layout(&self) -> Layout {
        Layout::new::<A::State>()
    }

    fn needs_destructor(&self) -> bool {
        std::mem::needs_drop::<A::State>()
    }

    unsafe fn initialize(&self, state: StatePtr) {
        // SAFETY: the caller provides memory with `A::State`'s layout.
        unsafe { state.as_ptr().cast::<A::State>().write(self.0.initialize()) };
    }

    unsafe fn update(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        states: &[StatePtr],
        count: usize,
    ) -> Result<()> {
        // SAFETY: per the trait contract the states are initialised and exclusive.
        let mut states = unsafe { States::scattered(&states[..count]) };
        self.0.update(inputs, input, &mut states, count)
    }

    unsafe fn simple_update(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        state: StatePtr,
        count: usize,
    ) -> Result<()> {
        // SAFETY: per the trait contract the state is initialised and exclusive.
        let state = unsafe { state_mut::<A::State>(state) };
        self.0.update(inputs, input, &mut States::single(state), count)
    }

    fn supports_update_selection(&self) -> bool {
        A::SELECTION_UPDATE
    }

    unsafe fn update_selection(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        state: StatePtr,
        sel: &[u32],
    ) -> Result<()> {
        // SAFETY: per the trait contract the state is initialised and exclusive.
        let state = unsafe { state_mut::<A::State>(state) };
        self.0.update_selection(inputs, input, state, sel)
    }

    unsafe fn combine(
        &self,
        source: &[StatePtr],
        target: &[StatePtr],
        input: AggregateInput<'_>,
    ) -> Result<()> {
        for (&source, &target) in source.iter().zip(target) {
            // SAFETY: per the trait contract; source and target are distinct states.
            self.0
                .combine(unsafe { state_mut(source) }, unsafe { state_mut(target) }, input)?;
        }
        Ok(())
    }

    unsafe fn finalize(
        &self,
        states: &[StatePtr],
        input: AggregateInput<'_>,
        result: &mut Vector,
        offset: usize,
    ) -> Result<()> {
        // SAFETY: per the trait contract the states are initialised, distinct and exclusive.
        let mut scattered = unsafe { States::scattered(states) };
        self.0
            .finalize_all(&mut scattered, states.len(), input, result, offset)
    }

    unsafe fn destroy(&self, states: &[StatePtr]) {
        for &state in states {
            // SAFETY: per the trait contract the state is initialised and not used again.
            unsafe { std::ptr::drop_in_place(state.as_ptr().cast::<A::State>()) };
        }
    }
}
