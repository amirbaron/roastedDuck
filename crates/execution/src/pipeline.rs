use std::any::Any;

use rustdb_common::{Error, Result};
use rustdb_types::{DataChunk, STANDARD_VECTOR_SIZE};

use crate::operator::{ErasedOperator, ErasedSink, ErasedSource, PhysicalRef};
use crate::{ExecutionContext, OperatorResult, SinkResult, SourceResult};

/// A source → operators → sink chain (DuckDB's `Pipeline`).
#[derive(Debug)]
pub(crate) struct Pipeline {
    pub source: Option<PhysicalRef>,
    /// Streaming operators in execution order (source side first).
    pub operators: Vec<PhysicalRef>,
    pub sink: PhysicalRef,
    /// Pipelines that must complete before this one starts.
    pub dependencies: Vec<usize>,
}

impl Pipeline {
    pub fn new(sink: PhysicalRef) -> Self {
        Self {
            source: None,
            operators: Vec::new(),
            sink,
            dependencies: Vec::new(),
        }
    }
}

/// Global (shared) states of one pipeline run.
pub(crate) struct PipelineStates<'a> {
    pub source_global: &'a (dyn Any + Send + Sync),
    pub operator_globals: Vec<&'a (dyn Any + Send + Sync)>,
    pub sink_global: &'a (dyn Any + Send + Sync),
}

/// Drives one thread's work on a pipeline (DuckDB's `PipelineExecutor`).
pub(crate) struct PipelineExecutor<'a> {
    ctx: ExecutionContext,
    source: &'a dyn ErasedSource,
    operators: Vec<&'a dyn ErasedOperator>,
    sink: &'a dyn ErasedSink,
    states: PipelineStates<'a>,
    source_local: Box<dyn Any + Send>,
    operator_locals: Vec<Box<dyn Any + Send>>,
    pub sink_local: Box<dyn Any + Send>,
    /// `outputs[i]` receives the output of operator `i`.
    outputs: Vec<DataChunk>,
    /// Operators that returned `HaveMoreOutput` and must be called again.
    in_process: Vec<usize>,
    finished: bool,
}

fn role<'a, T: ?Sized>(
    op: &'a PhysicalRef,
    get: impl FnOnce(&'a PhysicalRef) -> Option<&'a T>,
) -> Result<&'a T> {
    get(op).ok_or_else(|| {
        Error::internal(format!(
            "operator {} used in a pipeline role it does not support",
            op.name()
        ))
    })
}

impl<'a> PipelineExecutor<'a> {
    pub fn new(ctx: ExecutionContext, pipeline: &'a Pipeline, states: PipelineStates<'a>) -> Result<Self> {
        let source_op = pipeline
            .source
            .as_ref()
            .ok_or_else(|| Error::internal("pipeline without a source"))?;
        let source = role(source_op, |op| op.as_source())?;
        let sink = role(&pipeline.sink, |op| op.as_sink())?;
        let operators = pipeline
            .operators
            .iter()
            .map(|op| role(op, |op| op.as_operator()))
            .collect::<Result<Vec<_>>>()?;
        let source_local = source.local_source_state(&ctx, states.source_global)?;
        let operator_locals = operators
            .iter()
            .zip(&states.operator_globals)
            .map(|(op, global)| op.local_operator_state(&ctx, *global))
            .collect::<Result<Vec<_>>>()?;
        let sink_local = sink.local_sink_state(&ctx, states.sink_global)?;
        let outputs = pipeline
            .operators
            .iter()
            .map(|op| DataChunk::with_types(op.types(), STANDARD_VECTOR_SIZE))
            .collect();
        Ok(Self {
            ctx,
            source,
            operators,
            sink,
            states,
            source_local,
            operator_locals,
            sink_local,
            outputs,
            in_process: Vec::new(),
            finished: false,
        })
    }

