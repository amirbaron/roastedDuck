//! The C++ DuckDB oracle, driven through the `libslt_oracle` shim (`tools/oracle-shim`).
//!
//! The shim is loaded at runtime with `libloading`; nothing C++ is compiled by cargo. All `unsafe`
//! code of the crate's FFI lives in this module.

use std::ffi::{CStr, c_char};
use std::marker::PhantomData;
use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use libloading::Library;

use super::{ColumnClass, Engine, ExtensionLoad, QueryResult, RequireSetting};

/// The ABI version this runner speaks (`SLT_ORACLE_ABI_VERSION`).
const ABI_VERSION: i32 = 1;

macro_rules! opaque {
    ($($name:ident),*) => {$(
        #[repr(C)]
        struct $name {
            _data: [u8; 0],
            _marker: PhantomData<(*mut u8, std::marker::PhantomPinned)>,
        }
    )*};
}

opaque!(RawConfig, RawDb, RawConn, RawResult, RawSession);

type ErrorOut = *mut *mut c_char;

macro_rules! shim_api {
    ($($field:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        /// The shim's function table. Keeps the library loaded for as long as it is alive.
        struct Api {
            $($field: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
            _library: Library,
        }

        impl Api {
            /// Resolves every `slt_*` symbol of the shim.
            ///
            /// # Safety
            /// `library` must be a build of `tools/oracle-shim` whose exported functions have the
            /// signatures declared here (checked at runtime through `slt_abi_version`).
            unsafe fn load(library: Library) -> Result<Self, libloading::Error> {
                $(
                    // SAFETY: the symbol is a function with this signature (see the contract above);
                    // the copied pointer stays valid because `Api` owns the library.
                    let $field = unsafe {
                        *library.get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>(
                            concat!("slt_", stringify!($field), "\0").as_bytes(),
                        )?
                    };
                )*
                Ok(Self { $($field,)* _library: library })
            }
        }
    };
}

shim_api! {
    abi_version: fn() -> i32;
    build_directory: fn() -> *const c_char;
    string_free: fn(*mut c_char);
    config_new: fn(ErrorOut) -> *mut RawConfig;
    config_free: fn(*mut RawConfig);
    config_set_option: fn(*mut RawConfig, *const c_char, usize, *const c_char, usize, ErrorOut) -> bool;
    config_set_read_only: fn(*mut RawConfig, bool);
    config_set_storage_version: fn(*mut RawConfig, *const c_char, usize, ErrorOut) -> bool;
    config_inherit_storage_version: fn(*mut RawConfig, *const RawDb);
    config_setting: fn(*const RawConfig, i32) -> u64;
    db_open: fn(*const c_char, usize, *const RawConfig, ErrorOut) -> *mut RawDb;
    db_free: fn(*mut RawDb);
    db_load_extension: fn(*mut RawDb, *const c_char, usize, ErrorOut) -> i32;
    conn_new: fn(*mut RawDb, ErrorOut) -> *mut RawConn;
    conn_free: fn(*mut RawConn);
    conn_set_test_client_options: fn(*mut RawConn);
    session_save: fn(*mut RawConn, *mut RawConfig) -> *mut RawSession;
    session_restore: fn(*mut RawConn, *const RawSession, ErrorOut) -> bool;
    session_free: fn(*mut RawSession);
    query: fn(*mut RawConn, *const c_char, usize) -> *mut RawResult;
    result_free: fn(*mut RawResult);
    result_error: fn(*const RawResult) -> *const c_char;
    result_column_count: fn(*const RawResult) -> u64;
    result_row_count: fn(*const RawResult) -> u64;
    result_column_name: fn(*const RawResult, u64) -> *const c_char;
    result_column_type: fn(*const RawResult, u64) -> *const c_char;
    result_column_class: fn(*const RawResult, u64) -> i32;
    result_render: fn(*mut RawResult, *mut RawConn, bool, ErrorOut) -> bool;
    result_value_count: fn(*const RawResult) -> u64;
    result_value: fn(*const RawResult, u64, *mut usize) -> *const c_char;
    values_equal: fn(*mut RawConn, *const RawResult, u64, *const c_char, usize, *const c_char, usize) -> bool;
    load_result_file: fn(*mut RawConn, *const c_char, usize, *const *const c_char, *const usize, u64) -> *mut RawResult;
}

impl Api {
    /// Copies and frees a shim-allocated error message.
    fn take_error(&self, error: *mut c_char, fallback: &str) -> String {
        if error.is_null() {
            return fallback.to_owned();
        }
        // SAFETY: the shim hands out NUL-terminated malloc'd strings that we own from here on.
        let message = unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned();
        // SAFETY: `error` came from the shim's allocator and is freed exactly once.
        unsafe { (self.string_free)(error) };
        message
    }
}

/// Like [`borrowed_str`], but a null pointer is `None`.
///
/// # Safety
/// `text` must be null or point to a NUL-terminated string that outlives the call.
unsafe fn borrowed_optional_str(text: *const c_char) -> Option<String> {
    // SAFETY: guaranteed by the caller.
    (!text.is_null()).then(|| unsafe { borrowed_str(text) })
}

/// Copies a NUL-terminated string owned by a live shim handle.
///
/// # Safety
/// `text` must be null or point to a NUL-terminated string that outlives the call.
unsafe fn borrowed_str(text: *const c_char) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: guaranteed by the caller.
    unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned()
}

macro_rules! handle {
    ($name:ident, $raw:ty, $free:ident) => {
        #[doc = concat!("Owned shim handle freed with `slt_", stringify!($free), "`.")]
        #[derive(Debug)]
        pub struct $name {
            raw: NonNull<$raw>,
            api: Arc<Api>,
        }

        impl Drop for $name {
            fn drop(&mut self) {
                // SAFETY: `raw` was created by the shim and is freed exactly once, here.
                unsafe { (self.api.$free)(self.raw.as_ptr()) };
            }
        }

        // SAFETY: the shim objects are plain heap objects without thread affinity; the runner never
        // uses one handle from two threads at once except through `&self` methods that are
        // serialized (see `CppConnection`).
        unsafe impl Send for $name {}
        // SAFETY: as above; shared access only reads immutable data or takes the context lock.
        unsafe impl Sync for $name {}
    };
}

handle!(CppConfig, RawConfig, config_free);
handle!(CppDatabase, RawDb, db_free);
handle!(CppSession, RawSession, session_free);

/// A connection plus a lock that serializes the casts other threads run in its client context.
#[derive(Debug)]
pub struct CppConnection {
    raw: NonNull<RawConn>,
    api: Arc<Api>,
    context_lock: Mutex<()>,
}

impl CppConnection {
    fn lock_context(&self) -> MutexGuard<'_, ()> {
        self.context_lock.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for CppConnection {
    fn drop(&mut self) {
        // SAFETY: `raw` was created by `slt_conn_new` and is freed exactly once, here.
        unsafe { (self.api.conn_free)(self.raw.as_ptr()) };
    }
}

// SAFETY: a DuckDB connection may move between threads.
unsafe impl Send for CppConnection {}
// SAFETY: `&self` access (casting in the client context) is serialized by `context_lock`.
unsafe impl Sync for CppConnection {}

impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Api").finish_non_exhaustive()
    }
}

