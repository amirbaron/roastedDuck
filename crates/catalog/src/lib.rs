//! The catalog (DuckDB's `Catalog`, `CatalogSet`, `CatalogEntry`).
//!
//! Every named object lives in a [`CatalogSet`] as a chain of versions. A transaction sees
//! the newest version it created itself or that was committed before it started, which
//! gives snapshot isolation for DDL exactly like DuckDB.

mod catalog;
mod catalog_set;
mod entry;
mod transaction;

pub use catalog::{Catalog, DEFAULT_SCHEMA};
pub use catalog_set::{CatalogSet, Lookup};
pub use entry::{CatalogEntry, CatalogType, EntryKind, SchemaEntry};
pub use transaction::{MAX_TRANSACTION_ID, TRANSACTION_ID_START, Transaction, TransactionManager};
