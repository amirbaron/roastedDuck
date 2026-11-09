//! Appenders (DuckDB's `capi/appender-c.cpp`).

use std::ffi::{CString, c_char, c_void};
use std::sync::{Arc, Weak};

use roastedduck::{Appender, Connection};
use roastedduck_common::{Error, ErrorKind};
use roastedduck_types::{DataChunk, Date, Interval, LogicalType, Time, Timestamp, Value};

use crate::data_chunk::DuckdbDataChunk;
use crate::error_data::{DuckdbErrorData, ErrorDataWrapper};
use crate::helper::{bytes, catch, opt_bytes, to_cstring};
use crate::logical_types::{DuckdbLogicalType, type_ref, wrap_type};
use crate::open::{DuckdbConnection, SharedConnection, connection_ref};
use crate::types::*;
use crate::value::{DuckdbValue, value_ref};

/// What a `duckdb_appender` points to (DuckDB's `AppenderWrapper`): the appender, unless
/// creating it failed, and the last error.
#[derive(Debug)]
pub struct AppenderWrapper {
    /// The connection appended through; an appender does not keep it open (the
    /// `weak_ptr<ClientContext>` of DuckDB's `Appender`).
    context: Weak<SharedConnection>,
    appender: Option<Appender>,
    error: Option<Error>,
    /// The raw message of `error`, kept alive for `duckdb_appender_error`.
    message: CString,
}

/// `duckdb_appender`
pub type DuckdbAppender = *mut AppenderWrapper;

impl AppenderWrapper {
    fn new(context: &Arc<SharedConnection>, appender: roastedduck::Result<Appender>) -> Self {
        let mut wrapper = Self {
            context: Arc::downgrade(context),
            appender: None,
            error: None,
            message: CString::default(),
        };
        match appender {
            Ok(appender) => wrapper.appender = Some(appender),
            Err(error) => wrapper.set_error(error),
        }
        wrapper
    }

    fn set_error(&mut self, error: Error) {
        self.message = to_cstring(error.raw_message_bytes());
        self.error = Some(error);
    }

    /// Runs `function` on the appender, keeping its error (`duckdb_appender_run_function`).
    fn run(
        &mut self,
        function: impl FnOnce(&mut Appender, &Weak<SharedConnection>) -> roastedduck::Result<()>,
    ) -> DuckdbState {
        let Some(appender) = &mut self.appender else {
            self.set_error(Error::new(ErrorKind::Invalid, "not a valid appender"));
            return DuckdbState::DuckDBError;
        };
        let context = &self.context;
        match catch(|| function(appender, context)) {
            Ok(()) => DuckdbState::DuckDBSuccess,
            Err(error) => {
                self.set_error(error);
                DuckdbState::DuckDBError
            }
        }
    }
}

/// Runs `function` with the connection appended through, which fails once the connection
/// is closed (`context.lock()` in `Appender::FlushInternal`).
fn with_connection(
    appender: &mut Appender,
    context: &Weak<SharedConnection>,
    function: impl FnOnce(&mut Appender, &mut Connection) -> roastedduck::Result<()>,
) -> roastedduck::Result<()> {
    let Some(context) = context.upgrade() else {
        let message = if appender.is_query_appender() {
            "Attempting to flush query appender data on a closed connection"
        } else {
            "Appender: Attempting to flush data to a closed connection"
        };
        return Err(Error::invalid_input(message));
    };
    function(appender, &mut context.lock())
}

/// The appender behind a handle.
///
/// # Safety
/// `appender` must be NULL or a live `duckdb_appender`.
unsafe fn appender_ref<'a>(appender: DuckdbAppender) -> Option<&'a mut AppenderWrapper> {
    // SAFETY: guaranteed by the caller.
    unsafe { appender.as_mut() }
}

/// Hands a new appender (or the error creating it) to C.
///
/// # Safety
/// `out_appender` must be writable.
unsafe fn store_appender(
    context: &Arc<SharedConnection>,
    appender: roastedduck::Result<Appender>,
    out_appender: *mut DuckdbAppender,
) -> DuckdbState {
    let state = DuckdbState::from_ok(appender.is_ok());
    let wrapper = Box::into_raw(Box::new(AppenderWrapper::new(context, appender)));
    // SAFETY: guaranteed by the caller.
    unsafe { *out_appender = wrapper };
    state
}

