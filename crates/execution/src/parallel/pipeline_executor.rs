//! Drives one thread's work on a pipeline (DuckDB's `PipelineExecutor`,
//! `pipeline_executor.cpp`): pulls chunks from the source, pushes them through the
//! operators into the sink, flushes operators that hold rows back, and finally combines
//! the thread's sink state into the global one.

use std::any::Any;
use std::sync::Arc;
use std::time::Instant;

use rustdb_common::{Error, Result};
use rustdb_types::{DataChunk, STANDARD_VECTOR_SIZE};

use super::pipeline::{BATCH_INCREMENT, Pipeline, SharedState};
use crate::operator::{
    ErasedOperator, ErasedSink, ErasedSource, OperatorPartitionInfo, PartitionInfo, PhysicalRef,
};
use crate::{ExecutionContext, OperatorResult, SinkResult, SourceResult};

/// How many chunks a partial execution processes before returning
/// (`PipelineTask::PARTIAL_CHUNK_COUNT`).
pub(crate) const PARTIAL_CHUNK_COUNT: usize = 50;

/// The outcome of [`PipelineExecutor::execute`] (DuckDB's `PipelineExecuteResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PipelineExecuteResult {
    /// The chunk budget ran out; call again.
    NotFinished,
    /// The pipeline is exhausted and the thread's sink state was combined.
    Finished,
    /// The sink blocked (`INTERRUPTED`): call again once the task is rescheduled.
    Interrupted,
}

/// The outcome of pushing a chunk through the operators into the sink
/// (`OperatorResultType` as `ExecutePushInternal` returns it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PushResult {
    NeedMoreInput,
    HaveMoreOutput,
    Finished,
    /// The sink blocked; the final chunk is kept to be sunk again.
    Blocked,
}

impl From<OperatorResult> for PushResult {
    fn from(result: OperatorResult) -> Self {
        match result {
            OperatorResult::NeedMoreInput => Self::NeedMoreInput,
            OperatorResult::HaveMoreOutput => Self::HaveMoreOutput,
            OperatorResult::Finished => Self::Finished,
        }
    }
}

/// The number of chunks an execution may still push (DuckDB's `ExecutionBudget`).
struct ExecutionBudget {
    processed: usize,
    maximum: usize,
}

impl ExecutionBudget {
    fn new(maximum: usize) -> Self {
        Self {
            processed: 0,
            maximum,
        }
    }

    fn next(&mut self) -> bool {
        if self.is_depleted() {
            return false;
        }
        self.processed += 1;
        true
    }

    fn is_depleted(&self) -> bool {
        self.processed >= self.maximum
    }
}

/// The global states a pipeline's operators share across threads.
pub(crate) struct PipelineStates {
    pub source: SharedState,
    pub operators: Vec<SharedState>,
    pub sink: SharedState,
    /// Per operator: whether it is a sink that finalized with no possible output, which
    /// makes the pipeline produce nothing (`SinkFinalizeType::NO_OUTPUT_POSSIBLE`).
    pub no_output_possible: Vec<bool>,
}

/// The operators of a profiled pipeline.
struct Profiling {
    profiler: Arc<crate::QueryProfiler>,
    source: usize,
    operators: Vec<usize>,
    sink: usize,
}

/// How far the operators holding rows back have been flushed
/// (`TryFlushCachingOperators`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlushState {
    NotStarted,
    /// Flushing the operator at `operator`; `again` if its last `final_execute` said it
    /// has more output.
    Flushing {
        operator: usize,
        again: bool,
    },
    Done,
}

