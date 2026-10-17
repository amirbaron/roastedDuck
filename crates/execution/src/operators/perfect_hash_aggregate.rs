//! GROUP BY over small integer ranges (DuckDB's `PhysicalPerfectHashAggregate` and
//! `PerfectAggregateHashTable`): the statistics of the groups bound their values, so
//! every combination of group values (plus NULL) gets a fixed slot in an array of
//! aggregate states, addressed by the values' offsets from the group minima. The
//! groups are reconstructed from the slot index, and come out in slot order.

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::tree_renderer::RenderParams;
use rustdb_common::{Error, Result};
use rustdb_planner::Expression;
use rustdb_planner::expression::BoundAggregateExpression;
use rustdb_row::{
    GroupedUpdate, RowAggregate, RowOperationsState, RowPtr, TupleDataLayout, combine_states, destroy_states,
    finalize_states, initialize_states, supports_update_selection, update_states,
};
use rustdb_types::{
    DataChunk, LogicalType, PhysicalType, STANDARD_VECTOR_SIZE, SelectionVector, Value, Vector,
};

use crate::aggregate_hashtable::AggregateObject;
use crate::operator::{
    ErasedSink, ErasedSource, OrderPreservationType, PhysicalOperator, PhysicalRef, SinkState,
    aggregate_names,
};
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// The aggregate states of every possible group (DuckDB's `PerfectAggregateHashTable`).
struct PerfectAggregateTable {
    layout: Arc<TupleDataLayout>,
    tuple_size: usize,
    total_groups: usize,
    memory: NonNull<u8>,
    allocation: Layout,
    /// Whether a row fell into each slot (`group_is_set`).
    group_is_set: Vec<bool>,
    row_state: RowOperationsState,
    /// Whether every aggregate can add a selection of rows to one state, so a chunk with
    /// few groups can be added one group at a time.
    selection_update: bool,
    grouped_update: GroupedUpdate,
}

// SAFETY: the state array is uniquely owned and only accessed through `&mut self` (or
// `&self` for reads of finished states), under the operator's mutex when shared.
unsafe impl Send for PerfectAggregateTable {}
// SAFETY: as above.
unsafe impl Sync for PerfectAggregateTable {}

impl std::fmt::Debug for PerfectAggregateTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerfectAggregateTable")
            .field("total_groups", &self.total_groups)
            .field("tuple_size", &self.tuple_size)
            .finish_non_exhaustive()
    }
}

impl PerfectAggregateTable {
    /// A table with `2^total_bits` slots of initialised states of `aggregates`.
    fn new(aggregates: &[AggregateObject], total_bits: usize) -> Result<Self> {
        let row_aggregates: Vec<RowAggregate> = aggregates
            .iter()
            .map(|aggregate| {
                let state = aggregate.function.ops.state_layout();
                RowAggregate {
                    function: aggregate.function.clone(),
                    bind_data: aggregate.bind_data.clone(),
                    child_count: aggregate.child_count,
                    payload_size: state.size().next_multiple_of(state.align().max(1)),
                    alignment: state.align(),
                }
            })
            .collect();
        let alignment = row_aggregates
            .iter()
            .map(|aggregate| aggregate.alignment)
            .max()
            .unwrap_or(1)
            .max(8);
        let layout = Arc::new(TupleDataLayout::aggregates_only(row_aggregates)?);
        let tuple_size = layout.row_width().max(1).next_multiple_of(alignment);
        let total_groups = 1usize << total_bits;
        let size = tuple_size
            .checked_mul(total_groups)
            .ok_or_else(|| Error::internal("perfect hash aggregate too large"))?;
        let allocation = Layout::from_size_align(size, alignment)
            .map_err(|_| Error::internal("perfect hash aggregate layout"))?;
        // SAFETY: the size is non-zero (at least one slot of at least one byte).
        let memory = NonNull::new(unsafe { alloc_zeroed(allocation) })
            .unwrap_or_else(|| handle_alloc_error(allocation));
        let mut table = Self {
            layout,
            tuple_size,
            total_groups,
            memory,
            allocation,
            group_is_set: vec![false; total_groups],
            row_state: RowOperationsState::default(),
            selection_update: false,
            grouped_update: GroupedUpdate::default(),
        };
        table.selection_update = supports_update_selection(&table.layout);
        let mut start = 0;
        while start < total_groups {
            let end = (start + STANDARD_VECTOR_SIZE).min(total_groups);
            let rows: Vec<RowPtr> = (start..end).map(|slot| table.row(slot)).collect();
            // SAFETY: the rows are slots of this table's allocation, not initialised yet.
            unsafe { initialize_states(&table.layout, &rows, None, rows.len())? };
            start = end;
        }
        table.row_state = RowOperationsState::default();
        Ok(table)
    }

