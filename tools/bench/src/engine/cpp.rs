//! C++ DuckDB, loaded at run time from the reference build's `libduckdb` through its
//! stable C API (`duckdb.h`). Nothing C++ is compiled or linked into this crate.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::ptr;
use std::rc::Rc;

use super::{DbConfig, Engine, EngineError, ExtensionStatus, QueryResult};
use crate::verify::{ResultColumn, ResultTable, ValueKind};

type Idx = u64;
/// `duckdb_database`, `duckdb_connection`, `duckdb_config`, `duckdb_logical_type`.
type Handle = *mut c_void;
/// `duckdb_state`: `DuckDBSuccess` is 0.
type State = u32;
const SUCCESS: State = 0;

/// `struct duckdb_result` (`duckdb.h`).
#[repr(C)]
struct RawResult {
    deprecated_column_count: Idx,
    deprecated_row_count: Idx,
    deprecated_rows_changed: Idx,
    deprecated_columns: *mut c_void,
    deprecated_error_message: *mut c_char,
    internal_data: *mut c_void,
}

impl RawResult {
    const fn empty() -> Self {
        Self {
            deprecated_column_count: 0,
            deprecated_row_count: 0,
            deprecated_rows_changed: 0,
            deprecated_columns: ptr::null_mut(),
            deprecated_error_message: ptr::null_mut(),
            internal_data: ptr::null_mut(),
        }
    }
}

/// `DUCKDB_ERROR_NOT_IMPLEMENTED`.
const ERROR_NOT_IMPLEMENTED: u32 = 11;

// `DUCKDB_TYPE` values.
const TYPE_BOOLEAN: u32 = 1;
const TYPE_TINYINT: u32 = 2;
const TYPE_UBIGINT: u32 = 9;
const TYPE_FLOAT: u32 = 10;
const TYPE_DOUBLE: u32 = 11;
const TYPE_HUGEINT: u32 = 16;
const TYPE_VARCHAR: u32 = 17;
const TYPE_DECIMAL: u32 = 19;
const TYPE_LIST: u32 = 24;
const TYPE_STRUCT: u32 = 25;
const TYPE_MAP: u32 = 26;
const TYPE_UHUGEINT: u32 = 32;
const TYPE_ARRAY: u32 = 33;

/// Declares the C API functions we use and loads them by name (`duckdb_<field>`).
macro_rules! c_api {
    ($($field:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        /// The `libduckdb` functions used by the driver.
        struct CApi {
            $($field: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
        }

        impl CApi {
            /// Resolves every function in `library`, a `dlopen` handle.
            fn load(library: *mut c_void) -> Result<Self, String> {
                Ok(Self {
                    $($field: {
                        let name = concat!("duckdb_", stringify!($field), "\0");
                        // SAFETY: `library` is a live handle from `dlopen` and `name` is
                        // NUL-terminated.
                        let symbol = unsafe { libc::dlsym(library, name.as_ptr().cast()) };
                        if symbol.is_null() {
                            return Err(format!("libduckdb does not export {}", name.trim_end_matches('\0')));
                        }
                        // SAFETY: the symbol is the C function of that name in duckdb.h,
                        // whose signature the declaration above reproduces.
                        unsafe {
                            std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(symbol)
                        }
                    },)*
                })
            }
        }
    };
}

c_api! {
    library_version: fn() -> *const c_char;
    create_config: fn(*mut Handle) -> State;
    set_config: fn(Handle, *const c_char, *const c_char) -> State;
    destroy_config: fn(*mut Handle);
    open_ext: fn(*const c_char, *mut Handle, Handle, *mut *mut c_char) -> State;
    close: fn(*mut Handle);
    connect: fn(Handle, *mut Handle) -> State;
    disconnect: fn(*mut Handle);
    query: fn(Handle, *const c_char, *mut RawResult) -> State;
    destroy_result: fn(*mut RawResult);
    result_error: fn(*mut RawResult) -> *const c_char;
    result_error_type: fn(*mut RawResult) -> u32;
    column_count: fn(*mut RawResult) -> Idx;
    row_count: fn(*mut RawResult) -> Idx;
    column_name: fn(*mut RawResult, Idx) -> *const c_char;
    column_logical_type: fn(*mut RawResult, Idx) -> Handle;
    get_type_id: fn(Handle) -> u32;
    decimal_width: fn(Handle) -> u8;
    decimal_scale: fn(Handle) -> u8;
    list_type_child_type: fn(Handle) -> Handle;
    array_type_child_type: fn(Handle) -> Handle;
    array_type_array_size: fn(Handle) -> Idx;
    map_type_key_type: fn(Handle) -> Handle;
    map_type_value_type: fn(Handle) -> Handle;
    struct_type_child_count: fn(Handle) -> Idx;
    struct_type_child_name: fn(Handle, Idx) -> *mut c_char;
    struct_type_child_type: fn(Handle, Idx) -> Handle;
    destroy_logical_type: fn(*mut Handle);
    value_is_null: fn(*mut RawResult, Idx, Idx) -> bool;
    value_varchar: fn(*mut RawResult, Idx, Idx) -> *mut c_char;
    free: fn(*mut c_void);
}

/// Copies a C string owned by DuckDB.
///
/// # Safety
/// `ptr` must be null or point to a NUL-terminated string that stays valid for the call.
unsafe fn copy_c_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        // SAFETY: non-null and NUL-terminated per the caller's contract.
        unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
    }
}

