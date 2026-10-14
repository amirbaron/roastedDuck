use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::AggregateInput;
use rustdb_function::{AggregateOps, StatePtr};
use rustdb_planner::BoundAggregate;
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, ExpressionExecutor, Sink, SinkResult, Source, SourceResult};

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
        if let Some(aggregate) = aggregates.iter().find(|a| a.distinct) {
            return Err(Error::not_implemented(format!(
                "DISTINCT aggregate {}",
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
}

/// Per-thread state: local aggregate states plus executors for arguments and filters.
#[derive(Debug)]
pub struct UngroupedLocal {
    states: AggregateStates,
    arguments: Vec<ExpressionExecutor>,
    filters: Vec<Option<ExpressionExecutor>>,
    selection: Vec<u32>,
}

fn poisoned() -> Error {
    Error::internal("aggregate state lock poisoned")
}

impl Sink for PhysicalUngroupedAggregate {
    type Global = Mutex<AggregateStates>;
    type Local = UngroupedLocal;

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        Ok(Mutex::new(AggregateStates::new(&self.aggregates)))
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<UngroupedLocal> {
        Ok(UngroupedLocal {
            states: AggregateStates::new(&self.aggregates),
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
                        .map(|f| ExpressionExecutor::new(vec![(**f).clone()]))
                })
                .collect(),
            selection: Vec::new(),
        })
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        _: &Self::Global,
        local: &mut UngroupedLocal,
    ) -> Result<SinkResult> {
        for (index, aggregate) in self.aggregates.iter().enumerate() {
            let mut input = chunk.clone();
            if let Some(filter) = &mut local.filters[index] {
                let passed = filter.select(chunk, &mut local.selection)?;
                let sel = rustdb_types::SelectionVector::new(std::mem::take(&mut local.selection));
                input = DataChunk::new(chunk.columns().iter().map(|c| c.slice(&sel)).collect(), passed);
            }
            let mut arguments = DataChunk::default();
            local.arguments[index].execute(&input, &mut arguments)?;
            let data = AggregateInput {
                bind_data: aggregate.bind_data.as_deref().map(|d| d as _),
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

    fn combine(&self, _: &ExecutionContext, global: &Self::Global, local: UngroupedLocal) -> Result<()> {
        let global = global.lock().map_err(|_| poisoned())?;
        for (index, aggregate) in self.aggregates.iter().enumerate() {
            let data = AggregateInput {
                bind_data: aggregate.bind_data.as_deref().map(|d| d as _),
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
                .downcast_ref::<Mutex<AggregateStates>>()
                .ok_or_else(|| Error::internal("ungrouped aggregate sink state has the wrong type"))?;
            finalize_ungrouped(self, states, chunk)?;
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
            bind_data: aggregate.bind_data.as_deref().map(|d| d as _),
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