    /// The state row of slot `slot`.
    fn row(&self, slot: usize) -> RowPtr {
        // SAFETY: callers pass `slot < total_groups`, so the offset stays in the allocation.
        RowPtr::from_ptr(unsafe { self.memory.as_ptr().add(slot * self.tuple_size) })
    }

    /// Adds the rows of `groups` with aggregate `inputs` (DuckDB's `AddChunk`): the slot
    /// of a row is the concatenation of its groups' offsets (+1, 0 for NULL).
    fn add_chunk(
        &mut self,
        groups: &DataChunk,
        minima: &[Value],
        required_bits: &[usize],
        inputs: &[Vec<Vector>],
        selections: &[Option<Vec<u32>>],
    ) -> Result<()> {
        let count = groups.len();
        let mut slots = vec![0u64; count];
        let mut shift: usize = required_bits.iter().sum();
        for ((group, min), &bits) in groups.columns().iter().zip(minima).zip(required_bits) {
            shift -= bits;
            compute_group_location(group, min, &mut slots, shift, count)?;
        }
        for &slot in &slots {
            let slot = usize::try_from(slot).unwrap_or(usize::MAX);
            if slot >= self.total_groups {
                return Err(Error::invalid_input(format!(
                    "Perfect hash aggregate: aggregate group {slot} exceeded total groups {}. This likely means that the statistics in your data source are corrupt.\n* PRAGMA disable_optimizer to disable optimizations that rely on correct statistics",
                    self.total_groups
                )));
            }
            self.group_is_set[slot] = true;
        }
        let rows: Vec<RowPtr> = slots.iter().map(|&slot| self.row(slot as usize)).collect();
        if self.selection_update && selections.iter().all(Option::is_none) {
            // SAFETY: the rows are initialised state rows of this table.
            if unsafe { self.grouped_update.update(&self.layout, &rows, inputs)? } {
                return Ok(());
            }
        }
        for (index, columns) in inputs.iter().enumerate() {
            match selections.get(index).and_then(Option::as_ref) {
                None => {
                    // SAFETY: the rows are initialised slots of this table.
                    unsafe {
                        update_states(&mut self.row_state, &self.layout, index, &rows, columns, count)?;
                    };
                }
                Some(selection) if !selection.is_empty() => {
                    let selected: Vec<RowPtr> = selection.iter().map(|&row| rows[row as usize]).collect();
                    let sel = SelectionVector::new(selection.clone());
                    let sliced: Vec<Vector> = columns.iter().map(|column| column.slice(&sel)).collect();
                    // SAFETY: as above.
                    unsafe {
                        update_states(
                            &mut self.row_state,
                            &self.layout,
                            index,
                            &selected,
                            &sliced,
                            selected.len(),
                        )?;
                    }
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// Merges the states of `other` into this table (DuckDB's `Combine`).
    fn combine(&mut self, other: &Self) -> Result<()> {
        let mut sources = Vec::with_capacity(STANDARD_VECTOR_SIZE);
        let mut targets = Vec::with_capacity(STANDARD_VECTOR_SIZE);
        for slot in 0..self.total_groups {
            if !other.group_is_set[slot] {
                continue;
            }
            self.group_is_set[slot] = true;
            sources.push(other.row(slot));
            targets.push(self.row(slot));
            if sources.len() == STANDARD_VECTOR_SIZE {
                // SAFETY: both are initialised slots of distinct tables.
                unsafe {
                    combine_states(
                        &mut self.row_state,
                        &self.layout,
                        &sources,
                        &targets,
                        sources.len(),
                    )?;
                };
                sources.clear();
                targets.clear();
            }
        }
        if !sources.is_empty() {
            // SAFETY: as above.
            unsafe {
                combine_states(
                    &mut self.row_state,
                    &self.layout,
                    &sources,
                    &targets,
                    sources.len(),
                )?;
            };
        }
        Ok(())
    }

    /// The groups and aggregate results of the occupied slots, in slot order (DuckDB's
    /// `Scan`).
    fn scan(
        &mut self,
        group_types: &[LogicalType],
        minima: &[Value],
        required_bits: &[usize],
        result_types: &[LogicalType],
    ) -> Result<Vec<DataChunk>> {
        let occupied: Vec<usize> = (0..self.total_groups)
            .filter(|&slot| self.group_is_set[slot])
            .collect();
        let mut chunks = Vec::new();
        for batch in occupied.chunks(STANDARD_VECTOR_SIZE) {
            let count = batch.len();
            let mut columns = Vec::with_capacity(result_types.len());
            let mut shift: usize = required_bits.iter().sum();
            for ((ty, min), &bits) in group_types.iter().zip(minima).zip(required_bits) {
                shift -= bits;
                columns.push(reconstruct_group_vector(batch, ty, min, bits, shift)?);
            }
            let mut results: Vec<Vector> = result_types[group_types.len()..]
                .iter()
                .map(|ty| Vector::flat(ty.clone(), count))
                .collect();
            let rows: Vec<RowPtr> = batch.iter().map(|&slot| self.row(slot)).collect();
            // SAFETY: the rows are initialised slots of this table.
            unsafe { finalize_states(&mut self.row_state, &self.layout, &rows, count, &mut results, 0)? };
            columns.extend(results);
            chunks.push(DataChunk::new(columns, count));
        }
        Ok(chunks)
    }
}

impl Drop for PerfectAggregateTable {
    fn drop(&mut self) {
        if !self.layout.aggregate_destructor_indices().is_empty() {
            let mut start = 0;
            while start < self.total_groups {
                let end = (start + STANDARD_VECTOR_SIZE).min(self.total_groups);
                let rows: Vec<RowPtr> = (start..end).map(|slot| self.row(slot)).collect();
                // SAFETY: every slot holds an initialised state, destroyed exactly once here.
                let _ = unsafe { destroy_states(&mut self.row_state, &self.layout, &rows, rows.len()) };
                start = end;
            }
        }
        // SAFETY: allocated in `new` with this layout.
        unsafe { dealloc(self.memory.as_ptr(), self.allocation) };
    }
}

/// Adds the slot offsets of `group`'s values to `slots` (DuckDB's
/// `ComputeGroupLocation`): `(value - min + 1) << shift`, nothing for NULL.
fn compute_group_location(
    group: &Vector,
    min: &Value,
    slots: &mut [u64],
    shift: usize,
    count: usize,
) -> Result<()> {
    let min = min
        .as_i128()
        .ok_or_else(|| Error::internal("perfect hash aggregate group without a minimum"))?;
    macro_rules! location {
        ($native:ty) => {{
            let values = group.unified::<$native>();
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "UnsafeNumericCast"
            )]
            let location = |value: $native| ((i128::from(value) - min + 1) as u64) << shift;
            if values.validity.is_all_valid() {
                // Without NULLs: the selection resolved outside the loop.
                match values.sel {
                    rustdb_types::Sel::Identity => {
                        for (slot, &value) in slots[..count].iter_mut().zip(&values.values[..count]) {
                            *slot += location(value);
                        }
                    }
                    rustdb_types::Sel::Indices(indices) => {
                        for (slot, &index) in slots[..count].iter_mut().zip(&indices[..count]) {
                            *slot += location(values.values[index as usize]);
                        }
                    }
                    rustdb_types::Sel::Constant => {
                        let constant = location(values.values[0]);
                        for slot in &mut slots[..count] {
                            *slot += constant;
                        }
                    }
                }
                return Ok(());
            }
            if let (rustdb_types::Sel::Identity, Some(words)) = (values.sel, values.validity.words()) {
                // Flat with NULLs: NULL rows keep offset 0; the valid rows of each
                // validity word are added without a per-row lookup.
                for (block, (slots, values)) in slots[..count]
                    .chunks_mut(64)
                    .zip(values.values[..count].chunks(64))
                    .enumerate()
                {
                    let word = words.get(block).copied().unwrap_or(u64::MAX);
                    for (bit, (slot, &value)) in slots.iter_mut().zip(values).enumerate() {
                        let valid = u64::from(word >> bit & 1 == 1);
                        *slot += location(value) * valid;
                    }
                }
                return Ok(());
            }
            for (row, slot) in slots.iter_mut().enumerate().take(count) {
                if let Some(value) = values.get(row) {
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "UnsafeNumericCast"
                    )]
                    let adjusted = (i128::from(value) - min + 1) as u64;
                    *slot += adjusted << shift;
                }
            }
        }};
    }
    match group.logical_type().physical_type() {
        PhysicalType::Int8 => location!(i8),
        PhysicalType::Int16 => location!(i16),
        PhysicalType::Int32 => location!(i32),
        PhysicalType::Int64 => location!(i64),
        PhysicalType::UInt8 => location!(u8),
        PhysicalType::UInt16 => location!(u16),
        PhysicalType::UInt32 => location!(u32),
        PhysicalType::UInt64 => location!(u64),
        _ => {
            return Err(Error::internal(
                "Unsupported group type for perfect aggregate hash table",
            ));
        }
    }
    Ok(())
}

