use std::sync::Arc;

use rustdb_catalog::{Catalog, TransactionManager};
use rustdb_common::{Error, Result};

use crate::transaction::TransactionContext;
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

/// Shared state of one database (DuckDB's `DatabaseInstance`).
#[derive(Debug)]
pub(crate) struct DatabaseInstance {
    path: String,
    config: Config,
    pub(crate) transactions: Arc<TransactionManager>,
    /// Builtin functions and system objects (`system.main`); read by the binder.
    #[allow(dead_code)]
    pub(crate) system_catalog: Arc<Catalog>,
    /// The database's own catalog (`memory` for in-memory databases).
    pub(crate) catalog: Arc<Catalog>,
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
        let system_catalog = Catalog::new("system")?;
        for set in rustdb_functions::builtin_scalar_functions() {
            system_catalog.register_scalar_functions(set)?;
        }
        let instance = DatabaseInstance {
            path,
            config: config.clone(),
            transactions: Arc::default(),
            system_catalog: Arc::new(system_catalog),
            catalog: Arc::new(Catalog::new("memory")?),
        };
        Ok(Self {
            instance: Arc::new(instance),
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
        let transactions = TransactionContext::new(Arc::clone(&self.instance.transactions));
        Ok(Connection {
            database: self.clone(),
            transactions,
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

    pub(crate) fn instance(&self) -> &DatabaseInstance {
        &self.instance
    }
}

/// A connection to a database (DuckDB's `Connection`). Each connection is used by one
/// thread at a time; open one connection per thread for concurrency.
#[derive(Debug)]
pub struct Connection {
    database: Database,
    transactions: TransactionContext,
}

impl Connection {
    pub fn database(&self) -> &Database {
        &self.database
    }

    /// Runs every statement in `sql` and returns the result of the last one.
    pub fn query(&mut self, sql: &str) -> Result<MaterializedResult> {
        let _ = (sql, self.database.instance().catalog.name());
        // The statement lifecycle runs inside a transaction even before parsing is wired.
        let _transaction = self.transactions.active()?;
        let outcome = Err(Error::not_implemented("query execution is not implemented yet"));
        self.transactions.end_statement(outcome.is_ok())?;
        outcome
    }
}