/// `duckdb_state duckdb_appender_create(duckdb_connection connection, const char *schema,
/// const char *table, duckdb_appender *out_appender)` (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_create(
    connection: DuckdbConnection,
    schema: *const c_char,
    table: *const c_char,
    out_appender: *mut DuckdbAppender,
) -> DuckdbState {
    // SAFETY: forwarded API contract.
    unsafe { duckdb_appender_create_ext(connection, std::ptr::null(), schema, table, out_appender) }
}

/// `duckdb_state duckdb_appender_create_ext(duckdb_connection connection, const char *catalog,
/// const char *schema, const char *table, duckdb_appender *out_appender)` (capi/appender-c.cpp):
/// a NULL catalog is the default database, a NULL schema `main`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_create_ext(
    connection: DuckdbConnection,
    catalog: *const c_char,
    schema: *const c_char,
    table: *const c_char,
    out_appender: *mut DuckdbAppender,
) -> DuckdbState {
    // SAFETY: `connection` is NULL or a live handle; the names are NULL or NUL-terminated.
    let (Some(context), Some(table)) = (unsafe { (connection_ref(connection), opt_bytes(table)) }) else {
        return DuckdbState::DuckDBError;
    };
    if out_appender.is_null() {
        return DuckdbState::DuckDBError;
    }
    // SAFETY: as above.
    let (catalog, schema) = unsafe {
        (
            opt_bytes(catalog).unwrap_or(b""),
            opt_bytes(schema).unwrap_or(b"main"),
        )
    };
    let appender = catch(|| Appender::new(&mut context.lock(), catalog, schema, table));
    // SAFETY: `out_appender` is non-NULL and writable per the API contract.
    unsafe { store_appender(context, appender, out_appender) }
}

/// `duckdb_state duckdb_appender_create_query(duckdb_connection connection, const char *query,
/// idx_t column_count, duckdb_logical_type *types, const char *table_name,
/// const char **column_names, duckdb_appender *out_appender)` (capi/appender-c.cpp): an
/// appender whose rows `query` reads as `table_name` (`appended_data` when NULL).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_create_query(
    connection: DuckdbConnection,
    query: *const c_char,
    column_count: Idx,
    types: *mut DuckdbLogicalType,
    table_name: *const c_char,
    column_names: *mut *const c_char,
    out_appender: *mut DuckdbAppender,
) -> DuckdbState {
    // SAFETY: `connection` is NULL or a live handle; `query` is NULL or NUL-terminated.
    let (Some(context), Some(query)) = (unsafe { (connection_ref(connection), opt_bytes(query)) }) else {
        return DuckdbState::DuckDBError;
    };
    if column_count == 0 || types.is_null() || out_appender.is_null() {
        return DuckdbState::DuckDBError;
    }
    let count = column_count as usize;
    let mut logical_types = Vec::with_capacity(count);
    for index in 0..count {
        // SAFETY: `types` holds `column_count` NULL or live logical types.
        let Some(ty) = (unsafe { type_ref(*types.add(index)) }) else {
            return DuckdbState::DuckDBError;
        };
        logical_types.push(ty.clone());
    }
    let mut names = Vec::new();
    if !column_names.is_null() {
        for index in 0..count {
            // SAFETY: `column_names` holds `column_count` NULL or NUL-terminated names.
            let Some(name) = (unsafe { opt_bytes(*column_names.add(index)) }) else {
                return DuckdbState::DuckDBError;
            };
            names.push(name.into());
        }
    }
    // SAFETY: `table_name` is NULL or NUL-terminated.
    let table_name = unsafe { opt_bytes(table_name) }.unwrap_or_default().into();
    let appender = Appender::for_query_bytes(query.into(), logical_types, names, table_name);
    // SAFETY: `out_appender` is non-NULL and writable per the API contract.
    unsafe { store_appender(context, Ok(appender), out_appender) }
}

/// `duckdb_state duckdb_appender_destroy(duckdb_appender *appender)` (capi/appender-c.cpp):
/// closes the appender, then frees it whether or not closing succeeded.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_destroy(appender: *mut DuckdbAppender) -> DuckdbState {
    // SAFETY: `appender` is NULL or points to NULL or a live handle.
    let Some(slot) = (unsafe { appender.as_mut() }) else {
        return DuckdbState::DuckDBError;
    };
    if slot.is_null() {
        return DuckdbState::DuckDBError;
    }
    // SAFETY: `*slot` is a live handle.
    let state = unsafe { duckdb_appender_close(*slot) };
    // SAFETY: `*slot` came from `Box::into_raw` and is not used afterwards.
    drop(unsafe { Box::from_raw(*slot) });
    *slot = std::ptr::null_mut();
    state
}

