//! `EXPLAIN ANALYZE` (DuckDB's `PhysicalExplainAnalyze` and `QueryProfiler::ToString`):
//! runs the plan, discarding its rows, then returns the plan annotated with the rows
//! and time of every operator.

use rustdb_common::{ByteString, byte_format, byte_write};
use std::collections::HashMap;
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
    query: ByteString,
    client: Option<Arc<rustdb_function::FunctionClient>>,
    children: [PhysicalRef; 1],
    /// How the delim joins of the plan are shown, by operator.
    delim_joins: HashMap<usize, DelimExplain>,
}

impl PhysicalExplainAnalyze {
    pub(crate) fn new(
        types: Vec<LogicalType>,
        format: ExplainFormat,
        query: ByteString,
        child: PhysicalRef,
        delim_joins: HashMap<usize, DelimExplain>,
    ) -> Self {
        Self {
            types,
            format,
            query,
            client: rustdb_function::file_system::opener(),
            children: [child],
            delim_joins,
        }
    }

    /// The profile as DuckDB's text output (`QueryProfiler::QueryTreeToStream`).
    fn render_profile(&self, profiler: &QueryProfiler) -> Result<ByteString> {
        const TOTAL_BOX_WIDTH: usize = 50;
        // Without a format, the connection's profiling format applies
        // (`QueryProfiler::GetPrintFormat`).
        if self.format == ExplainFormat::Json
            || (self.format == ExplainFormat::Default && profiler.json_by_default())
        {
            return self.render_json(profiler);
        }
        let tree = RenderTree::create(&ProfileNode::Query {
            root: self,
            profiler,
            delim_joins: &self.delim_joins,
        });
        let plan = render(renderer_kind(self.format), tree)?;
        if !matches!(self.format, ExplainFormat::Default | ExplainFormat::Text) {
            return Ok(plan);
        }
        let mut out = ByteString::new();
        out.push_bytes("┌─────────────────────────────────────┐\n");
        out.push_bytes("│┌───────────────────────────────────┐│\n");
        out.push_bytes("││    Query Profiling Information    ││\n");
        out.push_bytes("│└───────────────────────────────────┘│\n");
        out.push_bytes("└─────────────────────────────────────┘\n");
        out.push_bytes(self.query.replace_byte(b'\n', b" "));
        out.push(b'\n');
        out.push_bytes("┌────────────────────────────────────────────────┐\n");
        out.push_bytes("│┌──────────────────────────────────────────────┐│\n");
        let total_time = format!("Total Time: {}", render_timing(profiler.elapsed().as_secs_f64()));
        let _ = byte_write!(out, "││{}││\n", draw_padded(&total_time, TOTAL_BOX_WIDTH - 4));
        out.push_bytes("│└──────────────────────────────────────────────┘│\n");
        out.push_bytes("└────────────────────────────────────────────────┘\n");
        if let Some(client) = &self.client {
            out.push_bytes(client.http_stats().render());
        }
        out.push_bytes(&plan);
        Ok(out)
    }
}

