//! The value types and enums of `duckdb.h`, laid out exactly like their C declarations.
//!
//! C enums are passed as `int`-sized integers. Enums the caller passes in are integer
//! aliases with constants, so out-of-range values from C stay well-defined; handles are
//! declared next to the objects they point to.

use std::ffi::{c_char, c_void};

/// `idx_t`
pub type Idx = u64;

/// `sel_t`
pub type SelT = u32;

/// `duckdb_state`
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuckdbState {
    DuckDBSuccess = 0,
    DuckDBError = 1,
}

impl DuckdbState {
    /// `DuckDBSuccess` if `ok`, otherwise `DuckDBError`.
    pub(crate) const fn from_ok(ok: bool) -> Self {
        if ok {
            Self::DuckDBSuccess
        } else {
            Self::DuckDBError
        }
    }
}

/// `duckdb_type` (`enum DUCKDB_TYPE`).
pub type DuckdbType = u32;

pub const DUCKDB_TYPE_INVALID: DuckdbType = 0;
pub const DUCKDB_TYPE_BOOLEAN: DuckdbType = 1;
pub const DUCKDB_TYPE_TINYINT: DuckdbType = 2;
pub const DUCKDB_TYPE_SMALLINT: DuckdbType = 3;
pub const DUCKDB_TYPE_INTEGER: DuckdbType = 4;
pub const DUCKDB_TYPE_BIGINT: DuckdbType = 5;
pub const DUCKDB_TYPE_UTINYINT: DuckdbType = 6;
pub const DUCKDB_TYPE_USMALLINT: DuckdbType = 7;
pub const DUCKDB_TYPE_UINTEGER: DuckdbType = 8;
pub const DUCKDB_TYPE_UBIGINT: DuckdbType = 9;
pub const DUCKDB_TYPE_FLOAT: DuckdbType = 10;
pub const DUCKDB_TYPE_DOUBLE: DuckdbType = 11;
pub const DUCKDB_TYPE_TIMESTAMP: DuckdbType = 12;
pub const DUCKDB_TYPE_DATE: DuckdbType = 13;
pub const DUCKDB_TYPE_TIME: DuckdbType = 14;
pub const DUCKDB_TYPE_INTERVAL: DuckdbType = 15;
pub const DUCKDB_TYPE_HUGEINT: DuckdbType = 16;
pub const DUCKDB_TYPE_VARCHAR: DuckdbType = 17;
pub const DUCKDB_TYPE_BLOB: DuckdbType = 18;
pub const DUCKDB_TYPE_DECIMAL: DuckdbType = 19;
pub const DUCKDB_TYPE_TIMESTAMP_S: DuckdbType = 20;
pub const DUCKDB_TYPE_TIMESTAMP_MS: DuckdbType = 21;
pub const DUCKDB_TYPE_TIMESTAMP_NS: DuckdbType = 22;
pub const DUCKDB_TYPE_ENUM: DuckdbType = 23;
pub const DUCKDB_TYPE_LIST: DuckdbType = 24;
pub const DUCKDB_TYPE_STRUCT: DuckdbType = 25;
pub const DUCKDB_TYPE_MAP: DuckdbType = 26;
pub const DUCKDB_TYPE_UUID: DuckdbType = 27;
pub const DUCKDB_TYPE_UNION: DuckdbType = 28;
pub const DUCKDB_TYPE_BIT: DuckdbType = 29;
pub const DUCKDB_TYPE_TIME_TZ: DuckdbType = 30;
pub const DUCKDB_TYPE_TIMESTAMP_TZ: DuckdbType = 31;
pub const DUCKDB_TYPE_UHUGEINT: DuckdbType = 32;
pub const DUCKDB_TYPE_ARRAY: DuckdbType = 33;
pub const DUCKDB_TYPE_ANY: DuckdbType = 34;
pub const DUCKDB_TYPE_BIGNUM: DuckdbType = 35;
pub const DUCKDB_TYPE_SQLNULL: DuckdbType = 36;
pub const DUCKDB_TYPE_STRING_LITERAL: DuckdbType = 37;
pub const DUCKDB_TYPE_INTEGER_LITERAL: DuckdbType = 38;
pub const DUCKDB_TYPE_TIME_NS: DuckdbType = 39;
pub const DUCKDB_TYPE_GEOMETRY: DuckdbType = 40;
pub const DUCKDB_TYPE_VARIANT: DuckdbType = 41;

