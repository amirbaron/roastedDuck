//! Values (DuckDB's `capi/duckdb_value-c.cpp`).
//!
//! A `duckdb_value` points to a heap-allocated [`Value`]. The typed getters cast the value
//! in place to the type they read (`Value::DefaultTryCastAs`), like DuckDB, and return
//! the type's NULL sentinel (`NullValue<T>`) when that fails. DuckDB raises an internal
//! error for a getter on a NULL value, which cannot cross the C boundary; the sentinel is
//! returned for it as well.

use std::ffi::c_char;

use roastedduck_function::cast::ValueCast;
use roastedduck_types::{
    Date, Interval, LogicalType, LogicalTypeId, Time, TimeNs, TimeTz, Timestamp, Uuid, Value,
};

use crate::helper::{bytes, c_string, malloc_bytes};
use crate::hugeint::decimal_to_c;
use crate::logical_types::{DuckdbLogicalType, MAX_ARRAY_SIZE, is_unresolved, type_ref};
use crate::types::*;

/// `duckdb_value`
pub type DuckdbValue = *mut Value;

/// Hands `value` to C.
pub(crate) fn wrap_value(value: Value) -> DuckdbValue {
    Box::into_raw(Box::new(value))
}

/// Borrows the value behind a handle.
///
/// # Safety
/// `value` must be NULL or a live `duckdb_value`.
pub(crate) unsafe fn value_ref<'a>(value: DuckdbValue) -> Option<&'a Value> {
    // SAFETY: guaranteed by the caller.
    unsafe { value.as_ref() }
}

/// Casts the value behind `val` to `target` in place (`Value::DefaultTryCastAs`) and
/// returns it, or `None` if the cast fails or the value is NULL.
///
/// # Safety
/// `val` must be NULL or a live `duckdb_value`.
unsafe fn cast_in_place<'a>(val: DuckdbValue, target: &LogicalType) -> Option<&'a Value> {
    // SAFETY: guaranteed by the caller.
    let value = unsafe { val.as_mut() }?;
    if value.logical_type() != target {
        *value = value.try_cast(target).ok()?;
    }
    (!value.is_null()).then_some(&*value)
}

/// The value behind `val` cast to `target` (`Value::DefaultCastAs`, on a copy), unless
/// it is NULL or the cast fails.
///
/// # Safety
/// `val` must be NULL or a live `duckdb_value`.
unsafe fn cast_copy(val: DuckdbValue, target: &LogicalType) -> Option<Value> {
    // SAFETY: guaranteed by the caller.
    let value = unsafe { value_ref(val) }?.default_cast_as(target).ok()?;
    (!value.is_null()).then_some(value)
}

/// Reads `count` values from a C array; `None` if one is NULL.
///
/// # Safety
/// `values` must point to `count` handles, each NULL or live.
unsafe fn values(values: *const DuckdbValue, count: Idx) -> Option<Vec<Value>> {
    (0..count as usize)
        .map(|i| {
            // SAFETY: the array holds `count` entries per the caller.
            unsafe { value_ref(*values.add(i)) }.cloned()
        })
        .collect()
}

/// `void duckdb_destroy_value(duckdb_value *value)` (capi/duckdb_value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_value(value: *mut DuckdbValue) {
    // SAFETY: `value` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = value.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `duckdb_value duckdb_create_varchar_length(const char *text, idx_t length)`
/// (capi/duckdb_value-c.cpp): NULL if the text is not valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_varchar_length(text: *const c_char, length: Idx) -> DuckdbValue {
    // SAFETY: `text` points to `length` bytes per the API contract.
    let text = unsafe { bytes(text.cast(), length) };
    Value::try_varchar(text).map_or(std::ptr::null_mut(), wrap_value)
}

/// `duckdb_value duckdb_create_varchar(const char *text)` (capi/duckdb_value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_varchar(text: *const c_char) -> DuckdbValue {
    if text.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `text` is a NUL-terminated string per the API contract.
    let length = unsafe { libc::strlen(text) };
    // SAFETY: `text` has `length` bytes.
    unsafe { duckdb_create_varchar_length(text, length as Idx) }
}

