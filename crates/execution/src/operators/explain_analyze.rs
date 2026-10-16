//! `EXPLAIN ANALYZE` (DuckDB's `PhysicalExplainAnalyze` and `QueryProfiler::ToString`):
//! runs the plan, discarding its rows, then returns the plan annotated with the rows
//! and time of every operator.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::ExplainFormat;
use rustdb_common::Result;
use rustdb_common::tree_renderer::{RenderParams, RenderSource, RenderTree, RenderTreeNode, render};
use rustdb_planner::operator::renderer_kind;
use rustdb_types::{DataChunk, LogicalType, Value};

use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::plan_generator::explain::{DelimExplain, operator_key};
use crate::{ExecutionContext, QueryProfiler, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// The root of an `EXPLAIN ANALYZE` plan (DuckDB's `PhysicalExplainAnalyze`).
#[derive(Debug)]
pub struct PhysicalExplainAnalyze {
    types: Vec<LogicalType>,
    format: ExplainFormat,
    /// The statement's text, shown above the profile.
    query: String,
    children: [PhysicalRef; 1],
    /// How the delim joins of the plan are shown, by operator.
    delim_joins: HashMap<usize, DelimExplain>,
}

impl PhysicalExplainAnalyze {
    pub(crate) fn new(
        types: Vec<LogicalType>,
        format: ExplainFormat,
        query: String,
        child: PhysicalRef,
        delim_joins: HashMap<usize, DelimExplain>,
    ) -> Self {
        Self {
            types,
            format,
            query,
            children: [child],
            delim_joins,
        }
    }

    /// The profile as DuckDB's text output (`QueryProfiler::QueryTreeToStream`).
    fn render_profile(&self, profiler: &QueryProfiler) -> String {
        const TOTAL_BOX_WIDTH: usize = 50;
        if self.format == ExplainFormat::Json {
            return self.render_json(profiler);
        }
        let tree = RenderTree::create(&ProfileNode::Query {
            root: self,
            profiler,
            delim_joins: &self.delim_joins,
        });
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

impl PhysicalExplainAnalyze {
    /// The profile as DuckDB's JSON profiling output (`QueryProfiler::ToJSON`).
    fn render_json(&self, profiler: &QueryProfiler) -> String {
        let mut out = String::from("{\n");
        let cumulative = cumulative_cardinality(self, profiler);
        let _ = writeln!(out, "    \"latency\": {},", profiler.elapsed().as_secs_f64());
        let _ = writeln!(out, "    \"query_name\": \"{}\",", json_escape(&self.query));
        let _ = writeln!(out, "    \"cumulative_cardinality\": {cumulative},");
        out.push_str("    \"children\": [\n");
        render_json_operator(self, profiler, 2, &mut out);
        out.push_str("\n    ]\n}");
        out
    }
}

/// The rows an operator produced (none for `EXPLAIN_ANALYZE` itself, whose result is
/// produced after the profile is rendered).
fn operator_rows(op: &dyn PhysicalOperator, profiler: &QueryProfiler) -> u64 {
    if op.name() == "EXPLAIN_ANALYZE" {
        0
    } else {
        profiler.profile(op).rows
    }
}

/// The rows produced by `op` and every operator below it.
fn cumulative_cardinality(op: &dyn PhysicalOperator, profiler: &QueryProfiler) -> u64 {
    operator_rows(op, profiler)
        + op.children()
            .iter()
            .map(|child| cumulative_cardinality(child.as_ref(), profiler))
            .sum::<u64>()
}

/// Escapes text for a JSON string.
fn json_escape(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(result, "\\u{:04x}", u32::from(c));
            }
            c => result.push(c),
        }
    }
    result
}

