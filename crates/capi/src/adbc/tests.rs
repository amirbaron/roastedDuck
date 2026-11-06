use super::*;
use std::ffi::CStr;

unsafe fn ok(code: u8, error: &mut AdbcError) {
    // SAFETY: message remains owned by error for this assertion.
    let message = unsafe { opt_bytes(error.message) }.unwrap_or_default();
    assert_eq!(code, OK, "{}", String::from_utf8_lossy(message));
}
unsafe fn query(statement: &mut AdbcStatement, sql: &[u8], error: &mut AdbcError) -> ArrowArrayStream {
    // SAFETY: the live handles and output are owned by the test.
    unsafe {
        let sql = to_cstring(sql);
        ok(statement::set_sql(statement, sql.as_ptr(), error), error);
        let mut stream = ArrowArrayStream::empty();
        let mut rows = 0;
        ok(
            statement::execute(statement, &raw mut stream, &raw mut rows, error),
            error,
        );
        stream
    }
}
unsafe fn values(stream: &mut ArrowArrayStream) -> Vec<i64> {
    let mut output = Vec::new();
    // SAFETY: the query produces a non-NULL BIGINT child; each exported batch is released once.
    unsafe {
        loop {
            let mut array = ArrowArray::empty();
            assert_eq!(stream.get_next.unwrap()(stream, &raw mut array), 0);
            if array.release.is_none() {
                break;
            }
            assert_eq!(array.n_children, 1);
            let column = &**array.children;
            let data = (*column.buffers.add(1)).cast::<i64>();
            output.extend_from_slice(std::slice::from_raw_parts(
                data.add(column.offset as usize),
                column.length as usize,
            ));
            array.release.unwrap()(&raw mut array);
        }
    }
    output
}
#[test]
fn streaming_results_survive_reuse_statement_and_connection_release() {
    // SAFETY: all handles are created/released by this test using their ADBC contracts.
    unsafe {
        let mut error = AdbcError::default();
        let mut db = AdbcDatabase::default();
        let mut conn = AdbcConnection::default();
        let mut stmt = AdbcStatement::default();
        ok(database_new(&raw mut db, &raw mut error), &mut error);
        ok(database_init(&raw mut db, &raw mut error), &mut error);
        ok(connection_new(&raw mut conn, &raw mut error), &mut error);
        ok(
            connection_init(&raw mut conn, &raw mut db, &raw mut error),
            &mut error,
        );
        ok(
            statement::new(&raw mut conn, &raw mut stmt, &raw mut error),
            &mut error,
        );
        let mut first = query(
            &mut stmt,
            b"SELECT i::BIGINT AS \"caf\xe9\" FROM range(5000) t(i)",
            &mut error,
        );
        let mut schema = ArrowSchema::empty();
        assert_eq!(first.get_schema.unwrap()(&raw mut first, &raw mut schema), 0);
        assert_eq!(CStr::from_ptr((**schema.children).name).to_bytes(), b"caf\xe9");
        schema.release.unwrap()(&raw mut schema);
        // Preparing a new query materializes only the unread portion of the previous stream.
        let mut batch = ArrowArray::empty();
        assert_eq!(first.get_next.unwrap()(&raw mut first, &raw mut batch), 0);
        assert!(batch.length > 0 && batch.length < 5000);
        let consumed = batch.length as usize;
        batch.release.unwrap()(&raw mut batch);
        let mut second = query(&mut stmt, b"SELECT 42::BIGINT", &mut error);
        assert_eq!(values(&mut first), (consumed as i64..5000).collect::<Vec<_>>());
        ok(statement::release(&raw mut stmt, &raw mut error), &mut error);
        ok(connection_release(&raw mut conn, &raw mut error), &mut error);
        ok(database_release(&raw mut db, &raw mut error), &mut error);
        assert_eq!(values(&mut second), vec![42]);
        first.release.unwrap()(&raw mut first);
        second.release.unwrap()(&raw mut second);
        assert!(first.release.is_none() && first.private_data.is_null());
        assert!(error.message.is_null());
    }
}
#[test]
fn multiple_statements_parameters_and_manual_transactions() {
    // SAFETY: all outputs and C strings are valid for their callback invocation.
    unsafe {
        let mut error = AdbcError::default();
        let mut db = AdbcDatabase::default();
        let mut conn = AdbcConnection::default();
        let mut stmt = AdbcStatement::default();
        ok(database_new(&raw mut db, &raw mut error), &mut error);
        ok(database_init(&raw mut db, &raw mut error), &mut error);
        ok(connection_new(&raw mut conn, &raw mut error), &mut error);
        ok(
            connection_set_option(
                &raw mut conn,
                c"adbc.connection.autocommit".as_ptr(),
                c"false".as_ptr(),
                &raw mut error,
            ),
            &mut error,
        );
        ok(
            connection_init(&raw mut conn, &raw mut db, &raw mut error),
            &mut error,
        );
        ok(
            statement::new(&raw mut conn, &raw mut stmt, &raw mut error),
            &mut error,
        );
        let mut stream = query(
            &mut stmt,
            b"CREATE TABLE t(i BIGINT); INSERT INTO t VALUES(7); SELECT * FROM t",
            &mut error,
        );
        assert_eq!(values(&mut stream), vec![7]);
        stream.release.unwrap()(&raw mut stream);
        ok(connection_commit(&raw mut conn, &raw mut error), &mut error);
        let mut stream = query(
            &mut stmt,
            b"INSERT INTO t VALUES(9);SELECT sum(i)::BIGINT FROM t",
            &mut error,
        );
        assert_eq!(values(&mut stream), vec![16]);
        stream.release.unwrap()(&raw mut stream);
        ok(connection_rollback(&raw mut conn, &raw mut error), &mut error);
        let mut stream = query(&mut stmt, b"SELECT sum(i)::BIGINT FROM t", &mut error);
        assert_eq!(values(&mut stream), vec![7]);
        stream.release.unwrap()(&raw mut stream);
        ok(
            statement::set_sql(
                &raw mut stmt,
                c"SELECT $1::INTEGER,$2::VARCHAR".as_ptr(),
                &raw mut error,
            ),
            &mut error,
        );
        let mut schema = ArrowSchema::empty();
        ok(
            statement::parameter_schema(&raw mut stmt, &raw mut schema, &raw mut error),
            &mut error,
        );
        assert_eq!(schema.n_children, 2);
        for i in 0..2 {
            let field = &**schema.children.add(i);
            assert_eq!(CStr::from_ptr(field.format).to_bytes(), b"n");
            assert_eq!(CStr::from_ptr(field.name).to_bytes(), i.to_string().as_bytes());
        }
        schema.release.unwrap()(&raw mut schema);
        ok(statement::release(&raw mut stmt, &raw mut error), &mut error);
        ok(connection_release(&raw mut conn, &raw mut error), &mut error);
        ok(database_release(&raw mut db, &raw mut error), &mut error);
    }
}

