//! How partitioned operators (window functions, as-of joins) get their input grouped and
//! sorted (DuckDB's `SortStrategy`: `HashedSort`, `FullSort`, `NaturalSort`).
//!
//! - [`HashedSort`] (`PARTITION BY ...`): rows are radix-partitioned on the hash of the
//!   partition columns into hash groups; each group is sorted by the partition columns
//!   (ASC NULLS FIRST) followed by the ORDER BY columns.
//! - [`FullSort`] (`ORDER BY` only): one group, sorted while sinking.
//! - [`NaturalSort`] (neither): one group in input order.
//!
//! After the sink phase, the source of an operator works through the hash groups in
//! stages that several threads share: [`SortStrategy::sort_column_data`] sorts a hash
//! group (callers share the scan of its partition; the last one finalizes the sort),
//! [`SortStrategy::materialize_column_data`] / [`SortStrategy::materialize_sorted_run`]
//! merge it (each caller merges the partitions it claims), and
//! [`SortStrategy::get_column_data`] / [`SortStrategy::get_sorted_run`] hand out the result
//! as buffer-managed [`GroupData`] or as one sorted run.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_buffer::{BufferManager, MemoryTag};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use super::encode::{NullOrder, OrderModifiers, OrderType};
use super::group_data::{GroupData, GroupDataAppend};
use super::merger::DEFAULT_ROW_GROUP_SIZE;
use super::run::SortedRun;
use super::sorter::{
    Sort, SortColumn, SortColumnStats, SortConfig, SortGlobalSinkState, SortLocalSinkState, SortSource,
};
use crate::collection::TupleDataCollection;
use crate::hash::hash_columns;
use crate::layout::{TupleDataLayout, ValidityType};
use crate::partitioned::{PartitionedAppendState, PartitionedTupleData};
use crate::radix::RadixPartitioning;
use crate::state::{ParallelScanState, PinProperties};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Rows and chunks of one hash group (`ChunkRow`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChunkRow {
    pub chunks: usize,
    pub count: usize,
}

/// Chooses and runs a strategy (`SortStrategy::Factory`).
#[derive(Debug)]
pub enum SortStrategy {
    Hashed(HashedSort),
    Full(FullSort),
    Natural(NaturalSort),
}

/// Shared state of a strategy's sink and source phases.
#[derive(Debug)]
pub enum StrategyGlobalState {
    Hashed(HashedGlobalState),
    Full(FullGlobalState),
    Natural(NaturalGlobalState),
}

/// A thread's sink state.
#[derive(Debug)]
pub enum StrategyLocalState {
    Hashed(Box<HashedLocalState>),
    Full(Box<SortLocalSinkState>),
    Natural(Option<Box<(GroupData, GroupDataAppend)>>),
}

/// Appends a constant NULL BOOLEAN column, the forced payload when every input column is
/// a key.
fn with_forced_payload(chunk: &DataChunk) -> Result<DataChunk> {
    let mut columns = chunk.columns().to_vec();
    columns.push(Vector::constant(&Value::null(LogicalType::BOOLEAN))?);
    Ok(DataChunk::new(columns, chunk.len()))
}

/// Whether a payload column has to be forced because all columns are sort keys.
fn needs_forced_payload(require_payload: bool, sort_columns: &[usize], column_count: usize) -> bool {
    if !require_payload {
        return false;
    }
    let mut distinct = sort_columns.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    distinct.len() >= column_count
}

/// The merge state of one sorted group while its source stages run: the sort's merger,
/// shared by the materializing threads, and the collections they produce.
#[derive(Debug, Default)]
struct GroupSource {
    source: Option<Arc<SortSource>>,
    /// Materialized column data per merge partition (`BatchedDataCollection`).
    column_data: Vec<(usize, GroupData)>,
}

impl GroupSource {
    fn new(source: Option<SortSource>) -> Self {
        Self {
            source: source.map(Arc::new),
            column_data: Vec::new(),
        }
    }
}

/// Merges `source` into column data, one collection per merge partition this thread
/// claims (`Sort::MaterializeColumnData`).
fn materialize_column_data(
    source: &SortSource,
    buffer_manager: &BufferManager,
    types: &[LogicalType],
) -> Result<Vec<(usize, GroupData)>> {
    let mut result: Vec<(usize, GroupData)> = Vec::new();
    let mut local = source.local_state();
    let mut current: Option<(usize, GroupData, GroupDataAppend)> = None;
    loop {
        let mut chunk = DataChunk::default();
        if !source.get_data(&mut chunk, &mut local)? {
            break;
        }
        let partition = local.partition_index().unwrap_or(0);
        if current.as_ref().is_none_or(|(index, ..)| *index != partition) {
            if let Some((index, data, mut append)) = current.take() {
                data.finalize_append(&mut append);
                result.push((index, data));
            }
            let mut data = GroupData::new(buffer_manager.clone(), types.to_vec(), MemoryTag::ColumnData)?;
            let append = data.initialize_append();
            current = Some((partition, data, append));
        }
        if let Some((_, data, append)) = &mut current {
            data.append(append, &chunk)?;
        }
    }
    if let Some((index, data, mut append)) = current.take() {
        data.finalize_append(&mut append);
        result.push((index, data));
    }
    Ok(result)
}

/// Combines materialized partitions into one collection in partition order
/// (`BatchedDataCollection::FetchCollection`).
fn combine_column_data(mut column_data: Vec<(usize, GroupData)>) -> Result<Option<GroupData>> {
    column_data.sort_by_key(|(index, _)| *index);
    let mut partitions = column_data.into_iter().map(|(_, rows)| rows);
    let Some(mut result) = partitions.next() else {
        return Ok(None);
    };
    for rows in partitions {
        result.combine(rows)?;
    }
    Ok(Some(result))
}

impl SortStrategy {
    /// `PARTITION BY partition_columns ORDER BY orders` over rows of `input_types`.
    pub fn new(
        buffer_manager: BufferManager,
        partition_columns: Vec<usize>,
        partition_stats: &[Option<SortColumnStats>],
        orders: Vec<SortColumn>,
        input_types: Vec<LogicalType>,
        estimated_cardinality: usize,
        require_payload: bool,
    ) -> Result<Self> {
        if !partition_columns.is_empty() {
            return HashedSort::new(
                buffer_manager,
                partition_columns,
                partition_stats,
                orders,
                input_types,
                estimated_cardinality,
                require_payload,
            )
            .map(Self::Hashed);
        }
        if !orders.is_empty() {
            return FullSort::new(buffer_manager, orders, input_types, require_payload).map(Self::Full);
        }
        Ok(Self::Natural(NaturalSort::new(buffer_manager, input_types)))
    }

    pub fn global_state(&self, config: SortConfig) -> Result<StrategyGlobalState> {
        Ok(match self {
            Self::Hashed(hashed) => StrategyGlobalState::Hashed(hashed.global_state(config)?),
            Self::Full(full) => StrategyGlobalState::Full(full.global_state(config)),
            Self::Natural(_) => StrategyGlobalState::Natural(NaturalGlobalState::default()),
        })
    }

    pub fn local_state(&self) -> StrategyLocalState {
        match self {
            Self::Hashed(_) => StrategyLocalState::Hashed(Box::default()),
            Self::Full(full) => StrategyLocalState::Full(Box::new(full.sort.local_sink_state())),
            Self::Natural(_) => StrategyLocalState::Natural(None),
        }
    }

    fn mismatch() -> Error {
        Error::internal("sort strategy state of the wrong kind")
    }

    pub fn sink(
        &self,
        chunk: &DataChunk,
        global: &StrategyGlobalState,
        local: &mut StrategyLocalState,
    ) -> Result<()> {
        match (self, global, local) {
            (
                Self::Hashed(hashed),
                StrategyGlobalState::Hashed(global),
                StrategyLocalState::Hashed(local),
            ) => hashed.sink(chunk, global, local),
            (Self::Full(full), StrategyGlobalState::Full(global), StrategyLocalState::Full(local)) => {
                full.sink(chunk, global, local)
            }
            (
                Self::Natural(natural),
                StrategyGlobalState::Natural(global),
                StrategyLocalState::Natural(local),
            ) => natural.sink(chunk, global, local),
            _ => Err(Self::mismatch()),
        }
    }

    pub fn combine(&self, global: &StrategyGlobalState, local: StrategyLocalState) -> Result<()> {
        match (self, global, local) {
            (
                Self::Hashed(hashed),
                StrategyGlobalState::Hashed(global),
                StrategyLocalState::Hashed(local),
            ) => hashed.combine(global, *local),
            (Self::Full(full), StrategyGlobalState::Full(global), StrategyLocalState::Full(local)) => {
                full.sort.combine(global.sink()?.as_ref(), *local)
            }
            (Self::Natural(_), StrategyGlobalState::Natural(global), StrategyLocalState::Natural(local)) => {
                NaturalSort::combine(global, local)
            }
            _ => Err(Self::mismatch()),
        }
    }

    /// Makes `target` (a state of this strategy) partition on the radix bits `source`
    /// partitions on, and keeps them fixed, so both have the same hash groups
    /// (`HashedSort::Synchronize`).
    pub fn synchronize(&self, source: &StrategyGlobalState, target: &StrategyGlobalState) -> Result<()> {
        if let (
            Self::Hashed(hashed),
            StrategyGlobalState::Hashed(source),
            StrategyGlobalState::Hashed(target),
        ) = (self, source, target)
        {
            let bits = lock(&source.inner)
                .grouping
                .as_ref()
                .map_or(0, PartitionedTupleData::radix_bits);
            let mut inner = lock(&target.inner);
            let old_bits = inner
                .grouping
                .as_ref()
                .map_or(0, PartitionedTupleData::radix_bits);
            inner.fixed_bits = bits != 0;
            if bits != old_bits {
                inner.grouping = Some(hashed.create_partition(bits)?);
            }
        }
        Ok(())
    }

    /// Ends the sink phase; `false` if there is no output.
    pub fn finalize(&self, global: &mut StrategyGlobalState) -> Result<bool> {
        match (self, global) {
            (Self::Hashed(hashed), StrategyGlobalState::Hashed(global)) => Ok(hashed.finalize(global)),
            (Self::Full(full), StrategyGlobalState::Full(global)) => full.finalize(global),
            (Self::Natural(_), StrategyGlobalState::Natural(global)) => {
                Ok(lock(&global.rows).as_ref().is_some_and(|rows| rows.count() > 0))
            }
            _ => Err(Self::mismatch()),
        }
    }

    /// Chunks and rows of each hash group (`GetHashGroups`).
    pub fn hash_groups(&self, global: &StrategyGlobalState) -> Vec<ChunkRow> {
        match global {
            StrategyGlobalState::Hashed(global) => global.chunk_rows(),
            StrategyGlobalState::Full(global) => {
                let count = global.count.load(Ordering::Relaxed);
                vec![ChunkRow {
                    chunks: count.div_ceil(STANDARD_VECTOR_SIZE),
                    count,
                }]
            }
            StrategyGlobalState::Natural(global) => {
                let rows = lock(&global.rows);
                vec![ChunkRow {
                    chunks: rows.as_ref().map_or(0, GroupData::chunk_count),
                    count: rows.as_ref().map_or(0, GroupData::count),
                }]
            }
        }
    }

    /// Sorts hash group `group` (`SortColumnData`; only hashed sorts have work here).
    /// Several threads may call this for the same group: they share the scan of the
    /// group's partition, and the last one to finish finalizes the group's sort.
    pub fn sort_column_data(&self, global: &StrategyGlobalState, group: usize) -> Result<()> {
        match (self, global) {
            (Self::Hashed(hashed), StrategyGlobalState::Hashed(global)) => hashed.sort_group(global, group),
            _ => Ok(()),
        }
    }

    /// The sorted group's merge state (hashed and full sorts).
    fn group_source(global: &StrategyGlobalState, group: usize) -> Result<&Mutex<GroupSource>> {
        match global {
            StrategyGlobalState::Hashed(global) => global
                .sources
                .get(group)
                .ok_or_else(|| Error::internal("hash group out of range")),
            StrategyGlobalState::Full(global) => Ok(&global.source),
            StrategyGlobalState::Natural(_) => {
                Err(Error::internal("NaturalSort does not implement sorted runs."))
            }
        }
    }

    /// The group's partition is no longer needed once it is sorted: release it
    /// (`MaterializeHashGroupData`).
    fn release_partition(global: &StrategyGlobalState, group: usize) {
        if let StrategyGlobalState::Hashed(global) = global
            && let Some(Some(state)) = lock(&global.groups).get_mut(group)
        {
            state.partition = None;
            state.scan = None;
        }
    }

    /// Merges the partitions of group `group` this thread can claim into sorted runs
    /// (`MaterializeSortedRun`); collect them with [`Self::get_sorted_run`].
    pub fn materialize_sorted_run(&self, global: &StrategyGlobalState, group: usize) -> Result<()> {
        Self::release_partition(global, group);
        let source = lock(Self::group_source(global, group)?).source.clone();
        if let Some(source) = source {
            let mut local = source.local_state();
            source.materialize(&mut local)?;
        }
        Ok(())
    }

    /// Group `group` as one sorted run, once every materializing thread is done
    /// (`GetSortedRun`); an empty run if the group has no rows. Natural sorts have no
    /// runs.
    pub fn get_sorted_run(&self, global: &StrategyGlobalState, group: usize) -> Result<Option<SortedRun>> {
        let source = lock(Self::group_source(global, group)?).source.take();
        let run = match source {
            Some(source) => source.take_sorted_run()?,
            None => None,
        };
        match run {
            Some(run) => Ok(Some(run)),
            None => Ok(Some(self.sort()?.empty_run()?)),
        }
    }

    /// Group `group` as one sorted run, merged by the calling thread alone.
    pub fn sorted_run(&self, global: &StrategyGlobalState, group: usize) -> Result<Option<SortedRun>> {
        self.materialize_sorted_run(global, group)?;
        self.get_sorted_run(global, group)
    }

    /// Merges the partitions of group `group` this thread can claim into rows
    /// (`MaterializeColumnData`); collect them with [`Self::get_column_data`].
    pub fn materialize_column_data(&self, global: &StrategyGlobalState, group: usize) -> Result<()> {
        if matches!(self, Self::Natural(_)) {
            return Ok(());
        }
        Self::release_partition(global, group);
        let group_source = Self::group_source(global, group)?;
        let source = lock(group_source).source.clone();
        let Some(source) = source else {
            return Ok(());
        };
        let buffer_manager = self.sort()?.layouts().buffer_manager.clone();
        let column_data = materialize_column_data(&source, &buffer_manager, &source_types(self))?;
        lock(group_source).column_data.extend(column_data);
        Ok(())
    }

    /// Group `group` in order, once every materializing thread is done
    /// (`GetColumnData`); `None` if another caller took it or the group is empty. The
    /// columns are the input columns, as passed to the strategy.
    pub fn get_column_data(&self, global: &StrategyGlobalState, group: usize) -> Result<Option<GroupData>> {
        if let StrategyGlobalState::Natural(global) = global {
            return Ok(lock(&global.rows).take());
        }
        let (source, column_data) = {
            let mut group_source = lock(Self::group_source(global, group)?);
            (
                group_source.source.take(),
                std::mem::take(&mut group_source.column_data),
            )
        };
        drop(source);
        combine_column_data(column_data)
    }

    /// Group `group` in order, merged by the calling thread alone. Hashed groups must be
    /// sorted first ([`Self::sort_column_data`]).
    pub fn group_rows(&self, global: &StrategyGlobalState, group: usize) -> Result<Option<GroupData>> {
        self.materialize_column_data(global, group)?;
        self.get_column_data(global, group)
    }

    fn sort(&self) -> Result<&Sort> {
        match self {
            Self::Hashed(hashed) => Ok(&hashed.sort),
            Self::Full(full) => Ok(&full.sort),
            Self::Natural(_) => Err(Error::internal("NaturalSort does not implement sorted runs.")),
        }
    }

    /// The types of the columns of a group's rows: the input columns (and the forced
    /// payload).
    pub fn payload_types(&self) -> Vec<LogicalType> {
        source_types(self)
    }
}

fn source_types(strategy: &SortStrategy) -> Vec<LogicalType> {
    match strategy {
        SortStrategy::Hashed(hashed) => hashed.sort.output_types().to_vec(),
        SortStrategy::Full(full) => full.sort.output_types().to_vec(),
        SortStrategy::Natural(natural) => natural.types.clone(),
    }
}

/// `ORDER BY` without `PARTITION BY`: one sorted group (`FullSort`).
#[derive(Debug)]
pub struct FullSort {
    sort: Sort,
    force_payload: bool,
}

/// Global state of a [`FullSort`].
#[derive(Debug)]
pub struct FullGlobalState {
    sink: Mutex<Option<Arc<SortGlobalSinkState>>>,
    source: Mutex<GroupSource>,
    count: AtomicUsize,
}

impl FullGlobalState {
    fn sink(&self) -> Result<Arc<SortGlobalSinkState>> {
        lock(&self.sink)
            .clone()
            .ok_or_else(|| Error::internal("sort sink used after finalize"))
    }
}

impl FullSort {
    pub fn new(
        buffer_manager: BufferManager,
        orders: Vec<SortColumn>,
        mut input_types: Vec<LogicalType>,
        require_payload: bool,
    ) -> Result<Self> {
        let sort_columns: Vec<usize> = orders.iter().map(|order| order.column).collect();
        let force_payload = needs_forced_payload(require_payload, &sort_columns, input_types.len());
        if force_payload {
            input_types.push(LogicalType::BOOLEAN);
        }
        Ok(Self {
            sort: Sort::new(buffer_manager, orders, &input_types, None)?,
            force_payload,
        })
    }

    fn global_state(&self, config: SortConfig) -> FullGlobalState {
        FullGlobalState {
            sink: Mutex::new(Some(Arc::new(self.sort.global_sink_state(config)))),
            source: Mutex::new(GroupSource::default()),
            count: AtomicUsize::new(0),
        }
    }

    fn sink(
        &self,
        chunk: &DataChunk,
        global: &FullGlobalState,
        local: &mut SortLocalSinkState,
    ) -> Result<()> {
        global.count.fetch_add(chunk.len(), Ordering::Relaxed);
        let sink = global.sink()?;
        if self.force_payload {
            self.sort.sink(&with_forced_payload(chunk)?, &sink, local)
        } else {
            self.sort.sink(chunk, &sink, local)
        }
    }

    fn finalize(&self, global: &mut FullGlobalState) -> Result<bool> {
        let Some(sink) = lock(&global.sink).take() else {
            return Ok(false);
        };
        let sink =
            Arc::try_unwrap(sink).map_err(|_| Error::internal("sort sink still shared at finalize"))?;
        let source = self.sort.finalize(sink)?;
        let has_output = source.is_some();
        *lock(&global.source) = GroupSource::new(source);
        Ok(has_output)
    }
}

/// `PARTITION BY` (with or without `ORDER BY`): sorted hash groups (`HashedSort`).
#[derive(Debug)]
pub struct HashedSort {
    sort: Sort,
    partition_columns: Vec<usize>,
    force_payload: bool,
    estimated_cardinality: usize,
    buffer_manager: BufferManager,
    /// Payload (input, maybe forced column) plus the hash.
    grouping_layout: Arc<TupleDataLayout>,
}

/// One hash group's sink phase (`HashedSortGroup`).
#[derive(Debug)]
struct HashGroup {
    /// The group's partition and the scan its sorting threads share.
    partition: Option<Arc<TupleDataCollection>>,
    scan: Option<Arc<ParallelScanState>>,
    partition_count: usize,
    sink: Option<Arc<SortGlobalSinkState>>,
    /// Rows sunk into the group's sort so far.
    sorted: usize,
}

/// Global state of a [`HashedSort`].
#[derive(Debug)]
pub struct HashedGlobalState {
    config: SortConfig,
    inner: Mutex<HashedInner>,
    count: AtomicUsize,
    max_bits: usize,
    /// Per hash group: its partition and sort (after finalize).
    groups: Mutex<Vec<Option<HashGroup>>>,
    /// Per hash group: the merge state once sorted.
    sources: Vec<Mutex<GroupSource>>,
}

#[derive(Debug)]
struct HashedInner {
    grouping: Option<PartitionedTupleData>,
    fixed_bits: bool,
}

/// A thread's sink state of a [`HashedSort`].
#[derive(Debug, Default)]
pub struct HashedLocalState {
    grouping: Option<PartitionedTupleData>,
    append: Option<PartitionedAppendState>,
}

impl HashedSort {
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors DuckDB's HashedSort constructor"
    )]
    pub fn new(
        buffer_manager: BufferManager,
        partition_columns: Vec<usize>,
        partition_stats: &[Option<SortColumnStats>],
        orders: Vec<SortColumn>,
        mut input_types: Vec<LogicalType>,
        estimated_cardinality: usize,
        require_payload: bool,
    ) -> Result<Self> {
        // Sort by the partition columns (ASC NULLS FIRST) first, then by the orders.
        let mut sort_orders: Vec<SortColumn> = partition_columns
            .iter()
            .enumerate()
            .map(|(i, &column)| SortColumn {
                column,
                modifiers: OrderModifiers::new(OrderType::Ascending, NullOrder::NullsFirst),
                stats: partition_stats.get(i).copied().flatten(),
            })
            .collect();
        sort_orders.extend(orders);
        let sort_columns: Vec<usize> = sort_orders.iter().map(|order| order.column).collect();
        let force_payload = needs_forced_payload(require_payload, &sort_columns, input_types.len());
        if force_payload {
            input_types.push(LogicalType::BOOLEAN);
        }
        let projection: Vec<usize> = (0..input_types.len()).collect();
        let mut grouping_types = input_types.clone();
        grouping_types.push(LogicalType::UBIGINT);
        Ok(Self {
            sort: Sort::new(
                buffer_manager.clone(),
                sort_orders,
                &input_types,
                Some(projection),
            )?,
            partition_columns,
            force_payload,
            estimated_cardinality,
            grouping_layout: Arc::new(TupleDataLayout::new(grouping_types, ValidityType::CanHaveNull)?),
            buffer_manager,
        })
    }

    fn hash_column(&self) -> usize {
        self.grouping_layout.column_count() - 1
    }

    fn create_partition(&self, radix_bits: usize) -> Result<PartitionedTupleData> {
        PartitionedTupleData::new(
            self.buffer_manager.clone(),
            Arc::clone(&self.grouping_layout),
            MemoryTag::Window,
            radix_bits,
            self.hash_column(),
        )
    }

    fn global_state(&self, config: SortConfig) -> Result<HashedGlobalState> {
        // A thread's memory, a quarter of its share (`GetMaxThreadMemory`), in pages.
        let threads = config.threads.max(1) as u64;
        let memory_per_thread = self.buffer_manager.query_max_memory() / threads / 4;
        let thread_pages = memory_per_thread / (4 * self.buffer_manager.block_alloc_size() as u64);
        let thread_pages = if thread_pages == 0 {
            0
        } else {
            1u64 << thread_pages.ilog2()
        };
        let mut max_bits = 1;
        while max_bits < 8 && (thread_pages >> max_bits) > 1 {
            max_bits += 1;
        }
        let state = HashedGlobalState {
            config,
            inner: Mutex::new(HashedInner {
                grouping: None,
                fixed_bits: false,
            }),
            count: AtomicUsize::new(0),
            max_bits,
            groups: Mutex::new(Vec::new()),
            sources: Vec::new(),
        };
        self.rehash(&state, &mut lock(&state.inner), self.estimated_cardinality)?;
        Ok(state)
    }

    /// Grows the radix bits while partitions would exceed a row group (`Rehash`).
    fn rehash(&self, global: &HashedGlobalState, inner: &mut HashedInner, cardinality: usize) -> Result<()> {
        if inner.fixed_bits {
            return Ok(());
        }
        let bits = inner
            .grouping
            .as_ref()
            .map_or(0, PartitionedTupleData::radix_bits);
        let mut new_bits = if bits == 0 { 4 } else { bits };
        while new_bits < global.max_bits
            && cardinality / RadixPartitioning::number_of_partitions(new_bits) > DEFAULT_ROW_GROUP_SIZE
        {
            new_bits += 1;
        }
        if new_bits != bits {
            inner.grouping = Some(self.create_partition(new_bits)?);
        }
        Ok(())
    }

    /// Makes the thread's partitions match the global radix bits.
    fn sync_local(&self, global_bits: usize, local: &mut HashedLocalState) -> Result<()> {
        let (Some(grouping), Some(append)) = (&mut local.grouping, &mut local.append) else {
            let mut grouping = self.create_partition(global_bits)?;
            local.append = Some(grouping.initialize_append_state(PinProperties::UnpinAfterDone));
            local.grouping = Some(grouping);
            return Ok(());
        };
        if grouping.radix_bits() == global_bits {
            return Ok(());
        }
        grouping.flush_append_state(append);
        let mut repartitioned = self.create_partition(global_bits)?;
        grouping.repartition(&mut repartitioned)?;
        local.append = Some(repartitioned.initialize_append_state(PinProperties::UnpinAfterDone));
        local.grouping = Some(repartitioned);
        Ok(())
    }

    fn sink(
        &self,
        chunk: &DataChunk,
        global: &HashedGlobalState,
        local: &mut HashedLocalState,
    ) -> Result<()> {
        global.count.fetch_add(chunk.len(), Ordering::Relaxed);
        {
            let mut inner = lock(&global.inner);
            if local.grouping.is_some() {
                self.rehash(global, &mut inner, global.count.load(Ordering::Relaxed))?;
            }
            let bits = inner
                .grouping
                .as_ref()
                .map_or(4, PartitionedTupleData::radix_bits);
            self.sync_local(bits, local)?;
        }
        let input = if self.force_payload {
            with_forced_payload(chunk)?
        } else {
            chunk.clone()
        };
        let partition_columns: Vec<&Vector> = self
            .partition_columns
            .iter()
            .map(|&column| chunk.column(column))
            .collect();
        let hashes = hash_columns(&partition_columns, chunk.len())?;
        let mut columns = input.into_columns();
        columns.push(hashes);
        let payload = DataChunk::new(columns, chunk.len());
        if let (Some(grouping), Some(append)) = (&mut local.grouping, &mut local.append) {
            grouping.append(append, &payload)?;
        }
        Ok(())
    }

    fn combine(&self, global: &HashedGlobalState, mut local: HashedLocalState) -> Result<()> {
        let (Some(mut grouping), Some(mut append)) = (local.grouping.take(), local.append.take()) else {
            return Ok(());
        };
        grouping.flush_append_state(&mut append);
        let mut inner = lock(&global.inner);
        let bits = inner
            .grouping
            .as_ref()
            .map_or(4, PartitionedTupleData::radix_bits);
        if grouping.radix_bits() != bits {
            let mut repartitioned = self.create_partition(bits)?;
            grouping.repartition(&mut repartitioned)?;
            grouping = repartitioned;
        }
        inner.fixed_bits = true;
        if let Some(target) = &mut inner.grouping {
            target.combine(grouping)
        } else {
            inner.grouping = Some(grouping);
            Ok(())
        }
    }

    /// Prepares each non-empty partition's sort and the scan its sorting threads share
    /// (`HashedSort::Finalize`).
    fn finalize(&self, global: &mut HashedGlobalState) -> bool {
        if global.count.load(Ordering::Relaxed) == 0 {
            return false;
        }
        let mut inner = lock(&global.inner);
        let Some(grouping) = &mut inner.grouping else {
            return false;
        };
        let partitions = grouping.take_partitions();
        let columns: Vec<usize> = (0..self.hash_column()).collect();
        let groups: Vec<Option<HashGroup>> = partitions
            .into_iter()
            .map(|partition| {
                (partition.count() > 0).then(|| {
                    // Prepare to scan into the sort.
                    let scan = partition
                        .initialize_parallel_scan(Some(columns.clone()), PinProperties::UnpinAfterDone);
                    HashGroup {
                        partition_count: partition.count(),
                        partition: Some(Arc::new(partition)),
                        scan: Some(Arc::new(scan)),
                        sink: Some(Arc::new(self.sort.global_sink_state(global.config.clone()))),
                        sorted: 0,
                    }
                })
            })
            .collect();
        global.sources = (0..groups.len())
            .map(|_| Mutex::new(GroupSource::default()))
            .collect();
        *lock(&global.groups) = groups;
        true
    }

    /// Sinks this thread's share of group `group`'s partition into the group's sort; the
    /// thread that completes the group finalizes the sort (`SortColumnData`).
    fn sort_group(&self, global: &HashedGlobalState, group: usize) -> Result<()> {
        let (partition, scan, sink) = {
            let groups = lock(&global.groups);
            let Some(Some(state)) = groups.get(group) else {
                return Ok(());
            };
            match (&state.partition, &state.scan, &state.sink) {
                (Some(partition), Some(scan), Some(sink)) => {
                    (Arc::clone(partition), Arc::clone(scan), Arc::clone(sink))
                }
                _ => return Ok(()),
            }
        };
        let mut local_scan = partition.initialize_local_scan(&scan);
        let mut local = self.sort.local_sink_state();
        let mut combined = 0;
        let mut chunk = DataChunk::default();
        while partition.parallel_scan(&scan, &mut local_scan, &mut chunk)? {
            combined += chunk.len();
            self.sort.sink(&chunk, &sink, &mut local)?;
        }
        drop(local_scan);
        self.sort.combine(&sink, local)?;
        drop(sink);
        // Whoever finishes last can finalize.
        let finished_sink = {
            let mut groups = lock(&global.groups);
            let Some(Some(state)) = groups.get_mut(group) else {
                return Ok(());
            };
            state.sorted += combined;
            if state.sorted == state.partition_count {
                state.sink.take()
            } else {
                None
            }
        };
        if let Some(sink) = finished_sink {
            // Other threads may still hold their clone for a moment after combining.
            let mut sink = sink;
            let sink = loop {
                match Arc::try_unwrap(sink) {
                    Ok(sink) => break sink,
                    Err(shared) => {
                        sink = shared;
                        std::thread::yield_now();
                    }
                }
            };
            let source = self.sort.finalize(sink)?;
            *lock(&global.sources[group]) = GroupSource::new(source);
        }
        Ok(())
    }
}

