//! Threads a client lends the database (DuckDB's `capi/threading-c.cpp`): they run
//! queued tasks of the database's queries.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rustdb::Database;

use crate::open::{DuckdbConnection, DuckdbDatabase, connection_ref};
use crate::types::*;

/// What a `duckdb_task_state` points to (DuckDB's `CAPITaskState`): the database, the
/// marker that keeps its threads running and how many threads entered
/// `duckdb_execute_tasks_state`.
#[derive(Debug)]
pub struct CAPITaskState {
    database: Arc<Database>,
    marker: AtomicBool,
    execute_count: AtomicU64,
}

/// `duckdb_task_state`
pub type DuckdbTaskState = *mut CAPITaskState;

/// `void duckdb_execute_tasks(duckdb_database database, idx_t max_tasks)`
/// (capi/threading-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_execute_tasks(database: DuckdbDatabase, max_tasks: Idx) {
    // SAFETY: `database` is NULL or a live handle.
    if let Some(wrapper) = unsafe { database.as_ref() } {
        wrapper
            .database
            .execute_tasks(None, usize::try_from(max_tasks).unwrap_or(usize::MAX));
    }
}

/// `duckdb_task_state duckdb_create_task_state(duckdb_database database)`
/// (capi/threading-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_task_state(database: DuckdbDatabase) -> DuckdbTaskState {
    // SAFETY: `database` is NULL or a live handle.
    unsafe { database.as_ref() }.map_or(std::ptr::null_mut(), |wrapper| {
        Box::into_raw(Box::new(CAPITaskState {
            database: Arc::clone(&wrapper.database),
            marker: AtomicBool::new(true),
            execute_count: AtomicU64::new(0),
        }))
    })
}

/// `void duckdb_execute_tasks_state(duckdb_task_state state)` (capi/threading-c.cpp): runs
/// tasks until `duckdb_finish_execution`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_execute_tasks_state(state: DuckdbTaskState) {
    // SAFETY: `state` is NULL or a live handle.
    if let Some(state) = unsafe { state.as_ref() } {
        state.execute_count.fetch_add(1, Ordering::AcqRel);
        state.database.execute_tasks_forever(&state.marker);
    }
}

/// `idx_t duckdb_execute_n_tasks_state(duckdb_task_state state, idx_t max_tasks)`
/// (capi/threading-c.cpp): runs at most `max_tasks` tasks, stopping early when none is
/// queued or the execution is finished; returns how many ran.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_execute_n_tasks_state(state: DuckdbTaskState, max_tasks: Idx) -> Idx {
    // SAFETY: `state` is NULL or a live handle.
    unsafe { state.as_ref() }.map_or(0, |state| {
        state.database.execute_tasks(
            Some(&state.marker),
            usize::try_from(max_tasks).unwrap_or(usize::MAX),
        ) as Idx
    })
}

/// `void duckdb_finish_execution(duckdb_task_state state)` (capi/threading-c.cpp): the
/// threads of the state return once their task is done.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_finish_execution(state: DuckdbTaskState) {
    // SAFETY: `state` is NULL or a live handle.
    if let Some(state) = unsafe { state.as_ref() } {
        state.marker.store(false, Ordering::Release);
        if state.execute_count.load(Ordering::Acquire) > 0 {
            state.database.signal_task_threads();
        }
    }
}

/// `bool duckdb_task_state_is_finished(duckdb_task_state state)` (capi/threading-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_task_state_is_finished(state: DuckdbTaskState) -> bool {
    // SAFETY: `state` is NULL or a live handle.
    unsafe { state.as_ref() }.is_some_and(|state| !state.marker.load(Ordering::Acquire))
}

/// `void duckdb_destroy_task_state(duckdb_task_state state)` (capi/threading-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_task_state(state: DuckdbTaskState) {
    if !state.is_null() {
        // SAFETY: `state` is a handle owned by the caller.
        drop(unsafe { Box::from_raw(state) });
    }
}

/// `bool duckdb_execution_is_finished(duckdb_connection con)` (capi/threading-c.cpp):
/// whether the connection's pending query finished executing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_execution_is_finished(con: DuckdbConnection) -> bool {
    // SAFETY: `con` is NULL or a live handle.
    unsafe { connection_ref(con) }.is_some_and(|context| context.lock().execution_is_finished())
}
