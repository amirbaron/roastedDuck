//! `first`, `last` and `any_value` (DuckDB's `first_last_any.cpp`).
//!
//! `first`/`last` return the first/last row's value, NULL included; `any_value` returns
//! the first non-NULL value.

use std::marker::PhantomData;

use rustdb_common::Result;
use rustdb_function::aggregate::{Aggregate, AggregateInput, States, Typed};
use rustdb_function::{AggregateFunction, AggregateFunctionSet, BindInput, FunctionData};
use rustdb_types::{LogicalType, Vector};

use crate::aggregate::stored::{StoredKind, with_stored_kind};

/// The state of `first`/`last`: whether a row was taken and its value (`None` is NULL).
#[derive(Clone, Debug)]
struct FirstState<V> {
    is_set: bool,
    value: Option<V>,
}

impl<V> Default for FirstState<V> {
    fn default() -> Self {
        Self {
            is_set: false,
            value: None,
        }
    }
}

struct First<K, const LAST: bool, const SKIP_NULLS: bool>(PhantomData<fn() -> K>);

impl<K: StoredKind, const LAST: bool, const SKIP_NULLS: bool> First<K, LAST, SKIP_NULLS> {
    /// Takes `value` (a row's value, `None` for NULL) into `state`; returns whether the
    /// row was taken.
    fn take(state: &mut FirstState<K::Owned>, value: Option<K::Ref<'_>>) -> bool {
        match value {
            Some(value) => {
                match &mut state.value {
                    Some(owned) => K::assign(owned, value),
                    None => state.value = Some(K::to_owned(value)),
                }
                state.is_set = true;
                true
            }
            None if SKIP_NULLS => false,
            None => {
                state.is_set = true;
                state.value = None;
                true
            }
        }
    }
}

impl<K: StoredKind, const LAST: bool, const SKIP_NULLS: bool> Aggregate for First<K, LAST, SKIP_NULLS> {
    type State = FirstState<K::Owned>;

    fn initialize(&self) -> Self::State {
        FirstState::default()
    }

    fn update(
        &self,
        inputs: &[Vector],
        _: AggregateInput<'_>,
        states: &mut States<'_, Self::State>,
        count: usize,
    ) -> Result<()> {
        if let Some(state) = states.as_single() {
            if !LAST && state.is_set {
                return Ok(());
            }
            let input = K::prepare(&inputs[0], count)?;
            if LAST {
                for row in (0..count).rev() {
                    if Self::take(state, K::get(&input, row)) {
                        break;
                    }
                }
            } else {
                for row in 0..count {
                    if Self::take(state, K::get(&input, row)) {
                        break;
                    }
                }
            }
            return Ok(());
        }
        let input = K::prepare(&inputs[0], count)?;
        for row in 0..count {
            let state = states.get(row);
            if !LAST && state.is_set {
                continue;
            }
            Self::take(state, K::get(&input, row));
        }
        Ok(())
    }

    fn combine(&self, source: &Self::State, target: &mut Self::State, _: AggregateInput<'_>) -> Result<()> {
        if source.is_set && (LAST || !target.is_set) {
            target.clone_from(source);
        }
        Ok(())
    }

    fn finalize(
        &self,
        state: &mut Self::State,
        _: AggregateInput<'_>,
        result: &mut Vector,
        row: usize,
    ) -> Result<()> {
        match (&state.value, state.is_set) {
            (Some(value), true) => K::write(value, result, row),
            _ => {
                let capacity = result.capacity();
                result.validity_mut().set_invalid(row, capacity);
                Ok(())
            }
        }
    }
}

/// The `first`/`last`/`any_value` aggregate over values of `ty` (DuckDB's
/// `GetFirstFunction`).
pub fn first_function<const LAST: bool, const SKIP_NULLS: bool>(ty: &LogicalType) -> AggregateFunction {
    let name = match (LAST, SKIP_NULLS) {
        (true, _) => "last",
        (false, false) => "first",
        (false, true) => "any_value",
    };
    with_stored_kind!(ty, false, K => AggregateFunction::new(
        name,
        vec![ty.clone()],
        ty.clone(),
        Typed(First::<K, LAST, SKIP_NULLS>(PhantomData)),
    ))
}

/// DuckDB's `BindFirst`/`BindDecimalFirst`: specialises on the argument type.
fn bind_first<const LAST: bool, const SKIP_NULLS: bool>(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Option<std::sync::Arc<dyn FunctionData>>> {
    let ty = input.argument_types[0].clone();
    function.replace_with(first_function::<LAST, SKIP_NULLS>(&ty).distinct_independent());
    Ok(None)
}

/// DuckDB's `AddFirstOperator`: a DECIMAL and an ANY overload.
fn first_set<const LAST: bool, const SKIP_NULLS: bool>(name: &str) -> AggregateFunctionSet {
    let mut set = AggregateFunctionSet::new(name);
    set.add(AggregateFunction::unbound(
        vec![LogicalType::simple(rustdb_types::LogicalTypeId::Decimal)],
        LogicalType::simple(rustdb_types::LogicalTypeId::Decimal),
        bind_first::<LAST, SKIP_NULLS>,
    ));
    set.add(AggregateFunction::unbound(
        vec![LogicalType::ANY],
        LogicalType::ANY,
        bind_first::<LAST, SKIP_NULLS>,
    ));
    set.renamed(name)
}

/// The `first` set.
pub fn first_functions() -> AggregateFunctionSet {
    first_set::<false, false>("first")
}

/// The `last` set.
pub fn last_functions() -> AggregateFunctionSet {
    first_set::<true, false>("last")
}

/// The `any_value` set.
pub fn any_value_functions() -> AggregateFunctionSet {
    first_set::<false, true>("any_value")
}
