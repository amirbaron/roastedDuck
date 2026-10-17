//! Single-argument aggregates over fixed-width values (DuckDB's
//! `AggregateFunction::UnaryAggregate` with `UnaryScatterUpdate`/`UnaryUpdate`).

use std::alloc::Layout;
use std::fmt;
use std::marker::PhantomData;

use rustdb_common::Result;
use rustdb_types::{NativeType, Sel, Vector, VectorKind};

use super::{AggregateInput, AggregateOps, StatePtr, set_nulls, state_mut};

/// A single-argument aggregate written in safe Rust. NULL inputs are skipped.
pub trait UnaryAggregate: Send + Sync + 'static {
    type Input: NativeType;
    type State: Default + Send + 'static;
    type Output: NativeType;

    fn update(state: &mut Self::State, value: Self::Input) -> Result<()>;

    /// Adds `value` `count` times; override when that can be done in O(1).
    fn update_repeated(state: &mut Self::State, value: Self::Input, count: usize) -> Result<()> {
        for _ in 0..count {
            Self::update(state, value)?;
        }
        Ok(())
    }

    fn combine(source: &Self::State, target: &mut Self::State) -> Result<()>;

    /// The result, or `None` for NULL.
    fn finalize(state: &Self::State) -> Result<Option<Self::Output>>;

    /// Like [`UnaryAggregate::finalize`], with access to the bind data.
    fn finalize_with(state: &Self::State, _input: AggregateInput<'_>) -> Result<Option<Self::Output>> {
        Self::finalize(state)
    }
}

/// Adapts a [`UnaryAggregate`] to [`AggregateOps`].
pub struct Unary<A>(PhantomData<fn() -> A>);

impl<A> Default for Unary<A> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<A> fmt::Debug for Unary<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(std::any::type_name::<A>())
    }
}

