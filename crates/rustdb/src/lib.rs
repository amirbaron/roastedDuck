//! rustdb: an in-process analytical SQL database, a 1:1 Rust port of DuckDB 1.5.6.
//!
//! The public API mirrors DuckDB's C++ `DuckDB` / `Connection` / `MaterializedQueryResult`.
//!
//! ```
//! let db = rustdb::Database::open_in_memory().unwrap();
//! let mut con = db.connect().unwrap();
//! let result = con.query("SELECT 42").unwrap();
//! assert_eq!(result.value(0, 0).to_varchar().unwrap(), "42");
//! ```

mod checkpoint;
mod config;
mod database;
mod result;
mod settings;
mod transaction;

pub use config::{AccessMode, Config};
pub use database::{Connection, Database, ExtensionLoad};
pub use result::MaterializedResult;
pub use rustdb_common::{Error, ErrorKind, Result};
pub use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value};