/// The values of one group for `slots` (DuckDB's `ReconstructGroupVector`): the slot's
/// bits for the group are the offset from the minimum plus one, 0 for NULL.
fn reconstruct_group_vector(
    slots: &[usize],
    ty: &LogicalType,
    min: &Value,
    bits: usize,
    shift: usize,
) -> Result<Vector> {
    let min = min
        .as_i128()
        .ok_or_else(|| Error::internal("perfect hash aggregate group without a minimum"))?;
    let mask = (1usize << bits) - 1;
    let mut result = Vector::flat(ty.clone(), slots.len());
    macro_rules! reconstruct {
        ($native:ty) => {{
            let mut nulls = Vec::new();
            {
                let values = result.values_mut::<$native>();
                for (row, &slot) in slots.iter().enumerate() {
                    let index = (slot >> shift) & mask;
                    if index == 0 {
                        nulls.push(row);
                    } else {
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            clippy::cast_possible_wrap,
                            reason = "UnsafeNumericCast"
                        )]
                        {
                            values[row] = (min + index as i128 - 1) as $native;
                        }
                    }
                }
            }
            for row in nulls {
                result.set_null(row);
            }
        }};
    }
    match ty.physical_type() {
        PhysicalType::Int8 => reconstruct!(i8),
        PhysicalType::Int16 => reconstruct!(i16),
        PhysicalType::Int32 => reconstruct!(i32),
        PhysicalType::Int64 => reconstruct!(i64),
        PhysicalType::UInt8 => reconstruct!(u8),
        PhysicalType::UInt16 => reconstruct!(u16),
        PhysicalType::UInt32 => reconstruct!(u32),
        PhysicalType::UInt64 => reconstruct!(u64),
        _ => return Err(Error::internal("Invalid type for perfect aggregate HT group")),
    }
    Ok(result)
}

