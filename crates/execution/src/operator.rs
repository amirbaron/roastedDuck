use std::any::Any;
use std::fmt;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_types::{DataChunk, LogicalType};

use crate::ExecutionContext;

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

/// A node of a physical plan. Its roles (source, streaming operator, sink) are exposed
/// through the typed traits below and erased for the executor.
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
    /// Whether this is a `UNION ALL`, whose children feed the same pipeline sink.
    fn is_union(&self) -> bool {
        false
    }
    /// Whether this is a materialised CTE: a sink for its first input whose second input
    /// continues the current pipeline once the sink has finished.
    fn is_cte(&self) -> bool {
        false
    }
    /// Sinks outside this source's subtree whose pipelines must finish first (delim
    /// scans read the data of their delim join, CTE scans that of their CTE).
    fn source_dependencies(&self) -> Vec<PhysicalRef> {
        Vec::new()
    }
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
}
