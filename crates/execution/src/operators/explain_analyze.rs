//! `EXPLAIN ANALYZE` (DuckDB's `PhysicalExplainAnalyze` and `QueryProfiler::ToString`):
//! runs the plan, discarding its rows, then returns the plan annotated with the rows
//! and time of every operator.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::ExplainFormat;
use rustdb_common::Result;
use rustdb_common::tree_renderer::{RenderParams, RenderSource, RenderTree, RenderTreeNode, render};
use rustdb_planner::operator::renderer_kind;
use rustdb_types::{DataChunk, LogicalType, Value};

use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, QueryProfiler, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// The root of an `EXPLAIN ANALYZE` plan (DuckDB's `PhysicalExplainAnalyze`).
#[derive(Debug)]
pub struct PhysicalExplainAnalyze {
    types: Vec<LogicalType>,
    format: ExplainFormat,
    /// The statement's text, shown above the profile.
    query: String,
    children: [PhysicalRef; 1],
}

impl PhysicalExplainAnalyze {
    pub fn new(types: Vec<LogicalType>, format: ExplainFormat, query: String, child: PhysicalRef) -> Self {
        Self {
            types,
            format,
            query,
            children: [child],
        }
    }

    /// The profile as DuckDB's text output (`QueryProfiler::QueryTreeToStream`).
    fn render_profile(&self, profiler: &QueryProfiler) -> String {
        const TOTAL_BOX_WIDTH: usize = 50;
        let tree = RenderTree::create(&ProfileNode::Query { root: self, profiler });
        let plan = render(renderer_kind(self.format), tree);
        if !matches!(self.format, ExplainFormat::Default | ExplainFormat::Text) {
            return plan;
        }
        let mut out = String::new();
        out.push_str("┌─────────────────────────────────────┐\n");
        out.push_str("│┌───────────────────────────────────┐│\n");
        out.push_str("││    Query Profiling Information    ││\n");
        out.push_str("│└───────────────────────────────────┘│\n");
        out.push_str("└─────────────────────────────────────┘\n");
        out.push_str(&self.query.replace('\n', " "));
        out.push('\n');
        out.push_str("┌────────────────────────────────────────────────┐\n");
        out.push_str("│┌──────────────────────────────────────────────┐│\n");
        let total_time = format!("Total Time: {}", render_timing(profiler.elapsed().as_secs_f64()));
        let _ = writeln!(out, "││{}││", draw_padded(&total_time, TOTAL_BOX_WIDTH - 4));
        out.push_str("│└──────────────────────────────────────────────┘│\n");
        out.push_str("└────────────────────────────────────────────────┘\n");
        out.push_str(&plan);
        out
    }
}

/// A timing in seconds as DuckDB prints it (`RenderTiming`).
fn render_timing(seconds: f64) -> String {
    if seconds >= 1.0 {
        format!("{seconds:.2}s")
    } else if seconds >= 0.1 {
        format!("{seconds:.3}s")
    } else {
        format!("{seconds:.4}s")
    }
}

/// `text` centered in `width` columns (DuckDB's `QueryProfiler::DrawPadded`).
fn draw_padded(text: &str, width: usize) -> String {
    let length = text.chars().count();
    if length > width {
        return text.chars().take(width).collect();
    }
    let spaces = width - length;
    let half = spaces / 2;
    let extra = spaces % 2;
    format!("{}{text}{}", " ".repeat(half + extra), " ".repeat(half))
}

/// A node of the profiled plan (DuckDB's `ProfilingNode`): the query root, then the
/// physical operators.
#[derive(Clone, Copy)]
enum ProfileNode<'a> {
    Query {
        root: &'a PhysicalExplainAnalyze,
        profiler: &'a QueryProfiler,
    },
    Operator {
        op: &'a dyn PhysicalOperator,
        profiler: &'a QueryProfiler,
    },
}

impl RenderSource for ProfileNode<'_> {
    fn render_node(&self) -> RenderTreeNode {
        match *self {
            ProfileNode::Query { .. } => RenderTreeNode::new("QUERY".to_owned(), RenderParams::new()),
            ProfileNode::Operator { op, profiler } => {
                let mut profile = profiler.profile(op);
                if op.name() == "EXPLAIN_ANALYZE" {
                    // Its own result is produced after the profile is rendered.
                    profile.rows = 0;
                }
                let mut params = op.params_to_string(0);
                params.insert(RenderTreeNode::CARDINALITY.to_owned(), profile.rows.to_string());
                params.insert(
                    RenderTreeNode::TIMING.to_owned(),
                    format!("{:.2}s", profile.time.as_secs_f64()),
                );
                RenderTreeNode::new(op.profiler_name(), params)
            }
        }
    }

    fn render_children(&self) -> Vec<Self> {
        match *self {
            ProfileNode::Query { root, profiler } => vec![ProfileNode::Operator { op: root, profiler }],
            ProfileNode::Operator { op, profiler } => op
                .children()
                .iter()
                .map(|child| ProfileNode::Operator {
                    op: child.as_ref(),
                    profiler,
                })
                .collect(),
        }
    }
}

impl PhysicalOperator for PhysicalExplainAnalyze {
    fn name(&self) -> &'static str {
        "EXPLAIN_ANALYZE"
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

impl Sink for PhysicalExplainAnalyze {
    type Global = ();
    type Local = ();

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<()> {
        Ok(())
    }

    fn local_sink_state(&self, _: &ExecutionContext, (): &()) -> Result<()> {
        Ok(())
    }

    fn sink(&self, _: &ExecutionContext, _: &DataChunk, (): &(), (): &mut ()) -> Result<SinkResult> {
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, (): &(), (): ()) -> Result<()> {
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, (): &mut ()) -> Result<SinkFinalize> {
        Ok(SinkFinalize::Ready)
    }
}

impl Source for PhysicalExplainAnalyze {
    type Global = Mutex<bool>;
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<Mutex<bool>> {
        Ok(Mutex::new(false))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &Mutex<bool>) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &Mutex<bool>,
        (): &mut (),
    ) -> Result<SourceResult> {
        let mut done = global.lock().unwrap_or_else(PoisonError::into_inner);
        if *done {
            return Ok(SourceResult::Finished);
        }
        *done = true;
        let profiler = ctx
            .profiler
            .clone()
            .unwrap_or_else(|| Arc::new(QueryProfiler::new()));
        let text = self.render_profile(&profiler);
        let mut output = DataChunk::with_types(&self.types, 1);
        output
            .column_mut(0)
            .set_value(0, &Value::varchar("analyzed_plan"))?;
        output.column_mut(1).set_value(0, &Value::varchar(text))?;
        output.set_len(1);
        *chunk = output;
        Ok(SourceResult::Finished)
    }
}
