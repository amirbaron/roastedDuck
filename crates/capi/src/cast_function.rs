//! Casts defined by the client (DuckDB's `capi/cast_function-c.cpp`).

use std::ffi::{c_char, c_void};
use std::sync::Arc;

use rustdb_common::Result;
use rustdb_function::cast::{BoundCastInfo, CastParameters};
use rustdb_types::{LogicalType, STANDARD_VECTOR_SIZE, Vector};

use crate::data_chunk::DuckdbVector;
use crate::function::{
    DuckdbDeleteCallback, DuckdbFunctionInfo, UserData, error_bytes, is_valid_return_type,
};
use crate::helper::catch;
use crate::logical_types::{DuckdbLogicalType, type_ref};
use crate::open::{DuckdbConnection, connection_ref};
use crate::types::*;

/// `duckdb_cast_function_t`
pub type DuckdbCastFunctionT =
    Option<unsafe extern "C" fn(DuckdbFunctionInfo, Idx, DuckdbVector, DuckdbVector) -> bool>;

/// `duckdb_cast_mode`
pub type DuckdbCastMode = u32;

pub const DUCKDB_CAST_NORMAL: DuckdbCastMode = 0;
pub const DUCKDB_CAST_TRY: DuckdbCastMode = 1;

/// What a `duckdb_cast_function` points to (DuckDB's `CCastFunction`).
#[derive(Debug)]
pub struct CCastFunction {
    source_type: Option<LogicalType>,
    target_type: Option<LogicalType>,
    implicit_cast_cost: i64,
    function: DuckdbCastFunctionT,
    extra_info: *mut c_void,
    delete_callback: DuckdbDeleteCallback,
}

/// `duckdb_cast_function`
pub type DuckdbCastFunction = *mut CCastFunction;

/// What a `duckdb_function_info` of a cast points to (DuckDB's `CCastExecuteInfo`).
struct CCastExecuteInfo<'a> {
    parameters: &'a CastParameters,
    extra_info: *mut c_void,
    error_message: Option<Vec<u8>>,
}

/// The execute info behind a handle.
///
/// # Safety
/// `info` must be NULL or the `duckdb_function_info` of a running cast callback.
unsafe fn execute_info<'a>(info: DuckdbFunctionInfo) -> Option<&'a mut CCastExecuteInfo<'a>> {
    // SAFETY: guaranteed by the caller.
    unsafe { info.cast::<CCastExecuteInfo<'a>>().as_mut() }
}

/// `CAPICastFunction`: calls the client's cast on the flattened input. A failure is
/// reported with the message the client set (`HandleCastError::AssignError`): an error
/// under `CAST`, NULL rows under `TRY_CAST`. A constant input of one row gives a constant
/// result.
fn capi_cast_function(
    function: unsafe extern "C" fn(DuckdbFunctionInfo, Idx, DuckdbVector, DuckdbVector) -> bool,
    extra_info: &UserData,
    input: &Vector,
    output: &mut Vector,
    count: usize,
    parameters: &mut CastParameters,
) -> Result<bool> {
    let is_constant = input.is_constant();
    let mut input = input.flattened(count);
    *output = Vector::flat(output.logical_type().clone(), STANDARD_VECTOR_SIZE.max(count));
    let mut info = CCastExecuteInfo {
        parameters,
        extra_info: extra_info.get(),
        error_message: None,
    };
    // SAFETY: the client's callback receives the execute info and both vectors, valid for
    // the call.
    let success = unsafe {
        function(
            std::ptr::from_mut(&mut info).cast(),
            count as Idx,
            &raw mut input,
            std::ptr::from_mut(output),
        )
    };
    let error_message = info.error_message.unwrap_or_default();
    if !success {
        parameters.assign_error_bytes(error_message)?;
    }
    if is_constant && count == 1 && (success || !parameters.strict) {
        output.make_constant();
    }
    Ok(success)
}

/// The definition behind a handle.
///
/// # Safety
/// `cast_function` must be NULL or a live `duckdb_cast_function`.
unsafe fn definition<'a>(cast_function: DuckdbCastFunction) -> Option<&'a mut CCastFunction> {
    // SAFETY: guaranteed by the caller.
    unsafe { cast_function.as_mut() }
}

/// `duckdb_cast_function duckdb_create_cast_function()` (capi/cast_function-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_cast_function() -> DuckdbCastFunction {
    Box::into_raw(Box::new(CCastFunction {
        source_type: None,
        target_type: None,
        implicit_cast_cost: -1,
        function: None,
        extra_info: std::ptr::null_mut(),
        delete_callback: None,
    }))
}

/// `void duckdb_cast_function_set_source_type(duckdb_cast_function cast_function,
/// duckdb_logical_type source_type)` (capi/cast_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_cast_function_set_source_type(
    cast_function: DuckdbCastFunction,
    source_type: DuckdbLogicalType,
) {
    // SAFETY: both handles are NULL or live.
    if let (Some(cast), Some(ty)) = unsafe { (definition(cast_function), type_ref(source_type)) } {
        cast.source_type = Some(ty.clone());
    }
}

/// `void duckdb_cast_function_set_target_type(duckdb_cast_function cast_function,
/// duckdb_logical_type target_type)` (capi/cast_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_cast_function_set_target_type(
    cast_function: DuckdbCastFunction,
    target_type: DuckdbLogicalType,
) {
    // SAFETY: both handles are NULL or live.
    if let (Some(cast), Some(ty)) = unsafe { (definition(cast_function), type_ref(target_type)) } {
        cast.target_type = Some(ty.clone());
    }
}

