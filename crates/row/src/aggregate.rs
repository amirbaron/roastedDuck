//! Aggregate states stored in rows (DuckDB's `RowOperations` aggregate functions).
//!
//! A layout created with [`crate::TupleDataLayout::with_aggregates`] reserves an aligned
//! state for every [`crate::RowAggregate`] after the data columns. These helpers drive the
//! aggregate functions' [`AggregateOps`] over the states of many rows at once, using the
//! state pointer `row + layout.offsets()[column_count + aggregate]`.

use std::ptr::NonNull;

use roastedduck_common::Result;
use roastedduck_function::StatePtr;
use roastedduck_function::aggregate::AggregateInput;
use roastedduck_types::Vector;

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
) {
    let offset = layout.offsets()[layout.column_count() + index];
    out.clear();
    // One pass without per-row capacity or null checks (DuckDB's `AddInPlace` on the
    // address vector); `rows` are non-null per the caller contract.
    out.extend(rows.map(|row| {
        // SAFETY: rows of the layout are non-null and hold an aligned state for every
        // aggregate at `offset`.
        unsafe { StatePtr::new(NonNull::new_unchecked(row.as_ptr().wrapping_add(offset))) }
    }));
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
            state_pointers(layout, index, selected, &mut states);
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
        state_pointers(layout, index, rows[..count].iter().copied(), &mut state.states);
        aggregate
            .function
            .ops
            .update(inputs, input(aggregate), &state.states, count)
    }
}

/// Whether every aggregate of `layout` can add a selection of rows to one state
/// ([`AggregateOps::update_selection`](roastedduck_function::aggregate::AggregateOps)).
pub fn supports_update_selection(layout: &TupleDataLayout) -> bool {
    layout
        .aggregates()
        .iter()
        .all(|aggregate| aggregate.function.ops.supports_update_selection())
}

/// Adds input rows `sel` (in order) to the state of aggregate `index` in `row`: the
/// result of [`update_states`] with those rows all pointing at `row`.
///
/// # Safety
/// `row` must be a pinned row of `layout` with initialised states, not accessed
/// concurrently; the aggregate must support selection updates.
pub unsafe fn update_state_selection(
    layout: &TupleDataLayout,
    index: usize,
    row: RowPtr,
    inputs: &[Vector],
    sel: &[u32],
) -> Result<()> {
    let aggregate = &layout.aggregates()[index];
    let offset = layout.offsets()[layout.column_count() + index];
    // SAFETY: rows of the layout are non-null and hold an aligned state for every
    // aggregate at `offset`; the rest is forwarded from the caller.
    unsafe {
        let state = StatePtr::new(NonNull::new_unchecked(row.as_ptr().wrapping_add(offset)));
        aggregate
            .function
            .ops
            .update_selection(inputs, input(aggregate), state, sel)
    }
}

/// Adds the rows of a chunk to their states one group at a time when the chunk has few
/// groups: each state then takes all its rows in one call, in row order, so it ends as
/// the row-at-a-time [`update_states`] leaves it, without the store-to-load chains of
/// updating the same few states row after row.
#[derive(Debug, Default)]
pub struct GroupedUpdate {
    /// The distinct state rows of the chunk.
    present: Vec<RowPtr>,
    /// The input rows of one group.
    rows: Vec<u32>,
}

impl GroupedUpdate {
    /// The most groups a chunk may have (each group is one pass over the chunk).
    const MAX_GROUPS: usize = 8;
    /// The fewest rows per group, on average.
    const MIN_ROWS_PER_GROUP: usize = 16;

    /// Collects the distinct rows of `addresses` into `present` and returns whether there
    /// are few of them, with many rows each.
    fn few_groups(&mut self, addresses: &[RowPtr]) -> bool {
        self.present.clear();
        let Some(&first) = addresses.first() else {
            return false;
        };
        // Unused entries repeat the first row, so each row compares against all of them
        // without branches; only a new group branches.
        let mut known = [first; Self::MAX_GROUPS];
        let mut len = 1;
        for &address in addresses {
            #[allow(
                clippy::needless_bitwise_bool,
                reason = "compares all entries without branches"
            )]
            let found = known.iter().fold(false, |found, &row| found | (row == address));
            if !found {
                if len == Self::MAX_GROUPS {
                    return false;
                }
                known[len] = address;
                len += 1;
            }
        }
        self.present.extend_from_slice(&known[..len]);
        addresses.len() >= len * Self::MIN_ROWS_PER_GROUP
    }

    /// Adds input row `i` of `inputs[a]` to the state of aggregate `a` in `addresses[i]`
    /// for every aggregate, like [`update_states`], if the chunk has few groups; returns
    /// whether it did (nothing is updated otherwise). Every aggregate of `layout` must
    /// support selection updates ([`supports_update_selection`]).
    ///
    /// # Safety
    /// As [`update_states`]; `addresses` holds a row for each input row.
    pub unsafe fn update(
        &mut self,
        layout: &TupleDataLayout,
        addresses: &[RowPtr],
        inputs: &[Vec<Vector>],
    ) -> Result<bool> {
        if !self.few_groups(addresses) {
            return Ok(false);
        }
        for group in 0..self.present.len() {
            let target = self.present[group];
            // The rows of this group, in order (branch-free: every row is written, and
            // kept if it matches).
            self.rows.clear();
            self.rows.resize(addresses.len(), 0);
            let mut kept = 0;
            for (row, &address) in addresses.iter().enumerate() {
                self.rows[kept] = row as u32;
                kept += usize::from(address == target);
            }
            self.rows.truncate(kept);
            for (index, columns) in inputs.iter().enumerate() {
                // SAFETY: forwarded from the caller; the rows are rows of the inputs.
                unsafe { update_state_selection(layout, index, target, columns, &self.rows)? };
            }
        }
        Ok(true)
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
            state_pointers(layout, index, sources[..count].iter().copied(), &mut state.states);
            state_pointers(
                layout,
                index,
                targets[..count].iter().copied(),
                &mut state.targets,
            );
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
            state_pointers(layout, index, rows[..count].iter().copied(), &mut state.states);
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
            state_pointers(layout, index, rows[..count].iter().copied(), &mut state.states);
            aggregate.function.ops.destroy(&state.states);
        }
    }
    Ok(())
}