/// `duckdb_pending_state`
pub type DuckdbPendingState = u32;

pub const DUCKDB_PENDING_RESULT_READY: DuckdbPendingState = 0;
pub const DUCKDB_PENDING_RESULT_NOT_READY: DuckdbPendingState = 1;
pub const DUCKDB_PENDING_ERROR: DuckdbPendingState = 2;
pub const DUCKDB_PENDING_NO_TASKS_AVAILABLE: DuckdbPendingState = 3;

/// `duckdb_result_type`
pub type DuckdbResultType = u32;

pub const DUCKDB_RESULT_TYPE_INVALID: DuckdbResultType = 0;
pub const DUCKDB_RESULT_TYPE_CHANGED_ROWS: DuckdbResultType = 1;
pub const DUCKDB_RESULT_TYPE_NOTHING: DuckdbResultType = 2;
pub const DUCKDB_RESULT_TYPE_QUERY_RESULT: DuckdbResultType = 3;

/// `duckdb_statement_type`: the values of DuckDB's `StatementType` (`StatementTypeToC`
/// maps them one to one, `INVALID` staying 0).
pub type DuckdbStatementType = u32;

pub const DUCKDB_STATEMENT_TYPE_INVALID: DuckdbStatementType = 0;

/// `duckdb_error_type`: the values of DuckDB's `ExceptionType` (`ErrorTypeToC` maps them
/// one to one).
pub type DuckdbErrorType = u32;

pub const DUCKDB_ERROR_INVALID: DuckdbErrorType = 0;
pub const DUCKDB_ERROR_INVALID_TYPE: DuckdbErrorType = 8;
pub const DUCKDB_ERROR_INVALID_INPUT: DuckdbErrorType = 32;

/// `duckdb_date`: days since 1970-01-01.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbDate {
    pub days: i32,
}

/// `duckdb_date_struct`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbDateStruct {
    pub year: i32,
    pub month: i8,
    pub day: i8,
}

/// `duckdb_time`: microseconds since 00:00:00.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbTime {
    pub micros: i64,
}

/// `duckdb_time_struct`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbTimeStruct {
    pub hour: i8,
    pub min: i8,
    pub sec: i8,
    pub micros: i32,
}

/// `duckdb_time_ns`: nanoseconds since 00:00:00.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbTimeNs {
    pub nanos: i64,
}

/// `duckdb_time_tz`: the 40-bit time in microseconds and the 24-bit offset.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbTimeTz {
    pub bits: u64,
}

/// `duckdb_time_tz_struct`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbTimeTzStruct {
    pub time: DuckdbTimeStruct,
    pub offset: i32,
}

/// `duckdb_timestamp`: microseconds since 1970-01-01.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbTimestamp {
    pub micros: i64,
}

/// `duckdb_timestamp_struct`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbTimestampStruct {
    pub date: DuckdbDateStruct,
    pub time: DuckdbTimeStruct,
}

/// `duckdb_timestamp_s`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbTimestampS {
    pub seconds: i64,
}

/// `duckdb_timestamp_ms`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbTimestampMs {
    pub millis: i64,
}

/// `duckdb_timestamp_ns`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbTimestampNs {
    pub nanos: i64,
}

/// `duckdb_interval`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbInterval {
    pub months: i32,
    pub days: i32,
    pub micros: i64,
}

/// `duckdb_hugeint`: `upper * 2^64 + lower`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbHugeint {
    pub lower: u64,
    pub upper: i64,
}

impl DuckdbHugeint {
    pub(crate) const fn from_i128(value: i128) -> Self {
        Self {
            lower: value as u64,
            upper: (value >> 64) as i64,
        }
    }

    pub(crate) const fn to_i128(self) -> i128 {
        ((self.upper as i128) << 64) | self.lower as i128
    }
}

/// `duckdb_uhugeint`: `upper * 2^64 + lower`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbUhugeint {
    pub lower: u64,
    pub upper: u64,
}

