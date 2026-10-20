//! The connection's query profiler (DuckDB's `QueryProfiler`, `ProfilingInfo`,
//! `ProfilingNode` and `ProfilingUtils`, and the profiling options of `ClientConfig`
//! with their `custom_settings.cpp` callbacks).
//!
//! When profiling is enabled, every statement's profile is built from the physical plan
//! that ran: a node per operator with the metrics the settings enable, the query root
//! carrying the query-wide ones (latency, planner and optimizer timings, storage
//! latencies, bytes read and written, ...). The profile is written after the statement
//! in the configured format, to the configured file or the terminal.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Instant;

use rustdb_common::profiling::{MetricGroup, MetricType, QueryMetrics};
use rustdb_common::tree_renderer::{RenderParams, RenderSource, RenderTree, RenderTreeNode, RendererKind};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_execution::{PhysicalOperator, PhysicalRef, operator_key};
use rustdb_function::logging::{ClientLogger, LogLevel, LogScope};
use rustdb_optimizer::OptimizerType;
use rustdb_serde::json::{JsonValue, parse_json, write_json};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

// ------------------------------------------------------------------------------------
// Configuration
// ------------------------------------------------------------------------------------

/// How profiles are printed (DuckDB's `ProfilerPrintFormat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProfilerPrintFormat {
    QueryTree,
    Json,
    QueryTreeOptimizer,
    NoOutput,
    Html,
    Graphviz,
    Mermaid,
}

impl ProfilerPrintFormat {
    /// Every format with its name (`EnumUtil::ToString`).
    const ALL: [(Self, &'static str); 7] = [
        (Self::QueryTree, "QUERY_TREE"),
        (Self::Json, "JSON"),
        (Self::QueryTreeOptimizer, "QUERY_TREE_OPTIMIZER"),
        (Self::NoOutput, "NO_OUTPUT"),
        (Self::Html, "HTML"),
        (Self::Graphviz, "GRAPHVIZ"),
        (Self::Mermaid, "MERMAID"),
    ];

    fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(format, _)| *format == self)
            .map_or("QUERY_TREE", |(_, name)| name)
    }

    /// `EnumUtil::FromString<ProfilerPrintFormat>` (case-insensitive).
    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, candidate)| candidate.eq_ignore_ascii_case(name))
            .map(|(format, _)| *format)
    }
}

/// Which statements are profiled (DuckDB's `ProfilingCoverage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProfilingCoverage {
    Select,
    All,
}

/// The type the metrics were last given in (DuckDB's `profiler_settings_type`), which
/// `custom_profiling_settings` reads back as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetricsInputType {
    Json,
    List,
    Struct,
}

/// The profiling options of a connection (the profiler fields of DuckDB's
/// `ClientConfig`).
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools, reason = "mirrors DuckDB's ClientConfig flags")]
pub(crate) struct ProfilerConfig {
    /// `enable_profiler`.
    pub enable_profiler: bool,
    /// `enable_detailed_profiling`.
    pub enable_detailed_profiling: bool,
    /// `profiler_print_format`.
    pub print_format: ProfilerPrintFormat,
    /// `profiler_save_location`: the file profiles are written to, or empty for the
    /// terminal.
    pub save_location: String,
    /// `profiler_settings`: the enabled metrics.
    pub metrics: BTreeSet<MetricType>,
    metrics_type: MetricsInputType,
    /// `emit_profiler_output`.
    pub emit_output: bool,
    /// `profiling_coverage`.
    pub coverage: ProfilingCoverage,
}

impl Default for ProfilerConfig {
    fn default() -> Self {
        Self {
            enable_profiler: false,
            enable_detailed_profiling: false,
            print_format: ProfilerPrintFormat::QueryTree,
            save_location: String::new(),
            metrics: MetricGroup::Default.metrics().into_iter().collect(),
            metrics_type: MetricsInputType::Json,
            emit_output: true,
            coverage: ProfilingCoverage::Select,
        }
    }
}

/// `FileSystem::ExtractExtension`: the text after the last `.` of the file name.
fn extract_extension(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((_, extension)) => extension,
        None => "",
    }
}

/// The optimizer metrics and phase timings (what `query_tree_optimizer` and `detailed`
/// add to the enabled metrics).
fn optimizer_and_phase_metrics() -> impl Iterator<Item = MetricType> {
    MetricGroup::Optimizer
        .metrics()
        .into_iter()
        .chain(MetricGroup::PhaseTiming.metrics())
}

/// Whether the pass of optimizer metric `metric` is not disabled (`IsEnabledOptimizer`).
fn is_enabled_optimizer(metric: MetricType, disabled: &BTreeSet<OptimizerType>) -> bool {
    metric
        .optimizer_name()
        .and_then(|name| OptimizerType::from_name(&name).ok())
        .is_some_and(|optimizer| !disabled.contains(&optimizer))
}

impl ProfilerConfig {
    /// `PRAGMA enable_profiling` (`PragmaEnableProfilingStatement`).
    pub fn enable(&mut self) {
        self.enable_profiler = true;
        self.emit_output = true;
    }

    /// `PRAGMA disable_profiling` (`PragmaDisableProfiling`).
    pub fn disable(&mut self) {
        self.enable_profiler = false;
    }

    /// `EnableProfilingSetting::SetLocal`.
    pub fn set_enable_profiling(&mut self, value: &Value) -> Result<()> {
        let parameter = value.to_varchar()?.to_lowercase();
        self.enable_profiler = true;
        self.emit_output = true;
        if parameter != "no_output" && !self.save_location.is_empty() {
            let file_type = extract_extension(&self.save_location);
            if file_type != parameter && file_type != "txt" {
                return Err(Error::parser(format!(
                    "Profiler file type ({}) must either have the same file extension as the profiling output type \
                     ({parameter}), or be a '.txt' file. Set 'profiling_output' to a '{parameter}' file or run \"RESET \
                     profiling_output\" first.",
                    self.save_location
                )));
            }
        }
        self.print_format = match parameter.as_str() {
            "json" => ProfilerPrintFormat::Json,
            "query_tree" => ProfilerPrintFormat::QueryTree,
            "query_tree_optimizer" => {
                self.metrics.extend(optimizer_and_phase_metrics());
                ProfilerPrintFormat::QueryTreeOptimizer
            }
            "no_output" => {
                self.emit_output = false;
                ProfilerPrintFormat::NoOutput
            }
            "html" => ProfilerPrintFormat::Html,
            "graphviz" => ProfilerPrintFormat::Graphviz,
            _ => {
                return Err(Error::parser(format!(
                    "Unrecognized print format {parameter}, supported formats: [json, query_tree, \
                     query_tree_optimizer, no_output, html, graphviz]"
                )));
            }
        };
        Ok(())
    }

    /// `EnableProfilingSetting::ResetLocal`.
    pub fn reset_enable_profiling(&mut self) {
        let defaults = Self::default();
        self.print_format = defaults.print_format;
        self.enable_profiler = defaults.enable_profiler;
        self.emit_output = defaults.emit_output;
        self.metrics = defaults.metrics;
    }

    /// `EnableProfilingSetting::GetSetting`.
    pub fn enable_profiling_value(&self) -> Value {
        if !self.enable_profiler {
            return Value::null(LogicalType::VARCHAR);
        }
        Value::varchar(self.print_format.name().to_lowercase())
    }

