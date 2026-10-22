//! Aggregate functions (DuckDB's `AggregateFunction`).
//!
//! Like DuckDB, aggregate states live as raw memory owned by the caller (inside hash
//! table rows, sort payloads or a single buffer for ungrouped aggregates), so the
//! low-level interface [`AggregateOps`] works on state pointers. Function authors
//! implement one of the safe traits instead, which confine the pointer handling to this
//! module:
//!
//! - [`UnaryAggregate`] (wrapped in [`Unary`]): one fixed-width input, NULLs skipped;
//!   DuckDB's `AggregateFunction::UnaryAggregate`.
//! - [`BinaryAggregate`] (wrapped in [`Binary`]): two fixed-width inputs, rows with a
//!   NULL in either skipped; DuckDB's `AggregateFunction::BinaryAggregate`.
//! - [`Aggregate`] (wrapped in [`Typed`]): full access to the input vectors and typed
//!   states, for aggregates over strings, nested values or several arguments.

mod binary;
mod export;
mod owned;
mod typed;
mod unary;

use std::alloc::Layout;
use std::fmt;
use std::ptr::NonNull;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, ValidityMask, Value, Vector};

use crate::{BindInput, FunctionData, NullHandling};

pub use binary::{Binary, BinaryAggregate};
pub use export::export_aggregate;
pub use owned::AggregateState;
pub use typed::{Aggregate, States, Typed};
pub use unary::{Unary, UnaryAggregate};

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

impl<'a> AggregateInput<'a> {
    /// The bind data downcast to `T`, or an internal error if it is missing or of
    /// another type.
    pub fn bind_data<T: FunctionData>(&self) -> Result<&'a T> {
        self.bind_data
            .and_then(|data| data.as_any().downcast_ref::<T>())
            .ok_or_else(|| Error::internal("aggregate called without its bind data"))
    }
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

    /// Whether [`AggregateOps::update_selection`] is implemented.
    fn supports_update_selection(&self) -> bool {
        false
    }

    /// Adds input rows `sel` (in order) to one state. The result is the state that
    /// [`AggregateOps::update`] leaves when those rows all point at `state`, so a grouped
    /// aggregate may add the rows of one group at a time.
    ///
    /// # Safety
    /// See the trait-level contract; every entry of `sel` is a row of `inputs`.
    unsafe fn update_selection(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        state: StatePtr,
        sel: &[u32],
    ) -> Result<()> {
        let _ = (inputs, input, state, sel);
        Err(Error::internal("aggregate without a selection update"))
    }

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

    /// Whether the function can be computed through update/combine/finalize (DuckDB's
    /// `AggregateFunction::CanAggregate`); window-only functions return `false`.
    fn can_aggregate(&self) -> bool {
        true
    }

    /// The function's own windowed evaluation, if it has one (DuckDB's `window` and
    /// `window_init` callbacks, used for holistic aggregates such as `quantile`).
    fn window(&self) -> Option<&dyn WindowAggregate> {
        None
    }
}

/// One subframe `[start, end)` of the rows of a window partition (DuckDB's `FrameBounds`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowFrame {
    pub start: usize,
    pub end: usize,
}

/// Bounds on how far a frame boundary lies from the current row (DuckDB's `FrameDelta`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameDelta {
    pub begin: i64,
    pub end: i64,
}

/// A window partition as seen by a [`WindowAggregate`] (DuckDB's `WindowPartitionInput`).
#[derive(Clone, Copy, Debug)]
pub struct WindowPartitionInput<'a> {
    /// The argument columns, one flat vector per argument holding every row.
    pub inputs: &'a [Vector],
    /// Rows in the partition data.
    pub count: usize,
    /// Per argument: whether no value is NULL.
    pub all_valid: &'a [bool],
    /// Rows that pass the aggregate's `FILTER` clause.
    pub filter_mask: &'a ValidityMask,
    /// Estimated ranges of the frame start and end relative to the current row.
    pub stats: [FrameDelta; 2],
}

/// A custom windowed evaluation of an aggregate (DuckDB's `aggregate_window_t` and
/// `aggregate_wininit_t`).
///
/// # Safety (for every method taking `StatePtr`s)
/// Each state must have been initialised with [`AggregateOps::initialize`] of the same
/// function and not be accessed concurrently.
pub trait WindowAggregate: Send + Sync {
    /// Whether [`WindowAggregate::window_init`] builds a state shared by all rows.
    fn has_window_init(&self) -> bool {
        false
    }