impl DuckdbUhugeint {
    pub(crate) const fn from_u128(value: u128) -> Self {
        Self {
            lower: value as u64,
            upper: (value >> 64) as u64,
        }
    }

    pub(crate) const fn to_u128(self) -> u128 {
        ((self.upper as u128) << 64) | self.lower as u128
    }
}

/// `duckdb_decimal`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuckdbDecimal {
    pub width: u8,
    pub scale: u8,
    pub value: DuckdbHugeint,
}

/// `duckdb_query_progress_type`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DuckdbQueryProgressType {
    pub percentage: f64,
    pub rows_processed: u64,
    pub total_rows_to_process: u64,
}

/// `duckdb_string`: a string owned by the caller (free `data` with `duckdb_free`).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DuckdbString {
    pub data: *mut c_char,
    pub size: Idx,
}

/// `duckdb_blob`
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DuckdbBlob {
    pub data: *mut c_void,
    pub size: Idx,
}

/// `duckdb_bit`: the stored bytes of a BIT value (padding byte first).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DuckdbBit {
    pub data: *mut u8,
    pub size: Idx,
}

/// `duckdb_bignum`: the big-endian magnitude and the sign.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DuckdbBignum {
    pub data: *mut u8,
    pub size: Idx,
    pub is_negative: bool,
}

/// `duckdb_string_t`: DuckDB's 16-byte `string_t` (see [`roastedduck_types::StringT`]).
pub type DuckdbStringT = roastedduck_types::StringT;

/// `struct duckdb_column` (deprecated fields kept for layout compatibility).
#[repr(C)]
#[derive(Debug)]
pub struct DuckdbColumn {
    pub deprecated_data: *mut c_void,
    pub deprecated_nullmask: *mut bool,
    pub deprecated_type: DuckdbType,
    pub deprecated_name: *mut c_char,
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
    pub deprecated_error_message: *mut c_char,
    pub internal_data: *mut c_void,
}

// The layouts of `duckdb.h` on 64-bit targets (checked against clang's `sizeof`/`offsetof`).
const _: () = {
    use std::mem::{align_of, offset_of, size_of};
    assert!(size_of::<DuckdbDate>() == 4 && align_of::<DuckdbDate>() == 4);
    assert!(size_of::<DuckdbDateStruct>() == 8 && align_of::<DuckdbDateStruct>() == 4);
    assert!(size_of::<DuckdbTimeStruct>() == 8 && offset_of!(DuckdbTimeStruct, micros) == 4);
    assert!(size_of::<DuckdbTimeTzStruct>() == 12 && align_of::<DuckdbTimeTzStruct>() == 4);
    assert!(size_of::<DuckdbTimestampStruct>() == 16 && align_of::<DuckdbTimestampStruct>() == 4);
    assert!(size_of::<DuckdbInterval>() == 16 && align_of::<DuckdbInterval>() == 8);
    assert!(size_of::<DuckdbHugeint>() == 16 && align_of::<DuckdbHugeint>() == 8);
    assert!(size_of::<DuckdbUhugeint>() == 16 && align_of::<DuckdbUhugeint>() == 8);
    assert!(size_of::<DuckdbDecimal>() == 24 && offset_of!(DuckdbDecimal, value) == 8);
    assert!(size_of::<DuckdbQueryProgressType>() == 24);
    assert!(size_of::<DuckdbStringT>() == 16 && align_of::<DuckdbStringT>() == 8);
    assert!(size_of::<DuckdbString>() == 16 && size_of::<DuckdbBlob>() == 16 && size_of::<DuckdbBit>() == 16);
    assert!(size_of::<DuckdbBignum>() == 24);
    assert!(size_of::<DuckdbColumn>() == 40);
    assert!(offset_of!(DuckdbColumn, deprecated_type) == 16 && offset_of!(DuckdbColumn, internal_data) == 32);
    assert!(size_of::<DuckdbResult>() == 48);
    assert!(
        offset_of!(DuckdbResult, deprecated_columns) == 24 && offset_of!(DuckdbResult, internal_data) == 40
    );
};
