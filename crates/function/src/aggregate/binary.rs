//! Two-argument aggregates over fixed-width values (DuckDB's
//! `AggregateFunction::BinaryAggregate`): rows where either argument is NULL are skipped.

use std::alloc::Layout;
use std::fmt;
use std::marker::PhantomData;

use roastedduck_common::Result;
use roastedduck_types::{NativeType, Unified, Vector};

use super::{AggregateInput, AggregateOps, StatePtr, set_nulls, state_mut};

/// A two-argument aggregate written in safe Rust.
pub trait BinaryAggregate: Send + Sync + 'static {
    type A: NativeType;
    type B: NativeType;
    type State: Default + Send + 'static;
    type Output: NativeType;

    fn update(state: &mut Self::State, a: Self::A, b: Self::B) -> Result<()>;

    fn combine(source: &Self::State, target: &mut Self::State) -> Result<()>;

    /// The result, or `None` for NULL.
    fn finalize(state: &Self::State) -> Result<Option<Self::Output>>;
}

/// Adapts a [`BinaryAggregate`] to [`AggregateOps`].
pub struct Binary<A>(PhantomData<fn() -> A>);

impl<A> Default for Binary<A> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<A> fmt::Debug for Binary<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(std::any::type_name::<A>())
    }
}

/// Both arguments of row `row`, if neither is NULL.
#[inline]
fn pair<A: NativeType, B: NativeType>(a: &Unified<'_, A>, b: &Unified<'_, B>, row: usize) -> Option<(A, B)> {
    Some((a.get(row)?, b.get(row)?))
}

impl<A: BinaryAggregate> AggregateOps for Binary<A> {
    fn state_layout(&self) -> Layout {
        Layout::new::<A::State>()
    }

    fn needs_destructor(&self) -> bool {
        std::mem::needs_drop::<A::State>()
    }

    unsafe fn initialize(&self, state: StatePtr) {
        // SAFETY: the caller provides memory with `A::State`'s layout.
        unsafe { state.as_ptr().cast::<A::State>().write(A::State::default()) };
    }

    unsafe fn update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        states: &[StatePtr],
        count: usize,
    ) -> Result<()> {
        let a = inputs[0].unified::<A::A>();
        let b = inputs[1].unified::<A::B>();
        for (row, &state) in states.iter().enumerate().take(count) {
            if let Some((a, b)) = pair(&a, &b, row) {
                // SAFETY: per the trait contract each state is initialised and exclusive.
                A::update(unsafe { state_mut(state) }, a, b)?;
            }
        }
        Ok(())
    }

    unsafe fn simple_update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        state: StatePtr,
        count: usize,
    ) -> Result<()> {
        // SAFETY: per the trait contract the state is initialised and exclusive.
        let state = unsafe { state_mut::<A::State>(state) };
        let a = inputs[0].unified::<A::A>();
        let b = inputs[1].unified::<A::B>();
        for row in 0..count {
            if let Some((a, b)) = pair(&a, &b, row) {
                A::update(state, a, b)?;
            }
        }
        Ok(())
    }

    unsafe fn combine(&self, source: &[StatePtr], target: &[StatePtr], _: AggregateInput<'_>) -> Result<()> {
        for (&source, &target) in source.iter().zip(target) {
            // SAFETY: per the trait contract; source and target are distinct states.
            A::combine(unsafe { state_mut(source) }, unsafe { state_mut(target) })?;
        }
        Ok(())
    }

    unsafe fn finalize(
        &self,
        states: &[StatePtr],
        _: AggregateInput<'_>,
        result: &mut Vector,
        offset: usize,
    ) -> Result<()> {
        let mut nulls = Vec::new();
        {
            let data = result.values_mut::<A::Output>();
            for (i, &state) in states.iter().enumerate() {
                // SAFETY: per the trait contract.
                match A::finalize(unsafe { state_mut(state) })? {
                    Some(value) => data[offset + i] = value,
                    None => nulls.push(offset + i),
                }
            }
        }
        set_nulls(result, &nulls);
        Ok(())
    }

    unsafe fn destroy(&self, states: &[StatePtr]) {
        for &state in states {
            // SAFETY: per the trait contract the state is initialised and not used again.
            unsafe { std::ptr::drop_in_place(state.as_ptr().cast::<A::State>()) };
        }
    }
}
