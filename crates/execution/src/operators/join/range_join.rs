//! What the range joins share (DuckDB's `PhysicalRangeJoin`): sorted tables of one join
//! side, the NULL handling of their keys, and the evaluation of trailing conditions.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use rustdb_ast::ExpressionType;
use rustdb_buffer::{BufferManager, MemoryTag};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::operator::JoinCondition;
use rustdb_row::sort::{
    ExternalBlockIteratorState, NullOrder, OrderModifiers, OrderType, Sort, SortColumn, SortConfig,
    SortGlobalSinkState, SortKeyType, SortLocalSinkState, SortSource, SortedRun, create_sort_keys,
};
use rustdb_row::{
    ChunkIterator, HeapKeepAlive, PinProperties, RowPtr, TupleDataCollection, TupleDataLayout, ValidityType,
};
use rustdb_types::{DataChunk, LogicalType, ValidityMask, Vector};

use crate::expression_executor::compare_vectors;
use crate::{ExecutionContext, ExpressionExecutor};

/// The sort order of a range condition (`BoundOrderByNode` with NULLS LAST).
pub(crate) fn order_modifiers(descending: bool) -> OrderModifiers {
    OrderModifiers::new(
        if descending {
            OrderType::Descending
        } else {
            OrderType::Ascending
        },
        NullOrder::NullsLast,
    )
}

/// Normalized sort keys of a column: byte strings that compare like the values in the
/// sort order.
#[derive(Debug, Default)]
pub(crate) struct KeyBytes {
    data: Vec<u8>,
    offsets: Vec<usize>,
}

impl KeyBytes {
    /// Appends the keys of the first `count` rows of `columns` sorted with `modifiers`.
    pub(crate) fn push_columns(
        &mut self,
        columns: &[&Vector],
        modifiers: &[OrderModifiers],
        count: usize,
    ) -> Result<()> {
        let keys = create_sort_keys(columns, modifiers, count)?;
        if self.offsets.is_empty() {
            self.offsets.push(0);
        }
        for row in 0..count {
            self.data.extend_from_slice(keys.key(row));
            self.offsets.push(self.data.len());
        }
        Ok(())
    }

    /// The key of row `row`.
    #[inline]
    pub(crate) fn get(&self, row: usize) -> &[u8] {
        &self.data[self.offsets[row]..self.offsets[row + 1]]
    }
}

/// `lhs` sorts before `rhs`, or (unless `strict`) is equal to it (`MergeJoinBefore` /
/// `IEJoinUnion::Compare` on sort keys).
#[inline]
pub(crate) fn sorts_before(lhs: &[u8], rhs: &[u8], strict: bool) -> bool {
    if strict { lhs < rhs } else { lhs <= rhs }
}

/// `lhs` sorts before `rhs`, or (unless `strict`) is equal to it, for sort keys
/// (`MergeJoinBefore`, `IEJoinUnion::Compare`).
#[inline]
pub(crate) fn key_before<K: rustdb_row::sort::SortKey>(lhs: &K, rhs: &K, strict: bool) -> bool {
    let less_than = lhs.less(rhs);
    if !less_than && !strict {
        return !rhs.less(lhs);
    }
    less_than
}

/// Whether a range comparison excludes equality (`MergeJoinStrictComparison`).
pub(crate) fn is_strict_comparison(comparison: ExpressionType) -> Result<bool> {
    match comparison {
        ExpressionType::CompareLessthan | ExpressionType::CompareGreaterthan => Ok(true),
        ExpressionType::CompareLessthanorequalto | ExpressionType::CompareGreaterthanorequalto => Ok(false),
        _ => Err(Error::internal("Unimplemented comparison type for range join!")),
    }
}

/// Moves the range conditions in front of the others (`PhysicalRangeJoin` constructor).
pub(crate) fn reorder_range_conditions(conditions: Vec<JoinCondition>) -> Vec<JoinCondition> {
    if conditions.len() <= 1 {
        return conditions;
    }
    let count = conditions.len();
    let mut slots: Vec<Option<JoinCondition>> = (0..count).map(|_| None).collect();
    let mut range_position = 0;
    let mut other_position = count;
    for condition in conditions {
        if matches!(
            condition.comparison,
            ExpressionType::CompareLessthan
                | ExpressionType::CompareLessthanorequalto
                | ExpressionType::CompareGreaterthan
                | ExpressionType::CompareGreaterthanorequalto
        ) {
            slots[range_position] = Some(condition);
            range_position += 1;
        } else {
            other_position -= 1;
            slots[other_position] = Some(condition);
        }
    }
    slots.into_iter().flatten().collect()
}

