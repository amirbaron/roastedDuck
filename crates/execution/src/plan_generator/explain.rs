//! `EXPLAIN` (DuckDB's `plan_explain.cpp` and `PhysicalOperator::ToString`): the plans
//! of a statement rendered as text and returned as `(explain_key, explain_value)` rows.

use std::collections::HashMap;
use std::sync::Arc;

use rustdb_ast::{ExplainFormat, ExplainType};
use rustdb_common::tree_renderer::{RenderSource, RenderTree, RenderTreeNode, render};
use rustdb_common::{Error, Result};
use rustdb_planner::LogicalOperator;
use rustdb_planner::operator::renderer_kind;
use rustdb_types::{DataChunk, LogicalType, Value};

use super::PlanGenerator;
use crate::operator::{PhysicalOperator, PhysicalRef};
use crate::operators::{PhysicalColumnDataScan, PhysicalExplainAnalyze};

/// Which plans `EXPLAIN` shows (DuckDB's `ExplainOutputType`, the `explain_output` setting).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExplainOutput {
    /// The unoptimized and optimized logical plans and the physical plan.
    All,
    /// The optimized logical plan.
    OptimizedOnly,
    /// The physical plan.
    #[default]
    PhysicalOnly,
}

impl ExplainOutput {
    /// Parses a value of the `explain_output` setting.
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "optimized_only" => Ok(Self::OptimizedOnly),
            "physical_only" => Ok(Self::PhysicalOnly),
            _ => Err(Error::not_implemented(format!(
                "Enum value: unrecognized value \"{value}\" for enum \"ExplainOutputType\""
            ))),
        }
    }
}

/// A statement the executor runs outside the physical plan (PRAGMA, SET, PREPARE, ...),
/// shown by `EXPLAIN` under DuckDB's physical operator name.
#[derive(Debug)]
struct PhysicalUtility {
    name: &'static str,
}

impl PhysicalOperator for PhysicalUtility {
    fn name(&self) -> &'static str {
        self.name
    }
    fn types(&self) -> &[LogicalType] {
        &[]
    }
    fn children(&self) -> &[PhysicalRef] {
        &[]
    }
}

/// The physical operator name of statements without a physical plan here.
fn utility_name(op: &LogicalOperator) -> Option<&'static str> {
    use rustdb_planner::LogicalKind as K;
    Some(match &op.kind {
        K::Pragma { .. } => "PRAGMA",
        K::Set { .. } => "SET",
        K::Reset { .. } => "RESET",
        K::Prepare { .. } => "PREPARE",
        K::Transaction { .. } => "TRANSACTION",
        K::Attach(_) => "ATTACH",
        K::Detach(_) => "DETACH",
        K::Load(_) => "LOAD",
        K::AlterDatabase(_) => "ALTER",
        K::Deallocate { .. } => "DEALLOCATE",
        _ => return None,
    })
}

/// The address identifying a physical operator in the estimates map.
pub(crate) fn operator_key(op: &dyn PhysicalOperator) -> usize {
    std::ptr::from_ref(op).cast::<()>().addr()
}

/// How `EXPLAIN` shows a delim join (DuckDB's `PhysicalLeftDelimJoin` /
/// `PhysicalRightDelimJoin`): the join's node, renamed and with its delim index, over the
/// duplicate-eliminated input, the join itself and the DISTINCT computing the
/// duplicate-eliminated values. Here the delim sink is a dependency of the join's cached
/// input instead of a node above the join.
#[derive(Clone, Debug)]
pub(crate) struct DelimExplain {
    pub name: &'static str,
    pub delim_index: u64,
    pub input: PhysicalRef,
    /// The input positions of the duplicate-eliminated columns (the DISTINCT's groups).
    pub distinct_groups: Vec<u64>,
    /// The DISTINCT's estimate: that of the first delim scan.
    pub distinct_estimate: u64,
}

/// What `EXPLAIN` knows about the planned operators.
#[derive(Clone, Copy)]
struct ExplainInfo<'a> {
    estimates: &'a HashMap<usize, u64>,
    delim_joins: &'a HashMap<usize, DelimExplain>,
}

/// A node of the rendered physical plan, as the tree renderer walks it.
#[derive(Clone, Copy)]
enum PhysicalRenderNode<'a> {
    /// A physical operator; `plain` renders a delim join's join as itself.
    Operator {
        op: &'a dyn PhysicalOperator,
        info: ExplainInfo<'a>,
        plain: bool,
    },
    /// The DISTINCT of a delim join (DuckDB's `PhysicalHashAggregate` over the
    /// duplicate-eliminated columns).
    DelimDistinct { delim: &'a DelimExplain },
}

impl<'a> PhysicalRenderNode<'a> {
    const fn operator(op: &'a dyn PhysicalOperator, info: ExplainInfo<'a>) -> Self {
        Self::Operator {
            op,
            info,
            plain: false,
        }
    }
}

