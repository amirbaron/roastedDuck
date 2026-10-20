//! Transactions (DuckDB's `DuckTransaction` and `DuckTransactionManager`).
//!
//! Start times and commit ids come from one counter, so a version is visible to a
//! transaction if it was committed before the transaction started (`id < start_time`) or
//! written by the transaction itself (`id == transaction_id`). Uncommitted versions carry
//! transaction ids, which are above every timestamp.
//!
//! A commit first flushes the transaction-local storage into the tables (which can fail,
//! e.g. on a constraint violation, and then rolls back), then stamps every undo entry
//! with the commit id. Committed undo logs are kept until no running transaction can
//! need the versions they replaced, and are then cleaned up.

mod local_storage;
mod undo;

pub use local_storage::{LocalStorage, LocalTableStorage};
pub use undo::{CatalogUndo, UndoProperties};
pub(crate) use undo::{SequenceValue, UndoBuffer, UndoEntry};

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_common::profiling::{MetricType, start_timer};
use rustdb_common::{Error, Result};

use crate::data_table::DataTable;
use crate::table::{DeleteRecord, UpdateRecord};
use crate::wal::WriteAheadLog;

/// Transaction ids start here; anything below is a timestamp (`TRANSACTION_ID_START`).
pub const TRANSACTION_ID_START: u64 = 4_611_686_018_427_388_000;
/// Larger than every transaction id (`MAX_TRANSACTION_ID`).
pub const MAX_TRANSACTION_ID: u64 = u64::MAX;
/// The delete id of rows that are not deleted (`NOT_DELETED_ID`).
pub const NOT_DELETED_ID: u64 = u64::MAX - 1;

/// The identity of a transaction as seen by version checks (DuckDB's
/// `TransactionData`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TransactionData {
    pub transaction_id: u64,
    pub start_time: u64,
}

impl TransactionData {
    pub const fn new(transaction_id: u64, start_time: u64) -> Self {
        Self {
            transaction_id,
            start_time,
        }
    }

    /// Sees every committed version and nothing uncommitted.
    pub const fn latest_committed() -> Self {
        Self::new(MAX_TRANSACTION_ID, TRANSACTION_ID_START - 1)
    }

    /// The identity used for transaction-local storage: deletes and updates there are
    /// final immediately.
    pub const fn local() -> Self {
        Self::new(0, 0)
    }
}

/// A running transaction: its identity, undo log and transaction-local storage.
#[derive(Debug)]
pub struct Transaction {
    /// Commit timestamps below this are visible.
    pub start_time: u64,
    /// Versions written by this transaction carry this id until commit.
    pub transaction_id: u64,
    commit_id: AtomicU64,
    undo: Mutex<UndoBuffer>,
    local_storage: Arc<LocalStorage>,
    /// The attached database the transaction writes to, and its WAL
    /// (`MetaTransaction::modified_database`).
    modified_database: Mutex<Option<(String, Option<Arc<WriteAheadLog>>)>>,
    /// The shared hold of the written database's checkpoint lock, until the transaction
    /// ends (`DuckTransaction::write_lock`).
    checkpoint_guard: Mutex<Option<crate::checkpoint_lock::SharedCheckpointGuard>>,
    /// The manager's oldest running transaction, for transactions it started.
    lowest_active: Option<Arc<LowestActive>>,
}

impl Transaction {
    fn new(start_time: u64, transaction_id: u64) -> Self {
        Self {
            start_time,
            transaction_id,
            commit_id: AtomicU64::new(0),
            undo: Mutex::default(),
            local_storage: Arc::default(),
            modified_database: Mutex::new(None),
            checkpoint_guard: Mutex::new(None),
            lowest_active: None,
        }
    }

    /// The versions any running transaction may still see (DuckDB's
    /// `TransactionData(LowestActiveId(), LowestActiveStart())` of
    /// `TABLE_SCAN_OMIT_PERMANENTLY_DELETED`); the latest committed rows without a
    /// transaction manager.
    pub fn oldest_active_snapshot(&self) -> TransactionData {
        match &self.lowest_active {
            Some(lowest) => TransactionData::new(
                lowest.id.load(Ordering::Acquire),
                lowest.start.load(Ordering::Acquire),
            ),
            None => TransactionData::latest_committed(),
        }
    }

    /// Releases the checkpoint lock when the transaction ends.
    fn release_checkpoint_lock(&self) {
        self.checkpoint_guard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }

    /// Records that the transaction writes to the database `name`, whose commits are
    /// logged to `wal` (`MetaTransaction::ModifyDatabase`). A transaction writes to one
    /// attached database only.
    /// With `checkpoint_lock` (updates and catalog changes, DuckDB's
    /// `SetModifications`), the database's checkpoint lock is held shared until the
    /// transaction ends.
    pub fn modify_database(
        &self,
        name: &str,
        wal: Option<&Arc<WriteAheadLog>>,
        checkpoint_lock: bool,
    ) -> Result<()> {
        let mut modified = self
            .modified_database
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match &*modified {
            None => {
                *modified = Some((name.to_owned(), wal.cloned()));
            }
            Some((current, _)) if current.eq_ignore_ascii_case(name) => {}
            Some((current, _)) => Err(Error::new(
                rustdb_common::ErrorKind::Transaction,
                format!(
                    "Attempting to write to database \"{name}\" in a transaction that has already modified database \"{current}\" - a single transaction can only write to a single attached database."
                ),
            ))?,
        }
        drop(modified);
        if checkpoint_lock && let Some(wal) = wal {
            let mut guard = self
                .checkpoint_guard
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if guard.is_none() {
                // Waits while a checkpoint replaces the table data.
                *guard = Some(wal.checkpoint_lock().shared());
            }
        }
        Ok(())
    }

    /// The database the transaction writes to (`MetaTransaction::ModifiedDatabase`).
    pub fn modified_database(&self) -> Option<String> {
        self.modified_database
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(name, _)| name.clone())
    }

    /// The WAL the transaction's commit is logged to.
    fn wal(&self) -> Option<Arc<WriteAheadLog>> {
        self.modified_database
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|(_, wal)| wal.clone())
    }

    /// A read-only view used for internal lookups: sees every committed version.
    pub fn system() -> Self {
        Self::new(TRANSACTION_ID_START - 1, MAX_TRANSACTION_ID)
    }

    /// The transaction's identity for version checks.
    pub fn data(&self) -> TransactionData {
        TransactionData::new(self.transaction_id, self.start_time)
    }

    /// The commit id once committed, 0 before.
    pub fn commit_id(&self) -> u64 {
        self.commit_id.load(Ordering::Acquire)
    }

    fn undo(&self) -> MutexGuard<'_, UndoBuffer> {
        self.undo.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a catalog change in the undo log (`PushCatalogEntry`).
    pub fn push_catalog_undo(&self, change: Box<dyn CatalogUndo>) {
        self.undo().push(UndoEntry::Catalog(change));
    }

    pub(crate) fn push_append(&self, table: Arc<DataTable>, start_row: u64, count: u64) {
        self.undo().push(UndoEntry::Append {
            table,
            start_row,
            count,
        });
    }

    pub(crate) fn push_deletes(&self, table: &Arc<DataTable>, records: Vec<DeleteRecord>) {
        let mut undo = self.undo();
        for record in records {
            undo.push(UndoEntry::Delete {
                table: Arc::clone(table),
                record,
            });
        }
    }

    pub(crate) fn push_updates(&self, table: &Arc<DataTable>, records: Vec<UpdateRecord>) {
        let mut undo = self.undo();
        for record in records {
            undo.push(UndoEntry::Update {
                table: Arc::clone(table),
                column_index: record.column_index,
                registration: record.registration,
            });
        }
    }

    /// Records that the transaction took a value from the sequence identified by `key`
    /// (any address unique to the sequence while it exists); the sequence's state after
    /// the transaction is logged at commit (`DuckTransaction::PushSequenceUsage`).
    pub fn push_sequence_usage(&self, key: usize, schema: &str, name: &str, usage_count: u64, counter: i64) {
        self.undo().push_sequence_value(
            key,
            SequenceValue {
                schema: schema.to_owned(),
                name: name.to_owned(),
                usage_count,
                counter,
            },
        );
    }

    /// The transaction-local storage (DuckDB's `LocalStorage`).
    pub fn local_storage(&self) -> &LocalStorage {
        &self.local_storage
    }

    /// A shared handle to the transaction-local storage.
    pub(crate) fn local_storage_arc(&self) -> Arc<LocalStorage> {
        Arc::clone(&self.local_storage)
    }

    /// Whether the transaction changed anything (`ChangesMade`).
    pub fn changes_made(&self) -> bool {
        !self.undo().is_empty() || self.local_storage.changes_made()
    }

    /// What the undo log contains.
    pub fn undo_properties(&self) -> UndoProperties {
        self.undo().properties()
    }
}

