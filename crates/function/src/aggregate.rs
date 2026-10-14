//! Aggregate functions (DuckDB's `AggregateFunction`).
//!
//! Like DuckDB, aggregate states live as raw memory owned by the caller (inside hash
//! table rows, sort payloads or a single buffer for ungrouped aggregates), so the
//! low-level interface [`AggregateOps`] works on state pointers. Function authors
//! normally implement the safe [`UnaryAggregate`] trait instead and wrap it in
//! [`Unary`], which confines the pointer handling to this module.

use std::alloc::Layout;
use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::Arc;

use rustdb_common::Result;
use rustdb_types::{LogicalType, NativeType, Vector};

use crate::{FunctionData, NullHandling};

/// A pointer to one aggregate state inside caller-owned memory.
///
/// The memory must be valid for [`AggregateOps::state_layout`] (size and alignment) for
/// as long as the state is in use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct StatePtr(NonNull<u8>);

impl StatePtr {
    /// # Safety
    /// `ptr` must point to memory suitable for the aggregate's state layout.
    pub unsafe fn new(ptr: NonNull<u8>) -> Self {
        Self(ptr)
    }

    pub fn as_ptr(self) -> *mut u8 {
        self.0.as_ptr()
    }
}

// SAFETY: a `StatePtr` is a plain address; the code holding it is responsible for not
// using one state from several threads at once (DuckDB's ownership rules).
unsafe impl Send for StatePtr {}
// SAFETY: see above.
unsafe impl Sync for StatePtr {}

/// Data available to every aggregate call (DuckDB's `AggregateInputData`).
#[derive(Clone, Copy, Debug, Default)]
pub struct AggregateInput<'a> {
    pub bind_data: Option<&'a dyn FunctionData>,
}

/// The type-erased operations of an aggregate.
///
/// # Safety (for every method taking `StatePtr`s)
/// Each pointer must reference memory laid out per [`AggregateOps::state_layout`] that was
/// initialised with [`AggregateOps::initialize`] and not yet destroyed, and no state may
/// be accessed concurrently.
pub trait AggregateOps: Send + Sync + 'static {
    /// Size and alignment of one state.
    fn state_layout(&self) -> Layout;

    /// Whether states own resources that [`AggregateOps::destroy`] must release.
    fn needs_destructor(&self) -> bool;

    /// Writes an empty state.
    ///
    /// # Safety
    /// `state` points to uninitialised memory of the state layout.
    unsafe fn initialize(&self, state: StatePtr);

    /// Adds input row `i` (of `count`) to `states[i]`.
    ///
    /// # Safety
    /// See the trait-level contract; `states.len() >= count`.
    unsafe fn update(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        states: &[StatePtr],
        count: usize,
    ) -> Result<()>;

    /// Adds `count` input rows to one state (ungrouped aggregates).
    ///
    /// # Safety
    /// See the trait-level contract.
    unsafe fn simple_update(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        state: StatePtr,
        count: usize,
    ) -> Result<()>;

    /// Merges `source[i]` into `target[i]`.
    ///
    /// # Safety
    /// See the trait-level contract; the slices have equal length.
    unsafe fn combine(
        &self,
        source: &[StatePtr],
        target: &[StatePtr],
        input: AggregateInput<'_>,
    ) -> Result<()>;

    /// Writes the result of `states[i]` into row `offset + i` of `result`.
    ///
    /// # Safety
    /// See the trait-level contract.
    unsafe fn finalize(
        &self,
        states: &[StatePtr],
        input: AggregateInput<'_>,
        result: &mut Vector,
        offset: usize,
    ) -> Result<()>;

    /// Releases resources held by the states; they must not be used afterwards.
    ///
    /// # Safety
    /// See the trait-level contract.
    unsafe fn destroy(&self, states: &[StatePtr]);
}

/// Whether the result depends on input order (DuckDB's `AggregateOrderDependent`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OrderDependence {
    #[default]
    OrderDependent,
    NotOrderDependent,
}

/// Whether `DISTINCT` changes the result (DuckDB's `AggregateDistinctDependent`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DistinctDependence {
    #[default]
    DistinctDependent,
    NotDistinctDependent,
}

/// One overload of an aggregate function.
#[derive(Clone)]
pub struct AggregateFunction {
    pub name: Arc<str>,
    pub arguments: Vec<LogicalType>,
    pub varargs: Option<LogicalType>,
    pub return_type: LogicalType,
    pub ops: Arc<dyn AggregateOps>,
    pub null_handling: NullHandling,
    pub order_dependence: OrderDependence,
    pub distinct_dependence: DistinctDependence,
}

impl AggregateFunction {
    pub fn new(
        name: impl Into<Arc<str>>,
        arguments: Vec<LogicalType>,
        return_type: LogicalType,
        ops: impl AggregateOps,
    ) -> Self {
        Self {
            name: name.into(),
            arguments,
            varargs: None,
            return_type,
            ops: Arc::new(ops),
            null_handling: NullHandling::Default,
            order_dependence: OrderDependence::default(),
            distinct_dependence: DistinctDependence::default(),
        }
    }
}

impl fmt::Debug for AggregateFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AggregateFunction")
            .field("name", &self.name)
            .field("arguments", &self.arguments)
            .field("return_type", &self.return_type)
            .finish_non_exhaustive()
    }
}

/// All overloads registered under one name.
#[derive(Clone, Debug)]
pub struct AggregateFunctionSet {
    pub name: Arc<str>,
    pub functions: Vec<AggregateFunction>,
}

impl AggregateFunctionSet {
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self {
            name: name.into(),
            functions: Vec::new(),
        }
    }

    pub fn add(&mut self, function: AggregateFunction) {
        self.functions.push(function);
    }
}

/// A single-argument aggregate written in safe Rust (DuckDB's `AggregateFunction::
/// UnaryAggregate` template). NULL inputs are skipped.
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

/// # Safety
/// `state` must point to an initialised `S` that no one else is accessing.
unsafe fn state_mut<'a, S>(state: StatePtr) -> &'a mut S {
    // SAFETY: guaranteed by the caller.
    unsafe { &mut *state.as_ptr().cast::<S>() }
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
        for (row, &state) in states.iter().enumerate().take(count) {
            if let Some(value) = values.get(row) {
                // SAFETY: per the trait contract each state is initialised and exclusive.
                A::update(unsafe { state_mut(state) }, value)?;
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
        if input.kind() == rustdb_types::VectorKind::Constant {
            return match input.unified::<A::Input>().get(0) {
                Some(value) => A::update_repeated(state, value, count),
                None => Ok(()),
            };
        }
        let values = input.unified::<A::Input>();
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
        _: AggregateInput<'_>,
        result: &mut Vector,
        offset: usize,
    ) -> Result<()> {
        let capacity = result.capacity();
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
        let validity = result.validity_mut();
        for row in nulls {
            validity.set_invalid(row, capacity);
        }
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
    use rustdb_types::Value;

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
        let layout = ops.state_layout();
        let mut memory = vec![0u64; 2 * layout.size().div_ceil(8)];
        let base = NonNull::new(memory.as_mut_ptr().cast::<u8>()).unwrap();
        // SAFETY: `memory` is large and aligned enough for two states.
        let states = unsafe {
            [
                StatePtr::new(base),
                StatePtr::new(base.add(layout.size().next_multiple_of(8))),
            ]
        };

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