/// One thread's execution of a pipeline (DuckDB's `PipelineExecutor`).
pub(crate) struct PipelineExecutor {
    ctx: ExecutionContext,
    pipeline: Arc<Pipeline>,
    states: PipelineStates,
    local_source_state: Box<dyn Any + Send>,
    /// Per operator.
    intermediate_states: Vec<Box<dyn Any + Send>>,
    /// Taken by the final combine.
    local_sink_state: Option<Box<dyn Any + Send>>,
    /// `chunks[0]` receives the source's output, `chunks[i]` is the input of operator
    /// `i`, and the last chunk the pipeline's output (DuckDB's `intermediate_chunks` plus
    /// `final_chunk`; with no operators the source writes the final chunk directly).
    chunks: Vec<DataChunk>,
    /// Operators (1-based positions) that returned `HaveMoreOutput` and must be called
    /// again with the same input.
    in_process_operators: Vec<usize>,
    /// Set when an operator (its 1-based position) or the sink (`usize::MAX`) declared
    /// the pipeline finished.
    finished_processing_idx: Option<usize>,
    /// What the sink needs to know about the batches of its input
    /// (`required_partition_info`).
    required_partition_info: OperatorPartitionInfo,
    partition_info: PartitionInfo,
    exhausted_pipeline: bool,
    /// The source returned its last chunk (DuckDB's `exhausted_source`).
    exhausted_source: bool,
    /// The sink blocked on the final chunk, which must be sunk again
    /// (`remaining_sink_chunk`).
    remaining_sink_chunk: bool,
    flush: FlushState,
    profiling: Option<Profiling>,
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

impl PipelineExecutor {
    pub fn new(mut ctx: ExecutionContext, pipeline: Arc<Pipeline>, states: PipelineStates) -> Result<Self> {
        let sink = role(&pipeline.sink, |op| op.as_sink())?;
        let source = role(&pipeline.source, |op| op.as_source())?;
        let mut local_sink_state = sink.local_sink_state(&ctx, states.sink.as_ref())?;
        let required_partition_info = pipeline.sink.required_partition_info();
        let mut partition_info = PartitionInfo::default();
        if required_partition_info.any_required() {
            if !pipeline
                .source
                .supports_partitioning(&OperatorPartitionInfo::batch_index())
            {
                return Err(Error::internal(
                    "Attempting to schedule a pipeline where the sink requires batch index but source does not support it",
                ));
            }
            // The batch index is set before anything is fetched.
            partition_info.batch_index = pipeline.register_new_batch_index();
            partition_info.min_batch_index = partition_info.batch_index;
            sink.next_batch(
                &ctx,
                states.sink.as_ref(),
                local_sink_state.as_mut(),
                &partition_info,
            )?;
            ctx.partition_info = partition_info.clone();
        }
        let local_source_state = source.local_source_state(&ctx, states.source.as_ref())?;
        let mut intermediate_states = Vec::with_capacity(pipeline.operators.len());
        let mut chunks = Vec::with_capacity(pipeline.operators.len() + 1);
        let output_chunk = |op: &PhysicalRef| {
            let capacity = if op.preallocate_output() {
                STANDARD_VECTOR_SIZE
            } else {
                0
            };
            DataChunk::with_types(op.types(), capacity)
        };
        chunks.push(output_chunk(&pipeline.source));
        let mut finished_processing_idx = None;
        for (index, op) in pipeline.operators.iter().enumerate() {
            let operator = role(op, |op| op.as_operator())?;
            intermediate_states.push(operator.local_operator_state(&ctx, states.operators[index].as_ref())?);
            chunks.push(output_chunk(op));
            if states.no_output_possible[index] {
                // A join already knows it produces nothing: skip the pipeline.
                finished_processing_idx = Some(usize::MAX);
            }
        }
        let profiling = ctx.profiler.clone().map(|profiler| Profiling {
            profiler,
            source: crate::profiler::operator_key(pipeline.source.as_ref()),
            operators: pipeline
                .operators
                .iter()
                .map(|op| crate::profiler::operator_key(op.as_ref()))
                .collect(),
            sink: crate::profiler::operator_key(pipeline.sink.as_ref()),
        });
        Ok(Self {
            ctx,
            pipeline,
            states,
            local_source_state,
            intermediate_states,
            local_sink_state: Some(local_sink_state),
            chunks,
            in_process_operators: Vec::new(),
            finished_processing_idx,
            required_partition_info,
            partition_info,
            exhausted_pipeline: false,
            exhausted_source: false,
            remaining_sink_chunk: false,
            flush: FlushState::NotStarted,
            profiling,
        })
    }

