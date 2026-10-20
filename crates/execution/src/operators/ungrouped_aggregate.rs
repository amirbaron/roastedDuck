use crate::operator::aggregate_names;
use rustdb_common::tree_renderer::RenderParams;
use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::AggregateInput;
use rustdb_function::{AggregateOps, StatePtr};
use rustdb_planner::expression::BoundAggregateExpression as BoundAggregate;
use rustdb_row::hash::hash_columns;
use rustdb_row::{PartitionedTupleData, TupleDataCollection};
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use crate::aggregate_hashtable::{GroupedAggregateHashTable, initial_capacity};
use crate::operators::radix_sink::{RadixSinkConfig, RadixSinkTable, initial_radix_bits};
use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::operators::hash_aggregate::{
    abandon_table, external_radix_bits, memory_pressure, repartitioned, table_data, thread_memory_limit,
};
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// One state per aggregate, allocated together in a single aligned buffer.
pub struct AggregateStates {
    memory: NonNull<u8>,
    layout: Layout,
    offsets: Vec<usize>,
    ops: Vec<std::sync::Arc<dyn AggregateOps>>,
}

// SAFETY: the buffer is uniquely owned; states are only touched through `&mut self` or
// under the global state's mutex.
unsafe impl Send for AggregateStates {}

impl std::fmt::Debug for AggregateStates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AggregateStates")
            .field("layout", &self.layout)
            .field("offsets", &self.offsets)
            .finish_non_exhaustive()
    }
}

impl AggregateStates {
    fn new(aggregates: &[BoundAggregate]) -> Self {
        let mut layout = Layout::new::<()>();
        let mut offsets = Vec::with_capacity(aggregates.len());
        for aggregate in aggregates {
            let (extended, offset) = layout
                .extend(aggregate.function.ops.state_layout())
                .unwrap_or_else(|_| handle_alloc_error(layout));
            layout = extended;
            offsets.push(offset);
        }
        let layout = layout.pad_to_align();
        let memory = if layout.size() == 0 {
            NonNull::<u8>::dangling()
        } else {
            // SAFETY: non-zero size.
            NonNull::new(unsafe { alloc_zeroed(layout) }).unwrap_or_else(|| handle_alloc_error(layout))
        };
        let states = Self {
            memory,
            layout,
            offsets,
            ops: aggregates.iter().map(|a| a.function.ops.clone()).collect(),
        };
        for (index, ops) in states.ops.iter().enumerate() {
            // SAFETY: each offset is within the buffer and aligned for that state.
            unsafe { ops.initialize(states.state(index)) };
        }
        states
    }

    fn state(&self, index: usize) -> StatePtr {
        // SAFETY: offsets come from `Layout::extend` over this buffer's layout.
        unsafe { StatePtr::new(self.memory.add(self.offsets[index])) }
    }
}

impl Drop for AggregateStates {
    fn drop(&mut self) {
        for (index, ops) in self.ops.iter().enumerate() {
            if ops.needs_destructor() {
                // SAFETY: initialised in `new`, destroyed exactly once here.
                unsafe { ops.destroy(&[self.state(index)]) };
            }
        }
        if self.layout.size() != 0 {
            // SAFETY: allocated in `new` with this layout.
            unsafe { dealloc(self.memory.as_ptr(), self.layout) };
        }
    }
}

/// Aggregates without GROUP BY (DuckDB's `PhysicalUngroupedAggregate`): each thread
/// aggregates into local states, which are combined into the global states.
#[derive(Debug)]
pub struct PhysicalUngroupedAggregate {
    types: Vec<LogicalType>,
    aggregates: Vec<BoundAggregate>,
    children: [PhysicalRef; 1],
}

