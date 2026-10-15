//! The engines under test, behind one small interface so that both are driven by exactly
//! the same benchmark logic ([`crate::driver`]).

mod cpp;
mod rust;

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub use cpp::CppEngine;
pub use rust::RustEngine;

use crate::verify::ResultTable;

/// Which engine a run measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum EngineKind {
    /// C++ DuckDB 1.5.6, loaded from the reference `libduckdb` by the bench driver.
    Cpp,
    /// rustdb, linked into the bench driver.
    Rust,
    /// C++ DuckDB through its own `benchmark_runner` (fixed 5 runs; cross-check only).
    CppRunner,
}

impl EngineKind {
    /// The name used in records and file names.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpp => "cpp",
            Self::Rust => "rust",
            Self::CppRunner => "cpp-runner",
        }
    }

    /// Engines that read and write DuckDB's file format share one benchmark root (and
    /// thus cached databases).
    pub fn root_name(self) -> &'static str {
        match self {
            Self::Cpp | Self::CppRunner => "duckdb",
            Self::Rust => "rustdb",
        }
    }
}

impl fmt::Display for EngineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Database settings applied when a database is opened.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbConfig {
    /// `threads`.
    pub threads: u32,
    /// `memory_limit`, e.g. `8GB`.
    pub memory_limit: Option<String>,
    /// `storage_compatibility_version` (the benchmark's `storage persistent <version>`).
    pub storage_version: Option<String>,
}

impl DbConfig {
    /// The settings as `(name, value)` pairs.
    pub fn options(&self) -> Vec<(&'static str, String)> {
        let mut options = vec![("threads", self.threads.to_string())];
        if let Some(limit) = &self.memory_limit {
            options.push(("memory_limit", limit.clone()));
        }
        if let Some(version) = &self.storage_version {
            options.push(("storage_compatibility_version", version.clone()));
        }
        options
    }
}

/// Why an engine call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineErrorKind {
    /// The engine does not implement the feature yet (`Not implemented Error`).
    Unsupported,
    /// Any other error.
    Failed,
}

/// An error from an engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineError {
    pub kind: EngineErrorKind,
    pub message: String,
}

impl EngineError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            kind: EngineErrorKind::Failed,
            message: message.into(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            kind: EngineErrorKind::Unsupported,
            message: message.into(),
        }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EngineError {}

/// Outcome of loading an extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionStatus {
    Loaded,
    /// Known, but not available in this build.
    NotAvailable,
    Unknown,
}

/// A query result held by the engine.
pub trait QueryResult {
    /// Number of rows.
    fn row_count(&self) -> usize;
    /// Number of columns.
    fn column_count(&self) -> usize;
    /// Renders the whole result to strings.
    fn to_table(&self) -> Result<ResultTable, EngineError>;
}

/// One database engine with at most one open database and connection.
pub trait Engine {
    type Result: QueryResult;

    /// A human-readable engine version.
    fn version(&self) -> String;

    /// Opens a database (closing any open one); `None` is an in-memory database.
    fn open(&mut self, path: Option<&Path>, config: &DbConfig) -> Result<(), EngineError>;

    /// Closes the open database, if any.
    fn close(&mut self);

    /// Runs every statement in `sql`; returns the result like DuckDB's
    /// `Connection::Query` (the first statement that produces a result).
    fn query(&mut self, sql: &str) -> Result<Self::Result, EngineError>;

    /// Loads an extension into the open database.
    fn load_extension(&mut self, name: &str) -> Result<ExtensionStatus, EngineError>;
}