/// Makes the primary key NULL wherever another condition key that rejects NULLs is NULL,
/// so such rows sort to the end; returns the number of NULL rows (`MergeNulls`).
fn merge_nulls(keys: &DataChunk, conditions: &[JoinCondition]) -> (Vector, usize) {
    let count = keys.len();
    let primary = keys.column(0);
    let mut validity = ValidityMask::all_valid();
    let mut nulls = 0;
    let mut merged = false;
    for (index, column) in keys.columns().iter().enumerate() {
        if index > 0 && conditions[index].comparison == ExpressionType::CompareDistinctFrom {
            continue;
        }
        if column.validity().is_all_valid() {
            continue;
        }
        merged |= index > 0;
        let sel = column.sel();
        for row in 0..count {
            if !column.validity().is_valid(sel.slot(row)) && validity.is_valid(row) {
                validity.set_invalid(row, count);
                nulls += 1;
            }
        }
    }
    if !merged {
        return (primary.clone(), nulls);
    }
    let mut flat = primary.flattened(count);
    flat.set_validity(validity);
    (flat, nulls)
}

/// One thread's sink into a [`SortedTable`] (DuckDB's `LocalSortedTable`): evaluates the
/// condition keys of one side and sinks `[primary key, input columns]`.
#[derive(Debug)]
pub struct LocalSortedTable {
    pub(crate) executor: ExpressionExecutor,
    sort: SortLocalSinkState,
    /// The condition keys of the last sunk chunk.
    pub(crate) keys: DataChunk,
    has_null: usize,
    count: usize,
}

impl LocalSortedTable {
    /// A sink for the side whose condition expressions are `expressions`.
    pub(crate) fn new(table: &SortedTable, expressions: Vec<rustdb_planner::Expression>) -> Self {
        Self {
            executor: ExpressionExecutor::new(expressions),
            sort: table.sort.local_sink_state(),
            keys: DataChunk::default(),
            has_null: 0,
            count: 0,
        }
    }

    /// Sinks `input` sorted on its first condition key (`LocalSortedTable::Sink`).
    pub(crate) fn sink(
        &mut self,
        table: &SortedTable,
        input: &DataChunk,
        conditions: &[JoinCondition],
    ) -> Result<()> {
        self.executor.execute(input, &mut self.keys)?;
        let (primary, nulls) = merge_nulls(&self.keys, conditions);
        self.has_null += nulls;
        self.count += input.len();
        let mut columns = vec![primary];
        columns.extend(input.columns().iter().cloned());
        table.sink(&mut self.sort, &DataChunk::new(columns, input.len()))
    }
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// The rows of one join side sorted on a key (DuckDB's `GlobalSortedTable`).
///
/// The sort input is `[key columns, payload columns]`; the sort only keeps the payload
/// columns. [`Self::finalize`] ends the sort; materializing merges its runs into one
/// [`SortedRun`] whose blocks can spill: row `i` of the sort order has its key at
/// position `i` of the run's keys and its payload in payload row `i`. Readers go through
/// an [`ExternalBlockIteratorState`] ([`Self::create_iterator_state`]) and
/// [`slice_sorted_payload`].
#[derive(Debug)]
pub struct SortedTable {
    sort: Sort,
    threads: usize,
    /// The sink phase of the sort (until [`Self::finalize`]).
    sink: RwLock<Option<SortGlobalSinkState>>,
    /// The merge phase of the sort (until the run is materialized).
    source: RwLock<Option<SortSource>>,
    /// Materialization tasks scheduled and finished (`tasks_completed`).
    tasks_scheduled: AtomicUsize,
    tasks_completed: AtomicUsize,
    /// The materialized rows.
    sorted: OnceLock<SortedRun>,
    /// Rows whose primary key is NULL (sorted last).
    pub(crate) has_null: AtomicUsize,
    pub(crate) count: AtomicUsize,
    /// Whether each sorted row found a match (outer joins).
    pub(crate) found_match: Vec<AtomicBool>,
}

impl SortedTable {
    /// A table sorted on `key_types` with `modifiers`, carrying `payload_types` columns.
    pub(crate) fn new(
        ctx: &ExecutionContext,
        key_types: &[LogicalType],
        modifiers: Vec<OrderModifiers>,
        payload_types: Vec<LogicalType>,
    ) -> Result<Self> {
        let mut input_types = key_types.to_vec();
        input_types.extend(payload_types.iter().cloned());
        let orders = modifiers
            .iter()
            .enumerate()
            .map(|(column, &modifiers)| SortColumn {
                column,
                modifiers,
                stats: None,
            })
            .collect();
        // The keys are only sorted on; the payload columns are what a scan produces.
        let projection = (key_types.len()..input_types.len()).collect();
        let sort = Sort::new(
            ctx.env.buffer_manager.clone(),
            orders,
            &input_types,
            Some(projection),
        )?;
        let threads = ctx.env.threads().max(1);
        let config = SortConfig {
            threads,
            connections: 1,
            ..SortConfig::default()
        };
        let sink = sort.global_sink_state(config);
        Ok(Self {
            sort,
            threads,
            sink: RwLock::new(Some(sink)),
            source: RwLock::new(None),
            tasks_scheduled: AtomicUsize::new(0),
            tasks_completed: AtomicUsize::new(0),
            sorted: OnceLock::new(),
            has_null: AtomicUsize::new(0),
            count: AtomicUsize::new(0),
            found_match: Vec::new(),
        })
    }