/// GROUP BY with a perfect hash table (DuckDB's `PhysicalPerfectHashAggregate`).
#[derive(Debug)]
pub struct PhysicalPerfectHashAggregate {
    types: Vec<LogicalType>,
    /// Column references into the input.
    groups: Vec<Expression>,
    aggregates: Vec<BoundAggregateExpression>,
    group_types: Vec<LogicalType>,
    /// The minimum of each group (`group_minima`).
    group_minima: Vec<Value>,
    /// The bits of the slot index each group takes (`required_bits`).
    required_bits: Vec<usize>,
    children: [PhysicalRef; 1],
}

impl PhysicalPerfectHashAggregate {
    pub fn new(
        types: Vec<LogicalType>,
        groups: Vec<Expression>,
        aggregates: Vec<BoundAggregateExpression>,
        group_minima: Vec<Value>,
        required_bits: Vec<usize>,
        child: PhysicalRef,
    ) -> Self {
        let group_types = groups.iter().map(|group| group.return_type.clone()).collect();
        Self {
            types,
            groups,
            aggregates,
            group_types,
            group_minima,
            required_bits,
            children: [child],
        }
    }

    fn aggregate_objects(&self) -> Vec<AggregateObject> {
        self.aggregates
            .iter()
            .map(|aggregate| AggregateObject {
                function: aggregate.function.clone(),
                bind_data: aggregate.bind_info.clone(),
                child_count: aggregate.children.len(),
            })
            .collect()
    }

    fn new_table(&self) -> Result<PerfectAggregateTable> {
        PerfectAggregateTable::new(&self.aggregate_objects(), self.required_bits.iter().sum())
    }
}

impl PhysicalOperator for PhysicalPerfectHashAggregate {
    fn name(&self) -> &'static str {
        "PERFECT_HASH_GROUP_BY"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn as_sink(&self) -> Option<&dyn ErasedSink> {
        Some(self)
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
    fn parallel_sink(&self) -> bool {
        true
    }
    fn parallel_source(&self) -> bool {
        true
    }
    fn source_order(&self) -> OrderPreservationType {
        OrderPreservationType::NoOrder
    }

    /// DuckDB's `PhysicalPerfectHashAggregate::ParamsToString` (no cardinality).
    fn params_to_string(&self, _: u64) -> RenderParams {
        let mut result = RenderParams::new();
        let groups: Vec<String> = self.groups.iter().map(Expression::name).collect();
        result.insert("Groups".to_owned(), groups.join("\n"));
        result.insert("Aggregates".to_owned(), aggregate_names(&self.aggregates));
        result
    }
}

/// The combined table and, once finalized, the result chunks.
#[derive(Debug, Default)]
pub struct PerfectHashAggregateGlobal {
    table: Mutex<Option<PerfectAggregateTable>>,
    results: Vec<DataChunk>,
}

/// A thread's table and expression executors.
#[derive(Debug)]
pub struct PerfectHashAggregateLocal {
    table: PerfectAggregateTable,
    groups: ExpressionExecutor,
    arguments: Vec<ExpressionExecutor>,
    filters: Vec<Option<ExpressionExecutor>>,
}

impl Sink for PhysicalPerfectHashAggregate {
    type Global = PerfectHashAggregateGlobal;
    type Local = PerfectHashAggregateLocal;

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<PerfectHashAggregateGlobal> {
        Ok(PerfectHashAggregateGlobal {
            table: Mutex::new(Some(self.new_table()?)),
            results: Vec::new(),
        })
    }

