//! Metadata SQL is preserved from DuckDB 1.5.6's ADBC implementation, including filters and nested schemas.
use super::*;
use roastedduck_arrow::ffi::{ArrowArrayStreamWrapper, ArrowArrayWrapper};
pub(super) unsafe fn query_internal(
    connection: *mut AdbcConnection,
    out: *mut ArrowArrayStream,
    sql: &[u8],
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: the temporary statement is initialized and released on every exit; output stream owns its result.
    unsafe {
        let mut statement = AdbcStatement::default();
        let mut code = statement::new(connection, &raw mut statement, error);
        if code == OK {
            let sql = to_cstring(sql);
            code = statement::set_sql(&raw mut statement, sql.as_ptr(), error);
        }
        if code == OK {
            code = statement::execute(&raw mut statement, out, std::ptr::null_mut(), error);
        }
        statement::release(&raw mut statement, error);
        if code != OK {
            set_error(error, b"unable to initialize statement");
        }
        code
    }
}
pub(super) fn filter(input: Option<&[u8]>) -> ByteString {
    let mut out = ByteString::from("'");
    for byte in input.unwrap_or(b"%") {
        out.push(*byte);
        if *byte == b'\'' {
            out.push(*byte);
        }
    }
    out.push(b'\'');
    out
}
pub(super) fn format_template(template: &[u8], values: &[&[u8]]) -> Option<ByteString> {
    let mut out = ByteString::new();
    let mut remaining = template;
    for value in values {
        let position = remaining.windows(2).position(|part| part == b"%s")?;
        out.push_bytes(&remaining[..position]);
        out.push_bytes(value);
        remaining = &remaining[position + 2..];
    }
    out.push_bytes(remaining);
    Some(out)
}
pub(super) unsafe extern "C" fn get_objects(
    connection: *mut AdbcConnection,
    depth: c_int,
    catalog: *const c_char,
    schema: *const c_char,
    table: *const c_char,
    table_types: *mut *const c_char,
    column: *const c_char,
    out: *mut ArrowArrayStream,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: all filters are nullable C strings; table_types is a NULL-terminated pointer array.
    unsafe {
        let catalog = filter(opt_bytes(catalog));
        let schema = filter(opt_bytes(schema));
        let table = filter(opt_bytes(table));
        let column = filter(opt_bytes(column));
        let mut condition = ByteString::new();
        if !table_types.is_null() && !(*table_types).is_null() {
            condition.push_bytes(b" AND table_type IN (");
            let mut i = 0;
            while let Some(kind) = opt_bytes(*table_types.add(i)) {
                if ![b"LOCAL TABLE".as_slice(), b"BASE TABLE", b"VIEW"].contains(&kind) {
                    return fail(
                        error,
                        INVALID_ARGUMENT,
                        byte_format!(
                            "Table type must be \"LOCAL TABLE\", \"BASE TABLE\" or \"VIEW\": \"{kind}\""
                        ),
                    );
                }
                if i > 0 {
                    condition.push_bytes(b", ");
                }
                condition.push_bytes(filter(Some(kind)));
                i += 1;
            }
            condition.push(b')');
        }
        let sql = match depth {
            1 => format_template(include_bytes!("sql/objects_catalogs.sql"), &[&catalog]),
            2 => format_template(include_bytes!("sql/objects_db_schemas.sql"), &[&schema, &catalog]),
            3 => format_template(
                include_bytes!("sql/objects_tables.sql"),
                &[&table, &condition, &schema, &catalog],
            ),
            0 => format_template(
                include_bytes!("sql/objects_columns.sql"),
                &[&column, &column, &table, &condition, &schema, &catalog],
            ),
            _ => return fail(error, INVALID_ARGUMENT, b"Invalid value of Depth"),
        };
        let Some(sql) = sql else {
            return fail(error, INTERNAL, b"Inconsistent metadata SQL template");
        };
        query_internal(connection, out, &sql, error)
    }
}
pub(super) unsafe extern "C" fn get_table_types(
    connection: *mut AdbcConnection,
    out: *mut ArrowArrayStream,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: delegated query helper validates handles and transfers stream ownership.
    unsafe {
        query_internal(
            connection,
            out,
            b"SELECT DISTINCT table_type FROM information_schema.tables ORDER BY table_type",
            error,
        )
    }
}
pub(super) unsafe extern "C" fn get_table_schema(
    connection: *mut AdbcConnection,
    catalog: *const c_char,
    schema: *const c_char,
    table: *const c_char,
    out: *mut ArrowSchema,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: temporary stream owns its result and releases after schema conversion; output owns schema allocations.
    unsafe {
        if connection.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Connection is not set");
        }
        let schema = opt_bytes(schema).filter(|v| !v.is_empty()).unwrap_or(b"main");
        let Some(table) = opt_bytes(table).filter(|v| !v.is_empty()) else {
            return fail(
                error,
                INVALID_ARGUMENT,
                b"AdbcConnectionGetTableSchema: must provide table_name",
            );
        };
        let quoted = roastedduck_ast::keyword::write_optionally_quoted_bytes;
        let mut sql = ByteString::from("SELECT * FROM ");
        if let Some(catalog) = opt_bytes(catalog).filter(|v| !v.is_empty()) {
            sql.push_bytes(quoted(catalog));
            sql.push(b'.');
        }
        sql.push_bytes(quoted(schema));
        sql.push(b'.');
        sql.push_bytes(quoted(table));
        sql.push_bytes(b" LIMIT 0;");
        let mut stream = ArrowArrayStreamWrapper::default();
        let code = query_internal(connection, &raw mut stream.arrow_array_stream, &sql, error);
        if code != OK {
            return code;
        }
        if let Some(get_schema) = stream.arrow_array_stream.get_schema {
            get_schema(&raw mut stream.arrow_array_stream, out);
        }
        OK
    }
}
pub(super) unsafe fn current_option(
    connection: *mut AdbcConnection,
    catalog: bool,
    out: *mut c_char,
    length: *mut usize,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: the frozen query yields one VARCHAR column; Arrow wrappers keep all buffers live while copying.
    unsafe {
        let Some(state) = super::connection(connection) else {
            return fail(error, INVALID_ARGUMENT, b"Connection is not set");
        };
        if state.handle().is_null() {
            return fail(error, INVALID_STATE, b"Connection is not initialized");
        }
        let mut stream = ArrowArrayStreamWrapper::default();
        let code = query_internal(
            connection,
            &raw mut stream.arrow_array_stream,
            if catalog {
                b"SELECT current_database()"
            } else {
                b"SELECT current_schema()"
            },
            error,
        );
        if code != OK {
            return code;
        }
        let mut array = ArrowArrayWrapper::default();
        if let Some(get_next) = stream.arrow_array_stream.get_next {
            get_next(&raw mut stream.arrow_array_stream, &raw mut array.arrow_array);
        }
        let array = &array.arrow_array;
        if array.release.is_none() || array.length < 1 || array.n_children < 1 {
            return fail(
                error,
                INTERNAL,
                if catalog {
                    b"Failed to get current catalog".as_slice()
                } else {
                    b"Failed to get current schema"
                },
            );
        }
        let column = &**array.children;
        let offsets = (*column.buffers.add(1)).cast::<i32>();
        let data = (*column.buffers.add(2)).cast::<u8>();
        let bytes =
            std::slice::from_raw_parts(data.add(*offsets as usize), (*offsets.add(1) - *offsets) as usize);
        option_string(bytes, out, length, error)
    }
}
