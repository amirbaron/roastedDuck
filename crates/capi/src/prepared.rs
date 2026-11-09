//! Prepared and extracted statements (DuckDB's `capi/prepared-c.cpp`).

use std::collections::HashMap;
use std::ffi::{CString, c_char, c_void};
use std::sync::Arc;

use roastedduck::{PreparedStatement, SqlStatement};
use roastedduck_common::{CaseInsensitiveMap, Error};
use roastedduck_types::{Date, Interval, LogicalType, LogicalTypeId, Time, Timestamp, Value};

use crate::helper::{bytes, c_string, catch, logical_type_id_to_c, statement_type_to_c, to_cstring};
use crate::logical_types::{DuckdbLogicalType, wrap_type};
use crate::open::{DuckdbConnection, SharedConnection, connection_ref};
use crate::result::translate_result;
use crate::types::*;
use crate::value::{DuckdbValue, value_ref};

/// An error with its message as handed to C (`ErrorData::Message`).
#[derive(Debug)]
pub(crate) struct CError {
    pub(crate) error: Error,
    pub(crate) message: CString,
}

impl CError {
    pub(crate) fn new(error: Error) -> Self {
        let message = to_cstring(error.to_bytes());
        Self { error, message }
    }
}

/// What a `duckdb_prepared_statement` points to (DuckDB's `PreparedStatementWrapper`).
#[derive(Debug)]
pub struct PreparedStatementWrapper {
    pub(crate) context: Arc<SharedConnection>,
    /// The statement, or why preparing it failed.
    pub(crate) statement: Result<PreparedStatement, CError>,
    /// The values bound so far, by parameter identifier.
    pub(crate) values: CaseInsensitiveMap<Value, roastedduck_common::ByteString>,
    /// The last binding error (`error_data` with `success` false).
    bind_error: Option<CError>,
    /// The identifier of each parameter, by index (`param_index_to_name`).
    param_index_to_name: HashMap<u64, roastedduck_common::ByteString>,
}

/// `duckdb_prepared_statement`
pub type DuckdbPreparedStatement = *mut PreparedStatementWrapper;

