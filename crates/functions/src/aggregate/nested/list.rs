//! `list`/`array_agg` (DuckDB's `list.cpp`): collects every input value, NULLs included.

use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::aggregate::{Aggregate, AggregateInput, States, Typed};
use rustdb_function::{AggregateFunction, AggregateFunctionSet, BindInput, FunctionData};
use rustdb_types::{ListEntry, LogicalType, Vector};

use crate::aggregate::collect::RowCollection;
use crate::signature::template;

struct List;

impl Aggregate for List {
    type State = RowCollection;

    fn initialize(&self) -> RowCollection {
        RowCollection::default()
    }

    fn update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        states: &mut States<'_, RowCollection>,
        count: usize,
    ) -> Result<()> {
        let input = Arc::new(inputs[0].clone());
        if let Some(state) = states.as_single() {
            state.push_all(&input, count);
            return Ok(());
        }
        for row in 0..count {
            states.get(row).push(&input, row);
        }
        Ok(())
    }

    fn combine(
        &self,
        source: &RowCollection,
        target: &mut RowCollection,
        _: AggregateInput<'_>,
    ) -> Result<()> {
        target.extend(source);
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut RowCollection,
        _: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        if state.is_empty() {
            let capacity = result.capacity();
            result.validity_mut().set_invalid(row, capacity);
            return Ok(());
        }
        append_list(
            result,
            row,
            |child, offset| state.write_to(child, offset),
            state.len(),
        )
    }
}

/// Makes row `row` of the LIST vector `result` a new list of `len` elements, written by
/// `write(child, offset)` at the end of the list's child vector.
pub(crate) fn append_list(
    result: &mut Vector,
    row: usize,
    write: impl FnOnce(&mut Vector, usize) -> Result<()>,
    len: usize,
) -> Result<()> {
    let offset = result.list_child().map_or(0, |(_, size)| size);
    let child = result
        .list_child_mut()
        .ok_or_else(|| Error::internal("list aggregate result is not a list"))?;
    if child.capacity() < offset + len {
        child.resize((offset + len).max(child.capacity() * 2));
    }
    write(child, offset)?;
    result.set_list_size(offset + len);
    result.values_mut::<ListEntry>()[row] = ListEntry::new(offset, len);
    Ok(())
}

/// DuckDB's `ListBindFunction`.
#[allow(clippy::unnecessary_wraps, reason = "the signature of a bind callback")]
fn bind_list(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let ty = input.argument_types[0].clone();
    function.arguments = vec![ty.clone()];
    function.return_type = LogicalType::list(ty);
    Ok(None)
}

/// `list(T) -> T[]` (alias `array_agg`).
pub fn list_functions() -> AggregateFunctionSet {
    let template = template("T");
    AggregateFunctionSet::single(
        "list",
        AggregateFunction::new(
            "list",
            vec![template.clone()],
            LogicalType::list(template),
            Typed(List),
        )
        .with_bind(bind_list),
    )
}
