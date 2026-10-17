use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::operator::{aggregate_names, expression_names, set_estimated_cardinality};
use rustdb_common::tree_renderer::RenderParams;
use rustdb_common::{Error, Result};
use rustdb_planner::Expression;
use rustdb_planner::expression::BoundAggregateExpression;
use rustdb_planner::operator::GroupingSet;
use rustdb_row::{
    PartitionedTupleData, RadixPartitioning, TupleDataCollection, ValidityType, hash::hash_columns,
};
use rustdb_storage::statistics::BaseStatistics;
use rustdb_types::{DataChunk, LogicalType, STANDARD_VECTOR_SIZE, Value, Vector};

use crate::aggregate_hashtable::{
    AggregateObject, GroupedAggregateHashTable, capacity_for_count, initial_capacity, resize_threshold,
};
use crate::operator::{
    ErasedSink, ErasedSource, OrderPreservationType, PhysicalOperator, PhysicalRef, SinkState,
};
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkResult, Source, SourceResult};

/// `GROUP BY` aggregation (DuckDB's `PhysicalHashAggregate` with one radix-partitioned hash
/// table per grouping set).
///
/// Each thread aggregates into local hash tables; `combine` hands their partitioned rows
/// to the global state; `finalize` merges each partition across threads into a final
/// table and materialises its groups.
#[derive(Debug)]
pub struct PhysicalHashAggregate {
    types: Vec<LogicalType>,
    groups: Vec<Expression>,
    aggregates: Vec<BoundAggregateExpression>,
    grouping_sets: Vec<GroupingSet>,
    grouping_functions: Vec<Vec<u64>>,
    /// Whether the groups can be NULL; without NULLs the rows store no validity for them
    /// (DuckDB's `group_validity`).
    group_validity: ValidityType,
    children: [PhysicalRef; 1],
}

impl PhysicalHashAggregate {
    pub fn new(
        types: Vec<LogicalType>,
        groups: Vec<Expression>,
        aggregates: Vec<BoundAggregateExpression>,
        mut grouping_sets: Vec<GroupingSet>,
        grouping_functions: Vec<Vec<u64>>,
        child: PhysicalRef,
    ) -> Result<Self> {
        if aggregates.iter().any(|a| a.order_bys.is_some()) {
            return Err(Error::not_implemented(
                "ORDER BY inside an aggregate with GROUP BY",
            ));
        }
        if grouping_sets.is_empty() {
            grouping_sets.push((0..groups.len() as u64).collect());
        }
        Ok(Self {
            types,
            groups,
            aggregates,
            grouping_sets,
            grouping_functions,
            group_validity: ValidityType::CanHaveNull,
            children: [child],
        })
    }

    /// Stores the groups without validity when their statistics show they cannot be NULL
    /// (`PhysicalPlanGenerator::CreatePlan(LogicalAggregate &)`).
    #[must_use]
    pub fn with_group_stats(mut self, group_stats: &[Option<BaseStatistics>]) -> Self {
        let no_nulls = group_stats.len() == self.groups.len()
            && group_stats
                .iter()
                .all(|stats| stats.as_ref().is_some_and(|stats| !stats.can_have_null()));
        if no_nulls {
            self.group_validity = ValidityType::CannotHaveNull;
        }
        self
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

    /// Radix bits of the thread-local tables, so threads can merge partitions
    /// independently.
    fn radix_bits(ctx: &ExecutionContext) -> usize {
        RadixPartitioning::radix_bits(ctx.env.threads().next_power_of_two()).min(4)
    }

    /// A table for grouping set `set` with every aggregate's state.
    fn new_table(
        &self,
        ctx: &ExecutionContext,
        set: &GroupingSet,
        capacity: usize,
        radix_bits: usize,
    ) -> Result<GroupedAggregateHashTable> {
        GroupedAggregateHashTable::with_validity(
            ctx.env.buffer_manager.clone(),
            self.set_group_types(set),
            &self.aggregate_objects(),
            capacity,
            radix_bits,
            self.group_validity,
        )
    }

    fn set_group_types(&self, set: &GroupingSet) -> Vec<LogicalType> {
        set.iter()
            .map(|&group| self.groups[group as usize].return_type.clone())
            .collect()
    }

    /// `GROUPING(...)` values for a grouping set: bit `j` (from the left) is set when the
    /// function's `j`-th group is not part of the set.
    fn grouping_values(&self, set: &GroupingSet) -> Vec<i64> {
        self.grouping_functions
            .iter()
            .map(|groups| {
                groups.iter().enumerate().fold(0i64, |value, (j, group)| {
                    if set.contains(group) {
                        value
                    } else {
                        value | (1 << (groups.len() - 1 - j))
                    }
                })
            })
            .collect()
    }
}

impl PhysicalOperator for PhysicalHashAggregate {
    fn name(&self) -> &'static str {
        "HASH_GROUP_BY"
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