    fn source(&self) -> Result<&dyn ErasedSource> {
        role(&self.pipeline.source, |op| op.as_source())
    }

    fn sink(&self) -> Result<&dyn ErasedSink> {
        role(&self.pipeline.sink, |op| op.as_sink())
    }

    fn operator(&self, index: usize) -> Result<&dyn ErasedOperator> {
        role(&self.pipeline.operators[index], |op| op.as_operator())
    }

    /// The position of the final chunk in `chunks`.
    fn final_index(&self) -> usize {
        self.pipeline.operators.len()
    }

    fn start_timer(&self) -> Option<Instant> {
        self.profiling.as_ref().map(|_| Instant::now())
    }

    /// Records a profiled call of the operator `key` selects, which produced
    /// `chunks[output]` (`OperatorProfiler::EndOperator`; a sink produces no chunk).
    fn record(&self, key: impl FnOnce(&Profiling) -> usize, output: Option<usize>, started: Option<Instant>) {
        if let (Some(profiling), Some(started)) = (&self.profiling, started) {
            let elapsed = started.elapsed();
            let (rows, bytes) = output.map_or((0, 0), |index| {
                let chunk = &self.chunks[index];
                (chunk.len() as u64, chunk.allocation_size() as u64)
            });
            profiling.profiler.record(key(profiling), rows, elapsed, bytes);
        }
    }

    fn is_finished(&self) -> bool {
        self.finished_processing_idx.is_some()
    }

    /// Stops pulling input: the operator at position `operator_idx` (or the sink, `None`)
    /// will produce no more output (`FinishProcessing`). Operators above it may still hold
    /// output for the rows it let through; those stay in process.
    fn finish_processing(&mut self, operator_idx: Option<usize>) {
        let index = operator_idx.unwrap_or(usize::MAX);
        self.finished_processing_idx = Some(index);
        self.in_process_operators.retain(|&position| position > index);
    }

    /// Whether the sink blocked on the final chunk (`RemainingSinkChunk`).
    pub fn remaining_sink_chunk(&self) -> bool {
        self.remaining_sink_chunk
    }

    /// Runs the pipeline until it is exhausted, or until `max_chunks` chunks were pushed
    /// (DuckDB's `Execute(max_chunks)`).
    pub fn execute(&mut self, max_chunks: usize) -> Result<PipelineExecuteResult> {
        let mut budget = ExecutionBudget::new(max_chunks);
        loop {
            self.ctx.check_interrupted()?;
            if self.is_finished() && self.in_process_operators.is_empty() && !self.remaining_sink_chunk {
                // Nothing more can come out: stop pulling from the source.
                self.exhausted_pipeline = true;
            }
            let flushing = self.flush != FlushState::NotStarted;
            let result = if self.exhausted_pipeline
                && self.flush == FlushState::Done
                && !self.remaining_sink_chunk
                && self.in_process_operators.is_empty()
            {
                break;
            } else if self.remaining_sink_chunk {
                // The sink blocked: sink the final chunk again.
                self.remaining_sink_chunk = false;
                self.execute_push_internal(self.final_index(), &mut budget)?
            } else if !self.in_process_operators.is_empty() && !flushing {
                // Operators returned `HaveMoreOutput`: call them again with the same input.
                self.execute_push_internal(0, &mut budget)?
            } else if self.exhausted_pipeline {
                if self.try_flush_caching_operators(&mut budget)? {
                    break;
                }
                if self.remaining_sink_chunk {
                    return Ok(PipelineExecuteResult::Interrupted);
                }
                return Ok(PipelineExecuteResult::NotFinished);
            } else {
                self.chunks[0].reset();
                let source_result = self.fetch_from_source()?;
                if source_result == SourceResult::Finished {
                    self.exhausted_pipeline = true;
                    self.exhausted_source = true;
                }
                if self.required_partition_info.any_required() {
                    self.next_batch(source_result == SourceResult::HaveMoreOutput)?;
                }
                if self.exhausted_pipeline && self.chunks[0].is_empty() {
                    continue;
                }
                self.execute_push_internal(0, &mut budget)?
            };
            if result == PushResult::Blocked {
                // `SINK INTERRUPT`.
                self.remaining_sink_chunk = true;
                return Ok(PipelineExecuteResult::Interrupted);
            }
            if result == PushResult::Finished {
                self.exhausted_pipeline = true;
            }
            if !budget.next() {
                break;
            }
        }
        if (!self.exhausted_pipeline || self.flush != FlushState::Done) && !self.is_finished() {
            return Ok(PipelineExecuteResult::NotFinished);
        }
        self.push_finalize()?;
        Ok(PipelineExecuteResult::Finished)
    }

