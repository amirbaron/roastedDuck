//! Aggregate states stored in rows (DuckDB's `RowOperations` aggregate functions).
//!
//! A layout created with [`crate::TupleDataLayout::with_aggregates`] reserves an aligned
//! state for every [`crate::RowAggregate`] after the data columns. These helpers drive the
//! aggregate functions' [`AggregateOps`] over the states of many rows at once, using the
//! state pointer `row + layout.offsets()[column_count + aggregate]`.

use std::ptr::NonNull;

use rustdb_common::{Error, Result};
use rustdb_function::StatePtr;
use rustdb_function::aggregate::AggregateInput;
use rustdb_types::Vector;

use crate::layout::{RowAggregate, TupleDataLayout};
use crate::row::RowPtr;

/// Reusable scratch space for aggregate row operations (DuckDB's `RowOperationsState`).
#[derive(Debug, Default)]
pub struct RowOperationsState {
    states: Vec<StatePtr>,
    targets: Vec<StatePtr>,
}

fn input(aggregate: &RowAggregate) -> AggregateInput<'_> {
    AggregateInput {
        bind_data: aggregate.bind_data.as_deref(),
    }
}

/// The state pointers of aggregate `index` of `rows`.
///
/// # Safety
/// `rows` must be rows of `layout` (non-null).
unsafe fn state_pointers(
    layout: &TupleDataLayout,
    index: usize,
    rows: impl Iterator<Item = RowPtr>,
    out: &mut Vec<StatePtr>,
) -> Result<()> {
    let offset = layout.offsets()[layout.column_count() + index];
    out.clear();
    for row in rows {
        let pointer = NonNull::new(row.as_ptr().wrapping_add(offset))
            .ok_or_else(|| Error::internal("aggregate state of a null row"))?;
        // SAFETY: rows of the layout hold an aligned state for every aggregate.
        out.push(unsafe { StatePtr::new(pointer) });
    }
    Ok(())
}

/// Initialises the aggregate states of rows `sel[..count]` (or the first `count`)
/// (`RowOperations::InitializeStates`).
///
/// # Safety
/// The rows must be pinned rows of `layout` whose states are not initialised yet.
pub unsafe fn initialize_states(
    layout: &TupleDataLayout,
    rows: &[RowPtr],
    sel: Option<&[u32]>,
    count: usize,
) -> Result<()> {
    let mut states = Vec::with_capacity(count);
    for (index, aggregate) in layout.aggregates().iter().enumerate() {
        let selected = (0..count).map(|i| rows[sel.map_or(i, |sel| sel[i] as usize)]);
        // SAFETY: forwarded from the caller.
        unsafe {
            state_pointers(layout, index, selected, &mut states)?;
            for &state in &states {
                aggregate.function.ops.initialize(state);
            }
        }
    }
    Ok(())
}

/// Adds input row `i` (of `count`) to the state of aggregate `index` in `rows[i]`
/// (`RowOperations::UpdateStates`). `inputs` are the aggregate's argument vectors.
///
/// # Safety
/// The rows must be pinned rows of `layout` with initialised states, not accessed
/// concurrently.
pub unsafe fn update_states(
    state: &mut RowOperationsState,
    layout: &TupleDataLayout,
    index: usize,
    rows: &[RowPtr],
    inputs: &[Vector],
    count: usize,
) -> Result<()> {
    let aggregate = &layout.aggregates()[index];
    // SAFETY: forwarded from the caller.
    unsafe {
        state_pointers(layout, index, rows[..count].iter().copied(), &mut state.states)?;
        aggregate
            .function
            .ops
            .update(inputs, input(aggregate), &state.states, count)
    }
}

/// Merges the states of `sources[i]` into `targets[i]` for every aggregate
/// (`RowOperations::CombineStates`).
///
/// # Safety
/// Both row sets must be pinned rows of `layout` with initialised states; sources and
/// targets must be distinct rows.
pub unsafe fn combine_states(
    state: &mut RowOperationsState,
    layout: &TupleDataLayout,
    sources: &[RowPtr],
    targets: &[RowPtr],
    count: usize,
) -> Result<()> {
    for (index, aggregate) in layout.aggregates().iter().enumerate() {
        // SAFETY: forwarded from the caller.
        unsafe {
            state_pointers(layout, index, sources[..count].iter().copied(), &mut state.states)?;
            state_pointers(
                layout,
                index,
                targets[..count].iter().copied(),
                &mut state.targets,
            )?;
            aggregate
                .function
                .ops
                .combine(&state.states, &state.targets, input(aggregate))?;
        }
    }
    Ok(())
}

/// Writes the results of every aggregate of `rows[..count]` into `results` (one vector
/// per aggregate, rows `offset..offset + count`) (`RowOperations::FinalizeStates`).
///
/// # Safety
/// The rows must be pinned rows of `layout` with initialised states.
pub unsafe fn finalize_states(
    state: &mut RowOperationsState,
    layout: &TupleDataLayout,
    rows: &[RowPtr],
    count: usize,
    results: &mut [Vector],
    offset: usize,
) -> Result<()> {
    for (index, (aggregate, result)) in layout.aggregates().iter().zip(results).enumerate() {
        // SAFETY: forwarded from the caller.
        unsafe {
            state_pointers(layout, index, rows[..count].iter().copied(), &mut state.states)?;
            aggregate
                .function
                .ops
                .finalize(&state.states, input(aggregate), result, offset)?;
        }
    }
    Ok(())
}

/// Releases resources of the aggregate states of `rows[..count]` that need it
/// (`RowOperations::DestroyStates`).
///
/// # Safety
/// The rows must be pinned rows of `layout` with initialised states that are not used
/// afterwards.
pub unsafe fn destroy_states(
    state: &mut RowOperationsState,
    layout: &TupleDataLayout,
    rows: &[RowPtr],
    count: usize,
) -> Result<()> {
    for &index in layout.aggregate_destructor_indices() {
        let aggregate = &layout.aggregates()[index];
        // SAFETY: forwarded from the caller.
        unsafe {
            state_pointers(layout, index, rows[..count].iter().copied(), &mut state.states)?;
            aggregate.function.ops.destroy(&state.states);
        }
    }
    Ok(())
}
