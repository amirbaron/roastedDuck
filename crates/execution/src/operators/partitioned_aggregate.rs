//! Aggregation of input that is partitioned by its groups (DuckDB's
//! `PhysicalPartitionedAggregate`): every batch of the source holds a single value of
//! the grouping columns (a hive partition), so each batch is aggregated like an
//! ungrouped aggregate and combined into the states of its partition.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rustdb_common::tree_renderer::RenderParams;
use rustdb_common::{Error, Result};
use rustdb_planner::Expression;
use rustdb_planner::expression::BoundAggregateExpression as BoundAggregate;
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use crate::column_data_collection::ColumnDataCollection;
use crate::operator::{
    ErasedSink, ErasedSource, OperatorPartitionInfo, PartitionInfo, PhysicalOperator, PhysicalRef, SinkState,
    aggregate_names,
};
use crate::operators::ungrouped_aggregate::{AggregateStates, UngroupedAggregateExecuteState};
use crate::{ExecutionContext, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// Aggregates input whose batches each hold one value of the groups; see the module docs.
#[derive(Debug)]
pub struct PhysicalPartitionedAggregate {
    types: Vec<LogicalType>,
    /// The columns of the source over which the input is partitioned (by position in
    /// the source's output), one per group.
    partitions: Vec<u64>,
    /// The groups, for `EXPLAIN`: their values come from the partitions.
    groups: Vec<Expression>,
    aggregates: Vec<BoundAggregate>,
    children: [PhysicalRef; 1],
}

impl PhysicalPartitionedAggregate {
    pub fn new(
        types: Vec<LogicalType>,
        aggregates: Vec<BoundAggregate>,
        groups: Vec<Expression>,
        partitions: Vec<u64>,
        child: PhysicalRef,
    ) -> Self {
        Self {
            types,
            partitions,
            groups,
            aggregates,
            children: [child],
        }
    }
}

impl PhysicalOperator for PhysicalPartitionedAggregate {
    fn name(&self) -> &'static str {
        "PARTITIONED_AGGREGATE"
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
    fn required_partition_info(&self) -> OperatorPartitionInfo {
        OperatorPartitionInfo::partition_columns(self.partitions.clone())
    }

    fn params_to_string(&self, _: u64) -> RenderParams {
        let mut result = RenderParams::new();
        let groups: Vec<String> = self.groups.iter().map(Expression::name).collect();
        result.insert("Groups".to_owned(), groups.join("\n"));
        result.insert("Aggregates".to_owned(), aggregate_names(&self.aggregates));
        result
    }
}

/// The aggregate states of every partition and, once finalized, the result rows
/// (DuckDB's `PartitionedAggregateGlobalSinkState`).
#[derive(Debug)]
pub struct PartitionedAggregateGlobal {
    /// The states of each partition, by the partition's group values.
    aggregate_states: Mutex<HashMap<Vec<Value>, AggregateStates>>,
    aggregate_result: Option<ColumnDataCollection>,
}

/// A thread's states for the partition of its current batch (DuckDB's
/// `PartitionedAggregateLocalSinkState`).
#[derive(Debug)]
pub struct PartitionedAggregateLocal {
    /// The group values of the current batch.
    current_partition: Vec<Value>,
    /// The aggregate states of the current batch, created by its first chunk.
    state: Option<AggregateStates>,
    execute_state: UngroupedAggregateExecuteState,
}

fn poisoned() -> Error {
    Error::internal("partitioned aggregate state lock poisoned")
}

impl PhysicalPartitionedAggregate {
    /// Combines the thread's states into those of its partition and clears them
    /// (DuckDB's `PartitionedAggregateGlobalSinkState::Combine`).
    fn combine_partition(
        &self,
        global: &PartitionedAggregateGlobal,
        local: &mut PartitionedAggregateLocal,
    ) -> Result<()> {
        let Some(state) = local.state.take() else {
            return Ok(());
        };
        let mut states = global.aggregate_states.lock().map_err(|_| poisoned())?;
        let global_state = states
            .entry(local.current_partition.clone())
            .or_insert_with(|| AggregateStates::new(&self.aggregates));
        state.combine_into(&self.aggregates, global_state)
    }
}

impl Sink for PhysicalPartitionedAggregate {
    type Global = PartitionedAggregateGlobal;
    type Local = PartitionedAggregateLocal;

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        Ok(PartitionedAggregateGlobal {
            aggregate_states: Mutex::new(HashMap::new()),
            aggregate_result: None,
        })
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<Self::Local> {
        Ok(PartitionedAggregateLocal {
            current_partition: Vec::new(),
            state: None,
            execute_state: UngroupedAggregateExecuteState::new(&self.aggregates),
        })
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        _: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<SinkResult> {
        if local.current_partition.len() != self.groups.len() {
            return Err(Error::internal(
                "partitioned aggregate input without a value for every partition column",
            ));
        }
        let state = local
            .state
            .get_or_insert_with(|| AggregateStates::new(&self.aggregates));
        local.execute_state.sink(&self.aggregates, state, chunk)?;
        Ok(SinkResult::NeedMoreInput)
    }

    /// Flushes the states of the previous batch into its partition (DuckDB's `NextBatch`
    /// and the partition set-up of `Sink`).
    fn next_batch(
        &self,
        _: &ExecutionContext,
        global: &Self::Global,
        local: &mut Self::Local,
        partition: &PartitionInfo,
    ) -> Result<()> {
        self.combine_partition(global, local)?;
        local.current_partition.clone_from(&partition.partition_data);
        Ok(())
    }

    fn combine(&self, _: &ExecutionContext, global: &Self::Global, mut local: Self::Local) -> Result<()> {
        self.combine_partition(global, &mut local)
    }

    /// Finalizes each partition into a row of its group values and aggregate results.
    fn finalize(&self, ctx: &ExecutionContext, global: &mut Self::Global) -> Result<SinkFinalize> {
        let mut result = ColumnDataCollection::new(ctx.env.buffer_manager.clone(), self.types.clone())?;
        let mut append = result.initialize_append();
        let states = global.aggregate_states.get_mut().map_err(|_| poisoned())?;
        for (partition, states) in states.iter() {
            let mut columns = partition
                .iter()
                .map(Vector::constant)
                .collect::<Result<Vec<_>>>()?;
            columns.extend(states.finalize(&self.aggregates)?);
            result.append(&mut append, &DataChunk::new(columns, 1))?;
        }
        result.finalize_append(&mut append);
        global.aggregate_result = Some(result);
        Ok(SinkFinalize::Ready)
    }
}

impl Source for PhysicalPartitionedAggregate {
    /// The finalized sink state and the next result chunk to emit.
    type Global = (SinkState, AtomicUsize);
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        let sink =
            sink.ok_or_else(|| Error::internal("partitioned aggregate source requires its sink state"))?;
        Ok((Arc::clone(sink), AtomicUsize::new(0)))
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
        let result = sink
            .downcast_ref::<PartitionedAggregateGlobal>()
            .and_then(|state| state.aggregate_result.as_ref())
            .ok_or_else(|| Error::internal("partitioned aggregate source before its sink finalized"))?;
        let index = next.fetch_add(1, Ordering::Relaxed);
        if index < result.chunk_count() {
            let columns: Vec<usize> = (0..self.types.len()).collect();
            result.fetch_chunk(index, &columns, chunk)?;
        }
        Ok(if index + 1 < result.chunk_count() {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }
}
