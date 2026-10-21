//! Splitting a physical plan into pipelines (DuckDB's `MetaPipeline`,
//! `PipelineBuildState` and the operators' `BuildPipelines`).
//!
//! A meta pipeline groups the pipelines that share a sink: its base pipeline, the extra
//! pipelines of `UNION ALL` branches, and the pipelines that scan a join's build side for
//! unmatched rows. A sink's input is built as a child meta pipeline, which completes
//! before the parent pipeline that reads the sink starts.

use std::collections::HashMap;
use std::sync::Arc;

use rustdb_common::{Error, Result};

use super::pipeline::{BATCH_INCREMENT, Pipeline, is_order_dependent};
use crate::operator::PhysicalRef;

/// What a meta pipeline computes (DuckDB's `MetaPipelineType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaPipelineType {
    Regular,
    /// The build side of a join.
    JoinBuild,
}

/// A pipeline while the plan is being split.
#[derive(Debug)]
struct PipelineBuild {
    source: Option<PhysicalRef>,
    /// Operators top-down while building; reversed by [`PipelineBuilder::finish`].
    operators: Vec<PhysicalRef>,
    sink: PhysicalRef,
    base_batch_index: u64,
    dependencies: Vec<usize>,
    intra_dependencies: Vec<usize>,
}

/// The pipelines that feed one sink (DuckDB's `MetaPipeline`).
#[derive(Debug)]
pub(crate) struct MetaPipeline {
    pub kind: MetaPipelineType,
    /// The pipelines, the base pipeline first.
    pub pipelines: Vec<usize>,
    /// The meta pipelines whose sinks this one's pipelines read.
    pub children: Vec<usize>,
    /// The pipeline reading this meta pipeline's sink (none for the root).
    pub parent: Option<usize>,
    next_batch_index: u64,
}

impl MetaPipeline {
    /// The base pipeline.
    pub fn base(&self) -> usize {
        self.pipelines[0]
    }
}

/// The pipelines and meta pipelines of a plan.
#[derive(Debug)]
pub(crate) struct PipelineGraph {
    pub pipelines: Vec<Arc<Pipeline>>,
    pub meta_pipelines: Vec<MetaPipeline>,
    /// The meta pipeline of the plan's root sink.
    pub root: usize,
}

impl PipelineGraph {
    /// The root meta pipeline and its descendants, in DuckDB's scheduling order
    /// (`GetMetaPipelines(recursive = true)`: each meta pipeline's children follow it,
    /// each child directly followed by its own descendants).
    pub fn meta_pipelines_in_order(&self) -> Vec<usize> {
        fn children(graph: &PipelineGraph, meta: usize, out: &mut Vec<usize>) {
            for &child in &graph.meta_pipelines[meta].children {
                out.push(child);
                children(graph, child, out);
            }
        }
        let mut out = vec![self.root];
        children(self, self.root, &mut out);
        out
    }
}

/// Builds the pipeline graph of a plan (DuckDB's `PipelineBuildState` plus the
/// `BuildPipelines` methods of the operators).
#[derive(Debug)]
pub(crate) struct PipelineBuilder {
    pipelines: Vec<PipelineBuild>,
    meta_pipelines: Vec<MetaPipeline>,
    /// The meta pipeline of every sink built so far, by operator address; scans of a
    /// sink's data elsewhere in the plan depend on it (`delim_join_dependencies`,
    /// `cte_dependencies`).
    sink_meta_pipelines: HashMap<usize, usize>,
    preserve_insertion_order: bool,
}

/// The address identifying an operator.
pub(crate) fn operator_key(op: &PhysicalRef) -> usize {
    Arc::as_ptr(op).cast::<()>() as usize
}

impl PipelineBuilder {
    /// Splits the plan below `root` (a sink, e.g. a result collector) into pipelines.
    pub fn build(root: &PhysicalRef, preserve_insertion_order: bool) -> Result<PipelineGraph> {
        let mut builder = Self {
            pipelines: Vec::new(),
            meta_pipelines: Vec::new(),
            sink_meta_pipelines: HashMap::new(),
            preserve_insertion_order,
        };
        let input = root
            .children()
            .first()
            .ok_or_else(|| Error::internal("the root operator of a plan must have an input"))?;
        let meta = builder.create_meta_pipeline(Arc::clone(root), MetaPipelineType::Regular, None);
        let base = builder.meta_pipelines[meta].base();
        builder.build_operator(input, base, meta)?;
        builder.finish(meta)
    }