/// `duckdb_state duckdb_appender_add_column(duckdb_appender appender, const char *name)`
/// (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_add_column(
    appender: DuckdbAppender,
    name: *const c_char,
) -> DuckdbState {
    // SAFETY: `appender` is NULL or a live handle; `name` is NULL or NUL-terminated.
    let (Some(wrapper), name) = (unsafe { (appender_ref(appender), opt_bytes(name)) }) else {
        return DuckdbState::DuckDBError;
    };
    wrapper.run(|appender, context| {
        let name = name.ok_or_else(|| Error::invalid_input("the column must exist in the table"))?;
        with_connection(appender, context, |appender, connection| {
            appender.add_column(connection, name)
        })
    })
}

/// `duckdb_state duckdb_appender_clear_columns(duckdb_appender appender)` (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_clear_columns(appender: DuckdbAppender) -> DuckdbState {
    // SAFETY: `appender` is NULL or a live handle.
    let Some(wrapper) = (unsafe { appender_ref(appender) }) else {
        return DuckdbState::DuckDBError;
    };
    wrapper.run(|appender, context| with_connection(appender, context, Appender::clear_columns))
}

/// `const char *duckdb_appender_error(duckdb_appender appender)` (capi/appender-c.cpp): the
/// raw message of the last error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_error(appender: DuckdbAppender) -> *const c_char {
    // SAFETY: `appender` is NULL or a live handle.
    match unsafe { appender_ref(appender) } {
        Some(wrapper) if wrapper.error.is_some() => wrapper.message.as_ptr(),
        _ => std::ptr::null(),
    }
}

/// `duckdb_error_data duckdb_appender_error_data(duckdb_appender appender)`
/// (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_error_data(appender: DuckdbAppender) -> DuckdbErrorData {
    // SAFETY: `appender` is NULL or a live handle.
    let error = unsafe { appender_ref(appender) }.and_then(|wrapper| wrapper.error.clone());
    ErrorDataWrapper::new(error).into_handle()
}

/// `duckdb_state duckdb_appender_begin_row(duckdb_appender appender)` (capi/appender-c.cpp):
/// rows begin implicitly.
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_appender_begin_row(_appender: DuckdbAppender) -> DuckdbState {
    DuckdbState::DuckDBSuccess
}

/// `duckdb_state duckdb_appender_end_row(duckdb_appender appender)` (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_end_row(appender: DuckdbAppender) -> DuckdbState {
    // SAFETY: `appender` is NULL or a live handle.
    let Some(wrapper) = (unsafe { appender_ref(appender) }) else {
        return DuckdbState::DuckDBError;
    };
    wrapper.run(|appender, context| with_connection(appender, context, Appender::end_row))
}

/// Appends `value` to the next column (`duckdb_append_internal<Value>`).
///
/// # Safety
/// `appender` must be NULL or a live `duckdb_appender`.
unsafe fn append(appender: DuckdbAppender, value: &Value) -> DuckdbState {
    // SAFETY: guaranteed by the caller.
    let Some(wrapper) = (unsafe { appender_ref(appender) }) else {
        return DuckdbState::DuckDBError;
    };
    wrapper.run(|appender, _| appender.append_value(value))
}

