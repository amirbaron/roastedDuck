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
        K::Deallocate { .. } => "DEALLOCATE",
        _ => return None,
    })
}

/// The address identifying a physical operator in the estimates map.
fn operator_key(op: &dyn PhysicalOperator) -> usize {
    std::ptr::from_ref(op).cast::<()>().addr()
}

/// A physical operator with the planner's estimates, as the tree renderer walks it.
#[derive(Clone, Copy)]
struct PhysicalRenderNode<'a> {
    op: &'a dyn PhysicalOperator,
    estimates: &'a HashMap<usize, u64>,
}

impl RenderSource for PhysicalRenderNode<'_> {
    fn render_node(&self) -> RenderTreeNode {
        let estimate = self.estimates.get(&operator_key(self.op)).copied().unwrap_or(0);
        RenderTreeNode::new(self.op.explain_name(), self.op.params_to_string(estimate))
    }

    fn render_children(&self) -> Vec<Self> {
        self.op
            .explain_children()
            .iter()
            .map(|child| PhysicalRenderNode {
                op: child.as_ref(),
                estimates: self.estimates,
            })
            .collect()
    }
}

/// A physical plan rendered for `EXPLAIN` (DuckDB's `PhysicalOperator::ToString(format)`).
pub fn render_physical_plan(
    root: &dyn PhysicalOperator,
    estimates: &HashMap<usize, u64>,
    format: ExplainFormat,
) -> String {
    let tree = RenderTree::create(&PhysicalRenderNode { op: root, estimates });
    render(renderer_kind(format), tree)
}

impl PlanGenerator {
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
            )));
        }
        let physical_plan = render_physical_plan(plan.as_ref(), &self.estimates, explain_format);
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