/// A shim result (freed with `slt_result_free`) with its error message decoded once; DuckDB error
/// messages may quote invalid UTF-8 from the data, which is replaced like everywhere else.
#[derive(Debug)]
pub struct CppResult {
    raw: NonNull<RawResult>,
    api: Arc<Api>,
    error: Option<String>,
}

impl CppResult {
    fn new(raw: NonNull<RawResult>, api: Arc<Api>) -> Self {
        // SAFETY: the error string (if any) is owned by the live result and NUL-terminated.
        let error = unsafe { borrowed_optional_str((api.result_error)(raw.as_ptr())) };
        Self { raw, api, error }
    }
}

impl Drop for CppResult {
    fn drop(&mut self) {
        // SAFETY: `raw` was created by the shim and is freed exactly once, here.
        unsafe { (self.api.result_free)(self.raw.as_ptr()) };
    }
}

// SAFETY: a materialized result is a plain heap object without thread affinity.
unsafe impl Send for CppResult {}

impl QueryResult for CppResult {
    fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn column_count(&self) -> usize {
        // SAFETY: plain accessor on a live result.
        unsafe { (self.api.result_column_count)(self.raw.as_ptr()) as usize }
    }

    fn row_count(&self) -> usize {
        // SAFETY: plain accessor on a live result.
        unsafe { (self.api.result_row_count)(self.raw.as_ptr()) as usize }
    }