    /// Freezes the pipelines: operators in execution order (`MetaPipeline::Ready`).
    fn finish(self, root: usize) -> Result<PipelineGraph> {
        let pipelines = self
            .pipelines
            .into_iter()
            .map(|mut build| {
                let source = build.source.ok_or_else(|| {
                    Error::internal(format!("pipeline into {} has no source", build.sink.name()))
                })?;
                build.operators.reverse();
                Ok(Arc::new(Pipeline::new(
                    source,
                    build.operators,
                    build.sink,
                    build.base_batch_index,
                    build.dependencies,
                    build.intra_dependencies,
                )))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(PipelineGraph {
            pipelines,
            meta_pipelines: self.meta_pipelines,
            root,
        })
    }

    /// A meta pipeline for `sink` with an empty base pipeline.
    fn create_meta_pipeline(
        &mut self,
        sink: PhysicalRef,
        kind: MetaPipelineType,
        parent: Option<usize>,
    ) -> usize {
        let id = self.meta_pipelines.len();
        self.sink_meta_pipelines.insert(operator_key(&sink), id);
        self.meta_pipelines.push(MetaPipeline {
            kind,
            pipelines: Vec::new(),
            children: Vec::new(),
            parent,
            next_batch_index: 0,
        });
        self.create_pipeline(id, sink);
        id
    }

    /// Adds a pipeline into the meta pipeline's sink (`MetaPipeline::CreatePipeline`).
    fn create_pipeline(&mut self, meta: usize, sink: PhysicalRef) -> usize {
        let id = self.pipelines.len();
        let meta_pipeline = &mut self.meta_pipelines[meta];
        let base_batch_index = BATCH_INCREMENT * meta_pipeline.next_batch_index;
        meta_pipeline.next_batch_index += 1;
        meta_pipeline.pipelines.push(id);
        self.pipelines.push(PipelineBuild {
            source: None,
            operators: Vec::new(),
            sink,
            base_batch_index,
            dependencies: Vec::new(),
            intra_dependencies: Vec::new(),
        });
        id
    }

    /// A child meta pipeline building `sink`'s input; `current` waits for it to complete
    /// (`CreateChildMetaPipeline`).
    fn create_child_meta_pipeline(
        &mut self,
        current: usize,
        meta: usize,
        sink: &PhysicalRef,
        kind: MetaPipelineType,
    ) -> usize {
        let child = self.create_meta_pipeline(Arc::clone(sink), kind, Some(current));
        self.meta_pipelines[meta].children.push(child);
        let base = self.meta_pipelines[child].base();
        self.pipelines[current].dependencies.push(base);
        child
    }

    /// Makes `dependant` wait for the pipelines of its meta pipeline created after
    /// `start` (and `start` itself if `including`) to push all their input
    /// (`AddDependenciesFrom`).
    fn add_dependencies_from(
        &mut self,
        meta: usize,
        dependant: usize,
        start: usize,
        including: bool,
    ) -> Result<()> {
        let pipelines = &self.meta_pipelines[meta].pipelines;
        let position = pipelines
            .iter()
            .position(|&pipeline| pipeline == start)
            .ok_or_else(|| Error::internal("pipeline not found in its meta pipeline"))?;
        let from = if including { position } else { position + 1 };
        let created: Vec<usize> = pipelines[from..]
            .iter()
            .copied()
            .filter(|&pipeline| pipeline != dependant)
            .collect();
        self.pipelines[dependant].intra_dependencies.extend(created);
        Ok(())
    }

    /// `MetaPipeline::CreateUnionPipeline`: a pipeline with the operators collected so far
    /// in `current` and the same dependencies.
    fn create_union_pipeline(&mut self, meta: usize, current: usize, order_matters: bool) -> usize {
        let sink = Arc::clone(&self.pipelines[current].sink);
        let union = self.create_pipeline(meta, sink);
        let (operators, dependencies, intra_dependencies) = {
            let current = &self.pipelines[current];
            (
                current.operators.clone(),
                current.dependencies.clone(),
                current.intra_dependencies.clone(),
            )
        };
        let pipeline = &mut self.pipelines[union];
        // The batch index is assigned when the branch is built.
        pipeline.base_batch_index = 0;
        pipeline.operators = operators;
        pipeline.dependencies = dependencies;
        pipeline.intra_dependencies = intra_dependencies;
        if order_matters {
            pipeline.intra_dependencies.push(current);
        }
        union
    }

    /// `MetaPipeline::CreateChildPipeline`: a pipeline into the same sink whose source is
    /// `op` (a join emitting unmatched build rows), with the operators above `op`. It runs
    /// after `current` and everything built since `last_pipeline`.
    fn create_child_pipeline(
        &mut self,
        meta: usize,
        current: usize,
        op: &PhysicalRef,
        last_pipeline: usize,
    ) -> Result<()> {
        let sink = Arc::clone(&self.pipelines[current].sink);
        let child = self.pipelines.len();
        let operators: Vec<PhysicalRef> = self.pipelines[current]
            .operators
            .iter()
            .take_while(|current_op| !Arc::ptr_eq(current_op, op))
            .cloned()
            .collect();
        self.meta_pipelines[meta].pipelines.push(child);
        self.pipelines.push(PipelineBuild {
            source: Some(Arc::clone(op)),
            operators,
            sink,
            base_batch_index: self.pipelines[current].base_batch_index,
            dependencies: Vec::new(),
            intra_dependencies: vec![current],
        });
        self.add_dependencies_from(meta, child, last_pipeline, false)
    }

    fn first_child<'a>(op: &'a PhysicalRef, role: &str) -> Result<&'a PhysicalRef> {
        op.children()
            .first()
            .ok_or_else(|| Error::internal(format!("{role} {} has no input", op.name())))
    }

