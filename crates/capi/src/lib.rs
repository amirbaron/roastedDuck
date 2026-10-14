//! DuckDB's C API (`duckdb.h`), exported with identical names, signatures and struct
//! layouts so existing C clients can link against this library instead of DuckDB.
//!
//! Every function validates its pointers and reports failures through the documented
//! `duckdb_state` / error-string channels; no Rust panic crosses the FFI boundary.
#![allow(unsafe_code)]
#![allow(clippy::missing_safety_doc)] // Safety contracts are those of duckdb.h.

mod memory;
mod open;
mod result;
mod types;

pub use memory::*;
pub use open::*;
pub use result::*;
pub use types::*;
