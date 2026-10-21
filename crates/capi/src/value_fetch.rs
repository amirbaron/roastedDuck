//! The deprecated per-value accessors of a result (DuckDB's `capi/value-c.cpp`, with the
//! casts of `capi/cast`).
//!
//! They read the C arrays the result was materialized into (`UnsafeFetch`) and cast the
//! value to the requested type (`GetInternalCValue`): a NULL, a failed cast, a position
//! out of range or a column type without a C representation give the type's default.

use std::ffi::{CStr, c_char};

use rustdb_function::cast::ValueCast;
use rustdb_types::{
    Date, Interval, LogicalType, LogicalTypeId, PhysicalType, Time, TimeNs, Timestamp, Value,
};

use crate::helper::malloc_bytes;
use crate::result::{deprecated_column, deprecated_materialize, result_data};
use crate::types::*;

/// `CanUseDeprecatedFetch`: the result materializes into C arrays and has `(col, row)`.
///
/// # Safety
/// `result` must be NULL or a result filled in by this library.
unsafe fn can_use_deprecated_fetch(result: *mut DuckdbResult, col: Idx, row: Idx) -> bool {
    // SAFETY: guaranteed by the caller.
    unsafe {
        if result.is_null() || !deprecated_materialize(result) {
            return false;
        }
        col < (*result).deprecated_column_count
            && row < (*result).deprecated_row_count
            && !deprecated_column(result, col).deprecated_nullmask.is_null()
    }
}

/// `CanFetchValue`: [`can_use_deprecated_fetch`] and the value is not NULL.
///
/// # Safety
/// `result` must be NULL or a result filled in by this library.
unsafe fn can_fetch_value(result: *mut DuckdbResult, col: Idx, row: Idx) -> bool {
    // SAFETY: guaranteed by the caller; the null mask has `deprecated_row_count` entries.
    unsafe {
        can_use_deprecated_fetch(result, col, row)
            && !*deprecated_column(result, col)
                .deprecated_nullmask
                .add(row as usize)
    }
}

/// The C value at `row` of a deprecated column as stored (`UnsafeFetch<T>`).
///
/// # Safety
/// The column's data must hold `T` values and `row` must be in range.
unsafe fn unsafe_fetch<T: Copy>(column: &DuckdbColumn, row: Idx) -> T {
    // SAFETY: guaranteed by the caller.
    unsafe {
        column
            .deprecated_data
            .cast::<T>()
            .add(row as usize)
            .read_unaligned()
    }
}

