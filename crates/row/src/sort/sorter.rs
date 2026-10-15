//! The sort operator core (DuckDB's `Sort`): sink into per-thread runs within a memory
//! reservation, then merge.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_buffer::{BufferManager, TemporaryMemoryContext, TemporaryMemoryState};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Vector};

use super::encode::{OrderModifiers, create_sort_keys, sort_key_type_for};
use super::merger::{DEFAULT_ROW_GROUP_SIZE, MergerLocalState, SortedRunMerger};
use super::run::SortedRun;
use super::{ColumnSource, OutputColumn, SortLayouts};
use crate::format::ChunkFormat;
use crate::layout::{SortKeyColumn, TupleDataLayout, ValidityType};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Statistics of an ORDER BY column that make keys narrower.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SortColumnStats {
    pub can_have_null: bool,
    /// Longest string, for VARCHAR columns.
    pub max_string_length: Option<u32>,
}

/// One ORDER BY column: an input column and its modifiers (the binder has already turned
/// expressions into columns).
#[derive(Clone, Debug)]
pub struct SortColumn {
    pub column: usize,
    pub modifiers: OrderModifiers,
    pub stats: Option<SortColumnStats>,
}

/// Execution settings of a sort.
#[derive(Clone, Debug, Default)]
pub struct SortConfig {
    /// Worker threads (`threads`).
    pub threads: usize,
    /// Open connections (shares memory between concurrent queries).
    pub connections: usize,
    /// `SET force_external`: spill-friendly runs even when memory suffices.
    pub force_external: bool,
    /// `verify_parallelism`: tiny merge partitions.
    pub verify_parallelism: bool,
    /// The query's interrupt flag.
    pub interrupted: Option<Arc<AtomicBool>>,
}

/// Sorts chunks by key columns; see the module documentation.
#[derive(Debug)]
pub struct Sort {
    layouts: Arc<SortLayouts>,
    orders: Vec<SortColumn>,
    /// Input columns stored in the payload.
    payload_columns: Vec<usize>,
    index_creation: bool,
}

impl Sort {
    /// A sort of rows with `input_types` by `orders`, producing the input columns in
    /// `projection` (all by default). Projected columns that are ORDER BY columns are decoded
    /// from the keys instead of being stored twice.
    pub fn new(
        buffer_manager: BufferManager,
        orders: Vec<SortColumn>,
        input_types: &[LogicalType],
        projection: Option<Vec<usize>>,
    ) -> Result<Self> {
        if orders.is_empty() {
            return Err(Error::internal("a sort needs at least one ORDER BY column"));
        }
        let key_types: Vec<LogicalType> = orders
            .iter()
            .map(|order| {
                input_types
                    .get(order.column)
                    .cloned()
                    .ok_or_else(|| Error::internal("ORDER BY column out of range"))
            })
            .collect::<Result<_>>()?;
        let key_type = sort_key_type_for(&key_types);
        let projection = projection.unwrap_or_else(|| (0..input_types.len()).collect());
        let mut input_to_key = HashMap::new();
        for (key, order) in orders.iter().enumerate() {
            input_to_key.entry(order.column).or_insert(key);
        }
        let mut outputs = Vec::with_capacity(projection.len());
        let mut output_types = Vec::with_capacity(projection.len());
        let mut payload_types = Vec::new();
        let mut payload_columns = Vec::new();
        for (output, &input) in projection.iter().enumerate() {
            let ty = input_types
                .get(input)
                .cloned()
                .ok_or_else(|| Error::internal("projected column out of range"))?;
            let source = if let Some(&key) = input_to_key.get(&input) {
                ColumnSource::Key(key)
            } else {
                payload_types.push(ty.clone());
                payload_columns.push(input);
                ColumnSource::Payload(payload_types.len() - 1)
            };
            outputs.push(OutputColumn { source, output });
            output_types.push(ty);
        }
        let payload_layout = if payload_types.is_empty() {
            None
        } else {
            Some(Arc::new(TupleDataLayout::new(
                payload_types,
                ValidityType::CanHaveNull,
            )?))
        };
        let key_columns: Vec<SortKeyColumn> = orders
            .iter()
            .zip(&key_types)
            .map(|(order, ty)| SortKeyColumn {
                physical_type: ty.physical_type(),
                is_varchar: ty.id() == LogicalTypeId::Varchar,
                can_have_null: order.stats.map(|stats| stats.can_have_null),
                max_string_length: order.stats.and_then(|stats| stats.max_string_length),
            })
            .collect();
        let key_layout = TupleDataLayout::sort_key(&key_columns, &key_type, payload_layout.is_some());
        let Some(sort_key_type) = key_layout.sort_key_type() else {
            return Err(Error::internal("sort key layout without key type"));
        };
        Ok(Self {
            layouts: Arc::new(SortLayouts {
                buffer_manager,
                key_layout: Arc::new(key_layout),
                payload_layout,
                key_type: sort_key_type,
                key_types,
                modifiers: orders.iter().map(|order| order.modifiers).collect(),
                outputs,
                output_types,
            }),
            orders,
            payload_columns,
            index_creation: false,
        })
    }

