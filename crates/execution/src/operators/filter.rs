use rustdb_common::Result;
use rustdb_planner::Expression;
use rustdb_types::{DataChunk, LogicalType, SelectionVector};

use crate::operator::{ErasedOperator, PhysicalOperator, PhysicalRef};
use crate::{ExecutionContext, ExpressionExecutor, Operator, OperatorResult};

/// Keeps the rows for which the predicate is true (DuckDB's `PhysicalFilter`).
#[derive(Debug)]
pub struct PhysicalFilter {
    types: Vec<LogicalType>,
    predicate: Expression,
    children: [PhysicalRef; 1],
}

impl PhysicalFilter {
    /// `predicate` is the conjunction of all filter expressions.
    pub fn new(types: Vec<LogicalType>, predicate: Expression, child: PhysicalRef) -> Self {
        Self {
            types,
            predicate,
            children: [child],
        }
    }
}

impl PhysicalOperator for PhysicalFilter {
    fn name(&self) -> &'static str {
        "FILTER"
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

/// Thread-local filter state: the predicate executor and a reusable selection buffer.
#[derive(Debug)]
pub struct FilterState {
    executor: ExpressionExecutor,
    selection: Vec<u32>,
}

impl Operator for PhysicalFilter {
    type Global = ();
    type Local = FilterState;

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<()> {
        Ok(())
    }

    fn local_operator_state(&self, _: &ExecutionContext, (): &()) -> Result<FilterState> {
        Ok(FilterState {
            executor: ExpressionExecutor::new(vec![self.predicate.clone()]),
            selection: Vec::new(),
        })
    }

    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        (): &(),
        state: &mut FilterState,
    ) -> Result<OperatorResult> {
        let passed = state.executor.select(input, &mut state.selection)?;
        if passed == input.len() {
            output.set_columns(input.columns().to_vec(), passed);
        } else {
            let sel = SelectionVector::new(std::mem::take(&mut state.selection));
            output.set_columns(
                input.columns().iter().map(|column| column.slice(&sel)).collect(),
                passed,
            );
        }
        Ok(OperatorResult::NeedMoreInput)
    }
}