    /// `ProfileOutputSetting::SetLocal`.
    pub fn set_save_location(&mut self, value: &Value) -> Result<()> {
        let parameter = value.to_varchar()?;
        if !parameter.is_empty() && self.print_format != ProfilerPrintFormat::NoOutput {
            let file_type = extract_extension(&parameter);
            if file_type != "txt" && ProfilerPrintFormat::from_name(file_type).is_none() {
                return Err(Error::parser(format!("Invalid output file type: {file_type}")));
            }
            let printer_format = self.print_format.name().to_lowercase();
            if file_type != printer_format && file_type != "txt" {
                return Err(Error::parser(format!(
                    "Profiler file type ({parameter}) must either have the same file extension as the profiling \
                     output type ({printer_format}), or be a '.txt' file. Set \"enable_profiling = '{file_type}'\" \
                     first."
                )));
            }
        }
        self.save_location = parameter;
        Ok(())
    }

    /// `ProfileOutputSetting::ResetLocal`.
    pub fn reset_save_location(&mut self) {
        self.save_location.clear();
    }

    /// `ProfilingModeSetting::SetLocal`.
    pub fn set_mode(&mut self, value: &Value) -> Result<()> {
        let parameter = value.to_varchar()?.to_lowercase();
        match parameter.as_str() {
            "standard" => {
                self.enable_profiler = true;
                self.enable_detailed_profiling = false;
            }
            "detailed" => {
                self.enable_profiler = true;
                self.enable_detailed_profiling = true;
                self.metrics.extend(optimizer_and_phase_metrics());
            }
            "all" => {
                self.enable_profiler = true;
                self.metrics.extend(MetricType::ALL.iter().copied());
            }
            _ => {
                return Err(Error::parser(format!(
                    "Unrecognized profiling mode \"{parameter}\", supported formats: [standard, detailed]"
                )));
            }
        }
        Ok(())
    }

    /// `ProfilingModeSetting::ResetLocal`.
    pub fn reset_mode(&mut self) {
        let defaults = Self::default();
        self.enable_profiler = defaults.enable_profiler;
        self.enable_detailed_profiling = defaults.enable_detailed_profiling;
        self.emit_output = defaults.emit_output;
        self.metrics = defaults.metrics;
    }

    /// `ProfilingModeSetting::GetSetting`.
    pub fn mode_value(&self) -> Value {
        if !self.enable_profiler {
            return Value::null(LogicalType::VARCHAR);
        }
        Value::varchar(if self.enable_detailed_profiling {
            "detailed"
        } else {
            "standard"
        })
    }

    /// `ProfilingCoverageSetting::SetLocal`.
    pub fn set_coverage(&mut self, value: &Value) -> Result<()> {
        let text = value.to_varchar()?;
        self.coverage = if text.eq_ignore_ascii_case("SELECT") {
            ProfilingCoverage::Select
        } else if text.eq_ignore_ascii_case("ALL") {
            ProfilingCoverage::All
        } else {
            return Err(crate::settings::enum_value_error(
                &text,
                "ProfilingCoverage",
                &["SELECT", "ALL"],
            ));
        };
        Ok(())
    }

    /// `ProfilingCoverageSetting::ResetLocal`.
    pub fn reset_coverage(&mut self) {
        self.coverage = ProfilingCoverage::Select;
    }

    /// `ProfilingCoverageSetting::GetSetting`.
    pub fn coverage_value(&self) -> Value {
        Value::varchar(match self.coverage {
            ProfilingCoverage::Select => "SELECT",
            ProfilingCoverage::All => "ALL",
        })
    }

    /// `CustomProfilingSettingsSetting::SetLocal`: the metrics come as a JSON object
    /// (entries with the value `"true"`), a list of names or a struct of booleans; names
    /// may be metrics or metric groups. Metrics of disabled optimizers are left out.
    pub fn set_custom_settings(&mut self, value: &Value, disabled: &BTreeSet<OptimizerType>) -> Result<()> {
        let ty = value.logical_type();
        let (names, input_type) = if ty.id() == LogicalTypeId::List
            && ty
                .child_type()
                .is_some_and(|child| child.id() == LogicalTypeId::Varchar)
        {
            let names = value
                .children()
                .unwrap_or_default()
                .iter()
                .map(Value::to_varchar)
                .collect::<Result<Vec<_>>>()?;
            (names, MetricsInputType::List)
        } else if ty.id() == LogicalTypeId::Struct {
            let fields = ty.struct_children().unwrap_or_default();
            let mut names = Vec::new();
            for ((name, _), child) in fields.iter().zip(value.children().unwrap_or_default()) {
                let enabled = if child.logical_type().id() == LogicalTypeId::Boolean {
                    child.as_bool() == Some(true)
                } else {
                    !child.is_null() && child.to_varchar()?.eq_ignore_ascii_case("true")
                };
                if enabled {
                    names.push(name.clone());
                }
            }
            (names, MetricsInputType::Struct)
        } else if ty.id() == LogicalTypeId::Varchar {
            (parse_metrics_json(&value.to_varchar()?)?, MetricsInputType::Json)
        } else {
            return Err(Error::parser(format!(
                "Invalid custom profiler settings type \"{ty}\", expected LIST(VARCHAR) or JSON"
            )));
        };
        let mut metrics = BTreeSet::new();
        let mut invalid = Vec::new();
        let mut insert_if_enabled = |metric: MetricType| {
            if !metric.is_optimizer() || is_enabled_optimizer(metric, disabled) {
                metrics.insert(metric);
            }
        };
        for name in names {
            let upper = name.to_uppercase();
            if let Some(metric) = MetricType::from_name(&upper) {
                insert_if_enabled(metric);
            } else if let Some(group) = MetricGroup::from_name(&upper) {
                for metric in group.metrics() {
                    insert_if_enabled(metric);
                }
            } else {
                invalid.push(name);
            }
        }
        if !invalid.is_empty() {
            return Err(Error::new(
                ErrorKind::Io,
                format!("Invalid custom profiler settings: \"{}\"", invalid.join(", ")),
            ));
        }
        // `AddOptimizerMetrics`.
        if metrics.contains(&MetricType::AllOptimizers) {
            for metric in MetricGroup::Optimizer.metrics() {
                if is_enabled_optimizer(metric, disabled) {
                    metrics.insert(metric);
                }
            }
        }
        self.metrics_type = input_type;
        self.enable_profiler = true;
        self.metrics = metrics;
        Ok(())
    }

    /// `CustomProfilingSettingsSetting::ResetLocal`.
    pub fn reset_custom_settings(&mut self) {
        let defaults = Self::default();
        self.enable_profiler = defaults.enable_profiler;
        self.metrics = defaults.metrics;
        self.metrics_type = MetricsInputType::Json;
    }

    /// `CustomProfilingSettingsSetting::GetSetting`: the metrics in the type they were
    /// given in, sorted by name.
    pub fn custom_settings_value(&self) -> Value {
        let mut names: Vec<&str> = self.metrics.iter().map(|metric| metric.name()).collect();
        names.sort_unstable();
        match self.metrics_type {
            MetricsInputType::Json => {
                let entries: Vec<String> = names.iter().map(|name| format!("\"{name}\": \"true\"")).collect();
                Value::varchar(format!("{{{}}}", entries.join(", ")))
            }
            MetricsInputType::Struct => Value::struct_value(
                names
                    .iter()
                    .map(|name| ((*name).to_owned(), Value::boolean(true)))
                    .collect(),
            ),
            MetricsInputType::List => Value::list(
                LogicalType::VARCHAR,
                names.iter().map(Value::varchar).collect(),
            ),
        }
    }
}