impl PhysicalUngroupedAggregate {
    pub fn new(types: Vec<LogicalType>, aggregates: Vec<BoundAggregate>, child: PhysicalRef) -> Result<Self> {
        if let Some(aggregate) = aggregates.iter().find(|a| a.order_bys.is_some()) {
            return Err(Error::not_implemented(format!(
                "ordered aggregate {}",
                aggregate.function.name
            )));
        }
        Ok(Self {
            types,
            aggregates,
            children: [child],
        })
    }
}

impl PhysicalOperator for PhysicalUngroupedAggregate {
    fn name(&self) -> &'static str {
        "UNGROUPED_AGGREGATE"
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
    fn sink_order_dependent(&self) -> bool {
        self.aggregates.iter().any(|aggregate| {
            aggregate.function.order_dependence == rustdb_function::aggregate::OrderDependence::OrderDependent
        })
    }

    fn params_to_string(&self, _: u64) -> RenderParams {
        let mut result = RenderParams::new();
        result.insert("Aggregates".to_owned(), aggregate_names(&self.aggregates));
        result
    }
}

/// The global aggregate states, and for each `DISTINCT` aggregate the threads'
/// partitioned tables of its distinct argument tuples (DuckDB's `DistinctAggregateData`),
/// which parallel finalize tasks deduplicate partition by partition and feed into its
/// state (`UngroupedDistinctAggregateFinalizeEvent`).
#[derive(Debug)]
pub struct UngroupedGlobal {
    states: Mutex<AggregateStates>,
    /// Per aggregate: the threads' distinct tuples (`None` unless DISTINCT), and whether
    /// each thread's are distinct (none were set aside).
    distinct: Vec<Option<Mutex<Vec<ThreadTuples>>>>,
    /// Per aggregate: how the threads fill their tables of distinct tuples (`None` unless
    /// DISTINCT).
    distinct_configs: Vec<Option<RadixSinkConfig>>,
    /// After `finalize`: one unit of work per DISTINCT aggregate and radix partition.
    distinct_partitions: Vec<DistinctPartition>,
    /// Whether the distinct partitions were handed out as finalize tasks.
    distinct_scheduled: AtomicBool,
}

/// One thread's distinct tuples of an aggregate, and whether they are distinct (none were
/// set aside).
type ThreadTuples = (PartitionedTupleData, bool);

/// The tuples of one radix partition of one DISTINCT aggregate's arguments, from every
/// thread.
#[derive(Debug)]
struct DistinctPartition {
    aggregate: usize,
    data: Mutex<Vec<TupleDataCollection>>,
    /// Whether the tuples are distinct already (they came from one thread's table).
    unique: bool,
}

/// Per-thread state: local aggregate states, the DISTINCT tables, and executors for
/// arguments and filters.
#[derive(Debug)]
pub struct UngroupedLocal {
    states: AggregateStates,
    /// Per aggregate: the thread's table of distinct argument tuples.
    distinct: Vec<Option<RadixSinkTable>>,
    /// Per aggregate: distinct tuples set aside under memory pressure, unpinned so that
    /// they can be spilled.
    abandoned: Vec<Option<PartitionedTupleData>>,
    /// The table size above which tuples are set aside under memory pressure.
    memory_limit: usize,
    /// The radix bits of tuples set aside (an out-of-core aggregation's partitions).
    external_bits: usize,
    arguments: Vec<ExpressionExecutor>,
    filters: Vec<Option<ExpressionExecutor>>,
    selection: Vec<u32>,
}

fn poisoned() -> Error {
    Error::internal("aggregate state lock poisoned")
}

