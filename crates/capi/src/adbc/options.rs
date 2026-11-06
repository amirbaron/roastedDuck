//! Frozen ADBC typed-option behavior, including options genuinely unsupported by DuckDB.
use super::*;
const TABLE: &[u8] = b"adbc.ingest.target_table";
const SCHEMA: &[u8] = b"adbc.ingest.target_db_schema";
const CATALOG: &[u8] = b"adbc.ingest.target_catalog";
const TEMPORARY: &[u8] = b"adbc.ingest.temporary";
const MODE: &[u8] = b"adbc.ingest.mode";
#[derive(Default)]
pub(super) struct IngestOptions {
    pub table: Option<ByteString>,
    pub schema: Option<ByteString>,
    pub catalog: Option<ByteString>,
    pub temporary: bool,
    pub mode: Mode,
}
#[derive(Default, Clone, Copy)]
pub(super) enum Mode {
    #[default]
    Create,
    Append,
    Replace,
    CreateAppend,
}
impl Mode {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Create => b"adbc.ingest.mode.create",
            Self::Append => b"adbc.ingest.mode.append",
            Self::Replace => b"adbc.ingest.mode.replace",
            Self::CreateAppend => b"adbc.ingest.mode.create_append",
        }
    }
}
pub(super) unsafe extern "C" fn statement_set(
    value: *mut AdbcStatement,
    key: *const c_char,
    input: *const c_char,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: option bytes are borrowed C strings; state owns independent copies.
    unsafe {
        if value.is_null() {
            return fail(error, INVALID_ARGUMENT, b"Missing statement object");
        }
        let Some(state) = statement(value) else {
            return fail(error, INVALID_ARGUMENT, b"Invalid statement object");
        };
        let Some(key) = opt_bytes(key) else {
            return fail(error, INVALID_ARGUMENT, b"Missing key object");
        };
        let Some(input) = opt_bytes(input) else {
            return fail(error, INVALID_ARGUMENT, b"Option value must not be NULL");
        };
        match key {
            TABLE=>state.ingest.table=Some(input.into()),SCHEMA=>state.ingest.schema=Some(input.into()),CATALOG=>state.ingest.catalog=Some(input.into()),
            TEMPORARY=>match input{b"true"=>{state.ingest.temporary=true;state.ingest.schema=None;state.ingest.catalog=None;},b"false"=>state.ingest.temporary=false,_=>return fail(error,INVALID_ARGUMENT,b"ADBC_INGEST_OPTION_TEMPORARY, can only be ADBC_OPTION_VALUE_ENABLED or ADBC_OPTION_VALUE_DISABLED")},
            MODE=>state.ingest.mode=match input{b"adbc.ingest.mode.create"=>Mode::Create,b"adbc.ingest.mode.append"=>Mode::Append,b"adbc.ingest.mode.replace"=>Mode::Replace,b"adbc.ingest.mode.create_append"=>Mode::CreateAppend,_=>return fail(error,INVALID_ARGUMENT,b"Invalid ingestion mode")},
            _=>return fail(error,INVALID_ARGUMENT,byte_format!("Statement Set Option {key} is not yet accepted by DuckDB")),
        }
        OK
    }
}
pub(super) unsafe extern "C" fn statement_get(
    value: *mut AdbcStatement,
    key: *const c_char,
    out: *mut c_char,
    length: *mut usize,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: strings stored in state remain live while copied into the caller's option buffer.
    unsafe {
        let Some(state) = statement(value) else {
            return fail(error, INVALID_ARGUMENT, b"Invalid statement object");
        };
        let Some(key) = opt_bytes(key) else {
            return fail(error, INVALID_ARGUMENT, b"Missing key");
        };
        let text = match key {
            TABLE => state.ingest.table.as_deref(),
            SCHEMA => state.ingest.schema.as_deref(),
            CATALOG => state.ingest.catalog.as_deref(),
            TEMPORARY => Some(if state.ingest.temporary {
                b"true".as_slice()
            } else {
                b"false".as_slice()
            }),
            MODE => Some(state.ingest.mode.bytes()),
            _ => return fail(error, NOT_FOUND, byte_format!("Option not found: {key}")),
        };
        match text {
            Some(text) => option_string(text, out, length, error),
            None => fail(error, NOT_FOUND, byte_format!("Option not set: {key}")),
        }
    }
}
pub(super) unsafe extern "C" fn statement_get_int(
    value: *mut AdbcStatement,
    key: *const c_char,
    out: *mut i64,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: caller supplies a writable numeric option slot.
    unsafe {
        let Some(state) = statement(value) else {
            return fail(error, INVALID_ARGUMENT, b"Invalid statement object");
        };
        let Some(key) = opt_bytes(key) else {
            return fail(error, INVALID_ARGUMENT, b"Missing key");
        };
        if key == TEMPORARY && !out.is_null() {
            *out = i64::from(state.ingest.temporary);
            OK
        } else {
            fail(error, NOT_FOUND, byte_format!("Option not found: {key}"))
        }
    }
}
pub(super) unsafe extern "C" fn statement_set_int(
    value: *mut AdbcStatement,
    key: *const c_char,
    input: i64,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: the generated option string lives through its delegated setter call.
    unsafe {
        let Some(bytes) = opt_bytes(key) else {
            return fail(error, INVALID_ARGUMENT, b"Missing key");
        };
        let text = if bytes == TEMPORARY {
            to_cstring(if input == 0 { b"false".as_slice() } else { b"true" })
        } else {
            to_cstring(input.to_string())
        };
        statement_set(value, key, text.as_ptr(), error)
    }
}
macro_rules! missing_getters {
    ($handle:ty,$lookup:ident,$missing:expr;$( $name:ident($($arg:ident:$ty:ty),*) );* $(;)?)=>{$(
        pub(super) unsafe extern "C" fn $name(value:*mut $handle,key:*const c_char,$($arg:$ty,)*error:*mut AdbcError)->u8 {
            // SAFETY: the lookup only reads initialized private_data; no unsupported output is touched.
            unsafe {if $lookup(value).is_none(){return fail(error,INVALID_ARGUMENT,$missing);}let key=opt_bytes(key).unwrap_or(b"(null)");fail(error,NOT_FOUND,byte_format!("Option not found: {key}"))}
        }
    )*};
}
missing_getters!(AdbcDatabase,database,b"Missing database object";
    database_get_bytes(_out:*mut u8,_length:*mut usize);database_get_double(_out:*mut f64);database_get_int(_out:*mut i64));
