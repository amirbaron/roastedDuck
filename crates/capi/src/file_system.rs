//! The file system handed to clients (DuckDB's `capi/file_system-c.cpp` over the local
//! file system, `LocalFileSystem::OpenFile`, `Read`, `Write`, ...).

use std::ffi::{c_char, c_void};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};

use rustdb_common::{Error, ErrorKind};

use crate::error_data::{DuckdbErrorData, ErrorDataWrapper};
use crate::helper::opt_str;
use crate::open::{DuckdbClientContext, client_context_ref};
use crate::types::*;

/// `duckdb_file_flag`
pub type DuckdbFileFlag = u32;

pub const DUCKDB_FILE_FLAG_INVALID: DuckdbFileFlag = 0;
pub const DUCKDB_FILE_FLAG_READ: DuckdbFileFlag = 1;
pub const DUCKDB_FILE_FLAG_WRITE: DuckdbFileFlag = 2;
pub const DUCKDB_FILE_FLAG_CREATE: DuckdbFileFlag = 3;
pub const DUCKDB_FILE_FLAG_CREATE_NEW: DuckdbFileFlag = 4;
pub const DUCKDB_FILE_FLAG_APPEND: DuckdbFileFlag = 5;

/// What a `duckdb_file_system` points to (DuckDB's `CFileSystem`): the local file system
/// and the error of its last failed open.
#[derive(Debug, Default)]
pub struct CFileSystem {
    error: Option<Error>,
}

/// `duckdb_file_system`
pub type DuckdbFileSystem = *mut CFileSystem;

/// What a `duckdb_file_open_options` points to (DuckDB's `CFileOpenOptions`, the
/// `FileOpenFlags` set).
#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "the bits of DuckDB's FileOpenFlags")]
pub struct CFileOpenOptions {
    read: bool,
    write: bool,
    append: bool,
    create: bool,
    exclusive_create: bool,
}

/// `duckdb_file_open_options`
pub type DuckdbFileOpenOptions = *mut CFileOpenOptions;

/// What a `duckdb_file_handle` points to (DuckDB's `CFileHandle`): an open file (none once
/// closed) and the error of its last failed operation.
#[derive(Debug)]
pub struct CFileHandle {
    path: String,
    file: Option<File>,
    error: Option<Error>,
}

/// `duckdb_file_handle`
pub type DuckdbFileHandle = *mut CFileHandle;

/// The text `strerror` gives for an operating system error.
fn os_error_text(error: &std::io::Error) -> String {
    let Some(code) = error.raw_os_error() else {
        return error.to_string();
    };
    let mut buffer = [0 as c_char; 256];
    // SAFETY: `strerror_r` writes a NUL-terminated message of at most `buffer.len()` bytes.
    if unsafe { libc::strerror_r(code, buffer.as_mut_ptr(), buffer.len()) } != 0 {
        return error.to_string();
    }
    // SAFETY: on success the buffer holds a NUL-terminated string.
    unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// An IO error (DuckDB's `IOException`).
fn io_error(message: String) -> Error {
    Error::new(ErrorKind::Io, message)
}

impl CFileHandle {
    /// Runs `operation` on the file; a failure (a closed file fails like a closed file
    /// descriptor) becomes the IO error `message` makes of the path and the system's
    /// error text, recorded as the handle's error.
    fn run<T>(
        &mut self,
        operation: impl FnOnce(&mut File) -> std::io::Result<T>,
        message: impl FnOnce(&str, &str) -> String,
    ) -> Option<T> {
        let outcome = match self.file.as_mut() {
            Some(file) => operation(file),
            None => Err(std::io::Error::from_raw_os_error(libc::EBADF)),
        };
        match outcome {
            Ok(value) => Some(value),
            Err(error) => {
                self.error = Some(io_error(message(&self.path, &os_error_text(&error))));
                None
            }
        }
    }
}

/// `LocalFileSystem::OpenFile`: the file at `path` opened as `options` asks.
fn open_file(path: &str, options: CFileOpenOptions) -> Result<File, Error> {
    let path = rustdb_function::file_system::expand_path(path);
    if !options.read && !options.write {
        return Err(Error::internal(
            "READ, WRITE or both should be specified when opening a file",
        ));
    }
    let mut open = OpenOptions::new();
    open.read(options.read).write(options.write);
    if options.write {
        open.create(options.create).append(options.append);
    }
    if options.exclusive_create {
        open.create_new(true);
    }
    open.open(&path)
        .map_err(|error| io_error(format!("Cannot open file \"{path}\": {}", os_error_text(&error))))
}

/// `duckdb_file_system duckdb_client_context_get_file_system(duckdb_client_context context)`
/// (capi/file_system-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_client_context_get_file_system(
    context: DuckdbClientContext,
) -> DuckdbFileSystem {
    // SAFETY: `context` is NULL or a live handle.
    if unsafe { client_context_ref(context) }.is_none() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::default())
}