impl PreparedStatementWrapper {
    fn new(context: &Arc<SharedConnection>, statement: roastedduck::Result<PreparedStatement>) -> Self {
        let param_index_to_name = statement
            .as_ref()
            .map(|statement| {
                statement
                    .named_parameters()
                    .map(|(name, index)| (index, name.to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            context: Arc::clone(context),
            statement: statement.map_err(CError::new),
            values: CaseInsensitiveMap::default(),
            bind_error: None,
            param_index_to_name,
        }
    }

    /// The identifier of parameter `index` (`duckdb_parameter_name_internal`).
    fn parameter_name(&self, index: Idx) -> Option<&[u8]> {
        self.param_index_to_name
            .get(&index)
            .map(roastedduck_common::ByteString::as_bytes)
    }
}

/// The prepared statement behind a handle, if it was prepared successfully.
///
/// # Safety
/// `prepared` must be NULL or a live `duckdb_prepared_statement`.
unsafe fn prepared_ref<'a>(prepared: DuckdbPreparedStatement) -> Option<&'a mut PreparedStatementWrapper> {
    // SAFETY: guaranteed by the caller.
    unsafe { prepared.as_mut() }.filter(|wrapper| wrapper.statement.is_ok())
}

/// Hands a prepared statement to C; `DuckDBError` if preparing it failed.
///
/// # Safety
/// `out` must point to writable storage.
unsafe fn store_prepared(
    context: &Arc<SharedConnection>,
    statement: roastedduck::Result<PreparedStatement>,
    out: *mut DuckdbPreparedStatement,
) -> DuckdbState {
    let wrapper = PreparedStatementWrapper::new(context, statement);
    let state = DuckdbState::from_ok(wrapper.statement.is_ok());
    // SAFETY: guaranteed by the caller.
    unsafe { *out = Box::into_raw(Box::new(wrapper)) };
    state
}

/// What a `duckdb_extracted_statements` points to (DuckDB's `ExtractStatementsWrapper`).
#[derive(Debug)]
pub struct ExtractStatementsWrapper {
    statements: Vec<SqlStatement>,
    error: Option<CString>,
}

/// `duckdb_extracted_statements`
pub type DuckdbExtractedStatements = *mut ExtractStatementsWrapper;

/// `idx_t duckdb_extract_statements(duckdb_connection connection, const char *query,
/// duckdb_extracted_statements *out_extracted_statements)` (capi/prepared-c.cpp): the
/// number of statements, 0 if the query does not parse.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_extract_statements(
    connection: DuckdbConnection,
    query: *const c_char,
    out_extracted_statements: *mut DuckdbExtractedStatements,
) -> Idx {
    // SAFETY: `connection` is NULL or a live handle.
    let Some(context) = (unsafe { connection_ref(connection) }) else {
        return 0;
    };
    if query.is_null() || out_extracted_statements.is_null() {
        return 0;
    }
    // SAFETY: `query` is a NUL-terminated string.
    let query = unsafe { crate::helper::opt_bytes(query) }.unwrap_or_default();
    let (statements, error) = match catch(|| context.lock().parse_statements_bytes(query)) {
        Ok(statements) => (statements, None),
        Err(error) => (Vec::new(), Some(to_cstring(error.to_bytes()))),
    };
    let count = statements.len() as Idx;
    // SAFETY: checked non-null above.
    unsafe {
        *out_extracted_statements = Box::into_raw(Box::new(ExtractStatementsWrapper { statements, error }));
    };
    count
}

/// `duckdb_state duckdb_prepare_extracted_statement(duckdb_connection connection,
/// duckdb_extracted_statements extracted_statements, idx_t index,
/// duckdb_prepared_statement *out_prepared_statement)` (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_prepare_extracted_statement(
    connection: DuckdbConnection,
    extracted_statements: DuckdbExtractedStatements,
    index: Idx,
    out_prepared_statement: *mut DuckdbPreparedStatement,
) -> DuckdbState {
    // SAFETY: both are NULL or live handles.
    let (Some(context), Some(extracted)) =
        (unsafe { (connection_ref(connection), extracted_statements.as_ref()) })
    else {
        return DuckdbState::DuckDBError;
    };
    let Some(statement) = extracted.statements.get(index as usize) else {
        return DuckdbState::DuckDBError;
    };
    if out_prepared_statement.is_null() {
        return DuckdbState::DuckDBError;
    }
    let prepared = catch(|| context.lock().prepare_statement(statement.clone()));
    // SAFETY: checked non-null above.
    unsafe { store_prepared(context, prepared, out_prepared_statement) }
}

/// `const char *duckdb_extract_statements_error(duckdb_extracted_statements extracted_statements)`
/// (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_extract_statements_error(
    extracted_statements: DuckdbExtractedStatements,
) -> *const c_char {
    // SAFETY: `extracted_statements` is NULL or a live handle.
    unsafe { extracted_statements.as_ref() }
        .and_then(|wrapper| wrapper.error.as_ref())
        .map_or(std::ptr::null(), |error| error.as_ptr())
}

/// `duckdb_state duckdb_prepare(duckdb_connection connection, const char *query,
/// duckdb_prepared_statement *out_prepared_statement)` (capi/prepared-c.cpp): on failure
/// the statement is still handed out, with the error, and must be destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_prepare(
    connection: DuckdbConnection,
    query: *const c_char,
    out_prepared_statement: *mut DuckdbPreparedStatement,
) -> DuckdbState {
    // SAFETY: `connection` is NULL or a live handle.
    let Some(context) = (unsafe { connection_ref(connection) }) else {
        return DuckdbState::DuckDBError;
    };
    if query.is_null() || out_prepared_statement.is_null() {
        return DuckdbState::DuckDBError;
    }
    // SAFETY: `query` is a NUL-terminated string.
    let query = unsafe { crate::helper::opt_bytes(query) }.unwrap_or_default();
    let prepared = catch(|| context.lock().prepare(query));
    // SAFETY: checked non-null above.
    unsafe { store_prepared(context, prepared, out_prepared_statement) }
}

/// `const char *duckdb_prepare_error(duckdb_prepared_statement prepared_statement)`
/// (capi/prepared-c.cpp): the error of a failed binding, else that of the preparation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_prepare_error(prepared_statement: DuckdbPreparedStatement) -> *const c_char {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    let Some(wrapper) = (unsafe { prepared_statement.as_ref() }) else {
        return std::ptr::null();
    };
    if let Some(error) = &wrapper.bind_error {
        return error.message.as_ptr();
    }
    wrapper
        .statement
        .as_ref()
        .err()
        .map_or(std::ptr::null(), |error| error.message.as_ptr())
}

