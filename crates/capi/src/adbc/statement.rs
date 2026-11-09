//! Prepared statement lifecycle. SQL is passed as native C-string bytes.
use super::*;
unsafe fn valid<'a>(value: *mut AdbcStatement, error: *mut AdbcError) -> Result<&'a mut StatementState, u8> {
    // SAFETY: caller follows the ADBC statement/error handle contract.
    unsafe {
        if value.is_null() {
            return Err(fail(error, INVALID_ARGUMENT, b"Missing statement object"));
        }
        statement(value).ok_or_else(|| fail(error, INVALID_ARGUMENT, b"Invalid statement object"))
    }
}
pub(super) unsafe extern "C" fn new(
    conn: *mut AdbcConnection,
    value: *mut AdbcStatement,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: caller owns writable handle; allocated state is released exactly once.
    unsafe {
        if conn.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing connection object");
        }
        let Some(connection) = connection(conn) else {
            return fail(error, INVALID_ARGUMENT, b"Invalid connection object");
        };
        if value.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing statement object");
        }
        (*value).private_data = Box::into_raw(Box::new(StatementState {
            connection: Some(Arc::clone(connection)),
            prepared: std::ptr::null_mut(),
            input: ArrowArrayStream::empty(),
            ingest: options::IngestOptions::default(),
        }))
        .cast();
        OK
    }
}
pub(super) unsafe extern "C" fn release(value: *mut AdbcStatement, _error: *mut AdbcError) -> u8 {
    // SAFETY: only an allocated, unreleased state is converted back into its Box.
    unsafe {
        if !value.is_null() && !(*value).private_data.is_null() {
            drop(Box::from_raw((*value).private_data.cast::<StatementState>()));
            (*value).private_data = std::ptr::null_mut();
        }
        OK
    }
}
pub(super) unsafe extern "C" fn prepare(value: *mut AdbcStatement, error: *mut AdbcError) -> u8 {
    // SAFETY: preparation occurs in set_sql; this callback only validates the handle.
    unsafe { valid(value, error).map_or_else(|code| code, |_| OK) }
}
pub(super) unsafe extern "C" fn cancel(value: *mut AdbcStatement, error: *mut AdbcError) -> u8 {
    // SAFETY: the Arc keeps the connection state live; interrupt does not acquire its query lock.
    unsafe {
        let state = match valid(value, error) {
            Ok(state) => state,
            Err(code) => return code,
        };
        let Some(connection) = &state.connection else {
            return fail(error, INVALID_ARGUMENT, b"Invalid statement object");
        };
        if connection.handle().is_null() {
            return fail(error, INVALID_ARGUMENT, b"Invalid statement object");
        }
        duckdb_interrupt(connection.handle());
        OK
    }
}
pub(super) unsafe extern "C" fn set_sql(
    value: *mut AdbcStatement,
    query: *const c_char,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: native query/error strings are borrowed only while their handles remain live.
    unsafe {
        let state = match valid(value, error) {
            Ok(state) => state,
            Err(code) => return code,
        };
        let Some(bytes) = opt_bytes(query) else {
            return fail(error, INVALID_ARGUMENT, b"Missing query");
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return fail(error, INVALID_ARGUMENT, b"No statements found");
        }
        let Some(connection) = &state.connection else {
            return fail(error, INVALID_STATE, b"Invalid connection");
        };
        connection.materialize();
        if let Some(release) = state.input.release {
            release(&raw mut state.input);
        }
        duckdb_destroy_prepare(&raw mut state.prepared);
        let mut extracted = std::ptr::null_mut();
        let count = duckdb_extract_statements(connection.handle(), query, &raw mut extracted);
        if let Some(message) = opt_bytes(duckdb_extract_statements_error(extracted)) {
            let code = fail(error, INTERNAL, message);
            duckdb_destroy_extracted(&raw mut extracted);
            return code;
        }
        let mut code = OK;
        for i in 0..count {
            let mut prepared = std::ptr::null_mut();
            let result =
                duckdb_prepare_extracted_statement(connection.handle(), extracted, i, &raw mut prepared);
            code = check(
                result,
                error,
                opt_bytes(duckdb_prepare_error(prepared)).unwrap_or_default(),
            );
            if code == OK && i + 1 == count {
                state.prepared = prepared;
                break;
            }
            if code == OK {
                let mut result = std::ptr::null_mut();
                if duckdb_execute_prepared_arrow(prepared, &raw mut result) != DuckdbState::DuckDBSuccess {
                    code = fail(
                        error,
                        INVALID_ARGUMENT,
                        opt_bytes(duckdb_query_arrow_error(result)).unwrap_or_default(),
                    );
                }
                duckdb_destroy_arrow(&raw mut result);
            }
            duckdb_destroy_prepare(&raw mut prepared);
            if code != OK {
                break;
            }
        }
        duckdb_destroy_extracted(&raw mut extracted);
        code
    }
}
pub(super) unsafe extern "C" fn execute(
    value: *mut AdbcStatement,
    out: *mut ArrowArrayStream,
    rows: *mut i64,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: result ownership transfers to the output stream or is dropped if no output is requested.
    unsafe {
        let state = match valid(value, error) {
            Ok(state) => state,
            Err(code) => return code,
        };
        let Some(connection) = &state.connection else {
            return fail(error, INVALID_STATE, b"Invalid connection");
        };
        if connection.handle().is_null() {
            return fail(error, INVALID_STATE, b"Invalid connection");
        }
        connection.materialize();
        if !rows.is_null() {
            *rows = 0;
        }
        if state.input.release.is_some() && state.ingest.table.is_some() {
            return ingest::execute(
                connection.handle(),
                &state.ingest,
                &raw mut state.input,
                rows,
                error,
            );
        }
        if state.prepared.is_null() {
            if !out.is_null() {
                *out = ArrowArrayStream::empty();
            }
            return OK;
        }
        let mut result = empty_result();
        let connection = Arc::clone(connection);
        let code = if state.input.release.is_some() && duckdb_nparams(state.prepared) > 0 {
            execute_bound(state, &raw mut result, error)
        } else {
            execute_result(state.prepared, &raw mut result, error)
        };
        if code != OK {
            duckdb_destroy_result(&raw mut result);
            return code;
        }
        if !rows.is_null() {
            let changed = duckdb_rows_changed(&raw mut result);
            *rows = if changed > 0 { changed as i64 } else { -1 };
        }
        stream::export(&connection, result, out);
        OK
    }
}
pub(super) unsafe extern "C" fn parameter_schema(
    value: *mut AdbcStatement,
    out: *mut ArrowSchema,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: the existing C API converter owns schema allocation and handles NULL parameters.
    unsafe {
        let state = match valid(value, error) {
            Ok(state) => state,
            Err(code) => return code,
        };
        if out.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing schema object");
        }
        if state.prepared.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Invalid prepared statement wrapper");
        }
        let mut pointer = out;
        if duckdb_prepared_arrow_schema(state.prepared, &raw mut pointer) == DuckdbState::DuckDBSuccess {
            OK
        } else {
            fail(error, INVALID_ARGUMENT, b"Invalid prepared statement wrapper")
        }
    }
}

