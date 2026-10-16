use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use rustdb_common::{Error, Result};

use crate::operator::{PhysicalRef, SinkState};
use crate::pipeline::{Pipeline, PipelineExecutor, PipelineStates};
use crate::{ClientEnv, ExecutionContext};

type AnyGlobal = Box<dyn Any + Send + Sync>;

/// Splits a physical plan into pipelines and runs them (DuckDB's `Executor` +
/// `MetaPipeline`).
///
/// Every sink owns one or more pipelines (more than one below a `UNION ALL`). Sink and
/// operator global states are shared by all pipelines that use the operator; a sink is
/// finalized once its last pipeline has finished.
#[derive(Debug)]
pub struct Executor {
    env: Arc<ClientEnv>,
    pipelines: Vec<Pipeline>,
    sink_globals: HashMap<usize, AnyGlobal>,
    operator_globals: HashMap<usize, AnyGlobal>,
    /// Pipelines still to run, per sink.
    remaining: HashMap<usize, usize>,
    /// Finalized sink states, keyed by operator address.
    sink_states: HashMap<usize, SinkState>,
    /// The profiler of an `EXPLAIN ANALYZE` run.
    profiler: Option<Arc<crate::QueryProfiler>>,
}

fn key(op: &PhysicalRef) -> usize {
    Arc::as_ptr(op).cast::<()>() as usize
}

impl Executor {
    /// Runs the plan rooted at `root` (a sink, e.g. a result collector) to completion and
    /// returns the root's finalized sink state.
    pub fn execute(root: &PhysicalRef, env: Arc<ClientEnv>) -> Result<SinkState> {
        Self::execute_profiled(root, env, None)
    }

    /// [`Self::execute`], recording every operator's rows and time in `profiler`.
    pub fn execute_profiled(
        root: &PhysicalRef,
        env: Arc<ClientEnv>,
        profiler: Option<Arc<crate::QueryProfiler>>,
    ) -> Result<SinkState> {
        let mut executor = Self {
            env,
            pipelines: Vec::new(),
            sink_globals: HashMap::new(),
            operator_globals: HashMap::new(),
            remaining: HashMap::new(),
            sink_states: HashMap::new(),
            profiler,
        };
        let root_pipeline = executor.add_pipeline(Pipeline::new(Arc::clone(root)));
        let input = root
            .children()
            .first()
            .ok_or_else(|| Error::internal("the root operator of a plan must have an input"))?;
        executor.build(input, root_pipeline)?;
        for pipeline in &mut executor.pipelines {
            // Operators were collected top-down; execution order is source side first.
            pipeline.operators.reverse();
            *executor.remaining.entry(key(&pipeline.sink)).or_default() += 1;
        }
        for index in executor.schedule(key(root)) {
            executor.run_pipeline(index)?;
        }
        executor
            .sink_states
            .remove(&key(root))
            .ok_or_else(|| Error::internal("the root pipeline produced no sink state"))
    }

    /// The order to run pipelines in: before a pipeline whose source is another sink, all
    /// of that sink's pipelines; the pipelines of one sink in creation order, so union
    /// branches produce their rows left to right.
    fn schedule(&self, root: usize) -> Vec<usize> {
        let mut by_sink: HashMap<usize, Vec<usize>> = HashMap::new();
        for (index, pipeline) in self.pipelines.iter().enumerate() {
            by_sink.entry(key(&pipeline.sink)).or_default().push(index);
        }
        let mut order = Vec::with_capacity(self.pipelines.len());
        let mut scheduled = std::collections::HashSet::new();
        let mut stack = vec![(root, false)];
        while let Some((sink, expanded)) = stack.pop() {
            let pipelines = by_sink.get(&sink).map(Vec::as_slice).unwrap_or_default();
            if expanded {
                order.extend_from_slice(pipelines);
                continue;
            }
            if !scheduled.insert(sink) {
                continue;
            }
            stack.push((sink, true));
            // Dependencies are pushed in reverse so the first branch's run first.
            for &index in pipelines.iter().rev() {
                let pipeline = &self.pipelines[index];
                let external = pipeline
                    .source
                    .as_ref()
                    .map(|source| source.source_dependencies())
                    .unwrap_or_default();
                let explicit = pipeline
                    .dependencies
                    .iter()
                    .map(|&dependency| &self.pipelines[dependency].sink);
                for dependency in pipeline
                    .operators
                    .iter()
                    .rev()
                    .chain(pipeline.source.as_ref())
                    .chain(external.iter())
                    .chain(explicit)
                {
                    if by_sink.contains_key(&key(dependency)) {
                        stack.push((key(dependency), false));
                    }
                }
            }
        }
        order
    }