/// The names of the entries of a JSON object whose value is `"true"`
/// (`StringUtil::ParseJSONMap(...)->Flatten()` in `ExtractFromJSON`).
fn parse_metrics_json(text: &str) -> Result<Vec<String>> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let parse_error = || {
        Error::new(
            ErrorKind::Io,
            format!(
                "Could not parse the custom profiler settings file due to incorrect JSON: \"{text}\".  Make sure all \
                 the keys and values start with a quote. (error: Failed to parse JSON string: {text})"
            ),
        )
    };
    let Ok(JsonValue::Object(entries)) = parse_json(text) else {
        return Err(parse_error());
    };
    let mut names: Vec<String> = Vec::new();
    for (name, value) in entries {
        let flattened = match value {
            JsonValue::Str(text) => text,
            JsonValue::Bool(true) => "true".to_owned(),
            _ => continue,
        };
        // A repeated key keeps its last value (`ComplexJSON::AddObjectEntry`).
        names.retain(|existing| *existing != name);
        if flattened.eq_ignore_ascii_case("true") {
            names.push(name);
        }
    }
    Ok(names)
}

// ------------------------------------------------------------------------------------
// The profiling tree
// ------------------------------------------------------------------------------------

/// A metric's value (DuckDB keeps them as `Value`s).
#[derive(Clone, Debug, PartialEq)]
enum MetricValue {
    Double(f64),
    UBigint(u64),
    Text(String),
    /// `EXTRA_INFO`: key-value lines.
    Map(Vec<(String, String)>),
}

impl MetricValue {
    fn as_f64(&self) -> f64 {
        match self {
            Self::Double(value) => *value,
            Self::UBigint(value) => *value as f64,
            _ => 0.0,
        }
    }

    fn as_u64(&self) -> u64 {
        match self {
            Self::UBigint(value) => *value,
            Self::Double(value) => *value as u64,
            _ => 0,
        }
    }

    /// `Value::ToString` of the metric.
    fn to_value(&self) -> Value {
        match self {
            Self::Double(value) => Value::double(*value),
            Self::UBigint(value) => Value::ubigint(*value),
            Self::Text(text) => Value::varchar(text),
            Self::Map(entries) => Value::map(
                LogicalType::VARCHAR,
                LogicalType::VARCHAR,
                entries
                    .iter()
                    .map(|(key, value)| (Value::varchar(key), Value::varchar(value)))
                    .collect(),
            ),
        }
    }
}

/// Whether a metric holds seconds (the `DOUBLE` metrics of `ProfilingUtils`).
fn is_timing(metric: MetricType) -> bool {
    use MetricType as M;
    metric.is_optimizer()
        || metric.is_phase_timing()
        || matches!(
            metric,
            M::AttachLoadStorageLatency
                | M::AttachReplayWalLatency
                | M::BlockedThreadTime
                | M::CheckpointLatency
                | M::CommitLocalStorageLatency
                | M::CpuTime
                | M::CumulativeVacuumTime
                | M::Latency
                | M::OperatorTiming
                | M::WaitingToAttachLatency
                | M::WriteToWalLatency
        )
}

/// `ProfilingUtils::SetMetricToDefault`.
fn default_metric(metric: MetricType) -> MetricValue {
    use MetricType as M;
    match metric {
        M::ExtraInfo => MetricValue::Map(Vec::new()),
        M::OperatorName | M::QueryName => MetricValue::Text(String::new()),
        M::OperatorType => MetricValue::Text("INVALID".to_owned()),
        _ if is_timing(metric) => MetricValue::Double(0.0),
        _ => MetricValue::UBigint(0),
    }
}

/// The metrics of one node (DuckDB's `ProfilingInfo`).
#[derive(Clone, Debug, Default)]
struct ProfilingInfo {
    /// The metrics the node shows.
    settings: BTreeSet<MetricType>,
    /// The metrics collected, which include those the shown ones are computed from.
    expanded_settings: BTreeSet<MetricType>,
    metrics: BTreeMap<MetricType, MetricValue>,
}

impl ProfilingInfo {
    /// `ProfilingInfo(settings, depth)`: operators show the operator metrics, the root
    /// the query-wide ones.
    fn new(settings: &BTreeSet<MetricType>, depth: usize) -> Self {
        let mut settings = settings.clone();
        if depth > 0 {
            settings.insert(MetricType::OperatorName);
            settings.insert(MetricType::OperatorType);
        }
        let mut expanded_settings = BTreeSet::new();
        for &metric in &settings {
            Self::expand(&mut expanded_settings, metric);
        }
        if depth == 0 {
            settings.retain(|metric| !metric.is_operator());
        } else {
            settings.retain(|metric| !metric.is_root_scope());
        }
        let metrics = expanded_settings
            .iter()
            .map(|&metric| (metric, default_metric(metric)))
            .collect();
        Self {
            settings,
            expanded_settings,
            metrics,
        }
    }

    /// `ProfilingInfo::Expand`: a metric and those it is computed from.
    fn expand(settings: &mut BTreeSet<MetricType>, metric: MetricType) {
        use MetricType as M;
        settings.insert(metric);
        match metric {
            M::CpuTime => {
                settings.insert(M::OperatorTiming);
            }
            M::CumulativeCardinality => {
                settings.insert(M::OperatorCardinality);
            }
            M::CumulativeRowsScanned => {
                settings.insert(M::OperatorRowsScanned);
            }
            M::CumulativeRowGroupsScanned => {
                settings.insert(M::OperatorRowGroupsScanned);
            }
            M::CumulativeTotalRowGroupsToScan => {
                settings.insert(M::OperatorTotalRowGroupsToScan);
            }
            M::CumulativeOptimizerTiming | M::AllOptimizers => {
                settings.extend(MetricGroup::Optimizer.metrics());
            }
            _ => {}
        }
    }

    fn collects(&self, metric: MetricType) -> bool {
        self.expanded_settings.contains(&metric)
    }

    fn shows(&self, metric: MetricType) -> bool {
        self.settings.contains(&metric)
    }

    /// `ProfilingInfo::MetricSum`.
    fn add_u64(&mut self, metric: MetricType, amount: u64) {
        let entry = self.metrics.entry(metric).or_insert(MetricValue::UBigint(0));
        *entry = MetricValue::UBigint(entry.as_u64() + amount);
    }

    fn add_f64(&mut self, metric: MetricType, amount: f64) {
        let entry = self.metrics.entry(metric).or_insert(MetricValue::Double(0.0));
        *entry = MetricValue::Double(entry.as_f64() + amount);
    }

    fn get(&self, metric: MetricType) -> MetricValue {
        self.metrics
            .get(&metric)
            .cloned()
            .unwrap_or_else(|| default_metric(metric))
    }

    /// `ProfilingInfo::WriteMetricsToJSON`.
    fn write_json(&self, out: &mut Vec<(String, JsonValue)>) {
        for &metric in &self.settings {
            let key = metric.name().to_lowercase();
            let value = match self.get(metric) {
                MetricValue::Map(entries) => JsonValue::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| {
                            let lines: Vec<&str> = value.split('\n').collect();
                            let value = if lines.len() > 1 {
                                JsonValue::Array(lines.into_iter().map(|line| JsonValue::Str(line.to_owned())).collect())
                            } else {
                                JsonValue::Str(value)
                            };
                            (key, value)
                        })
                        .collect(),
                ),
                MetricValue::Text(text) => JsonValue::Str(text),
                MetricValue::Double(value) => JsonValue::Real(value),
                MetricValue::UBigint(value) if is_timing(metric) => JsonValue::Real(value as f64),
                MetricValue::UBigint(value) => JsonValue::UInt(value),
            };
            out.push((key, value));
        }
    }
}

