//! Foundations shared by every rustdb crate.
//!
//! The error model mirrors DuckDB's `ExceptionType` + `ErrorData` so that user-visible
//! error text is byte-identical to the reference engine.

mod error;

pub use error::{Error, ErrorKind, Result};

/// DuckDB's `idx_t`: the index/count type used throughout the engine.
pub type Idx = u64;

/// Number of rows processed per vector, identical to DuckDB's `STANDARD_VECTOR_SIZE`.
pub const STANDARD_VECTOR_SIZE: usize = 2048;