impl RenderSource for PhysicalRenderNode<'_> {
    fn render_node(&self) -> RenderTreeNode {
        match *self {
            Self::Operator { op, info, plain } => {
                let estimate = info.estimates.get(&operator_key(op)).copied().unwrap_or(0);
                let mut params = op.params_to_string(estimate);
                if !plain && let Some(delim) = info.delim_joins.get(&operator_key(op)) {
                    // DuckDB's `PhysicalDelimJoin::ParamsToString`.
                    params.insert("Delim Index".to_owned(), delim.delim_index.to_string());
                    return RenderTreeNode::new(delim.name.to_owned(), params);
                }
                RenderTreeNode::new(op.explain_name(), params)
            }
            Self::DelimDistinct { delim } => {
                let mut params = rustdb_common::tree_renderer::RenderParams::new();
                let groups: Vec<String> = delim
                    .distinct_groups
                    .iter()
                    .map(|index| format!("#{index}"))
                    .collect();
                params.insert("Groups".to_owned(), groups.join("\n"));
                crate::operator::set_estimated_cardinality(&mut params, delim.distinct_estimate);
                RenderTreeNode::new("HASH_GROUP_BY".to_owned(), params)
            }
        }
    }

    fn render_children(&self) -> Vec<Self> {
        let Self::Operator { op, info, plain } = *self else {
            return Vec::new();
        };
        if !plain && let Some(delim) = info.delim_joins.get(&operator_key(op)) {
            // DuckDB's `PhysicalDelimJoin::GetChildren`.
            return vec![
                Self::operator(delim.input.as_ref(), info),
                Self::Operator {
                    op,
                    info,
                    plain: true,
                },
                Self::DelimDistinct { delim },
            ];
        }
        op.explain_children()
            .iter()
            .map(|child| Self::operator(child.as_ref(), info))
            .collect()
    }
}

/// A physical plan rendered for `EXPLAIN` (DuckDB's `PhysicalOperator::ToString(format)`).
fn render_physical_plan(
    root: &dyn PhysicalOperator,
    estimates: &HashMap<usize, u64>,
    delim_joins: &HashMap<usize, DelimExplain>,
    format: ExplainFormat,
) -> String {
    let info = ExplainInfo {
        estimates,
        delim_joins,
    };
    let tree = RenderTree::create(&PhysicalRenderNode::operator(root, info));
    render(renderer_kind(format), tree)
}

impl PlanGenerator {
    /// The estimate recorded for `op`, 0 if none.
    pub(super) fn recorded_estimate(&self, op: &PhysicalRef) -> u64 {
        self.estimates
            .get(&operator_key(op.as_ref()))
            .copied()
            .unwrap_or(0)
    }

    /// Records `estimate` for `op` and the operators below it that have none yet: the
    /// operators planned for one logical operator share its estimate.
    pub(super) fn record_estimate(&mut self, op: &PhysicalRef, estimate: u64) {
        let key = operator_key(op.as_ref());
        if self.estimates.contains_key(&key) {
            return;
        }
        self.estimates.insert(key, estimate);
        for child in op.children() {
            self.record_estimate(child, estimate);
        }
    }

    /// `EXPLAIN` (DuckDB's `CreatePlan(LogicalExplain &)`).
    pub(super) fn plan_explain(
        &mut self,
        explain_type: ExplainType,
        explain_format: ExplainFormat,
        logical_plan_unopt: String,
        child: LogicalOperator,
        types: Vec<LogicalType>,
    ) -> Result<PhysicalRef> {
        let logical_plan_opt = child.to_explain_string(explain_format);
        let mut child = child;
        if let rustdb_planner::LogicalKind::Execute { .. } = &child.kind
            && let Some(prepared) = child.children.pop()
        {
            // EXPLAIN EXECUTE shows the plan of the prepared statement.
            child = prepared;
        }
        let plan: PhysicalRef = match utility_name(&child) {
            Some(name) => Arc::new(PhysicalUtility { name }),
            None => self.plan(child)?,
        };
        if explain_type == ExplainType::ExplainAnalyze {
            let query = self
                .context
                .as_ref()
                .map(|context| context.query.clone())
                .unwrap_or_default();
            return Ok(Arc::new(PhysicalExplainAnalyze::new(
                types,
                explain_format,
                query,
                plan,
                self.delim_explain.clone(),
            )));
        }
        let physical_plan = render_physical_plan(
            plan.as_ref(),
            &self.estimates,
            &self.delim_explain,
            explain_format,
        );
        let output = self
            .context
            .as_ref()
            .map_or(ExplainOutput::default(), |context| context.explain_output);
        let rows: Vec<(&str, String)> = match output {
            ExplainOutput::OptimizedOnly => vec![("logical_opt", logical_plan_opt)],
            ExplainOutput::PhysicalOnly => vec![("physical_plan", physical_plan)],
            ExplainOutput::All => vec![
                ("logical_plan", logical_plan_unopt),
                ("logical_opt", logical_plan_opt),
                ("physical_plan", physical_plan),
            ],
        };
        let count = rows.len();
        let mut chunk = DataChunk::with_types(&types, count);
        for (row, (key, value)) in rows.into_iter().enumerate() {
            chunk.column_mut(0).set_value(row, &Value::varchar(key))?;
            chunk.column_mut(1).set_value(row, &Value::varchar(value))?;
        }
        chunk.set_len(count);
        Ok(Arc::new(PhysicalColumnDataScan::new(types, Arc::from([chunk]))))
    }
}