/// Copies and frees a C string allocated by DuckDB.
///
/// # Safety
/// `ptr` must be null or a NUL-terminated string allocated by DuckDB and not yet freed.
unsafe fn take_c_string(api: &CApi, ptr: *mut c_char) -> String {
    // SAFETY: per the caller's contract.
    let value = unsafe { copy_c_string(ptr) };
    if !ptr.is_null() {
        // SAFETY: allocated by DuckDB and freed exactly once, here.
        unsafe { (api.free)(ptr.cast()) };
    }
    value
}

fn c_string(s: &str) -> Result<CString, EngineError> {
    CString::new(s).map_err(|_| EngineError::failed("string contains a NUL byte"))
}

impl CApi {
    /// `LogicalType::ToString` for a C API logical type. Consumes `ty`.
    ///
    /// # Safety
    /// `ty` must be a live logical type owned by the caller.
    unsafe fn type_name(&self, mut ty: Handle) -> String {
        // SAFETY: `ty` is live (caller's contract).
        let id = unsafe { (self.get_type_id)(ty) };
        let name = match id {
            TYPE_DECIMAL => {
                // SAFETY: `ty` is a live DECIMAL type.
                let (width, scale) = unsafe { ((self.decimal_width)(ty), (self.decimal_scale)(ty)) };
                format!("DECIMAL({width},{scale})")
            }
            TYPE_LIST => {
                // SAFETY: `ty` is a live LIST type; the child is owned by us and consumed.
                let child = unsafe { self.type_name((self.list_type_child_type)(ty)) };
                format!("{child}[]")
            }
            TYPE_ARRAY => {
                // SAFETY: `ty` is a live ARRAY type; the child is owned by us and consumed.
                let (child, size) = unsafe {
                    (
                        self.type_name((self.array_type_child_type)(ty)),
                        (self.array_type_array_size)(ty),
                    )
                };
                format!("{child}[{size}]")
            }
            TYPE_MAP => {
                // SAFETY: `ty` is a live MAP type; both children are owned by us and consumed.
                let (key, value) = unsafe {
                    (
                        self.type_name((self.map_type_key_type)(ty)),
                        self.type_name((self.map_type_value_type)(ty)),
                    )
                };
                format!("MAP({key}, {value})")
            }
            TYPE_STRUCT => {
                // SAFETY: `ty` is a live STRUCT type.
                let count = unsafe { (self.struct_type_child_count)(ty) };
                let children: Vec<String> = (0..count)
                    .map(|i| {
                        // SAFETY: `i` is in range; the name and child type are owned by
                        // us and released by `take_c_string` / `type_name`.
                        let (name, child) = unsafe {
                            (
                                take_c_string(self, (self.struct_type_child_name)(ty, i)),
                                self.type_name((self.struct_type_child_type)(ty, i)),
                            )
                        };
                        format!("{} {child}", quote_identifier(&name))
                    })
                    .collect();
                format!("STRUCT({})", children.join(", "))
            }
            _ => primitive_type_name(id).to_owned(),
        };
        // SAFETY: `ty` is owned by us and destroyed exactly once.
        unsafe { (self.destroy_logical_type)(&raw mut ty) };
        name
    }

    /// The type name and comparison kind of a result column.
    ///
    /// # Safety
    /// `result` must be a live, successful result and `column` in range.
    unsafe fn column_type(&self, result: *mut RawResult, column: Idx) -> (String, ValueKind) {
        // SAFETY: per the caller's contract; the returned type is owned by us.
        let ty = unsafe { (self.column_logical_type)(result, column) };
        // SAFETY: `ty` is live.
        let (id, scale) = unsafe { ((self.get_type_id)(ty), (self.decimal_scale)(ty)) };
        let kind = match id {
            TYPE_BOOLEAN => ValueKind::Boolean,
            TYPE_TINYINT..=TYPE_UBIGINT | TYPE_HUGEINT | TYPE_UHUGEINT => ValueKind::Integer,
            TYPE_FLOAT => ValueKind::Float,
            TYPE_DOUBLE => ValueKind::Double,
            TYPE_DECIMAL => ValueKind::Decimal { scale },
            TYPE_VARCHAR => ValueKind::Varchar,
            _ => ValueKind::Other,
        };
        // SAFETY: `ty` is live and owned by us; `type_name` consumes it.
        (unsafe { self.type_name(ty) }, kind)
    }
}

