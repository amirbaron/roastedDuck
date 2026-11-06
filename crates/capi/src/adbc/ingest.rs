//! Stream-to-table ingestion using the native C API appender, as in the frozen driver.
use super::*;
use options::{IngestOptions, Mode};
use rustdb_arrow::ffi::{ArrowArrayWrapper, ArrowSchemaWrapper};
struct Types(DuckdbArrowConvertedSchema);
impl Drop for Types {
    fn drop(&mut self) {
        // SAFETY: this guard owns the converted Arrow schema.
        unsafe {
            duckdb_destroy_arrow_converted_schema(&raw mut self.0);
        }
    }
}
struct Chunk(DuckdbDataChunk);
impl Drop for Chunk {
    fn drop(&mut self) {
        // SAFETY: this guard owns the imported data chunk.
        unsafe {
            duckdb_destroy_data_chunk(&raw mut self.0);
        }
    }
}
struct Appender(DuckdbAppender);
impl Drop for Appender {
    fn drop(&mut self) {
        // SAFETY: closes and destroys the owned appender, including failed creation handles.
        unsafe {
            duckdb_appender_destroy(&raw mut self.0);
        }
    }
}
fn contains(message: &[u8], needle: &[u8]) -> bool {
    message.windows(needle.len()).any(|part| part == needle)
}
fn create_sql(
    catalog: Option<&[u8]>,
    schema: Option<&[u8]>,
    table: &[u8],
    types: &[rustdb_types::LogicalType],
    names: &[ByteString],
    options: &IngestOptions,
) -> ByteString {
    let quoted = rustdb_ast::keyword::write_optionally_quoted_bytes;
    let mut sql = ByteString::from(if matches!(options.mode, Mode::Replace) {
        "CREATE OR REPLACE "
    } else {
        "CREATE "
    });
    if options.temporary {
        sql.push_bytes(b"TEMP ");
    }
    sql.push_bytes(b"TABLE ");
    if matches!(options.mode, Mode::CreateAppend) {
        sql.push_bytes(b"IF NOT EXISTS ");
    }
    if !options.temporary {
        for part in [catalog, schema].into_iter().flatten() {
            sql.push_bytes(quoted(part));
            sql.push(b'.');
        }
    }
    sql.push_bytes(quoted(table));
    sql.push_bytes(b" (");
    for (i, (ty, name)) in types.iter().zip(names).enumerate() {
        if i > 0 {
            sql.push_bytes(b", ");
        }
        sql.push_bytes(quoted(name));
        sql.push(b' ');
        sql.push_bytes(ty.to_bytes());
    }
    sql.push_bytes(b");");
    sql
}
unsafe fn set_ingest_error(error: *mut AdbcError, message: &[u8], table: &[u8]) {
    // SAFETY: message and native table bytes are copied into the error before returning.
    unsafe {
        if contains(message, b"could not be found") || message.is_empty() {
            set_error(error, byte_format!("Table \"{table}\" does not exist"));
        } else {
            set_error(error, message);
        }
    }
}
pub(super) unsafe fn execute(
    connection: DuckdbConnection,
    options: &IngestOptions,
    input: *mut ArrowArrayStream,
    rows: *mut i64,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: the statement retains stream ownership, and all C API/Arrow resources have unique guards.
    unsafe {
        if connection.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing connection object");
        }
        if input.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing input arrow stream pointer");
        }
        let Some(table) = options.table.as_ref() else {
            return fail(error, INVALID_ARGUMENT, b"Missing database object name");
        };
        if options.temporary && options.schema.is_some() {
            return fail(
                error,
                INVALID_STATE,
                b"Temporary option is not supported with schema",
            );
        }
        if options.temporary && options.catalog.is_some() {
            return fail(
                error,
                INVALID_STATE,
                b"Temporary option is not supported with catalog",
            );
        }
        let Some(get_schema) = (*input).get_schema else {
            return fail(error, INVALID_ARGUMENT, b"Invalid input stream");
        };
        let Some(get_next) = (*input).get_next else {
            return fail(error, INVALID_ARGUMENT, b"Invalid input stream");
        };
        let mut catalog = options.catalog.clone();
        let mut schema = options.schema.clone();
        if options.temporary {
            catalog = Some("temp".into());
            schema = None;
        } else {
            if catalog.is_none() {
                let mut result = empty_result();
                let mut name = std::ptr::null_mut();
                if duckdb_query(connection, c"SELECT current_catalog()".as_ptr(), &raw mut result)
                    == DuckdbState::DuckDBSuccess
                {
                    name = duckdb_value_varchar(&raw mut result, 0, 0);
                }
                catalog = Some(
                    opt_bytes(name)
                        .filter(|v| !v.is_empty())
                        .unwrap_or(b"memory")
                        .into(),
                );
                duckdb_free(name.cast());
                duckdb_destroy_result(&raw mut result);
            }
            if schema.is_none() {
                schema = Some("main".into());
            }
        }
        let mut source_schema = ArrowSchemaWrapper::default();
        get_schema(input, &raw mut source_schema.arrow_schema);
        let mut types = Types(std::ptr::null_mut());
        let mut conversion =
            duckdb_schema_from_arrow(connection, &raw mut source_schema.arrow_schema, &raw mut types.0);
        if !conversion.is_null() {
            let code = fail(
                error,
                INTERNAL,
                opt_bytes(duckdb_error_data_message(conversion)).unwrap_or_default(),
            );
            errors::append(error, duckdb_error_data_error_type(conversion));
            duckdb_destroy_error_data(&raw mut conversion);
            return code;
        }
        let Some(converted) = types.0.as_ref() else {
            return fail(error, INTERNAL, b"Invalid converted schema");
        };
        if !matches!(options.mode, Mode::Append) {
            let sql = to_cstring(create_sql(
                catalog.as_deref(),
                schema.as_deref(),
                table,
                converted.types(),
                converted.names(),
                options,
            ));
            let mut result = empty_result();
            let outcome = duckdb_query(connection, sql.as_ptr(), &raw mut result);
            if outcome != DuckdbState::DuckDBSuccess {
                let message = opt_bytes(duckdb_result_error(&raw mut result)).unwrap_or_default();
                let code = if message == b"Interrupted!" {
                    CANCELLED
                } else if matches!(options.mode, Mode::Create) && contains(message, b"already exists") {
                    4
                } else {
                    INTERNAL
                };
                set_error(error, message);
                errors::append(error, duckdb_result_error_type(&raw mut result));
                duckdb_destroy_result(&raw mut result);
                return code;
            }
            duckdb_destroy_result(&raw mut result);
        }
        let catalog = catalog.as_ref().map(to_cstring);
        let schema = schema.as_ref().map(to_cstring);
        let table_name = to_cstring(table);
        let mut appender = Appender(std::ptr::null_mut());
        if duckdb_appender_create_ext(
            connection,
            catalog.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            schema.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            table_name.as_ptr(),
            &raw mut appender.0,
        ) != DuckdbState::DuckDBSuccess
        {
            let mut data = duckdb_appender_error_data(appender.0);
            set_ingest_error(
                error,
                opt_bytes(duckdb_error_data_message(data)).unwrap_or_default(),
                table,
            );
            errors::append(error, duckdb_error_data_error_type(data));
            duckdb_destroy_error_data(&raw mut data);
            return INTERNAL;
        }
        let mut affected = 0;
        loop {
            let mut array = ArrowArrayWrapper::default();
            get_next(input, &raw mut array.arrow_array);
            if array.arrow_array.release.is_none() {
                break;
            }
            let mut chunk = Chunk(std::ptr::null_mut());
            let mut conversion = duckdb_data_chunk_from_arrow(
                connection,
                &raw mut array.arrow_array,
                types.0,
                &raw mut chunk.0,
            );
            if !conversion.is_null() {
                set_error(
                    error,
                    opt_bytes(duckdb_error_data_message(conversion)).unwrap_or_default(),
                );
                errors::append(error, duckdb_error_data_error_type(conversion));
                duckdb_destroy_error_data(&raw mut conversion);
            }
            if let Some(chunk) = chunk.0.as_ref() {
                affected += chunk.len() as i64;
            }
            if duckdb_append_data_chunk(appender.0, chunk.0) != DuckdbState::DuckDBSuccess {
                let mut data = duckdb_appender_error_data(appender.0);
                let message = opt_bytes(duckdb_error_data_message(data)).unwrap_or_default();
                let code = if message == b"Interrupted!" {
                    CANCELLED
                } else {
                    INTERNAL
                };
                set_ingest_error(error, message, table);
                errors::append(error, duckdb_error_data_error_type(data));
                duckdb_destroy_error_data(&raw mut data);
                return code;
            }
        }
        if !rows.is_null() {
            *rows = affected;
        }
        OK
    }
}
