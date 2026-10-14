//! C-compatible type definitions mirroring `duckdb.h`.

use std::ffi::c_void;

use rustdb::LogicalTypeId;

/// `idx_t`
pub type Idx = u64;

/// `duckdb_state`
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuckdbState {
    DuckDBSuccess = 0,
    DuckDBError = 1,
}

/// `duckdb_type` (values from `DUCKDB_TYPE` in duckdb.h).
pub type DuckdbType = u32;

/// Maps a logical type id to its `DUCKDB_TYPE_*` value.
pub(crate) fn duckdb_type(id: LogicalTypeId) -> DuckdbType {
    use LogicalTypeId as L;
    match id {
        L::Boolean => 1,
        L::TinyInt => 2,
        L::SmallInt => 3,
        L::Integer => 4,
        L::BigInt => 5,
        L::UTinyInt => 6,
        L::USmallInt => 7,
        L::UInteger => 8,
        L::UBigInt => 9,
        L::Float => 10,
        L::Double => 11,
        L::Timestamp => 12,
        L::Date => 13,
        L::Time => 14,
        L::Interval => 15,
        L::HugeInt => 16,
        L::Varchar | L::Char => 17,
        L::Blob => 18,
        L::Decimal => 19,
        L::TimestampSec => 20,
        L::TimestampMs => 21,
        L::TimestampNs => 22,
        L::Enum => 23,
        L::List => 24,
        L::Struct => 25,
        L::Map => 26,
        L::Uuid => 27,
        L::Union => 28,
        L::Bit => 29,
        L::TimeTz => 30,
        L::TimestampTz => 31,
        L::UHugeInt => 32,
        L::Array => 33,
        L::Any => 34,
        L::Bignum => 35,
        L::SqlNull => 36,
        L::StringLiteral => 37,
        L::IntegerLiteral => 38,
        L::TimeNs => 39,
        L::Geometry => 40,
        L::Variant => 41,
        _ => 0,
    }
}

/// `struct duckdb_column` (deprecated fields kept for layout compatibility).
#[repr(C)]
#[derive(Debug)]
pub struct DuckdbColumn {
    pub deprecated_data: *mut c_void,
    pub deprecated_nullmask: *mut bool,
    pub deprecated_type: DuckdbType,
    pub deprecated_name: *mut std::ffi::c_char,
    pub internal_data: *mut c_void,
}

/// `struct duckdb_result`.
#[repr(C)]
#[derive(Debug)]
pub struct DuckdbResult {
    pub deprecated_column_count: Idx,
    pub deprecated_row_count: Idx,
    pub deprecated_rows_changed: Idx,
    pub deprecated_columns: *mut DuckdbColumn,
    pub deprecated_error_message: *mut std::ffi::c_char,
    pub internal_data: *mut c_void,
}

/// `struct _duckdb_database` / `_duckdb_connection`: an opaque pointer wrapper.
#[repr(C)]
#[derive(Debug)]
pub struct Handle {
    pub internal_ptr: *mut c_void,
}

/// `duckdb_database`
pub type DuckdbDatabase = *mut Handle;
/// `duckdb_connection`
pub type DuckdbConnection = *mut Handle;