impl HashedGlobalState {
    fn chunk_rows(&self) -> Vec<ChunkRow> {
        let groups = lock(&self.groups);
        groups
            .iter()
            .map(|group| {
                let count = group.as_ref().map_or(0, |group| group.partition_count);
                ChunkRow {
                    chunks: count.div_ceil(STANDARD_VECTOR_SIZE),
                    count,
                }
            })
            .collect()
    }
}

/// Neither `PARTITION BY` nor `ORDER BY`: the input as is (`NaturalSort`).
#[derive(Debug)]
pub struct NaturalSort {
    buffer_manager: BufferManager,
    types: Vec<LogicalType>,
}

/// Global state of a [`NaturalSort`].
#[derive(Debug, Default)]
pub struct NaturalGlobalState {
    rows: Mutex<Option<GroupData>>,
}

impl NaturalSort {
    pub fn new(buffer_manager: BufferManager, types: Vec<LogicalType>) -> Self {
        Self {
            buffer_manager,
            types,
        }
    }

    fn sink(
        &self,
        chunk: &DataChunk,
        _global: &NaturalGlobalState,
        local: &mut Option<Box<(GroupData, GroupDataAppend)>>,
    ) -> Result<()> {
        if local.is_none() {
            let mut data = GroupData::new(
                self.buffer_manager.clone(),
                self.types.clone(),
                MemoryTag::ColumnData,
            )?;
            let append = data.initialize_append();
            *local = Some(Box::new((data, append)));
        }
        if let Some(local) = local {
            let (data, append) = &mut **local;
            data.append(append, chunk)?;
        }
        Ok(())
    }

    fn combine(global: &NaturalGlobalState, local: Option<Box<(GroupData, GroupDataAppend)>>) -> Result<()> {
        let Some(local) = local else {
            return Ok(());
        };
        let (local, mut append) = *local;
        local.finalize_append(&mut append);
        let mut rows = lock(&global.rows);
        if let Some(rows) = &mut *rows {
            rows.combine(local)
        } else {
            *rows = Some(local);
            Ok(())
        }
    }
}