/// The `duckdb_create_*` / `duckdb_get_*` pairs of the fixed-width types: the getter casts
/// to the type and returns `NullValue<T>` when it cannot.
macro_rules! scalar_values {
    ($($(#[$doc:meta])* $create:ident, $get:ident: $c:ty, $ty:expr, $make:expr, $read:expr, $null:expr;)*) => {$(
        $(#[$doc])*
        #[unsafe(no_mangle)]
        pub extern "C" fn $create(input: $c) -> DuckdbValue {
            let make: fn($c) -> Value = $make;
            wrap_value(make(input))
        }

        $(#[$doc])*
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $get(val: DuckdbValue) -> $c {
            let read: fn(&Value) -> Option<$c> = $read;
            // SAFETY: `val` is NULL or a live handle.
            unsafe { cast_in_place(val, &$ty) }.and_then(read).unwrap_or($null)
        }
    )*};
}

scalar_values! {
    /// `duckdb_create_bool` / `duckdb_get_bool` (capi/duckdb_value-c.cpp)
    duckdb_create_bool, duckdb_get_bool: bool, LogicalType::BOOLEAN, Value::boolean, Value::as_bool, false;
    /// `duckdb_create_int8` / `duckdb_get_int8` (capi/duckdb_value-c.cpp)
    duckdb_create_int8, duckdb_get_int8: i8, LogicalType::TINYINT, Value::tinyint, Value::as_i8, i8::MIN;
    /// `duckdb_create_uint8` / `duckdb_get_uint8` (capi/duckdb_value-c.cpp)
    duckdb_create_uint8, duckdb_get_uint8: u8, LogicalType::UTINYINT, Value::utinyint, Value::as_u8, 0;
    /// `duckdb_create_int16` / `duckdb_get_int16` (capi/duckdb_value-c.cpp)
    duckdb_create_int16, duckdb_get_int16: i16, LogicalType::SMALLINT, Value::smallint, Value::as_i16, i16::MIN;
    /// `duckdb_create_uint16` / `duckdb_get_uint16` (capi/duckdb_value-c.cpp)
    duckdb_create_uint16, duckdb_get_uint16: u16, LogicalType::USMALLINT, Value::usmallint, Value::as_u16, 0;
    /// `duckdb_create_int32` / `duckdb_get_int32` (capi/duckdb_value-c.cpp)
    duckdb_create_int32, duckdb_get_int32: i32, LogicalType::INTEGER, Value::integer, Value::as_i32, i32::MIN;
    /// `duckdb_create_uint32` / `duckdb_get_uint32` (capi/duckdb_value-c.cpp)
    duckdb_create_uint32, duckdb_get_uint32: u32, LogicalType::UINTEGER, Value::uinteger, Value::as_u32, 0;
    /// `duckdb_create_int64` / `duckdb_get_int64` (capi/duckdb_value-c.cpp)
    duckdb_create_int64, duckdb_get_int64: i64, LogicalType::BIGINT, Value::bigint, Value::as_i64, i64::MIN;
    /// `duckdb_create_uint64` / `duckdb_get_uint64` (capi/duckdb_value-c.cpp)
    duckdb_create_uint64, duckdb_get_uint64: u64, LogicalType::UBIGINT, Value::ubigint, Value::as_u64, 0;
    /// `duckdb_create_float` / `duckdb_get_float` (capi/duckdb_value-c.cpp)
    duckdb_create_float, duckdb_get_float: f32, LogicalType::FLOAT, Value::float, Value::as_f32, f32::NAN;
    /// `duckdb_create_double` / `duckdb_get_double` (capi/duckdb_value-c.cpp)
    duckdb_create_double, duckdb_get_double: f64, LogicalType::DOUBLE, Value::double, Value::as_f64, f64::NAN;
    /// `duckdb_create_hugeint` / `duckdb_get_hugeint` (capi/duckdb_value-c.cpp)
    duckdb_create_hugeint, duckdb_get_hugeint: DuckdbHugeint, LogicalType::HUGEINT,
        |input| Value::hugeint(input.to_i128()),
        |value| value.as_i128().map(DuckdbHugeint::from_i128),
        DuckdbHugeint::from_i128(i128::MIN);
    /// `duckdb_create_uhugeint` / `duckdb_get_uhugeint` (capi/duckdb_value-c.cpp)
    duckdb_create_uhugeint, duckdb_get_uhugeint: DuckdbUhugeint, LogicalType::UHUGEINT,
        |input| Value::uhugeint(input.to_u128()),
        |value| value.as_u128().map(DuckdbUhugeint::from_u128),
        DuckdbUhugeint::default();
    /// `duckdb_create_date` / `duckdb_get_date` (capi/duckdb_value-c.cpp)
    duckdb_create_date, duckdb_get_date: DuckdbDate, LogicalType::DATE,
        |input| Value::date(Date(input.days)),
        |value| value.as_i32().map(|days| DuckdbDate { days }),
        DuckdbDate::default();
    /// `duckdb_create_time` / `duckdb_get_time` (capi/duckdb_value-c.cpp)
    duckdb_create_time, duckdb_get_time: DuckdbTime, LogicalType::TIME,
        |input| Value::time(Time(input.micros)),
        |value| value.as_i64().map(|micros| DuckdbTime { micros }),
        DuckdbTime::default();
    /// `duckdb_create_time_ns` / `duckdb_get_time_ns` (capi/duckdb_value-c.cpp)
    duckdb_create_time_ns, duckdb_get_time_ns: DuckdbTimeNs, LogicalType::TIME_NS,
        |input| Value::time_ns(TimeNs(input.nanos)),
        |value| value.as_i64().map(|nanos| DuckdbTimeNs { nanos }),
        DuckdbTimeNs::default();
    /// `duckdb_create_time_tz_value` / `duckdb_get_time_tz` (capi/duckdb_value-c.cpp)
    duckdb_create_time_tz_value, duckdb_get_time_tz: DuckdbTimeTz, LogicalType::TIME_TZ,
        |input| Value::time_tz(TimeTz(input.bits)),
        |value| value.as_i64().map(|bits| DuckdbTimeTz { bits: bits as u64 }),
        DuckdbTimeTz::default();
    /// `duckdb_create_timestamp` / `duckdb_get_timestamp` (capi/duckdb_value-c.cpp)
    duckdb_create_timestamp, duckdb_get_timestamp: DuckdbTimestamp, LogicalType::TIMESTAMP,
        |input| Value::timestamp(Timestamp(input.micros)),
        |value| value.as_i64().map(|micros| DuckdbTimestamp { micros }),
        DuckdbTimestamp::default();
    /// `duckdb_create_timestamp_tz` / `duckdb_get_timestamp_tz` (capi/duckdb_value-c.cpp)
    duckdb_create_timestamp_tz, duckdb_get_timestamp_tz: DuckdbTimestamp, LogicalType::TIMESTAMP_TZ,
        |input| Value::timestamp_tz(Timestamp(input.micros)),
        |value| value.as_i64().map(|micros| DuckdbTimestamp { micros }),
        DuckdbTimestamp::default();
    /// `duckdb_create_timestamp_s` / `duckdb_get_timestamp_s` (capi/duckdb_value-c.cpp)
    duckdb_create_timestamp_s, duckdb_get_timestamp_s: DuckdbTimestampS, LogicalType::TIMESTAMP_S,
        |input| Value::timestamp_s(input.seconds),
        |value| value.as_i64().map(|seconds| DuckdbTimestampS { seconds }),
        DuckdbTimestampS::default();
    /// `duckdb_create_timestamp_ms` / `duckdb_get_timestamp_ms` (capi/duckdb_value-c.cpp)
    duckdb_create_timestamp_ms, duckdb_get_timestamp_ms: DuckdbTimestampMs, LogicalType::TIMESTAMP_MS,
        |input| Value::timestamp_ms(input.millis),
        |value| value.as_i64().map(|millis| DuckdbTimestampMs { millis }),
        DuckdbTimestampMs::default();
    /// `duckdb_create_timestamp_ns` / `duckdb_get_timestamp_ns` (capi/duckdb_value-c.cpp)
    duckdb_create_timestamp_ns, duckdb_get_timestamp_ns: DuckdbTimestampNs, LogicalType::TIMESTAMP_NS,
        |input| Value::timestamp_ns(input.nanos),
        |value| value.as_i64().map(|nanos| DuckdbTimestampNs { nanos }),
        DuckdbTimestampNs::default();
    /// `duckdb_create_interval` / `duckdb_get_interval` (capi/duckdb_value-c.cpp)
    duckdb_create_interval, duckdb_get_interval: DuckdbInterval, LogicalType::INTERVAL,
        |input| Value::interval(Interval { months: input.months, days: input.days, micros: input.micros }),
        |value| value.as_interval().map(|interval| DuckdbInterval {
            months: interval.months,
            days: interval.days,
            micros: interval.micros,
        }),
        DuckdbInterval { months: i32::MIN, days: i32::MIN, micros: i64::MIN };
    /// `duckdb_create_uuid` / `duckdb_get_uuid` (capi/duckdb_value-c.cpp): the UUID's bits
    /// in text order (`UUID::FromUHugeint` / `UUID::ToUHugeint`).
    duckdb_create_uuid, duckdb_get_uuid: DuckdbUhugeint, LogicalType::UUID,
        |input| Value::uuid(Uuid::from_u128(input.to_u128())),
        |value| value.as_uuid().map(|uuid| DuckdbUhugeint::from_u128(uuid.to_u128())),
        DuckdbUhugeint::from_u128(Uuid(i128::MIN).to_u128());
}

/// `duckdb_value duckdb_create_bignum(duckdb_bignum input)` (capi/duckdb_value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_bignum(input: DuckdbBignum) -> DuckdbValue {
    // SAFETY: `input.data` points to `input.size` bytes per the API contract.
    let magnitude = unsafe { bytes(input.data.cast(), input.size) };
    let stored = roastedduck_function::bignum::from_magnitude(magnitude, input.is_negative);
    wrap_value(Value::bytes(LogicalType::BIGNUM, &stored))
}

/// `duckdb_bignum duckdb_get_bignum(duckdb_value val)` (capi/duckdb_value-c.cpp): the
/// magnitude is allocated with `duckdb_malloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_bignum(val: DuckdbValue) -> DuckdbBignum {
    // SAFETY: `val` is NULL or a live handle.
    let stored = unsafe { cast_copy(val, &LogicalType::BIGNUM) };
    let (magnitude, is_negative) = stored
        .and_then(|value| roastedduck_function::bignum::magnitude(value.as_bytes()?).ok())
        .unwrap_or_default();
    DuckdbBignum {
        data: malloc_bytes(&magnitude, false),
        size: magnitude.len() as Idx,
        is_negative,
    }
}

/// `duckdb_value duckdb_create_decimal(duckdb_decimal input)` (capi/duckdb_value-c.cpp):
/// NULL for an invalid width or scale.
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_decimal(input: DuckdbDecimal) -> DuckdbValue {
    if !(1..=LogicalType::MAX_DECIMAL_WIDTH).contains(&input.width) || input.scale > input.width {
        return std::ptr::null_mut();
    }
    wrap_value(Value::decimal(input.value.to_i128(), input.width, input.scale))
}

/// `duckdb_decimal duckdb_get_decimal(duckdb_value val)` (capi/duckdb_value-c.cpp): the
/// zero decimal unless the value is a DECIMAL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_decimal(val: DuckdbValue) -> DuckdbDecimal {
    // SAFETY: `val` is NULL or a live handle.
    unsafe { value_ref(val) }.map_or_else(DuckdbDecimal::default, decimal_to_c)
}