impl Sink for PhysicalUngroupedAggregate {
    type Global = UngroupedGlobal;
    type Local = UngroupedLocal;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<Self::Global> {
        let external_bits = external_radix_bits(ctx, initial_radix_bits(ctx));
        let distinct_configs = self
            .aggregates
            .iter()
            .map(|aggregate| {
                if !aggregate.is_distinct() {
                    return Ok(None);
                }
                let row_width = Self::distinct_table(ctx, aggregate, initial_capacity(), 0)?
                    .layout()
                    .row_width();
                Ok(Some(RadixSinkConfig::new(ctx, row_width, external_bits)))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(UngroupedGlobal {
            states: Mutex::new(AggregateStates::new(&self.aggregates)),
            distinct: self
                .aggregates
                .iter()
                .map(|aggregate| aggregate.is_distinct().then(Mutex::default))
                .collect(),
            distinct_configs,
            distinct_partitions: Vec::new(),
            distinct_scheduled: AtomicBool::new(false),
        })
    }

    fn local_sink_state(&self, ctx: &ExecutionContext, global: &Self::Global) -> Result<UngroupedLocal> {
        // Radix partitioned so that threads can deduplicate partitions independently.
        let radix_bits = initial_radix_bits(ctx);
        let distinct = self
            .aggregates
            .iter()
            .zip(&global.distinct_configs)
            .map(|(aggregate, config)| {
                config
                    .as_ref()
                    .map(|config| {
                        let bits = config.radix_bits.load(Ordering::Acquire);
                        let table = Self::distinct_table(ctx, aggregate, config.sink_capacity, bits)?;
                        Ok(RadixSinkTable::new(config, table))
                    })
                    .transpose()
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(UngroupedLocal {
            states: AggregateStates::new(&self.aggregates),
            abandoned: self.aggregates.iter().map(|_| None).collect(),
            memory_limit: thread_memory_limit(ctx),
            external_bits: external_radix_bits(ctx, radix_bits),
            distinct,
            arguments: self
                .aggregates
                .iter()
                .map(|a| ExpressionExecutor::new(a.children.clone()))
                .collect(),
            filters: self
                .aggregates
                .iter()
                .map(|a| {
                    a.filter
                        .as_ref()
                        .map(|f| ExpressionExecutor::new(vec![f.clone()]))
                })
                .collect(),
            selection: Vec::new(),
        })
    }

    fn sink(
        &self,
        ctx: &ExecutionContext,
        chunk: &DataChunk,
        global: &Self::Global,
        local: &mut UngroupedLocal,
    ) -> Result<SinkResult> {
        for (index, aggregate) in self.aggregates.iter().enumerate() {
            let mut input = chunk.clone();
            if let Some(filter) = &mut local.filters[index] {
                let passed = filter.select(chunk, &mut local.selection)?;
                let sel = rustdb_types::SelectionVector::new(std::mem::take(&mut local.selection));
                input = DataChunk::new(chunk.columns().iter().map(|c| c.slice(&sel)).collect(), passed);
                if passed == 0 {
                    // No row passed the FILTER: the state stays as it is (constant
                    // arguments must not be counted for zero rows).
                    continue;
                }
            }
            let mut arguments = DataChunk::default();
            local.arguments[index].execute(&input, &mut arguments)?;
            if let Some(distinct) = &mut local.distinct[index] {
                let arguments = DataChunk::new(arguments.columns().to_vec(), input.len());
                add_distinct(&mut distinct.table, &arguments)?;
                if let Some(config) = &global.distinct_configs[index] {
                    distinct.after_add_chunk(config)?;
                }
                if distinct.table.size_in_bytes() > local.memory_limit && memory_pressure(ctx) {
                    abandon_table(&mut distinct.table, &mut local.abandoned[index], local.external_bits)?;
                }
                continue;
            }
            let data = AggregateInput {
                bind_data: aggregate.bind_info.as_deref().map(|d| d as _),
            };
            // SAFETY: the local state was initialised for this aggregate and is only used by
            // this thread.
            unsafe {
                aggregate.function.ops.simple_update(
                    arguments.columns(),
                    data,
                    local.states.state(index),
                    input.len(),
                )?;
            }
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, global: &Self::Global, mut local: UngroupedLocal) -> Result<()> {
        for ((table, abandoned), collected) in local
            .distinct
            .iter_mut()
            .zip(&mut local.abandoned)
            .zip(&global.distinct)
        {
            if let (Some(table), Some(collected)) = (table, collected) {
                let abandoned = abandoned.take();
                let distinct = abandoned.is_none() && table.table.is_deduplicated();
                let data = table_data(&mut table.table, abandoned)?;
                collected.lock().map_err(|_| poisoned())?.push((data, distinct));
            }
        }
        let global = global.states.lock().map_err(|_| poisoned())?;
        for (index, aggregate) in self.aggregates.iter().enumerate() {
            let data = AggregateInput {
                bind_data: aggregate.bind_info.as_deref().map(|d| d as _),
            };
            // SAFETY: both states are initialised; the global one is protected by the lock.
            unsafe {
                aggregate
                    .function
                    .ops
                    .combine(&[local.states.state(index)], &[global.state(index)], data)?;
            }
        }
        Ok(())
    }

    /// Splits the threads' distinct tuples into one unit per DISTINCT aggregate and radix
    /// partition, for the finalize tasks.
    fn finalize(&self, _: &ExecutionContext, global: &mut Self::Global) -> Result<SinkFinalize> {
        for (aggregate, collected) in global.distinct.iter_mut().enumerate() {
            let Some(collected) = collected else { continue };
            let parts = std::mem::take(collected.get_mut().map_err(|_| poisoned())?);
            // A single thread's tuples (none set aside) are distinct already.
            let mut filled = parts.iter().filter(|(part, _)| part.count() > 0);
            let unique = matches!((filled.next(), filled.next()), (Some((_, true)), None));
            let parts: Vec<PartitionedTupleData> = parts.into_iter().map(|(part, _)| part).collect();
            // Threads that set tuples aside have more partitions than the others.
            let radix_bits = parts
                .iter()
                .map(PartitionedTupleData::radix_bits)
                .max()
                .unwrap_or(0);
            let parts = parts
                .into_iter()
                .map(|part| repartitioned(part, radix_bits))
                .collect::<Result<Vec<_>>>()?;
            let partition_count = parts
                .iter()
                .map(PartitionedTupleData::partition_count)
                .max()
                .unwrap_or(0);
            let mut partitions: Vec<Vec<TupleDataCollection>> =
                (0..partition_count).map(|_| Vec::new()).collect();
            for mut part in parts {
                for (partition, collection) in part.take_partitions().into_iter().enumerate() {
                    if collection.count() > 0 {
                        partitions[partition].push(collection);
                    }
                }
            }
            global
                .distinct_partitions
                .extend(
                    partitions
                        .into_iter()
                        .filter(|data| !data.is_empty())
                        .map(|data| DistinctPartition {
                            aggregate,
                            data: Mutex::new(data),
                            unique,
                        }),
                );
        }
        Ok(SinkFinalize::Ready)
    }

    fn finalize_tasks(&self, _: &ExecutionContext, global: &Self::Global) -> Result<usize> {
        Ok(if global.distinct_scheduled.swap(true, Ordering::AcqRel) {
            0
        } else {
            global.distinct_partitions.len()
        })
    }

    /// Deduplicates one partition of a DISTINCT aggregate's tuples, aggregates them into a
    /// fresh state and combines that into the global state.
    fn execute_finalize_task(
        &self,
        ctx: &ExecutionContext,
        global: &Self::Global,
        task: usize,
    ) -> Result<()> {
        let partition = global
            .distinct_partitions
            .get(task)
            .ok_or_else(|| Error::internal("ungrouped aggregate finalize task out of range"))?;
        let index = partition.aggregate;
        let aggregate = &self.aggregates[index];
        let data = std::mem::take(&mut *partition.data.lock().map_err(|_| poisoned())?);
        let states = AggregateStates::new(&self.aggregates);
        let input = || AggregateInput {
            bind_data: aggregate.bind_info.as_deref().map(|d| d as _),
        };
        let mut update = |tuples: DataChunk| {
            // SAFETY: the state is initialised and owned by this task.
            unsafe {
                aggregate.function.ops.simple_update(
                    tuples.columns(),
                    input(),
                    states.state(index),
                    tuples.len(),
                )
            }
        };
        if partition.unique {
            for collection in &data {
                let tuple_columns = collection.layout().column_count() - 1;
                // SAFETY: distinct tables hold no aggregate states.
                unsafe {
                    GroupedAggregateHashTable::finalize_collection(
                        collection.layout(),
                        tuple_columns,
                        collection,
                        &mut update,
                    )?;
                }
            }
        } else {
            let mut table = Self::distinct_table(ctx, aggregate, initial_capacity(), 0)?;
            for collection in &data {
                table.combine(collection)?;
            }
            table.finalize_into(&mut update)?;
        }
        drop(data);
        let global_states = global.states.lock().map_err(|_| poisoned())?;
        // SAFETY: both states are initialised; the global one is protected by the lock.
        unsafe {
            aggregate
                .function
                .ops
                .combine(&[states.state(index)], &[global_states.state(index)], input())
        }
    }
}

impl PhysicalUngroupedAggregate {
    /// An empty table of the distinct argument tuples of `aggregate`.
    fn distinct_table(
        ctx: &ExecutionContext,
        aggregate: &BoundAggregate,
        capacity: usize,
        radix_bits: usize,
    ) -> Result<GroupedAggregateHashTable> {
        let types = aggregate.children.iter().map(|c| c.return_type.clone()).collect();
        GroupedAggregateHashTable::new(ctx.env.buffer_manager.clone(), types, &[], capacity, radix_bits)
    }
}

/// Adds the argument tuples of one chunk to a distinct table.
fn add_distinct(distinct: &mut GroupedAggregateHashTable, arguments: &DataChunk) -> Result<()> {
    if arguments.is_empty() {
        return Ok(());
    }
    let hashes = if arguments.column_count() == 0 {
        Vector::constant(&Value::ubigint(0))?
    } else {
        let columns: Vec<&Vector> = arguments.columns().iter().collect();
        hash_columns(&columns, arguments.len())?
    };
    distinct.add_chunk(arguments, &hashes, &[], &[])?;
    Ok(())
}

impl Source for PhysicalUngroupedAggregate {
    /// The finalized sink state and whether the result row was emitted.
    type Global = (SinkState, AtomicBool);
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        let sink =
            sink.ok_or_else(|| Error::internal("ungrouped aggregate source requires its sink state"))?;
        Ok((Arc::clone(sink), AtomicBool::new(false)))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        (sink, emitted): &Self::Global,
        (): &mut (),
    ) -> Result<SourceResult> {
        if !emitted.swap(true, Ordering::Relaxed) {
            let states = sink
                .downcast_ref::<UngroupedGlobal>()
                .ok_or_else(|| Error::internal("ungrouped aggregate sink state has the wrong type"))?;
            finalize_ungrouped(self, &states.states, chunk)?;
        }
        Ok(SourceResult::Finished)
    }
}

/// Emits the single result row of an ungrouped aggregate.
fn finalize_ungrouped(
    op: &PhysicalUngroupedAggregate,
    states: &Mutex<AggregateStates>,
    chunk: &mut DataChunk,
) -> Result<()> {
    let states = states.lock().map_err(|_| poisoned())?;
    let mut columns = Vec::with_capacity(op.aggregates.len());
    for (index, aggregate) in op.aggregates.iter().enumerate() {
        let mut result = Vector::flat(aggregate.function.return_type.clone(), 1);
        let data = AggregateInput {
            bind_data: aggregate.bind_info.as_deref().map(|d| d as _),
        };
        // SAFETY: the global state is initialised and protected by the lock.
        unsafe {
            aggregate
                .function
                .ops
                .finalize(&[states.state(index)], data, &mut result, 0)?;
        };
        columns.push(result);
    }
    chunk.set_columns(columns, 1);
    Ok(())
}