    /// Lets operators that hold rows back emit them once the input is exhausted, each
    /// operator completely before the next (`TryFlushCachingOperators`). Returns whether
    /// flushing is complete.
    fn try_flush_caching_operators(&mut self, budget: &mut ExecutionBudget) -> Result<bool> {
        let (mut index, mut again) = match self.flush {
            FlushState::Done => return Ok(true),
            FlushState::Flushing { operator, again } => (operator, again),
            // Operators below one that finished have nothing left to give.
            FlushState::NotStarted => (self.finished_processing_idx.unwrap_or(0), true),
        };
        while index < self.pipeline.operators.len() {
            if !self.operator(index)?.requires_final_execute() {
                index += 1;
                continue;
            }
            if !again && self.in_process_operators.is_empty() {
                again = true;
                index += 1;
                continue;
            }
            // Operators in process mean an earlier push of this operator's output was cut
            // short by the budget: resume it instead of flushing again.
            if self.in_process_operators.is_empty() {
                let output = index + 1;
                self.chunks[output].reset();
                let started = self.start_timer();
                again = {
                    let operator = role(&self.pipeline.operators[index], |op| op.as_operator())?;
                    operator.final_execute(
                        &self.ctx,
                        &mut self.chunks[output],
                        self.states.operators[index].as_ref(),
                        self.intermediate_states[index].as_mut(),
                    )?
                };
                self.record(|p| p.operators[index], Some(output), started);
            }
            self.flush = FlushState::Flushing {
                operator: index,
                again,
            };
            match self.execute_push_internal(index + 1, budget)? {
                PushResult::HaveMoreOutput => return Ok(false),
                PushResult::Blocked => {
                    self.remaining_sink_chunk = true;
                    return Ok(false);
                }
                PushResult::NeedMoreInput | PushResult::Finished => {}
            }
        }
        self.flush = FlushState::Done;
        Ok(true)
    }