/// `duckdb_value duckdb_create_blob(const uint8_t *data, idx_t length)` (capi/duckdb_value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_blob(data: *const u8, length: Idx) -> DuckdbValue {
    // SAFETY: `data` points to `length` bytes per the API contract.
    wrap_value(Value::blob(unsafe { bytes(data.cast(), length) }))
}

/// `duckdb_blob duckdb_get_blob(duckdb_value val)` (capi/duckdb_value-c.cpp): the bytes are
/// allocated with `duckdb_malloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_blob(val: DuckdbValue) -> DuckdbBlob {
    // SAFETY: `val` is NULL or a live handle.
    let blob = unsafe { cast_copy(val, &LogicalType::BLOB) };
    let data = blob.as_ref().and_then(Value::as_bytes).unwrap_or_default();
    DuckdbBlob {
        data: malloc_bytes(data, false).cast(),
        size: data.len() as Idx,
    }
}

/// `duckdb_value duckdb_create_bit(duckdb_bit input)` (capi/duckdb_value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_bit(input: DuckdbBit) -> DuckdbValue {
    // SAFETY: `input.data` points to `input.size` bytes per the API contract.
    wrap_value(Value::bytes(LogicalType::BIT, unsafe {
        bytes(input.data.cast(), input.size)
    }))
}