/// A node of the profile (DuckDB's `ProfilingNode`): the query root, then an operator
/// per physical operator.
#[derive(Clone, Debug)]
struct ProfilingNode {
    info: ProfilingInfo,
    depth: usize,
    /// The physical operator the node profiles ([`operator_key`]).
    operator: Option<usize>,
    children: Vec<ProfilingNode>,
}

/// `JSONSanitize`: keys like `__estimated_cardinality__` read `Estimated Cardinality`.
fn sanitize_key(key: &str) -> String {
    if !key.starts_with("__") {
        return key.to_owned();
    }
    let key = key.replace("__", "").replace('_', " ");
    // `StringUtil::Title`.
    let mut result = String::with_capacity(key.len());
    let mut capitalize = true;
    for c in key.chars() {
        if c.is_ascii_alphabetic() {
            if capitalize {
                result.push(c.to_ascii_uppercase());
            } else {
                result.push(c.to_ascii_lowercase());
            }
            capitalize = false;
        } else {
            result.push(c);
            capitalize = true;
        }
    }
    result
}

impl ProfilingNode {
    /// `ToJSONRecursive`.
    fn to_json(&self) -> JsonValue {
        let mut entries = Vec::new();
        let mut info = self.info.clone();
        if let Some(MetricValue::Map(extra)) = info.metrics.get_mut(&MetricType::ExtraInfo) {
            for (key, _) in extra.iter_mut() {
                *key = sanitize_key(key);
            }
        }
        info.write_json(&mut entries);
        entries.push((
            "children".to_owned(),
            JsonValue::Array(self.children.iter().map(Self::to_json).collect()),
        ));
        JsonValue::Object(entries)
    }

    /// `QueryProfiler::Finalize`: a UNION produces the rows of its children.
    fn finalize_unions(&mut self) {
        for index in 0..self.children.len() {
            self.children[index].finalize_unions();
            if self.info.get(MetricType::OperatorType) == MetricValue::Text("UNION".to_owned())
                && self.info.collects(MetricType::OperatorCardinality)
            {
                let rows = self.children[index]
                    .info
                    .get(MetricType::OperatorCardinality)
                    .as_u64();
                self.info.add_u64(MetricType::OperatorCardinality, rows);
            }
        }
    }

    /// `AggregateMetric` with a sum: `cumulative` of each node is its `metric` plus the
    /// `cumulative` of its children.
    fn cumulative(&mut self, cumulative: MetricType, metric: MetricType) {
        let own = self.info.get(metric);
        let mut total = own;
        for child in &mut self.children {
            child.cumulative(cumulative, metric);
            let child_total = child.info.get(cumulative);
            total = match (total, child_total) {
                (MetricValue::Double(a), b) => MetricValue::Double(a + b.as_f64()),
                (a, b) => MetricValue::UBigint(a.as_u64() + b.as_u64()),
            };
        }
        self.info.metrics.insert(cumulative, total);
    }
}

/// Renders the profile as a tree (DuckDB's `RenderTree::CreateRenderTree(ProfilingNode)`).
impl RenderSource for &ProfilingNode {
    fn render_node(&self) -> RenderTreeNode {
        let info = &self.info;
        let mut extra_text = RenderParams::new();
        if info.shows(MetricType::ExtraInfo)
            && let MetricValue::Map(entries) = info.get(MetricType::ExtraInfo)
        {
            for (key, value) in entries {
                extra_text.insert(key, value);
            }
        }
        let name = if self.depth > 0 {
            match info.get(MetricType::OperatorType) {
                MetricValue::Text(name) => name,
                _ => String::new(),
            }
        } else {
            "QUERY".to_owned()
        };
        if info.shows(MetricType::OperatorCardinality) {
            extra_text.insert(
                RenderTreeNode::CARDINALITY.to_owned(),
                info.get(MetricType::OperatorCardinality).as_u64().to_string(),
            );
        }
        if info.shows(MetricType::OperatorTiming) {
            extra_text.insert(
                RenderTreeNode::TIMING.to_owned(),
                format!("{:.2}s", info.get(MetricType::OperatorTiming).as_f64()),
            );
        }
        RenderTreeNode::new(name, extra_text)
    }

    fn render_children(&self) -> Vec<Self> {
        self.children.iter().collect()
    }
}

/// `OperatorRequiresProfiling` with `profiling_coverage = SELECT`: operators of queries.
fn operator_requires_profiling(name: &str) -> bool {
    matches!(
        name,
        "ORDER_BY"
            | "RESERVOIR_SAMPLE"
            | "STREAMING_SAMPLE"
            | "LIMIT"
            | "LIMIT_PERCENT"
            | "STREAMING_LIMIT"
            | "TOP_N"
            | "WINDOW"
            | "UNNEST"
            | "UNGROUPED_AGGREGATE"
            | "HASH_GROUP_BY"
            | "FILTER"
            | "PROJECTION"
            | "COPY_TO_FILE"
            | "TABLE_SCAN"
            | "CHUNK_SCAN"
            | "DELIM_SCAN"
            | "EXPRESSION_SCAN"
            | "BLOCKWISE_NL_JOIN"
            | "NESTED_LOOP_JOIN"
            | "HASH_JOIN"
            | "CROSS_PRODUCT"
            | "PIECEWISE_MERGE_JOIN"
            | "IE_JOIN"
            | "LEFT_DELIM_JOIN"
            | "RIGHT_DELIM_JOIN"
            | "UNION"
            | "RECURSIVE_CTE"
            | "RECURSIVE_KEY_CTE"
            | "EMPTY_RESULT"
            | "EXTENSION"
    )
}

/// `EraseQueryRootSettings`: what the root's children collect.
fn erase_query_root_settings(settings: &BTreeSet<MetricType>) -> BTreeSet<MetricType> {
    settings
        .iter()
        .copied()
        .filter(|metric| !(metric.is_optimizer() || metric.is_phase_timing() || metric.is_root_scope()))
        .collect()
}

// ------------------------------------------------------------------------------------
// The query profiler
// ------------------------------------------------------------------------------------

/// The profiler of a connection (DuckDB's `QueryProfiler`).
#[derive(Debug, Default)]
pub(crate) struct QueryProfiler {
    /// Whether a query is being profiled.
    running: bool,
    /// Whether the statement is `EXPLAIN ANALYZE`, which profiles it regardless of the
    /// settings and renders the profile itself.
    is_explain_analyze: bool,
    query_requires_profiling: bool,
    metrics_finalized: bool,
    query_name: String,
    latency: Option<Instant>,
    /// The counters the storage records into while the query runs.
    query_metrics: Arc<QueryMetrics>,
    /// Query-wide metrics the operators report (`query_global_info`).
    global_info: BTreeMap<MetricType, MetricValue>,
    /// Seconds per phase (`phase_timings`) and the phases running (`phase_stack`).
    phase_timings: HashMap<MetricType, f64>,
    phase_stack: Vec<MetricType>,
    phase_start: Option<Instant>,
    root: Option<ProfilingNode>,
}

impl QueryProfiler {
    /// `QueryProfiler::IsEnabled`.
    fn is_enabled(&self, config: &ProfilerConfig) -> bool {
        self.is_explain_analyze || config.enable_profiler
    }

    /// `QueryProfiler::IsDetailedEnabled`.
    fn is_detailed_enabled(&self, config: &ProfilerConfig) -> bool {
        !self.is_explain_analyze && config.enable_detailed_profiling
    }

    /// `QueryProfiler::GetPrintFormat` of the default explain format.
    fn print_format(config: &ProfilerConfig) -> ProfilerPrintFormat {
        if config.print_format == ProfilerPrintFormat::NoOutput {
            ProfilerPrintFormat::QueryTree
        } else {
            config.print_format
        }
    }