    /// A new thread-local sink state of the sort.
    pub(crate) fn local_sink_state(&self) -> SortLocalSinkState {
        self.sort.local_sink_state()
    }

    /// Sinks `[keys, payload]` rows.
    pub(crate) fn sink(&self, local: &mut SortLocalSinkState, chunk: &DataChunk) -> Result<()> {
        let sink = read(&self.sink);
        let sink = sink
            .as_ref()
            .ok_or_else(|| Error::internal("sorted table sink after finalize"))?;
        self.sort.sink(chunk, sink, local)
    }

    /// Merges a thread's sink state (`GlobalSortedTable::Combine`).
    pub(crate) fn combine(&self, local: LocalSortedTable) -> Result<()> {
        self.has_null.fetch_add(local.has_null, Ordering::Relaxed);
        self.count.fetch_add(local.count, Ordering::Relaxed);
        self.combine_sort(local.sort)
    }

    /// Merges a thread's sort state of rows sunk with [`Self::sink`].
    pub(crate) fn combine_sort(&self, local: SortLocalSinkState) -> Result<()> {
        let sink = read(&self.sink);
        let sink = sink
            .as_ref()
            .ok_or_else(|| Error::internal("sorted table combine after finalize"))?;
        self.sort.combine(sink, local)
    }

    /// Ends the sort's sink phase (`GlobalSortedTable::Finalize`); without rows the table
    /// is materialized empty right away.
    pub(crate) fn finalize(&self) -> Result<()> {
        let sink = write(&self.sink)
            .take()
            .ok_or_else(|| Error::internal("sorted table finalized twice"))?;
        match self.sort.finalize(sink)? {
            Some(source) => *write(&self.source) = Some(source),
            None => self.materialize_empty()?,
        }
        Ok(())
    }

    /// An empty materialized run (`MaterializeEmpty`).
    fn materialize_empty(&self) -> Result<()> {
        self.sorted
            .set(self.sort.empty_run()?)
            .map_err(|_| Error::internal("sorted table materialized twice"))
    }

    /// The number of parallel materialization tasks to schedule once the sort is
    /// finalized, as many as the sort allows (`RangeJoinMaterializeEvent::Schedule`); 0
    /// when there is nothing (more) to schedule.
    pub(crate) fn materialize_tasks(&self) -> usize {
        let source = read(&self.source);
        let Some(source) = source.as_ref() else {
            return 0;
        };
        if self.tasks_scheduled.load(Ordering::Acquire) != 0 {
            return 0;
        }
        let tasks = self.threads.min(source.max_threads()).max(1);
        self.tasks_scheduled.store(tasks, Ordering::Release);
        tasks
    }

    /// One materialization task; the last one to finish collects the run
    /// (`RangeJoinMaterializeTask`).
    pub(crate) fn execute_materialize_task(&self) -> Result<()> {
        self.materialize_sorted_run()?;
        let completed = self.tasks_completed.fetch_add(1, Ordering::AcqRel) + 1;
        if completed == self.tasks_scheduled.load(Ordering::Acquire) {
            self.get_sorted_run()?;
        }
        Ok(())
    }

    /// Merges the partitions this thread can claim into runs (`MaterializeSortedRun`).
    pub(crate) fn materialize_sorted_run(&self) -> Result<()> {
        let source = read(&self.source);
        if let Some(source) = source.as_ref() {
            let mut local = source.local_state();
            source.materialize(&mut local)?;
        }
        Ok(())
    }