/// `duckdb_bit duckdb_get_bit(duckdb_value val)` (capi/duckdb_value-c.cpp): the stored
/// bytes, allocated with `duckdb_malloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_bit(val: DuckdbValue) -> DuckdbBit {
    // SAFETY: `val` is NULL or a live handle.
    let bit = unsafe { cast_copy(val, &LogicalType::BIT) };
    let data = bit.as_ref().and_then(Value::as_bytes).unwrap_or_default();
    DuckdbBit {
        data: malloc_bytes(data, false),
        size: data.len() as Idx,
    }
}

/// `duckdb_logical_type duckdb_get_value_type(duckdb_value val)` (capi/duckdb_value-c.cpp):
/// the value's own type, valid while the value is; not to be destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_value_type(val: DuckdbValue) -> DuckdbLogicalType {
    // SAFETY: `val` is NULL or a live handle.
    unsafe { value_ref(val) }.map_or(std::ptr::null_mut(), |value| {
        std::ptr::from_ref(value.logical_type()).cast_mut()
    })
}

/// `char *duckdb_get_varchar(duckdb_value value)` (capi/duckdb_value-c.cpp): the value cast
/// to VARCHAR, allocated with `duckdb_malloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_varchar(value: DuckdbValue) -> *mut c_char {
    // SAFETY: `value` is NULL or a live handle.
    unsafe { cast_copy(value, &LogicalType::VARCHAR) }
        .and_then(|text| text.as_bytes().map(|bytes| malloc_bytes(bytes, true).cast()))
        .unwrap_or(std::ptr::null_mut())
}