    /// `QueryProfiler::PrintOptimizerOutput`.
    fn print_optimizer_output(&self, config: &ProfilerConfig) -> bool {
        Self::print_format(config) == ProfilerPrintFormat::QueryTreeOptimizer || self.is_detailed_enabled(config)
    }

    /// Whether the statement is being profiled.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The counters storage records into, while a query is profiled.
    pub fn active_metrics(&self) -> Option<&Arc<QueryMetrics>> {
        self.running.then_some(&self.query_metrics)
    }

    /// `QueryProfiler::StartQuery`.
    pub fn start_query(&mut self, config: &ProfilerConfig, query: &str, is_explain_analyze: bool) {
        if is_explain_analyze {
            self.is_explain_analyze = true;
        }
        if !self.is_enabled(config) || self.running {
            return;
        }
        // `QueryProfiler::Start` after `Reset`.
        self.root = None;
        self.phase_timings.clear();
        self.phase_stack.clear();
        self.global_info.clear();
        self.query_metrics = Arc::new(QueryMetrics::default());
        self.metrics_finalized = false;
        self.running = true;
        self.query_name = query.to_owned();
        self.latency = Some(Instant::now());
    }

    /// `QueryProfiler::StartPhase`.
    pub fn start_phase(&mut self, config: &ProfilerConfig, phase: MetricType) {
        if !self.is_enabled(config) || !self.running {
            return;
        }
        self.charge_phases();
        self.phase_stack.push(phase);
        self.phase_start = Some(Instant::now());
    }

    /// `QueryProfiler::EndPhase`.
    pub fn end_phase(&mut self, config: &ProfilerConfig) {
        if !self.is_enabled(config) || !self.running {
            return;
        }
        self.charge_phases();
        self.phase_stack.pop();
        self.phase_start = (!self.phase_stack.is_empty()).then(Instant::now);
    }

    /// Adds the time since the phase timer started to every running phase.
    fn charge_phases(&mut self) {
        if let Some(start) = self.phase_start.take() {
            let elapsed = start.elapsed().as_secs_f64();
            for phase in &self.phase_stack {
                *self.phase_timings.entry(*phase).or_default() += elapsed;
            }
        }
    }

    /// Records the time of one optimizer pass (`RunOptimizer`'s phase).
    pub fn add_optimizer_timing(&mut self, config: &ProfilerConfig, optimizer: OptimizerType, seconds: f64) {
        if !self.is_enabled(config) || !self.running {
            return;
        }
        if let Some(metric) = MetricType::optimizer_metric(optimizer.name()) {
            *self.phase_timings.entry(metric).or_default() += seconds;
        }
    }

    /// `QueryProfiler::Initialize`: the profile of the physical plan rooted at `root`;
    /// a plan without an operator that requires profiling is not profiled.
    pub fn initialize(&mut self, config: &ProfilerConfig, root: &PhysicalRef) {
        if !self.is_enabled(config) || !self.running {
            return;
        }
        self.query_requires_profiling = false;
        let tree = self.create_tree(config, root.as_ref(), &config.metrics, 0);
        self.finish_initialize(tree);
    }

    /// [`Self::initialize`] for a statement the connection runs itself: the result
    /// collector over the statement's operator, of type `operator_type`.
    pub fn initialize_statement(&mut self, config: &ProfilerConfig, operator_type: &str) {
        if !self.is_enabled(config) || !self.running {
            return;
        }
        self.query_requires_profiling = config.coverage == ProfilingCoverage::All;
        let mut root = Self::node(&config.metrics, 0, None, "RESULT_COLLECTOR", "RESULT_COLLECTOR", RenderParams::new());
        let child_settings = erase_query_root_settings(&config.metrics);
        root.children
            .push(Self::node(&child_settings, 1, None, operator_type, operator_type, RenderParams::new()));
        self.finish_initialize(root);
    }

    fn finish_initialize(&mut self, root: ProfilingNode) {
        if self.query_requires_profiling {
            self.root = Some(root);
        } else {
            // The query does not require profiling: disable profiling for it.
            self.running = false;
            self.root = None;
            self.phase_timings.clear();
            self.phase_stack.clear();
        }
    }

    /// A node with the given operator metrics.
    fn node(
        settings: &BTreeSet<MetricType>,
        depth: usize,
        operator: Option<usize>,
        name: &str,
        operator_type: &str,
        params: RenderParams,
    ) -> ProfilingNode {
        let mut info = ProfilingInfo::new(settings, depth);
        if depth != 0 {
            info.metrics
                .insert(MetricType::OperatorName, MetricValue::Text(name.to_owned()));
            info.metrics
                .insert(MetricType::OperatorType, MetricValue::Text(operator_type.to_owned()));
        }
        if info.shows(MetricType::ExtraInfo) {
            // The estimated cardinality is not known to the physical operators.
            let entries = params
                .iter()
                .filter(|(key, _)| key.as_str() != RenderTreeNode::ESTIMATED_CARDINALITY)
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            info.metrics.insert(MetricType::ExtraInfo, MetricValue::Map(entries));
        }
        ProfilingNode {
            info,
            depth,
            operator,
            children: Vec::new(),
        }
    }

    /// `QueryProfiler::CreateTree`.
    fn create_tree(
        &mut self,
        config: &ProfilerConfig,
        op: &dyn PhysicalOperator,
        settings: &BTreeSet<MetricType>,
        depth: usize,
    ) -> ProfilingNode {
        let operator_type = op.profiler_name();
        if config.coverage == ProfilingCoverage::All || operator_requires_profiling(&operator_type) {
            self.query_requires_profiling = true;
        }
        let child_settings = if depth == 0 {
            erase_query_root_settings(settings)
        } else {
            settings.clone()
        };
        let mut node = Self::node(
            settings,
            depth,
            Some(operator_key(op)),
            &op.explain_name(),
            &operator_type,
            op.params_to_string(0),
        );
        for child in op.children() {
            let child = self.create_tree(config, child.as_ref(), &child_settings, depth + 1);
            node.children.push(child);
        }
        node
    }