/// A committed undo log waiting until no transaction needs the versions it replaced.
#[derive(Debug)]
struct CommittedUndo {
    commit_id: u64,
    undo: UndoBuffer,
}

#[derive(Debug)]
struct ManagerState {
    /// The next start timestamp / commit id.
    current_start_timestamp: u64,
    current_transaction_id: u64,
    /// `(transaction_id, start_time)` of every running transaction.
    active: Vec<(u64, u64)>,
    /// Ordered by commit id.
    recently_committed: VecDeque<CommittedUndo>,
}

/// Hands out timestamps and transaction ids, and commits, rolls back and cleans up
/// transactions (DuckDB's `DuckTransactionManager`).
#[derive(Debug)]
pub struct TransactionManager {
    state: Mutex<ManagerState>,
    lowest_active: Arc<LowestActive>,
    last_commit: AtomicU64,
}

/// The start time and id of the oldest running transaction, shared with the
/// transactions so that scans can see what any running transaction may still see.
#[derive(Debug)]
struct LowestActive {
    start: AtomicU64,
    id: AtomicU64,
}

impl Default for TransactionManager {
    fn default() -> Self {
        Self {
            state: Mutex::new(ManagerState {
                current_start_timestamp: 2,
                current_transaction_id: TRANSACTION_ID_START,
                active: Vec::new(),
                recently_committed: VecDeque::new(),
            }),
            lowest_active: Arc::new(LowestActive {
                start: AtomicU64::new(MAX_TRANSACTION_ID),
                id: AtomicU64::new(TRANSACTION_ID_START),
            }),
            last_commit: AtomicU64::new(0),
        }
    }
}

impl TransactionManager {
    fn lock(&self) -> MutexGuard<'_, ManagerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Logs the undo log of `transaction` and stamps it with `commit_id`, holding the
    /// WAL lock so that a checkpoint sees either both or neither
    /// (`DuckTransaction::WriteToWAL` + `UndoBuffer::Commit`). If stamping fails, the
    /// logged entries are cut off again (`RevertCommit`).
    fn log_and_commit(transaction: &Transaction, commit_id: u64, others_active: bool) -> Result<()> {
        transaction.undo().verify_commit(transaction.start_time)?;
        let Some(wal) = transaction.wal() else {
            transaction.local_storage.commit(transaction)?;
            return transaction
                .undo()
                .commit(commit_id, transaction.transaction_id, others_active);
        };
        // The transaction-local rows reach the table under the WAL lock, so a checkpoint
        // (which holds it) never sees appended rows whose commit is not logged yet.
        let mut guard = wal.lock();
        let mut commit_timer = start_timer(MetricType::CommitLocalStorageLatency);
        transaction.local_storage.commit(transaction)?;
        commit_timer.end();
        let undo = transaction.undo();
        if undo.is_empty() {
            return undo.commit(commit_id, transaction.transaction_id, others_active);
        }
        let initial_size = guard.size();
        let mut wal_timer = start_timer(MetricType::WriteToWalLatency);
        let mut batch = wal.batch();
        undo.write_to_wal(&mut batch, transaction.data())?;
        guard.commit(batch)?;
        wal_timer.end();
        let result = undo.commit(commit_id, transaction.transaction_id, others_active);
        if result.is_err() {
            let _ = guard.truncate(initial_size);
        }
        result
    }

    /// Starts a transaction (`StartTransaction`).
    pub fn begin(&self) -> Transaction {
        let mut state = self.lock();
        let start_time = state.current_start_timestamp;
        state.current_start_timestamp += 1;
        let transaction_id = state.current_transaction_id;
        state.current_transaction_id += 1;
        if state.active.is_empty() {
            self.lowest_active.start.store(start_time, Ordering::Release);
            self.lowest_active.id.store(transaction_id, Ordering::Release);
        }
        state.active.push((transaction_id, start_time));
        let mut transaction = Transaction::new(start_time, transaction_id);
        transaction.lowest_active = Some(Arc::clone(&self.lowest_active));
        transaction
    }

    /// The start time of the oldest running transaction (`LowestActiveStart`).
    pub fn lowest_active_start(&self) -> u64 {
        self.lowest_active.start.load(Ordering::Acquire)
    }

    /// The id of the oldest running transaction (`LowestActiveId`).
    pub fn lowest_active_id(&self) -> u64 {
        self.lowest_active.id.load(Ordering::Acquire)
    }

