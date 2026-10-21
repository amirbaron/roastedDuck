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

mod attached;
mod checkpoint;
mod config;
mod database;
mod default_functions;
mod default_settings;
mod default_views;
mod extensions;
mod log_functions;
mod logging;
mod query_profiler;
mod read_duckdb;
mod read_file;
mod result;
mod secrets;
mod sequence;
mod settings;
mod storage;
mod switch;
mod system_functions;
mod temp_catalog;
mod transaction;
mod wal;

pub use config::{AccessMode, Config, config_flags};
pub use database::{
    Appender, ClientHandle, ColumnDescription, Connection, Database, ExtensionLoad, PendingExecutionResult,
    PendingQuery, PreparedStatement, QueryId, QueryProgress, QueryResult, StreamQueryResult,
    TableDescription,
};
pub use logging::LogStorage;
pub use result::MaterializedResult;
pub use rustdb_ast::{SqlStatement, StatementType};
pub use rustdb_common::{Error, ErrorKind, Result};
pub use rustdb_planner::StatementReturnType;
pub use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value};