    /// `QueryProfiler::Flush`: adds what the operators did to their nodes.
    pub fn flush(&mut self, config: &ProfilerConfig, profiler: &rustdb_execution::QueryProfiler) {
        if !self.is_enabled(config) || !self.running {
            return;
        }
        let Some(root) = &mut self.root else {
            return;
        };
        for (metric, value) in [
            (MetricType::SystemPeakBufferMemory, profiler.peak_memory()),
            (MetricType::SystemPeakTempDirSize, profiler.peak_temp_dir_size()),
        ] {
            if root.info.collects(metric) {
                let entry = self.global_info.entry(metric).or_insert(MetricValue::UBigint(0));
                *entry = MetricValue::UBigint(entry.as_u64().max(value));
            }
        }
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if let Some(key) = node.operator {
                Self::flush_node(&mut node.info, key, profiler);
            }
            stack.extend(node.children.iter_mut());
        }
    }

    fn flush_node(info: &mut ProfilingInfo, key: usize, profiler: &rustdb_execution::QueryProfiler) {
        let profile = profiler.profile_by_key(key);
        if info.collects(MetricType::OperatorTiming) {
            info.add_f64(MetricType::OperatorTiming, profile.time.as_secs_f64());
        }
        if info.collects(MetricType::OperatorCardinality) {
            info.add_u64(MetricType::OperatorCardinality, profile.rows);
        }
        if info.collects(MetricType::ResultSetSize) {
            info.add_u64(MetricType::ResultSetSize, profile.result_set_size);
        }
        if let Some(scan) = profiler.scan_by_key(key) {
            if info.collects(MetricType::OperatorRowsScanned)
                && let Some(rows) = scan.rows_scanned
            {
                info.add_u64(MetricType::OperatorRowsScanned, rows);
            }
            if info.collects(MetricType::OperatorRowGroupsScanned)
                && let Some(row_groups) = scan.row_groups_scanned
            {
                info.add_u64(MetricType::OperatorRowGroupsScanned, row_groups);
            }
            if info.collects(MetricType::OperatorTotalRowGroupsToScan)
                && let Some(total) = scan.total_row_groups
            {
                info.metrics
                    .insert(MetricType::OperatorTotalRowGroupsToScan, MetricValue::UBigint(total));
            }
        }
        if info.collects(MetricType::ExtraInfo) {
            let reported = profiler.extra_info_by_key(key);
            if !reported.is_empty()
                && let Some(MetricValue::Map(entries)) = info.metrics.get_mut(&MetricType::ExtraInfo)
            {
                // `MergeOperatorExtraInfo`.
                for (name, value) in reported {
                    match entries.iter_mut().find(|(existing, _)| *existing == name) {
                        Some(entry) => entry.1 = value,
                        None => entries.push((name, value)),
                    }
                }
            }
        }
    }

    /// `QueryProfiler::SetBlockedTime`: our tasks never block, so threads spent no time
    /// blocked.
    fn set_blocked_time(&mut self) {
        let Some(root) = &self.root else {
            return;
        };
        if root.info.collects(MetricType::BlockedThreadTime) {
            self.global_info
                .insert(MetricType::BlockedThreadTime, MetricValue::Double(0.0));
        }
    }

    /// `QueryProfiler::FinalizeMetricsInternal`: the query-wide metrics of the root.
    fn finalize_metrics(&mut self, config: &ProfilerConfig) {
        if self.metrics_finalized || !self.is_enabled(config) || self.root.is_none() {
            return;
        }
        let latency = self.latency.take().map_or(0, |start| {
            u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
        });
        self.query_metrics.update(MetricType::Latency, latency);
        self.set_blocked_time();
        let Some(root) = &mut self.root else {
            return;
        };
        if root.info.collects(MetricType::OperatorCardinality)
            && let Some(child) = root.children.first_mut()
        {
            child.finalize_unions();
        }
        for (metric, value) in &self.global_info {
            root.info.metrics.insert(*metric, value.clone());
        }
        // `MoveOptimizerPhasesToRoot`.
        for (phase, timing) in &self.phase_timings {
            if root.info.collects(*phase) {
                root.info.metrics.insert(*phase, MetricValue::Double(*timing));
            }
        }
        let child_info = root
            .children
            .first()
            .map(|child| child.info.clone())
            .unwrap_or_default();
        let metrics: Vec<MetricType> = root.info.metrics.keys().copied().collect();
        for metric in metrics {
            if !root.info.collects(metric) {
                continue;
            }
            Self::collect_metric(metric, &self.query_metrics, &self.query_name, root, &child_info);
        }
        self.metrics_finalized = true;
    }

    /// `ProfilingUtils::CollectMetrics`.
    fn collect_metric(
        metric: MetricType,
        query_metrics: &QueryMetrics,
        query_name: &str,
        root: &mut ProfilingNode,
        child_info: &ProfilingInfo,
    ) {
        use MetricType as M;
        let value = match metric {
            M::CpuTime => return root.cumulative(M::CpuTime, M::OperatorTiming),
            M::CumulativeCardinality => return root.cumulative(M::CumulativeCardinality, M::OperatorCardinality),
            M::CumulativeRowsScanned => return root.cumulative(M::CumulativeRowsScanned, M::OperatorRowsScanned),
            M::CumulativeRowGroupsScanned => {
                return root.cumulative(M::CumulativeRowGroupsScanned, M::OperatorRowGroupsScanned);
            }
            M::CumulativeTotalRowGroupsToScan => {
                return root.cumulative(M::CumulativeTotalRowGroupsToScan, M::OperatorTotalRowGroupsToScan);
            }
            M::AttachLoadStorageLatency
            | M::AttachReplayWalLatency
            | M::CheckpointLatency
            | M::CommitLocalStorageLatency
            | M::CumulativeVacuumTime
            | M::Latency
            | M::WaitingToAttachLatency
            | M::WriteToWalLatency => MetricValue::Double(query_metrics.seconds(metric)),
            M::TotalBytesRead | M::TotalBytesWritten | M::TotalMemoryAllocated | M::WalReplayEntryCount => {
                MetricValue::UBigint(query_metrics.value(metric))
            }
            M::QueryName => MetricValue::Text(query_name.to_owned()),
            M::ResultSetSize => child_info.get(M::ResultSetSize),
            M::RowsReturned => child_info.get(M::OperatorCardinality),
            M::CumulativeOptimizerTiming => MetricValue::Double(
                root.info
                    .metrics
                    .iter()
                    .filter(|(metric, _)| metric.is_optimizer())
                    .map(|(_, value)| value.as_f64())
                    .sum(),
            ),
            _ => return,
        };
        root.info.metrics.insert(metric, value);
    }

    /// The profile in the default explain format, for `EXPLAIN ANALYZE`'s default.
    pub fn prints_json(config: &ProfilerConfig) -> bool {
        Self::print_format(config) == ProfilerPrintFormat::Json
    }

    /// `QueryProfiler::EndQuery`: finalizes the profile, logs its metrics and writes it
    /// (`EXPLAIN ANALYZE` renders its own).
    pub fn end_query(&mut self, config: &ProfilerConfig, logger: &dyn ClientLogger) -> Result<()> {
        if !self.is_enabled(config) || !self.running {
            return Ok(());
        }
        self.finalize_metrics(config);
        self.running = false;
        let emit_output = !self.is_explain_analyze && config.emit_output;
        let save_location = if self.is_explain_analyze {
            String::new()
        } else {
            config.save_location.clone()
        };
        self.is_explain_analyze = false;
        self.to_log(logger);
        if emit_output {
            let tree = self.to_string(config);
            if save_location.is_empty() {
                // `Printer::Print` writes to the terminal's error stream.
                eprintln!("{tree}\n");
            } else {
                // `WriteToFile`: the file is created anew.
                std::fs::write(&save_location, tree).map_err(|error| {
                    Error::new(
                        ErrorKind::Io,
                        format!("Cannot open file \"{save_location}\": {error}"),
                    )
                })?;
            }
        }
        Ok(())
    }

    /// `QueryProfiler::ToLog`: the root's metrics as `Metrics` log entries.
    fn to_log(&self, logger: &dyn ClientLogger) {
        let Some(root) = &self.root else {
            return;
        };
        if !logger.should_log(LogScope::Connection, "Metrics", LogLevel::Info) {
            return;
        }
        for &metric in &root.info.settings {
            let entry = Value::struct_value(vec![
                ("metric".to_owned(), Value::varchar(metric.name())),
                (
                    "value".to_owned(),
                    Value::varchar(root.info.get(metric).to_value().to_varchar().unwrap_or_default()),
                ),
            ]);
            if let Ok(message) = entry.to_varchar() {
                let _ = logger.write_log(LogScope::Connection, "Metrics", LogLevel::Info, &message);
            }
        }
    }

    /// `QueryProfiler::ToString` in the configured format.
    fn to_string(&self, config: &ProfilerConfig) -> String {
        match Self::print_format(config) {
            ProfilerPrintFormat::QueryTree | ProfilerPrintFormat::QueryTreeOptimizer => self.query_tree(config),
            ProfilerPrintFormat::Json => self.to_json(),
            ProfilerPrintFormat::NoOutput => String::new(),
            format => {
                let Some(root) = &self.root else {
                    return String::new();
                };
                if self.query_name.is_empty() {
                    return String::new();
                }
                let kind = match format {
                    ProfilerPrintFormat::Html => RendererKind::Html,
                    ProfilerPrintFormat::Graphviz => RendererKind::Graphviz,
                    _ => RendererKind::Mermaid,
                };
                rustdb_common::tree_renderer::render(kind, RenderTree::create(&root))
            }
        }
    }

    /// `QueryProfiler::ToJSON`.
    fn to_json(&self) -> String {
        let Some(root) = &self.root else {
            let result = if self.query_name.is_empty() { "empty" } else { "error" };
            return write_json(
                &JsonValue::Object(vec![("result".to_owned(), JsonValue::Str(result.to_owned()))]),
                true,
            );
        };
        let mut entries = Vec::new();
        root.info.write_json(&mut entries);
        let children = root.children.first().map(ProfilingNode::to_json);
        entries.push(("children".to_owned(), JsonValue::Array(children.into_iter().collect())));
        write_json(&JsonValue::Object(entries), true)
    }

    /// `QueryProfiler::QueryTreeToStream`.
    fn query_tree(&self, config: &ProfilerConfig) -> String {
        const TOTAL_BOX_WIDTH: usize = 50;
        let mut out = String::new();
        let show_query_name = self
            .root
            .as_ref()
            .is_some_and(|root| root.info.shows(MetricType::QueryName));
        out.push_str("┌─────────────────────────────────────┐\n");
        out.push_str("│┌───────────────────────────────────┐│\n");
        out.push_str("││    Query Profiling Information    ││\n");
        out.push_str("│└───────────────────────────────────┘│\n");
        out.push_str("└─────────────────────────────────────┘\n");
        if show_query_name {
            out.push_str(&self.query_name.replace('\n', " "));
        }
        out.push('\n');
        if self.query_name.is_empty() && self.root.is_none() {
            return out;
        }
        out.push_str("┌────────────────────────────────────────────────┐\n");
        out.push_str("│┌──────────────────────────────────────────────┐│\n");
        let total_time = format!(
            "Total Time: {}",
            render_timing(self.query_metrics.seconds(MetricType::Latency))
        );
        out.push_str(&format!("││{}││\n", draw_padded(&total_time, TOTAL_BOX_WIDTH - 4)));
        out.push_str("│└──────────────────────────────────────────────┘│\n");
        out.push_str("└────────────────────────────────────────────────┘\n");
        if let Some(root) = &self.root {
            if self.print_optimizer_output(config) {
                print_phase_timings(&mut out, &root.info, TOTAL_BOX_WIDTH);
            }
            let mut renderer = rustdb_common::tree_renderer::TextTreeRenderer::default();
            if self.is_detailed_enabled(config) {
                renderer.max_extra_lines = 1000;
            }
            out.push_str(&renderer.render(&RenderTree::create(&root)));
        }
        out
    }
}