    fn local_sink_state(
        &self,
        _: &ExecutionContext,
        _: &PerfectHashAggregateGlobal,
    ) -> Result<PerfectHashAggregateLocal> {
        Ok(PerfectHashAggregateLocal {
            table: self.new_table()?,
            groups: ExpressionExecutor::new(self.groups.clone()),
            arguments: self
                .aggregates
                .iter()
                .map(|aggregate| ExpressionExecutor::new(aggregate.children.clone()))
                .collect(),
            filters: self
                .aggregates
                .iter()
                .map(|aggregate| {
                    aggregate
                        .filter
                        .as_ref()
                        .map(|filter| ExpressionExecutor::new(vec![filter.clone()]))
                })
                .collect(),
        })
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        _: &PerfectHashAggregateGlobal,
        local: &mut PerfectHashAggregateLocal,
    ) -> Result<SinkResult> {
        if chunk.is_empty() {
            return Ok(SinkResult::NeedMoreInput);
        }
        let mut groups = DataChunk::default();
        local.groups.execute(chunk, &mut groups)?;
        let mut inputs = Vec::with_capacity(self.aggregates.len());
        for executor in &mut local.arguments {
            let mut arguments = DataChunk::default();
            executor.execute(chunk, &mut arguments)?;
            inputs.push(arguments.into_columns());
        }
        let mut selections = Vec::with_capacity(self.aggregates.len());
        for filter in &mut local.filters {
            selections.push(match filter {
                Some(filter) => {
                    let mut selection = Vec::new();
                    filter.select(chunk, &mut selection)?;
                    Some(selection)
                }
                None => None,
            });
        }
        let groups = DataChunk::new(groups.into_columns(), chunk.len());
        local.table.add_chunk(
            &groups,
            &self.group_minima,
            &self.required_bits,
            &inputs,
            &selections,
        )?;
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(
        &self,
        _: &ExecutionContext,
        global: &PerfectHashAggregateGlobal,
        local: PerfectHashAggregateLocal,
    ) -> Result<()> {
        let mut table = global.table.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(table) = table.as_mut() {
            table.combine(&local.table)?;
        }
        Ok(())
    }

    fn finalize(
        &self,
        _: &ExecutionContext,
        global: &mut PerfectHashAggregateGlobal,
    ) -> Result<SinkFinalize> {
        let table = global.table.get_mut().unwrap_or_else(PoisonError::into_inner);
        if let Some(table) = table.as_mut() {
            global.results = table.scan(
                &self.group_types,
                &self.group_minima,
                &self.required_bits,
                &self.types,
            )?;
        }
        Ok(SinkFinalize::Ready)
    }
}

impl Source for PhysicalPerfectHashAggregate {
    type Global = (SinkState, std::sync::atomic::AtomicUsize);
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        let sink =
            sink.ok_or_else(|| Error::internal("perfect hash aggregate source requires its sink state"))?;
        Ok((Arc::clone(sink), std::sync::atomic::AtomicUsize::new(0)))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        (sink, next): &Self::Global,
        (): &mut (),
    ) -> Result<SourceResult> {
        let state = sink
            .downcast_ref::<PerfectHashAggregateGlobal>()
            .ok_or_else(|| Error::internal("perfect hash aggregate sink state has the wrong type"))?;
        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(result) = state.results.get(index) {
            chunk.set_columns(result.columns().to_vec(), result.len());
        }
        Ok(if index + 1 < state.results.len() {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }

    fn max_threads(&self, (sink, _): &Self::Global) -> usize {
        sink.downcast_ref::<PerfectHashAggregateGlobal>()
            .map_or(1, |state| state.results.len().max(1))
    }
}