impl PhysicalExplainAnalyze {
    /// The profile as DuckDB's JSON profiling output (`QueryProfiler::ToJSON`).
    fn render_json(&self, profiler: &QueryProfiler) -> Result<ByteString> {
        let mut out = ByteString::from("{\n");
        let cumulative = cumulative_cardinality(self, profiler);
        let _ = byte_write!(out, "    \"latency\": {},\n", profiler.elapsed().as_secs_f64());
        let _ = byte_write!(out, "    \"query_name\": \"{}\",\n", json_escape(&self.query)?);
        let _ = byte_write!(out, "    \"cumulative_cardinality\": {cumulative},\n");
        out.push_bytes("    \"children\": [\n");
        render_json_operator(self, profiler, 2, &mut out)?;
        out.push_bytes("\n    ]\n}");
        Ok(out)
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
fn json_escape(text: impl AsRef<[u8]>) -> Result<ByteString> {
    // Profiling stores strings as Values before yyjson writes its C-string view.
    let value = Value::try_varchar(text.as_ref())?;
    let bytes = value.as_bytes().unwrap_or_default();
    let bytes = bytes.split(|byte| *byte == 0).next().unwrap_or_default();
    let quoted = rustdb_common::string_util::json_quote_bytes(bytes);
    Ok(quoted[1..quoted.len() - 1].into())
}

/// One operator of the JSON profile, indented by `level` steps of four spaces.
fn render_json_operator(
    op: &dyn PhysicalOperator,
    profiler: &QueryProfiler,
    level: usize,
    out: &mut ByteString,
) -> Result<()> {
    let indent = "    ".repeat(level);
    let inner = "    ".repeat(level + 1);
    let profile = profiler.profile(op);
    let _ = byte_write!(out, "{indent}{{\n");
    let _ = byte_write!(
        out,
        "{inner}\"operator_type\": \"{}\",\n",
        json_escape(op.profiler_name())?
    );
    let _ = byte_write!(
        out,
        "{inner}\"operator_timing\": {},\n",
        profile.time.as_secs_f64()
    );
    let _ = byte_write!(
        out,
        "{inner}\"operator_name\": \"{}\",\n",
        json_escape(op.explain_name())?
    );
    let mut params = op.params_to_string(0);
    for (key, value) in profiler.extra_info(op) {
        params.insert(key, value);
    }
    let entries: Vec<ByteString> = params
        .iter()
        .filter(|(key, _)| !key.starts_with(b"__"))
        .map(|(key, value)| {
            Ok(byte_format!(
                "{inner}    \"{}\": \"{}\"",
                json_escape(key)?,
                json_escape(value)?
            ))
        })
        .collect::<Result<_>>()?;
    if entries.is_empty() {
        let _ = byte_write!(out, "{inner}\"extra_info\": {{}},\n");
    } else {
        let _ = byte_write!(
            out,
            "{inner}\"extra_info\": {{\n{}\n{inner}}},\n",
            ByteString::join(&entries, b",\n")
        );
    }
    let _ = byte_write!(
        out,
        "{inner}\"operator_cardinality\": {},\n",
        operator_rows(op, profiler)
    );
    let _ = byte_write!(
        out,
        "{inner}\"cumulative_cardinality\": {},\n",
        cumulative_cardinality(op, profiler)
    );
    let children = op.children();
    if children.is_empty() {
        let _ = byte_write!(out, "{inner}\"children\": []\n");
    } else {
        let _ = byte_write!(out, "{inner}\"children\": [\n");
        for (index, child) in children.iter().enumerate() {
            if index > 0 {
                out.push_bytes(",\n");
            }
            render_json_operator(child.as_ref(), profiler, level + 2, out)?;
        }
        let _ = byte_write!(out, "\n{inner}]\n");
    }
    let _ = byte_write!(out, "{indent}}}");
    Ok(())
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
                for (key, value) in profiler.extra_info(op) {
                    params.insert(key, value);
                }
                add_profile(&mut params, profile.rows, profile.time.as_secs_f64());
                RenderTreeNode::new(op.profiler_name(), params)
            }
            ProfileNode::DelimDistinct { delim } => {
                let mut params = RenderParams::new();
                let groups: Vec<ByteString> = delim
                    .distinct_groups
                    .iter()
                    .map(|index| byte_format!("#{index}"))
                    .collect();
                params.insert("Groups".to_owned(), ByteString::join(&groups, b"\n"));
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
        let text = self.render_profile(&profiler)?;
        let mut output = DataChunk::with_types(&self.types, 1);
        output
            .column_mut(0)
            .set_value(0, &Value::varchar("analyzed_plan"))?;
        output.column_mut(1).set_value(0, &Value::try_varchar(&text)?)?;
        output.set_len(1);
        *chunk = output;
        Ok(SourceResult::Finished)
    }
}
