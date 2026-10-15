//! The catalog (DuckDB's `Catalog`, `CatalogSet`, `CatalogEntry`).
//!
//! Every named object lives in a [`CatalogSet`] as a chain of versions. A transaction sees
//! the newest version it created itself or that was committed before it started, which
//! gives snapshot isolation for DDL exactly like DuckDB.

mod catalog;
mod catalog_set;
pub mod checkpoint;
mod entry;
mod transaction;
mod wal;

pub use catalog::{Catalog, DEFAULT_SCHEMA};
pub use catalog_set::{CatalogSet, Lookup};
pub use entry::{
    CatalogEntry, CatalogType, EntryKind, IndexEntry, SchemaEntry, SequenceData, SequenceEntry, TableColumn,
    TableEntry, ViewEntry,
};
pub use transaction::{
    MAX_TRANSACTION_ID, StorageHook, TRANSACTION_ID_START, Transaction, TransactionManager,
};
pub use wal::WalRecord;