/// The value at `(col, row)` read from the C arrays as the engine value the cast starts
/// from, or `None` for the column types `GetInternalCValue` does not handle.
///
/// # Safety
/// `result` must have passed [`can_fetch_value`] for `(col, row)`.
unsafe fn source_value(result: *mut DuckdbResult, col: Idx, row: Idx) -> Option<Value> {
    // SAFETY: guaranteed by the caller.
    let column = unsafe { deprecated_column(result, col) };
    // SAFETY: the data holds values of the column's C type.
    let value = unsafe {
        match column.deprecated_type {
            DUCKDB_TYPE_BOOLEAN => Value::boolean(unsafe_fetch(column, row)),
            DUCKDB_TYPE_TINYINT => Value::tinyint(unsafe_fetch(column, row)),
            DUCKDB_TYPE_SMALLINT => Value::smallint(unsafe_fetch(column, row)),
            DUCKDB_TYPE_INTEGER => Value::integer(unsafe_fetch(column, row)),
            DUCKDB_TYPE_BIGINT => Value::bigint(unsafe_fetch(column, row)),
            DUCKDB_TYPE_UTINYINT => Value::utinyint(unsafe_fetch(column, row)),
            DUCKDB_TYPE_USMALLINT => Value::usmallint(unsafe_fetch(column, row)),
            DUCKDB_TYPE_UINTEGER => Value::uinteger(unsafe_fetch(column, row)),
            DUCKDB_TYPE_UBIGINT => Value::ubigint(unsafe_fetch(column, row)),
            DUCKDB_TYPE_FLOAT => Value::float(unsafe_fetch(column, row)),
            DUCKDB_TYPE_DOUBLE => Value::double(unsafe_fetch(column, row)),
            DUCKDB_TYPE_DATE => Value::date(Date(unsafe_fetch(column, row))),
            DUCKDB_TYPE_TIME => Value::time(Time(unsafe_fetch(column, row))),
            DUCKDB_TYPE_TIME_NS => Value::time_ns(TimeNs(unsafe_fetch(column, row))),
            DUCKDB_TYPE_TIMESTAMP => Value::timestamp(Timestamp(unsafe_fetch(column, row))),
            DUCKDB_TYPE_HUGEINT => Value::hugeint(unsafe_fetch::<DuckdbHugeint>(column, row).to_i128()),
            DUCKDB_TYPE_UHUGEINT => Value::uhugeint(unsafe_fetch::<DuckdbUhugeint>(column, row).to_u128()),
            DUCKDB_TYPE_DECIMAL => decimal_source(result, col, unsafe_fetch(column, row))?,
            DUCKDB_TYPE_INTERVAL => {
                let interval: DuckdbInterval = unsafe_fetch(column, row);
                Value::interval(Interval {
                    months: interval.months,
                    days: interval.days,
                    micros: interval.micros,
                })
            }
            // `FromCStringCastWrapper`: the C string, up to its NUL byte.
            DUCKDB_TYPE_VARCHAR => {
                let text: *const c_char = unsafe_fetch(column, row);
                Value::varchar(CStr::from_ptr(text).to_string_lossy())
            }
            DUCKDB_TYPE_BLOB => {
                let blob: DuckdbBlob = unsafe_fetch(column, row);
                Value::blob(crate::helper::bytes(blob.data, blob.size))
            }
            _ => return None,
        }
    };
    Some(value)
}

/// The DECIMAL at a stored hugeint: the low 64 bits for decimals stored in up to 64 bits
/// (`CDecimalConverter` keeps the value there), all 128 otherwise.
///
/// # Safety
/// `result` must be a result filled in by this library with column `col`.
unsafe fn decimal_source(result: *mut DuckdbResult, col: Idx, stored: DuckdbHugeint) -> Option<Value> {
    // SAFETY: guaranteed by the caller.
    let ty = unsafe { result_data(result) }?.types.get(col as usize)?.clone();
    let (width, scale) = ty.decimal_properties()?;
    let unscaled = match ty.physical_type() {
        PhysicalType::Int16 => i128::from(stored.lower as i16),
        PhysicalType::Int32 => i128::from(stored.lower as i32),
        PhysicalType::Int64 => i128::from(stored.lower as i64),
        _ => stored.to_i128(),
    };
    Some(Value::decimal(unscaled, width, scale))
}

/// `GetInternalCValue`: the value at `(col, row)` cast to `target` (`TryCast`); `None` for
/// NULLs, positions out of range, types without a C representation and failed casts.
/// BLOB values only cast to VARCHAR (`FromCBlobCastWrapper`).
///
/// # Safety
/// `result` must be NULL or a result filled in by this library.
unsafe fn internal_value(
    result: *mut DuckdbResult,
    col: Idx,
    row: Idx,
    target: &LogicalType,
) -> Option<Value> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        if !can_fetch_value(result, col, row) {
            return None;
        }
        let source = source_value(result, col, row)?;
        if source.logical_type().id() == LogicalTypeId::Blob && target.id() != LogicalTypeId::Varchar {
            return None;
        }
        let cast = source.try_cast(target).ok()?;
        (!cast.is_null()).then_some(cast)
    }
}

