use rustdb_common::Result;
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
    fn as_operator(&self) -> Option<&dyn ErasedOperator> {
        Some(self)
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