#[test]
fn legacy_error_prefix_and_option_buffer_contract() {
    #[repr(C)]
    struct LegacyError {
        message: *mut c_char,
        vendor_code: i32,
        sqlstate: [c_char; 5],
        release: Option<unsafe extern "C" fn(*mut AdbcError)>,
        canary: [u8; 16],
    }
    // SAFETY: lifecycle callbacks only access the stable 1.0 prefix; the canary detects tail writes.
    unsafe {
        let mut error = LegacyError {
            message: std::ptr::null_mut(),
            vendor_code: 17,
            sqlstate: (*b"ABCDE").map(|b| b as c_char),
            release: None,
            canary: [0xa5; 16],
        };
        let pointer = std::ptr::from_mut(&mut error).cast::<AdbcError>();
        assert_eq!(database_new(std::ptr::null_mut(), pointer), INVALID_ARGUMENT);
        assert_eq!(
            CStr::from_ptr(error.message).to_bytes(),
            b"Missing database object"
        );
        assert_eq!(error.canary, [0xa5; 16]);
        assert_eq!(error.vendor_code, 17);
        error.release.unwrap()(pointer);
        let mut db = AdbcDatabase::default();
        assert_eq!(database_new(&raw mut db, pointer), OK);
        assert_eq!(
            database_set_option(&raw mut db, c"path".as_ptr(), c"snow-雪.db".as_ptr(), pointer),
            OK
        );
        let mut length = 0;
        assert_eq!(
            database_get_option(
                &raw mut db,
                c"path".as_ptr(),
                std::ptr::null_mut(),
                &raw mut length,
                pointer
            ),
            OK
        );
        assert_eq!(length, c"snow-雪.db".to_bytes_with_nul().len());
        let mut short = [0x55 as c_char; 2];
        let mut capacity = short.len();
        assert_eq!(
            database_get_option(
                &raw mut db,
                c"path".as_ptr(),
                short.as_mut_ptr(),
                &raw mut capacity,
                pointer
            ),
            OK
        );
        assert_eq!(capacity, length);
        assert_eq!(short, [0x55; 2]);
        let mut buffer = vec![0 as c_char; length];
        assert_eq!(
            database_get_option(
                &raw mut db,
                c"path".as_ptr(),
                buffer.as_mut_ptr(),
                &raw mut length,
                pointer
            ),
            OK
        );
        assert_eq!(
            CStr::from_ptr(buffer.as_ptr()).to_bytes(),
            c"snow-雪.db".to_bytes()
        );
        assert_eq!(database_release(&raw mut db, pointer), OK);
        assert_eq!(error.canary, [0xa5; 16]);
        assert!(error.message.is_null());
    }
}
#[test]
fn result_schema_without_execution_and_invalid_query_errors() {
    // SAFETY: all handles and Arrow outputs are test-owned, and each release is invoked once.
    unsafe {
        let mut error = AdbcError::default();
        let mut db = AdbcDatabase::default();
        let mut conn = AdbcConnection::default();
        let mut stmt = AdbcStatement::default();
        ok(database_new(&raw mut db, &raw mut error), &mut error);
        ok(database_init(&raw mut db, &raw mut error), &mut error);
        ok(connection_new(&raw mut conn, &raw mut error), &mut error);
        ok(
            connection_init(&raw mut conn, &raw mut db, &raw mut error),
            &mut error,
        );
        ok(
            statement::new(&raw mut conn, &raw mut stmt, &raw mut error),
            &mut error,
        );
        let mut schema = ArrowSchema::empty();
        assert_eq!(
            statement::execute_schema(&raw mut stmt, &raw mut schema, &raw mut error),
            INVALID_STATE
        );
        release_error(&raw mut error);
        ok(
            statement::set_sql(
                &raw mut stmt,
                c"SELECT 1::BIGINT AS answer,'x' AS text".as_ptr(),
                &raw mut error,
            ),
            &mut error,
        );
        ok(
            statement::execute_schema(&raw mut stmt, &raw mut schema, &raw mut error),
            &mut error,
        );
        assert_eq!(schema.n_children, 2);
        assert_eq!(CStr::from_ptr((**schema.children).format).to_bytes(), b"l");
        assert_eq!(CStr::from_ptr((**schema.children).name).to_bytes(), b"answer");
        schema.release.unwrap()(&raw mut schema);
        assert_eq!(
            statement::set_sql(&raw mut stmt, c"SELECT FROM".as_ptr(), &raw mut error),
            INTERNAL
        );
        assert!(!error.message.is_null());
        release_error(&raw mut error);
        ok(
            statement::set_sql(
                &raw mut stmt,
                c"SELECT error('interrupt')".as_ptr(),
                &raw mut error,
            ),
            &mut error,
        );
        let mut output = ArrowArrayStream::empty();
        assert_eq!(
            statement::execute(
                &raw mut stmt,
                &raw mut output,
                std::ptr::null_mut(),
                &raw mut error
            ),
            INVALID_ARGUMENT
        );
        assert!(
            opt_bytes(error.message)
                .unwrap()
                .windows(9)
                .any(|part| part == b"interrupt")
        );
        release_error(&raw mut error);
        ok(statement::release(&raw mut stmt, &raw mut error), &mut error);
        ok(connection_release(&raw mut conn, &raw mut error), &mut error);
        ok(database_release(&raw mut db, &raw mut error), &mut error);
    }
}