/// `RenderTiming`.
fn render_timing(seconds: f64) -> String {
    if seconds >= 1.0 {
        format!("{seconds:.2}s")
    } else if seconds >= 0.1 {
        format!("{seconds:.3}s")
    } else {
        format!("{seconds:.4}s")
    }
}

/// `QueryProfiler::DrawPadded`.
fn draw_padded(text: &str, width: usize) -> String {
    let length = text.chars().count();
    if length > width {
        return text.chars().take(width).collect();
    }
    let spaces = width - length;
    let half = spaces / 2;
    format!("{}{text}{}", " ".repeat(half + spaces % 2), " ".repeat(half))
}

/// `RenderTitleCase`: `PHYSICAL_PLANNER` reads `Physical Planner`.
fn render_title_case(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut capitalize = true;
    for c in text.to_lowercase().chars() {
        if c == '_' {
            result.push(' ');
            capitalize = true;
        } else if capitalize {
            result.push(c.to_ascii_uppercase());
            capitalize = false;
        } else {
            result.push(c);
        }
    }
    result
}

/// `RenderPhaseTimings`.
fn render_phase_timings(out: &mut String, head: &(String, f64), timings: &BTreeMap<String, f64>, width: usize) {
    out.push_str("┌────────────────────────────────────────────────┐\n");
    out.push_str(&format!(
        "│{}│\n",
        draw_padded(
            &format!("{}: {}", render_title_case(&head.0), render_timing(head.1)),
            width - 2
        )
    ));
    out.push_str("│┌──────────────────────────────────────────────┐│\n");
    for (name, timing) in timings {
        out.push_str(&format!(
            "││{}││\n",
            draw_padded(
                &format!("{}: {}", render_title_case(name), render_timing(*timing)),
                width - 4
            )
        ));
    }
    out.push_str("│└──────────────────────────────────────────────┘│\n");
    out.push_str("└────────────────────────────────────────────────┘\n");
}

/// `PrintPhaseTimingsToStream`: the optimizer, physical planner and planner timings.
fn print_phase_timings(out: &mut String, info: &ProfilingInfo, width: usize) {
    let mut optimizer = BTreeMap::new();
    let mut planner = BTreeMap::new();
    let mut physical_planner = BTreeMap::new();
    let mut optimizer_head = (String::new(), 0.0);
    let mut planner_head = (String::new(), 0.0);
    let mut physical_planner_head = (String::new(), 0.0);
    for (&metric, value) in &info.metrics {
        let name = metric.name();
        if metric.is_optimizer() {
            optimizer.insert(name["OPTIMIZER_".len()..].to_owned(), value.as_f64());
        } else if metric.is_phase_timing() {
            match metric {
                MetricType::CumulativeOptimizerTiming => continue,
                MetricType::AllOptimizers => optimizer_head = ("Optimizer".to_owned(), value.as_f64()),
                MetricType::PhysicalPlanner => {
                    physical_planner_head = ("Physical Planner".to_owned(), value.as_f64());
                }
                MetricType::Planner => planner_head = ("Planner".to_owned(), value.as_f64()),
                _ => {}
            }
            if let Some(rest) = name.strip_prefix("PHYSICAL_PLANNER_") {
                physical_planner.insert(rest.to_owned(), value.as_f64());
            } else if let Some(rest) = name.strip_prefix("PLANNER_") {
                planner.insert(rest.to_owned(), value.as_f64());
            }
        }
    }
    render_phase_timings(out, &optimizer_head, &optimizer, width);
    render_phase_timings(out, &physical_planner_head, &physical_planner, width);
    render_phase_timings(out, &planner_head, &planner, width);
}

// ------------------------------------------------------------------------------------
// enable_profiling / disable_profiling
// ------------------------------------------------------------------------------------

impl ProfilerConfig {
    /// `EnableProfiling`: enables profiling with the options of an
    /// `enable_profiling(...)` call.
    pub fn apply_enable_profiling(
        &mut self,
        options: &ProfilingOptions<'_>,
        disabled: &BTreeSet<OptimizerType>,
    ) -> Result<()> {
        self.enable_profiler = true;
        self.emit_output = true;
        if let (Some(format), Some(save_location)) = (options.format, options.save_location) {
            let file_type = extract_extension(save_location);
            if file_type != "txt" && file_type != format {
                return Err(Error::invalid_input(
                    "EnableProfiling: the save_location must be a .txt file or match the specified format.",
                ));
            }
            self.reset_enable_profiling();
            self.reset_save_location();
        }
        if let Some(format) = options.format {
            self.set_enable_profiling(&Value::varchar(format))?;
        }
        if let Some(coverage) = options.coverage {
            self.set_coverage(&Value::varchar(coverage))?;
        }
        if let Some(save_location) = options.save_location {
            self.set_save_location(&Value::varchar(save_location))?;
        }
        if let Some(mode) = options.mode {
            self.set_mode(&Value::varchar(mode))?;
        }
        if let Some(metrics) = options.metrics {
            self.set_custom_settings(metrics, disabled)?;
        }
        Ok(())
    }

