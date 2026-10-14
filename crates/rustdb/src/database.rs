use std::sync::Arc;

use rustdb_common::{Error, Result};

use crate::{Config, MaterializedResult};

/// Outcome of loading an extension by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionLoad {
    Loaded,
    /// A known DuckDB extension that this build does not provide.
    NotAvailable,
    /// Not a known extension name.
    Unknown,
}

#[derive(Debug)]
struct DatabaseInstance {
    path: String,
    config: Config,
}

/// A database instance (DuckDB's `DuckDB`). Cheap to clone; clones share the instance.
#[derive(Clone, Debug)]
pub struct Database {
    instance: Arc<DatabaseInstance>,
}

impl Database {
    /// Opens (or creates) the database at `path`; `""` or `":memory:"` is in-memory.
    pub fn open(path: &str, config: &Config) -> Result<Self> {
        let path = if path == ":memory:" {
            String::new()
        } else {
            path.to_owned()
        };
        if !path.is_empty() {
            return Err(Error::not_implemented(
                "persistent databases are not supported yet",
            ));
        }
        Ok(Self {
            instance: Arc::new(DatabaseInstance {
                path,
                config: config.clone(),
            }),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::open("", &Config::default())
    }

    /// The database file path, or `""` for in-memory databases.
    pub fn path(&self) -> &str {
        &self.instance.path
    }

    pub fn config(&self) -> &Config {
        &self.instance.config
    }

    pub fn connect(&self) -> Result<Connection> {
        Ok(Connection {
            database: self.clone(),
        })
    }

    /// Loads a (statically linked) extension, like `LOAD name`.
    pub fn load_extension(&self, name: &str) -> ExtensionLoad {
        match name {
            "core_functions" | "parquet" | "json" | "icu" | "tpch" | "tpcds" | "autocomplete" | "httpfs"
            | "fts" | "inet" | "excel" | "sqlsmith" | "spatial" | "delta" | "iceberg" | "aws" | "azure"
            | "motherduck" | "postgres_scanner" | "mysql_scanner" | "sqlite_scanner" | "vss" | "ui"
            | "encodings" | "ducklake" | "avro" | "demo_capi" => ExtensionLoad::NotAvailable,
            _ => ExtensionLoad::Unknown,
        }
    }
}

/// A connection to a database (DuckDB's `Connection`). Each connection is used by one
/// thread at a time; open one connection per thread for concurrency.
#[derive(Debug)]
pub struct Connection {
    database: Database,
}

impl Connection {
    pub fn database(&self) -> &Database {
        &self.database
    }

    /// Runs every statement in `sql` and returns the result of the last one.
    pub fn query(&mut self, sql: &str) -> Result<MaterializedResult> {
        let _ = sql;
        Err(Error::not_implemented("query execution is not implemented yet"))
    }
}