    /// Moves the sink on to the batch of the chunk just fetched (`NextBatch`).
    fn next_batch(&mut self, have_more_output: bool) -> Result<()> {
        let base = self.pipeline.base_batch_index;
        let max_batch_index = base + BATCH_INCREMENT - 1;
        let current = self.partition_info.batch_index;
        let mut partition_data = Vec::new();
        let next = if !self.chunks[0].is_empty() {
            let data = self.source()?.partition_data(
                self.states.source.as_ref(),
                self.local_source_state.as_ref(),
                &self.required_partition_info,
            )?;
            let next = base + data.batch_index + 1;
            if next >= max_batch_index {
                return Err(Error::internal(format!(
                    "Pipeline batch index - invalid batch index {} returned by source operator",
                    data.batch_index
                )));
            }
            partition_data = data.partition_data;
            next
        } else if have_more_output {
            current
        } else {
            max_batch_index
        };
        if next == current {
            return Ok(());
        }
        if current > next {
            return Err(Error::internal(format!(
                "Pipeline batch index - gotten lower batch index {next} (down from previous batch index of {current})"
            )));
        }
        self.partition_info.batch_index = next;
        self.partition_info.partition_data = partition_data;
        // The sink sees the new batch before the minimum moves, so it can flush the
        // previous one.
        let local = self
            .local_sink_state
            .as_mut()
            .ok_or_else(|| Error::internal("pipeline sink state already combined"))?;
        role(&self.pipeline.sink, |op| op.as_sink())?.next_batch(
            &self.ctx,
            self.states.sink.as_ref(),
            local.as_mut(),
            &self.partition_info,
        )?;
        self.partition_info.min_batch_index = self.pipeline.update_batch_index(current, next)?;
        // The sink sees the batch of its input and the minimum (`partition_info` of the
        // local sink state).
        self.ctx.partition_info = self.partition_info.clone();
        Ok(())
    }

    /// Pushes `chunks[initial_idx]` through the operators after position `initial_idx`
    /// into the sink, as long as operators have more output and the budget lasts
    /// (`ExecutePushInternal`).
    fn execute_push_internal(
        &mut self,
        initial_idx: usize,
        budget: &mut ExecutionBudget,
    ) -> Result<PushResult> {
        if self.chunks[initial_idx].is_empty() {
            return Ok(PushResult::NeedMoreInput);
        }
        let final_index = self.final_index();
        let mut result;
        loop {
            if initial_idx == final_index {
                // The input is the final chunk: it only needs to be sunk.
                result = OperatorResult::NeedMoreInput;
            } else {
                self.chunks[final_index].reset();
                result = self.execute_operators(initial_idx)?;
            }
            if !self.chunks[final_index].is_empty() {
                let started = self.start_timer();
                let sink_result = {
                    let local = self
                        .local_sink_state
                        .as_mut()
                        .ok_or_else(|| Error::internal("pipeline sink state already combined"))?;
                    role(&self.pipeline.sink, |op| op.as_sink())?.sink(
                        &self.ctx,
                        &self.chunks[final_index],
                        self.states.sink.as_ref(),
                        local.as_mut(),
                    )?
                };
                self.record(|p| p.sink, None, started);
                match sink_result {
                    SinkResult::Finished => {
                        self.finish_processing(None);
                        return Ok(PushResult::Finished);
                    }
                    SinkResult::Blocked => return Ok(PushResult::Blocked),
                    SinkResult::NeedMoreInput => {}
                }
            }
            if result != OperatorResult::HaveMoreOutput {
                return Ok(result.into());
            }
            if !budget.next() {
                return Ok(result.into());
            }
        }
    }

    /// The position to resume at: the most recent operator with pending output, else the
    /// input (`GoToSource`).
    fn go_to_source(&mut self, initial_idx: usize) -> usize {
        if self.is_finished() && self.in_process_operators.is_empty() {
            return initial_idx;
        }
        match self.in_process_operators.pop() {
            Some(current) => {
                debug_assert!(current >= initial_idx);
                current
            }
            None => initial_idx,
        }
    }