/// `void duckdb_destroy_file_system(duckdb_file_system *file_system)` (capi/file_system-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_file_system(file_system: *mut DuckdbFileSystem) {
    // SAFETY: `file_system` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = file_system.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `duckdb_file_open_options duckdb_create_file_open_options()` (capi/file_system-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_file_open_options() -> DuckdbFileOpenOptions {
    Box::into_raw(Box::default())
}

/// `duckdb_state duckdb_file_open_options_set_flag(duckdb_file_open_options options,
/// duckdb_file_flag flag, bool value)` (capi/file_system-c.cpp): a flag, once set, stays
/// set (`flags |= ...`, whatever `value` is).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_file_open_options_set_flag(
    options: DuckdbFileOpenOptions,
    flag: DuckdbFileFlag,
    _value: bool,
) -> DuckdbState {
    // SAFETY: `options` is NULL or a live handle.
    let Some(options) = (unsafe { options.as_mut() }) else {
        return DuckdbState::DuckDBError;
    };
    match flag {
        DUCKDB_FILE_FLAG_READ => options.read = true,
        DUCKDB_FILE_FLAG_WRITE => options.write = true,
        DUCKDB_FILE_FLAG_APPEND => options.append = true,
        DUCKDB_FILE_FLAG_CREATE => options.create = true,
        DUCKDB_FILE_FLAG_CREATE_NEW => options.exclusive_create = true,
        _ => return DuckdbState::DuckDBError,
    }
    DuckdbState::DuckDBSuccess
}

/// `void duckdb_destroy_file_open_options(duckdb_file_open_options *options)`
/// (capi/file_system-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_file_open_options(options: *mut DuckdbFileOpenOptions) {
    // SAFETY: `options` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = options.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `duckdb_state duckdb_file_system_open(duckdb_file_system file_system, const char *path,
/// duckdb_file_open_options options, duckdb_file_handle *out_file)`
/// (capi/file_system-c.cpp): a failure is kept as the file system's error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_file_system_open(
    file_system: DuckdbFileSystem,
    path: *const c_char,
    options: DuckdbFileOpenOptions,
    out_file: *mut DuckdbFileHandle,
) -> DuckdbState {
    // SAFETY: `file_system` is NULL or a live handle.
    let Some(file_system) = (unsafe { file_system.as_mut() }) else {
        if !out_file.is_null() {
            // SAFETY: checked non-null; writable per the API contract.
            unsafe { *out_file = std::ptr::null_mut() };
        }
        return DuckdbState::DuckDBError;
    };
    if out_file.is_null() {
        file_system.error = Some(io_error("Invalid out file to duckdb_file_system_open".to_owned()));
        return DuckdbState::DuckDBError;
    }
    // SAFETY: `path` is NULL or a C string; `options` NULL or a live handle.
    let (Some(path), Some(options)) = (unsafe { opt_str(path) }, unsafe { options.as_ref() }) else {
        file_system.error = Some(io_error("Invalid input to duckdb_file_system_open".to_owned()));
        // SAFETY: checked non-null; writable per the API contract.
        unsafe { *out_file = std::ptr::null_mut() };
        return DuckdbState::DuckDBError;
    };
    let handle = match open_file(path, *options) {
        Ok(file) => Box::into_raw(Box::new(CFileHandle {
            path: path.to_owned(),
            file: Some(file),
            error: None,
        })),
        Err(error) => {
            file_system.error = Some(error);
            std::ptr::null_mut()
        }
    };
    // SAFETY: checked non-null; writable per the API contract.
    unsafe { *out_file = handle };
    DuckdbState::from_ok(!handle.is_null())
}

/// `duckdb_error_data duckdb_file_system_error_data(duckdb_file_system file_system)`
/// (capi/file_system-c.cpp): the error of the last failed open, if any.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_file_system_error_data(file_system: DuckdbFileSystem) -> DuckdbErrorData {
    // SAFETY: `file_system` is NULL or a live handle.
    let error = unsafe { file_system.as_ref() }.and_then(|file_system| file_system.error.clone());
    ErrorDataWrapper::new(error).into_handle()
}

/// `void duckdb_destroy_file_handle(duckdb_file_handle *file)` (capi/file_system-c.cpp):
/// closes the file first.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_file_handle(file: *mut DuckdbFileHandle) {
    // SAFETY: `file` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = file.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `duckdb_error_data duckdb_file_handle_error_data(duckdb_file_handle file)`
/// (capi/file_system-c.cpp): the error of the handle's last failed operation, if any.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_file_handle_error_data(file: DuckdbFileHandle) -> DuckdbErrorData {
    // SAFETY: `file` is NULL or a live handle.
    let error = unsafe { file.as_ref() }.and_then(|file| file.error.clone());
    ErrorDataWrapper::new(error).into_handle()
}