    /// Builds the shared state `global` for a partition (e.g. a sorted index).
    ///
    /// # Safety
    /// See the trait-level contract.
    unsafe fn window_init(
        &self,
        input: AggregateInput<'_>,
        partition: &WindowPartitionInput<'_>,
        global: StatePtr,
    ) -> Result<()> {
        let _ = (input, partition, global);
        Ok(())
    }

    /// Computes the aggregate over `frames` into row `row` of `result`; `local` is a
    /// per-thread scratch state kept across rows, `global` the shared state if any.
    ///
    /// # Safety
    /// See the trait-level contract.
    #[allow(clippy::too_many_arguments, reason = "mirrors DuckDB's aggregate_window_t")]
    unsafe fn window(
        &self,
        input: AggregateInput<'_>,
        partition: &WindowPartitionInput<'_>,
        global: Option<StatePtr>,
        local: StatePtr,
        frames: &[WindowFrame],
        result: &mut Vector,
        row: usize,
    ) -> Result<()>;
}

/// Placeholder operations of overloads that are always replaced by their bind callback
/// (DuckDB registers such overloads with null function pointers).
#[derive(Debug, Default)]
pub struct Unbound;

impl Unbound {
    fn error() -> Error {
        Error::internal("aggregate overload was used without being bound")
    }
}

impl AggregateOps for Unbound {
    fn state_layout(&self) -> Layout {
        Layout::new::<()>()
    }

    fn needs_destructor(&self) -> bool {
        false
    }

    unsafe fn initialize(&self, _: StatePtr) {}

    unsafe fn update(&self, _: &[Vector], _: AggregateInput<'_>, _: &[StatePtr], _: usize) -> Result<()> {
        Err(Self::error())
    }

    unsafe fn simple_update(&self, _: &[Vector], _: AggregateInput<'_>, _: StatePtr, _: usize) -> Result<()> {
        Err(Self::error())
    }

    unsafe fn combine(&self, _: &[StatePtr], _: &[StatePtr], _: AggregateInput<'_>) -> Result<()> {
        Err(Self::error())
    }

    unsafe fn finalize(&self, _: &[StatePtr], _: AggregateInput<'_>, _: &mut Vector, _: usize) -> Result<()> {
        Err(Self::error())
    }

