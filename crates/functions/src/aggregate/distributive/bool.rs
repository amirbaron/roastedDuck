//! `bool_and` and `bool_or` (DuckDB's `bool.cpp`).

use rustdb_common::Result;
use rustdb_function::aggregate::{Unary, UnaryAggregate};
use rustdb_function::{AggregateFunction, AggregateFunctionSet};
use rustdb_types::LogicalType;

/// Whether any value was seen, and the running AND/OR.
#[derive(Clone, Copy, Debug)]
struct BoolState<const AND: bool> {
    empty: bool,
    value: bool,
}

impl<const AND: bool> Default for BoolState<AND> {
    fn default() -> Self {
        Self {
            empty: true,
            value: AND,
        }
    }
}

struct BoolAgg<const AND: bool>;

impl<const AND: bool> UnaryAggregate for BoolAgg<AND> {
    type Input = bool;
    type State = BoolState<AND>;
    type Output = bool;

    #[inline]
    fn update(state: &mut BoolState<AND>, value: bool) -> Result<()> {
        state.empty = false;
        state.value = if AND {
            value && state.value
        } else {
            value || state.value
        };
        Ok(())
    }

    fn update_repeated(state: &mut BoolState<AND>, value: bool, _: usize) -> Result<()> {
        Self::update(state, value)
    }

    fn combine(source: &BoolState<AND>, target: &mut BoolState<AND>) -> Result<()> {
        target.value = if AND {
            target.value && source.value
        } else {
            target.value || source.value
        };
        target.empty = target.empty && source.empty;
        Ok(())
    }

    fn finalize(state: &BoolState<AND>) -> Result<Option<bool>> {
        Ok((!state.empty).then_some(state.value))
    }
}

fn bool_function<const AND: bool>(name: &str) -> AggregateFunctionSet {
    AggregateFunctionSet::single(
        name,
        AggregateFunction::new(
            name,
            vec![LogicalType::BOOLEAN],
            LogicalType::BOOLEAN,
            Unary::<BoolAgg<AND>>::default(),
        )
        .order_independent()
        .distinct_independent(),
    )
}

/// `bool_and(BOOLEAN) -> BOOLEAN`.
pub fn bool_and_functions() -> AggregateFunctionSet {
    bool_function::<true>("bool_and")
}

/// `bool_or(BOOLEAN) -> BOOLEAN`.
pub fn bool_or_functions() -> AggregateFunctionSet {
    bool_function::<false>("bool_or")
}