/// `int64_t duckdb_file_handle_read(duckdb_file_handle file, void *buffer, int64_t size)`
/// (capi/file_system-c.cpp): one read at the file position (`read(2)`), which may return
/// fewer bytes; 0 at the end of the file, -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_file_handle_read(
    file: DuckdbFileHandle,
    buffer: *mut c_void,
    size: i64,
) -> i64 {
    // SAFETY: `file` is NULL or a live handle.
    let Some(handle) = (unsafe { file.as_mut() }) else {
        return -1;
    };
    if buffer.is_null() || size < 0 {
        return -1;
    }
    // SAFETY: `buffer` holds `size` writable bytes per the API contract.
    let buffer = unsafe { std::slice::from_raw_parts_mut(buffer.cast::<u8>(), size as usize) };
    handle
        .run(
            |file| file.read(buffer),
            |path, reason| format!("Could not read from file \"{path}\": {reason}"),
        )
        .map_or(-1, |read| read as i64)
}

/// `int64_t duckdb_file_handle_write(duckdb_file_handle file, const void *buffer,
/// int64_t size)` (capi/file_system-c.cpp): writes all `size` bytes at the file position;
/// -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_file_handle_write(
    file: DuckdbFileHandle,
    buffer: *const c_void,
    size: i64,
) -> i64 {
    // SAFETY: `file` is NULL or a live handle.
    let Some(handle) = (unsafe { file.as_mut() }) else {
        return -1;
    };
    if buffer.is_null() || size < 0 {
        return -1;
    }
    // SAFETY: `buffer` holds `size` readable bytes per the API contract.
    let buffer = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), size as usize) };
    handle
        .run(
            |file| file.write_all(buffer),
            |path, reason| format!("Could not write file \"{path}\": {reason}"),
        )
        .map_or(-1, |()| size)
}

/// `int64_t duckdb_file_handle_tell(duckdb_file_handle file)` (capi/file_system-c.cpp):
/// the file position, -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_file_handle_tell(file: DuckdbFileHandle) -> i64 {
    // SAFETY: `file` is NULL or a live handle.
    let Some(handle) = (unsafe { file.as_mut() }) else {
        return -1;
    };
    handle
        .run(Seek::stream_position, |path, reason| {
            format!("Could not get file position for file \"{path}\": {reason}")
        })
        .map_or(-1, |position| position as i64)
}

/// `int64_t duckdb_file_handle_size(duckdb_file_handle file)` (capi/file_system-c.cpp):
/// -1 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_file_handle_size(file: DuckdbFileHandle) -> i64 {
    // SAFETY: `file` is NULL or a live handle.
    let Some(handle) = (unsafe { file.as_mut() }) else {
        return -1;
    };
    handle
        .run(
            |file| file.metadata().map(|metadata| metadata.len()),
            |path, reason| format!("Failed to get file size for file \"{path}\": {reason}"),
        )
        .map_or(-1, |size| size as i64)
}

/// `duckdb_state duckdb_file_handle_seek(duckdb_file_handle file, int64_t position)`
/// (capi/file_system-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_file_handle_seek(file: DuckdbFileHandle, position: i64) -> DuckdbState {
    // SAFETY: `file` is NULL or a live handle.
    let Some(handle) = (unsafe { file.as_mut() }) else {
        return DuckdbState::DuckDBError;
    };
    if position < 0 {
        return DuckdbState::DuckDBError;
    }
    let sought = handle.run(
        |file| file.seek(SeekFrom::Start(position as u64)),
        |path, reason| format!("Could not seek to location {position} for file \"{path}\": {reason}"),
    );
    DuckdbState::from_ok(sought.is_some())
}

/// `duckdb_state duckdb_file_handle_sync(duckdb_file_handle file)` (capi/file_system-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_file_handle_sync(file: DuckdbFileHandle) -> DuckdbState {
    // SAFETY: `file` is NULL or a live handle.
    let Some(handle) = (unsafe { file.as_mut() }) else {
        return DuckdbState::DuckDBError;
    };
    let synced = handle.run(
        |file| file.sync_all(),
        |path, reason| format!("Could not fsync file \"{path}\": {reason}"),
    );
    DuckdbState::from_ok(synced.is_some())
}

/// `duckdb_state duckdb_file_handle_close(duckdb_file_handle file)` (capi/file_system-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_file_handle_close(file: DuckdbFileHandle) -> DuckdbState {
    // SAFETY: `file` is NULL or a live handle.
    let Some(handle) = (unsafe { file.as_mut() }) else {
        return DuckdbState::DuckDBError;
    };
    handle.file = None;
    DuckdbState::DuckDBSuccess
}