/// The accessors that cast to a fixed-width type and return its default otherwise.
macro_rules! value_accessors {
    ($($(#[$doc:meta])* $name:ident -> $c:ty, $target:expr, $read:expr;)*) => {$(
        $(#[$doc])*
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(result: *mut DuckdbResult, col: Idx, row: Idx) -> $c {
            let read: fn(&Value) -> Option<$c> = $read;
            // SAFETY: `result` is NULL or a result filled in by this library.
            unsafe { internal_value(result, col, row, &$target) }
                .as_ref()
                .and_then(read)
                .unwrap_or_default()
        }
    )*};
}

value_accessors! {
    /// `bool duckdb_value_boolean(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
    duckdb_value_boolean -> bool, LogicalType::BOOLEAN, Value::as_bool;
    /// `int8_t duckdb_value_int8(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
    duckdb_value_int8 -> i8, LogicalType::TINYINT, Value::as_i8;
    /// `int16_t duckdb_value_int16(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
    duckdb_value_int16 -> i16, LogicalType::SMALLINT, Value::as_i16;
    /// `int32_t duckdb_value_int32(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
    duckdb_value_int32 -> i32, LogicalType::INTEGER, Value::as_i32;
    /// `int64_t duckdb_value_int64(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
    duckdb_value_int64 -> i64, LogicalType::BIGINT, Value::as_i64;
    /// `uint8_t duckdb_value_uint8(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
    duckdb_value_uint8 -> u8, LogicalType::UTINYINT, Value::as_u8;
    /// `uint16_t duckdb_value_uint16(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
    duckdb_value_uint16 -> u16, LogicalType::USMALLINT, Value::as_u16;
    /// `uint32_t duckdb_value_uint32(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
    duckdb_value_uint32 -> u32, LogicalType::UINTEGER, Value::as_u32;
    /// `uint64_t duckdb_value_uint64(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
    duckdb_value_uint64 -> u64, LogicalType::UBIGINT, Value::as_u64;
    /// `float duckdb_value_float(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
    duckdb_value_float -> f32, LogicalType::FLOAT, Value::as_f32;
    /// `double duckdb_value_double(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
    duckdb_value_double -> f64, LogicalType::DOUBLE, Value::as_f64;
    /// `duckdb_hugeint duckdb_value_hugeint(duckdb_result *result, idx_t col, idx_t row)`
    /// (capi/value-c.cpp)
    duckdb_value_hugeint -> DuckdbHugeint, LogicalType::HUGEINT,
        |value| value.as_i128().map(DuckdbHugeint::from_i128);
    /// `duckdb_uhugeint duckdb_value_uhugeint(duckdb_result *result, idx_t col, idx_t row)`
    /// (capi/value-c.cpp)
    duckdb_value_uhugeint -> DuckdbUhugeint, LogicalType::UHUGEINT,
        |value| value.as_u128().map(DuckdbUhugeint::from_u128);
    /// `duckdb_date duckdb_value_date(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
    duckdb_value_date -> DuckdbDate, LogicalType::DATE,
        |value| value.as_date().map(|date| DuckdbDate { days: date.0 });
    /// `duckdb_time duckdb_value_time(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
    duckdb_value_time -> DuckdbTime, LogicalType::TIME,
        |value| value.as_time().map(|time| DuckdbTime { micros: time.0 });
    /// `duckdb_timestamp duckdb_value_timestamp(duckdb_result *result, idx_t col, idx_t row)`
    /// (capi/value-c.cpp)
    duckdb_value_timestamp -> DuckdbTimestamp, LogicalType::TIMESTAMP,
        |value| value.as_timestamp().map(|timestamp| DuckdbTimestamp { micros: timestamp.0 });
    /// `duckdb_interval duckdb_value_interval(duckdb_result *result, idx_t col, idx_t row)`
    /// (capi/value-c.cpp)
    duckdb_value_interval -> DuckdbInterval, LogicalType::INTERVAL,
        |value| value.as_interval().map(|interval| DuckdbInterval {
            months: interval.months,
            days: interval.days,
            micros: interval.micros,
        });
}

/// `duckdb_decimal duckdb_value_decimal(duckdb_result *result, idx_t col, idx_t row)`
/// (capi/value-c.cpp): only for DECIMAL columns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_value_decimal(
    result: *mut DuckdbResult,
    col: Idx,
    row: Idx,
) -> DuckdbDecimal {
    // SAFETY: `result` is NULL or a result filled in by this library.
    unsafe {
        if !can_fetch_value(result, col, row) {
            return DuckdbDecimal::default();
        }
        let is_decimal = result_data(result)
            .and_then(|data| data.types.get(col as usize))
            .is_some_and(|ty| ty.id() == LogicalTypeId::Decimal);
        if !is_decimal {
            return DuckdbDecimal::default();
        }
        source_value(result, col, row).map_or_else(DuckdbDecimal::default, |value| {
            crate::hugeint::decimal_to_c(&value)
        })
    }
}

/// `duckdb_string duckdb_value_string(duckdb_result *result, idx_t col, idx_t row)`
/// (capi/value-c.cpp): the value cast to VARCHAR, allocated with `duckdb_malloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_value_string(result: *mut DuckdbResult, col: Idx, row: Idx) -> DuckdbString {
    // SAFETY: `result` is NULL or a result filled in by this library.
    let text = unsafe { internal_value(result, col, row, &LogicalType::VARCHAR) };
    match text.as_ref().and_then(Value::as_bytes) {
        Some(bytes) => DuckdbString {
            data: malloc_bytes(bytes, true).cast(),
            size: bytes.len() as Idx,
        },
        None => DuckdbString {
            data: std::ptr::null_mut(),
            size: 0,
        },
    }
}

