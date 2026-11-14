//! roastedduck: an in-process analytical SQL database, a 1:1 Rust port of DuckDB 1.5.6.
//!
//! The public API mirrors DuckDB's C++ `DuckDB` / `Connection` / `MaterializedQueryResult`.
//!
//! ```
//! let db = roastedduck::Database::open_in_memory().unwrap();
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
mod extension_http;
mod extensions;
pub mod external_extensions;
mod fts;
mod function_lists;
mod function_registration;
mod log_functions;
mod logging;
mod platform;
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
mod vss;
mod wal;

pub use config::{AccessMode, Config, ConfigurationSnapshot, config_flags};
pub use database::{
    Appender, ClientHandle, ClientSessionSnapshot, ColumnDescription, Connection, Database, ExtensionLoad,
    MemoryStatus, PendingExecutionResult, PendingQuery, PreparedStatement, QueryId, QueryProgress,
    QueryResult, StreamQueryResult, TableDescription,
};
pub use logging::LogStorage;
pub use query_profiler::ProfilingTree;
pub use result::{MaterializedResult, ResultChunks};
pub use roastedduck_ast::{SqlStatement, StatementType};
pub use roastedduck_common::{Error, ErrorKind, Result};
pub use roastedduck_execution::operators::QueryResultMemoryType;
pub use roastedduck_function::logging::LogLevel;
pub use roastedduck_planner::StatementReturnType;
pub use roastedduck_types::{DataChunk, LogicalType, LogicalTypeId, Value, Vector};