    /// Adds `op` and its inputs to pipeline `current` of meta pipeline `meta`
    /// (`PhysicalOperator::BuildPipelines` and its overrides).
    fn build_operator(&mut self, op: &PhysicalRef, current: usize, meta: usize) -> Result<()> {
        if op.is_union() {
            return self.build_union(op, current, meta);
        }
        if op.is_cte() {
            // `PhysicalCTE::BuildPipelines`: the definition is materialised by a child
            // meta pipeline, the query continues the current pipeline.
            let [definition, query] = op.children() else {
                return Err(Error::internal("CTE must have two inputs"));
            };
            let key = operator_key(op);
            if let Some(&existing) = self.sink_meta_pipelines.get(&key) {
                let base = self.meta_pipelines[existing].base();
                self.pipelines[current].dependencies.push(base);
            } else {
                let child = self.create_child_meta_pipeline(current, meta, op, MetaPipelineType::Regular);
                let base = self.meta_pipelines[child].base();
                self.build_operator(definition, base, child)?;
            }
            return self.build_operator(query, current, meta);
        }
        if op.as_sink().is_some() && op.as_operator().is_some() {
            return self.build_join(op, current, meta);
        }
        if op.as_sink().is_some() && op.sinks_all_children() {
            // `PhysicalIEJoin::BuildPipelines`: one child meta pipeline sinks both inputs,
            // the right one first. The left pipeline waits for the right one, and the sink
            // is finalized once both are done.
            self.pipelines[current].source = Some(Arc::clone(op));
            let child = self.create_child_meta_pipeline(current, meta, op, MetaPipelineType::Regular);
            let [left, right] = op.children() else {
                return Err(Error::internal(format!("{} must have two inputs", op.name())));
            };
            let right_pipeline = self.meta_pipelines[child].base();
            self.build_operator(right, right_pipeline, child)?;
            let left_pipeline = self.create_pipeline(child, Arc::clone(op));
            self.add_dependencies_from(child, left_pipeline, right_pipeline, true)?;
            return self.build_operator(left, left_pipeline, child);
        }
        if op.as_sink().is_some() {
            // A sink ends a child pipeline and is the source of the current one.
            self.pipelines[current].source = Some(Arc::clone(op));
            let child = self.create_child_meta_pipeline(current, meta, op, MetaPipelineType::Regular);
            let base = self.meta_pipelines[child].base();
            return self.build_operator(Self::first_child(op, "sink")?, base, child);
        }
        if op.as_operator().is_some() {
            self.pipelines[current].operators.push(Arc::clone(op));
            return self.build_operator(Self::first_child(op, "operator")?, current, meta);
        }
        if op.as_source().is_some() {
            self.pipelines[current].source = Some(Arc::clone(op));
            // Scans of data materialised elsewhere in the plan (delim and CTE scans) wait
            // for the sink producing it (`PhysicalColumnDataScan::BuildPipelines`); a sink
            // not built yet is built as a child meta pipeline here.
            for dependency in op.source_dependencies() {
                let key = operator_key(&dependency);
                if let Some(&existing) = self.sink_meta_pipelines.get(&key) {
                    let base = self.meta_pipelines[existing].base();
                    self.pipelines[current].dependencies.push(base);
                    continue;
                }
                let child =
                    self.create_child_meta_pipeline(current, meta, &dependency, MetaPipelineType::Regular);
                let base = self.meta_pipelines[child].base();
                self.build_operator(Self::first_child(&dependency, "sink")?, base, child)?;
            }
            return Ok(());
        }
        Err(Error::internal(format!(
            "operator {} has no pipeline role",
            op.name()
        )))
    }

