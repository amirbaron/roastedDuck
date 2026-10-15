//! Transactions are owned by the storage layer (`DuckTransaction`); the catalog records
//! its version changes in the transaction's undo log so that they commit, roll back and
//! are logged in order with the data changes.

use std::sync::Arc;

use rustdb_storage::CatalogUndo;
pub use rustdb_storage::transaction::{
    MAX_TRANSACTION_ID, TRANSACTION_ID_START, Transaction, TransactionManager,
};

use crate::catalog_set::{CatalogSet, Version};

/// A created or dropped catalog entry version, as an undo-log entry.
#[derive(Debug)]
struct CatalogChange {
    set: Arc<CatalogSet>,
    name: String,
    version: Arc<Version>,
}

impl CatalogUndo for CatalogChange {
    fn commit(&self, commit_id: u64) {
        self.version.commit(commit_id);
    }

    fn revert_commit(&self, transaction_id: u64) {
        self.version.commit(transaction_id);
    }

    fn rollback(&self) {
        self.set.undo(&self.name, &self.version);
    }

    fn is_drop(&self) -> bool {
        self.version.is_tombstone()
    }
}

/// Registers catalog changes in a transaction's undo log.
pub(crate) trait TransactionExt {
    fn push_catalog_change(&self, set: Arc<CatalogSet>, name: String, version: Arc<Version>);
}

impl TransactionExt for Transaction {
    fn push_catalog_change(&self, set: Arc<CatalogSet>, name: String, version: Arc<Version>) {
        self.push_catalog_undo(Box::new(CatalogChange { set, name, version }));
    }
}