macro_rules! append_functions {
    ($($(#[$doc:meta])* $name:ident($c:ty) => $make:expr;)*) => {$(
        $(#[$doc])*
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(appender: DuckdbAppender, value: $c) -> DuckdbState {
            let make: fn($c) -> Value = $make;
            // SAFETY: `appender` is NULL or a live handle.
            let Some(wrapper) = (unsafe { appender_ref(appender) }) else {
                return DuckdbState::DuckDBError;
            };
            wrapper.run(|appender, _| appender.append_typed(&make(value)))
        }
    )*};
}

append_functions! {
    /// `duckdb_append_bool` (capi/appender-c.cpp)
    duckdb_append_bool(bool) => Value::boolean;
    /// `duckdb_append_int8` (capi/appender-c.cpp)
    duckdb_append_int8(i8) => Value::tinyint;
    /// `duckdb_append_int16` (capi/appender-c.cpp)
    duckdb_append_int16(i16) => Value::smallint;
    /// `duckdb_append_int32` (capi/appender-c.cpp)
    duckdb_append_int32(i32) => Value::integer;
    /// `duckdb_append_int64` (capi/appender-c.cpp)
    duckdb_append_int64(i64) => Value::bigint;
    /// `duckdb_append_hugeint` (capi/appender-c.cpp)
    duckdb_append_hugeint(DuckdbHugeint) => |value| Value::hugeint(value.to_i128());
    /// `duckdb_append_uint8` (capi/appender-c.cpp)
    duckdb_append_uint8(u8) => Value::utinyint;
    /// `duckdb_append_uint16` (capi/appender-c.cpp)
    duckdb_append_uint16(u16) => Value::usmallint;
    /// `duckdb_append_uint32` (capi/appender-c.cpp)
    duckdb_append_uint32(u32) => Value::uinteger;
    /// `duckdb_append_uint64` (capi/appender-c.cpp)
    duckdb_append_uint64(u64) => Value::ubigint;
    /// `duckdb_append_uhugeint` (capi/appender-c.cpp)
    duckdb_append_uhugeint(DuckdbUhugeint) => |value| Value::uhugeint(value.to_u128());
    /// `duckdb_append_float` (capi/appender-c.cpp)
    duckdb_append_float(f32) => Value::float;
    /// `duckdb_append_double` (capi/appender-c.cpp)
    duckdb_append_double(f64) => Value::double;
    /// `duckdb_append_date` (capi/appender-c.cpp)
    duckdb_append_date(DuckdbDate) => |value| Value::date(Date(value.days));
    /// `duckdb_append_time` (capi/appender-c.cpp)
    duckdb_append_time(DuckdbTime) => |value| Value::time(Time(value.micros));
    /// `duckdb_append_timestamp` (capi/appender-c.cpp)
    duckdb_append_timestamp(DuckdbTimestamp) => |value| Value::timestamp(Timestamp(value.micros));
    /// `duckdb_append_interval` (capi/appender-c.cpp)
    duckdb_append_interval(DuckdbInterval) => |value| Value::interval(Interval {
        months: value.months,
        days: value.days,
        micros: value.micros,
    });
}

/// `duckdb_state duckdb_append_null(duckdb_appender appender)` (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_append_null(appender: DuckdbAppender) -> DuckdbState {
    // SAFETY: `appender` is NULL or a live handle.
    let Some(wrapper) = (unsafe { appender_ref(appender) }) else {
        return DuckdbState::DuckDBError;
    };
    wrapper.run(|appender, _| appender.append_null())
}

/// `duckdb_state duckdb_append_default(duckdb_appender appender)` (capi/appender-c.cpp):
/// the column's constant `DEFAULT`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_append_default(appender: DuckdbAppender) -> DuckdbState {
    // SAFETY: `appender` is NULL or a live handle.
    let Some(wrapper) = (unsafe { appender_ref(appender) }) else {
        return DuckdbState::DuckDBError;
    };
    wrapper.run(|appender, _| appender.append_default())
}

/// `duckdb_state duckdb_append_default_to_chunk(duckdb_appender appender,
/// duckdb_data_chunk chunk, idx_t col, idx_t row)` (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_append_default_to_chunk(
    appender: DuckdbAppender,
    chunk: DuckdbDataChunk,
    col: Idx,
    row: Idx,
) -> DuckdbState {
    // SAFETY: `appender` and `chunk` are NULL or live handles.
    let (Some(wrapper), Some(chunk)) = (unsafe { (appender_ref(appender), chunk.as_mut()) }) else {
        return DuckdbState::DuckDBError;
    };
    wrapper.run(|appender, _| appender.append_default_to_chunk(chunk, col as usize, row as usize))
}

/// `duckdb_state duckdb_append_varchar(duckdb_appender appender, const char *val)`
/// (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_append_varchar(appender: DuckdbAppender, val: *const c_char) -> DuckdbState {
    if val.is_null() {
        return DuckdbState::DuckDBError;
    }
    // SAFETY: `val` is a NUL-terminated string.
    let length = unsafe { libc::strlen(val) } as Idx;
    // SAFETY: forwarded API contract.
    unsafe { duckdb_append_varchar_length(appender, val, length) }
}

/// `duckdb_state duckdb_append_varchar_length(duckdb_appender appender, const char *val,
/// idx_t length)` (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_append_varchar_length(
    appender: DuckdbAppender,
    val: *const c_char,
    length: Idx,
) -> DuckdbState {
    // SAFETY: `appender` is NULL or a live handle.
    let Some(wrapper) = (unsafe { appender_ref(appender) }) else {
        return DuckdbState::DuckDBError;
    };
    // SAFETY: `val` points to `length` bytes.
    let text = unsafe { bytes(val.cast(), length) };
    wrapper.run(|appender, _| appender.append_string(text))
}

