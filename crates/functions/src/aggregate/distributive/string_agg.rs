//! `string_agg`/`listagg`/`group_concat` (DuckDB's `string_agg.cpp`).

use std::sync::Arc;

use roastedduck_common::{Error, Result};
use roastedduck_function::aggregate::{Aggregate, AggregateInput, States, Typed};
use roastedduck_function::{AggregateFunction, AggregateFunctionSet, BindInput, FunctionData};
use roastedduck_serde::Serializer;
use roastedduck_types::{LogicalType, LogicalTypeId, StringT, Vector};

use crate::signature::any_params;

/// The separator, or `None` when it is NULL (the result is then NULL).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Separator(Option<String>);

impl FunctionData for Separator {
    fn equals(&self, other: &dyn FunctionData) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self == other)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

struct StringAgg;

impl StringAgg {
    fn append(state: &mut Option<Vec<u8>>, value: &[u8], separator: &[u8]) {
        match state {
            None => *state = Some(value.to_vec()),
            Some(buffer) => {
                buffer.extend_from_slice(separator);
                buffer.extend_from_slice(value);
            }
        }
    }
}

impl Aggregate for StringAgg {
    type State = Option<Vec<u8>>;

    fn initialize(&self) -> Self::State {
        None
    }

    fn update(
        &self,
        inputs: &[Vector],
        input: AggregateInput<'_>,
        states: &mut States<'_, Self::State>,
        count: usize,
    ) -> Result<()> {
        let Some(separator) = &input.bind_data::<Separator>()?.0 else {
            return Ok(());
        };
        let vector = &inputs[0];
        let values = vector.unified::<StringT>();
        for row in 0..count {
            if let Some(value) = values.get(row) {
                Self::append(states.get(row), vector.string_value(&value), separator.as_bytes());
            }
        }
        Ok(())
    }

    fn combine(
        &self,
        source: &Self::State,
        target: &mut Self::State,
        input: AggregateInput<'_>,
    ) -> Result<()> {
        if let (Some(value), Some(separator)) = (source, &input.bind_data::<Separator>()?.0) {
            Self::append(target, value, separator.as_bytes());
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
        if let Some(value) = state {
            let string = result.add_string(value);
            result.values_mut::<StringT>()[row] = string;
        } else {
            result.set_null(row);
        }
        Ok(())
    }
}

/// DuckDB's `StringAggBind`: the separator must be a constant and is removed from the
/// arguments.
fn bind_string_agg(
    function: &mut AggregateFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    if input.argument_types.len() == 1 {
        return Ok(Some(Arc::new(Separator(Some(",".to_owned())))));
    }
    if input
        .argument_types
        .iter()
        .any(|ty| ty.id() == LogicalTypeId::Unknown)
    {
        return Err(crate::parameter_not_resolved());
    }
    let Some(separator) = &input.constants[1] else {
        return Err(Error::binder(
            "Separator argument to StringAgg must be a constant",
        ));
    };
    let separator = if separator.is_null() {
        None
    } else {
        Some(separator.to_varchar()?)
    };
    function.erase_argument(1);
    Ok(Some(Arc::new(Separator(separator))))
}

/// DuckDB's `StringAggSerialize`: the separator (`,` for a NULL one).
fn serialize_string_agg(
    serializer: &mut dyn Serializer,
    bind_data: Option<&dyn FunctionData>,
    _: &AggregateFunction,
) -> Result<()> {
    let separator = bind_data
        .and_then(|data| data.as_any().downcast_ref::<Separator>())
        .and_then(|separator| separator.0.as_deref())
        .unwrap_or(",");
    serializer.write_property(100, "separator", separator)
}

/// The `string_agg` set: `string_agg(ANY)` and `string_agg(ANY, VARCHAR)`.
pub fn string_agg_functions() -> AggregateFunctionSet {
    let mut set = AggregateFunctionSet::new("string_agg");
    let function = AggregateFunction::new(
        "string_agg",
        vec![any_params(LogicalType::VARCHAR)],
        LogicalType::VARCHAR,
        Typed(StringAgg),
    )
    .with_bind(bind_string_agg)
    .without_state_destructor();
    let mut function = function;
    function.serialize = Some(Arc::new(serialize_string_agg));
    let mut with_separator = function.clone();
    with_separator.arguments.push(LogicalType::VARCHAR);
    set.add(function);
    set.add(with_separator);
    set
}
