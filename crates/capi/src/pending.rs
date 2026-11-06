//! Pending query results (DuckDB's `capi/pending-c.cpp`).

use std::ffi::c_char;
use std::sync::Arc;

use rustdb::{PendingExecutionResult, PendingQuery};

use crate::helper::catch;
use crate::open::SharedConnection;
use crate::prepared::{CError, DuckdbPreparedStatement};
use crate::result::translate_result;
use crate::types::*;

/// What a `duckdb_pending_result` points to (DuckDB's `PendingStatementWrapper`).
#[derive(Debug)]
pub struct PendingStatementWrapper {
    context: Arc<SharedConnection>,
    /// The pending query, its failure, or `None` once its result was taken.
    statement: Option<Result<PendingQuery, CError>>,
}

/// `duckdb_pending_result`
pub type DuckdbPendingResult = *mut PendingStatementWrapper;

/// Starts the prepared statement with its bound values (`duckdb_pending_prepared_internal`).
///
/// # Safety
/// As for `duckdb_pending_prepared`.
unsafe fn pending_prepared(
    prepared_statement: DuckdbPreparedStatement,
    out_result: *mut DuckdbPendingResult,
    allow_streaming: bool,
) -> DuckdbState {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    let Some(wrapper) = (unsafe { prepared_statement.as_ref() }) else {
        return DuckdbState::DuckDBError;
    };
    if out_result.is_null() {
        return DuckdbState::DuckDBError;
    }
    let pending = match &wrapper.statement {
        Ok(statement) => catch(|| {
            wrapper
                .context
                .lock()
                .pending_prepared(statement, &wrapper.values, allow_streaming)
        }),
        Err(_) => Err(rustdb::Error::invalid_input(
            "Attempting to execute an unsuccessfully prepared statement!",
        )),
    };
    let state = DuckdbState::from_ok(pending.is_ok());
    let pending = PendingStatementWrapper {
        context: Arc::clone(&wrapper.context),
        statement: Some(pending.map_err(CError::new)),
    };
    // SAFETY: checked non-null above.
    unsafe { *out_result = Box::into_raw(Box::new(pending)) };
    state
}

/// `duckdb_state duckdb_pending_prepared(duckdb_prepared_statement prepared_statement,
/// duckdb_pending_result *out_result)` (capi/pending-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_pending_prepared(
    prepared_statement: DuckdbPreparedStatement,
    out_result: *mut DuckdbPendingResult,
) -> DuckdbState {
    // SAFETY: forwarded API contract.
    unsafe { pending_prepared(prepared_statement, out_result, false) }
}

/// `duckdb_state duckdb_pending_prepared_streaming(duckdb_prepared_statement prepared_statement,
/// duckdb_pending_result *out_result)` (capi/pending-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_pending_prepared_streaming(
    prepared_statement: DuckdbPreparedStatement,
    out_result: *mut DuckdbPendingResult,
) -> DuckdbState {
    // SAFETY: forwarded API contract.
    unsafe { pending_prepared(prepared_statement, out_result, true) }
}

/// `void duckdb_destroy_pending(duckdb_pending_result *pending_result)` (capi/pending-c.cpp):
/// a query still pending is closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_pending(pending_result: *mut DuckdbPendingResult) {
    // SAFETY: `pending_result` is NULL or points to NULL or a handle owned by the caller.
    let wrapper = unsafe {
        let Some(slot) = pending_result.as_mut() else {
            return;
        };
        if slot.is_null() {
            return;
        }
        let wrapper = Box::from_raw(*slot);
        *slot = std::ptr::null_mut();
        wrapper
    };
    if let Some(Ok(pending)) = &wrapper.statement {
        wrapper.context.lock().close_query(pending.id());
    }
}

/// `const char *duckdb_pending_error(duckdb_pending_result pending_result)` (capi/pending-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_pending_error(pending_result: DuckdbPendingResult) -> *const c_char {
    // SAFETY: `pending_result` is NULL or a live handle.
    match unsafe { pending_result.as_ref() }.and_then(|wrapper| wrapper.statement.as_ref()) {
        Some(Err(error)) => error.message.as_ptr(),
        Some(Ok(_)) => c"".as_ptr(),
        None => std::ptr::null(),
    }
}

