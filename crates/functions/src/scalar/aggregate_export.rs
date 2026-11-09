//! `finalize` and `combine` (DuckDB's `function/scalar/system/aggregate_export.cpp`):
//! read back the `AGGREGATE_STATE` values of `EXPORT_STATE` aggregates.

use std::any::Any;
use std::sync::Arc;

use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_function::aggregate::{AggregateFunction, AggregateInput, OwnedStates};
use roastedduck_function::{
    BindInput, FunctionData, FunctionState, NullHandling, ScalarFunction, ScalarFunctionSet,
};
use roastedduck_types::{DataChunk, LogicalType, LogicalTypeId, StringT, TypeInfo, Vector};

use crate::scalar::executor::addressable;

/// The aggregate whose states are read (`ExportAggregateBindData`).
#[derive(Debug)]
struct ExportAggregateBindData {
    aggregate: AggregateFunction,
    state_size: usize,
}

impl FunctionData for ExportAggregateBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `BindAggregateState`: binds the aggregate named by the state type again.
fn bind_aggregate_state(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let state_type = input
        .argument_types
        .first()
        .cloned()
        .unwrap_or(LogicalType::SQLNULL);
    for argument in &mut function.arguments {
        *argument = state_type.clone();
    }
    let Some(TypeInfo::AggregateState(state)) = state_type
        .info()
        .filter(|_| state_type.id() == LogicalTypeId::AggregateState)
    else {
        return Err(Error::binder(format!(
            "Can only FINALIZE aggregate state, not {state_type}"
        )));
    };
    if let Some(other) = input.argument_types.get(1)
        && *other != state_type
        && other.id() != LogicalTypeId::Blob
    {
        return Err(Error::binder(format!(
            "Cannot COMBINE aggregate states from different functions, {state_type} <> {other}"
        )));
    }
    let resolver = input
        .aggregates
        .ok_or_else(|| Error::internal("finalize bound without an aggregate resolver"))?;
    let arguments: Vec<(LogicalType, Option<roastedduck_types::Value>)> = state
        .bound_argument_types
        .iter()
        .map(|ty| (ty.clone(), None))
        .collect();
    let bound = resolver
        .bind_aggregate(&state.function_name, &arguments)
        .map_err(|error| {
            Error::internal(format!(
                "Could not re-bind exported aggregate {}: {}",
                state.function_name,
                error.raw_message()
            ))
        })?;
    if bound.bind_data.is_some() {
        return Err(Error::binder(
            "Aggregate function with bind info not supported yet in aggregate state export",
        ));
    }
    let aggregate = bound.function;
    if aggregate.return_type != state.return_type || aggregate.arguments != state.bound_argument_types {
        return Err(Error::internal(format!(
            "Type mismatch for exported aggregate {}",
            state.function_name
        )));
    }
    function.return_type = if function.name.as_bytes() == b"finalize" {
        aggregate.return_type.clone()
    } else {
        state_type.clone()
    };
    let state_size = aggregate.ops.state_layout().size();
    Ok(Some(Arc::new(ExportAggregateBindData {
        aggregate,
        state_size,
    })))
}

fn bind_data(state: &FunctionState) -> Result<&ExportAggregateBindData> {
    state
        .bind_data
        .as_deref()
        .and_then(|data| data.as_any().downcast_ref::<ExportAggregateBindData>())
        .ok_or_else(|| Error::internal("aggregate state function without bind data"))
}

/// Copies an exported state into `target`, a state of the aggregate.
fn load_state(target: roastedduck_function::aggregate::StatePtr, bytes: &[u8]) {
    // SAFETY: `target` is a state of the aggregate, `state_size` bytes long, and the
    // callers checked that `bytes` has that length.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.as_ptr(), bytes.len()) };
}

