//! Bound expressions handed to bind callbacks (DuckDB's `capi/expression-c.cpp`).

use std::sync::Arc;

use rustdb_planner::{Expression, ExpressionEvaluator};

use crate::error_data::{DuckdbErrorData, ErrorDataWrapper};
use crate::helper::catch;
use crate::logical_types::{DuckdbLogicalType, wrap_type};
use crate::open::DuckdbClientContext;
use crate::value::{DuckdbValue, wrap_value};

/// What a `duckdb_expression` points to (DuckDB's `ExpressionWrapper`): a copy of a bound
/// expression, with the evaluator of the binder that bound it, which folds it.
#[derive(Debug)]
pub struct ExpressionWrapper {
    pub(crate) expr: Expression,
    pub(crate) evaluator: Arc<dyn ExpressionEvaluator>,
}

/// `duckdb_expression`
pub type DuckdbExpression = *mut ExpressionWrapper;

/// Hands a copy of `expr` to C.
pub(crate) fn wrap_expression(expr: Expression, evaluator: Arc<dyn ExpressionEvaluator>) -> DuckdbExpression {
    Box::into_raw(Box::new(ExpressionWrapper { expr, evaluator }))
}

/// `void duckdb_destroy_expression(duckdb_expression *expr)` (capi/expression-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_expression(expr: *mut DuckdbExpression) {
    // SAFETY: `expr` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = expr.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `duckdb_logical_type duckdb_expression_return_type(duckdb_expression expr)`
/// (capi/expression-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_expression_return_type(expr: DuckdbExpression) -> DuckdbLogicalType {
    // SAFETY: `expr` is NULL or a live handle.
    unsafe { expr.as_ref() }.map_or(std::ptr::null_mut(), |wrapper| {
        wrap_type(wrapper.expr.return_type.clone())
    })
}

/// `bool duckdb_expression_is_foldable(duckdb_expression expr)` (capi/expression-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_expression_is_foldable(expr: DuckdbExpression) -> bool {
    // SAFETY: `expr` is NULL or a live handle.
    unsafe { expr.as_ref() }.is_some_and(|wrapper| wrapper.expr.is_foldable())
}

/// `duckdb_error_data duckdb_expression_fold(duckdb_client_context context,
/// duckdb_expression expr, duckdb_value *out_value)` (capi/expression-c.cpp): evaluates a
/// foldable expression (`ExpressionExecutor::EvaluateScalar`). An expression that is not
/// foldable leaves `out_value` untouched and reports no error; a failed evaluation
/// reports an `INVALID_INPUT` error whose message is the failure's `what()` text. The
/// expression is evaluated by the binder that bound it, so `context` is not needed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_expression_fold(
    _context: DuckdbClientContext,
    expr: DuckdbExpression,
    out_value: *mut DuckdbValue,
) -> DuckdbErrorData {
    // SAFETY: `expr` is NULL or a live handle.
    let Some(wrapper) = (unsafe { expr.as_ref() }) else {
        return std::ptr::null_mut();
    };
    if !wrapper.expr.is_foldable() {
        return std::ptr::null_mut();
    }
    match catch(|| wrapper.evaluator.evaluate_scalar(&wrapper.expr)) {
        Ok(value) => {
            if !out_value.is_null() {
                // SAFETY: `out_value` is writable per the API contract.
                unsafe { *out_value = wrap_value(value) };
            }
            std::ptr::null_mut()
        }
        Err(error) => {
            let what = error.into_json().raw_message_bytes().to_vec();
            ErrorDataWrapper::new(Some(rustdb_common::Error::new_bytes(
                rustdb_common::ErrorKind::InvalidInput,
                what,
            )))
            .into_handle()
        }
    }
}