    /// Collects the materialized runs into the table's run and drops the merge state
    /// (`GetSortedRun`).
    pub(crate) fn get_sorted_run(&self) -> Result<()> {
        let Some(source) = write(&self.source).take() else {
            return Ok(());
        };
        match source.take_sorted_run()? {
            Some(run) => self
                .sorted
                .set(run)
                .map_err(|_| Error::internal("sorted table materialized twice")),
            None => self.materialize_empty(),
        }
    }

    /// Single-threaded materialization (`Materialize(context, interrupt)`).
    pub(crate) fn materialize(&self) -> Result<()> {
        self.materialize_sorted_run()?;
        self.get_sorted_run()
    }

    /// The materialized rows.
    pub(crate) fn sorted(&self) -> Result<&SortedRun> {
        self.sorted
            .get()
            .ok_or_else(|| Error::internal("sorted table read before it was materialized"))
    }

    /// The type of the keys (`GetSortKeyType`).
    pub(crate) fn sort_key_type(&self) -> Result<SortKeyType> {
        Ok(self.sorted()?.key_type())
    }

    /// An iteration state over the keys, pinning keys and payload (`CreateIteratorState`).
    pub(crate) fn create_iterator_state(&self) -> Result<ExternalBlockIteratorState> {
        let mut state = ExternalBlockIteratorState::new(self.sorted()?);
        // Unless we do this, we will only get values from the first chunk.
        state.repin();
        Ok(state)
    }

    /// Number of rows.
    pub(crate) fn count(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }

    /// Number of rows whose key is not NULL (they come first).
    pub(crate) fn not_null(&self) -> usize {
        self.count() - self.has_null.load(Ordering::Relaxed)
    }

    /// Number of `STANDARD_VECTOR_SIZE` blocks (`BlockCount`).
    pub(crate) fn block_count(&self) -> usize {
        self.sorted
            .get()
            .map_or(0, |sorted| sorted.key_data().chunk_count())
    }

    /// First row of block `block` (`BlockStart`).
    pub(crate) fn block_start(&self, block: usize) -> usize {
        (block * STANDARD_VECTOR_SIZE).min(self.count())
    }

    /// Rows in block `block` (`BlockSize`).
    pub(crate) fn block_size(&self, block: usize) -> usize {
        if block < self.block_count() {
            STANDARD_VECTOR_SIZE.min(self.count() - self.block_start(block))
        } else {
            0
        }
    }

    /// Creates the match flags of an outer join (`IntializeMatches`).
    pub(crate) fn initialize_matches(&mut self) {
        self.found_match = (0..self.count()).map(|_| AtomicBool::new(false)).collect();
    }

    /// Marks sorted row `row` matched.
    #[inline]
    pub(crate) fn set_match(&self, row: usize) {
        self.found_match[row].store(true, Ordering::Relaxed);
    }

