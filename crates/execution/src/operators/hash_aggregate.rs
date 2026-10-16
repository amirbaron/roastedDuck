use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::operator::{aggregate_names, expression_names, set_estimated_cardinality};
use rustdb_common::tree_renderer::RenderParams;
use rustdb_common::{Error, Result};
use rustdb_planner::Expression;
use rustdb_planner::expression::BoundAggregateExpression;
use rustdb_planner::operator::GroupingSet;
use rustdb_row::{PartitionedTupleData, RadixPartitioning, TupleDataCollection, hash::hash_columns};
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use crate::aggregate_hashtable::{AggregateObject, GroupedAggregateHashTable, initial_capacity};
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
            children: [child],
        })
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
        radix_bits: usize,
    ) -> Result<GroupedAggregateHashTable> {
        GroupedAggregateHashTable::new(
            ctx.env.buffer_manager.clone(),
            self.set_group_types(set),
            &self.aggregate_objects(),
            initial_capacity(),
            radix_bits,
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
    /// Per grouping set, per `DISTINCT` aggregate: the distinct (groups, arguments)
    /// tuples (DuckDB's `DistinctAggregateData`).
    distinct: Vec<Vec<Option<Mutex<GroupedAggregateHashTable>>>>,
    /// Whether any input row arrived (for empty grouping sets).
    saw_input: std::sync::atomic::AtomicBool,
    /// After finalisation: the groups still to combine and emit, one unit per grouping
    /// set and radix partition (DuckDB's `RadixHTGlobalSourceState` partitions).
    partitions: Vec<AggregatePartition>,
    /// The row an empty grouping set produces without input.
    results: Vec<DataChunk>,
}

/// The rows of one radix partition of one grouping set, from every thread, which a
/// source task merges and emits.
#[derive(Debug)]
struct AggregatePartition {
    set_index: usize,
    data: Mutex<Vec<TupleDataCollection>>,
}

/// Per-thread state: one hash table per grouping set plus expression executors.
#[derive(Debug)]
pub struct HashAggregateLocal {
    tables: Vec<GroupedAggregateHashTable>,
    groups: ExpressionExecutor,
    arguments: Vec<ExpressionExecutor>,
    filters: Vec<Option<ExpressionExecutor>>,
}

impl Sink for PhysicalHashAggregate {
    type Global = HashAggregateGlobal;
    type Local = HashAggregateLocal;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<HashAggregateGlobal> {
        let distinct = self
            .grouping_sets
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
                        let table = GroupedAggregateHashTable::new(
                            ctx.env.buffer_manager.clone(),
                            types,
                            &[],
                            initial_capacity(),
                            0,
                        )?;
                        Ok(Some(Mutex::new(table)))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(HashAggregateGlobal {
            collected: Mutex::new(self.grouping_sets.iter().map(|_| Vec::new()).collect()),
            distinct,
            ..HashAggregateGlobal::default()
        })
    }

    fn local_sink_state(
        &self,
        ctx: &ExecutionContext,
        _: &HashAggregateGlobal,
    ) -> Result<HashAggregateLocal> {
        let radix_bits = Self::radix_bits(ctx);
        let tables = self
            .grouping_sets
            .iter()
            .map(|set| self.new_table(ctx, set, radix_bits))
            .collect::<Result<_>>()?;
        Ok(HashAggregateLocal {
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
        _: &ExecutionContext,
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
            for (index, distinct) in global.distinct[set_index].iter().enumerate() {
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
                add_tuples(
                    &mut distinct.lock().unwrap_or_else(PoisonError::into_inner),
                    &tuples,
                    &[],
                    &[],
                )?;
                selections[index] = Some(Vec::new());
            }
            let hashes = if groups.column_count() == 0 {
                Vector::constant(&Value::ubigint(0))?
            } else {
                let refs: Vec<&Vector> = groups.columns().iter().collect();
                hash_columns(&refs, chunk.len())?
            };
            table.add_chunk(&groups, &hashes, &inputs, &selections)?;
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(
        &self,
        _: &ExecutionContext,
        global: &HashAggregateGlobal,
        mut local: HashAggregateLocal,
    ) -> Result<()> {
        let mut collected = global.collected.lock().unwrap_or_else(PoisonError::into_inner);
        for (set, table) in local.tables.iter_mut().enumerate() {
            collected[set].push(table.take_partitioned_data()?);
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
        self.finalize_distinct(ctx, global)?;
        let collected = std::mem::take(global.collected.get_mut().unwrap_or_else(PoisonError::into_inner));
        let objects = self.aggregate_objects();
        for (set_index, (set, mut parts)) in self.grouping_sets.iter().zip(collected).enumerate() {
            let partition_count = parts
                .iter()
                .map(PartitionedTupleData::partition_count)
                .max()
                .unwrap_or(1);
            let rows: usize = parts.iter().map(PartitionedTupleData::count).sum();
            let mut taken: Vec<Vec<TupleDataCollection>> = (0..partition_count).map(|_| Vec::new()).collect();
            for part in &mut parts {
                for (partition, collection) in part.take_partitions().into_iter().enumerate() {
                    if collection.count() > 0 {
                        taken[partition].push(collection);
                    }
                }
            }
            global.partitions.extend(
                taken
                    .into_iter()
                    .filter(|data| !data.is_empty())
                    .map(|data| AggregatePartition {
                        set_index,
                        data: Mutex::new(data),
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
                    global.results.push(self.output_chunk(set_index, set, chunk)?);
                    Ok(())
                })?;
            }
        }
        Ok(crate::SinkFinalize::Ready)
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
    /// Feeds the distinct tuples of each `DISTINCT` aggregate into a table of their
    /// groups, whose partitions are merged with the threads' (DuckDB's
    /// `FinalizeDistinct`).
    fn finalize_distinct(&self, ctx: &ExecutionContext, global: &mut HashAggregateGlobal) -> Result<()> {
        let collected = global.collected.get_mut().unwrap_or_else(PoisonError::into_inner);
        for (set_index, set) in self.grouping_sets.iter().enumerate() {
            if global.distinct[set_index].iter().all(Option::is_none) {
                continue;
            }
            let mut table = self.new_table(ctx, set, Self::radix_bits(ctx))?;
            let group_count = set.len();
            for (index, distinct) in global.distinct[set_index].iter_mut().enumerate() {
                let Some(distinct) = distinct else { continue };
                let selections: Vec<Option<Vec<u32>>> = (0..self.aggregates.len())
                    .map(|other| if other == index { None } else { Some(Vec::new()) })
                    .collect();
                distinct
                    .get_mut()
                    .unwrap_or_else(PoisonError::into_inner)
                    .finalize_into(|tuples| {
                        let count = tuples.len();
                        let mut columns = tuples.into_columns();
                        let arguments = columns.split_off(group_count);
                        let mut inputs = vec![Vec::new(); self.aggregates.len()];
                        inputs[index] = arguments;
                        add_tuples(&mut table, &DataChunk::new(columns, count), &inputs, &selections)
                    })?;
            }
            collected[set_index].push(table.take_partitioned_data()?);
        }
        Ok(())
    }

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
}

/// A source task's emitted partition: its finalized chunks, in order.
#[derive(Debug, Default)]
pub struct HashAggregateSourceLocal {
    pending: VecDeque<DataChunk>,
}

impl PhysicalHashAggregate {
    /// Merges partition `partition` of the threads' groups into one table and returns
    /// its rows in output layout.
    fn finalize_partition(&self, ctx: &ExecutionContext, partition: &AggregatePartition) -> Result<Vec<DataChunk>> {
        let data = std::mem::take(&mut *partition.data.lock().unwrap_or_else(PoisonError::into_inner));
        let set = &self.grouping_sets[partition.set_index];
        let mut table = GroupedAggregateHashTable::new(
            ctx.env.buffer_manager.clone(),
            self.set_group_types(set),
            &self.aggregate_objects(),
            initial_capacity(),
            0,
        )?;
        for collection in &data {
            table.combine(collection)?;
        }
        drop(data);
        let mut chunks = Vec::new();
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

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        let sink = sink.ok_or_else(|| Error::internal("hash aggregate source requires its sink state"))?;
        Ok(HashAggregateSourceState {
            sink: Arc::clone(sink),
            next: AtomicUsize::new(0),
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
            if let Some(partition) = state.partitions.get(index) {
                local.pending = self.finalize_partition(ctx, partition)?.into();
            } else if let Some(result) = state.results.get(index - state.partitions.len()) {
                chunk.set_columns(result.columns().to_vec(), result.len());
                return Ok(SourceResult::HaveMoreOutput);
            } else {
                return Ok(SourceResult::Finished);
            }
        }
    }

    fn max_threads(&self, global: &Self::Global) -> usize {
        global
            .sink
            .downcast_ref::<HashAggregateGlobal>()
            .map_or(1, |state| (state.partitions.len() + state.results.len()).max(1))
    }
}
