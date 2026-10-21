//! DuckDB's C API (`duckdb.h`), exported with identical names, signatures and struct
//! layouts so existing C clients can link against this library instead of DuckDB.
//!
//! The modules follow DuckDB's `src/main/capi/*-c.cpp` files. Handles point to the
//! engine's objects (or to wrappers holding them, as in DuckDB); data chunks and vectors
//! use DuckDB's in-memory layout, so the pointers C callers get are the engine's buffers.
//! Every function validates its pointers and reports failures through the documented
//! `duckdb_state` / error channels; no Rust panic crosses the FFI boundary. Memory handed
//! to the caller comes from `malloc` (`duckdb_malloc`), as in DuckDB.
#![allow(unsafe_code)]
#![allow(clippy::missing_safety_doc)] // Safety contracts are those of duckdb.h.
#![allow(clippy::wildcard_imports)] // The modules share the `duckdb.h` vocabulary of `types`.

mod appender;
mod config;
mod data_chunk;
mod datetime;
mod error_data;
mod helper;
mod hugeint;
mod logical_types;
mod open;
mod pending;
mod prepared;
mod result;
mod table_description;
mod types;
mod value;
mod value_fetch;

pub use appender::*;
pub use config::*;
pub use data_chunk::*;
pub use datetime::*;
pub use error_data::*;
pub use helper::{
    duckdb_free, duckdb_malloc, duckdb_string_is_inlined, duckdb_string_t_data, duckdb_string_t_length,
    duckdb_valid_utf8_check, duckdb_vector_size,
};
pub use hugeint::*;
pub use logical_types::*;
pub use open::*;
pub use pending::*;
pub use prepared::*;
pub use result::*;
pub use table_description::*;
pub use types::*;
pub use value::*;
pub use value_fetch::*;
