use std::any::Any;
use std::fmt;
use std::sync::Arc;

use rustdb_common::tree_renderer::{RenderParams, RenderTreeNode};
use rustdb_common::{Error, Result};
use rustdb_types::{DataChunk, LogicalType};

use crate::ExecutionContext;
use crate::profiler::ScanMetrics;

/// A shared handle to a physical operator.
pub type PhysicalRef = Arc<dyn PhysicalOperator>;

/// Outcome of [`Source::get_data`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceResult {
    HaveMoreOutput,
    Finished,
}

/// Outcome of [`Operator::execute`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperatorResult {
    /// The input chunk is fully processed.
    NeedMoreInput,
    /// Call again with the same input to get more output.
    HaveMoreOutput,
    /// No more output will ever be produced; the pipeline can stop.
    Finished,
}

/// Outcome of [`Sink::sink`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkResult {
    NeedMoreInput,
    /// The sink needs no more input (e.g. a satisfied LIMIT).
    Finished,
}

/// Outcome of [`Sink::finalize`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkFinalize {
    Ready,
    /// The sink is known to produce no rows; dependent pipelines can be skipped.
    NoOutputPossible,
}

/// The row order an operator produces (DuckDB's `OrderPreservationType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderPreservationType {
    /// The rows come in no particular order.
    NoOrder,
    /// The rows keep the order of the operator's input (the default).
    InsertionOrder,
    /// The rows come in an order that must be kept (e.g. `ORDER BY`).
    FixedOrder,
}

/// The batch a sink's current input belongs to (DuckDB's `OperatorPartitionInfo` in the
/// local sink state): sinks that require batch indices see their input split into batches
/// numbered in source order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PartitionInfo {
    /// The batch the next chunks belong to.
    pub batch_index: u64,
    /// The lowest batch index any thread of the pipeline is still working on: every batch
    /// below it is complete.
    pub min_batch_index: u64,
}

/// A node of a physical plan. Its roles (source, streaming operator, sink) are exposed
/// through the typed traits below and erased for the executor.
///
/// The `parallel_*`, `*_order` and batch index methods tell the executor how a pipeline
/// may be split across threads; their defaults are DuckDB's `PhysicalOperator` defaults
/// (sequential, insertion order, no batch indices).
pub trait PhysicalOperator: Send + Sync + fmt::Debug + 'static {
    fn name(&self) -> &'static str;
    fn types(&self) -> &[LogicalType];
    fn children(&self) -> &[PhysicalRef];

    fn as_source(&self) -> Option<&dyn ErasedSource> {
        None
    }
    fn as_operator(&self) -> Option<&dyn ErasedOperator> {
        None
    }
    fn as_sink(&self) -> Option<&dyn ErasedSink> {
        None
    }
    /// Whether several threads may read from the source role at once, each with its own
    /// local state (DuckDB's `ParallelSource`).
    fn parallel_source(&self) -> bool {
        false
    }
    /// Whether several threads may run the operator role at once (`ParallelOperator`).
    fn parallel_operator(&self) -> bool {
        false
    }
    /// Whether several threads may feed the sink role at once (`ParallelSink`).
    fn parallel_sink(&self) -> bool {
        false
    }
    /// The order of the rows the source role emits (`SourceOrder`).
    fn source_order(&self) -> OrderPreservationType {
        OrderPreservationType::InsertionOrder
    }
    /// The order of the rows the operator role emits relative to its input
    /// (`OperatorOrder`).
    fn operator_order(&self) -> OrderPreservationType {
        OrderPreservationType::InsertionOrder
    }
    /// Whether the sink's result depends on the order of its input (`SinkOrderDependent`).
    fn sink_order_dependent(&self) -> bool {
        false
    }
    /// Whether the source role numbers its output in batches that follow its row order
    /// (`SupportsPartitioning(OperatorPartitionInfo::BatchIndex())`).
    fn supports_batch_index(&self) -> bool {
        false
    }
    /// Whether the sink role needs the batch index of its input (`RequiredPartitionInfo`).
    fn requires_batch_index(&self) -> bool {
        false
    }
    /// Whether this is a `UNION ALL`, whose children feed the same pipeline sink.
    fn is_union(&self) -> bool {
        false
    }
    /// Whether this is a materialised CTE: a sink for its first input whose second input
    /// continues the current pipeline once the sink has finished.
    fn is_cte(&self) -> bool {
        false
    }
    /// Whether every child (not only the first) feeds this sink, each in its own
    /// pipeline, the last child first (DuckDB's `PhysicalIEJoin::BuildPipelines`).
    fn sinks_all_children(&self) -> bool {
        false
    }
    /// Sinks outside this source's subtree whose pipelines must finish first (delim
    /// scans read the data of their delim join, CTE scans that of their CTE).
    fn source_dependencies(&self) -> Vec<PhysicalRef> {
        Vec::new()
    }
    /// The operator's name in `EXPLAIN` output (DuckDB's `PhysicalOperator::GetName`).
    fn explain_name(&self) -> String {
        self.name().to_owned()
    }
    /// The operator's name in `EXPLAIN ANALYZE` output (DuckDB's `PhysicalOperatorType`
    /// name in the query profiler).
    fn profiler_name(&self) -> String {
        self.explain_name()
    }
    /// The operator's children in `EXPLAIN` output: its children, plus inputs that run
    /// as nested executions rather than pipelines (a recursive CTE's parts).
    fn explain_children(&self) -> &[PhysicalRef] {
        self.children()
    }
    /// The operator's key-value lines in `EXPLAIN` output (DuckDB's `ParamsToString`);
    /// `estimated_cardinality` is the planner's estimate of the operator's output rows.
    fn params_to_string(&self, estimated_cardinality: u64) -> RenderParams {
        let _ = estimated_cardinality;
        RenderParams::new()
    }
}

