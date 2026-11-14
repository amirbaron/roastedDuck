//! Table-in/table-out functions over a child's rows (DuckDB's
//! `PhysicalTableInOutFunction`): `range(x)` or `unnest(l)` with column arguments.

use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_function::{FunctionData, InOutResult, TableFunction, TableInOutFunction, TableScanState};
use roastedduck_types::{DataChunk, LogicalType, Vector};

use crate::operator::{ErasedOperator, PhysicalOperator, PhysicalRef};
use crate::{ExecutionContext, Operator, OperatorResult};

/// Runs a table function's in-out callback on each input chunk. With `projected_input`
/// (correlated columns of a LATERAL join), the input is processed row by row and those
/// columns of the row are appended to each output row.
pub struct PhysicalTableInOutFunction {
    types: Vec<LogicalType>,
    function: TableFunction,
    in_out: TableInOutFunction,
    bind_data: Option<Arc<dyn FunctionData>>,
    /// The function's columns the plan reads, virtual ones included.
    column_ids: Vec<u64>,
    projected_input: Vec<u64>,
    /// The output column numbering the rows produced for each input row
    /// (`WITH ORDINALITY` of a correlated call).
    ordinality_index: Option<u64>,
    children: [PhysicalRef; 1],
}

impl std::fmt::Debug for PhysicalTableInOutFunction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhysicalTableInOutFunction")
            .field("function", &self.function.name)
            .field("projected_input", &self.projected_input)
            .finish_non_exhaustive()
    }
}

impl PhysicalTableInOutFunction {
    pub fn new(
        types: Vec<LogicalType>,
        function: TableFunction,
        bind_data: Option<Arc<dyn FunctionData>>,
        column_ids: Vec<u64>,
        projected_input: Vec<u64>,
        ordinality_index: Option<u64>,
        child: PhysicalRef,
    ) -> Result<Self> {
        let in_out = function
            .in_out_function
            .clone()
            .ok_or_else(|| Error::internal(format!("{} has no in-out function", function.name)))?;
        Ok(Self {
            types,
            function,
            in_out,
            bind_data,
            column_ids,
            projected_input,
            ordinality_index,
            children: [child],
        })
    }

    /// The function's own output columns (without ordinality and projected input).
    fn function_columns(&self) -> usize {
        self.types.len() - self.projected_input.len() - usize::from(self.ordinality_index.is_some())
    }

    /// Calls the in-out function and appends the ordinality column (`SetOrdinality`).
    fn call(&self, state: &mut InOutState, input: &DataChunk, output: &mut DataChunk) -> Result<InOutResult> {
        let mut produced = DataChunk::default();
        let result = (self.in_out)(self.bind_data.as_deref(), &mut state.scan, input, &mut produced)?;
        let count = produced.len();
        let mut columns: Vec<Vector> = produced.columns().to_vec();
        columns.truncate(self.function_columns());
        if self.ordinality_index.is_some() {
            let start = i64::try_from(state.ordinality).unwrap_or(i64::MAX);
            columns.push(Vector::sequence(LogicalType::BIGINT, start, 1));
            state.ordinality += count as u64;
        }
        *output = DataChunk::new(columns, count);
        Ok(result)
    }
}

impl PhysicalOperator for PhysicalTableInOutFunction {
    fn name(&self) -> &'static str {
        if self.function.name.eq_ignore_ascii_case("hnsw_index_join") {
            "HNSW_INDEX_JOIN"
        } else {
            "INOUT_FUNCTION"
        }
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
        !self.function.name.eq_ignore_ascii_case("hnsw_index_join")
    }
}

/// A thread's progress (DuckDB's `TableInOutLocalState`).
#[derive(Debug, Default)]
pub struct InOutState {
    scan: TableScanState,
    row_index: usize,
    new_row: bool,
    ordinality: u64,
}

fn operator_result(result: InOutResult) -> OperatorResult {
    match result {
        InOutResult::NeedMoreInput => OperatorResult::NeedMoreInput,
        InOutResult::HaveMoreOutput => OperatorResult::HaveMoreOutput,
        InOutResult::Finished => OperatorResult::Finished,
    }
}

impl Operator for PhysicalTableInOutFunction {
    type Global = ();
    type Local = InOutState;

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<()> {
        Ok(())
    }

    fn local_operator_state(&self, _: &ExecutionContext, (): &()) -> Result<InOutState> {
        Ok(InOutState {
            scan: TableScanState {
                column_ids: self.column_ids.clone(),
                ..TableScanState::default()
            },
            new_row: true,
            ordinality: 1,
            ..InOutState::default()
        })
    }

    fn execute(
        &self,
        context: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        (): &(),
        state: &mut InOutState,
    ) -> Result<OperatorResult> {
        if self.projected_input.is_empty() {
            let result = self.call(state, input, output)?;
            if let (Some(profiler), Some(explain)) = (&context.profiler, &self.function.in_out_explain) {
                profiler.record_extra_info(crate::profiler::operator_key(self), explain(&state.scan));
            }
            return Ok(operator_result(result));
        }
        // Row by row, with the row's projected columns next to its output.
        if state.new_row {
            if state.row_index >= input.len() {
                state.new_row = true;
                state.row_index = 0;
                *output = DataChunk::new(
                    self.types.iter().map(|ty| Vector::flat(ty.clone(), 1)).collect(),
                    0,
                );
                return Ok(OperatorResult::NeedMoreInput);
            }
            state.row_index += 1;
            state.new_row = false;
            state.ordinality = 1;
        }
        let row = state.row_index - 1;
        let input_length = input.columns().len().saturating_sub(self.projected_input.len());
        let row_input = DataChunk::new(
            input.columns()[..input_length]
                .iter()
                .map(|column| Vector::constant(&column.get_value(row)))
                .collect::<Result<Vec<_>>>()?,
            1,
        );
        let mut produced = DataChunk::default();
        let result = self.call(state, &row_input, &mut produced)?;
        if let (Some(profiler), Some(explain)) = (&context.profiler, &self.function.in_out_explain) {
            profiler.record_extra_info(crate::profiler::operator_key(self), explain(&state.scan));
        }
        let count = produced.len();
        let mut columns: Vec<Vector> = produced.columns().to_vec();
        for &source in &self.projected_input {
            let column = input
                .columns()
                .get(source as usize)
                .ok_or_else(|| Error::internal("projected input column out of range"))?;
            columns.push(Vector::constant(&column.get_value(row))?);
        }
        *output = DataChunk::new(columns, count);
        match result {
            InOutResult::Finished => Ok(OperatorResult::Finished),
            InOutResult::NeedMoreInput => {
                state.new_row = true;
                Ok(OperatorResult::HaveMoreOutput)
            }
            InOutResult::HaveMoreOutput => Ok(OperatorResult::HaveMoreOutput),
        }
    }
}