#[test]
fn bound_arrow_batch_ownership_parameters_and_row_limit() {
    // SAFETY: producer and parameter statements own their handles and move Arrow inputs once.
    unsafe {
        let mut error = AdbcError::default();
        let mut db = AdbcDatabase::default();
        let mut conn = AdbcConnection::default();
        let mut producer = AdbcStatement::default();
        let mut target = AdbcStatement::default();
        ok(database_new(&raw mut db, &raw mut error), &mut error);
        ok(database_init(&raw mut db, &raw mut error), &mut error);
        ok(connection_new(&raw mut conn, &raw mut error), &mut error);
        ok(
            connection_init(&raw mut conn, &raw mut db, &raw mut error),
            &mut error,
        );
        ok(
            statement::new(&raw mut conn, &raw mut producer, &raw mut error),
            &mut error,
        );
        ok(
            statement::new(&raw mut conn, &raw mut target, &raw mut error),
            &mut error,
        );
        let mut source = query(&mut producer, b"SELECT 7::BIGINT AS parameter", &mut error);
        let mut schema = ArrowSchema::empty();
        let mut array = ArrowArray::empty();
        assert_eq!(source.get_schema.unwrap()(&raw mut source, &raw mut schema), 0);
        assert_eq!(source.get_next.unwrap()(&raw mut source, &raw mut array), 0);
        source.release.unwrap()(&raw mut source);
        ok(
            statement::set_sql(
                &raw mut target,
                c"SELECT $1::BIGINT + 35".as_ptr(),
                &raw mut error,
            ),
            &mut error,
        );
        ok(
            statement::bind(&raw mut target, &raw mut array, &raw mut schema, &raw mut error),
            &mut error,
        );
        assert!(array.release.is_none() && schema.release.is_none());
        // Schema output is independently owned, and remains valid after the bound stream is destroyed.
        let mut copied = ArrowSchema::empty();
        let input = &raw mut statement(&raw mut target).unwrap().input;
        assert_eq!((*input).get_schema.unwrap()(input, &raw mut copied), 0);
        let mut output = ArrowArrayStream::empty();
        ok(
            statement::execute(
                &raw mut target,
                &raw mut output,
                std::ptr::null_mut(),
                &raw mut error,
            ),
            &mut error,
        );
        assert_eq!(values(&mut output), vec![42]);
        output.release.unwrap()(&raw mut output);
        ok(
            statement::set_sql(&raw mut target, c"SELECT $1::BIGINT".as_ptr(), &raw mut error),
            &mut error,
        );
        assert_eq!(CStr::from_ptr((**copied.children).name).to_bytes(), b"parameter");
        copied.release.unwrap()(&raw mut copied);
        let mut source = query(&mut producer, b"SELECT i::BIGINT FROM range(2) t(i)", &mut error);
        ok(
            statement::bind_stream(&raw mut target, &raw mut source, &raw mut error),
            &mut error,
        );
        assert!(source.release.is_none());
        assert_eq!(
            statement::execute(
                &raw mut target,
                &raw mut output,
                std::ptr::null_mut(),
                &raw mut error
            ),
            NOT_IMPLEMENTED
        );
        assert_eq!(
            CStr::from_ptr(error.message).to_bytes(),
            b"Binding multiple rows at once is not supported yet"
        );
        release_error(&raw mut error);
        ok(statement::release(&raw mut target, &raw mut error), &mut error);
        ok(statement::release(&raw mut producer, &raw mut error), &mut error);
        ok(connection_release(&raw mut conn, &raw mut error), &mut error);
        ok(database_release(&raw mut db, &raw mut error), &mut error);
    }
}