/// `duckdb_state duckdb_append_blob(duckdb_appender appender, const void *data, idx_t length)`
/// (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_append_blob(
    appender: DuckdbAppender,
    data: *const c_void,
    length: Idx,
) -> DuckdbState {
    // SAFETY: `data` points to `length` bytes.
    let value = Value::blob(unsafe { bytes(data, length) });
    // SAFETY: `appender` is NULL or a live handle.
    unsafe { append(appender, &value) }
}

/// `duckdb_state duckdb_append_value(duckdb_appender appender, duckdb_value value)`
/// (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_append_value(appender: DuckdbAppender, value: DuckdbValue) -> DuckdbState {
    // SAFETY: `value` is NULL or a live handle.
    let Some(value) = (unsafe { value_ref(value) }) else {
        return DuckdbState::DuckDBError;
    };
    // SAFETY: `appender` is NULL or a live handle.
    unsafe { append(appender, value) }
}

/// `duckdb_state duckdb_append_data_chunk(duckdb_appender appender, duckdb_data_chunk chunk)`
/// (capi/appender-c.cpp): the chunk's columns are cast to the appender's.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_append_data_chunk(
    appender: DuckdbAppender,
    chunk: DuckdbDataChunk,
) -> DuckdbState {
    // SAFETY: `chunk` is NULL or a live handle.
    let Some(chunk) = (unsafe { chunk.as_ref() }) else {
        return DuckdbState::DuckDBError;
    };
    // SAFETY: `appender` is NULL or a live handle.
    let Some(wrapper) = (unsafe { appender_ref(appender) }) else {
        return DuckdbState::DuckDBError;
    };
    wrapper.run(|appender, context| append_chunk(appender, context, chunk))
}

fn append_chunk(
    appender: &mut Appender,
    context: &Weak<SharedConnection>,
    chunk: &DataChunk,
) -> roastedduck::Result<()> {
    with_connection(appender, context, |appender, connection| {
        appender.append_data_chunk(connection, chunk)
    })
}

/// `duckdb_state duckdb_appender_flush(duckdb_appender appender)` (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_flush(appender: DuckdbAppender) -> DuckdbState {
    // SAFETY: `appender` is NULL or a live handle.
    let Some(wrapper) = (unsafe { appender_ref(appender) }) else {
        return DuckdbState::DuckDBError;
    };
    wrapper.run(|appender, context| with_connection(appender, context, Appender::flush))
}

/// `duckdb_state duckdb_appender_clear(duckdb_appender appender)` (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_clear(appender: DuckdbAppender) -> DuckdbState {
    // SAFETY: `appender` is NULL or a live handle.
    let Some(wrapper) = (unsafe { appender_ref(appender) }) else {
        return DuckdbState::DuckDBError;
    };
    wrapper.run(|appender, _| {
        appender.clear();
        Ok(())
    })
}

/// `duckdb_state duckdb_appender_close(duckdb_appender appender)` (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_close(appender: DuckdbAppender) -> DuckdbState {
    // SAFETY: `appender` is NULL or a live handle.
    let Some(wrapper) = (unsafe { appender_ref(appender) }) else {
        return DuckdbState::DuckDBError;
    };
    wrapper.run(|appender, context| {
        // Closing an appender without rows does not need the connection.
        if !appender.has_pending_rows() {
            return Ok(());
        }
        with_connection(appender, context, Appender::close)
    })
}

/// `idx_t duckdb_appender_column_count(duckdb_appender appender)` (capi/appender-c.cpp): the
/// number of columns appended to.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_column_count(appender: DuckdbAppender) -> Idx {
    // SAFETY: `appender` is NULL or a live handle.
    unsafe { appender_ref(appender) }
        .and_then(|wrapper| wrapper.appender.as_ref())
        .map_or(0, |appender| appender.active_types().len() as Idx)
}

/// `duckdb_logical_type duckdb_appender_column_type(duckdb_appender appender, idx_t col_idx)`
/// (capi/appender-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_appender_column_type(
    appender: DuckdbAppender,
    col_idx: Idx,
) -> DuckdbLogicalType {
    // SAFETY: `appender` is NULL or a live handle.
    unsafe { appender_ref(appender) }
        .and_then(|wrapper| wrapper.appender.as_ref())
        .and_then(|appender| appender.active_types().get(col_idx as usize))
        .map_or(std::ptr::null_mut(), |ty: &LogicalType| wrap_type(ty.clone()))
}