/// Adds the estimated cardinality to `EXPLAIN` lines (DuckDB's
/// `PhysicalOperator::SetEstimatedCardinality`).
pub fn set_estimated_cardinality(result: &mut RenderParams, estimated_cardinality: u64) {
    result.insert(
        RenderTreeNode::ESTIMATED_CARDINALITY.to_owned(),
        estimated_cardinality.to_string(),
    );
}

/// The names of aggregates with their filters, one per line (the aggregate loops of
/// DuckDB's aggregate `ParamsToString`).
pub fn aggregate_names(aggregates: &[rustdb_planner::expression::BoundAggregateExpression]) -> String {
    aggregates
        .iter()
        .map(|aggregate| {
            let filter = aggregate.filter.as_ref().map(rustdb_planner::Expression::name);
            let expression = rustdb_planner::Expression::new(
                rustdb_planner::ExpressionType::BoundAggregate,
                aggregate.function.return_type.clone(),
                rustdb_planner::ExpressionKind::Aggregate(Box::new(aggregate.clone())),
            );
            match filter {
                Some(filter) => format!("{} Filter: {filter}", expression.name()),
                None => expression.name(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Sort keys as `expression ASC|DESC`, one per line (DuckDB's `PhysicalOrder::ParamsToString`).
pub fn order_names(orders: &[rustdb_planner::expression::BoundOrderByNode]) -> String {
    orders
        .iter()
        .map(|order| {
            let direction = if order.order_type == rustdb_ast::OrderType::Descending {
                "DESC"
            } else {
                "ASC"
            };
            format!("{} {direction}", order.expression)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The names of expressions, one per line (the `GetName` loops of `ParamsToString`).
pub fn expression_names(expressions: &[rustdb_planner::Expression]) -> String {
    expressions
        .iter()
        .map(rustdb_planner::Expression::name)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Data materialised by a sink and read by scans elsewhere in the plan.
pub trait SinkData: std::fmt::Debug + Send + Sync {
    /// The sink producing the data, while the plan is alive.
    fn sink(&self) -> Option<PhysicalRef>;
}

/// An operator that produces chunks at the start of a pipeline.
pub trait Source: PhysicalOperator {
    type Global: Send + Sync + 'static;
    type Local: Send + 'static;

    /// `sink_state` is this operator's finalized sink state when it is also a sink.
    fn global_source_state(
        &self,
        ctx: &ExecutionContext,
        sink_state: Option<&SinkState>,
    ) -> Result<Self::Global>;
    fn local_source_state(&self, ctx: &ExecutionContext, global: &Self::Global) -> Result<Self::Local>;
    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<SourceResult>;

    /// How many threads can usefully read the source at once
    /// (`GlobalSourceState::MaxThreads`); only consulted for parallel sources.
    fn max_threads(&self, global: &Self::Global) -> usize {
        let _ = global;
        1
    }

    /// What `EXPLAIN ANALYZE` adds to the operator's parameters once a thread has
    /// exhausted the source (`ExtraSourceParams`).
    fn extra_source_params(&self, global: &Self::Global, local: &Self::Local) -> Vec<(String, String)> {
        let _ = (global, local);
        Vec::new()
    }

    /// What a thread's table scan read, for the profiler (`PhysicalTableScan::GetMetrics`
    /// with the cardinality fallback of `FinalizeSourceProfiling`); `exhausted` tells
    /// whether the thread read the source to its end. `None` for other sources.
    fn scan_metrics(&self, global: &Self::Global, local: &Self::Local, exhausted: bool) -> Option<ScanMetrics> {
        let _ = (global, local, exhausted);
        None
    }

    /// The batch index of the chunk `get_data` just returned, counted from 0 in the
    /// source's row order (DuckDB's `GetPartitionData`); only called on sources that
    /// [support batch indices](PhysicalOperator::supports_batch_index).
    fn batch_index(&self, global: &Self::Global, local: &Self::Local) -> Result<u64> {
        let _ = (global, local);
        Err(Error::internal(format!(
            "{} does not support batch indices",
            self.name()
        )))
    }
}

/// A streaming operator in the middle of a pipeline.
pub trait Operator: PhysicalOperator {
    type Global: Send + Sync + 'static;
    type Local: Send + 'static;

    fn global_operator_state(&self, ctx: &ExecutionContext) -> Result<Self::Global>;
    /// Like [`Operator::global_operator_state`], for operators that are also sinks (joins):
    /// `sink_state` is the operator's finalized sink state.
    fn global_operator_state_with_sink(
        &self,
        ctx: &ExecutionContext,
        sink_state: Option<&SinkState>,
    ) -> Result<Self::Global> {
        let _ = sink_state;
        self.global_operator_state(ctx)
    }
    fn local_operator_state(&self, ctx: &ExecutionContext, global: &Self::Global) -> Result<Self::Local>;
    fn execute(
        &self,
        ctx: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        global: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<OperatorResult>;

    /// Whether the operator holds back rows that [`Operator::final_execute`] must emit
    /// once its input is exhausted (DuckDB's `RequiresFinalExecute`).
    fn requires_final_execute(&self) -> bool {
        false
    }

    /// Emits held-back rows after the last input chunk (DuckDB's `FinalExecute`).
    /// Returns whether it must be called again for more output.
    fn final_execute(
        &self,
        ctx: &ExecutionContext,
        output: &mut DataChunk,
        global: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<bool> {
        let _ = (ctx, output, global, local);
        Ok(false)
    }
}

/// An operator that consumes a pipeline's output (and may act as the source of a parent
/// pipeline afterwards).
pub trait Sink: PhysicalOperator {
    type Global: Send + Sync + 'static;
    type Local: Send + 'static;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<Self::Global>;
    fn local_sink_state(&self, ctx: &ExecutionContext, global: &Self::Global) -> Result<Self::Local>;
    fn sink(
        &self,
        ctx: &ExecutionContext,
        chunk: &DataChunk,
        global: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<SinkResult>;
    /// Merges a thread's local state into the global state.
    fn combine(&self, ctx: &ExecutionContext, global: &Self::Global, local: Self::Local) -> Result<()>;
    /// Called once after every thread has combined.
    fn finalize(&self, ctx: &ExecutionContext, global: &mut Self::Global) -> Result<SinkFinalize> {
        let _ = (ctx, global);
        Ok(SinkFinalize::Ready)
    }

    /// How many threads may feed the sink at once, given that the source allows
    /// `source_max` (`GlobalSinkState::MaxThreads`).
    fn max_threads(&self, global: &Self::Global, source_max: usize) -> usize {
        let _ = global;
        source_max
    }

    /// The input moves on to the batch `partition.batch_index` (DuckDB's `NextBatch`);
    /// only called on sinks that [require batch indices](PhysicalOperator::requires_batch_index),
    /// once before the first chunk and whenever the batch changes.
    fn next_batch(
        &self,
        ctx: &ExecutionContext,
        global: &Self::Global,
        local: &mut Self::Local,
        partition: PartitionInfo,
    ) -> Result<()> {
        let _ = (ctx, global, local, partition);
        Ok(())
    }

    /// The number of tasks of the sink's next parallel finalize stage, or 0 once the sink
    /// is completely finalized. Called after [`Sink::finalize`] and after every stage;
    /// each stage's tasks run concurrently (DuckDB's sink-specific finalize events such as
    /// `HashJoinFinalizeEvent`).
    fn finalize_tasks(&self, ctx: &ExecutionContext, global: &Self::Global) -> Result<usize> {
        let _ = (ctx, global);
        Ok(0)
    }

    /// Runs task `task` of the current finalize stage.
    fn execute_finalize_task(
        &self,
        ctx: &ExecutionContext,
        global: &Self::Global,
        task: usize,
    ) -> Result<()> {
        let _ = (ctx, global, task);
        Err(Error::internal(format!(
            "{} has no parallel finalize tasks",
            self.name()
        )))
    }
}

/// A sink's global state after finalisation, shared with the operator's source role.
pub type SinkState = Arc<dyn Any + Send + Sync>;

type AnyGlobal = Box<dyn Any + Send + Sync>;
type AnyLocal = Box<dyn Any + Send>;

fn state_type_error(op: &str) -> Error {
    Error::internal(format!("operator state of the wrong type passed to {op}"))
}

fn downcast<'a, T: 'static>(state: &'a (dyn Any + Send + Sync), op: &str) -> Result<&'a T> {
    state.downcast_ref::<T>().ok_or_else(|| state_type_error(op))
}

fn downcast_local<'a, T: 'static>(state: &'a mut (dyn Any + Send), op: &str) -> Result<&'a mut T> {
    state.downcast_mut::<T>().ok_or_else(|| state_type_error(op))
}

/// Object-safe view of [`Source`] used by the executor.
pub trait ErasedSource {
    fn global_source_state(
        &self,
        ctx: &ExecutionContext,
        sink_state: Option<&SinkState>,
    ) -> Result<AnyGlobal>;
    fn local_source_state(
        &self,
        ctx: &ExecutionContext,
        global: &(dyn Any + Send + Sync),
    ) -> Result<AnyLocal>;
    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &(dyn Any + Send + Sync),
        local: &mut (dyn Any + Send),
    ) -> Result<SourceResult>;
    fn max_threads(&self, global: &(dyn Any + Send + Sync)) -> Result<usize>;
    fn batch_index(&self, global: &(dyn Any + Send + Sync), local: &(dyn Any + Send)) -> Result<u64>;
    fn extra_source_params(
        &self,
        global: &(dyn Any + Send + Sync),
        local: &(dyn Any + Send),
    ) -> Result<Vec<(String, String)>>;
    fn scan_metrics(
        &self,
        global: &(dyn Any + Send + Sync),
        local: &(dyn Any + Send),
        exhausted: bool,
    ) -> Result<Option<ScanMetrics>>;
}

impl<T: Source> ErasedSource for T {
    fn global_source_state(
        &self,
        ctx: &ExecutionContext,
        sink_state: Option<&SinkState>,
    ) -> Result<AnyGlobal> {
        Ok(Box::new(Source::global_source_state(self, ctx, sink_state)?))
    }
    fn local_source_state(
        &self,
        ctx: &ExecutionContext,
        global: &(dyn Any + Send + Sync),
    ) -> Result<AnyLocal> {
        Ok(Box::new(Source::local_source_state(
            self,
            ctx,
            downcast(global, self.name())?,
        )?))
    }
    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &(dyn Any + Send + Sync),
        local: &mut (dyn Any + Send),
    ) -> Result<SourceResult> {
        let name = self.name();
        Source::get_data(
            self,
            ctx,
            chunk,
            downcast(global, name)?,
            downcast_local(local, name)?,
        )
    }
    fn max_threads(&self, global: &(dyn Any + Send + Sync)) -> Result<usize> {
        Ok(Source::max_threads(self, downcast(global, self.name())?))
    }
    fn batch_index(&self, global: &(dyn Any + Send + Sync), local: &(dyn Any + Send)) -> Result<u64> {
        let name = self.name();
        let local = local
            .downcast_ref::<T::Local>()
            .ok_or_else(|| state_type_error(name))?;
        Source::batch_index(self, downcast(global, name)?, local)
    }
    fn extra_source_params(
        &self,
        global: &(dyn Any + Send + Sync),
        local: &(dyn Any + Send),
    ) -> Result<Vec<(String, String)>> {
        let name = self.name();
        let local = local
            .downcast_ref::<T::Local>()
            .ok_or_else(|| state_type_error(name))?;
        Ok(Source::extra_source_params(self, downcast(global, name)?, local))
    }
    fn scan_metrics(
        &self,
        global: &(dyn Any + Send + Sync),
        local: &(dyn Any + Send),
        exhausted: bool,
    ) -> Result<Option<ScanMetrics>> {
        let name = self.name();
        let local = local
            .downcast_ref::<T::Local>()
            .ok_or_else(|| state_type_error(name))?;
        Ok(Source::scan_metrics(
            self,
            downcast(global, name)?,
            local,
            exhausted,
        ))
    }
}

/// Object-safe view of [`Operator`] used by the executor.
pub trait ErasedOperator {
    fn global_operator_state(
        &self,
        ctx: &ExecutionContext,
        sink_state: Option<&SinkState>,
    ) -> Result<AnyGlobal>;
    fn local_operator_state(
        &self,
        ctx: &ExecutionContext,
        global: &(dyn Any + Send + Sync),
    ) -> Result<AnyLocal>;
    fn execute(
        &self,
        ctx: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        global: &(dyn Any + Send + Sync),
        local: &mut (dyn Any + Send),
    ) -> Result<OperatorResult>;
    fn requires_final_execute(&self) -> bool;
    fn final_execute(
        &self,
        ctx: &ExecutionContext,
        output: &mut DataChunk,
        global: &(dyn Any + Send + Sync),
        local: &mut (dyn Any + Send),
    ) -> Result<bool>;
}

impl<T: Operator> ErasedOperator for T {
    fn global_operator_state(
        &self,
        ctx: &ExecutionContext,
        sink_state: Option<&SinkState>,
    ) -> Result<AnyGlobal> {
        Ok(Box::new(Operator::global_operator_state_with_sink(
            self, ctx, sink_state,
        )?))
    }
    fn local_operator_state(
        &self,
        ctx: &ExecutionContext,
        global: &(dyn Any + Send + Sync),
    ) -> Result<AnyLocal> {
        Ok(Box::new(Operator::local_operator_state(
            self,
            ctx,
            downcast(global, self.name())?,
        )?))
    }
    fn execute(
        &self,
        ctx: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        global: &(dyn Any + Send + Sync),
        local: &mut (dyn Any + Send),
    ) -> Result<OperatorResult> {
        let name = self.name();
        Operator::execute(
            self,
            ctx,
            input,
            output,
            downcast(global, name)?,
            downcast_local(local, name)?,
        )
    }
    fn requires_final_execute(&self) -> bool {
        Operator::requires_final_execute(self)
    }
    fn final_execute(
        &self,
        ctx: &ExecutionContext,
        output: &mut DataChunk,
        global: &(dyn Any + Send + Sync),
        local: &mut (dyn Any + Send),
    ) -> Result<bool> {
        let name = self.name();
        Operator::final_execute(
            self,
            ctx,
            output,
            downcast(global, name)?,
            downcast_local(local, name)?,
        )
    }
}

/// Object-safe view of [`Sink`] used by the executor.
pub trait ErasedSink {
    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<AnyGlobal>;
    fn local_sink_state(&self, ctx: &ExecutionContext, global: &(dyn Any + Send + Sync)) -> Result<AnyLocal>;
    fn sink(
        &self,
        ctx: &ExecutionContext,
        chunk: &DataChunk,
        global: &(dyn Any + Send + Sync),
        local: &mut (dyn Any + Send),
    ) -> Result<SinkResult>;
    fn combine(
        &self,
        ctx: &ExecutionContext,
        global: &(dyn Any + Send + Sync),
        local: AnyLocal,
    ) -> Result<()>;
    fn finalize(&self, ctx: &ExecutionContext, global: &mut (dyn Any + Send + Sync)) -> Result<SinkFinalize>;
    fn max_threads(&self, global: &(dyn Any + Send + Sync), source_max: usize) -> Result<usize>;
    fn next_batch(
        &self,
        ctx: &ExecutionContext,
        global: &(dyn Any + Send + Sync),
        local: &mut (dyn Any + Send),
        partition: PartitionInfo,
    ) -> Result<()>;
    fn finalize_tasks(&self, ctx: &ExecutionContext, global: &(dyn Any + Send + Sync)) -> Result<usize>;
    fn execute_finalize_task(
        &self,
        ctx: &ExecutionContext,
        global: &(dyn Any + Send + Sync),
        task: usize,
    ) -> Result<()>;
}

impl<T: Sink> ErasedSink for T {
    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<AnyGlobal> {
        Ok(Box::new(Sink::global_sink_state(self, ctx)?))
    }
    fn local_sink_state(&self, ctx: &ExecutionContext, global: &(dyn Any + Send + Sync)) -> Result<AnyLocal> {
        Ok(Box::new(Sink::local_sink_state(
            self,
            ctx,
            downcast(global, self.name())?,
        )?))
    }
    fn sink(
        &self,
        ctx: &ExecutionContext,
        chunk: &DataChunk,
        global: &(dyn Any + Send + Sync),
        local: &mut (dyn Any + Send),
    ) -> Result<SinkResult> {
        let name = self.name();
        Sink::sink(
            self,
            ctx,
            chunk,
            downcast(global, name)?,
            downcast_local(local, name)?,
        )
    }
    fn combine(
        &self,
        ctx: &ExecutionContext,
        global: &(dyn Any + Send + Sync),
        local: AnyLocal,
    ) -> Result<()> {
        let local = local
            .downcast::<T::Local>()
            .map_err(|_| state_type_error(self.name()))?;
        Sink::combine(self, ctx, downcast(global, self.name())?, *local)
    }
    fn finalize(&self, ctx: &ExecutionContext, global: &mut (dyn Any + Send + Sync)) -> Result<SinkFinalize> {
        let name = self.name();
        let global = global
            .downcast_mut::<T::Global>()
            .ok_or_else(|| state_type_error(name))?;
        Sink::finalize(self, ctx, global)
    }
    fn max_threads(&self, global: &(dyn Any + Send + Sync), source_max: usize) -> Result<usize> {
        Ok(Sink::max_threads(
            self,
            downcast(global, self.name())?,
            source_max,
        ))
    }
    fn next_batch(
        &self,
        ctx: &ExecutionContext,
        global: &(dyn Any + Send + Sync),
        local: &mut (dyn Any + Send),
        partition: PartitionInfo,
    ) -> Result<()> {
        let name = self.name();
        Sink::next_batch(
            self,
            ctx,
            downcast(global, name)?,
            downcast_local(local, name)?,
            partition,
        )
    }
    fn finalize_tasks(&self, ctx: &ExecutionContext, global: &(dyn Any + Send + Sync)) -> Result<usize> {
        Sink::finalize_tasks(self, ctx, downcast(global, self.name())?)
    }
    fn execute_finalize_task(
        &self,
        ctx: &ExecutionContext,
        global: &(dyn Any + Send + Sync),
        task: usize,
    ) -> Result<()> {
        Sink::execute_finalize_task(self, ctx, downcast(global, self.name())?, task)
    }
}