    /// `PhysicalJoin::BuildJoinPipelines`: the join probes in the current pipeline, its
    /// right input is built by a child meta pipeline, and a join that is also a source
    /// (outer joins emitting unmatched build rows) gets a pipeline that runs after the
    /// probe with the operators above it.
    fn build_join(&mut self, op: &PhysicalRef, current: usize, meta: usize) -> Result<()> {
        let [left, right] = op.children() else {
            return Err(Error::internal(format!(
                "join {} must have two inputs",
                op.name()
            )));
        };
        self.pipelines[current].operators.push(Arc::clone(op));
        let last_pipeline = *self.meta_pipelines[meta]
            .pipelines
            .last()
            .ok_or_else(|| Error::internal("meta pipeline without pipelines"))?;
        let child = self.create_child_meta_pipeline(current, meta, op, MetaPipelineType::JoinBuild);
        let base = self.meta_pipelines[child].base();
        self.build_operator(right, base, child)?;
        self.build_operator(left, current, meta)?;
        if op.as_source().is_some() {
            self.create_child_pipeline(meta, current, op, last_pipeline)?;
        }
        Ok(())
    }

    /// `PhysicalUnion::BuildPipelines`: every branch after the first runs in a union
    /// pipeline with the operators collected so far; if the order of the rows matters,
    /// each branch waits for the branches before it.
    fn build_union(&mut self, op: &PhysicalRef, current: usize, meta: usize) -> Result<()> {
        let children = op.children();
        if children.len() < 2 {
            return Err(Error::internal("UNION must have at least two inputs"));
        }
        let sink = Arc::clone(&self.pipelines[current].sink);
        let order_matters = is_order_dependent(
            self.pipelines[current].source.as_ref(),
            &self.pipelines[current].operators,
            Some(&sink),
            self.preserve_insertion_order,
        ) || sink.sink_order_dependent()
            || sink.required_partition_info().batch_index
            || !sink.parallel_sink();
        let union_pipelines: Vec<usize> = (1..children.len())
            .map(|_| self.create_union_pipeline(meta, current, order_matters))
            .collect();
        self.build_operator(&children[0], current, meta)?;
        for (i, child) in children.iter().enumerate().skip(1) {
            let union = union_pipelines[children.len() - i - 1];
            if order_matters {
                // The branch comes after all pipelines created by the branches before it.
                self.add_dependencies_from(meta, union, union, false)?;
            }
            // `AssignNextBatchIndex`.
            let meta_pipeline = &mut self.meta_pipelines[meta];
            self.pipelines[union].base_batch_index = meta_pipeline.next_batch_index * BATCH_INCREMENT;
            meta_pipeline.next_batch_index += 1;
            self.build_operator(child, union, meta)?;
        }
        Ok(())
    }
}
