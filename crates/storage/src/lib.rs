//! The storage engine (DuckDB's `src/storage`, the storage half of `src/transaction` and
//! `src/execution/index`).
//!
//! # Structure
//! - [`DataTable`] stores a table as a [`RowGroupCollection`](table::RowGroupCollection):
//!   row groups of up to 122,880 rows, each holding one [`ColumnData`](table::ColumnData)
//!   per column. Column data is a list of column segments on buffer-managed blocks; nested
//!   types are split into validity, offset and child columns exactly like DuckDB.
//! - MVCC: every row group has a [`RowVersionManager`](table::RowVersionManager) with the
//!   insert and delete ids of its rows; updates keep the newest values next to the column
//!   and the overwritten ones in per-transaction undo nodes
//!   ([`UpdateSegment`](table::UpdateSegment)).
//! - [`Transaction`] and [`TransactionManager`] (DuckDB's `DuckTransaction` and
//!   `DuckTransactionManager`) own the undo log and the transaction-local storage, and
//!   commit, roll back and clean up versions. The catalog registers its own changes in the
//!   same undo log through [`CatalogUndo`].
//! - Statistics ([`statistics`]) double as zone maps for [`TableFilter`]s pushed into scans.
//!
//! # Concurrency
//! Tables are shared between threads (`Arc<DataTable>`). Appends to the base table are
//! serialised by the table's append lock; scans run concurrently with appends and read
//! only rows below a column's published count, which appends never touch again.

pub mod compression;
pub mod index;
pub mod statistics;
pub mod table;
pub mod table_filter;
pub mod transaction;

mod data_table;

pub use data_table::{
    ColumnDefinition, ConstraintVerifier, DataTable, DataTableInfo, LocalAppendState, TableConstraints,
    TableIoManager,
};
pub use table::{ParallelTableScanState, StorageIndex, TableScanState};
pub use table_filter::{TableFilter, TableFilterSet};
pub use transaction::{
    CatalogUndo, MAX_TRANSACTION_ID, NOT_DELETED_ID, TRANSACTION_ID_START, Transaction, TransactionData,
    TransactionManager,
};

/// Row ids at or above this belong to transaction-local storage (DuckDB's `MAX_ROW_ID`).
pub const MAX_ROW_ID: u64 = 36_028_797_018_960_000;

/// The number of rows in a full row group (DuckDB's `DEFAULT_ROW_GROUP_SIZE`).
pub const DEFAULT_ROW_GROUP_SIZE: u64 = 122_880;

/// The column id of the `rowid` pseudo column (DuckDB's `COLUMN_IDENTIFIER_ROW_ID`).
pub const COLUMN_IDENTIFIER_ROW_ID: u64 = u64::MAX;

/// `STANDARD_VECTOR_SIZE` as a `u64`, for row arithmetic.
pub(crate) const VECTOR_SIZE: u64 = rustdb_common::STANDARD_VECTOR_SIZE as u64;