    /// The commit id of the last successful commit.
    pub fn last_commit(&self) -> u64 {
        self.last_commit.load(Ordering::Acquire)
    }

    /// Number of running transactions.
    pub fn active_count(&self) -> usize {
        self.lock().active.len()
    }

    /// Commits `transaction` (`CommitTransaction`). If flushing its local storage or
    /// stamping its versions fails, the transaction is rolled back and the error returned.
    pub fn commit(&self, transaction: &Transaction) -> Result<()> {
        let mut state = self.lock();
        let commit_id = state.current_start_timestamp;
        state.current_start_timestamp += 1;
        transaction.commit_id.store(commit_id, Ordering::Release);
        // Deleted rows stay in a side index while other transactions may still see them.
        let others_active = state.active.len() > 1;
        let result = Self::log_and_commit(transaction, commit_id, others_active);
        let undo = std::mem::take(&mut *transaction.undo());
        let outcome = match result {
            Ok(()) => {
                self.last_commit.store(commit_id, Ordering::Release);
                if !undo.is_empty() {
                    state
                        .recently_committed
                        .push_back(CommittedUndo { commit_id, undo });
                }
                Ok(())
            }
            Err(error) => {
                transaction.commit_id.store(0, Ordering::Release);
                transaction.local_storage.rollback();
                if let Err(rollback_error) = undo.rollback() {
                    return Err(Error::new(
                        rustdb_common::ErrorKind::Fatal,
                        format!(
                            "Failed to rollback transaction. Cannot continue operation.\nOriginal Error: {}\nRollback Error: {}",
                            error.raw_message(),
                            rollback_error.raw_message()
                        ),
                    ));
                }
                Err(error)
            }
        };
        let cleanup = self.remove_transaction(&mut state, transaction.transaction_id);
        drop(state);
        transaction.release_checkpoint_lock();
        for committed in cleanup {
            committed.undo.cleanup(committed.lowest_active_start);
        }
        outcome
    }

    /// Discards everything `transaction` did (`RollbackTransaction`).
    pub fn rollback(&self, transaction: &Transaction) {
        let mut state = self.lock();
        transaction.local_storage.rollback();
        let undo = std::mem::take(&mut *transaction.undo());
        // Rolling back only fails on internal errors; the versions are then left as they
        // are, invisible to everyone.
        let _ = undo.rollback();
        let cleanup = self.remove_transaction(&mut state, transaction.transaction_id);
        drop(state);
        transaction.release_checkpoint_lock();
        for committed in cleanup {
            committed.undo.cleanup(committed.lowest_active_start);
        }
    }

    /// Removes a finished transaction from the active list and collects the committed
    /// undo logs no running transaction needs any more (`RemoveTransaction`).
    fn remove_transaction(&self, state: &mut ManagerState, transaction_id: u64) -> Vec<CleanupTask> {
        state.active.retain(|&(id, _)| id != transaction_id);
        let lowest_start = state
            .active
            .iter()
            .map(|&(_, start)| start)
            .min()
            .unwrap_or(TRANSACTION_ID_START);
        let lowest_id = state
            .active
            .iter()
            .map(|&(id, _)| id)
            .min()
            .unwrap_or(MAX_TRANSACTION_ID);
        self.lowest_active.start.store(lowest_start, Ordering::Release);
        self.lowest_active.id.store(lowest_id, Ordering::Release);
        let mut cleanup = Vec::new();
        while state
            .recently_committed
            .front()
            .is_some_and(|committed| committed.commit_id < lowest_start)
        {
            if let Some(committed) = state.recently_committed.pop_front() {
                cleanup.push(CleanupTask {
                    undo: committed.undo,
                    lowest_active_start: lowest_start,
                });
            }
        }
        cleanup
    }
}

/// An undo log to clean up with the lowest start time of the running transactions.
#[derive(Debug)]
struct CleanupTask {
    undo: UndoBuffer,
    lowest_active_start: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_and_ids_follow_duckdb() {
        let manager = TransactionManager::default();
        let t1 = manager.begin();
        assert_eq!((t1.start_time, t1.transaction_id), (2, TRANSACTION_ID_START));
        let t2 = manager.begin();
        assert_eq!(t2.start_time, 3);
        manager.commit(&t1).unwrap();
        assert_eq!(t1.commit_id(), 4);
        assert_eq!(manager.lowest_active_start(), 3);
        manager.rollback(&t2);
        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.lowest_active_start(), TRANSACTION_ID_START);
    }
}
