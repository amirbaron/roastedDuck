//! Public C API controls for the frozen `DBInstanceCache` case-folding contract.
#![allow(unsafe_code)]

use duckdb::{
    DuckdbResult, DuckdbState, duckdb_close, duckdb_connect, duckdb_create_instance_cache,
    duckdb_destroy_instance_cache, duckdb_destroy_result, duckdb_disconnect, duckdb_get_or_create_from_cache,
    duckdb_query, duckdb_value_int64,
};
use std::ffi::CStr;
use std::ptr;

fn shared_marker(first: &CStr, second: &CStr) -> bool {
    // SAFETY: every output slot is writable, handles remain live until cleanup,
    // and result reads follow successful queries. All strings are NUL-terminated.
    unsafe {
        let mut cache = duckdb_create_instance_cache();
        let mut db1 = ptr::null_mut();
        let mut db2 = ptr::null_mut();
        let mut con1 = ptr::null_mut();
        let mut con2 = ptr::null_mut();
        assert_eq!(
            duckdb_get_or_create_from_cache(
                cache,
                first.as_ptr(),
                &raw mut db1,
                ptr::null_mut(),
                ptr::null_mut()
            ),
            DuckdbState::DuckDBSuccess
        );
        assert_eq!(duckdb_connect(db1, &raw mut con1), DuckdbState::DuckDBSuccess);
        let mut result = DuckdbResult {
            deprecated_column_count: 0,
            deprecated_row_count: 0,
            deprecated_rows_changed: 0,
            deprecated_columns: ptr::null_mut(),
            deprecated_error_message: ptr::null_mut(),
            internal_data: ptr::null_mut(),
        };
        assert_eq!(
            duckdb_query(
                con1,
                c"CREATE TABLE marker AS SELECT 42 AS value".as_ptr(),
                &raw mut result
            ),
            DuckdbState::DuckDBSuccess
        );
        duckdb_destroy_result(&raw mut result);
        assert_eq!(
            duckdb_get_or_create_from_cache(
                cache,
                second.as_ptr(),
                &raw mut db2,
                ptr::null_mut(),
                ptr::null_mut()
            ),
            DuckdbState::DuckDBSuccess
        );
        assert_eq!(duckdb_connect(db2, &raw mut con2), DuckdbState::DuckDBSuccess);
        let status = duckdb_query(con2, c"SELECT value FROM marker".as_ptr(), &raw mut result);
        let shared = status == DuckdbState::DuckDBSuccess && duckdb_value_int64(&raw mut result, 0, 0) == 42;
        duckdb_destroy_result(&raw mut result);
        duckdb_disconnect(&raw mut con2);
        duckdb_close(&raw mut db2);
        duckdb_disconnect(&raw mut con1);
        duckdb_close(&raw mut db1);
        duckdb_destroy_instance_cache(&raw mut cache);
        shared
    }
}

#[test]
fn identical_named_memory_paths_share_the_instance() {
    assert!(shared_marker(c":memory:CaseProbe", c":memory:CaseProbe"));
}

#[test]
fn named_memory_case_matches_the_frozen_platform_contract() {
    let shared = shared_marker(c":memory:CaseProbe", c":memory:caseprobe");
    // Frozen C++ folds names on Apple/Windows, including named memory paths.
    #[cfg(any(target_vendor = "apple", windows))]
    assert!(shared);
    #[cfg(not(any(target_vendor = "apple", windows)))]
    assert!(!shared);
}

#[test]
fn unnamed_memory_instances_remain_independent() {
    assert!(!shared_marker(c":memory:", c":memory:"));
}