    /// Marks this as an (approximate) index-creation sort.
    #[must_use]
    pub fn index_sort(mut self) -> Self {
        self.index_creation = true;
        self
    }

    /// Types of the produced columns.
    pub fn output_types(&self) -> &[LogicalType] {
        &self.layouts.output_types
    }

    pub(crate) fn layouts(&self) -> &Arc<SortLayouts> {
        &self.layouts
    }

    /// The shared state of the sink phase.
    pub fn global_sink_state(&self, config: SortConfig) -> SortGlobalSinkState {
        let threads = config.threads.max(1);
        let context = self.memory_context(&config);
        let state = self
            .layouts
            .buffer_manager
            .temporary_memory_manager()
            .register(&context);
        SortGlobalSinkState {
            external: AtomicBool::new(config.force_external),
            config,
            threads,
            temporary_memory: state,
            inner: Mutex::new(GlobalInner::default()),
            sorted_tuples: AtomicUsize::new(0),
        }
    }

    fn memory_context(&self, config: &SortConfig) -> TemporaryMemoryContext {
        self.layouts.buffer_manager.temporary_memory_context(
            config.threads.max(1) as u64,
            config.connections.max(1) as u64,
            config.force_external,
        )
    }

    /// A thread's sink state.
    pub fn local_sink_state(&self) -> SortLocalSinkState {
        SortLocalSinkState {
            sorted_run: None,
            maximum_run_size: 0,
            external: false,
        }
    }

    /// Adds a chunk to the thread's run (`Sort::Sink`).
    pub fn sink(
        &self,
        chunk: &DataChunk,
        global: &SortGlobalSinkState,
        local: &mut SortLocalSinkState,
    ) -> Result<()> {
        if local.sorted_run.is_none() {
            // An index sort could be approximate, but (like DuckDB) runs are sorted exactly.
            local.sorted_run = Some(SortedRun::new(&self.layouts)?);
            global.update_local_state(local);
        }
        let count = chunk.len();
        let key_columns: Vec<&Vector> = self
            .orders
            .iter()
            .map(|order| chunk.column(order.column))
            .collect();
        let keys = create_sort_keys(&key_columns, &self.layouts.modifiers, count)?;
        let payload: Vec<Vector> = self
            .payload_columns
            .iter()
            .map(|&column| chunk.column(column).clone())
            .collect();
        let formats = if payload.is_empty() {
            None
        } else {
            Some(ChunkFormat::from_columns(&payload, count)?)
        };
        if let Some(run) = &mut local.sorted_run {
            run.sink(&keys, formats.as_ref(), count)?;
        }
        if Self::try_finish_sink(global, local)? {
            return Ok(());
        }
        // The run outgrew its share: refresh the share and check again.
        global.update_local_state(local);
        if Self::try_finish_sink(global, local)? {
            return Ok(());
        }
        // Still too large: ask for more memory (or switch to an external sort).
        {
            let mut inner = global.lock();
            global.update_local_state(local);
            global.try_increase_reservation(self, &mut inner, local);
            global.update_local_state(local);
        }
        // Without enough memory this may still not finish; pins will then fail with an
        // out-of-memory error, as in DuckDB.
        Self::try_finish_sink(global, local)?;
        Ok(())
    }

    /// Whether the sink call is done: the run is small enough, or (external sorts) it was
    /// finalized and handed to the global state (`TryFinishSink`).
    fn try_finish_sink(global: &SortGlobalSinkState, local: &mut SortLocalSinkState) -> Result<bool> {
        let Some(run) = &local.sorted_run else {
            return Ok(true);
        };
        if run.size_in_bytes() < local.maximum_run_size {
            return Ok(true);
        }
        if !local.external {
            return Ok(false);
        }
        if let Some(mut run) = local.sorted_run.take() {
            run.finalize(true, global.config.interrupted.as_deref())?;
            global.add_sorted_run(run);
        }
        Ok(true)
    }

    /// Finishes the thread's run and hands it to the global state (`Sort::Combine`).
    pub fn combine(&self, global: &SortGlobalSinkState, mut local: SortLocalSinkState) -> Result<()> {
        let Some(mut run) = local.sorted_run.take() else {
            return Ok(());
        };
        global.lock().any_combined = true;
        run.finalize(
            global.external.load(Ordering::Acquire),
            global.config.interrupted.as_deref(),
        )?;
        global.add_sorted_run(run);
        Ok(())
    }

    /// Ends the sink phase (`Sort::Finalize`); `None` if there were no rows.
    pub fn finalize(&self, global: SortGlobalSinkState) -> Result<Option<SortSource>> {
        let SortGlobalSinkState {
            config,
            temporary_memory,
            inner,
            external,
            threads,
            ..
        } = global;
        let runs = inner
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .sorted_runs;
        if runs.is_empty() {
            return Ok(None);
        }
        let total: usize = runs.iter().map(SortedRun::count).sum();
        if total == 0 {
            return Ok(None);
        }
        let partition_size = if config.verify_parallelism {
            STANDARD_VECTOR_SIZE
        } else {
            total.min(DEFAULT_ROW_GROUP_SIZE)
        };
        let merger = SortedRunMerger::new(
            Arc::clone(&self.layouts),
            runs,
            partition_size,
            external.load(Ordering::Acquire),
            self.index_creation,
            threads,
        )?;
        Ok(Some(SortSource {
            merger,
            _temporary_memory: temporary_memory,
        }))
    }
}