/// `idx_t duckdb_nparams(duckdb_prepared_statement prepared_statement)` (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_nparams(prepared_statement: DuckdbPreparedStatement) -> Idx {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    unsafe { prepared_ref(prepared_statement) }.map_or(0, |wrapper| wrapper.param_index_to_name.len() as Idx)
}

/// `const char *duckdb_parameter_name(duckdb_prepared_statement prepared_statement, idx_t index)`
/// (capi/prepared-c.cpp): allocated with `duckdb_malloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_parameter_name(
    prepared_statement: DuckdbPreparedStatement,
    index: Idx,
) -> *const c_char {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    unsafe { prepared_ref(prepared_statement) }
        .and_then(|wrapper| wrapper.parameter_name(index))
        .map_or(std::ptr::null(), |name| c_string(name).cast_const())
}

/// `duckdb_type duckdb_param_type(duckdb_prepared_statement prepared_statement, idx_t param_idx)`
/// (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_param_type(
    prepared_statement: DuckdbPreparedStatement,
    param_idx: Idx,
) -> DuckdbType {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    unsafe { parameter_type(prepared_statement, param_idx) }
        .map_or(DUCKDB_TYPE_INVALID, |ty| logical_type_id_to_c(ty.id()))
}

/// The type of parameter `param_idx`: as bound when the statement was prepared, else that
/// of the value bound to it (`TryGetType`, then `values`).
///
/// # Safety
/// `prepared_statement` must be NULL or a live `duckdb_prepared_statement`.
unsafe fn parameter_type(prepared_statement: DuckdbPreparedStatement, param_idx: Idx) -> Option<LogicalType> {
    // SAFETY: guaranteed by the caller.
    let wrapper = unsafe { prepared_ref(prepared_statement) }?;
    let identifier = wrapper.parameter_name(param_idx)?;
    let statement = wrapper.statement.as_ref().ok()?;
    statement
        .parameter_type(identifier)
        .or_else(|| wrapper.values.get(identifier).map(parameter_value_type))
}

/// The type of a bound value (`BoundParameterData::return_type`): strings without a
/// collation bind as string literals.
fn parameter_value_type(value: &Value) -> LogicalType {
    let ty = value.logical_type();
    if ty.id() == LogicalTypeId::Varchar && roastedduck_planner::expression_binder::collation(ty).is_none() {
        return LogicalType::STRING_LITERAL;
    }
    ty.clone()
}

/// `duckdb_logical_type duckdb_param_logical_type(duckdb_prepared_statement prepared_statement,
/// idx_t param_idx)` (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_param_logical_type(
    prepared_statement: DuckdbPreparedStatement,
    param_idx: Idx,
) -> DuckdbLogicalType {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    unsafe { parameter_type(prepared_statement, param_idx) }.map_or(std::ptr::null_mut(), wrap_type)
}

/// `duckdb_state duckdb_clear_bindings(duckdb_prepared_statement prepared_statement)`
/// (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_clear_bindings(prepared_statement: DuckdbPreparedStatement) -> DuckdbState {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    let Some(wrapper) = (unsafe { prepared_ref(prepared_statement) }) else {
        return DuckdbState::DuckDBError;
    };
    wrapper.values = CaseInsensitiveMap::default();
    DuckdbState::DuckDBSuccess
}

/// `idx_t duckdb_prepared_statement_column_count(duckdb_prepared_statement prepared_statement)`
/// (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_prepared_statement_column_count(
    prepared_statement: DuckdbPreparedStatement,
) -> Idx {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    unsafe { prepared_ref(prepared_statement) }
        .and_then(|wrapper| wrapper.statement.as_ref().ok())
        .map_or(0, |statement| statement.types().len() as Idx)
}

/// `const char *duckdb_prepared_statement_column_name(duckdb_prepared_statement prepared_statement,
/// idx_t col_idx)` (capi/prepared-c.cpp): allocated with `duckdb_malloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_prepared_statement_column_name(
    prepared_statement: DuckdbPreparedStatement,
    col_idx: Idx,
) -> *const c_char {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    unsafe { prepared_ref(prepared_statement) }
        .and_then(|wrapper| wrapper.statement.as_ref().ok())
        .and_then(|statement| statement.names().get(col_idx as usize))
        .map_or(std::ptr::null(), |name| c_string(name).cast_const())
}