/// Works on the pending query (`PendingQueryResult::ExecuteTask`); a failure becomes the
/// pending result's error.
fn execute_task(wrapper: &mut PendingStatementWrapper) -> DuckdbPendingState {
    let Some(Ok(pending)) = &wrapper.statement else {
        return DUCKDB_PENDING_ERROR;
    };
    match catch(|| wrapper.context.lock().execute_task(pending)) {
        Ok(PendingExecutionResult::ResultReady | PendingExecutionResult::ExecutionFinished) => {
            DUCKDB_PENDING_RESULT_READY
        }
        Ok(PendingExecutionResult::ResultNotReady) => DUCKDB_PENDING_RESULT_NOT_READY,
        // `duckdb_pending_execute_task` reports blocked tasks as no tasks available.
        Ok(PendingExecutionResult::NoTasksAvailable | PendingExecutionResult::Blocked) => {
            DUCKDB_PENDING_NO_TASKS_AVAILABLE
        }
        Err(error) => {
            wrapper.statement = Some(Err(CError::new(error)));
            DUCKDB_PENDING_ERROR
        }
    }
}

/// `duckdb_pending_state duckdb_pending_execute_check_state(duckdb_pending_result pending_result)`
/// (capi/pending-c.cpp): the state without working on the query (`CheckPulse`); a query
/// that ran to completion reports `DUCKDB_PENDING_ERROR`, as in DuckDB.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_pending_execute_check_state(
    pending_result: DuckdbPendingResult,
) -> DuckdbPendingState {
    // SAFETY: `pending_result` is NULL or a live handle.
    let Some(wrapper) = (unsafe { pending_result.as_mut() }) else {
        return DUCKDB_PENDING_ERROR;
    };
    let Some(Ok(pending)) = &wrapper.statement else {
        return DUCKDB_PENDING_ERROR;
    };
    match catch(|| wrapper.context.lock().check_pending(pending)) {
        // `CheckPulse` reports a blocked query as ready.
        Ok(PendingExecutionResult::ResultReady | PendingExecutionResult::Blocked) => {
            DUCKDB_PENDING_RESULT_READY
        }
        Ok(PendingExecutionResult::ResultNotReady) => DUCKDB_PENDING_RESULT_NOT_READY,
        Ok(PendingExecutionResult::NoTasksAvailable) => DUCKDB_PENDING_NO_TASKS_AVAILABLE,
        Ok(PendingExecutionResult::ExecutionFinished) => DUCKDB_PENDING_ERROR,
        Err(error) => {
            wrapper.statement = Some(Err(CError::new(error)));
            DUCKDB_PENDING_ERROR
        }
    }
}

/// `duckdb_pending_state duckdb_pending_execute_task(duckdb_pending_result pending_result)`
/// (capi/pending-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_pending_execute_task(
    pending_result: DuckdbPendingResult,
) -> DuckdbPendingState {
    // SAFETY: `pending_result` is NULL or a live handle.
    unsafe { pending_result.as_mut() }.map_or(DUCKDB_PENDING_ERROR, execute_task)
}

/// `bool duckdb_pending_execution_is_finished(duckdb_pending_state pending_state)`
/// (capi/pending-c.cpp): whether the result is ready or the query failed
/// (`PendingQueryResult::IsResultReady`).
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_pending_execution_is_finished(pending_state: DuckdbPendingState) -> bool {
    !matches!(
        pending_state,
        DUCKDB_PENDING_RESULT_NOT_READY | DUCKDB_PENDING_NO_TASKS_AVAILABLE
    )
}

/// `duckdb_state duckdb_execute_pending(duckdb_pending_result pending_result,
/// duckdb_result *out_result)` (capi/pending-c.cpp): runs the query to completion; the
/// pending result is used up.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_execute_pending(
    pending_result: DuckdbPendingResult,
    out_result: *mut DuckdbResult,
) -> DuckdbState {
    // SAFETY: `pending_result` is NULL or a live handle.
    let Some(wrapper) = (unsafe { pending_result.as_mut() }) else {
        return DuckdbState::DuckDBError;
    };
    if out_result.is_null() {
        return DuckdbState::DuckDBError;
    }
    let Some(statement) = wrapper.statement.take() else {
        return DuckdbState::DuckDBError;
    };
    let (outcome, properties) = {
        let mut connection = wrapper.context.lock();
        let outcome = match statement {
            Ok(pending) => catch(|| connection.execute_pending(&pending)),
            Err(error) => {
                // PendingQueryResult::CheckExecutableInternal rejects a result
                // whose error was already observed by ExecuteTask/CheckPulse.
                // Execute without prior task failure still returns its own error.
                let mut message =
                    b"Attempting to execute an unsuccessful or closed pending query result\nError: ".to_vec();
                message.extend_from_slice(&error.error.to_bytes());
                Err(rustdb::Error::invalid_input(message))
            }
        };
        (outcome, connection.client_properties())
    };
    // SAFETY: checked non-null above.
    unsafe { translate_result(&wrapper.context, outcome, properties, out_result) }
}