#[test]
fn bound_schema_copy_preserves_nested_dictionary_and_binary_metadata() {
    unsafe extern "C" fn release_schema(value: *mut ArrowSchema) {
        // SAFETY: this fixture borrows stack/static data, so release only clears the callback.
        unsafe {
            (*value).release = None;
        }
    }
    unsafe extern "C" fn release_array(value: *mut ArrowArray) {
        // SAFETY: this empty fixture owns no buffers.
        unsafe {
            (*value).release = None;
        }
    }
    // SAFETY: all borrowed fixture data remains live until the input stream is released.
    unsafe {
        let mut dictionary = ArrowSchema::empty();
        dictionary.format = c"u".as_ptr();
        dictionary.release = Some(release_schema);
        let mut child = ArrowSchema::empty();
        child.format = c"i".as_ptr();
        child.name = c"native-雪".as_ptr();
        child.flags = 3;
        child.dictionary = &raw mut dictionary;
        child.release = Some(release_schema);
        let mut children = [&raw mut child];
        let mut metadata = Vec::new();
        metadata.extend(1i32.to_ne_bytes());
        metadata.extend(3i32.to_ne_bytes());
        metadata.extend(b"key");
        metadata.extend(3i32.to_ne_bytes());
        metadata.extend(b"a\0b");
        let mut schema = ArrowSchema::empty();
        schema.format = c"+s".as_ptr();
        schema.name = c"root".as_ptr();
        schema.metadata = metadata.as_ptr().cast();
        schema.n_children = 1;
        schema.children = children.as_mut_ptr();
        schema.release = Some(release_schema);
        let mut array = ArrowArray::empty();
        array.release = Some(release_array);
        let mut error = AdbcError::default();
        let mut stream = ArrowArrayStream::empty();
        ok(
            batch::from_batch(&raw mut array, &raw mut schema, &raw mut stream, &raw mut error),
            &mut error,
        );
        let mut copied = ArrowSchema::empty();
        assert_eq!(stream.get_schema.unwrap()(&raw mut stream, &raw mut copied), 0);
        stream.release.unwrap()(&raw mut stream);
        assert_eq!(CStr::from_ptr(copied.name).to_bytes(), b"root");
        assert_eq!(
            std::slice::from_raw_parts(copied.metadata.cast::<u8>(), metadata.len()),
            metadata
        );
        let field = &**copied.children;
        assert_eq!(field.flags, 3);
        assert_eq!(CStr::from_ptr(field.name).to_bytes(), c"native-雪".to_bytes());
        assert_eq!(CStr::from_ptr((*field.dictionary).format).to_bytes(), b"u");
        copied.release.unwrap()(&raw mut copied);
        assert!(copied.release.is_none());
    }
}

unsafe fn ingest_batch(
    producer: &mut AdbcStatement,
    target: &mut AdbcStatement,
    sql: &[u8],
    error: &mut AdbcError,
) -> (u8, i64) {
    // SAFETY: a produced stream transfers ownership to the target statement.
    unsafe {
        let mut source = query(producer, sql, error);
        ok(statement::bind_stream(target, &raw mut source, error), error);
        let mut rows = -99;
        let code = statement::execute(target, std::ptr::null_mut(), &raw mut rows, error);
        (code, rows)
    }
}
#[test]
fn ingestion_modes_native_names_and_temporary_shadowing() {
    // SAFETY: real engine handles and bound streams are owned and released by this test.
    unsafe {
        let mut error = AdbcError::default();
        let mut db = AdbcDatabase::default();
        let mut conn = AdbcConnection::default();
        let mut producer = AdbcStatement::default();
        let mut target = AdbcStatement::default();
        ok(database_new(&raw mut db, &raw mut error), &mut error);
        ok(database_init(&raw mut db, &raw mut error), &mut error);
        ok(connection_new(&raw mut conn, &raw mut error), &mut error);
        ok(
            connection_init(&raw mut conn, &raw mut db, &raw mut error),
            &mut error,
        );
        ok(
            statement::new(&raw mut conn, &raw mut producer, &raw mut error),
            &mut error,
        );
        ok(
            statement::new(&raw mut conn, &raw mut target, &raw mut error),
            &mut error,
        );
        let table = to_cstring(b"table-\xe9");
        ok(
            options::statement_set(
                &raw mut target,
                c"adbc.ingest.target_table".as_ptr(),
                table.as_ptr(),
                &raw mut error,
            ),
            &mut error,
        );
        let (code, rows) = ingest_batch(
            &mut producer,
            &mut target,
            b"SELECT 7::BIGINT AS \"caf\xe9\"",
            &mut error,
        );
        ok(code, &mut error);
        assert_eq!(rows, 1);
        assert_eq!(
            ingest_batch(
                &mut producer,
                &mut target,
                b"SELECT 8::BIGINT AS \"caf\xe9\"",
                &mut error
            )
            .0,
            4
        );
        release_error(&raw mut error);
        for (mode, value, expected) in [
            (c"adbc.ingest.mode.append", 9, 16),
            (c"adbc.ingest.mode.replace", 11, 11),
            (c"adbc.ingest.mode.create_append", 13, 24),
        ] {
            ok(
                options::statement_set(
                    &raw mut target,
                    c"adbc.ingest.mode".as_ptr(),
                    mode.as_ptr(),
                    &raw mut error,
                ),
                &mut error,
            );
            let name = b"caf\xe9".as_slice();
            let sql = byte_format!("SELECT {value}::BIGINT AS \"{name}\"");
            let (code, rows) = ingest_batch(&mut producer, &mut target, &sql, &mut error);
            ok(code, &mut error);
            assert_eq!(rows, 1);
            let mut result = query(
                &mut producer,
                b"SELECT sum(\"caf\xe9\")::BIGINT FROM memory.main.\"table-\xe9\"",
                &mut error,
            );
            assert_eq!(values(&mut result), vec![expected]);
            result.release.unwrap()(&raw mut result);
        }
        ok(
            options::statement_set(
                &raw mut target,
                c"adbc.ingest.target_db_schema".as_ptr(),
                c"main".as_ptr(),
                &raw mut error,
            ),
            &mut error,
        );
        ok(
            options::statement_set_int(
                &raw mut target,
                c"adbc.ingest.temporary".as_ptr(),
                1,
                &raw mut error,
            ),
            &mut error,
        );
        let mut temporary = 0;
        ok(
            options::statement_get_int(
                &raw mut target,
                c"adbc.ingest.temporary".as_ptr(),
                &raw mut temporary,
                &raw mut error,
            ),
            &mut error,
        );
        assert_eq!(temporary, 1);
        assert!(statement(&raw mut target).unwrap().ingest.schema.is_none());
        ok(
            ingest_batch(
                &mut producer,
                &mut target,
                b"SELECT 3::BIGINT AS \"caf\xe9\"",
                &mut error,
            )
            .0,
            &mut error,
        );
        let mut result = query(
            &mut producer,
            b"SELECT sum(\"caf\xe9\")::BIGINT FROM temp.main.\"table-\xe9\"",
            &mut error,
        );
        assert_eq!(values(&mut result), vec![3]);
        result.release.unwrap()(&raw mut result);
        ok(
            options::statement_set_int(
                &raw mut target,
                c"adbc.ingest.temporary".as_ptr(),
                0,
                &raw mut error,
            ),
            &mut error,
        );
        ok(
            ingest_batch(
                &mut producer,
                &mut target,
                b"SELECT 5::BIGINT AS \"caf\xe9\"",
                &mut error,
            )
            .0,
            &mut error,
        );
        let mut result = query(
            &mut producer,
            b"SELECT sum(\"caf\xe9\")::BIGINT FROM memory.main.\"table-\xe9\"",
            &mut error,
        );
        assert_eq!(values(&mut result), vec![29]);
        result.release.unwrap()(&raw mut result);
        ok(
            options::statement_set(
                &raw mut target,
                c"adbc.ingest.target_table".as_ptr(),
                c"missing".as_ptr(),
                &raw mut error,
            ),
            &mut error,
        );
        ok(
            options::statement_set(
                &raw mut target,
                c"adbc.ingest.mode".as_ptr(),
                c"adbc.ingest.mode.append".as_ptr(),
                &raw mut error,
            ),
            &mut error,
        );
        assert_eq!(
            ingest_batch(&mut producer, &mut target, b"SELECT 1::BIGINT", &mut error).0,
            INTERNAL
        );
        assert_eq!(
            CStr::from_ptr(error.message).to_bytes(),
            b"Table \"missing\" does not exist"
        );
        release_error(&raw mut error);
        ok(statement::release(&raw mut target, &raw mut error), &mut error);
        ok(statement::release(&raw mut producer, &raw mut error), &mut error);
        ok(connection_release(&raw mut conn, &raw mut error), &mut error);
        ok(database_release(&raw mut db, &raw mut error), &mut error);
    }
}