/// `duckdb_logical_type duckdb_prepared_statement_column_logical_type(
/// duckdb_prepared_statement prepared_statement, idx_t col_idx)` (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_prepared_statement_column_logical_type(
    prepared_statement: DuckdbPreparedStatement,
    col_idx: Idx,
) -> DuckdbLogicalType {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    unsafe { prepared_ref(prepared_statement) }
        .and_then(|wrapper| wrapper.statement.as_ref().ok())
        .and_then(|statement| statement.types().get(col_idx as usize))
        .map_or(std::ptr::null_mut(), |ty| wrap_type(ty.clone()))
}

/// `duckdb_type duckdb_prepared_statement_column_type(duckdb_prepared_statement prepared_statement,
/// idx_t col_idx)` (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_prepared_statement_column_type(
    prepared_statement: DuckdbPreparedStatement,
    col_idx: Idx,
) -> DuckdbType {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    unsafe { prepared_ref(prepared_statement) }
        .and_then(|wrapper| wrapper.statement.as_ref().ok())
        .and_then(|statement| statement.types().get(col_idx as usize))
        .map_or(DUCKDB_TYPE_INVALID, |ty| logical_type_id_to_c(ty.id()))
}

/// Binds `value` to parameter `param_idx` (`duckdb_bind_value`).
fn bind(wrapper: &mut PreparedStatementWrapper, param_idx: Idx, value: Value) -> DuckdbState {
    let count = wrapper.param_index_to_name.len();
    let Some(identifier) = wrapper.parameter_name(param_idx).filter(|_| param_idx > 0) else {
        wrapper.bind_error = Some(CError::new(Error::invalid_input(format!(
            "Can not bind to parameter number {param_idx}, statement only has {count} parameter(s)"
        ))));
        return DuckdbState::DuckDBError;
    };
    let identifier = roastedduck_common::ByteString::from(identifier);
    wrapper.values.insert(identifier, value);
    DuckdbState::DuckDBSuccess
}

/// `duckdb_state duckdb_bind_value(duckdb_prepared_statement prepared_statement, idx_t param_idx,
/// duckdb_value val)` (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_value(
    prepared_statement: DuckdbPreparedStatement,
    param_idx: Idx,
    val: DuckdbValue,
) -> DuckdbState {
    // SAFETY: both are NULL or live handles.
    let (Some(wrapper), Some(value)) = (unsafe { (prepared_ref(prepared_statement), value_ref(val)) }) else {
        return DuckdbState::DuckDBError;
    };
    bind(wrapper, param_idx, value.clone())
}

/// `duckdb_state duckdb_bind_parameter_index(duckdb_prepared_statement prepared_statement,
/// idx_t *param_idx_out, const char *name)` (capi/prepared-c.cpp): case-insensitive.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_parameter_index(
    prepared_statement: DuckdbPreparedStatement,
    param_idx_out: *mut Idx,
    name: *const c_char,
) -> DuckdbState {
    // SAFETY: `prepared_statement` is NULL or a live handle; `name` NULL or a C string.
    let (Some(wrapper), Some(name)) =
        (unsafe { (prepared_ref(prepared_statement), crate::helper::opt_bytes(name)) })
    else {
        return DuckdbState::DuckDBError;
    };
    if param_idx_out.is_null() {
        return DuckdbState::DuckDBError;
    }
    let found = wrapper
        .param_index_to_name
        .iter()
        .find(|(_, identifier)| identifier.eq_ignore_ascii_case(name));
    match found {
        Some((index, _)) => {
            // SAFETY: checked non-null above.
            unsafe { *param_idx_out = *index };
            DuckdbState::DuckDBSuccess
        }
        None => DuckdbState::DuckDBError,
    }
}