/// `char *duckdb_value_varchar(duckdb_result *result, idx_t col, idx_t row)`
/// (capi/value-c.cpp): the value cast to VARCHAR, allocated with `duckdb_malloc`; NULL for
/// NULL values and for column types without a C representation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_value_varchar(result: *mut DuckdbResult, col: Idx, row: Idx) -> *mut c_char {
    // SAFETY: forwarded API contract.
    unsafe { duckdb_value_string(result, col, row) }.data
}

/// `duckdb_string duckdb_value_string_internal(duckdb_result *result, idx_t col, idx_t row)`
/// (capi/value-c.cpp): the string of a VARCHAR column as stored in the result, not to be
/// freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_value_string_internal(
    result: *mut DuckdbResult,
    col: Idx,
    row: Idx,
) -> DuckdbString {
    let none = DuckdbString {
        data: std::ptr::null_mut(),
        size: 0,
    };
    // SAFETY: `result` is NULL or a result filled in by this library.
    unsafe {
        if !can_fetch_value(result, col, row)
            || deprecated_column(result, col).deprecated_type != DUCKDB_TYPE_VARCHAR
        {
            return none;
        }
        let data: *mut c_char = unsafe_fetch(deprecated_column(result, col), row);
        DuckdbString {
            data,
            size: libc::strlen(data) as Idx,
        }
    }
}

/// `char *duckdb_value_varchar_internal(duckdb_result *result, idx_t col, idx_t row)`
/// (capi/value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_value_varchar_internal(
    result: *mut DuckdbResult,
    col: Idx,
    row: Idx,
) -> *mut c_char {
    // SAFETY: forwarded API contract.
    unsafe { duckdb_value_string_internal(result, col, row) }.data
}

/// `duckdb_blob duckdb_value_blob(duckdb_result *result, idx_t col, idx_t row)`
/// (capi/value-c.cpp): a copy of a BLOB value, allocated with `duckdb_malloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_value_blob(result: *mut DuckdbResult, col: Idx, row: Idx) -> DuckdbBlob {
    // SAFETY: `result` is NULL or a result filled in by this library.
    unsafe {
        if can_fetch_value(result, col, row)
            && deprecated_column(result, col).deprecated_type == DUCKDB_TYPE_BLOB
        {
            let blob: DuckdbBlob = unsafe_fetch(deprecated_column(result, col), row);
            let bytes = crate::helper::bytes(blob.data, blob.size);
            return DuckdbBlob {
                data: malloc_bytes(bytes, false).cast(),
                size: blob.size,
            };
        }
    }
    DuckdbBlob {
        data: std::ptr::null_mut(),
        size: 0,
    }
}

/// `bool duckdb_value_is_null(duckdb_result *result, idx_t col, idx_t row)` (capi/value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_value_is_null(result: *mut DuckdbResult, col: Idx, row: Idx) -> bool {
    // SAFETY: `result` is NULL or a result filled in by this library; the null mask has
    // `deprecated_row_count` entries.
    unsafe {
        can_use_deprecated_fetch(result, col, row)
            && *deprecated_column(result, col)
                .deprecated_nullmask
                .add(row as usize)
    }
}