    /// Payload column `column` of every row, in order (`ExtractColumn`).
    pub(crate) fn extract_column<T: rustdb_types::NativeType>(&self, column: usize) -> Result<Vec<T>> {
        let sorted = self.sorted()?;
        let mut result = Vec::with_capacity(self.count());
        let Some(payload) = sorted.payload_data() else {
            return Ok(result);
        };
        let mut scan = payload.initialize_scan_columns(vec![column], PinProperties::UnpinAfterDone);
        let mut chunk = DataChunk::default();
        while payload.scan(&mut scan, &mut chunk)? {
            let unified = chunk.column(0).unified::<T>();
            result.extend((0..chunk.len()).map(|row| unified.values[unified.slot(row)]));
        }
        Ok(result)
    }
}

/// Gathers the payload of rows `indices` of `table` into `chunk`, reading their keys
/// through `state`, which must have been re-pinned (`SliceSortedPayload`).
pub(crate) fn slice_sorted_payload(
    chunk: &mut DataChunk,
    table: &SortedTable,
    state: &mut ExternalBlockIteratorState,
    indices: &[u32],
    decode_buffer: &mut Vec<u8>,
) -> Result<()> {
    let sorted = table.sorted()?;
    rustdb_row::dispatch_sort_key!(sorted.key_type(), K => {
        let mut keys: Vec<K> = Vec::with_capacity(indices.len());
        for &index in indices {
            keys.push(state.get::<K>(sorted, index as usize)?);
        }
        let keep_alive = state.keep_alive();
        // SAFETY: the keys were read through `state`, which keeps their chunks (and
        // payload) pinned until it is re-pinned; `keep_alive` holds those pins.
        unsafe { sorted.scan(&keys, decode_buffer, Some(&keep_alive), chunk) }
    })
}

/// Sorted rows with random access by position: their normalized keys and their payload
/// columns, kept pinned (the positional join's buffered right side).
#[derive(Debug)]
pub struct SortedRows {
    keys: KeyBytes,
    payload_types: Vec<LogicalType>,
    payload: TupleDataCollection,
    rows: Vec<RowPtr>,
    keep_alive: HeapKeepAlive,
}

impl SortedRows {
    /// Materializes sorted `chunks`: the keys of columns `key_columns` sorted with
    /// `modifiers`, and payload columns `payload_columns`.
    pub(crate) fn materialize(
        buffer_manager: &BufferManager,
        chunks: &[DataChunk],
        key_columns: &[usize],
        modifiers: &[OrderModifiers],
        payload_columns: &[usize],
        payload_types: Vec<LogicalType>,
    ) -> Result<Self> {
        let layout_types = if payload_types.is_empty() {
            vec![LogicalType::BOOLEAN]
        } else {
            payload_types.clone()
        };
        let layout = Arc::new(TupleDataLayout::new(layout_types, ValidityType::CanHaveNull)?);
        let mut payload = TupleDataCollection::new(buffer_manager.clone(), layout, MemoryTag::OrderBy)?;
        let mut append = payload.initialize_append(PinProperties::KeepEverythingPinned);
        let mut keys = KeyBytes::default();
        keys.offsets.push(0);
        let mut total = 0;
        for chunk in chunks {
            let count = chunk.len();
            let columns: Vec<&Vector> = key_columns.iter().map(|&column| chunk.column(column)).collect();
            keys.push_columns(&columns, modifiers, count)?;
            let columns: Vec<Vector> = if payload_types.is_empty() {
                vec![Vector::flat(LogicalType::BOOLEAN, count)]
            } else {
                payload_columns
                    .iter()
                    .map(|&column| chunk.column(column).clone())
                    .collect()
            };
            payload.append(&mut append, &DataChunk::new(columns, count))?;
            total += count;
        }
        payload.finalize_pin_state(&mut append.pin_state);
        let mut rows = Vec::with_capacity(total);
        if total > 0 {
            let mut iterator = ChunkIterator::new(&payload, PinProperties::AlreadyPinned, false)?;
            loop {
                rows.extend_from_slice(iterator.row_locations());
                if !iterator.next_chunk()? {
                    break;
                }
            }
        }
        let keep_alive = payload.heap_keep_alive();
        Ok(Self {
            keys,
            payload_types,
            payload,
            rows,
            keep_alive,
        })
    }

    /// Number of rows.
    pub(crate) fn count(&self) -> usize {
        self.rows.len()
    }

    /// The normalized key of row `row`.
    #[inline]
    pub(crate) fn key(&self, row: usize) -> &[u8] {
        self.keys.get(row)
    }

    /// Gathers the payload of rows `indices` into `result` (`SliceSortedPayload`).
    pub(crate) fn gather(&self, indices: &[u32], result: &mut DataChunk) -> Result<()> {
        let count = indices.len();
        let mut columns: Vec<Vector> = self
            .payload_types
            .iter()
            .map(|ty| Vector::flat(ty.clone(), count.max(1)))
            .collect();
        let column_ids: Vec<usize> = (0..self.payload_types.len()).collect();
        if count > 0 && !columns.is_empty() {
            // SAFETY: every row of the payload stays pinned while `self` lives.
            unsafe {
                self.payload.gather(
                    &self.rows,
                    Some(indices),
                    count,
                    &column_ids,
                    &mut columns,
                    None,
                    Some(&self.keep_alive),
                )?;
            }
        }
        result.set_columns(columns, count);
        Ok(())
    }
}

/// Keeps the rows of `sel` (or all `count` rows) where `left <comparison> right`
/// holds (`PhysicalRangeJoin::SelectJoinTail`).
pub(crate) fn select_join_tail(
    comparison: ExpressionType,
    left: &Vector,
    right: &Vector,
    count: usize,
) -> Result<Vec<u32>> {
    let result = compare_vectors(comparison, left, right, count)?;
    let values = result.unified::<bool>();
    Ok((0..count as u32)
        .filter(|&row| values.get(row as usize) == Some(true))
        .collect())
}
