//! How partitioned operators (window functions) get their input grouped and sorted
//! (DuckDB's `SortStrategy`: `HashedSort`, `FullSort`, `NaturalSort`).
//!
//! - [`HashedSort`] (`PARTITION BY ...`): rows are radix-partitioned on the hash of the
//!   partition columns into hash groups; each group is sorted by the partition columns
//!   (ASC NULLS FIRST) followed by the ORDER BY columns.
//! - [`FullSort`] (`ORDER BY` only): one group, sorted.
//! - [`NaturalSort`] (neither): one group in input order.
//!
//! A hash group is handed out either as a sorted run or as a materialized collection.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_buffer::{BufferManager, MemoryTag};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use super::encode::{NullOrder, OrderModifiers, OrderType};
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
use crate::state::{PinProperties, ScanState};

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
    Natural(Option<TupleDataCollection>),
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
                    chunks: rows.as_ref().map_or(0, TupleDataCollection::chunk_count),
                    count: rows.as_ref().map_or(0, TupleDataCollection::count),
                }]
            }
        }
    }

    /// Sorts hash group `group` (hashed strategy only; `SortColumnData`). Several threads
    /// may call this for the same group; the last one to finish finalizes it.
    pub fn sort_hash_group(&self, global: &StrategyGlobalState, group: usize) -> Result<()> {
        match (self, global) {
            (Self::Hashed(hashed), StrategyGlobalState::Hashed(global)) => hashed.sort_group(global, group),
            _ => Ok(()),
        }
    }

    /// Hash group `group` as one sorted run (`GetSortedRun`). Natural sorts have no runs.
    pub fn sorted_run(&self, global: &StrategyGlobalState, group: usize) -> Result<Option<SortedRun>> {
        let source = match global {
            StrategyGlobalState::Hashed(global) => global.group_source(group),
            StrategyGlobalState::Full(global) => lock(&global.source).take(),
            StrategyGlobalState::Natural(_) => {
                return Err(Error::internal("NaturalSort does not implement sorted runs."));
            }
        };
        let Some(source) = source else {
            return Ok(None);
        };
        let mut local = source.local_state();
        source.materialize(&mut local)?;
        source.take_sorted_run()
    }

    /// Hash group `group` materialized in order (`GetColumnData`).
    pub fn group_rows(
        &self,
        global: &StrategyGlobalState,
        group: usize,
    ) -> Result<Option<TupleDataCollection>> {
        let source = match global {
            StrategyGlobalState::Hashed(global) => global.group_source(group),
            StrategyGlobalState::Full(global) => lock(&global.source).take(),
            StrategyGlobalState::Natural(global) => return Ok(lock(&global.rows).take()),
        };
        let Some(source) = source else {
            return Ok(None);
        };
        let buffer_manager = match self {
            Self::Hashed(hashed) => hashed.sort.layouts().buffer_manager.clone(),
            Self::Full(full) => full.sort.layouts().buffer_manager.clone(),
            Self::Natural(natural) => natural.buffer_manager.clone(),
        };
        let types = source_types(self);
        let layout = Arc::new(TupleDataLayout::new(types, ValidityType::CanHaveNull)?);
        let mut rows = TupleDataCollection::new(buffer_manager, layout, MemoryTag::Window)?;
        let mut append = rows.initialize_append(PinProperties::UnpinAfterDone);
        let mut local = source.local_state();
        loop {
            let mut chunk = DataChunk::default();
            if !source.get_data(&mut chunk, &mut local)? {
                break;
            }
            rows.append(&mut append, &chunk)?;
        }
        rows.finalize_pin_state(&mut append.pin_state);
        Ok(Some(rows))
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
    source: Mutex<Option<SortSource>>,
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
            source: Mutex::new(None),
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
        *lock(&global.source) = source;
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

/// One hash group's sort.
#[derive(Debug)]
struct HashGroup {
    sink: Option<Arc<SortGlobalSinkState>>,
    source: Option<SortSource>,
    sorted: usize,
}

/// Global state of a [`HashedSort`].
#[derive(Debug)]
pub struct HashedGlobalState {
    config: SortConfig,
    inner: Mutex<HashedInner>,
    count: AtomicUsize,
    max_bits: usize,
    /// Per hash group: rows (after finalize) and its sort.
    groups: Mutex<Vec<Option<HashGroup>>>,
    /// The partitions being sorted, consumed per group.
    partitions: Mutex<Vec<Option<TupleDataCollection>>>,
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
            partitions: Mutex::new(Vec::new()),
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

    fn finalize(&self, global: &mut HashedGlobalState) -> bool {
        if global.count.load(Ordering::Relaxed) == 0 {
            return false;
        }
        let mut inner = lock(&global.inner);
        let Some(grouping) = &mut inner.grouping else {
            return false;
        };
        let partitions = grouping.take_partitions();
        let groups = partitions
            .iter()
            .map(|partition| {
                (partition.count() > 0).then(|| HashGroup {
                    sink: Some(Arc::new(self.sort.global_sink_state(global.config.clone()))),
                    source: None,
                    sorted: 0,
                })
            })
            .collect();
        *lock(&global.groups) = groups;
        *lock(&global.partitions) = partitions.into_iter().map(Some).collect();
        true
    }

    /// Sorts one hash group from its partition (single caller per group here; DuckDB lets
    /// several threads share the scan).
    fn sort_group(&self, global: &HashedGlobalState, group: usize) -> Result<()> {
        let Some(partition) = lock(&global.partitions).get_mut(group).and_then(Option::take) else {
            return Ok(());
        };
        let sink = {
            let groups = lock(&global.groups);
            match groups
                .get(group)
                .and_then(Option::as_ref)
                .and_then(|group| group.sink.clone())
            {
                Some(sink) => sink,
                None => return Ok(()),
            }
        };
        let columns: Vec<usize> = (0..self.hash_column()).collect();
        let mut scan: ScanState = partition.initialize_scan_columns(columns, PinProperties::DestroyAfterDone);
        let mut local = self.sort.local_sink_state();
        let mut sorted = 0;
        loop {
            let mut chunk = DataChunk::default();
            if !partition.scan(&mut scan, &mut chunk)? {
                break;
            }
            sorted += chunk.len();
            self.sort.sink(&chunk, &sink, &mut local)?;
        }
        self.sort.combine(&sink, local)?;
        drop(sink);
        let mut groups = lock(&global.groups);
        if let Some(Some(state)) = groups.get_mut(group) {
            state.sorted += sorted;
            if state.sorted == partition.count()
                && let Some(sink) = state.sink.take()
            {
                let sink =
                    Arc::try_unwrap(sink).map_err(|_| Error::internal("hash group sink still shared"))?;
                state.source = self.sort.finalize(sink)?;
            }
        }
        Ok(())
    }
}

impl HashedGlobalState {
    fn chunk_rows(&self) -> Vec<ChunkRow> {
        let partitions = lock(&self.partitions);
        let groups = lock(&self.groups);
        groups
            .iter()
            .enumerate()
            .map(|(index, group)| {
                let count = match group {
                    Some(group) if group.source.is_some() => {
                        group.source.as_ref().map_or(0, SortSource::count)
                    }
                    Some(_) => partitions
                        .get(index)
                        .and_then(Option::as_ref)
                        .map_or(0, TupleDataCollection::count),
                    None => 0,
                };
                ChunkRow {
                    chunks: count.div_ceil(STANDARD_VECTOR_SIZE),
                    count,
                }
            })
            .collect()
    }

    fn group_source(&self, group: usize) -> Option<SortSource> {
        lock(&self.groups)
            .get_mut(group)
            .and_then(Option::as_mut)
            .and_then(|group| group.source.take())
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
    rows: Mutex<Option<TupleDataCollection>>,
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
        local: &mut Option<TupleDataCollection>,
    ) -> Result<()> {
        if local.is_none() {
            let layout = Arc::new(TupleDataLayout::new(
                self.types.clone(),
                ValidityType::CanHaveNull,
            )?);
            *local = Some(TupleDataCollection::new(
                self.buffer_manager.clone(),
                layout,
                MemoryTag::Window,
            )?);
        }
        if let Some(rows) = local {
            rows.append_chunk(chunk)?;
        }
        Ok(())
    }

    fn combine(global: &NaturalGlobalState, local: Option<TupleDataCollection>) -> Result<()> {
        let Some(local) = local else {
            return Ok(());
        };
        let mut rows = lock(&global.rows);
        if let Some(rows) = &mut *rows {
            rows.combine(local)
        } else {
            *rows = Some(local);
            Ok(())
        }
    }
}