/// `duckdb_value duckdb_create_struct_value(duckdb_logical_type type, duckdb_value *values)`
/// (capi/duckdb_value-c.cpp): the fields are cast to the field types.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_struct_value(
    ty: DuckdbLogicalType,
    values: *mut DuckdbValue,
) -> DuckdbValue {
    // SAFETY: `ty` is NULL or a live handle.
    let Some(ty) = (unsafe { type_ref(ty) }) else {
        return std::ptr::null_mut();
    };
    if values.is_null() || ty.id() != LogicalTypeId::Struct || is_unresolved(ty) {
        return std::ptr::null_mut();
    }
    let fields = ty.struct_children().unwrap_or_default();
    // SAFETY: `values` holds one handle per field per the API contract.
    let Some(children) = (unsafe { self::values(values, fields.len() as Idx) }) else {
        return std::ptr::null_mut();
    };
    let cast: Option<Vec<Value>> = children
        .iter()
        .zip(fields)
        .map(|(child, (_, field_type))| child.default_cast_as(field_type).ok())
        .collect();
    cast.map_or(std::ptr::null_mut(), |children| {
        wrap_value(Value::from_children(ty.clone(), children))
    })
}

/// `duckdb_value duckdb_create_list_value(duckdb_logical_type type, duckdb_value *values,
/// idx_t value_count)` (capi/duckdb_value-c.cpp): `type` is the element type; the elements
/// are cast to it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_list_value(
    ty: DuckdbLogicalType,
    values: *mut DuckdbValue,
    value_count: Idx,
) -> DuckdbValue {
    // SAFETY: `ty` is NULL or a live handle.
    let Some(child_type) = (unsafe { type_ref(ty) }) else {
        return std::ptr::null_mut();
    };
    if values.is_null() || is_unresolved(child_type) {
        return std::ptr::null_mut();
    }
    // SAFETY: `values` holds `value_count` handles per the API contract.
    let elements = unsafe { self::values(values, value_count) }.and_then(|elements| {
        elements
            .iter()
            .map(|element| element.default_cast_as(child_type).ok())
            .collect::<Option<Vec<_>>>()
    });
    elements.map_or(std::ptr::null_mut(), |elements| {
        wrap_value(Value::list(child_type.clone(), elements))
    })
}

/// `duckdb_value duckdb_create_array_value(duckdb_logical_type type, duckdb_value *values,
/// idx_t value_count)` (capi/duckdb_value-c.cpp): `type` is the element type; the array
/// has `value_count` elements, cast to it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_array_value(
    ty: DuckdbLogicalType,
    values: *mut DuckdbValue,
    value_count: Idx,
) -> DuckdbValue {
    // SAFETY: `ty` is NULL or a live handle.
    let Some(child_type) = (unsafe { type_ref(ty) }) else {
        return std::ptr::null_mut();
    };
    if values.is_null() || value_count >= MAX_ARRAY_SIZE || is_unresolved(child_type) {
        return std::ptr::null_mut();
    }
    // SAFETY: `values` holds `value_count` handles per the API contract.
    let elements = unsafe { self::values(values, value_count) }.and_then(|elements| {
        elements
            .iter()
            .map(|element| element.default_cast_as(child_type).ok())
            .collect::<Option<Vec<_>>>()
    });
    elements.map_or(std::ptr::null_mut(), |elements| {
        wrap_value(Value::array(child_type.clone(), elements))
    })
}

