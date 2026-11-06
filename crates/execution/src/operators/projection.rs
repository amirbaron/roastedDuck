use crate::operator::{expression_names, set_estimated_cardinality};
use rustdb_common::Result;
use rustdb_common::tree_renderer::RenderParams;
use rustdb_planner::Expression;
use rustdb_types::{DataChunk, LogicalType};

use crate::operator::{ErasedOperator, PhysicalOperator, PhysicalRef};
use crate::{ExecutionContext, ExpressionExecutor, Operator, OperatorResult};

/// Computes one output column per expression.
#[derive(Debug)]
pub struct PhysicalProjection {
    types: Vec<LogicalType>,
    expressions: Vec<Expression>,
    children: [PhysicalRef; 1],
}

impl PhysicalProjection {
    /// The projected expressions (DuckDB's `select_list`).
    pub fn select_list(&self) -> &[Expression] {
        &self.expressions
    }

    pub fn new(types: Vec<LogicalType>, expressions: Vec<Expression>, child: PhysicalRef) -> Self {
        Self {
            types,
            expressions,
            children: [child],
        }
    }
}

impl PhysicalOperator for PhysicalProjection {
    fn name(&self) -> &'static str {
        "PROJECTION"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn preallocate_output(&self) -> bool {
        false
    }
    fn as_operator(&self) -> Option<&dyn ErasedOperator> {
        Some(self)
    }
    fn parallel_operator(&self) -> bool {
        true
    }

    fn params_to_string(&self, estimated_cardinality: u64) -> RenderParams {
        let mut result = RenderParams::new();
        result.insert("__projections__".to_owned(), expression_names(&self.expressions));
        set_estimated_cardinality(&mut result, estimated_cardinality);
        result
    }
}

impl Operator for PhysicalProjection {
    type Global = ();
    type Local = ExpressionExecutor;

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<()> {
        Ok(())
    }

    fn local_operator_state(&self, _: &ExecutionContext, (): &()) -> Result<ExpressionExecutor> {
        Ok(ExpressionExecutor::new(self.expressions.clone()))
    }

    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        (): &(),
        executor: &mut ExpressionExecutor,
    ) -> Result<OperatorResult> {
        executor.execute(input, output)?;
        Ok(OperatorResult::NeedMoreInput)
    }
}