    /// `DisableProfiling`.
    pub fn apply_disable_profiling(&mut self) {
        self.enable_profiler = false;
        self.emit_output = false;
    }
}

/// The options of an `enable_profiling(...)` call (`EnableProfilingBindData`).
#[derive(Debug, Default)]
pub(crate) struct ProfilingOptions<'a> {
    pub format: Option<&'a str>,
    pub coverage: Option<&'a str>,
    pub save_location: Option<&'a str>,
    pub mode: Option<&'a str>,
    pub metrics: Option<&'a Value>,
}

/// The bind data of `enable_profiling` / `disable_profiling`: the change to hand to the
/// connection.
#[derive(Debug)]
struct ProfilingCallData {
    client: Arc<rustdb_function::FunctionClient>,
    request: rustdb_function::ClientRequest,
}

impl rustdb_function::FunctionData for ProfilingCallData {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Whether `value` can be the metrics: a list of names, a struct or a JSON string.
fn is_metrics_value(value: &Value) -> bool {
    let ty = value.logical_type();
    (ty.id() == LogicalTypeId::List
        && ty
            .child_type()
            .is_some_and(|child| child.id() == LogicalTypeId::Varchar))
        || ty.id() == LogicalTypeId::Struct
        || ty.id() == LogicalTypeId::Varchar
}

/// The binding connection's function state.
fn function_client(
    input: &rustdb_function::TableFunctionBindInput<'_>,
) -> Result<Arc<rustdb_function::FunctionClient>> {
    input
        .binder
        .and_then(|binder| binder.downcast_ref::<rustdb_planner::ClientContext>())
        .map(|context| Arc::clone(&context.function_client))
        .ok_or_else(|| Error::internal("profiling function without a client context"))
}

/// `BindEnableProfiling`.
fn bind_enable_profiling(
    input: &rustdb_function::TableFunctionBindInput<'_>,
) -> Result<rustdb_function::TableFunctionBindOutput> {
    if input.inputs.len() > 1 {
        return Err(Error::invalid_input("EnableProfiling: expected 0 or 1 parameter"));
    }
    let metrics_error =
        || Error::invalid_input("EnableProfiling: metrics must be a list of strings or a JSON string");
    let (mut format, mut coverage, mut save_location, mut mode, mut metrics) = (None, None, None, None, None);
    for (name, value) in input.named_parameters {
        match name.to_ascii_lowercase().as_str() {
            "format" => format = Some(value.to_varchar()?.to_lowercase()),
            "coverage" => coverage = Some(value.to_varchar()?.to_lowercase()),
            "save_location" => save_location = Some(value.to_varchar()?),
            "mode" => mode = Some(value.to_varchar()?.to_lowercase()),
            _ => {
                if !is_metrics_value(value) {
                    return Err(metrics_error());
                }
                metrics = Some(value.clone());
            }
        }
    }
    if let Some(value) = input.inputs.first() {
        if metrics.is_some() {
            return Err(Error::invalid_input(
                "EnableProfiling: cannot specify both metrics and positional parameters",
            ));
        }
        if !is_metrics_value(value) {
            return Err(metrics_error());
        }
        metrics = Some(value.clone());
    }
    Ok(rustdb_function::TableFunctionBindOutput {
        return_types: vec![LogicalType::BOOLEAN],
        names: vec!["Success".to_owned()],
        bind_data: Some(Arc::new(ProfilingCallData {
            client: function_client(input)?,
            request: rustdb_function::ClientRequest::EnableProfiling {
                format,
                coverage,
                save_location,
                mode,
                metrics,
            },
        })),
    })
}

/// Runs a profiling call: hands its change to the connection, returning no rows.
fn run_profiling_call(
    bind_data: Option<&dyn rustdb_function::FunctionData>,
    _: &[u64],
    state: &mut rustdb_function::TableScanState,
    chunk: &mut rustdb_types::DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<ProfilingCallData>())
        .ok_or_else(|| Error::internal("profiling function without bind data"))?;
    *chunk = rustdb_types::DataChunk::with_types(&[LogicalType::BOOLEAN], 0);
    if state.position == 0 {
        state.position = 1;
        data.client.push_client_request(data.request.clone());
    }
    Ok(false)
}

/// `enable_profiling` and `disable_profiling` (`EnableProfilingFun::RegisterFunction`).
pub(crate) fn profiling_functions() -> Vec<rustdb_function::TableFunctionSet> {
    use rustdb_function::{TableFunction, TableFunctionSet};
    let mut enable = TableFunction::new("enable_profiling", Vec::new())
        .with_bind(bind_enable_profiling)
        .with_scan(run_profiling_call);
    enable.named_parameters = vec![
        ("format".to_owned(), LogicalType::VARCHAR),
        ("coverage".to_owned(), LogicalType::VARCHAR),
        ("save_location".to_owned(), LogicalType::VARCHAR),
        ("mode".to_owned(), LogicalType::VARCHAR),
        ("metrics".to_owned(), LogicalType::ANY),
    ];
    enable.varargs = Some(LogicalType::list(LogicalType::VARCHAR));
    let mut enable_set = TableFunctionSet::new("enable_profiling");
    enable_set.add(enable);
    let disable = TableFunction::new("disable_profiling", Vec::new())
        .with_bind(|input| {
            Ok(rustdb_function::TableFunctionBindOutput {
                return_types: vec![LogicalType::BOOLEAN],
                names: vec!["Success".to_owned()],
                bind_data: Some(Arc::new(ProfilingCallData {
                    client: function_client(input)?,
                    request: rustdb_function::ClientRequest::DisableProfiling,
                })),
            })
        })
        .with_scan(run_profiling_call);
    let mut disable_set = TableFunctionSet::new("disable_profiling");
    disable_set.add(disable);
    vec![enable_set, disable_set]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_settings_accept_metrics_and_groups() {
        let mut config = ProfilerConfig::default();
        config
            .set_custom_settings(
                &Value::varchar(r#"{"CPU_TIME": "false", "EXTRA_INFO": "true", "operator": "true"}"#),
                &BTreeSet::new(),
            )
            .unwrap();
        assert!(config.metrics.contains(&MetricType::ExtraInfo));
        assert!(config.metrics.contains(&MetricType::OperatorRowGroupsScanned));
        assert!(!config.metrics.contains(&MetricType::CpuTime));
        let error = config
            .set_custom_settings(&Value::varchar("BONJOUR"), &BTreeSet::new())
            .unwrap_err();
        assert!(error.to_string().starts_with(
            "IO Error: Could not parse the custom profiler settings file due to incorrect JSON: \"BONJOUR\""
        ));
        assert_eq!(
            ProfilerConfig::default()
                .custom_settings_value()
                .to_varchar()
                .unwrap()
                .matches("\"true\"")
                .count(),
            26
        );
    }

    #[test]
    fn timings_render_like_duckdb() {
        assert_eq!(render_timing(0.012_34), "0.0123s");
        assert_eq!(render_title_case("PHYSICAL_PLANNER"), "Physical Planner");
        assert_eq!(sanitize_key("__estimated_cardinality__"), "Estimated Cardinality");
        assert_eq!(extract_extension("dir.x/file.json"), "json");
        assert_eq!(extract_extension("dir.x/file"), "");
    }
}
