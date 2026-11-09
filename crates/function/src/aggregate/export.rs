//! `EXPORT_STATE` (DuckDB's `ExportAggregateFunction` in `aggregate_export.cpp`): an
//! aggregate whose result is its raw state, as an `AGGREGATE_STATE` value that
//! `finalize` and `combine` read back.

use std::alloc::Layout;
use std::any::TypeId;
use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_types::{LogicalType, StringT, Vector};

use super::{AggregateCallbackIdentity, AggregateFunction, AggregateInput, AggregateOps, StatePtr};
use crate::NullHandling;

/// The operations of an exported aggregate: those of the aggregate, except that
/// finalizing copies the state's bytes (`ExportAggregateFinalize`).
struct ExportOps {
    inner: Arc<dyn AggregateOps>,
}

impl AggregateOps for ExportOps {
    fn callback_identity(&self) -> AggregateCallbackIdentity {
        let mut identity = self.inner.callback_identity();
        identity.finalize = TypeId::of::<Self>();
        identity.window = None;
        identity
    }

    fn has_simple_update(&self) -> bool {
        self.inner.has_simple_update()
    }

    fn verify_state_layout(&self) -> Result<()> {
        self.inner.verify_state_layout()
    }

    fn state_layout(&self) -> Layout {
        self.inner.state_layout()
    }

    fn needs_destructor(&self) -> bool {
        self.inner.needs_destructor()
    }

    unsafe fn initialize(&self, state: StatePtr) {
        // SAFETY: forwarded under the caller's contract.
        unsafe { self.inner.initialize(state) }
    }

    unsafe fn update(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        states: &[StatePtr],
        count: usize,
    ) -> Result<()> {
        // SAFETY: forwarded under the caller's contract.
        unsafe { self.inner.update(inputs, input, states, count) }
    }

    unsafe fn simple_update(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        state: StatePtr,
        count: usize,
    ) -> Result<()> {
        // SAFETY: forwarded under the caller's contract.
        unsafe { self.inner.simple_update(inputs, input, state, count) }
    }

    fn supports_update_selection(&self) -> bool {
        self.inner.supports_update_selection()
    }

    unsafe fn update_selection(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        state: StatePtr,
        sel: &[u32],
    ) -> Result<()> {
        // SAFETY: forwarded under the caller's contract.
        unsafe { self.inner.update_selection(inputs, input, state, sel) }
    }

    unsafe fn combine(
        &self,
        source: &[StatePtr],
        target: &[StatePtr],
        input: AggregateInput<'_>,
    ) -> Result<()> {
        // SAFETY: forwarded under the caller's contract.
        unsafe { self.inner.combine(source, target, input) }
    }

    unsafe fn finalize(
        &self,
        states: &[StatePtr],
        _input: AggregateInput<'_>,
        result: &mut Vector,
        offset: usize,
    ) -> Result<()> {
        let size = self.inner.state_layout().size();
        for (i, state) in states.iter().enumerate() {
            // SAFETY: each state is `size` initialised bytes (the trait contract); the
            // exported aggregates' states hold no pointers (see `export_aggregate`).
            let bytes = unsafe { std::slice::from_raw_parts(state.as_ptr().cast_const(), size) };
            let value = result.add_string(bytes);
            result.values_mut::<StringT>()[offset + i] = value;
        }
        Ok(())
    }

    unsafe fn destroy(&self, states: &[StatePtr]) {
        // SAFETY: forwarded under the caller's contract.
        unsafe { self.inner.destroy(states) }
    }
}

/// `ExportAggregateFunction::Bind`: the aggregate `function`, bound for its arguments,
/// returning its state instead of its result.
pub fn export_aggregate(function: &AggregateFunction) -> Result<AggregateFunction> {
    if !function.ops.can_aggregate() {
        return Err(Error::binder(format!(
            "Cannot use EXPORT_STATE for non-combinable function {}",
            function.name
        )));
    }
    if function.bind.is_some() {
        return Err(Error::binder(
            "Cannot use EXPORT_STATE on aggregate functions with custom binders",
        ));
    }
    // States that own memory cannot leave the aggregate as bytes; DuckDB keeps such
    // states in the aggregate's arena, which its custom binders already rule out.
    if function.has_state_destructor || function.ops.needs_destructor() {
        return Err(Error::binder(
            "Cannot use EXPORT_STATE on aggregate functions with custom destructors",
        ));
    }
    let return_type = LogicalType::aggregate_state(
        function.name.to_string(),
        function.return_type.clone(),
        function.arguments.clone(),
    );
    let mut export = AggregateFunction::new(
        format!("aggregate_state_export_{}", function.name),
        function.arguments.clone(),
        return_type,
        ExportOps {
            inner: Arc::clone(&function.ops),
        },
    );
    export.null_handling = NullHandling::Special;
    // `ExportStateAggregateSerialize`.
    export.serialize = Some(Arc::new(|_, _, _| {
        Err(Error::not_implemented("FIXME: export state serialize"))
    }));
    Ok(export)
}