/// The `duckdb_bind_*` functions of the fixed-width types.
macro_rules! bind_functions {
    ($($(#[$doc:meta])* $name:ident($c:ty) => $make:expr;)*) => {$(
        $(#[$doc])*
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(
            prepared_statement: DuckdbPreparedStatement,
            param_idx: Idx,
            val: $c,
        ) -> DuckdbState {
            // SAFETY: `prepared_statement` is NULL or a live handle.
            let Some(wrapper) = (unsafe { prepared_ref(prepared_statement) }) else {
                return DuckdbState::DuckDBError;
            };
            let make: fn($c) -> Value = $make;
            bind(wrapper, param_idx, make(val))
        }
    )*};
}

bind_functions! {
    /// `duckdb_bind_boolean` (capi/prepared-c.cpp)
    duckdb_bind_boolean(bool) => Value::boolean;
    /// `duckdb_bind_int8` (capi/prepared-c.cpp)
    duckdb_bind_int8(i8) => Value::tinyint;
    /// `duckdb_bind_int16` (capi/prepared-c.cpp)
    duckdb_bind_int16(i16) => Value::smallint;
    /// `duckdb_bind_int32` (capi/prepared-c.cpp)
    duckdb_bind_int32(i32) => Value::integer;
    /// `duckdb_bind_int64` (capi/prepared-c.cpp)
    duckdb_bind_int64(i64) => Value::bigint;
    /// `duckdb_bind_hugeint` (capi/prepared-c.cpp)
    duckdb_bind_hugeint(DuckdbHugeint) => |val| Value::hugeint(val.to_i128());
    /// `duckdb_bind_uhugeint` (capi/prepared-c.cpp)
    duckdb_bind_uhugeint(DuckdbUhugeint) => |val| Value::uhugeint(val.to_u128());
    /// `duckdb_bind_uint8` (capi/prepared-c.cpp)
    duckdb_bind_uint8(u8) => Value::utinyint;
    /// `duckdb_bind_uint16` (capi/prepared-c.cpp)
    duckdb_bind_uint16(u16) => Value::usmallint;
    /// `duckdb_bind_uint32` (capi/prepared-c.cpp)
    duckdb_bind_uint32(u32) => Value::uinteger;
    /// `duckdb_bind_uint64` (capi/prepared-c.cpp)
    duckdb_bind_uint64(u64) => Value::ubigint;
    /// `duckdb_bind_float` (capi/prepared-c.cpp)
    duckdb_bind_float(f32) => Value::float;
    /// `duckdb_bind_double` (capi/prepared-c.cpp)
    duckdb_bind_double(f64) => Value::double;
    /// `duckdb_bind_date` (capi/prepared-c.cpp)
    duckdb_bind_date(DuckdbDate) => |val| Value::date(Date(val.days));
    /// `duckdb_bind_time` (capi/prepared-c.cpp)
    duckdb_bind_time(DuckdbTime) => |val| Value::time(Time(val.micros));
    /// `duckdb_bind_timestamp` (capi/prepared-c.cpp)
    duckdb_bind_timestamp(DuckdbTimestamp) => |val| Value::timestamp(Timestamp(val.micros));
    /// `duckdb_bind_timestamp_tz` (capi/prepared-c.cpp)
    duckdb_bind_timestamp_tz(DuckdbTimestamp) => |val| Value::timestamp_tz(Timestamp(val.micros));
    /// `duckdb_bind_interval` (capi/prepared-c.cpp)
    duckdb_bind_interval(DuckdbInterval) => |val| Value::interval(Interval {
        months: val.months,
        days: val.days,
        micros: val.micros,
    });
    /// `duckdb_bind_decimal` (capi/prepared-c.cpp): a decimal of up to 18 digits takes
    /// the low 64 bits of the value.
    duckdb_bind_decimal(DuckdbDecimal) => |val| {
        let unscaled = if val.width > 18 {
            val.value.to_i128()
        } else {
            i128::from(val.value.lower as i64)
        };
        Value::decimal(unscaled, val.width, val.scale)
    };
}

/// `duckdb_state duckdb_bind_varchar(duckdb_prepared_statement prepared_statement, idx_t param_idx,
/// const char *val)` (capi/prepared-c.cpp): fails for invalid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_varchar(
    prepared_statement: DuckdbPreparedStatement,
    param_idx: Idx,
    val: *const c_char,
) -> DuckdbState {
    if val.is_null() {
        return DuckdbState::DuckDBError;
    }
    // SAFETY: `val` is a NUL-terminated string.
    let length = unsafe { libc::strlen(val) } as Idx;
    // SAFETY: forwarded API contract.
    unsafe { duckdb_bind_varchar_length(prepared_statement, param_idx, val, length) }
}

/// `duckdb_state duckdb_bind_varchar_length(duckdb_prepared_statement prepared_statement,
/// idx_t param_idx, const char *val, idx_t length)` (capi/prepared-c.cpp): fails for invalid
/// UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_varchar_length(
    prepared_statement: DuckdbPreparedStatement,
    param_idx: Idx,
    val: *const c_char,
    length: Idx,
) -> DuckdbState {
    // SAFETY: `prepared_statement` is NULL or a live handle; `val` points to `length` bytes.
    let (Some(wrapper), text) = (unsafe { (prepared_ref(prepared_statement), bytes(val.cast(), length)) })
    else {
        return DuckdbState::DuckDBError;
    };
    match std::str::from_utf8(text) {
        Ok(text) => bind(wrapper, param_idx, Value::varchar(text)),
        Err(_) => DuckdbState::DuckDBError,
    }
}