    /// Runs `chunks[initial_idx]` through the operators after position `initial_idx` once,
    /// leaving the pipeline's output in the final chunk (DuckDB's
    /// `PipelineExecutor::Execute(input, result, initial_idx)`). Positions are 1-based:
    /// operator `p - 1` reads `chunks[p - 1]` and writes `chunks[p]`.
    fn execute_operators(&mut self, initial_idx: usize) -> Result<OperatorResult> {
        let operator_count = self.pipeline.operators.len();
        let mut current_idx = self.go_to_source(initial_idx);
        if current_idx == initial_idx {
            current_idx += 1;
        }
        if current_idx > operator_count {
            return Err(Error::internal("pipeline executed past its last operator"));
        }
        loop {
            self.ctx.check_interrupted()?;
            self.chunks[current_idx].reset();
            if current_idx == initial_idx {
                // Back at the input: it is fully processed.
                return Ok(if self.is_finished() {
                    OperatorResult::Finished
                } else {
                    OperatorResult::NeedMoreInput
                });
            }
            let operator_idx = current_idx - 1;
            let started = self.start_timer();
            let result = {
                let (before, after) = self.chunks.split_at_mut(current_idx);
                let operator = role(&self.pipeline.operators[operator_idx], |op| op.as_operator())?;
                operator.execute(
                    &self.ctx,
                    &before[operator_idx],
                    &mut after[0],
                    self.states.operators[operator_idx].as_ref(),
                    self.intermediate_states[operator_idx].as_mut(),
                )?
            };
            self.record(|p| p.operators[operator_idx], Some(current_idx), started);
            match result {
                OperatorResult::HaveMoreOutput => self.in_process_operators.push(current_idx),
                // Unlike DuckDB's operators, ours may emit their last rows together with
                // `Finished`: those rows still go through the rest of the pipeline.
                OperatorResult::Finished => self.finish_processing(Some(current_idx)),
                OperatorResult::NeedMoreInput => {}
            }
            if self.chunks[current_idx].is_empty() {
                // No output: pull from the source (or a pending operator) again.
                current_idx = self.go_to_source(initial_idx);
                continue;
            }
            current_idx += 1;
            if current_idx > operator_count {
                break;
            }
        }
        Ok(if !self.in_process_operators.is_empty() {
            OperatorResult::HaveMoreOutput
        } else if self.is_finished() {
            OperatorResult::Finished
        } else {
            OperatorResult::NeedMoreInput
        })
    }

    /// Records what the source reports once the thread is done with it: parameters for
    /// `EXPLAIN ANALYZE` and what a table scan read (`FinalizeSourceProfiling`).
    fn finalize_source_profiling(&self) -> Result<()> {
        let Some(profiling) = &self.profiling else {
            return Ok(());
        };
        let source = role(&self.pipeline.source, |op| op.as_source())?;
        let params =
            source.extra_source_params(self.states.source.as_ref(), self.local_source_state.as_ref())?;
        if !params.is_empty() {
            profiling.profiler.record_extra_info(profiling.source, params);
        }
        // Our sources may return their last rows with `Finished`: when an operator
        // finished the pipeline on those rows, DuckDB would not have asked the source
        // again, so it does not count as exhausted.
        let exhausted = self.exhausted_source && !self.is_finished();
        if let Some(metrics) = source.scan_metrics(
            self.states.source.as_ref(),
            self.local_source_state.as_ref(),
            exhausted,
        )? {
            profiling.profiler.record_scan(profiling.source, metrics);
        }
        Ok(())
    }

    /// Fetches the next chunk from the source into `chunks[0]` (`FetchFromSource`).
    fn fetch_from_source(&mut self) -> Result<SourceResult> {
        let started = self.start_timer();
        let result = role(&self.pipeline.source, |op| op.as_source())?.get_data(
            &self.ctx,
            &mut self.chunks[0],
            self.states.source.as_ref(),
            self.local_source_state.as_mut(),
        )?;
        self.record(|p| p.source, Some(0), started);
        Ok(result)
    }

    /// Combines the thread's sink state into the global one (`PushFinalize`).
    fn push_finalize(&mut self) -> Result<()> {
        let local = self.local_sink_state.take().ok_or_else(|| {
            Error::internal("Calling PushFinalize on a pipeline that has been finalized already")
        })?;
        self.sink()?
            .combine(&self.ctx, self.states.sink.as_ref(), local)?;
        self.finalize_source_profiling()
    }
}