    fn column_name(&self, column: usize) -> &str {
        // SAFETY: the name is owned by the live result; out-of-range columns yield "".
        let name = unsafe { (self.api.result_column_name)(self.raw.as_ptr(), column as u64) };
        // SAFETY: NUL-terminated string borrowed from `self`.
        unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("")
    }

    fn column_type(&self, column: usize) -> String {
        // SAFETY: the type name is owned by the live result.
        unsafe { borrowed_str((self.api.result_column_type)(self.raw.as_ptr(), column as u64)) }
    }

    fn column_class(&self, column: usize) -> ColumnClass {
        // SAFETY: plain accessor on a live result.
        match unsafe { (self.api.result_column_class)(self.raw.as_ptr(), column as u64) } {
            1 => ColumnClass::Numeric,
            2 => ColumnClass::Boolean,
            _ => ColumnClass::Other,
        }
    }
}

impl CppResult {
    /// The rendered values held by the result.
    fn values(&self) -> Vec<String> {
        // SAFETY: plain accessor on a live result.
        let count = unsafe { (self.api.result_value_count)(self.raw.as_ptr()) };
        (0..count)
            .map(|index| {
                let mut len = 0usize;
                // SAFETY: `index < count`; the returned buffer of `len` bytes is owned by `self`.
                let data = unsafe { (self.api.result_value)(self.raw.as_ptr(), index, &raw mut len) };
                // SAFETY: as above.
                let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) };
                String::from_utf8_lossy(bytes).into_owned()
            })
            .collect()
    }
}

/// The C++ DuckDB 1.5.6 engine behind the oracle shim.
#[derive(Debug)]
pub struct CppEngine {
    api: Arc<Api>,
    build_directory: String,
}

impl CppEngine {
    /// Loads the shim library at `path`.
    pub fn load(path: &Path) -> Result<Self, String> {
        // SAFETY: loading the shim runs DuckDB's static initializers, which have no preconditions.
        let library = unsafe { Library::new(path) }
            .map_err(|error| format!("cannot load oracle shim {}: {error}", path.display()))?;
        // SAFETY: the library is the oracle shim; its ABI version is verified right below before
        // any other function is called.
        let api = unsafe { Api::load(library) }
            .map_err(|error| format!("{} is not an oracle shim: {error}", path.display()))?;
        // SAFETY: no arguments, returns a constant.
        let version = unsafe { (api.abi_version)() };
        if version != ABI_VERSION {
            return Err(format!(
                "oracle shim ABI version {version} does not match the runner ({ABI_VERSION}); rebuild it"
            ));
        }
        // SAFETY: returns a static NUL-terminated string.
        let build_directory = unsafe { borrowed_str((api.build_directory)()) };
        Ok(Self {
            api: Arc::new(api),
            build_directory,
        })
    }

    fn wrap<T>(&self, raw: *mut T, error: *mut c_char, what: &str) -> Result<(NonNull<T>, Arc<Api>), String> {
        match NonNull::new(raw) {
            Some(raw) => Ok((raw, Arc::clone(&self.api))),
            None => Err(self.api.take_error(error, &format!("{what} failed"))),
        }
    }
}

impl Engine for CppEngine {
    type Config = CppConfig;
    type Database = CppDatabase;
    type Connection = CppConnection;
    type Result = CppResult;
    type Session = CppSession;