/// `duckdb_state duckdb_bind_blob(duckdb_prepared_statement prepared_statement, idx_t param_idx,
/// const void *data, idx_t length)` (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_blob(
    prepared_statement: DuckdbPreparedStatement,
    param_idx: Idx,
    data: *const c_void,
    length: Idx,
) -> DuckdbState {
    // SAFETY: `prepared_statement` is NULL or a live handle; `data` points to `length` bytes.
    let (Some(wrapper), data) = (unsafe { (prepared_ref(prepared_statement), bytes(data, length)) }) else {
        return DuckdbState::DuckDBError;
    };
    bind(wrapper, param_idx, Value::blob(data))
}

/// `duckdb_state duckdb_bind_null(duckdb_prepared_statement prepared_statement, idx_t param_idx)`
/// (capi/prepared-c.cpp): a NULL of type `"NULL"`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_bind_null(
    prepared_statement: DuckdbPreparedStatement,
    param_idx: Idx,
) -> DuckdbState {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    let Some(wrapper) = (unsafe { prepared_ref(prepared_statement) }) else {
        return DuckdbState::DuckDBError;
    };
    bind(wrapper, param_idx, Value::default())
}

/// Runs the statement with its bound values (`PreparedStatement::Execute`).
///
/// # Safety
/// As for `duckdb_execute_prepared`.
unsafe fn execute_prepared(
    prepared_statement: DuckdbPreparedStatement,
    out_result: *mut DuckdbResult,
    allow_stream: bool,
) -> DuckdbState {
    // SAFETY: `prepared_statement` is NULL or a live handle.
    let Some(wrapper) = (unsafe { prepared_ref(prepared_statement) }) else {
        return DuckdbState::DuckDBError;
    };
    let Ok(statement) = &wrapper.statement else {
        return DuckdbState::DuckDBError;
    };
    let (outcome, properties) = {
        let mut connection = wrapper.context.lock();
        let outcome = catch(|| connection.execute_prepared(statement, &wrapper.values, allow_stream));
        (outcome, connection.client_properties())
    };
    // SAFETY: `out_result` is NULL or writable.
    unsafe { translate_result(&wrapper.context, outcome, properties, out_result) }
}

/// `duckdb_state duckdb_execute_prepared(duckdb_prepared_statement prepared_statement,
/// duckdb_result *out_result)` (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_execute_prepared(
    prepared_statement: DuckdbPreparedStatement,
    out_result: *mut DuckdbResult,
) -> DuckdbState {
    // SAFETY: forwarded API contract.
    unsafe { execute_prepared(prepared_statement, out_result, false) }
}

/// `duckdb_state duckdb_execute_prepared_streaming(duckdb_prepared_statement prepared_statement,
/// duckdb_result *out_result)` (capi/prepared-c.cpp): a query's rows are produced as they
/// are fetched.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_execute_prepared_streaming(
    prepared_statement: DuckdbPreparedStatement,
    out_result: *mut DuckdbResult,
) -> DuckdbState {
    // SAFETY: forwarded API contract.
    unsafe { execute_prepared(prepared_statement, out_result, true) }
}

/// `duckdb_statement_type duckdb_prepared_statement_type(duckdb_prepared_statement statement)`
/// (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_prepared_statement_type(
    statement: DuckdbPreparedStatement,
) -> DuckdbStatementType {
    // SAFETY: `statement` is NULL or a live handle.
    unsafe { prepared_ref(statement) }
        .and_then(|wrapper| wrapper.statement.as_ref().ok())
        .map_or(DUCKDB_STATEMENT_TYPE_INVALID, |statement| {
            statement_type_to_c(statement.statement_type())
        })
}

/// `void duckdb_destroy_extracted(duckdb_extracted_statements *extracted_statements)`
/// (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_extracted(extracted_statements: *mut DuckdbExtractedStatements) {
    // SAFETY: `extracted_statements` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = extracted_statements.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `void duckdb_destroy_prepare(duckdb_prepared_statement *prepared_statement)` (capi/prepared-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_prepare(prepared_statement: *mut DuckdbPreparedStatement) {
    // SAFETY: `prepared_statement` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = prepared_statement.as_mut() else {
            return;
        };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}