/// `void duckdb_cast_function_set_implicit_cast_cost(duckdb_cast_function cast_function,
/// int64_t cost)` (capi/cast_function-c.cpp): `-1` (the default) allows the cast only
/// explicitly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_cast_function_set_implicit_cast_cost(
    cast_function: DuckdbCastFunction,
    cost: i64,
) {
    // SAFETY: the handle is NULL or live.
    if let Some(cast) = unsafe { definition(cast_function) } {
        cast.implicit_cast_cost = cost;
    }
}

/// `void duckdb_cast_function_set_function(duckdb_cast_function cast_function,
/// duckdb_cast_function_t function)` (capi/cast_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_cast_function_set_function(
    cast_function: DuckdbCastFunction,
    function: DuckdbCastFunctionT,
) {
    // SAFETY: the handle is NULL or live.
    if let (Some(cast), Some(function)) = (unsafe { definition(cast_function) }, function) {
        cast.function = Some(function);
    }
}

/// `duckdb_cast_mode duckdb_cast_function_get_cast_mode(duckdb_function_info info)`
/// (capi/cast_function-c.cpp): `DUCKDB_CAST_TRY` under `TRY_CAST`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_cast_function_get_cast_mode(info: DuckdbFunctionInfo) -> DuckdbCastMode {
    // SAFETY: `info` is NULL or the info of a running cast callback.
    match unsafe { execute_info(info) } {
        Some(info) if info.parameters.is_try_cast() => DUCKDB_CAST_TRY,
        _ => DUCKDB_CAST_NORMAL,
    }
}

/// `void duckdb_cast_function_set_error(duckdb_function_info info, const char *error)`
/// (capi/cast_function-c.cpp): the message a failed cast reports.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_cast_function_set_error(info: DuckdbFunctionInfo, error: *const c_char) {
    // SAFETY: `info` is NULL or the info of a running cast callback.
    if let Some(info) = unsafe { execute_info(info) }
        && !error.is_null()
    {
        // SAFETY: `error` is a C string per the API contract.
        info.error_message = Some(unsafe { error_bytes(error) });
    }
}

/// `void duckdb_cast_function_set_row_error(duckdb_function_info info, const char *error,
/// idx_t row, duckdb_vector output)` (capi/cast_function-c.cpp): the message a failed
/// cast reports, and row `row` of `output` becomes NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_cast_function_set_row_error(
    info: DuckdbFunctionInfo,
    error: *const c_char,
    row: Idx,
    output: DuckdbVector,
) {
    // SAFETY: forwarded API contract.
    unsafe { duckdb_cast_function_set_error(info, error) };
    // SAFETY: `output` is NULL or the cast's live output vector.
    if let Some(output) = unsafe { output.as_mut() } {
        output.set_null(row as usize);
    }
}

/// `void duckdb_cast_function_set_extra_info(duckdb_cast_function cast_function,
/// void *extra_info, duckdb_delete_callback_t destroy)` (capi/cast_function-c.cpp):
/// a registered cast releases it with `destroy` once the cast is gone.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_cast_function_set_extra_info(
    cast_function: DuckdbCastFunction,
    extra_info: *mut c_void,
    destroy: DuckdbDeleteCallback,
) {
    // SAFETY: the handle is NULL or live.
    if let Some(cast) = unsafe { definition(cast_function) }
        && !extra_info.is_null()
    {
        cast.extra_info = extra_info;
        cast.delete_callback = destroy;
    }
}

/// `void *duckdb_cast_function_get_extra_info(duckdb_function_info info)`
/// (capi/cast_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_cast_function_get_extra_info(info: DuckdbFunctionInfo) -> *mut c_void {
    // SAFETY: `info` is NULL or the info of a running cast callback.
    unsafe { execute_info(info) }.map_or(std::ptr::null_mut(), |info| info.extra_info)
}

/// `duckdb_state duckdb_register_cast_function(duckdb_connection con, duckdb_cast_function
/// cast_function)` (capi/cast_function-c.cpp): needs resolved source and target types and
/// a function; the extra info is from then on owned by the registered cast.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_register_cast_function(
    con: DuckdbConnection,
    cast_function: DuckdbCastFunction,
) -> DuckdbState {
    // SAFETY: both handles are NULL or live.
    let (Some(context), Some(cast)) = (unsafe { connection_ref(con) }, unsafe { cast_function.as_ref() })
    else {
        return DuckdbState::DuckDBError;
    };
    let (Some(source), Some(target), Some(function)) = (&cast.source_type, &cast.target_type, cast.function)
    else {
        return DuckdbState::DuckDBError;
    };
    if !is_valid_return_type(source) || !is_valid_return_type(target) {
        return DuckdbState::DuckDBError;
    }
    let extra_info = Arc::new(UserData::new(cast.extra_info, cast.delete_callback));
    let bound = BoundCastInfo::new(move |input, output, count, parameters| {
        capi_cast_function(function, &extra_info, input, output, count, parameters)
    });
    let (source, target, cost) = (source.clone(), target.clone(), cast.implicit_cast_cost);
    let registered = catch(|| {
        context.lock().register_cast_function(source, target, bound, cost);
        Ok(())
    });
    DuckdbState::from_ok(registered.is_ok())
}

/// `void duckdb_destroy_cast_function(duckdb_cast_function *cast_function)`
/// (capi/cast_function-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_cast_function(cast_function: *mut DuckdbCastFunction) {
    // SAFETY: `cast_function` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = cast_function.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}