    fn params_to_string(&self, estimated_cardinality: u64) -> RenderParams {
        let mut result = RenderParams::new();
        result.insert("Groups".to_owned(), expression_names(&self.groups));
        result.insert("Aggregates".to_owned(), aggregate_names(&self.aggregates));
        set_estimated_cardinality(&mut result, estimated_cardinality);
        result
    }
}

/// Global state: the partitioned rows each thread produced, per grouping set, and after
/// finalisation the result chunks.
#[derive(Debug, Default)]
pub struct HashAggregateGlobal {
    collected: Mutex<Vec<Vec<PartitionedTupleData>>>,
    /// Per grouping set, per aggregate: the threads' distinct (groups, arguments) tuples
    /// of each `DISTINCT` aggregate (DuckDB's `DistinctAggregateData` radix tables).
    distinct_collected: Mutex<Vec<Vec<Vec<PartitionedTupleData>>>>,
    /// Whether any aggregate is `DISTINCT`.
    has_distinct: bool,
    /// After finalisation: one unit per `DISTINCT` aggregate and radix partition, whose
    /// tuples a finalize task deduplicates and feeds into the groups (DuckDB's
    /// `HashAggregateDistinctFinalizeTask`).
    distinct_units: Vec<DistinctUnit>,
    /// Whether the distinct units were handed out as finalize tasks.
    distinct_scheduled: AtomicBool,
    /// Whether any input row arrived (for empty grouping sets).
    saw_input: std::sync::atomic::AtomicBool,
    /// The radix bits every thread's data ends up with; raised to
    /// `external_radix_bits` once a thread runs out of its memory share (DuckDB's
    /// `RadixHTConfig::SetRadixBitsToExternal`).
    radix_bits: AtomicUsize,
    /// The radix bits of an out-of-core aggregation.
    external_radix_bits: usize,
    /// The most radix bits the sink grows to while in memory (DuckDB's
    /// `maximum_sink_radix_bits`).
    maximum_sink_radix_bits: usize,
    /// Pointer table capacity of the threads' tables (`RadixHTConfig::SinkCapacity`).
    sink_capacity: usize,
    /// Number of threads (`number_of_threads`): up to `GROW_STRATEGY_THREAD_THRESHOLD`
    /// the threads' tables grow, beyond it they start over when full.
    threads: usize,
    /// Size of a block, which partitions should fill before more are added.
    block_alloc_size: usize,
    /// Whether the aggregation went out of core (`external`).
    external: AtomicBool,
    /// Whether a thread has combined, after which the radix bits stay (`any_combined`).
    any_combined: AtomicBool,
    /// After finalisation: the groups still to combine and emit, one unit per grouping
    /// set and radix partition (DuckDB's `RadixHTGlobalSourceState` partitions), and the
    /// row an empty grouping set produces without input.
    finalized: OnceLock<FinalizedGroups>,
}

/// The finalized sink's output units.
#[derive(Debug, Default)]
struct FinalizedGroups {
    partitions: Vec<AggregatePartition>,
    results: Vec<DataChunk>,
}

impl HashAggregateGlobal {
    fn partitions(&self) -> &[AggregatePartition] {
        self.finalized
            .get()
            .map_or(&[], |finalized| &finalized.partitions)
    }