/// One operator of the JSON profile, indented by `level` steps of four spaces.
fn render_json_operator(op: &dyn PhysicalOperator, profiler: &QueryProfiler, level: usize, out: &mut String) {
    let indent = "    ".repeat(level);
    let inner = "    ".repeat(level + 1);
    let profile = profiler.profile(op);
    let _ = writeln!(out, "{indent}{{");
    let _ = writeln!(
        out,
        "{inner}\"operator_type\": \"{}\",",
        json_escape(&op.profiler_name())
    );
    let _ = writeln!(out, "{inner}\"operator_timing\": {},", profile.time.as_secs_f64());
    let _ = writeln!(
        out,
        "{inner}\"operator_name\": \"{}\",",
        json_escape(&op.explain_name())
    );
    let params = op.params_to_string(0);
    let entries: Vec<String> = params
        .iter()
        .filter(|(key, _)| !key.starts_with("__"))
        .map(|(key, value)| format!("{inner}    \"{}\": \"{}\"", json_escape(key), json_escape(value)))
        .collect();
    if entries.is_empty() {
        let _ = writeln!(out, "{inner}\"extra_info\": {{}},");
    } else {
        let _ = writeln!(
            out,
            "{inner}\"extra_info\": {{\n{}\n{inner}}},",
            entries.join(",\n")
        );
    }
    let _ = writeln!(
        out,
        "{inner}\"operator_cardinality\": {},",
        operator_rows(op, profiler)
    );
    let _ = writeln!(
        out,
        "{inner}\"cumulative_cardinality\": {},",
        cumulative_cardinality(op, profiler)
    );
    let children = op.children();
    if children.is_empty() {
        let _ = writeln!(out, "{inner}\"children\": []");
    } else {
        let _ = writeln!(out, "{inner}\"children\": [");
        for (index, child) in children.iter().enumerate() {
            if index > 0 {
                out.push_str(",\n");
            }
            render_json_operator(child.as_ref(), profiler, level + 2, out);
        }
        let _ = writeln!(out, "\n{inner}]");
    }
    let _ = write!(out, "{indent}}}");
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
        delim_joins: &'a HashMap<usize, DelimExplain>,
    },
    /// An operator; `plain` shows a delim join's join as itself.
    Operator {
        op: &'a dyn PhysicalOperator,
        profiler: &'a QueryProfiler,
        delim_joins: &'a HashMap<usize, DelimExplain>,
        plain: bool,
    },
    /// The DISTINCT of a delim join.
    DelimDistinct { delim: &'a DelimExplain },
}

impl<'a> ProfileNode<'a> {
    const fn operator(
        op: &'a dyn PhysicalOperator,
        profiler: &'a QueryProfiler,
        delim_joins: &'a HashMap<usize, DelimExplain>,
    ) -> Self {
        Self::Operator {
            op,
            profiler,
            delim_joins,
            plain: false,
        }
    }
}

/// The rows and time lines of a profiled node.
fn add_profile(params: &mut RenderParams, rows: u64, seconds: f64) {
    params.insert(RenderTreeNode::CARDINALITY.to_owned(), rows.to_string());
    params.insert(RenderTreeNode::TIMING.to_owned(), format!("{seconds:.2}s"));
}

impl RenderSource for ProfileNode<'_> {
    fn render_node(&self) -> RenderTreeNode {
        match *self {
            ProfileNode::Query { .. } => RenderTreeNode::new("QUERY".to_owned(), RenderParams::new()),
            ProfileNode::Operator {
                op,
                profiler,
                delim_joins,
                plain,
            } => {
                if !plain && let Some(delim) = delim_joins.get(&operator_key(op)) {
                    // DuckDB's delim join: a sink producing no rows itself.
                    let mut params = op.params_to_string(0);
                    params.insert("Delim Index".to_owned(), delim.delim_index.to_string());
                    add_profile(&mut params, 0, 0.0);
                    return RenderTreeNode::new(delim.name.to_owned(), params);
                }
                let mut profile = profiler.profile(op);
                if op.name() == "EXPLAIN_ANALYZE" {
                    // Its own result is produced after the profile is rendered.
                    profile.rows = 0;
                }
                let mut params = op.params_to_string(0);
                add_profile(&mut params, profile.rows, profile.time.as_secs_f64());
                RenderTreeNode::new(op.profiler_name(), params)
            }
            ProfileNode::DelimDistinct { delim } => {
                let mut params = RenderParams::new();
                let groups: Vec<String> = delim
                    .distinct_groups
                    .iter()
                    .map(|index| format!("#{index}"))
                    .collect();
                params.insert("Groups".to_owned(), groups.join("\n"));
                add_profile(&mut params, 0, 0.0);
                RenderTreeNode::new("HASH_GROUP_BY".to_owned(), params)
            }
        }
    }

    fn render_children(&self) -> Vec<Self> {
        match *self {
            ProfileNode::Query {
                root,
                profiler,
                delim_joins,
            } => vec![ProfileNode::operator(root, profiler, delim_joins)],
            ProfileNode::Operator {
                op,
                profiler,
                delim_joins,
                plain,
            } => {
                if !plain && let Some(delim) = delim_joins.get(&operator_key(op)) {
                    // DuckDB's `PhysicalDelimJoin::GetChildren`.
                    return vec![
                        ProfileNode::operator(delim.input.as_ref(), profiler, delim_joins),
                        ProfileNode::Operator {
                            op,
                            profiler,
                            delim_joins,
                            plain: true,
                        },
                        ProfileNode::DelimDistinct { delim },
                    ];
                }
                op.children()
                    .iter()
                    .map(|child| ProfileNode::operator(child.as_ref(), profiler, delim_joins))
                    .collect()
            }
            ProfileNode::DelimDistinct { .. } => Vec::new(),
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