impl<A: UnaryAggregate> AggregateOps for Unary<A> {
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
        let values = inputs[0].unified::<A::Input>();
        let states = &states[..count];
        if values.validity.is_all_valid() {
            // `UnaryScatterLoop` without NULLs: no per-row validity or selection lookups.
            match values.sel {
                Sel::Identity => {
                    for (&state, &value) in states.iter().zip(&values.values[..count]) {
                        // SAFETY: per the trait contract each state is initialised and exclusive.
                        A::update(unsafe { state_mut(state) }, value)?;
                    }
                }
                Sel::Constant => {
                    let value = values.values[0];
                    for &state in states {
                        // SAFETY: as above.
                        A::update(unsafe { state_mut(state) }, value)?;
                    }
                }
                Sel::Indices(indices) => {
                    for (&state, &slot) in states.iter().zip(indices) {
                        // SAFETY: as above.
                        A::update(unsafe { state_mut(state) }, values.values[slot as usize])?;
                    }
                }
            }
            return Ok(());
        }
        if let (Sel::Identity, Some(words)) = (values.sel, values.validity.words()) {
            // Flat with NULLs: 64 rows per validity word (`UnaryScatterLoop` with
            // `ValidityMask::GetValidityEntry`); all-valid and all-NULL words skip the
            // per-row checks.
            for (block, (states, rows)) in states
                .chunks(64)
                .zip(values.values[..count].chunks(64))
                .enumerate()
            {
                let word = words.get(block).copied().unwrap_or(u64::MAX);
                if word == 0 {
                    continue;
                }
                for (bit, (&state, &value)) in states.iter().zip(rows).enumerate() {
                    if word == u64::MAX || word >> bit & 1 == 1 {
                        // SAFETY: per the trait contract each state is initialised and exclusive.
                        A::update(unsafe { state_mut(state) }, value)?;
                    }
                }
            }
            return Ok(());
        }
        for (row, &state) in states.iter().enumerate() {
            if let Some(value) = values.get(row) {
                // SAFETY: per the trait contract each state is initialised and exclusive.
                A::update(unsafe { state_mut(state) }, value)?;
            }
        }
        Ok(())
    }

    fn supports_update_selection(&self) -> bool {
        true
    }

    unsafe fn update_selection(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        state: StatePtr,
        sel: &[u32],
    ) -> Result<()> {
        // SAFETY: per the trait contract the state is initialised and exclusive.
        let state = unsafe { state_mut::<A::State>(state) };
        let values = inputs[0].unified::<A::Input>();
        // The same `A::update` calls as `update` makes for these rows, in the same order
        // (no `update_repeated`, which may round differently).
        if values.validity.is_all_valid() {
            match values.sel {
                // Unchecked reads, so the loop has no panic path and the state can stay
                // in registers.
                Sel::Identity => {
                    for &row in sel {
                        // SAFETY: `row` is a row of the flat input (the caller's contract).
                        A::update(state, unsafe { *values.values.get_unchecked(row as usize) })?;
                    }
                }
                Sel::Constant => {
                    let value = values.values[0];
                    for _ in sel {
                        A::update(state, value)?;
                    }
                }
                Sel::Indices(indices) => {
                    for &row in sel {
                        // SAFETY: `row` is a row of the input, whose selection maps it to
                        // a slot of its data.
                        let value = unsafe {
                            *values
                                .values
                                .get_unchecked(*indices.get_unchecked(row as usize) as usize)
                        };
                        A::update(state, value)?;
                    }
                }
            }
            return Ok(());
        }
        for &row in sel {
            if let Some(value) = values.get(row as usize) {
                A::update(state, value)?;
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
        let input = &inputs[0];
        if input.kind() == VectorKind::Constant {
            return match input.unified::<A::Input>().get(0) {
                Some(value) => A::update_repeated(state, value, count),
                None => Ok(()),
            };
        }
        let values = input.unified::<A::Input>();
        if values.validity.is_all_valid() {
            if let Sel::Identity = values.sel {
                for &value in &values.values[..count] {
                    A::update(state, value)?;
                }
            } else {
                for row in 0..count {
                    A::update(state, values.values[values.slot(row)])?;
                }
            }
            return Ok(());
        }
        if let (Sel::Identity, Some(words)) = (values.sel, values.validity.words()) {
            // Flat with NULLs: 64 rows per validity word.
            for (block, rows) in values.values[..count].chunks(64).enumerate() {
                let word = words.get(block).copied().unwrap_or(u64::MAX);
                if word == u64::MAX {
                    for &value in rows {
                        A::update(state, value)?;
                    }
                } else if word != 0 {
                    for (bit, &value) in rows.iter().enumerate() {
                        if word >> bit & 1 == 1 {
                            A::update(state, value)?;
                        }
                    }
                }
            }
            return Ok(());
        }
        for row in 0..count {
            if let Some(value) = values.get(row) {
                A::update(state, value)?;
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
        input: AggregateInput<'_>,
        result: &mut Vector,
        offset: usize,
    ) -> Result<()> {
        let mut nulls = Vec::new();
        {
            let data = result.values_mut::<A::Output>();
            for (i, &state) in states.iter().enumerate() {
                // SAFETY: per the trait contract.
                match A::finalize_with(unsafe { state_mut(state) }, input)? {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::test_support::StateBuffer;
    use rustdb_types::{LogicalType, Value};

    /// `sum(INTEGER)`-like aggregate for testing the adapter.
    struct SumInt;

    impl UnaryAggregate for SumInt {
        type Input = i32;
        type State = Option<i64>;
        type Output = i64;

        fn update(state: &mut Option<i64>, value: i32) -> Result<()> {
            *state = Some(state.unwrap_or(0) + i64::from(value));
            Ok(())
        }

        fn update_repeated(state: &mut Option<i64>, value: i32, count: usize) -> Result<()> {
            *state = Some(state.unwrap_or(0) + i64::from(value) * count as i64);
            Ok(())
        }

        fn combine(source: &Option<i64>, target: &mut Option<i64>) -> Result<()> {
            if let Some(value) = source {
                *target = Some(target.unwrap_or(0) + value);
            }
            Ok(())
        }

        fn finalize(state: &Option<i64>) -> Result<Option<i64>> {
            Ok(*state)
        }
    }

    #[test]
    fn unary_adapter_updates_combines_and_finalizes() {
        let ops = Unary::<SumInt>::default();
        let mut buffer = StateBuffer::new(&ops, 2);
        let states = [buffer.state(0), buffer.state(1)];

        let mut input = Vector::flat(LogicalType::INTEGER, 3);
        for (row, value) in [Some(1), None, Some(5)].into_iter().enumerate() {
            input
                .set_value(
                    row,
                    &value.map_or(Value::null(LogicalType::INTEGER), Value::integer),
                )
                .unwrap();
        }
        let constant = Vector::constant(&Value::integer(10)).unwrap();
        let mut result = Vector::flat(LogicalType::BIGINT, 2);
        // SAFETY: both states are initialised before use and destroyed once at the end.
        unsafe {
            for &state in &states {
                ops.initialize(state);
            }
            ops.simple_update(
                std::slice::from_ref(&input),
                AggregateInput::default(),
                states[0],
                3,
            )
            .unwrap();
            ops.simple_update(
                std::slice::from_ref(&constant),
                AggregateInput::default(),
                states[1],
                4,
            )
            .unwrap();
            ops.combine(&states[..1], &states[1..], AggregateInput::default())
                .unwrap();
            ops.finalize(&states, AggregateInput::default(), &mut result, 0)
                .unwrap();
            ops.destroy(&states);
        }
        assert_eq!(result.get_value(0), Value::bigint(6));
        assert_eq!(result.get_value(1), Value::bigint(46));
    }
}
