//! The profile of a connection's latest query (DuckDB's `capi/profiling_info-c.cpp`).
//!
//! A `duckdb_profiling_info` points into the profile the connection handle keeps; it is
//! valid until the connection runs its next query, like DuckDB's pointer into the
//! profiler's tree.

use std::ffi::c_char;
use std::sync::{Arc, PoisonError};

use roastedduck::ProfilingTree;
use roastedduck_types::{LogicalType, Value};

use crate::helper::{catch, opt_str};
use crate::open::DuckdbConnection;
use crate::types::*;
use crate::value::{DuckdbValue, wrap_value};

/// `duckdb_profiling_info`
pub type DuckdbProfilingInfo = *const ProfilingTree;

/// `duckdb_profiling_info duckdb_get_profiling_info(duckdb_connection connection)`
/// (capi/profiling_info-c.cpp): NULL if profiling is off or the latest query was not
/// profiled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_profiling_info(connection: DuckdbConnection) -> DuckdbProfilingInfo {
    // SAFETY: `connection` is NULL or a live handle.
    let Some(wrapper) = (unsafe { connection.as_ref() }) else {
        return std::ptr::null();
    };
    let Ok(Some(tree)) = catch(|| Ok(wrapper.context.lock().profiling_tree())) else {
        return std::ptr::null();
    };
    let pointer = Arc::as_ptr(&tree);
    *wrapper.profiling.lock().unwrap_or_else(PoisonError::into_inner) = Some(tree);
    pointer
}

/// The profile node behind a handle.
///
/// # Safety
/// `info` must be NULL or a `duckdb_profiling_info` whose connection has not run another
/// query since.
unsafe fn node<'a>(info: DuckdbProfilingInfo) -> Option<&'a ProfilingTree> {
    // SAFETY: guaranteed by the caller.
    unsafe { info.as_ref() }
}

/// `duckdb_value duckdb_profiling_info_get_value(duckdb_profiling_info info, const char *key)`
/// (capi/profiling_info-c.cpp): the metric's value as text, NULL if the node does not show
/// it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_profiling_info_get_value(
    info: DuckdbProfilingInfo,
    key: *const c_char,
) -> DuckdbValue {
    // SAFETY: `info` is NULL or a live node; `key` is NULL or a C string.
    let (Some(node), Some(key)) = (unsafe { node(info) }, unsafe { opt_str(key) }) else {
        return std::ptr::null_mut();
    };
    node.metrics
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map_or(std::ptr::null_mut(), |(_, text)| wrap_value(Value::varchar(text)))
}

/// `duckdb_value duckdb_profiling_info_get_metrics(duckdb_profiling_info info)`
/// (capi/profiling_info-c.cpp): the metrics the node shows, as a `MAP(VARCHAR, VARCHAR)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_profiling_info_get_metrics(info: DuckdbProfilingInfo) -> DuckdbValue {
    // SAFETY: `info` is NULL or a live node.
    let Some(node) = (unsafe { node(info) }) else {
        return std::ptr::null_mut();
    };
    let entries = node
        .metrics
        .iter()
        .map(|(name, text)| (Value::varchar(name), Value::varchar(text)))
        .collect();
    wrap_value(Value::map(LogicalType::VARCHAR, LogicalType::VARCHAR, entries))
}

/// `idx_t duckdb_profiling_info_get_child_count(duckdb_profiling_info info)`
/// (capi/profiling_info-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_profiling_info_get_child_count(info: DuckdbProfilingInfo) -> Idx {
    // SAFETY: `info` is NULL or a live node.
    unsafe { node(info) }.map_or(0, |node| node.children.len() as Idx)
}

/// `duckdb_profiling_info duckdb_profiling_info_get_child(duckdb_profiling_info info,
/// idx_t index)` (capi/profiling_info-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_profiling_info_get_child(
    info: DuckdbProfilingInfo,
    index: Idx,
) -> DuckdbProfilingInfo {
    // SAFETY: `info` is NULL or a live node.
    unsafe { node(info) }
        .and_then(|node| node.children.get(index as usize))
        .map_or(std::ptr::null(), std::ptr::from_ref)
}