/// `AggregateStateFinalize`.
fn aggregate_state_finalize(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let data = bind_data(state)?;
    let count = args.len();
    let input = addressable(args.column(0), count);
    let states = OwnedStates::new(Arc::clone(&data.aggregate.ops), count);
    let pointers = states.pointers();
    let mut nulls = Vec::new();
    for (row, &pointer) in pointers.iter().enumerate() {
        if input.is_null(row) {
            // finalize does not understand NULLs: the initialised state stands in.
            nulls.push(row);
            continue;
        }
        let bytes = input.string_bytes(row);
        if bytes.len() != data.state_size {
            return Err(Error::new(
                ErrorKind::Io,
                format!(
                    "Aggregate state size mismatch, expect {}, got {}",
                    data.state_size,
                    bytes.len()
                ),
            ));
        }
        load_state(pointer, bytes);
    }
    *result = Vector::flat(result.logical_type().clone(), count);
    // SAFETY: the states were initialised by `OwnedStates` and hold the exported bytes.
    unsafe {
        data.aggregate
            .ops
            .finalize(&pointers, AggregateInput { bind_data: None }, result, 0)?;
    }
    for row in nulls {
        result.set_null(row);
    }
    Ok(())
}

/// `AggregateStateCombine`.
fn aggregate_state_combine(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let data = bind_data(state)?;
    let count = args.len();
    let left = addressable(args.column(0), count);
    let right = addressable(args.column(1), count);
    *result = Vector::flat(result.logical_type().clone(), count);
    let buffers = OwnedStates::new(Arc::clone(&data.aggregate.ops), 2);
    let pointers = buffers.pointers();
    for row in 0..count {
        let value = match (!left.is_null(row), !right.is_null(row)) {
            // If both are NULL, the result is NULL; if one is, the result is the other.
            (false, false) => {
                result.set_null(row);
                continue;
            }
            (true, false) => result.add_string(left.string_bytes(row)),
            (false, true) => result.add_string(right.string_bytes(row)),
            (true, true) => {
                let (source, target) = (left.string_bytes(row), right.string_bytes(row));
                if source.len() != data.state_size || target.len() != data.state_size {
                    return Err(Error::new(
                        ErrorKind::Io,
                        format!(
                            "Aggregate state size mismatch, expect {}, got {} and {}",
                            data.state_size,
                            source.len(),
                            target.len()
                        ),
                    ));
                }
                load_state(pointers[0], source);
                load_state(pointers[1], target);
                // SAFETY: both states are initialised states of the aggregate.
                unsafe {
                    data.aggregate.ops.combine(
                        &pointers[..1],
                        &pointers[1..],
                        AggregateInput { bind_data: None },
                    )?;
                }
                // SAFETY: the combined state is `state_size` initialised bytes.
                let bytes =
                    unsafe { std::slice::from_raw_parts(pointers[1].as_ptr().cast_const(), data.state_size) };
                result.add_string(bytes)
            }
        };
        result.values_mut::<StringT>()[row] = value;
    }
    Ok(())
}

/// `FinalizeFun` and `CombineFun`.
pub fn aggregate_export_functions() -> Vec<ScalarFunctionSet> {
    let state = LogicalType::simple(LogicalTypeId::AggregateState);
    let make = |name: &'static str, arguments: Vec<LogicalType>, return_type: LogicalType, kernel| {
        let mut function = ScalarFunction::new(name, arguments, return_type, kernel)
            .with_bind(bind_aggregate_state)
            // `ExportStateScalarSerialize`.
            .with_serialize(|_, _, _| Err(Error::not_implemented("FIXME: export state serialize")));
        function.null_handling = NullHandling::Special;
        let mut set = ScalarFunctionSet::new(name);
        set.add(function);
        set
    };
    vec![
        make(
            "finalize",
            vec![state.clone()],
            LogicalType::INVALID,
            aggregate_state_finalize as fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()>,
        ),
        make(
            "combine",
            vec![state.clone(), LogicalType::ANY],
            state,
            aggregate_state_combine,
        ),
    ]
}