    /// Runs the pipeline until the source is exhausted or the sink is satisfied.
    pub fn run(&mut self, source_types: &[rustdb_types::LogicalType]) -> Result<()> {
        let mut source_chunk = DataChunk::with_types(source_types, STANDARD_VECTOR_SIZE);
        while !self.finished {
            source_chunk.reset();
            let source_result = self.source.get_data(
                &self.ctx,
                &mut source_chunk,
                self.states.source_global,
                self.source_local.as_mut(),
            )?;
            if !source_chunk.is_empty() {
                self.push(&source_chunk)?;
            }
            if source_result == SourceResult::Finished {
                break;
            }
        }
        if !self.finished {
            self.flush_operators()?;
        }
        Ok(())
    }

    /// Lets operators emit the rows they held back once the source is exhausted
    /// (DuckDB's `FlushCachingOperatorsPush`).
    fn flush_operators(&mut self) -> Result<()> {
        for index in 0..self.operators.len() {
            if self.finished {
                break;
            }
            if !self.operators[index].requires_final_execute() {
                continue;
            }
            loop {
                let mut chunk = DataChunk::default();
                let more = self.operators[index].final_execute(
                    &self.ctx,
                    &mut chunk,
                    self.states.operator_globals[index],
                    self.operator_locals[index].as_mut(),
                )?;
                if !chunk.is_empty() {
                    self.push_from(&chunk, index + 1)?;
                }
                if !more || self.finished {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Pushes one source chunk through the operators into the sink, re-entering operators
    /// that still have output buffered (DuckDB's `ExecutePushInternal`).
    fn push(&mut self, input: &DataChunk) -> Result<()> {
        self.push_from(input, 0)
    }

    /// Pushes `input` through the operators from `start` on into the sink.
    fn push_from(&mut self, input: &DataChunk, start: usize) -> Result<()> {
        loop {
            let (result, final_index) = self.execute_operators(input, start)?;
            let final_chunk = match final_index {
                Some(index) => &self.outputs[index],
                None => input,
            };
            if !final_chunk.is_empty() {
                let sink_result = self.sink.sink(
                    &self.ctx,
                    final_chunk,
                    self.states.sink_global,
                    self.sink_local.as_mut(),
                )?;
                if sink_result == SinkResult::Finished {
                    self.finished = true;
                    return Ok(());
                }
            }
            match result {
                OperatorResult::NeedMoreInput => return Ok(()),
                OperatorResult::Finished => {
                    self.finished = true;
                    return Ok(());
                }
                OperatorResult::HaveMoreOutput => {}
            }
        }
    }

    /// Runs the operator chain once. Returns whether operators still have buffered output
    /// and which chunk holds the final result (`None` = the input itself, when there are
    /// no operators). Mirrors DuckDB's `PipelineExecutor::Execute`.
    fn execute_operators(
        &mut self,
        input: &DataChunk,
        start: usize,
    ) -> Result<(OperatorResult, Option<usize>)> {
        let count = self.operators.len();
        if start >= count {
            return Ok((OperatorResult::NeedMoreInput, None));
        }
        // `current` is the index of the next operator to run; operators re-entered from
        // the in-process stack resume with their previous input.
        // Only output produced in this pass may reach the sink (DuckDB resets the final
        // chunk before every pass).
        self.outputs[count - 1].reset();
        let mut current = self.in_process.pop().unwrap_or(start);
        loop {
            let (before, rest) = self.outputs.split_at_mut(current);
            let output = &mut rest[0];
            output.reset();
            let op_input = if current == start {
                input
            } else {
                &before[current - 1]
            };
            let result = self.operators[current].execute(
                &self.ctx,
                op_input,
                output,
                self.states.operator_globals[current],
                self.operator_locals[current].as_mut(),
            )?;
            match result {
                OperatorResult::HaveMoreOutput => self.in_process.push(current),
                OperatorResult::Finished => self.finished = true,
                OperatorResult::NeedMoreInput => {}
            }
            if output.is_empty() {
                // Nothing to pass on: resume the most recent operator with pending output,
                // or return to the source.
                match self.in_process.pop() {
                    Some(resume) => current = resume,
                    None => break,
                }
            } else if current + 1 == count {
                break;
            } else {
                current += 1;
            }
        }
        let state = if self.finished {
            OperatorResult::Finished
        } else if self.in_process.is_empty() {
            OperatorResult::NeedMoreInput
        } else {
            OperatorResult::HaveMoreOutput
        };
        Ok((state, Some(count - 1)))
    }
}
