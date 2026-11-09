//! Foundations shared by every roastedduck crate.
//!
//! The error model mirrors DuckDB's `ExceptionType` + `ErrorData` so that user-visible
//! error text is byte-identical to the reference engine.

extern crate self as roastedduck_common;

pub mod blob;
pub mod byte_format;
mod byte_string;
pub mod collections;
mod error;
pub mod glob;
mod index;
pub mod libcxx;
pub mod libcxx_sort;
pub mod libstdcxx;
pub mod libstdcxx_sort;
pub mod platform_order;
pub mod profiling;
mod query_context;
pub mod string_util;
pub mod tree_renderer;
pub mod utf8;

pub use byte_string::ByteString;
pub use collections::{CaseInsensitiveMap, CaseInsensitiveSet, OrderedMap};
pub use error::{Error, ErrorKind, Result, os_error_text};
pub use index::{LogicalIndex, PhysicalIndex};
pub use roastedduck_byte_format::{byte_format, byte_write};

/// DuckDB's `idx_t`: the index/count type used throughout the engine.
pub type Idx = u64;

/// DuckDB's `DConstants::INVALID_INDEX`.
pub const INVALID_INDEX: Idx = Idx::MAX;

/// Number of rows processed per vector, identical to DuckDB's `STANDARD_VECTOR_SIZE`.
pub const STANDARD_VECTOR_SIZE: usize = 2048;

/// Local file path at the native OS boundary. POSIX APIs receive `path.c_str()`.
pub fn native_file_path(bytes: &[u8]) -> Result<std::path::PathBuf> {
    let bytes = bytes.split(|byte| *byte == 0).next().unwrap_or_default();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        Ok(std::str::from_utf8(bytes)
            .map_err(|_| Error::invalid_input("Invalid UTF-8 in filesystem path"))?
            .into())
    }
}