/// `duckdb_value duckdb_create_map_value(duckdb_logical_type map_type, duckdb_value *keys,
/// duckdb_value *values, idx_t entry_count)` (capi/duckdb_value-c.cpp): keys are cast to
/// the key type and must be distinct and not NULL (`MapKeyCheck`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_map_value(
    map_type: DuckdbLogicalType,
    keys: *mut DuckdbValue,
    values: *mut DuckdbValue,
    entry_count: Idx,
) -> DuckdbValue {
    // SAFETY: `map_type` is NULL or a live handle.
    let Some(map_type) = (unsafe { type_ref(map_type) }) else {
        return std::ptr::null_mut();
    };
    if keys.is_null() || values.is_null() || map_type.id() != LogicalTypeId::Map || is_unresolved(map_type) {
        return std::ptr::null_mut();
    }
    let Some((key_type, value_type)) = map_type.map_key_value() else {
        return std::ptr::null_mut();
    };
    // SAFETY: both arrays hold `entry_count` handles per the API contract.
    let (Some(keys), Some(values)) =
        (unsafe { (self::values(keys, entry_count), self::values(values, entry_count)) })
    else {
        return std::ptr::null_mut();
    };
    let mut entries = Vec::with_capacity(keys.len());
    let mut seen = std::collections::HashSet::new();
    for (key, value) in keys.into_iter().zip(values) {
        // The entry's `STRUCT(key, value)` casts both fields (`Value::STRUCT`).
        let (Ok(key), Ok(value)) = (key.default_cast_as(key_type), value.default_cast_as(value_type)) else {
            return std::ptr::null_mut();
        };
        if key.is_null() || !seen.insert(key.clone()) {
            return std::ptr::null_mut();
        }
        entries.push((key, value));
    }
    wrap_value(Value::map(key_type.clone(), value_type.clone(), entries))
}

/// `duckdb_value duckdb_create_union_value(duckdb_logical_type union_type, idx_t tag_index,
/// duckdb_value value)` (capi/duckdb_value-c.cpp): `value` must have the member's type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_union_value(
    union_type: DuckdbLogicalType,
    tag_index: Idx,
    value: DuckdbValue,
) -> DuckdbValue {
    // SAFETY: both are NULL or live handles.
    let (Some(union_type), Some(value)) = (unsafe { (type_ref(union_type), value_ref(value)) }) else {
        return std::ptr::null_mut();
    };
    if union_type.id() != LogicalTypeId::Union {
        return std::ptr::null_mut();
    }
    let members = union_type.union_members().unwrap_or_default();
    match members.get(tag_index as usize) {
        Some((_, member_type)) if member_type == value.logical_type() => wrap_value(Value::union_value(
            union_type.clone(),
            tag_index as u8,
            value.clone(),
        )),
        _ => std::ptr::null_mut(),
    }
}

/// The children of a non-NULL value of type `id`.
///
/// # Safety
/// `value` must be NULL or a live `duckdb_value`.
unsafe fn nested_children<'a>(value: DuckdbValue, id: LogicalTypeId) -> Option<&'a [Value]> {
    // SAFETY: guaranteed by the caller.
    unsafe { value_ref(value) }
        .filter(|value| value.logical_type().id() == id && !value.is_null())
        .and_then(Value::children)
}

/// `idx_t duckdb_get_map_size(duckdb_value value)` (capi/duckdb_value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_map_size(value: DuckdbValue) -> Idx {
    // SAFETY: `value` is NULL or a live handle.
    unsafe { nested_children(value, LogicalTypeId::Map) }.map_or(0, |entries| entries.len() as Idx)
}