pub(super) unsafe extern "C" fn execute_schema(
    value: *mut AdbcStatement,
    out: *mut ArrowSchema,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: all returned logical types, option handles and owned names are released after conversion.
    unsafe {
        let state = match valid(value, error) {
            Ok(state) => state,
            Err(code) => return code,
        };
        if out.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing schema object");
        }
        if state.prepared.is_null() {
            return fail(
                error,
                INVALID_STATE,
                b"Must call StatementSetSqlQuery before StatementExecuteSchema",
            );
        }
        let Some(connection) = &state.connection else {
            return fail(error, INVALID_STATE, b"Invalid connection");
        };
        connection.materialize();
        let count = duckdb_prepared_statement_column_count(state.prepared);
        let mut types: Vec<_> = (0..count)
            .map(|i| duckdb_prepared_statement_column_logical_type(state.prepared, i))
            .collect();
        let names: Vec<_> = (0..count)
            .map(|i| duckdb_prepared_statement_column_name(state.prepared, i))
            .collect();
        let mut options = std::ptr::null_mut();
        duckdb_connection_get_arrow_options(connection.handle(), &raw mut options);
        let mut conversion = duckdb_to_arrow_schema(options, types.as_mut_ptr(), names.as_ptr(), count, out);
        for item in &mut types {
            duckdb_destroy_logical_type(item);
        }
        for name in names {
            duckdb_free(name.cast_mut().cast());
        }
        duckdb_destroy_arrow_options(&raw mut options);
        let code = if conversion.is_null() {
            OK
        } else {
            fail(
                error,
                INVALID_ARGUMENT,
                opt_bytes(duckdb_error_data_message(conversion)).unwrap_or_default(),
            )
        };
        if !conversion.is_null() {
            errors::append(error, duckdb_error_data_error_type(conversion));
        }
        duckdb_destroy_error_data(&raw mut conversion);
        code
    }
}

pub(super) unsafe extern "C" fn bind(
    value: *mut AdbcStatement,
    array: *mut ArrowArray,
    schema: *mut ArrowSchema,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: the statement owns its input; from_batch validates Arrow initialization before moving it.
    unsafe {
        let state = match valid(value, error) {
            Ok(state) => state,
            Err(code) => return code,
        };
        if array.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing values object");
        }
        if schema.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Invalid schemas object");
        }
        if let Some(release) = state.input.release {
            release(&raw mut state.input);
        }
        batch::from_batch(array, schema, &raw mut state.input, error)
    }
}
pub(super) unsafe extern "C" fn bind_stream(
    value: *mut AdbcStatement,
    input: *mut ArrowArrayStream,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: release ownership transfers to the statement; the source release slot is cleared.
    unsafe {
        let state = match valid(value, error) {
            Ok(state) => state,
            Err(code) => return code,
        };
        if input.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing values object");
        }
        if let Some(release) = state.input.release {
            release(&raw mut state.input);
        }
        state.input = std::ptr::read(input);
        (*input).release = None;
        OK
    }
}

