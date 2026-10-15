//! Foundations shared by every rustdb crate.
//!
//! The error model mirrors DuckDB's `ExceptionType` + `ErrorData` so that user-visible
//! error text is byte-identical to the reference engine.

pub mod blob;
pub mod collections;
mod error;
mod index;
mod query_context;
pub mod string_util;

pub use collections::{CaseInsensitiveMap, CaseInsensitiveSet, OrderedMap};
pub use error::{Error, ErrorKind, Result};
pub use index::{LogicalIndex, PhysicalIndex};

/// DuckDB's `idx_t`: the index/count type used throughout the engine.
pub type Idx = u64;

/// DuckDB's `DConstants::INVALID_INDEX`.
pub const INVALID_INDEX: Idx = Idx::MAX;

/// Number of rows processed per vector, identical to DuckDB's `STANDARD_VECTOR_SIZE`.
pub const STANDARD_VECTOR_SIZE: usize = 2048;
