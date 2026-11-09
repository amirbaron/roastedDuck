use roastedduck_common::Result;
use roastedduck_planner::Expression;
use roastedduck_types::{DataChunk, LogicalType};

use crate::operator::{ErasedOperator, PhysicalOperator, PhysicalRef};
use crate::{ExecutionContext, ExpressionExecutor, Operator, OperatorResult};

/// `VALUES` whose rows depend on the input, e.g. a `LATERAL (VALUES (t.a), (t.b))` (DuckDB's
/// `PhysicalExpressionScan`): every input chunk produces each row's expressions evaluated
/// over it, one row list after the other.
#[derive(Debug)]
pub struct PhysicalExpressionScan {
    types: Vec<LogicalType>,
    rows: Vec<Vec<Expression>>,
    children: [PhysicalRef; 1],
}

impl PhysicalExpressionScan {
    pub fn new(types: Vec<LogicalType>, rows: Vec<Vec<Expression>>, child: PhysicalRef) -> Self {
        Self {
            types,
            rows,
            children: [child],
        }
    }
}

impl PhysicalOperator for PhysicalExpressionScan {
    fn name(&self) -> &'static str {
        "EXPRESSION_SCAN"
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
    fn parallel_operator(&self) -> bool {
        true
    }
}

/// One executor per row list and the next list to evaluate (`ExpressionScanState`).
#[derive(Debug)]
pub struct ExpressionScanState {
    executors: Vec<ExpressionExecutor>,
    next: usize,
}

impl Operator for PhysicalExpressionScan {
    type Global = ();
    type Local = ExpressionScanState;

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<()> {
        Ok(())
    }

    fn local_operator_state(&self, _: &ExecutionContext, (): &()) -> Result<ExpressionScanState> {
        Ok(ExpressionScanState {
            executors: self.rows.iter().cloned().map(ExpressionExecutor::new).collect(),
            next: 0,
        })
    }

    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        (): &(),
        state: &mut ExpressionScanState,
    ) -> Result<OperatorResult> {
        if let Some(executor) = state.executors.get_mut(state.next) {
            executor.execute(input, output)?;
            state.next += 1;
        }
        if state.next < state.executors.len() {
            return Ok(OperatorResult::HaveMoreOutput);
        }
        state.next = 0;
        Ok(OperatorResult::NeedMoreInput)
    }
}