struct ConvertedSchema(DuckdbArrowConvertedSchema);
impl Drop for ConvertedSchema {
    fn drop(&mut self) {
        // SAFETY: this guard owns its conversion handle.
        unsafe {
            duckdb_destroy_arrow_converted_schema(&raw mut self.0);
        }
    }
}
struct Chunk(DuckdbDataChunk);
impl Drop for Chunk {
    fn drop(&mut self) {
        // SAFETY: this guard owns its data chunk.
        unsafe {
            duckdb_destroy_data_chunk(&raw mut self.0);
        }
    }
}
unsafe fn conversion_status(mut conversion: DuckdbErrorData, error: *mut AdbcError) -> u8 {
    // SAFETY: conversion is owned here, and its message is copied before destruction.
    unsafe {
        if conversion.is_null() {
            return OK;
        }
        let code = fail(
            error,
            INVALID_ARGUMENT,
            opt_bytes(duckdb_error_data_message(conversion)).unwrap_or_default(),
        );
        if !conversion.is_null() {
            errors::append(error, duckdb_error_data_error_type(conversion));
        }
        duckdb_destroy_error_data(&raw mut conversion);
        code
    }
}
unsafe fn execute_bound(state: &mut StatementState, result: *mut DuckdbResult, error: *mut AdbcError) -> u8 {
    // SAFETY: statement owns its input stream; all imported Arrow/chunk/schema handles have RAII guards.
    unsafe {
        let handle = state
            .connection
            .as_ref()
            .map_or(std::ptr::null_mut(), |connection| connection.handle());
        let Some(get_schema) = state.input.get_schema else {
            return fail(error, INVALID_ARGUMENT, b"Invalid input stream");
        };
        let Some(get_next) = state.input.get_next else {
            return fail(error, INVALID_ARGUMENT, b"Invalid input stream");
        };
        let mut schema = roastedduck_arrow::ffi::ArrowSchemaWrapper::default();
        get_schema(&raw mut state.input, &raw mut schema.arrow_schema);
        let mut converted = ConvertedSchema(std::ptr::null_mut());
        let code = conversion_status(
            duckdb_schema_from_arrow(handle, &raw mut schema.arrow_schema, &raw mut converted.0),
            error,
        );
        if code != OK {
            return code;
        }
        loop {
            let mut array = roastedduck_arrow::ffi::ArrowArrayWrapper::default();
            get_next(&raw mut state.input, &raw mut array.arrow_array);
            if array.arrow_array.release.is_none() {
                break;
            }
            let mut chunk = Chunk(std::ptr::null_mut());
            let code = conversion_status(
                duckdb_data_chunk_from_arrow(
                    handle,
                    &raw mut array.arrow_array,
                    converted.0,
                    &raw mut chunk.0,
                ),
                error,
            );
            if code != OK {
                return code;
            }
            let Some(data) = chunk.0.as_ref() else {
                return fail(
                    error,
                    INVALID_ARGUMENT,
                    b"Please provide a non-empty chunk to be bound",
                );
            };
            if data.is_empty() {
                return fail(
                    error,
                    INVALID_ARGUMENT,
                    b"Please provide a non-empty chunk to be bound",
                );
            }
            if data.len() != 1 {
                return fail(
                    error,
                    NOT_IMPLEMENTED,
                    b"Binding multiple rows at once is not supported yet",
                );
            }
            if data.column_count() > duckdb_nparams(state.prepared) as usize {
                return fail(
                    error,
                    INVALID_ARGUMENT,
                    b"Input data has more column than prepared statement has parameters",
                );
            }
            duckdb_clear_bindings(state.prepared);
            for index in 0..data.column_count() {
                let mut value = data.column(index).get_value(0);
                if duckdb_bind_value(state.prepared, 1 + index as Idx, &raw mut value)
                    != DuckdbState::DuckDBSuccess
                {
                    return fail(
                        error,
                        INVALID_ARGUMENT,
                        opt_bytes(duckdb_prepare_error(state.prepared)).unwrap_or_default(),
                    );
                }
            }
            duckdb_destroy_result(result);
            let code = execute_result(state.prepared, result, error);
            if code != OK {
                return code;
            }
        }
        OK
    }
}
unsafe fn execute_result(
    prepared: DuckdbPreparedStatement,
    result: *mut DuckdbResult,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: prepared and output result are owned by the calling statement execution.
    unsafe {
        if duckdb_execute_prepared_streaming(prepared, result) == DuckdbState::DuckDBSuccess {
            return OK;
        }
        let message = opt_bytes(duckdb_result_error(result)).unwrap_or_default();
        let code = if message == b"Interrupted!" {
            CANCELLED
        } else {
            INVALID_ARGUMENT
        };
        set_error(error, message);
        errors::append(error, duckdb_result_error_type(result));
        code
    }
}