    fn name(&self) -> &'static str {
        "cpp"
    }

    fn build_directory(&self) -> &str {
        &self.build_directory
    }

    fn test_config(&self) -> Result<CppConfig, String> {
        let mut error = ptr::null_mut();
        // SAFETY: `error` is a valid out-parameter.
        let raw = unsafe { (self.api.config_new)(&raw mut error) };
        let (raw, api) = self.wrap(raw, error, "creating the test configuration")?;
        Ok(CppConfig { raw, api })
    }

    fn set_option(&self, config: &mut CppConfig, name: &str, value: &str) -> Result<(), String> {
        let mut error = ptr::null_mut();
        // SAFETY: the strings are passed as (pointer, length) and only read during the call.
        let ok = unsafe {
            (self.api.config_set_option)(
                config.raw.as_ptr(),
                name.as_ptr().cast(),
                name.len(),
                value.as_ptr().cast(),
                value.len(),
                &raw mut error,
            )
        };
        if ok {
            Ok(())
        } else {
            Err(self.api.take_error(error, "setting an option failed"))
        }
    }

    fn set_read_only(&self, config: &mut CppConfig, read_only: bool) {
        // SAFETY: plain setter on a live configuration.
        unsafe { (self.api.config_set_read_only)(config.raw.as_ptr(), read_only) };
    }

    fn set_storage_version(&self, config: &mut CppConfig, version: &str) -> Result<(), String> {
        let mut error = ptr::null_mut();
        // SAFETY: the string is only read during the call.
        let ok = unsafe {
            (self.api.config_set_storage_version)(
                config.raw.as_ptr(),
                version.as_ptr().cast(),
                version.len(),
                &raw mut error,
            )
        };
        if ok {
            Ok(())
        } else {
            Err(self.api.take_error(error, "setting the storage version failed"))
        }
    }

    fn inherit_storage_version(&self, config: &mut CppConfig, database: &CppDatabase) {
        // SAFETY: both handles are live.
        unsafe { (self.api.config_inherit_storage_version)(config.raw.as_ptr(), database.raw.as_ptr()) };
    }

    fn setting(&self, config: &CppConfig, setting: RequireSetting) -> u64 {
        let id = match setting {
            RequireSetting::DefaultBlockSize => 0,
            RequireSetting::AutoloadKnownExtensions => 1,
            RequireSetting::AllowUnsignedExtensions => 2,
            RequireSetting::VacuumRebuildIndexes => 3,
        };
        // SAFETY: plain accessor on a live configuration.
        unsafe { (self.api.config_setting)(config.raw.as_ptr(), id) }
    }

    fn open(&self, path: &str, config: &CppConfig) -> Result<CppDatabase, String> {
        let mut error = ptr::null_mut();
        // SAFETY: the path is only read during the call; the configuration is copied by DuckDB.
        let raw = unsafe {
            (self.api.db_open)(
                path.as_ptr().cast(),
                path.len(),
                config.raw.as_ptr(),
                &raw mut error,
            )
        };
        let (raw, api) = self.wrap(raw, error, "opening the database")?;
        Ok(CppDatabase { raw, api })
    }

    fn load_extension(&self, database: &CppDatabase, name: &str) -> Result<ExtensionLoad, String> {
        let mut error = ptr::null_mut();
        // SAFETY: the name is only read during the call.
        let code = unsafe {
            (self.api.db_load_extension)(
                database.raw.as_ptr(),
                name.as_ptr().cast(),
                name.len(),
                &raw mut error,
            )
        };
        match code {
            0 => Ok(ExtensionLoad::Loaded),
            1 => Ok(ExtensionLoad::Unknown),
            2 => Ok(ExtensionLoad::NotLoaded),
            _ => Err(self.api.take_error(error, "loading the extension failed")),
        }
    }

    fn connect(&self, database: &CppDatabase) -> Result<CppConnection, String> {
        let mut error = ptr::null_mut();
        // SAFETY: the database handle is live; DuckDB connections keep the instance alive.
        let raw = unsafe { (self.api.conn_new)(database.raw.as_ptr(), &raw mut error) };
        let (raw, api) = self.wrap(raw, error, "connecting")?;
        Ok(CppConnection {
            raw,
            api,
            context_lock: Mutex::new(()),
        })
    }

    fn configure_test_client(&self, connection: &mut CppConnection) {
        // SAFETY: exclusive access to a live connection.
        unsafe { (self.api.conn_set_test_client_options)(connection.raw.as_ptr()) };
    }

    fn query(&self, connection: &mut CppConnection, sql: &str) -> CppResult {
        // SAFETY: exclusive access to a live connection; the SQL is only read during the call.
        let raw = unsafe { (self.api.query)(connection.raw.as_ptr(), sql.as_ptr().cast(), sql.len()) };
        // The shim reports query errors inside the result; NULL only means it ran out of memory.
        let raw = NonNull::new(raw).unwrap_or_else(|| std::process::abort());
        CppResult::new(raw, Arc::clone(&self.api))
    }

    fn render(
        &self,
        context: &CppConnection,
        result: &mut CppResult,
        original_sqlite_test: bool,
    ) -> Result<Vec<String>, String> {
        let mut error = ptr::null_mut();
        let ok = {
            let _guard = context.lock_context();
            // SAFETY: exclusive access to the result; the context connection is only used for casts
            // and its use is serialized by the lock.
            unsafe {
                (self.api.result_render)(
                    result.raw.as_ptr(),
                    context.raw.as_ptr(),
                    original_sqlite_test,
                    &raw mut error,
                )
            }
        };
        if ok {
            Ok(result.values())
        } else {
            Err(self.api.take_error(error, "converting the result failed"))
        }
    }

    fn values_equal(
        &self,
        context: &CppConnection,
        result: &CppResult,
        column: usize,
        actual: &str,
        expected: &str,
    ) -> bool {
        let _guard = context.lock_context();
        // SAFETY: the strings are only read during the call; context use is serialized.
        unsafe {
            (self.api.values_equal)(
                context.raw.as_ptr(),
                result.raw.as_ptr(),
                column as u64,
                actual.as_ptr().cast(),
                actual.len(),
                expected.as_ptr().cast(),
                expected.len(),
            )
        }
    }

    fn load_result_file(
        &self,
        context: &CppConnection,
        path: &str,
        names: &[String],
    ) -> Result<(Vec<String>, usize), String> {
        let pointers: Vec<*const c_char> = names.iter().map(|name| name.as_ptr().cast()).collect();
        let lengths: Vec<usize> = names.iter().map(String::len).collect();
        let raw = {
            let _guard = context.lock_context();
            // SAFETY: the arrays hold `names.len()` valid (pointer, length) pairs that outlive the call.
            unsafe {
                (self.api.load_result_file)(
                    context.raw.as_ptr(),
                    path.as_ptr().cast(),
                    path.len(),
                    pointers.as_ptr(),
                    lengths.as_ptr(),
                    names.len() as u64,
                )
            }
        };
        let raw = NonNull::new(raw).unwrap_or_else(|| std::process::abort());
        let result = CppResult::new(raw, Arc::clone(&self.api));
        let loaded = match result.error() {
            Some(error) => Err(error.to_owned()),
            None => Ok((result.values(), result.column_count())),
        };
        if std::env::var_os("SLT_VERIFY_RESULT_FILES").is_some() {
            verify_result_file_loader(path, names.len(), &loaded);
        }
        loaded
    }

    fn save_session(&self, main: &CppConnection, config: &mut CppConfig) -> CppSession {
        let _guard = main.lock_context();
        // SAFETY: both handles are live; the session copies what it needs.
        let raw = unsafe { (self.api.session_save)(main.raw.as_ptr(), config.raw.as_ptr()) };
        let raw = NonNull::new(raw).unwrap_or_else(|| std::process::abort());
        CppSession {
            raw,
            api: Arc::clone(&self.api),
        }
    }

    fn restore_session(&self, main: &mut CppConnection, session: CppSession) -> Result<(), String> {
        let mut error = ptr::null_mut();
        // SAFETY: exclusive access to the connection; the session is live for the call.
        let ok =
            unsafe { (self.api.session_restore)(main.raw.as_ptr(), session.raw.as_ptr(), &raw mut error) };
        if ok {
            Ok(())
        } else {
            Err(self.api.take_error(error, "restoring the client state failed"))
        }
    }
}

/// Debug aid (`SLT_VERIFY_RESULT_FILES=1`): checks the Rust `<FILE>:` loader used by the Rust
/// backend against DuckDB's `read_csv` and reports differences on stderr.
fn verify_result_file_loader(path: &str, columns: usize, oracle: &Result<(Vec<String>, usize), String>) {
    let rust = crate::result_file::load(path, columns);
    let agree = match (&rust, oracle) {
        (Ok(rust), Ok((values, _))) => rust == values,
        (Err(_), Err(_)) => true,
        _ => false,
    };
    if !agree {
        eprintln!("SLT_VERIFY_RESULT_FILES: loaders disagree on {path}: rust={rust:?} oracle={oracle:?}");
    }
}