#[derive(Debug, Default)]
struct GlobalInner {
    sorted_runs: Vec<SortedRun>,
    any_combined: bool,
}

/// Shared state of the sink phase (`SortGlobalSinkState`).
#[derive(Debug)]
pub struct SortGlobalSinkState {
    config: SortConfig,
    threads: usize,
    temporary_memory: TemporaryMemoryState,
    inner: Mutex<GlobalInner>,
    external: AtomicBool,
    sorted_tuples: AtomicUsize,
}

impl SortGlobalSinkState {
    fn lock(&self) -> MutexGuard<'_, GlobalInner> {
        lock(&self.inner)
    }

    /// Whether runs are being written for an external merge.
    pub fn is_external(&self) -> bool {
        self.external.load(Ordering::Acquire)
    }

    /// Rows already in sorted runs (progress).
    pub fn sorted_tuples(&self) -> usize {
        self.sorted_tuples.load(Ordering::Relaxed)
    }

    fn update_local_state(&self, local: &mut SortLocalSinkState) {
        local.maximum_run_size = (self.temporary_memory.reservation() / self.threads as u64) as usize;
        local.external = self.is_external();
    }

    fn add_sorted_run(&self, run: SortedRun) {
        self.sorted_tuples.fetch_add(run.count(), Ordering::Relaxed);
        self.lock().sorted_runs.push(run);
    }

    /// Asks for twice the memory until the runs fit, switching to external sorting if the
    /// request is not granted (`TryIncreaseReservation`).
    fn try_increase_reservation(&self, sort: &Sort, inner: &mut GlobalInner, local: &SortLocalSinkState) {
        if self.is_external() {
            return;
        }
        let state = &self.temporary_memory;
        if state.reservation() < state.remaining_size() {
            if !inner.any_combined {
                self.external.store(true, Ordering::Release);
            }
            return;
        }
        let run_size = local.sorted_run.as_ref().map_or(0, SortedRun::size_in_bytes) as u64;
        let mut required = self.threads as u64 * run_size;
        if sort.index_creation {
            required *= 4;
        }
        let mut request = state.remaining_size().max(1) * 2;
        while request < required {
            request *= 2;
        }
        state.set_remaining_size_and_update_reservation(&sort.memory_context(&self.config), request);
        if state.reservation() < required && !inner.any_combined {
            self.external.store(true, Ordering::Release);
        }
    }
}

/// A thread's sink state (`SortLocalSinkState`).
#[derive(Debug)]
pub struct SortLocalSinkState {
    sorted_run: Option<SortedRun>,
    maximum_run_size: usize,
    external: bool,
}

/// The source phase of a sort: the merger over all runs (`SortGlobalSourceState`).
#[derive(Debug)]
pub struct SortSource {
    merger: SortedRunMerger,
    _temporary_memory: TemporaryMemoryState,
}

impl SortSource {
    /// Threads that can produce output in parallel.
    pub fn max_threads(&self) -> usize {
        self.merger.max_threads()
    }

    /// Total rows.
    pub fn count(&self) -> usize {
        self.merger.total_count
    }

    /// Whether the runs were written for an external merge.
    pub fn is_external(&self) -> bool {
        self.merger.external
    }

    /// Rows produced so far.
    pub fn scanned(&self) -> usize {
        self.merger.scanned()
    }

    pub fn local_state(&self) -> MergerLocalState {
        self.merger.local_state()
    }

    /// Reads the next sorted chunk; `false` when this thread has nothing more to produce.
    /// Chunks of one partition come in order; partitions are numbered by
    /// [`MergerLocalState::partition_index`].
    pub fn get_data(&self, chunk: &mut DataChunk, local: &mut MergerLocalState) -> Result<bool> {
        self.merger.get_data(chunk, local)?;
        Ok(!chunk.is_empty())
    }

    /// Merges into sorted runs instead of chunks (`MaterializeSortedRun`); call from every
    /// thread, then [`Self::take_sorted_run`].
    pub fn materialize(&self, local: &mut MergerLocalState) -> Result<()> {
        self.merger.materialize(local)
    }

    /// The materialized result as one sorted run (`GetSortedRun`).
    pub fn take_sorted_run(&self) -> Result<Option<SortedRun>> {
        self.merger.take_sorted_run()
    }

    /// Every remaining row, in order (single-threaded convenience).
    pub fn collect(&self) -> Result<Vec<DataChunk>> {
        let mut local = self.local_state();
        let mut chunks = Vec::new();
        loop {
            let mut chunk = DataChunk::default();
            if !self.get_data(&mut chunk, &mut local)? {
                break;
            }
            chunks.push(chunk);
        }
        Ok(chunks)
    }
}