    fn results(&self) -> &[DataChunk] {
        self.finalized.get().map_or(&[], |finalized| &finalized.results)
    }
}

/// One radix partition of one `DISTINCT` aggregate's tuples, from every thread.
#[derive(Debug)]
struct DistinctUnit {
    set_index: usize,
    aggregate: usize,
    data: Mutex<Vec<TupleDataCollection>>,
}

/// The rows of one radix partition of one grouping set, from every thread, which a
/// source task merges and emits.
#[derive(Debug)]
struct AggregatePartition {
    set_index: usize,
    data: Mutex<Vec<TupleDataCollection>>,
    /// Bytes of the partition's rows.
    size: usize,
    /// Whether the rows' groups are already distinct, so that the partition is emitted
    /// without combining (DuckDB's `single_ht`).
    combined: bool,
}

/// Per-thread state: one hash table per grouping set plus expression executors.
#[derive(Debug)]
pub struct HashAggregateLocal {
    tables: Vec<GroupedAggregateHashTable>,
    /// Per grouping set, per `DISTINCT` aggregate: the thread's distinct tuples, handed
    /// to the global state in `combine`.
    distinct: Vec<Vec<Option<GroupedAggregateHashTable>>>,
    /// Per grouping set, per `DISTINCT` aggregate: distinct tuples set aside under memory
    /// pressure, unpinned so that they can be spilled.
    distinct_abandoned: Vec<Vec<Option<PartitionedTupleData>>>,
    /// Per grouping set: groups taken out of the tables when they outgrew the thread's
    /// memory share, in the external partitions and unpinned so that they can be spilled
    /// (DuckDB's `abandoned_data`).
    abandoned: Vec<Option<PartitionedTupleData>>,
    /// The size above which the thread's tables are set aside under memory pressure: a
    /// quarter of its share of the memory limit (DuckDB's `GetMaxThreadMemory`).
    memory_limit: usize,
    /// Per grouping set: the pointer table capacity of the thread's table
    /// (`local_sink_capacity`).
    sink_capacity: Vec<usize>,
    /// Per grouping set: whether the table's strategy was adapted to the data
    /// (`adapted`).
    adapted: Vec<bool>,
    groups: ExpressionExecutor,
    arguments: Vec<ExpressionExecutor>,
    filters: Vec<Option<ExpressionExecutor>>,
}

impl Sink for PhysicalHashAggregate {
    type Global = HashAggregateGlobal;
    type Local = HashAggregateLocal;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<HashAggregateGlobal> {
        let threads = ctx.env.threads().max(1);
        let initial_bits = Self::radix_bits(ctx);
        // The widest row decides how many partitions the threads can fill without too
        // many cache misses (`MaximumSinkRadixBits`).
        let row_width = self
            .grouping_sets
            .iter()
            .map(|set| {
                self.new_table(ctx, set, initial_capacity(), 0)
                    .map(|table| table.layout().row_width())
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .max()
            .unwrap_or(0);
        let external_bits = external_radix_bits(ctx, initial_bits);
        Ok(HashAggregateGlobal {
            collected: Mutex::new(self.grouping_sets.iter().map(|_| Vec::new()).collect()),
            distinct_collected: Mutex::new(
                self.grouping_sets
                    .iter()
                    .map(|_| self.aggregates.iter().map(|_| Vec::new()).collect())
                    .collect(),
            ),
            has_distinct: self.aggregates.iter().any(BoundAggregateExpression::is_distinct),
            radix_bits: AtomicUsize::new(initial_bits),
            external_radix_bits: external_bits,
            maximum_sink_radix_bits: maximum_sink_radix_bits(threads, row_width, initial_bits)
                .min(external_bits),
            sink_capacity: sink_capacity(threads),
            threads,
            block_alloc_size: ctx.env.buffer_manager.block_alloc_size(),
            ..HashAggregateGlobal::default()
        })
    }

    fn local_sink_state(
        &self,
        ctx: &ExecutionContext,
        global: &HashAggregateGlobal,
    ) -> Result<HashAggregateLocal> {
        let radix_bits = global.radix_bits.load(Ordering::Acquire);
        let threads = ctx.env.threads().max(1) as u64;
        let tables = self
            .grouping_sets
            .iter()
            .map(|set| self.new_table(ctx, set, global.sink_capacity, radix_bits))
            .collect::<Result<Vec<_>>>()?;
        // Beyond the grow-strategy thread count, the tables count distinct groups so that
        // they can adapt their strategy later.
        let grow = global.threads <= GROW_STRATEGY_THREAD_THRESHOLD;
        let mut tables = tables;
        for table in &mut tables {
            table.enable_hll(!grow);
        }
        Ok(HashAggregateLocal {
            sink_capacity: vec![global.sink_capacity; tables.len()],
            adapted: vec![grow; tables.len()],
            distinct: self.distinct_tables(ctx)?,
            distinct_abandoned: self
                .grouping_sets
                .iter()
                .map(|_| self.aggregates.iter().map(|_| None).collect())
                .collect(),
            abandoned: self.grouping_sets.iter().map(|_| None).collect(),
            memory_limit: usize::try_from(ctx.env.buffer_manager.max_memory() / threads / 4)
                .unwrap_or(usize::MAX),
            tables,
            groups: ExpressionExecutor::new(self.groups.clone()),
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
        })
    }

    fn sink(
        &self,
        ctx: &ExecutionContext,
        chunk: &DataChunk,
        global: &HashAggregateGlobal,
        local: &mut HashAggregateLocal,
    ) -> Result<SinkResult> {
        if chunk.is_empty() {
            return Ok(SinkResult::NeedMoreInput);
        }
        global.saw_input.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut group_values = DataChunk::default();
        local.groups.execute(chunk, &mut group_values)?;
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
        for (set_index, (set, table)) in self.grouping_sets.iter().zip(&mut local.tables).enumerate() {
            let columns: Vec<Vector> = set
                .iter()
                .map(|&group| group_values.column(group as usize).clone())
                .collect();
            let groups = DataChunk::new(columns, chunk.len());
            let mut selections = selections.clone();
            for (index, distinct) in local.distinct[set_index].iter_mut().enumerate() {
                let Some(distinct) = distinct else { continue };
                // The aggregate's rows go to its distinct table; the main table only
                // creates their groups.
                let mut tuples: Vec<Vector> = groups.columns().to_vec();
                tuples.extend(inputs[index].iter().cloned());
                let tuples = match &selections[index] {
                    Some(selection) => {
                        let sel = rustdb_types::SelectionVector::new(selection.clone());
                        DataChunk::new(tuples.iter().map(|c| c.slice(&sel)).collect(), selection.len())
                    }
                    None => DataChunk::new(tuples, chunk.len()),
                };
                add_tuples(distinct, &tuples, &[], &[])?;
                selections[index] = Some(Vec::new());
            }
            let hashes = if groups.column_count() == 0 {
                Vector::constant(&Value::ubigint(0))?
            } else {
                let refs: Vec<&Vector> = groups.columns().iter().collect();
                hash_columns(&refs, chunk.len())?
            };
            table.add_chunk(&groups, &hashes, &inputs, &selections)?;
            Self::after_add_chunk(
                global,
                table,
                &mut local.sink_capacity[set_index],
                &mut local.adapted[set_index],
            )?;
        }
        // Under memory pressure, a thread whose tables hold more than a quarter of its
        // share of the memory limit sets their groups aside so that they can be spilled.
        let size: usize = local
            .tables
            .iter()
            .chain(local.distinct.iter().flatten().flatten())
            .map(GroupedAggregateHashTable::size_in_bytes)
            .sum();
        if size > local.memory_limit && memory_pressure(ctx) {
            Self::go_external(global, local)?;
            for (tables, abandoned) in local.distinct.iter_mut().zip(&mut local.distinct_abandoned) {
                for (table, abandoned) in tables.iter_mut().zip(abandoned) {
                    if let Some(table) = table {
                        abandon_table(table, abandoned, global.external_radix_bits)?;
                    }
                }
            }
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(
        &self,
        _: &ExecutionContext,
        global: &HashAggregateGlobal,
        mut local: HashAggregateLocal,
    ) -> Result<()> {
        global.any_combined.store(true, Ordering::Release);
        if global.has_distinct {
            let mut distinct_parts = Vec::new();
            for (set_index, (local_set, abandoned_set)) in local
                .distinct
                .iter_mut()
                .zip(&mut local.distinct_abandoned)
                .enumerate()
            {
                for (index, (local_table, abandoned)) in local_set.iter_mut().zip(abandoned_set).enumerate() {
                    if let Some(local_table) = local_table {
                        let data = table_data(local_table, abandoned.take())?;
                        if data.count() > 0 {
                            distinct_parts.push((set_index, index, data));
                        }
                    }
                }
            }
            let mut collected = global
                .distinct_collected
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for (set_index, index, data) in distinct_parts {
                collected[set_index][index].push(data);
            }
        }
        let mut parts = Vec::with_capacity(local.tables.len());
        for (table, abandoned) in local.tables.iter_mut().zip(&mut local.abandoned) {
            let mut data = table.take_partitioned_data()?;
            if let Some(mut abandoned) = abandoned.take() {
                data.repartition(&mut abandoned)?;
                data = abandoned;
            }
            data.unpin();
            parts.push(data);
        }
        let mut collected = global.collected.lock().unwrap_or_else(PoisonError::into_inner);
        for (set, data) in parts.into_iter().enumerate() {
            collected[set].push(data);
        }
        Ok(())
    }

    /// Merges the threads' partitions per grouping set; the source combines and emits
    /// them partition by partition, in parallel (DuckDB's `RadixPartitionedHashTable`
    /// finalizes partitions in its source tasks).
    fn finalize(
        &self,
        ctx: &ExecutionContext,
        global: &mut HashAggregateGlobal,
    ) -> Result<crate::SinkFinalize> {
        if global.has_distinct {
            // The groups are complete only after the distinct tuples were fed in by the
            // finalize tasks.
            global.distinct_units = Self::distinct_units(global)?;
            return Ok(crate::SinkFinalize::Ready);
        }
        self.build_partitions(ctx, global)?;
        Ok(crate::SinkFinalize::Ready)
    }

    /// One stage of `DISTINCT` tasks (DuckDB's `HashAggregateDistinctFinalizeEvent`), after
    /// which the groups' partitions are built.
    fn finalize_tasks(&self, ctx: &ExecutionContext, global: &HashAggregateGlobal) -> Result<usize> {
        if !global.has_distinct || global.finalized.get().is_some() {
            return Ok(0);
        }
        if !global.distinct_scheduled.swap(true, Ordering::AcqRel) && !global.distinct_units.is_empty() {
            return Ok(global.distinct_units.len());
        }
        self.build_partitions(ctx, global)?;
        Ok(0)
    }

    /// Deduplicates one partition of one `DISTINCT` aggregate's tuples and adds them to a
    /// fresh table of the groups, whose rows join the threads' (DuckDB's
    /// `HashAggregateDistinctFinalizeTask`).
    fn execute_finalize_task(
        &self,
        ctx: &ExecutionContext,
        global: &HashAggregateGlobal,
        task: usize,
    ) -> Result<()> {
        let unit = global
            .distinct_units
            .get(task)
            .ok_or_else(|| Error::internal("hash aggregate distinct task out of range"))?;
        let data = std::mem::take(&mut *unit.data.lock().unwrap_or_else(PoisonError::into_inner));
        let Some(first) = data.first() else {
            return Ok(());
        };
        let set = &self.grouping_sets[unit.set_index];
        let rows: usize = data.iter().map(TupleDataCollection::count).sum();
        let mut distinct = GroupedAggregateHashTable::new(
            ctx.env.buffer_manager.clone(),
            first.layout().types()[..first.layout().column_count() - 1].to_vec(),
            &[],
            capacity_for_count(rows),
            0,
        )?;
        for collection in &data {
            distinct.combine(collection)?;
        }
        drop(data);
        let mut table = self.new_table(
            ctx,
            set,
            initial_capacity(),
            global.radix_bits.load(Ordering::Acquire),
        )?;
        let group_count = set.len();
        let selections: Vec<Option<Vec<u32>>> = (0..self.aggregates.len())
            .map(|other| {
                if other == unit.aggregate {
                    None
                } else {
                    Some(Vec::new())
                }
            })
            .collect();
        distinct.finalize_into(|tuples| {
            let count = tuples.len();
            let mut columns = tuples.into_columns();
            let arguments = columns.split_off(group_count);
            let mut inputs = vec![Vec::new(); self.aggregates.len()];
            inputs[unit.aggregate] = arguments;
            add_tuples(&mut table, &DataChunk::new(columns, count), &inputs, &selections)
        })?;
        let data = table.take_partitioned_data()?;
        data.unpin();
        global.collected.lock().unwrap_or_else(PoisonError::into_inner)[unit.set_index].push(data);
        Ok(())
    }
}

impl PhysicalHashAggregate {
    /// Splits every `DISTINCT` aggregate's tuples into their radix partitions, one unit
    /// of work each.
    fn distinct_units(global: &mut HashAggregateGlobal) -> Result<Vec<DistinctUnit>> {
        let collected = std::mem::take(
            global
                .distinct_collected
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner),
        );
        let mut units = Vec::new();
        for (set_index, aggregates) in collected.into_iter().enumerate() {
            for (aggregate, parts) in aggregates.into_iter().enumerate() {
                let radix_bits = parts
                    .iter()
                    .map(PartitionedTupleData::radix_bits)
                    .max()
                    .unwrap_or(0);
                let mut taken: Vec<Vec<TupleDataCollection>> =
                    (0..1usize << radix_bits).map(|_| Vec::new()).collect();
                for part in parts {
                    let mut part = repartitioned(part, radix_bits)?;
                    for (partition, collection) in part.take_partitions().into_iter().enumerate() {
                        if collection.count() > 0 {
                            taken[partition].push(collection);
                        }
                    }
                }
                units.extend(
                    taken
                        .into_iter()
                        .filter(|data| !data.is_empty())
                        .map(|data| DistinctUnit {
                            set_index,
                            aggregate,
                            data: Mutex::new(data),
                        }),
                );
            }
        }
        Ok(units)
    }

    /// Merges the threads' partitions per grouping set into the units the source
    /// combines and emits.
    fn build_partitions(&self, ctx: &ExecutionContext, global: &HashAggregateGlobal) -> Result<()> {
        let collected = std::mem::take(&mut *global.collected.lock().unwrap_or_else(PoisonError::into_inner));
        let mut finalized = FinalizedGroups::default();
        let objects = self.aggregate_objects();
        let radix_bits = global.radix_bits.load(Ordering::Acquire);
        for (set_index, (set, parts)) in self.grouping_sets.iter().zip(collected).enumerate() {
            // Threads that combined before the sink went external have fewer partitions.
            let mut parts = parts
                .into_iter()
                .map(|part| repartitioned(part, radix_bits))
                .collect::<Result<Vec<_>>>()?;
            let partition_count = parts
                .iter()
                .map(PartitionedTupleData::partition_count)
                .max()
                .unwrap_or(1);
            let rows: usize = parts.iter().map(PartitionedTupleData::count).sum();
            // Everything was aggregated by a single thread in a single table: its groups
            // are distinct already.
            let single_table = !global.external.load(Ordering::Acquire)
                && global.threads == 1
                && parts.iter().filter(|part| part.count() > 0).count() == 1;
            let mut taken: Vec<Vec<TupleDataCollection>> = (0..partition_count).map(|_| Vec::new()).collect();
            for part in &mut parts {
                for (partition, collection) in part.take_partitions().into_iter().enumerate() {
                    if collection.count() > 0 {
                        taken[partition].push(collection);
                    }
                }
            }
            finalized
                .partitions
                .extend(
                    taken
                        .into_iter()
                        .filter(|data| !data.is_empty())
                        .map(|data| AggregatePartition {
                            set_index,
                            size: data.iter().map(TupleDataCollection::size_in_bytes).sum(),
                            data: Mutex::new(data),
                            combined: single_table,
                        }),
                );
            // An empty grouping set yields one row even without input (like an ungrouped
            // aggregate).
            if set.is_empty() && rows == 0 {
                let mut table = GroupedAggregateHashTable::new(
                    ctx.env.buffer_manager.clone(),
                    Vec::new(),
                    &objects,
                    initial_capacity(),
                    0,
                )?;
                let empty_inputs: Vec<Vec<Vector>> = Vec::new();
                table.add_chunk(
                    &DataChunk::new(Vec::new(), 1),
                    &Vector::constant(&Value::ubigint(0))?,
                    &empty_inputs,
                    &[],
                )?;
                table.finalize_into(|chunk| {
                    finalized.results.push(self.output_chunk(set_index, set, chunk)?);
                    Ok(())
                })?;
            }
        }
        global
            .finalized
            .set(finalized)
            .map_err(|_| Error::internal("hash aggregate finalized twice"))?;
        Ok(())
    }
}

/// The most radix bits of an out-of-core aggregation (DuckDB's
/// `MAXIMUM_FINAL_SINK_RADIX_BITS` is 8; at 7 a partition's blocks still fill well).
const MAXIMUM_EXTERNAL_RADIX_BITS: usize = 7;

/// The radix bits of an out-of-core aggregation: as many partitions (so that each can be
/// finalized on its own) as the threads can append to at once within half the memory
/// limit, every partition pinning a row and a heap block while appending; at least
/// `minimum`.
pub(crate) fn external_radix_bits(ctx: &ExecutionContext, minimum: usize) -> usize {
    let buffer_manager = &ctx.env.buffer_manager;
    let per_thread = buffer_manager.max_memory() / 2 / ctx.env.threads().max(1) as u64;
    let partitions = per_thread / (2 * buffer_manager.block_alloc_size() as u64).max(1);
    let bits = (u64::BITS - 1).saturating_sub(partitions.max(1).leading_zeros()) as usize;
    bits.clamp(minimum, MAXIMUM_EXTERNAL_RADIX_BITS.max(minimum))
}

/// The share of the memory limit one thread's tables may hold before their rows are set
/// aside under memory pressure: a quarter of its share (DuckDB's `GetMaxThreadMemory`).
pub(crate) fn thread_memory_limit(ctx: &ExecutionContext) -> usize {
    let threads = ctx.env.threads().max(1) as u64;
    usize::try_from(ctx.env.buffer_manager.max_memory() / threads / 4).unwrap_or(usize::MAX)
}

/// Whether the buffer manager is under memory pressure (over three quarters used).
pub(crate) fn memory_pressure(ctx: &ExecutionContext) -> bool {
    let buffer_manager = &ctx.env.buffer_manager;
    buffer_manager.used_memory() > buffer_manager.max_memory() / 4 * 3
}

/// Moves the rows of `table` into `abandoned` (created with `external_bits` radix bits)
/// and unpins them so that they can be spilled; the table starts over. Duplicates across
/// the two are merged when the partitions are combined (DuckDB's `abandoned_data`).
pub(crate) fn abandon_table(
    table: &mut GroupedAggregateHashTable,
    abandoned: &mut Option<PartitionedTupleData>,
    external_bits: usize,
) -> Result<()> {
    let mut data = table.take_partitioned_data()?;
    let target = match abandoned {
        Some(target) => target,
        None => abandoned.insert(data.with_radix_bits(external_bits.max(data.radix_bits()))?),
    };
    data.repartition(target)?;
    target.unpin();
    Ok(())
}

/// The rows of `table` plus those set aside in `abandoned`, unpinned.
pub(crate) fn table_data(
    table: &mut GroupedAggregateHashTable,
    abandoned: Option<PartitionedTupleData>,
) -> Result<PartitionedTupleData> {
    let mut data = table.take_partitioned_data()?;
    if let Some(mut abandoned) = abandoned {
        data.repartition(&mut abandoned)?;
        data = abandoned;
    }
    data.unpin();
    Ok(data)
}

/// `data` with at least `radix_bits` radix bits, unpinned so it can be spilled.
pub(crate) fn repartitioned(
    mut data: PartitionedTupleData,
    radix_bits: usize,
) -> Result<PartitionedTupleData> {
    if data.radix_bits() < radix_bits {
        let mut target = data.with_radix_bits(radix_bits)?;
        data.repartition(&mut target)?;
        data = target;
    }
    data.unpin();
    Ok(data)
}

/// Up to this many threads, the threads' tables grow; beyond it they keep a small,
/// cache-resident pointer table and start over when it is full
/// (`RadixHTConfig::GROW_STRATEGY_THREAD_THRESHOLD`).
const GROW_STRATEGY_THREAD_THRESHOLD: usize = 2;
/// Rows after which a thread decides whether to adapt its table
/// (`RadixHTLocalSinkState::ADAPTIVITY_THRESHOLD`); also the largest sink capacity.
const ADAPTIVITY_THRESHOLD: usize = 1 << 20;
/// Partitions filled to this share of a block trigger a repartition (`BLOCK_FILL_FACTOR`).
const BLOCK_FILL_FACTOR: f64 = 0.5;
/// Radix bits added per repartition (`REPARTITION_RADIX_BITS`).
const REPARTITION_RADIX_BITS: usize = 2;
/// The most radix bits of the sink (`MAXIMUM_FINAL_SINK_RADIX_BITS`).
const MAXIMUM_FINAL_SINK_RADIX_BITS: usize = 8;

/// `RadixHTConfig::SinkCapacity`
fn sink_capacity(threads: usize) -> usize {
    if threads <= GROW_STRATEGY_THREAD_THRESHOLD {
        // Grow strategy: start off a bit bigger.
        262_144
    } else {
        // Start off tiny; `decide_adaptation` may grow it later.
        32_768
    }
}

/// `RadixHTConfig::MaximumSinkRadixBits`: wide rows get fewer partitions, otherwise
/// cache misses get out of hand.
fn maximum_sink_radix_bits(threads: usize, row_width: usize, initial_bits: usize) -> usize {
    if threads <= GROW_STRATEGY_THREAD_THRESHOLD {
        return initial_bits; // Don't repartition unless we go external.
    }
    let bits = if row_width >= 64 {
        MAXIMUM_FINAL_SINK_RADIX_BITS - 2
    } else if row_width >= 32 {
        MAXIMUM_FINAL_SINK_RADIX_BITS - 1
    } else {
        MAXIMUM_FINAL_SINK_RADIX_BITS
    };
    bits.max(initial_bits)
}

impl PhysicalHashAggregate {
    /// The thread-local strategy after each chunk (the rest of DuckDB's
    /// `RadixPartitionedHashTable::Sink`): adapt once enough rows were seen, and when the
    /// pointer table is full either let it grow (few threads) or start a new one, raising
    /// the radix bits as the partitions fill (`MaybeRepartition`).
    fn after_add_chunk(
        global: &HashAggregateGlobal,
        table: &mut GroupedAggregateHashTable,
        capacity: &mut usize,
        adapted: &mut bool,
    ) -> Result<()> {
        let external = global.external.load(Ordering::Acquire);
        if !*adapted && table.sink_count() >= ADAPTIVITY_THRESHOLD {
            if !external {
                Self::decide_adaptation(global, table, capacity)?;
            }
            table.enable_hll(false);
            *adapted = true;
        }
        if table.count() + STANDARD_VECTOR_SIZE < resize_threshold(*capacity) {
            return Ok(()); // Another chunk fits.
        }
        let grow = global.threads <= GROW_STRATEGY_THREAD_THRESHOLD;
        if !grow || external {
            // Start a new pointer table and keep appending to the same rows; this works
            // because the table never grows.
            table.abandon();
        }
        if grow {
            return Ok(()); // Repartitioning only happens when going external.
        }
        let current_bits = table.radix_bits();
        let partitions = 1usize << current_bits;
        let row_size_per_partition = table.materialized_count() * table.layout().row_width() / partitions;
        if row_size_per_partition as f64 > BLOCK_FILL_FACTOR * global.block_alloc_size as f64
            && !global.any_combined.load(Ordering::Acquire)
        {
            let bits = (current_bits + REPARTITION_RADIX_BITS).min(global.maximum_sink_radix_bits);
            global.radix_bits.fetch_max(bits, Ordering::AcqRel);
        }
        let global_bits = global.radix_bits.load(Ordering::Acquire);
        if current_bits < global_bits {
            table.repartition(global_bits)?;
            if external {
                table.resize_to(*capacity)?;
            }
        }
        Ok(())
    }

    /// Picks a better strategy from the distinct groups seen so far (`DecideAdaptation`):
    /// skip the lookups when nearly every row is a new group, or grow the pointer table
    /// when a bigger one would merge many more duplicates.
    fn decide_adaptation(
        global: &HashAggregateGlobal,
        table: &mut GroupedAggregateHashTable,
        capacity: &mut usize,
    ) -> Result<()> {
        const SKIP_LOOKUP_UNIQUE_PERCENTAGE_THRESHOLD: f64 = 0.95;
        const CAPACITY_INCREASE_DEDUPLICATION_RATE_THRESHOLD: f64 = 2.0;
        let sink_count = table.sink_count() as f64;
        let deduplicated_count = table.materialized_count();
        let hll_count = table.hll_upper_bound().min(deduplicated_count);
        let deduplicated_percentage = deduplicated_count as f64 / sink_count;
        let hll_percentage = hll_count as f64 / sink_count;
        if hll_percentage > SKIP_LOOKUP_UNIQUE_PERCENTAGE_THRESHOLD {
            // Almost everything is unique: just append, deduplicate when combining.
            table.skip_lookups();
            return Ok(());
        }
        if deduplicated_percentage / hll_percentage > CAPACITY_INCREASE_DEDUPLICATION_RATE_THRESHOLD {
            let new_capacity = capacity_for_count(hll_count).min(ADAPTIVITY_THRESHOLD);
            *capacity = global.sink_capacity.max(new_capacity);
            table.abandon();
            table.resize_to(*capacity)?;
        }
        Ok(())
    }

    /// The thread's tables outgrew its memory share: their groups are set aside (unpinned,
    /// so they can be spilled) and the tables start over, and the aggregation switches to
    /// enough partitions to finalize them one at a time (DuckDB's `MaybeRepartition`
    /// abandoning the hash table when going external).
    fn go_external(global: &HashAggregateGlobal, local: &mut HashAggregateLocal) -> Result<()> {
        global.external.store(true, Ordering::Release);
        global
            .radix_bits
            .fetch_max(global.external_radix_bits, Ordering::AcqRel);
        // The tables keep their few partitions (every partition pins a block while
        // appending); the data set aside goes to the external partitions while it is
        // still in memory.
        for (table, abandoned) in local.tables.iter_mut().zip(&mut local.abandoned) {
            let mut data = table.take_partitioned_data()?;
            let target = match abandoned {
                Some(target) => target,
                None => abandoned.insert(data.with_radix_bits(global.external_radix_bits)?),
            };
            data.repartition(target)?;
            target.unpin();
        }
        Ok(())
    }
}

impl PhysicalHashAggregate {
    /// Per grouping set, per aggregate: an empty table of the distinct (groups,
    /// arguments) tuples of each `DISTINCT` aggregate.
    fn distinct_tables(&self, ctx: &ExecutionContext) -> Result<Vec<Vec<Option<GroupedAggregateHashTable>>>> {
        self.grouping_sets
            .iter()
            .map(|set| {
                self.aggregates
                    .iter()
                    .map(|aggregate| {
                        if !aggregate.is_distinct() {
                            return Ok(None);
                        }
                        let mut types = self.set_group_types(set);
                        types.extend(aggregate.children.iter().map(|c| c.return_type.clone()));
                        GroupedAggregateHashTable::new(
                            ctx.env.buffer_manager.clone(),
                            types,
                            &[],
                            initial_capacity(),
                            Self::radix_bits(ctx),
                        )
                        .map(Some)
                    })
                    .collect()
            })
            .collect()
    }
}

/// Adds `tuples` to `table`, hashing all its columns as groups.
fn add_tuples(
    table: &mut GroupedAggregateHashTable,
    tuples: &DataChunk,
    inputs: &[Vec<Vector>],
    selections: &[Option<Vec<u32>>],
) -> Result<()> {
    if tuples.is_empty() {
        return Ok(());
    }
    let hashes = if tuples.column_count() == 0 {
        Vector::constant(&Value::ubigint(0))?
    } else {
        let refs: Vec<&Vector> = tuples.columns().iter().collect();
        hash_columns(&refs, tuples.len())?
    };
    table.add_chunk(tuples, &hashes, inputs, selections)?;
    Ok(())
}

impl PhysicalHashAggregate {
    /// Expands a finalized chunk of one grouping set (its groups, then the aggregates) to
    /// the operator's output: every group column (NULL when not in the set), the
    /// aggregates, then the `GROUPING()` values.
    fn output_chunk(&self, set_index: usize, set: &GroupingSet, chunk: DataChunk) -> Result<DataChunk> {
        let count = chunk.len();
        let mut columns = chunk.into_columns().into_iter();
        let mut set_columns: Vec<Option<Vector>> = vec![None; self.groups.len()];
        for &group in set {
            set_columns[group as usize] = columns.next();
        }
        let mut output = Vec::with_capacity(self.types.len());
        for (group, column) in set_columns.into_iter().enumerate() {
            output.push(match column {
                Some(column) => column,
                None => Vector::constant(&Value::null(self.groups[group].return_type.clone()))?,
            });
        }
        output.extend(columns);
        for value in self.grouping_values(&self.grouping_sets[set_index]) {
            output.push(Vector::constant(&Value::bigint(value))?);
        }
        Ok(DataChunk::new(output, count))
    }
}

/// The source's shared state: the finalized sink and the next unit (partition, then
/// empty-set row) to hand out.
#[derive(Debug)]
pub struct HashAggregateSourceState {
    sink: SinkState,
    next: AtomicUsize,
    /// The memory limit, which bounds how many partitions are finalized at once.
    max_memory: u64,
}

/// A source task's emitted partition: its finalized chunks, in order.
#[derive(Debug, Default)]
pub struct HashAggregateSourceLocal {
    pending: VecDeque<DataChunk>,
}

impl PhysicalHashAggregate {
    /// Merges partition `partition` of the threads' groups into one table and returns
    /// its rows in output layout.
    fn finalize_partition(
        &self,
        ctx: &ExecutionContext,
        partition: &AggregatePartition,
    ) -> Result<Vec<DataChunk>> {
        let data = std::mem::take(&mut *partition.data.lock().unwrap_or_else(PoisonError::into_inner));
        let set = &self.grouping_sets[partition.set_index];
        let mut chunks = Vec::new();
        if partition.combined {
            for collection in &data {
                // SAFETY: the partition's rows hold live states of their layout and are
                // finalized only here (the partition's data was taken out above).
                unsafe {
                    GroupedAggregateHashTable::finalize_collection(
                        collection.layout(),
                        set.len(),
                        collection,
                        |chunk| {
                            chunks.push(self.output_chunk(partition.set_index, set, chunk)?);
                            Ok(())
                        },
                    )?;
                }
            }
            return Ok(chunks);
        }
        // Room for every row of the partition, limited so that a thread does not
        // over-allocate (`RadixHTGlobalSourceState` creating the partition's table).
        let rows: usize = data.iter().map(TupleDataCollection::count).sum();
        let bytes: usize = data.iter().map(TupleDataCollection::size_in_bytes).sum();
        let threads = ctx.env.threads().max(1) as f64;
        let thread_limit = (0.6 * ctx.env.buffer_manager.max_memory() as f64 / threads) as usize;
        let size_per_entry = bytes / rows.max(1) + (1.5 * size_of::<u64>() as f64) as usize;
        let capacity_limit = (thread_limit / size_per_entry.max(1))
            .next_power_of_two()
            .max(initial_capacity());
        let mut table = self.new_table(ctx, set, capacity_for_count(rows).min(capacity_limit), 0)?;
        for collection in &data {
            table.combine(collection)?;
        }
        drop(data);
        table.finalize_into(|chunk| {
            chunks.push(self.output_chunk(partition.set_index, set, chunk)?);
            Ok(())
        })?;
        Ok(chunks)
    }
}

impl Source for PhysicalHashAggregate {
    type Global = HashAggregateSourceState;
    type Local = HashAggregateSourceLocal;

    fn global_source_state(&self, ctx: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        let sink = sink.ok_or_else(|| Error::internal("hash aggregate source requires its sink state"))?;
        Ok(HashAggregateSourceState {
            sink: Arc::clone(sink),
            next: AtomicUsize::new(0),
            max_memory: ctx.env.buffer_manager.max_memory(),
        })
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<Self::Local> {
        Ok(HashAggregateSourceLocal::default())
    }

    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<SourceResult> {
        let state = global
            .sink
            .downcast_ref::<HashAggregateGlobal>()
            .ok_or_else(|| Error::internal("hash aggregate sink state has the wrong type"))?;
        loop {
            if let Some(result) = local.pending.pop_front() {
                chunk.set_columns(result.columns().to_vec(), result.len());
                return Ok(SourceResult::HaveMoreOutput);
            }
            let index = global.next.fetch_add(1, Ordering::Relaxed);
            if let Some(partition) = state.partitions().get(index) {
                local.pending = self.finalize_partition(ctx, partition)?.into();
            } else if let Some(result) = state.results().get(index - state.partitions().len()) {
                chunk.set_columns(result.columns().to_vec(), result.len());
                return Ok(SourceResult::HaveMoreOutput);
            } else {
                return Ok(SourceResult::Finished);
            }
        }
    }

    /// One thread per partition, as many as fit in memory: finalizing a partition takes
    /// about twice its size (DuckDB limits the threads through the partitions' temporary
    /// memory reservation).
    fn max_threads(&self, global: &Self::Global) -> usize {
        let Some(state) = global.sink.downcast_ref::<HashAggregateGlobal>() else {
            return 1;
        };
        let largest = state
            .partitions()
            .iter()
            .map(|partition| partition.size)
            .max()
            .unwrap_or(0) as u64;
        let fitting = usize::try_from(global.max_memory / (2 * largest).max(1)).unwrap_or(usize::MAX);
        (state.partitions().len() + state.results().len())
            .min(fitting)
            .max(1)
    }
}
