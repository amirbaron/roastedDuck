use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_planner::{Expression, ExpressionKind};
use roastedduck_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, SelectionVector, Value, Vector};

use crate::operator::{ErasedOperator, PhysicalOperator, PhysicalRef};
use crate::{ExecutionContext, ExpressionExecutor, Operator, OperatorResult};

/// `UNNEST` in a select list (DuckDB's `PhysicalUnnest`): every input row is repeated as
/// often as its longest unnested list, next to the list elements (NULL where a list is
/// shorter).
#[derive(Debug)]
pub struct PhysicalUnnest {
    types: Vec<LogicalType>,
    /// The list expressions inside each `UNNEST`.
    lists: Vec<Expression>,
    children: [PhysicalRef; 1],
}

impl PhysicalUnnest {
    pub fn new(types: Vec<LogicalType>, select_list: Vec<Expression>, child: PhysicalRef) -> Result<Self> {
        let lists = select_list
            .into_iter()
            .map(|expression| match expression.kind {
                ExpressionKind::Unnest(unnest) => Ok(unnest.child),
                _ => Err(Error::internal("UNNEST operator expects unnest expressions")),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            types,
            lists,
            children: [child],
        })
    }
}

impl PhysicalOperator for PhysicalUnnest {
    fn name(&self) -> &'static str {
        "UNNEST"
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

/// Where a thread is in the current input chunk (DuckDB's `UnnestOperatorState`).
#[derive(Debug)]
pub struct UnnestState {
    executor: ExpressionExecutor,
    /// The evaluated lists of the current input chunk, once fetched.
    list_data: Option<DataChunk>,
    /// Per input row: the length of its longest list.
    unnest_lengths: Vec<u64>,
    current_row: usize,
    list_position: u64,
}

impl UnnestState {
    fn reset(&mut self) {
        self.list_data = None;
        self.current_row = 0;
        self.list_position = 0;
    }

    /// Evaluates the lists of `input` and the number of output rows of each input row
    /// (`PrepareInput`).
    fn prepare_input(&mut self, input: &DataChunk) -> Result<()> {
        let mut list_data = DataChunk::default();
        self.executor.execute(input, &mut list_data)?;
        self.unnest_lengths.clear();
        self.unnest_lengths.resize(input.len(), 0);
        for list in list_data.columns() {
            if list.logical_type().id() == LogicalTypeId::SqlNull {
                continue;
            }
            let entries = list.unified::<ListEntry>();
            for (row, length) in self.unnest_lengths.iter_mut().enumerate() {
                if let Some(entry) = entries.get(row) {
                    *length = (*length).max(entry.length);
                }
            }
        }
        self.list_data = Some(list_data);
        Ok(())
    }
}

/// Whether a list vector cannot be sliced: `UNNEST(NULL)` or an empty child.
fn unnests_to_null(list: &Vector) -> bool {
    if list.logical_type().id() == LogicalTypeId::SqlNull {
        return true;
    }
    match list.list_child() {
        Some((child, size)) => size == 0 || child.logical_type().id() == LogicalTypeId::SqlNull,
        None => true,
    }
}

impl Operator for PhysicalUnnest {
    type Global = ();
    type Local = UnnestState;

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<()> {
        Ok(())
    }

    fn local_operator_state(&self, _: &ExecutionContext, (): &()) -> Result<UnnestState> {
        Ok(UnnestState {
            executor: ExpressionExecutor::new(self.lists.clone()),
            list_data: None,
            unnest_lengths: Vec::new(),
            current_row: 0,
            list_position: 0,
        })
    }

    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        (): &(),
        state: &mut UnnestState,
    ) -> Result<OperatorResult> {
        loop {
            if state.list_data.is_none() {
                state.prepare_input(input)?;
            }
            if state.current_row >= input.len() {
                state.reset();
                return Ok(OperatorResult::NeedMoreInput);
            }
            let Some(list_data) = &state.list_data else {
                return Err(Error::internal("unnest without list data"));
            };
            let list_count = list_data.column_count();
            let mut input_sel = Vec::with_capacity(STANDARD_VECTOR_SIZE);
            let mut unnest_sels = vec![Vec::with_capacity(STANDARD_VECTOR_SIZE); list_count];
            let mut null_rows = vec![Vec::new(); list_count];
            let mut result_length = 0usize;
            let mut unnest_list_count = 0;
            let initial_row = state.current_row;
            while result_length < STANDARD_VECTOR_SIZE && state.current_row < input.len() {
                let row = state.current_row;
                let row_length = ((STANDARD_VECTOR_SIZE - result_length) as u64)
                    .min(state.unnest_lengths[row] - state.list_position);
                if row_length > 0 {
                    input_sel.extend(std::iter::repeat_n(row as u32, row_length as usize));
                    for (column, list) in list_data.columns().iter().enumerate() {
                        let entry = if list.logical_type().id() == LogicalTypeId::SqlNull {
                            None
                        } else {
                            list.unified::<ListEntry>().get(row)
                        };
                        let (length, offset) = entry.map_or((0, 0), |e| (e.length, e.offset));
                        let unnest_length = (length - length.min(state.list_position)).min(row_length);
                        let sel = &mut unnest_sels[column];
                        sel.extend((0..unnest_length).map(|r| (offset + state.list_position + r) as u32));
                        // Rows past the end of this list are NULL (slot 0 is overwritten).
                        for r in unnest_length..row_length {
                            null_rows[column].push(result_length + r as usize);
                            sel.push(0);
                        }
                    }
                    result_length += row_length as usize;
                    state.list_position += row_length;
                }
                unnest_list_count += 1;
                if state.list_position == state.unnest_lengths[row] {
                    state.current_row += 1;
                    state.list_position = 0;
                }
            }
            if result_length == 0 {
                continue;
            }
            let mut columns = Vec::with_capacity(self.types.len());
            if unnest_list_count == 1 {
                // Every output row comes from the same input row.
                for column in input.columns() {
                    columns.push(column.reference_row(initial_row, result_length)?);
                }
            } else {
                let sel = SelectionVector::new(input_sel);
                columns.extend(input.columns().iter().map(|column| column.slice(&sel)));
            }
            for (column, list) in list_data.columns().iter().enumerate() {
                let result_type = &self.types[input.column_count() + column];
                if unnests_to_null(list) {
                    columns.push(Vector::constant(&Value::null(result_type.clone()))?);
                    continue;
                }
                let Some((child, _)) = list.list_child() else {
                    return Err(Error::internal("unnest of a non-list vector"));
                };
                let sel = SelectionVector::new(std::mem::take(&mut unnest_sels[column]));
                let mut result = child.slice(&sel);
                if !null_rows[column].is_empty() {
                    result.flatten(result_length);
                    for &row in &null_rows[column] {
                        result.set_null(row);
                    }
                }
                columns.push(result);
            }
            output.set_columns(columns, result_length);
            return Ok(OperatorResult::HaveMoreOutput);
        }
    }
}