missing_getters!(AdbcConnection,connection,b"Connection is not set";
    connection_get_bytes(_out:*mut u8,_length:*mut usize);connection_get_double(_out:*mut f64));
missing_getters!(AdbcStatement,statement,b"Invalid statement object";
    statement_get_bytes(_out:*mut u8,_length:*mut usize);statement_get_double(_out:*mut f64));
macro_rules! unsupported_setters {
    ($handle:ty,$label:literal;$( $name:ident,$operation:literal($($arg:ident:$ty:ty),*) );* $(;)?)=>{$(
        pub(super) unsafe extern "C" fn $name(_value:*mut $handle,_key:*const c_char,$($arg:$ty,)*error:*mut AdbcError)->u8 {
            // SAFETY: this exact unsupported operation only writes to the optional error prefix.
            unsafe {fail(error,NOT_IMPLEMENTED,concat!($operation," is not supported for ",$label).as_bytes())}
        }
    )*};
}
unsupported_setters!(AdbcDatabase,"database";database_set_bytes,"SetOptionBytes"(_out:*const u8,_length:usize));
unsupported_setters!(AdbcConnection,"connection";connection_set_bytes,"SetOptionBytes"(_out:*const u8,_length:usize);connection_set_double,"SetOptionDouble"(_input:f64));
unsupported_setters!(AdbcStatement,"statement";statement_set_bytes,"SetOptionBytes"(_out:*const u8,_length:usize);statement_set_double,"SetOptionDouble"(_input:f64));
pub(super) unsafe extern "C" fn read_partition(
    _value: *mut AdbcConnection,
    _data: *const u8,
    _length: usize,
    _out: *mut ArrowArrayStream,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: frozen unsupported operation only writes to the optional error prefix.
    unsafe {
        fail(
            error,
            NOT_IMPLEMENTED,
            b"Read Partitions are not supported in DuckDB",
        )
    }
}
pub(super) unsafe extern "C" fn execute_partitions(
    _value: *mut AdbcStatement,
    _schema: *mut ArrowSchema,
    _partitions: *mut AdbcPartitions,
    _rows: *mut i64,
    error: *mut AdbcError,
) -> u8 {
    // SAFETY: frozen unsupported operation only writes to the optional error prefix.
    unsafe {
        fail(
            error,
            NOT_IMPLEMENTED,
            b"Execute Partitions are not supported in DuckDB",
        )
    }
}