/// C++ DuckDB through `libduckdb`, with at most one open database and connection.
pub struct CppEngine {
    api: Rc<CApi>,
    database: Handle,
    connection: Handle,
}

impl std::fmt::Debug for CppEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CppEngine")
            .field("open", &!self.database.is_null())
            .finish_non_exhaustive()
    }
}

impl CppEngine {
    /// Loads `libduckdb` from `library`.
    pub fn load(library: &Path) -> Result<Self, EngineError> {
        let path = c_string(&library.to_string_lossy())?;
        // SAFETY: `path` is NUL-terminated. Loading libduckdb only runs its static
        // initialisers; the handle is intentionally kept for the life of the process.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            // SAFETY: `dlerror` returns null or a NUL-terminated message.
            let reason = unsafe { copy_c_string(libc::dlerror()) };
            return Err(EngineError::failed(format!(
                "could not load {}: {reason}",
                library.display()
            )));
        }
        let api = CApi::load(handle).map_err(EngineError::failed)?;
        Ok(Self {
            api: Rc::new(api),
            database: ptr::null_mut(),
            connection: ptr::null_mut(),
        })
    }

    fn create_config(&self, config: &DbConfig) -> Result<Handle, EngineError> {
        let mut handle: Handle = ptr::null_mut();
        // SAFETY: `handle` is a valid out-pointer.
        if unsafe { (self.api.create_config)(&raw mut handle) } != SUCCESS {
            return Err(EngineError::failed("duckdb_create_config failed"));
        }
        for (name, value) in config.options() {
            let (c_name, c_value) = (c_string(name)?, c_string(&value)?);
            // SAFETY: `handle` is a live config; both strings are NUL-terminated.
            if unsafe { (self.api.set_config)(handle, c_name.as_ptr(), c_value.as_ptr()) } != SUCCESS {
                // SAFETY: `handle` is a live config, destroyed exactly once.
                unsafe { (self.api.destroy_config)(&raw mut handle) };
                return Err(EngineError::failed(format!("invalid setting {name}={value}")));
            }
        }
        Ok(handle)
    }
}

impl Drop for CppEngine {
    fn drop(&mut self) {
        self.close();
    }
}

/// A `duckdb_result`, destroyed on drop.
pub struct CppResult {
    raw: RawResult,
    api: Rc<CApi>,
}

impl std::fmt::Debug for CppResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CppResult").finish_non_exhaustive()
    }
}

impl CppResult {
    /// The result as the mutable pointer the C API takes; the accessors used here only
    /// read the result (DuckDB materialises values lazily behind the pointer).
    fn raw(&self) -> *mut RawResult {
        (&raw const self.raw).cast_mut()
    }
}

impl Drop for CppResult {
    fn drop(&mut self) {
        // SAFETY: the result was filled by `duckdb_query` and is destroyed exactly once.
        // DuckDB results own their data, so this is valid even after the database closed.
        unsafe { (self.api.destroy_result)(&raw mut self.raw) };
    }
}

impl QueryResult for CppResult {
    fn row_count(&self) -> usize {
        // SAFETY: a live, successful result.
        unsafe { (self.api.row_count)(self.raw()) as usize }
    }

    fn column_count(&self) -> usize {
        // SAFETY: a live, successful result.
        unsafe { (self.api.column_count)(self.raw()) as usize }
    }

    fn to_table(&self) -> Result<ResultTable, EngineError> {
        let (api, raw) = (&*self.api, self.raw());
        // SAFETY: a live, successful result.
        let (column_count, row_count) = unsafe { ((api.column_count)(raw), (api.row_count)(raw)) };
        let columns: Vec<ResultColumn> = (0..column_count)
            .map(|c| {
                // SAFETY: `c` is in range; the name is owned by the result.
                let name = unsafe { copy_c_string((api.column_name)(raw, c)) };
                // SAFETY: `c` is in range of a live, successful result.
                let (type_name, kind) = unsafe { api.column_type(raw, c) };
                ResultColumn {
                    name,
                    type_name,
                    kind,
                }
            })
            .collect();
        let rows = (0..row_count)
            .map(|r| {
                (0..column_count)
                    .map(|c| {
                        // SAFETY: `c` and `r` are in range of a live result; the string
                        // returned by `value_varchar` is ours to free.
                        unsafe {
                            if (api.value_is_null)(raw, c, r) {
                                None
                            } else {
                                Some(take_c_string(api, (api.value_varchar)(raw, c, r)))
                            }
                        }
                    })
                    .collect()
            })
            .collect();
        Ok(ResultTable { columns, rows })
    }
}