/// The field `field` of the map entry `index` as a new value.
///
/// # Safety
/// `value` must be NULL or a live `duckdb_value`.
unsafe fn map_entry_field(value: DuckdbValue, index: Idx, field: usize) -> DuckdbValue {
    // SAFETY: forwarded.
    unsafe { nested_children(value, LogicalTypeId::Map) }
        .and_then(|entries| entries.get(index as usize))
        .and_then(Value::children)
        .and_then(|entry| entry.get(field))
        .map_or(std::ptr::null_mut(), |field| wrap_value(field.clone()))
}

/// `duckdb_value duckdb_get_map_key(duckdb_value value, idx_t index)` (capi/duckdb_value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_map_key(value: DuckdbValue, index: Idx) -> DuckdbValue {
    // SAFETY: `value` is NULL or a live handle.
    unsafe { map_entry_field(value, index, 0) }
}

/// `duckdb_value duckdb_get_map_value(duckdb_value value, idx_t index)` (capi/duckdb_value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_map_value(value: DuckdbValue, index: Idx) -> DuckdbValue {
    // SAFETY: `value` is NULL or a live handle.
    unsafe { map_entry_field(value, index, 1) }
}

/// `bool duckdb_is_null_value(duckdb_value value)` (capi/duckdb_value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_is_null_value(value: DuckdbValue) -> bool {
    // SAFETY: `value` is NULL or a live handle.
    unsafe { value_ref(value) }.is_some_and(Value::is_null)
}

/// `duckdb_value duckdb_create_null_value()` (capi/duckdb_value-c.cpp): a NULL of type
/// `"NULL"`.
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_null_value() -> DuckdbValue {
    wrap_value(Value::default())
}

/// `idx_t duckdb_get_list_size(duckdb_value value)` (capi/duckdb_value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_list_size(value: DuckdbValue) -> Idx {
    // SAFETY: `value` is NULL or a live handle.
    unsafe { nested_children(value, LogicalTypeId::List) }.map_or(0, |children| children.len() as Idx)
}

/// `duckdb_value duckdb_get_list_child(duckdb_value value, idx_t index)` (capi/duckdb_value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_list_child(value: DuckdbValue, index: Idx) -> DuckdbValue {
    // SAFETY: `value` is NULL or a live handle.
    unsafe { nested_children(value, LogicalTypeId::List) }
        .and_then(|children| children.get(index as usize))
        .map_or(std::ptr::null_mut(), |child| wrap_value(child.clone()))
}

/// `duckdb_value duckdb_create_enum_value(duckdb_logical_type type, uint64_t value)`
/// (capi/duckdb_value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_enum_value(ty: DuckdbLogicalType, value: u64) -> DuckdbValue {
    // SAFETY: `ty` is NULL or a live handle.
    let Some(ty) = (unsafe { type_ref(ty) }) else {
        return std::ptr::null_mut();
    };
    match ty.enum_values() {
        Some(values) if ty.id() == LogicalTypeId::Enum && value < values.len() as u64 => {
            wrap_value(Value::enum_value(ty.clone(), value as u32))
        }
        _ => std::ptr::null_mut(),
    }
}

/// `uint64_t duckdb_get_enum_value(duckdb_value value)` (capi/duckdb_value-c.cpp): the
/// dictionary position of an ENUM value, 0 for anything else.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_enum_value(value: DuckdbValue) -> u64 {
    // SAFETY: `value` is NULL or a live handle.
    unsafe { value_ref(value) }
        .filter(|value| value.logical_type().id() == LogicalTypeId::Enum && !value.is_null())
        .and_then(Value::as_i128)
        .map_or(0, |position| position as u64)
}

/// `duckdb_value duckdb_get_struct_child(duckdb_value value, idx_t index)`
/// (capi/duckdb_value-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_get_struct_child(value: DuckdbValue, index: Idx) -> DuckdbValue {
    // SAFETY: `value` is NULL or a live handle.
    unsafe { nested_children(value, LogicalTypeId::Struct) }
        .and_then(|children| children.get(index as usize))
        .map_or(std::ptr::null_mut(), |child| wrap_value(child.clone()))
}

/// `char *duckdb_value_to_string(duckdb_value value)` (capi/duckdb_value-c.cpp): the value
/// as SQL text (`Value::ToSQLString`), allocated with `duckdb_malloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_value_to_string(value: DuckdbValue) -> *mut c_char {
    // SAFETY: `value` is NULL or a live handle.
    unsafe { value_ref(value) }.map_or(std::ptr::null_mut(), |value| c_string(value.to_sql_string()))
}