    fn add_pipeline(&mut self, pipeline: Pipeline) -> usize {
        self.pipelines.push(pipeline);
        self.pipelines.len() - 1
    }

    fn first_child<'a>(op: &'a PhysicalRef, role: &str) -> Result<&'a PhysicalRef> {
        op.children()
            .first()
            .ok_or_else(|| Error::internal(format!("{role} {} has no input", op.name())))
    }

    /// Adds `op` (and its inputs) to pipeline `current`, top-down (DuckDB's
    /// `BuildPipelines`).
    fn build(&mut self, op: &PhysicalRef, current: usize) -> Result<()> {
        if op.is_union() {
            // Each union child runs in its own pipeline with the operators collected so
            // far and the same sink (`MetaPipeline::CreateUnionPipeline`).
            let [left, right] = op.children() else {
                return Err(Error::internal("UNION must have two inputs"));
            };
            let sibling = Pipeline {
                source: None,
                operators: self.pipelines[current].operators.clone(),
                sink: Arc::clone(&self.pipelines[current].sink),
                dependencies: Vec::new(),
            };
            let sibling = self.add_pipeline(sibling);
            self.build(left, current)?;
            return self.build(right, sibling);
        }
        if op.is_cte() {
            let [definition, query] = op.children() else {
                return Err(Error::internal("CTE must have two inputs"));
            };
            if !self
                .pipelines
                .iter()
                .any(|pipeline| Arc::ptr_eq(&pipeline.sink, op))
            {
                let materialize = self.add_pipeline(Pipeline::new(Arc::clone(op)));
                self.build(definition, materialize)?;
            }
            let materialize = self
                .pipelines
                .iter()
                .position(|pipeline| Arc::ptr_eq(&pipeline.sink, op))
                .ok_or_else(|| Error::internal("CTE pipeline missing"))?;
            self.pipelines[current].dependencies.push(materialize);
            return self.build(query, current);
        }
        if op.as_sink().is_some() && op.as_operator().is_some() {
            return self.build_join(op, current);
        }
        if op.as_sink().is_some() && op.sinks_all_children() {
            // Every child is sunk in its own pipeline, the last child first.
            self.pipelines[current].source = Some(Arc::clone(op));
            for child_op in op.children().iter().rev() {
                let child = self.add_pipeline(Pipeline::new(Arc::clone(op)));
                self.pipelines[current].dependencies.push(child);
                self.build(child_op, child)?;
            }
            return Ok(());
        }
        if op.as_sink().is_some() {
            // A sink ends a child pipeline and is the source of the current one.
            self.pipelines[current].source = Some(Arc::clone(op));
            let child = self.add_pipeline(Pipeline::new(Arc::clone(op)));
            self.pipelines[current].dependencies.push(child);
            return self.build(Self::first_child(op, "sink")?, child);
        }
        if op.as_operator().is_some() {
            self.pipelines[current].operators.push(Arc::clone(op));
            return self.build(Self::first_child(op, "operator")?, current);
        }
        if op.as_source().is_some() {
            self.pipelines[current].source = Some(Arc::clone(op));
            for dependency in op.source_dependencies() {
                if self
                    .pipelines
                    .iter()
                    .any(|pipeline| Arc::ptr_eq(&pipeline.sink, &dependency))
                {
                    continue;
                }
                // The sink feeding this source is outside its subtree: give it a pipeline.
                let pipeline = self.add_pipeline(Pipeline::new(Arc::clone(&dependency)));
                self.build(Self::first_child(&dependency, "sink")?, pipeline)?;
            }
            return Ok(());
        }
        Err(Error::internal(format!(
            "operator {} has no pipeline role",
            op.name()
        )))
    }

    /// A join probes in the current pipeline, builds from its right input in a child
    /// pipeline, and, if it is also a source (outer joins emitting unmatched build rows),
    /// gets a pipeline that runs after the probe with the operators above it
    /// (`PhysicalJoin::BuildJoinPipelines`).
    fn build_join(&mut self, op: &PhysicalRef, current: usize) -> Result<()> {
        let [left, right] = op.children() else {
            return Err(Error::internal(format!(
                "join {} must have two inputs",
                op.name()
            )));
        };
        let above = self.pipelines[current].operators.clone();
        self.pipelines[current].operators.push(Arc::clone(op));
        let build = self.add_pipeline(Pipeline::new(Arc::clone(op)));
        self.pipelines[current].dependencies.push(build);
        self.build(right, build)?;
        self.build(left, current)?;
        if op.as_source().is_some() {
            let sink = Arc::clone(&self.pipelines[current].sink);
            let outer = Pipeline {
                source: Some(Arc::clone(op)),
                operators: above,
                sink,
                dependencies: Vec::new(),
            };
            self.add_pipeline(outer);
        }
        Ok(())
    }

    fn run_pipeline(&mut self, index: usize) -> Result<()> {
        let mut ctx = ExecutionContext::new(Arc::clone(&self.env));
        ctx.profiler.clone_from(&self.profiler);
        let no_role = |name: &str| Error::internal(format!("operator {name} lacks its pipeline role"));
        let (source_op, sink_op, operators) = {
            let pipeline = &self.pipelines[index];
            let source = pipeline
                .source
                .clone()
                .ok_or_else(|| Error::internal("pipeline without a source"))?;
            (source, Arc::clone(&pipeline.sink), pipeline.operators.clone())
        };
        let source = source_op.as_source().ok_or_else(|| no_role(source_op.name()))?;
        let sink = sink_op.as_sink().ok_or_else(|| no_role(sink_op.name()))?;

        let source_global = source.global_source_state(&ctx, self.sink_states.get(&key(&source_op)))?;
        for op in &operators {
            if let std::collections::hash_map::Entry::Vacant(entry) = self.operator_globals.entry(key(op)) {
                let operator = op.as_operator().ok_or_else(|| no_role(op.name()))?;
                entry.insert(operator.global_operator_state(&ctx, self.sink_states.get(&key(op)))?);
            }
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = self.sink_globals.entry(key(&sink_op)) {
            entry.insert(sink.global_sink_state(&ctx)?);
        }

        let local = {
            let operator_globals = operators
                .iter()
                .map(|op| {
                    self.operator_globals
                        .get(&key(op))
                        .map(AsRef::as_ref)
                        .ok_or_else(|| Error::internal("operator state missing"))
                })
                .collect::<Result<Vec<_>>>()?;
            let sink_global = self
                .sink_globals
                .get(&key(&sink_op))
                .ok_or_else(|| Error::internal("sink state missing"))?;
            let states = PipelineStates {
                source_global: source_global.as_ref(),
                operator_globals,
                sink_global: sink_global.as_ref(),
            };
            let mut executor = PipelineExecutor::new(ctx.clone(), &self.pipelines[index], states)?;
            executor.run(source_op.types())?;
            std::mem::replace(&mut executor.sink_local, Box::new(()))
        };
        let sink_global = self
            .sink_globals
            .get_mut(&key(&sink_op))
            .ok_or_else(|| Error::internal("sink state missing"))?;
        sink.combine(&ctx, sink_global.as_ref(), local)?;

        let remaining = self.remaining.entry(key(&sink_op)).or_default();
        *remaining = remaining.saturating_sub(1);
        if *remaining == 0 {
            let mut sink_global = self
                .sink_globals
                .remove(&key(&sink_op))
                .ok_or_else(|| Error::internal("sink state missing"))?;
            // `NoOutputPossible` needs no special handling yet: dependent sources simply
            // observe an empty state.
            sink.finalize(&ctx, sink_global.as_mut())?;
            self.sink_states.insert(key(&sink_op), Arc::from(sink_global));
        }
        Ok(())
    }
}