    unsafe fn destroy(&self, _: &[StatePtr]) {}
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

/// Specialises a chosen aggregate overload for a call (DuckDB's
/// `bind_aggregate_function_t`); may rewrite `arguments`/`return_type` or replace the
/// whole function.
pub type AggregateBind = Arc<
    dyn Fn(&mut AggregateFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> + Send + Sync,
>;

/// How the binder applies collations to an aggregate's arguments (the
/// `ExpressionBinder::PushCollation` calls of DuckDB's aggregate binds).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AggregateCollation {
    /// Arguments are used as they are.
    #[default]
    None,
    /// A VARCHAR argument at this index is compared collated (`arg_min`/`arg_max`).
    Argument(usize),
    /// `BindMinMax`: a collated VARCHAR input turns `min(x)`/`max(x)` into
    /// `arg_min(x, collated x)`/`arg_max(x, collated x)`.
    MinMax,
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
    /// Called once per call site after overload resolution.
    pub bind: Option<AggregateBind>,
    /// How collations of the arguments are applied before `bind`.
    pub collation: AggregateCollation,
    /// Whether DuckDB's aggregate has a state destructor callback
    /// (`HasStateDestructorCallback`), which keeps it out of streaming windows. By
    /// default whether the Rust state needs dropping; aggregates whose DuckDB states
    /// live in the aggregate arena (`list`, `string_agg`) have none.
    pub has_state_destructor: bool,
    /// Called by statistics propagation with each argument's `(min, max)`, when known;
    /// returns new bind data (DuckDB's `statistics` callback with `AggregateStatisticsInput`,
    /// which may update the bind data).
    pub statistics: Option<AggregateStatistics>,
    /// The arguments before the bind removed some (DuckDB's `original_arguments`, set by
    /// `Function::EraseArgument`); empty when it removed none.
    pub original_arguments: Vec<LogicalType>,
    /// The catalog and schema of the function's catalog entry (DuckDB's `catalog_name`
    /// and `schema_name`); empty for a function not taken from a catalog.
    pub catalog_name: Arc<str>,
    pub schema_name: Arc<str>,
    /// Writes the bind data, so that a serialized plan can bind the function again
    /// (DuckDB's `serialize` callback).
    pub serialize: Option<AggregateSerialize>,
}

/// See [`AggregateFunction::serialize`] (DuckDB's `aggregate_serialize_t`).
pub type AggregateSerialize = Arc<
    dyn Fn(&mut dyn rustdb_serde::Serializer, Option<&dyn FunctionData>, &AggregateFunction) -> Result<()>
        + Send
        + Sync,
>;

/// See [`AggregateFunction::statistics`].
pub type AggregateStatistics = Arc<
    dyn Fn(Option<&Arc<dyn FunctionData>>, &[Option<(Value, Value)>]) -> Option<Arc<dyn FunctionData>>
        + Send
        + Sync,
>;

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
            bind: None,
            collation: AggregateCollation::None,
            has_state_destructor: false,
            statistics: None,
            original_arguments: Vec::new(),
            catalog_name: Arc::from(""),
            schema_name: Arc::from(""),
            serialize: None,
        }
        .with_state_destructor_of_ops()
    }

    /// Sets the catalog and schema of the function's catalog entry (see
    /// [`AggregateFunction::catalog_name`]); empty ones for a function the engine uses
    /// directly rather than through the catalog (DuckDB's `XFun::GetFunction()`).
    pub fn set_catalog_entry(&mut self, catalog: &Arc<str>, schema: &Arc<str>) {
        self.catalog_name = Arc::clone(catalog);
        self.schema_name = Arc::clone(schema);
    }

    /// Removes argument `index`, which the bind consumed, keeping the arguments as they
    /// were before the first removal (DuckDB's `Function::EraseArgument`).
    pub fn erase_argument(&mut self, index: usize) {
        if self.original_arguments.is_empty() {
            self.original_arguments.clone_from(&self.arguments);
        }
        self.arguments.remove(index);
    }

    fn with_state_destructor_of_ops(mut self) -> Self {
        self.has_state_destructor = self.ops.needs_destructor();
        self
    }

    /// Marks an aggregate whose DuckDB state has no destructor callback (see
    /// [`AggregateFunction::has_state_destructor`]).
    #[must_use]
    pub fn without_state_destructor(mut self) -> Self {
        self.has_state_destructor = false;
        self
    }

    /// An overload whose bind callback always replaces it with a concrete function.
    pub fn unbound(
        arguments: Vec<LogicalType>,
        return_type: LogicalType,
        bind: impl Fn(&mut AggregateFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self::new("", arguments, return_type, Unbound).with_bind(bind)
    }

    /// Sets the bind callback.
    #[must_use]
    pub fn with_bind(
        mut self,
        bind: impl Fn(&mut AggregateFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.bind = Some(Arc::new(bind));
        self
    }

    /// Sets the statistics callback (see [`AggregateFunction::statistics`]).
    #[must_use]
    pub fn with_statistics(
        mut self,
        statistics: impl Fn(
            Option<&Arc<dyn FunctionData>>,
            &[Option<(Value, Value)>],
        ) -> Option<Arc<dyn FunctionData>>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.statistics = Some(Arc::new(statistics));
        self
    }

    /// Sets how NULL inputs are treated.
    #[must_use]
    pub fn with_null_handling(mut self, null_handling: NullHandling) -> Self {
        self.null_handling = null_handling;
        self
    }

    /// Marks the result as independent of the input order.
    #[must_use]
    pub fn order_independent(mut self) -> Self {
        self.order_dependence = OrderDependence::NotOrderDependent;
        self
    }

    /// Marks the result as unaffected by `DISTINCT`.
    #[must_use]
    pub fn distinct_independent(mut self) -> Self {
        self.distinct_dependence = DistinctDependence::NotDistinctDependent;
        self
    }

    /// Sets the type of additional trailing arguments.
    #[must_use]
    pub fn with_varargs(mut self, varargs: LogicalType) -> Self {
        self.varargs = Some(varargs);
        self
    }

    /// Replaces the function by `other`, keeping this overload's name (what DuckDB's bind
    /// callbacks do with `function = GetFunction(...); function.name = name`).
    pub fn replace_with(&mut self, other: Self) {
        let name = std::mem::take(&mut self.name);
        *self = other;
        self.name = name;
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

    /// A set holding a single overload.
    pub fn single(name: impl Into<Arc<str>>, function: AggregateFunction) -> Self {
        let mut set = Self::new(name);
        set.add(function);
        set
    }

    pub fn add(&mut self, function: AggregateFunction) {
        self.functions.push(function);
    }

    /// The set under another name, every overload renamed too (DuckDB's
    /// `CreateAggregateFunctionInfo` names each function after its set).
    #[must_use]
    pub fn renamed(mut self, name: &str) -> Self {
        let name: Arc<str> = Arc::from(name);
        for function in &mut self.functions {
            function.name = Arc::clone(&name);
        }
        self.name = name;
        self
    }
}

/// Aggregate states in one allocation owned by the caller of a scalar function (e.g.
/// `list_aggregate`): initialised on creation, destroyed and freed on drop.
pub struct OwnedStates {
    ops: Arc<dyn AggregateOps>,
    memory: NonNull<u8>,
    layout: std::alloc::Layout,
    stride: usize,
    count: usize,
}

impl OwnedStates {
    /// `count` initialised states of `ops`.
    pub fn new(ops: Arc<dyn AggregateOps>, count: usize) -> Self {
        let state = ops.state_layout();
        let stride = state.size().max(1).next_multiple_of(state.align());
        let layout = Layout::from_size_align(stride * count.max(1), state.align())
            .unwrap_or_else(|_| Layout::new::<u128>());
        // SAFETY: the layout has a non-zero size.
        let memory = NonNull::new(unsafe { std::alloc::alloc_zeroed(layout) })
            .unwrap_or_else(|| std::alloc::handle_alloc_error(layout));
        let states = Self {
            ops,
            memory,
            layout,
            stride,
            count,
        };
        for state in states.pointers() {
            // SAFETY: `state` points to uninitialised memory of the state layout.
            unsafe { states.ops.initialize(state) };
        }
        states
    }

    /// Pointers to the states, in order.
    pub fn pointers(&self) -> Vec<StatePtr> {
        (0..self.count)
            .map(|index| {
                // SAFETY: the allocation holds `count` states of `stride` bytes each,
                // aligned to the state alignment.
                unsafe { StatePtr::new(self.memory.add(index * self.stride)) }
            })
            .collect()
    }
}

impl fmt::Debug for OwnedStates {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedStates")
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

impl Drop for OwnedStates {
    fn drop(&mut self) {
        if self.ops.needs_destructor() {
            // SAFETY: the states were initialised in `new` and are not used afterwards.
            unsafe { self.ops.destroy(&self.pointers()) };
        }
        // SAFETY: `memory` was allocated with `layout` in `new`.
        unsafe { std::alloc::dealloc(self.memory.as_ptr(), self.layout) };
    }
}

// SAFETY: the states are plain memory owned by this value; the aggregate ops are
// `Send + Sync`.
unsafe impl Send for OwnedStates {}

/// # Safety
/// `state` must point to an initialised `S` that no one else is accessing.
pub(crate) unsafe fn state_mut<'a, S>(state: StatePtr) -> &'a mut S {
    // SAFETY: guaranteed by the caller.
    unsafe { &mut *state.as_ptr().cast::<S>() }
}

/// Marks rows of a result vector NULL (finalize helpers).
pub(crate) fn set_nulls(result: &mut Vector, rows: &[usize]) {
    if rows.is_empty() {
        return;
    }
    let capacity = result.capacity();
    let validity = result.validity_mut();
    for &row in rows {
        validity.set_invalid(row, capacity);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::ptr::NonNull;

    use super::{AggregateOps, StatePtr};

    /// Aligned memory for `count` states of `ops`.
    pub struct StateBuffer {
        memory: Vec<u128>,
        stride: usize,
    }

    impl StateBuffer {
        pub fn new(ops: &dyn AggregateOps, count: usize) -> Self {
            let layout = ops.state_layout();
            assert!(layout.align() <= 16);
            let stride = layout.size().max(1).next_multiple_of(16);
            Self {
                memory: vec![0; count * stride / 16 + 1],
                stride,
            }
        }

        pub fn state(&mut self, index: usize) -> StatePtr {
            let base = NonNull::new(self.memory.as_mut_ptr().cast::<u8>()).unwrap();
            // SAFETY: the buffer holds `count` aligned states of `stride` bytes.
            unsafe { StatePtr::new(base.add(index * self.stride)) }
        }
    }
}
