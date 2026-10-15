use std::collections::HashMap;
use std::sync::Arc;

use rustdb_common::{Error, Result};

use crate::operator::{PhysicalRef, SinkState};
use crate::pipeline::{Pipeline, PipelineExecutor, PipelineStates};
use crate::{ClientEnv, ExecutionContext, SinkFinalize};

/// Splits a physical plan into pipelines and runs them (DuckDB's `Executor`).
#[derive(Debug)]
pub struct Executor {
    env: Arc<ClientEnv>,
    pipelines: Vec<Pipeline>,
    /// Finalized sink states, keyed by operator address.
    sink_states: HashMap<usize, SinkState>,
}

fn key(op: &PhysicalRef) -> usize {
    Arc::as_ptr(op).cast::<()>() as usize
}

impl Executor {
    /// Runs the plan rooted at `root` (a sink, e.g. a result collector) to completion and
    /// returns the root's finalized sink state.
    pub fn execute(root: &PhysicalRef, env: Arc<ClientEnv>) -> Result<SinkState> {
        let mut executor = Self {
            env,
            pipelines: Vec::new(),
            sink_states: HashMap::new(),
        };
        let root_pipeline = executor.add_pipeline(Pipeline::new(Arc::clone(root)));
        let input = root
            .children()
            .first()
            .ok_or_else(|| Error::internal("the root operator of a plan must have an input"))?;
        executor.build(input, root_pipeline)?;
        // Child pipelines are always created after the pipelines that depend on them, so
        // running in reverse creation order respects every dependency.
        for index in (0..executor.pipelines.len()).rev() {
            executor.run_pipeline(index)?;
        }
        executor
            .sink_states
            .remove(&key(root))
            .ok_or_else(|| Error::internal("the root pipeline produced no sink state"))
    }

    fn add_pipeline(&mut self, pipeline: Pipeline) -> usize {
        self.pipelines.push(pipeline);
        self.pipelines.len() - 1
    }

    /// Adds `op` (and its inputs) to pipeline `current` (DuckDB's `BuildPipelines`).
    fn build(&mut self, op: &PhysicalRef, current: usize) -> Result<()> {
        if op.as_sink().is_some() {
            // A sink ends a child pipeline and is the source of the current one.
            self.pipelines[current].source = Some(Arc::clone(op));
            let child = self.add_pipeline(Pipeline::new(Arc::clone(op)));
            self.pipelines[current].dependencies.push(child);
            let input = op
                .children()
                .first()
                .ok_or_else(|| Error::internal(format!("sink {} has no input", op.name())))?;
            return self.build(input, child);
        }
        if op.as_operator().is_some() {
            let input = op
                .children()
                .first()
                .ok_or_else(|| Error::internal(format!("operator {} has no input", op.name())))?;
            self.build(input, current)?;
            self.pipelines[current].operators.push(Arc::clone(op));
            return Ok(());
        }
        if op.as_source().is_some() {
            self.pipelines[current].source = Some(Arc::clone(op));
            return Ok(());
        }
        Err(Error::internal(format!(
            "operator {} has no pipeline role",
            op.name()
        )))
    }

    fn run_pipeline(&mut self, index: usize) -> Result<()> {
        let ctx = ExecutionContext::new(Arc::clone(&self.env));
        let pipeline = &self.pipelines[index];
        let source_op = pipeline
            .source
            .as_ref()
            .ok_or_else(|| Error::internal("pipeline without a source"))?;
        let no_role = |name: &str| Error::internal(format!("operator {name} lacks its pipeline role"));
        let source = source_op.as_source().ok_or_else(|| no_role(source_op.name()))?;
        let sink = pipeline
            .sink
            .as_sink()
            .ok_or_else(|| no_role(pipeline.sink.name()))?;

        let source_global = source.global_source_state(&ctx, self.sink_states.get(&key(source_op)))?;
        let operator_globals = pipeline
            .operators
            .iter()
            .map(|op| {
                op.as_operator()
                    .ok_or_else(|| no_role(op.name()))?
                    .global_operator_state(&ctx)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut sink_global = sink.global_sink_state(&ctx)?;

        let states = PipelineStates {
            source_global: source_global.as_ref(),
            operator_globals: &operator_globals,
            sink_global: sink_global.as_ref(),
        };
        let mut executor = PipelineExecutor::new(ctx.clone(), pipeline, states)?;
        executor.run(source_op.types())?;
        let local = std::mem::replace(&mut executor.sink_local, Box::new(()));
        drop(executor);
        sink.combine(&ctx, sink_global.as_ref(), local)?;
        // `NoOutputPossible` needs no special handling yet: dependent sources simply observe
        // an empty state.
        let _: SinkFinalize = sink.finalize(&ctx, sink_global.as_mut())?;
        self.sink_states
            .insert(key(&pipeline.sink), Arc::from(sink_global));
        Ok(())
    }
}
