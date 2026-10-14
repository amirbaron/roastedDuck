use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::Result;

use crate::catalog_set::{CatalogSet, Version};

/// Transaction ids start here; anything below is a commit timestamp (DuckDB's
/// `TRANSACTION_ID_START`).
pub const TRANSACTION_ID_START: u64 = 4_611_686_018_427_388_000;
/// Larger than every transaction id (DuckDB's `MAX_TRANSACTION_ID`).
pub const MAX_TRANSACTION_ID: u64 = u64::MAX;

/// A catalog change to finalise on commit or revert on rollback.
#[derive(Debug)]
struct CatalogUndo {
    set: Arc<CatalogSet>,
    name: String,
    version: Arc<Version>,
}

/// A running transaction's identity and its undo log.
#[derive(Debug)]
pub struct Transaction {
    /// Commit timestamps below this are visible.
    pub start_time: u64,
    /// Versions written by this transaction carry this id until commit.
    pub transaction_id: u64,
    catalog_undo: Mutex<Vec<CatalogUndo>>,
}

impl Transaction {
    /// A read-only view used for internal lookups (sees every committed version).
    pub fn system() -> Self {
        Self {
            start_time: TRANSACTION_ID_START - 1,
            transaction_id: MAX_TRANSACTION_ID,
            catalog_undo: Mutex::default(),
        }
    }

    pub(crate) fn push_catalog_change(&self, set: Arc<CatalogSet>, name: String, version: Arc<Version>) {
        self.catalog_undo
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(CatalogUndo { set, name, version });
    }

    fn take_undo(&self) -> Vec<CatalogUndo> {
        std::mem::take(&mut *self.catalog_undo.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Hands out start timestamps and transaction ids, and commits/rolls back transactions
/// (DuckDB's `DuckTransactionManager`, catalog part).
#[derive(Debug)]
pub struct TransactionManager {
    current_timestamp: AtomicU64,
    next_transaction_id: AtomicU64,
    /// Serialises commits so commit timestamps are assigned in order.
    commit_lock: Mutex<()>,
}

impl Default for TransactionManager {
    fn default() -> Self {
        Self {
            current_timestamp: AtomicU64::new(2),
            next_transaction_id: AtomicU64::new(TRANSACTION_ID_START),
            commit_lock: Mutex::new(()),
        }
    }
}

impl TransactionManager {
    pub fn begin(&self) -> Transaction {
        Transaction {
            start_time: self.current_timestamp.load(Ordering::SeqCst),
            transaction_id: self.next_transaction_id.fetch_add(1, Ordering::SeqCst),
            catalog_undo: Mutex::default(),
        }
    }

    /// Makes the transaction's changes visible to transactions that start afterwards.
    pub fn commit(&self, transaction: &Transaction) -> Result<()> {
        let _guard = self.commit_lock.lock().unwrap_or_else(PoisonError::into_inner);
        let commit_id = self.current_timestamp.fetch_add(1, Ordering::SeqCst);
        for change in transaction.take_undo() {
            change.version.commit(commit_id);
        }
        Ok(())
    }

    /// Discards the transaction's changes, newest first.
    pub fn rollback(&self, transaction: &Transaction) {
        for change in transaction.take_undo().into_iter().rev() {
            change.set.undo(&change.name, &change.version);
        }
    }
}