impl Engine for CppEngine {
    type Result = CppResult;

    fn version(&self) -> String {
        // SAFETY: returns a static NUL-terminated string.
        let version = unsafe { copy_c_string((self.api.library_version)()) };
        format!("DuckDB {version} (libduckdb C API)")
    }

    fn open(&mut self, path: Option<&Path>, config: &DbConfig) -> Result<(), EngineError> {
        self.close();
        let path = c_string(&path.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default())?;
        let mut db_config = self.create_config(config)?;
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: valid out-pointers, a live config and a NUL-terminated path.
        let state =
            unsafe { (self.api.open_ext)(path.as_ptr(), &raw mut self.database, db_config, &raw mut error) };
        // SAFETY: the config is no longer needed once the database is opened.
        unsafe { (self.api.destroy_config)(&raw mut db_config) };
        if state != SUCCESS {
            // SAFETY: `error` is null or a message allocated by DuckDB.
            let message = unsafe { take_c_string(&self.api, error) };
            self.database = ptr::null_mut();
            return Err(EngineError::failed(message));
        }
        // SAFETY: `database` is open; valid out-pointer.
        if unsafe { (self.api.connect)(self.database, &raw mut self.connection) } != SUCCESS {
            self.close();
            return Err(EngineError::failed("duckdb_connect failed"));
        }
        Ok(())
    }

    fn close(&mut self) {
        if !self.connection.is_null() {
            // SAFETY: a live connection, disconnected once (the call nulls the handle).
            unsafe { (self.api.disconnect)(&raw mut self.connection) };
        }
        if !self.database.is_null() {
            // SAFETY: a live database, closed once (the call nulls the handle).
            unsafe { (self.api.close)(&raw mut self.database) };
        }
    }

    fn query(&mut self, sql: &str) -> Result<CppResult, EngineError> {
        if self.connection.is_null() {
            return Err(EngineError::failed("no open database"));
        }
        let sql = c_string(sql)?;
        let mut result = CppResult {
            raw: RawResult::empty(),
            api: Rc::clone(&self.api),
        };
        // SAFETY: a live connection, NUL-terminated SQL and a valid out-pointer. The
        // result is destroyed by `CppResult::drop` even on error, as the C API requires.
        let state = unsafe { (self.api.query)(self.connection, sql.as_ptr(), &raw mut result.raw) };
        if state == SUCCESS {
            return Ok(result);
        }
        // SAFETY: the failed result stays live until `result` drops.
        let (message, kind) = unsafe {
            (
                copy_c_string((self.api.result_error)(&raw mut result.raw)),
                (self.api.result_error_type)(&raw mut result.raw),
            )
        };
        Err(if kind == ERROR_NOT_IMPLEMENTED {
            EngineError::unsupported(message)
        } else {
            EngineError::failed(message)
        })
    }

    fn load_extension(&mut self, name: &str) -> Result<ExtensionStatus, EngineError> {
        self.query(&format!("LOAD {}", quote_identifier(name)))?;
        Ok(ExtensionStatus::Loaded)
    }
}

/// `KeywordHelper::WriteOptionallyQuoted`, simplified: always quote.
fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `LogicalTypeIdToString` for the C API's primitive type ids.
fn primitive_type_name(id: u32) -> &'static str {
    match id {
        1 => "BOOLEAN",
        2 => "TINYINT",
        3 => "SMALLINT",
        4 => "INTEGER",
        5 => "BIGINT",
        6 => "UTINYINT",
        7 => "USMALLINT",
        8 => "UINTEGER",
        9 => "UBIGINT",
        10 => "FLOAT",
        11 => "DOUBLE",
        12 => "TIMESTAMP",
        13 => "DATE",
        14 => "TIME",
        15 => "INTERVAL",
        16 => "HUGEINT",
        18 => "BLOB",
        20 => "TIMESTAMP_S",
        21 => "TIMESTAMP_MS",
        22 => "TIMESTAMP_NS",
        27 => "UUID",
        29 => "BIT",
        30 => "TIME WITH TIME ZONE",
        31 => "TIMESTAMP WITH TIME ZONE",
        32 => "UHUGEINT",
        35 => "BIGNUM",
        36 => "\"NULL\"",
        39 => "TIME_NS",
        40 => "GEOMETRY",
        41 => "VARIANT",
        // VARCHAR itself; ENUM and UNION values round-trip through VARCHAR too.
        _ => "VARCHAR",
    }
}