#[test]
fn typed_option_errors_and_statement_option_state() {
    // SAFETY: valid pre-init handles are sufficient for these option operations; outputs remain test-owned.
    unsafe {
        let mut error = AdbcError::default();
        let mut db = AdbcDatabase::default();
        let mut conn = AdbcConnection::default();
        let mut stmt = AdbcStatement::default();
        ok(database_new(&raw mut db, &raw mut error), &mut error);
        for (number, expected) in [
            (1.5, b"1.500000".as_slice()),
            (f64::NAN, b"nan"),
            (f64::from_bits(f64::NAN.to_bits() | (1 << 63)), b"nan"),
            (f64::INFINITY, b"inf"),
            (-0.0, b"-0.000000"),
        ] {
            ok(
                database_set_double(&raw mut db, c"custom_user_agent".as_ptr(), number, &raw mut error),
                &mut error,
            );
            let mut buffer = [0 as c_char; 64];
            let mut length = buffer.len();
            ok(
                database_get_option(
                    &raw mut db,
                    c"custom_user_agent".as_ptr(),
                    buffer.as_mut_ptr(),
                    &raw mut length,
                    &raw mut error,
                ),
                &mut error,
            );
            assert_eq!(CStr::from_ptr(buffer.as_ptr()).to_bytes(), expected);
        }
        ok(connection_new(&raw mut conn, &raw mut error), &mut error);
        ok(
            statement::new(&raw mut conn, &raw mut stmt, &raw mut error),
            &mut error,
        );
        let mut untouched = 19;
        assert_eq!(
            options::database_get_int(
                &raw mut db,
                c"threads".as_ptr(),
                &raw mut untouched,
                &raw mut error
            ),
            NOT_FOUND
        );
        assert_eq!(untouched, 19);
        assert_eq!(
            CStr::from_ptr(error.message).to_bytes(),
            b"Option not found: threads"
        );
        release_error(&raw mut error);
        let mut length = 0;
        assert_eq!(
            options::statement_get(
                &raw mut stmt,
                c"adbc.ingest.target_table".as_ptr(),
                std::ptr::null_mut(),
                &raw mut length,
                &raw mut error
            ),
            NOT_FOUND
        );
        assert_eq!(
            CStr::from_ptr(error.message).to_bytes(),
            b"Option not set: adbc.ingest.target_table"
        );
        release_error(&raw mut error);
        ok(
            options::statement_set(
                &raw mut stmt,
                c"adbc.ingest.target_catalog".as_ptr(),
                c"memory".as_ptr(),
                &raw mut error,
            ),
            &mut error,
        );
        ok(
            options::statement_set(
                &raw mut stmt,
                c"adbc.ingest.target_db_schema".as_ptr(),
                c"main".as_ptr(),
                &raw mut error,
            ),
            &mut error,
        );
        ok(
            options::statement_set_int(
                &raw mut stmt,
                c"adbc.ingest.temporary".as_ptr(),
                -7,
                &raw mut error,
            ),
            &mut error,
        );
        assert_eq!(
            options::statement_get(
                &raw mut stmt,
                c"adbc.ingest.target_catalog".as_ptr(),
                std::ptr::null_mut(),
                &raw mut length,
                &raw mut error
            ),
            NOT_FOUND
        );
        release_error(&raw mut error);
        let key = to_cstring(b"unknown-\xff");
        assert_eq!(
            options::statement_set(&raw mut stmt, key.as_ptr(), c"value".as_ptr(), &raw mut error),
            INVALID_ARGUMENT
        );
        assert_eq!(
            CStr::from_ptr(error.message).to_bytes(),
            b"Statement Set Option unknown-\xff is not yet accepted by DuckDB"
        );
        release_error(&raw mut error);
        assert_eq!(
            options::statement_set(
                &raw mut stmt,
                c"adbc.ingest.mode".as_ptr(),
                c"wrong".as_ptr(),
                &raw mut error
            ),
            INVALID_ARGUMENT
        );
        assert_eq!(
            CStr::from_ptr(error.message).to_bytes(),
            b"Invalid ingestion mode"
        );
        release_error(&raw mut error);
        assert_eq!(
            options::connection_set_double(std::ptr::null_mut(), std::ptr::null(), 1.0, &raw mut error),
            NOT_IMPLEMENTED
        );
        assert_eq!(
            CStr::from_ptr(error.message).to_bytes(),
            b"SetOptionDouble is not supported for connection"
        );
        release_error(&raw mut error);
        assert_eq!(
            options::read_partition(
                std::ptr::null_mut(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                &raw mut error
            ),
            NOT_IMPLEMENTED
        );
        assert_eq!(
            CStr::from_ptr(error.message).to_bytes(),
            b"Read Partitions are not supported in DuckDB"
        );
        release_error(&raw mut error);
        ok(statement::release(&raw mut stmt, &raw mut error), &mut error);
        ok(connection_release(&raw mut conn, &raw mut error), &mut error);
        ok(database_release(&raw mut db, &raw mut error), &mut error);
    }
}

unsafe fn row_count(stream: &mut ArrowArrayStream) -> i64 {
    // SAFETY: each output batch transfers ownership to this helper and is released exactly once.
    unsafe {
        let mut count = 0;
        loop {
            let mut array = ArrowArray::empty();
            assert_eq!(stream.get_next.unwrap()(stream, &raw mut array), 0);
            if array.release.is_none() {
                break;
            }
            count += array.length;
            array.release.unwrap()(&raw mut array);
        }
        count
    }
}
#[test]
fn metadata_depths_schema_types_and_option_query_path() {
    // SAFETY: test owns every connection, stream and schema until its matching release.
    unsafe {
        let mut error = AdbcError::default();
        let mut db = AdbcDatabase::default();
        let mut conn = AdbcConnection::default();
        let mut stmt = AdbcStatement::default();
        ok(database_new(&raw mut db, &raw mut error), &mut error);
        ok(database_init(&raw mut db, &raw mut error), &mut error);
        ok(connection_new(&raw mut conn, &raw mut error), &mut error);
        ok(
            connection_init(&raw mut conn, &raw mut db, &raw mut error),
            &mut error,
        );
        ok(
            statement::new(&raw mut conn, &raw mut stmt, &raw mut error),
            &mut error,
        );
        let mut result=query(&mut stmt,b"CREATE SCHEMA s;CREATE TABLE s.t(i INTEGER PRIMARY KEY,txt VARCHAR DEFAULT 'a');CREATE VIEW s.v AS SELECT * FROM s.t",&mut error);
        result.release.unwrap()(&raw mut result);
        for depth in 0..=3 {
            let mut stream = ArrowArrayStream::empty();
            ok(
                metadata::get_objects(
                    &raw mut conn,
                    depth,
                    c"memory".as_ptr(),
                    c"s".as_ptr(),
                    c"t".as_ptr(),
                    std::ptr::null_mut(),
                    c"i".as_ptr(),
                    &raw mut stream,
                    &raw mut error,
                ),
                &mut error,
            );
            let mut schema = ArrowSchema::empty();
            assert_eq!(stream.get_schema.unwrap()(&raw mut stream, &raw mut schema), 0);
            assert_eq!(schema.n_children, 2);
            assert_eq!(
                CStr::from_ptr((**schema.children).name).to_bytes(),
                b"catalog_name"
            );
            schema.release.unwrap()(&raw mut schema);
            assert_eq!(row_count(&mut stream), 1);
            stream.release.unwrap()(&raw mut stream);
        }
        let mut schema = ArrowSchema::empty();
        ok(
            metadata::get_table_schema(
                &raw mut conn,
                std::ptr::null(),
                c"s".as_ptr(),
                c"t".as_ptr(),
                &raw mut schema,
                &raw mut error,
            ),
            &mut error,
        );
        assert_eq!(schema.n_children, 2);
        assert_eq!(CStr::from_ptr((**schema.children).name).to_bytes(), b"i");
        schema.release.unwrap()(&raw mut schema);
        let mut types = ArrowArrayStream::empty();
        ok(
            metadata::get_table_types(&raw mut conn, &raw mut types, &raw mut error),
            &mut error,
        );
        assert!(row_count(&mut types) >= 2);
        types.release.unwrap()(&raw mut types);
        ok(
            connection_set_option(
                &raw mut conn,
                c"adbc.connection.db_schema".as_ptr(),
                c"s".as_ptr(),
                &raw mut error,
            ),
            &mut error,
        );
        let mut buffer = [0 as c_char; 64];
        let mut length = buffer.len();
        ok(
            connection_get_option(
                &raw mut conn,
                c"adbc.connection.db_schema".as_ptr(),
                buffer.as_mut_ptr(),
                &raw mut length,
                &raw mut error,
            ),
            &mut error,
        );
        assert_eq!(CStr::from_ptr(buffer.as_ptr()).to_bytes(), b"s");
        assert_eq!(
            metadata::get_objects(
                &raw mut conn,
                9,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                &raw mut types,
                &raw mut error
            ),
            INVALID_ARGUMENT
        );
        release_error(&raw mut error);
        ok(statement::release(&raw mut stmt, &raw mut error), &mut error);
        ok(connection_release(&raw mut conn, &raw mut error), &mut error);
        ok(database_release(&raw mut db, &raw mut error), &mut error);
    }
}
#[test]
fn rich_error_details_and_stream_error_origin() {
    // SAFETY: the vendor sentinel opts this full error into the 1.1 tail; all detail borrows end before release.
    unsafe {
        let mut error = AdbcError {
            vendor_code: i32::MIN,
            ..AdbcError::default()
        };
        let mut db = AdbcDatabase::default();
        let mut conn = AdbcConnection::default();
        let mut stmt = AdbcStatement::default();
        ok(database_new(&raw mut db, &raw mut error), &mut error);
        ok(database_init(&raw mut db, &raw mut error), &mut error);
        ok(connection_new(&raw mut conn, &raw mut error), &mut error);
        ok(
            connection_init(&raw mut conn, &raw mut db, &raw mut error),
            &mut error,
        );
        ok(
            statement::new(&raw mut conn, &raw mut stmt, &raw mut error),
            &mut error,
        );
        ok(
            statement::set_sql(
                &raw mut stmt,
                c"SELECT error('rich failure')".as_ptr(),
                &raw mut error,
            ),
            &mut error,
        );
        let mut stream = ArrowArrayStream::empty();
        assert_eq!(
            statement::execute(
                &raw mut stmt,
                &raw mut stream,
                std::ptr::null_mut(),
                &raw mut error
            ),
            INVALID_ARGUMENT
        );
        assert_eq!(errors::count(&raw const error), 1);
        let detail = errors::detail(&raw const error, 0);
        assert_eq!(CStr::from_ptr(detail.key).to_bytes(), b"duckdb:error_type");
        assert_eq!(
            std::slice::from_raw_parts(detail.value, detail.value_length),
            b"InvalidInput"
        );
        assert!(errors::detail(&raw const error, -1).key.is_null());
        error.release.unwrap()(&raw mut error);
        assert!(error.message.is_null() && error.private_data.is_null());
        // An invalid stream must not be cast to this driver's private state.
        assert!(stream::error_from_stream(&raw mut stream, std::ptr::null_mut()).is_null());
        let mut result=query(&mut stmt,b"SELECT i::BIGINT FROM range(1000000) t(i) WHERE CASE WHEN i<900000 THEN true ELSE error('late failure '||i::VARCHAR) END",&mut error);
        let mut initial = ArrowArray::empty();
        assert_eq!(result.get_next.unwrap()(&raw mut result, &raw mut initial), 0);
        assert!(initial.length > 0);
        initial.release.unwrap()(&raw mut initial);
        let mut second = query(&mut stmt, b"SELECT 42::BIGINT", &mut error);
        let mut consumed = 0;
        loop {
            let mut array = ArrowArray::empty();
            let code = result.get_next.unwrap()(&raw mut result, &raw mut array);
            if code != 0 {
                assert!(array.release.is_none());
                break;
            }
            assert!(array.release.is_some(), "expected deferred error");
            consumed += array.length;
            array.release.unwrap()(&raw mut array);
        }
        assert!(consumed > 0);
        let mut status = 0;
        let detail_error = stream::error_from_stream(&raw mut result, &raw mut status);
        assert!(!detail_error.is_null());
        assert_eq!(status, INTERNAL);
        assert_eq!(errors::count(detail_error), 1);
        let detail = errors::detail(detail_error, 0);
        assert_eq!(
            std::slice::from_raw_parts(detail.value, detail.value_length),
            b"InvalidInput"
        );
        assert!(
            CStr::from_ptr((*detail_error).message)
                .to_bytes()
                .windows(12)
                .any(|s| s == b"late failure")
        );
        result.release.unwrap()(&raw mut result);
        assert_eq!(values(&mut second), vec![42]);
        second.release.unwrap()(&raw mut second);
        ok(statement::release(&raw mut stmt, &raw mut error), &mut error);
        ok(connection_release(&raw mut conn, &raw mut error), &mut error);
        ok(database_release(&raw mut db, &raw mut error), &mut error);
    }
}

#[test]
fn info_dense_union_statistics_and_empty_statistic_names() {
    // SAFETY: schema and array offsets/children are inspected according to the driver's fixed metadata layouts.
    unsafe {
        let mut error = AdbcError::default();
        let mut db = AdbcDatabase::default();
        let mut conn = AdbcConnection::default();
        let mut stmt = AdbcStatement::default();
        ok(database_new(&raw mut db, &raw mut error), &mut error);
        ok(database_init(&raw mut db, &raw mut error), &mut error);
        ok(connection_new(&raw mut conn, &raw mut error), &mut error);
        ok(
            connection_init(&raw mut conn, &raw mut db, &raw mut error),
            &mut error,
        );
        ok(
            statement::new(&raw mut conn, &raw mut stmt, &raw mut error),
            &mut error,
        );
        let codes = [103, 999, 0, 103];
        let mut stream = ArrowArrayStream::empty();
        ok(
            info::get_info(
                &raw mut conn,
                codes.as_ptr(),
                codes.len(),
                &raw mut stream,
                &raw mut error,
            ),
            &mut error,
        );
        let mut schema = ArrowSchema::empty();
        assert_eq!(stream.get_schema.unwrap()(&raw mut stream, &raw mut schema), 0);
        assert_eq!(
            CStr::from_ptr((**schema.children.add(1)).format).to_bytes(),
            b"+ud:0,1,2,3,4,5"
        );
        assert_eq!((**schema.children).flags, 0);
        schema.release.unwrap()(&raw mut schema);
        let mut array = ArrowArray::empty();
        assert_eq!(stream.get_next.unwrap()(&raw mut stream, &raw mut array), 0);
        assert_eq!(array.length, 3);
        let names = &**array.children;
        assert_eq!(
            std::slice::from_raw_parts((*names.buffers.add(1)).cast::<u32>(), 3),
            [103, 0, 103]
        );
        let union = &**array.children.add(1);
        assert_eq!(
            std::slice::from_raw_parts((*union.buffers).cast::<u8>(), 3),
            [2, 0, 2]
        );
        assert_eq!(
            std::slice::from_raw_parts((*union.buffers.add(1)).cast::<i32>(), 3),
            [0, 0, 1]
        );
        let numbers = &**union.children.add(2);
        assert_eq!(
            std::slice::from_raw_parts((*numbers.buffers.add(1)).cast::<i64>(), 2),
            [1_001_000, 1_001_000]
        );
        array.release.unwrap()(&raw mut array);
        stream.release.unwrap()(&raw mut stream);
        let mut result = query(
            &mut stmt,
            b"CREATE TABLE stats_t(i INTEGER);INSERT INTO stats_t VALUES(1),(2),(3)",
            &mut error,
        );
        result.release.unwrap()(&raw mut result);
        ok(
            statistics::get_statistics(
                &raw mut conn,
                c"memory".as_ptr(),
                c"main".as_ptr(),
                c"stats_t".as_ptr(),
                0,
                &raw mut stream,
                &raw mut error,
            ),
            &mut error,
        );
        let mut array = ArrowArray::empty();
        assert_eq!(stream.get_next.unwrap()(&raw mut stream, &raw mut array), 0);
        assert_eq!(array.length, 1);
        let schemas = &**(**array.children.add(1)).children;
        let statistics = &**(**schemas.children.add(1)).children;
        assert_eq!(statistics.length, 1);
        let key = &**statistics.children.add(2);
        assert_eq!(*(*key.buffers.add(1)).cast::<i16>(), 6);
        let union = &**statistics.children.add(3);
        assert_eq!(*(*union.buffers).cast::<u8>(), 2);
        let counts = &**union.children.add(2);
        assert_eq!(*(*counts.buffers.add(1)).cast::<f64>(), 3.0);
        let null_column = &**statistics.children.add(1);
        assert_eq!(null_column.null_count, 1);
        array.release.unwrap()(&raw mut array);
        stream.release.unwrap()(&raw mut stream);
        ok(
            statistics::get_names(&raw mut conn, &raw mut stream, &raw mut error),
            &mut error,
        );
        assert_eq!(row_count(&mut stream), 0);
        stream.release.unwrap()(&raw mut stream);
        ok(statement::release(&raw mut stmt, &raw mut error), &mut error);
        ok(connection_release(&raw mut conn, &raw mut error), &mut error);
        ok(database_release(&raw mut db, &raw mut error), &mut error);
    }
}

#[test]
fn initializer_respects_version_prefix_and_untouched_slots() {
    // SAFETY: aligned backing storage covers the full layout; tail words act as a 1.0 overwrite canary.
    unsafe {
        let mut storage = [0xa5a5_a5a5_a5a5_a5a5usize; 64];
        let driver = storage.as_mut_ptr().cast::<AdbcDriver>();
        let before = storage;
        assert_eq!(
            duckdb_adbc_init(42, driver.cast(), std::ptr::null_mut()),
            NOT_IMPLEMENTED
        );
        assert_eq!(storage, before);
        assert_eq!(
            duckdb_adbc_init(1_001_000, std::ptr::null_mut(), std::ptr::null_mut()),
            INVALID_ARGUMENT
        );
        assert_eq!(
            duckdb_adbc_init(1_000_000, driver.cast(), std::ptr::null_mut()),
            OK
        );
        let prefix = std::mem::offset_of!(AdbcDriver, ErrorGetDetailCount) / std::mem::size_of::<usize>();
        assert_eq!(&storage[prefix..], &before[prefix..]);
        for offset in [
            std::mem::offset_of!(AdbcDriver, private_data),
            std::mem::offset_of!(AdbcDriver, private_manager),
            std::mem::offset_of!(AdbcDriver, release),
            std::mem::offset_of!(AdbcDriver, StatementSetSubstraitPlan),
        ] {
            assert_eq!(storage[offset / 8], before[offset / 8]);
        }
        let mut full = AdbcDriver::default();
        assert_eq!(
            duckdb_adbc_init(1_001_000, (&raw mut full).cast(), std::ptr::null_mut()),
            OK
        );
        assert!(
            full.ConnectionGetInfo.is_some()
                && full.ConnectionGetStatistics.is_some()
                && full.ErrorFromArrayStream.is_some()
        );
        assert!(full.release.is_none() && full.StatementSetSubstraitPlan.is_none());
        let mut db = AdbcDatabase::default();
        let mut error = AdbcError::default();
        ok(full.DatabaseNew.unwrap()(&raw mut db, &raw mut error), &mut error);
        ok(
            full.DatabaseInit.unwrap()(&raw mut db, &raw mut error),
            &mut error,
        );
        ok(
            full.DatabaseRelease.unwrap()(&raw mut db, &raw mut error),
            &mut error,
        );
    }
}
