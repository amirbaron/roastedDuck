use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::collections::HashMap;
use std::ptr::NonNull;

use rustdb_common::{Error, Result};
use rustdb_function::StatePtr;
use rustdb_function::aggregate::AggregateInput;
use rustdb_planner::ExpressionKind;
use rustdb_planner::operator::BoundPivotInfo;
use rustdb_types::{DataChunk, ListEntry, LogicalType, Value, Vector};

use crate::operator::{ErasedOperator, PhysicalOperator, PhysicalRef};
use crate::{ExecutionContext, Operator, OperatorResult};

/// The last stage of a LIST-based PIVOT (DuckDB's `PhysicalPivot`): each input row holds
/// the groups, one list per aggregate, and the list of pivot column names; the list
/// elements are moved into the columns their names select. Columns without a value get
/// the aggregate's result over no rows.
#[derive(Debug)]
pub struct PhysicalPivot {
    types: Vec<LogicalType>,
    group_count: usize,
    /// Output column of the first aggregate of each pivot value name.
    pivot_map: HashMap<rustdb_common::ByteString, usize>,
    /// Each aggregate finalized over an empty state.
    empty_aggregates: Vec<Value>,
    children: [PhysicalRef; 1],
}

impl PhysicalPivot {
    pub fn new(types: Vec<LogicalType>, child: PhysicalRef, pivot: BoundPivotInfo) -> Result<Self> {
        let group_count = pivot.group_count as usize;
        let mut pivot_map = HashMap::new();
        for (index, value) in pivot.pivot_values.into_iter().enumerate() {
            pivot_map.entry(value).or_insert(group_count + index);
        }
        let empty_aggregates = pivot
            .aggregates
            .iter()
            .map(|expression| match &expression.kind {
                ExpressionKind::Aggregate(aggregate) => empty_aggregate(aggregate),
                _ => Err(Error::internal("PIVOT aggregate is not an aggregate")),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            types,
            group_count,
            pivot_map,
            empty_aggregates,
            children: [child],
        })
    }
}

/// The aggregate's result over no rows: a fresh state, finalized.
fn empty_aggregate(aggregate: &rustdb_planner::expression::BoundAggregateExpression) -> Result<Value> {
    let ops = &aggregate.function.ops;
    let layout = ops.state_layout();
    let layout = Layout::from_size_align(layout.size().max(1), layout.align())
        .map_err(|_| Error::internal("bad state layout"))?;
    // SAFETY: non-zero size.
    let memory = NonNull::new(unsafe { alloc_zeroed(layout) }).unwrap_or_else(|| handle_alloc_error(layout));
    // SAFETY: the buffer is sized and aligned for the state.
    let state = unsafe { StatePtr::new(memory) };
    let mut result = Vector::flat(aggregate.function.return_type.clone(), 1);
    let input = AggregateInput {
        bind_data: aggregate.bind_info.as_deref().map(|data| data as _),
    };
    // SAFETY: the state is initialised before use and destroyed exactly once.
    let finalized = unsafe {
        ops.initialize(state);
        let finalized = ops.finalize(&[state], input, &mut result, 0);
        if ops.needs_destructor() {
            ops.destroy(&[state]);
        }
        finalized
    };
    // SAFETY: allocated above with this layout.
    unsafe { dealloc(memory.as_ptr(), layout) };
    finalized?;
    Ok(result.get_value(0))
}

impl PhysicalOperator for PhysicalPivot {
    fn name(&self) -> &'static str {
        "PIVOT"
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

impl Operator for PhysicalPivot {
    type Global = ();
    type Local = ();

    fn global_operator_state(&self, _: &ExecutionContext) -> Result<()> {
        Ok(())
    }

    fn local_operator_state(&self, _: &ExecutionContext, (): &()) -> Result<()> {
        Ok(())
    }

    fn execute(
        &self,
        _: &ExecutionContext,
        input: &DataChunk,
        output: &mut DataChunk,
        (): &(),
        (): &mut (),
    ) -> Result<OperatorResult> {
        let count = input.len();
        let mut columns: Vec<Vector> = input.columns()[..self.group_count].to_vec();
        // Every pivot column starts as the empty aggregate value; with several
        // aggregates the columns alternate [AGGR1][AGGR2][AGGR1][AGGR2]...
        let mut values: Vec<Vec<Value>> = (self.group_count..self.types.len())
            .enumerate()
            .map(|(i, _)| vec![self.empty_aggregates[i % self.empty_aggregates.len()].clone(); count])
            .collect();
        let names = input
            .columns()
            .last()
            .ok_or_else(|| Error::internal("PIVOT input without pivot names"))?;
        let name_entries = names.unified::<ListEntry>();
        let (name_child, _) = names
            .list_child()
            .ok_or_else(|| Error::internal("PIVOT names are not a list"))?;
        #[allow(
            clippy::needless_range_loop,
            reason = "`row` indexes the input lists and every output column"
        )]
        for row in 0..count {
            let Some(list) = name_entries.get(row) else {
                continue;
            };
            for element in 0..list.length {
                let position = (list.offset + element) as usize;
                let name = name_child.get_value(position);
                let Some(&column) = name.as_bytes().and_then(|name| self.pivot_map.get(name)) else {
                    // Not one of the requested pivot values.
                    continue;
                };
                for (aggregate, _) in self.empty_aggregates.iter().enumerate() {
                    let lists = &input.columns()[self.group_count + aggregate];
                    let entry = lists.unified::<ListEntry>().get(row).ok_or_else(|| {
                        Error::internal("Pivot - unaligned lists between values and columns!?")
                    })?;
                    if entry.length != list.length {
                        return Err(Error::internal(
                            "Pivot - unaligned lists between values and columns!?",
                        ));
                    }
                    let (child, _) = lists
                        .list_child()
                        .ok_or_else(|| Error::internal("PIVOT values are not a list"))?;
                    values[column + aggregate - self.group_count][row] =
                        child.get_value((entry.offset + element) as usize);
                }
            }
        }
        for (column, column_values) in values.into_iter().enumerate() {
            let mut vector = Vector::flat(self.types[self.group_count + column].clone(), count.max(1));
            for (row, value) in column_values.iter().enumerate() {
                vector.set_value(row, value)?;
            }
            columns.push(vector);
        }
        output.set_columns(columns, count);
        Ok(OperatorResult::NeedMoreInput)
    }
}
