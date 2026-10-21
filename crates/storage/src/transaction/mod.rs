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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    /// The attached databases the transaction looked into (DuckDB's `MetaTransaction`
    /// has a transaction of each), by lower-cased name; shared with the manager's list of
    /// running transactions.
    accessed_databases: Arc<Mutex<Vec<String>>>,
    /// The shared hold of the written database's checkpoint lock, until the transaction
    /// ends (`DuckTransaction::write_lock`).
    checkpoint_guard: Mutex<Option<crate::checkpoint_lock::SharedCheckpointGuard>>,
    /// The shared hold of the vacuum lock of a transaction that inserts or deletes rows.
    vacuum_guard: Mutex<Option<crate::checkpoint_lock::SharedCheckpointGuard>>,
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
            accessed_databases: Arc::default(),
            checkpoint_guard: Mutex::new(None),
            vacuum_guard: Mutex::new(None),
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
        self.vacuum_guard
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
    ///
    /// With `vacuum_lock` (inserts and deletes), the database's vacuum lock is held shared
    /// until the transaction ends, so that no checkpoint moves row ids under it.
    pub fn modify_database(
        &self,
        name: &str,
        wal: Option<&Arc<WriteAheadLog>>,
        locks: &crate::checkpoint_lock::DatabaseLocks,
        checkpoint_lock: bool,
        vacuum_lock: bool,
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
        if checkpoint_lock {
            let mut guard = self
                .checkpoint_guard
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if guard.is_none() {
                // Waits while a checkpoint replaces the table data.
                *guard = Some(locks.checkpoint.shared());
            }
        }
        if vacuum_lock {
            let mut guard = self.vacuum_guard.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.is_none() {
                *guard = Some(locks.vacuum.shared());
            }
        }
        Ok(())
    }

    /// Records that the transaction looked into database `name`
    /// (`MetaTransaction::GetTransaction`).
    pub fn access_database(&self, name: &str) {
        let mut accessed = self
            .accessed_databases
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !accessed
            .iter()
            .any(|accessed| accessed.eq_ignore_ascii_case(name))
        {
            accessed.push(name.to_lowercase());
        }
    }

    /// Whether the transaction looked into or wrote to database `name`
    /// (`MetaTransaction::TryGetTransaction`).
    pub fn has_accessed_database(&self, name: &str) -> bool {
        self.accessed_databases
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|accessed| accessed.eq_ignore_ascii_case(name))
            || self
                .modified_database()
                .is_some_and(|modified| modified.eq_ignore_ascii_case(name))
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

    /// What the undo log contains, with the estimated size of the transaction-local rows
    /// (`DuckTransaction::GetUndoProperties`).
    pub fn undo_properties(&self) -> UndoProperties {
        let mut properties = self.undo().properties();
        properties.estimated_size += self.local_storage.estimated_size();
        properties
    }
}

/// A committed undo log waiting until no transaction needs the versions it replaced.
#[derive(Debug)]
struct CommittedUndo {
    commit_id: u64,
    undo: UndoBuffer,
    /// Whether the transaction updated rows or changed the catalog: DuckDB keeps such a
    /// `DuckTransaction`, and with it its shared checkpoint lock, in
    /// `recently_committed_transactions` until no running transaction needs the older
    /// versions, which blocks checkpoints.
    blocks_checkpoint: bool,
    /// The database the transaction wrote to; every attached database has its own
    /// recently committed transactions in DuckDB. `None` blocks every checkpoint.
    database: Option<String>,
}

/// A running transaction as its manager sees it.
#[derive(Debug)]
struct ActiveTransaction {
    id: u64,
    start: u64,
    /// The databases it looked into (its `accessed_databases`): in DuckDB it runs in the
    /// transaction manager of each of them only.
    databases: Arc<Mutex<Vec<String>>>,
}

impl ActiveTransaction {
    fn accessed(&self, database: &str) -> bool {
        self.databases
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|accessed| accessed.eq_ignore_ascii_case(database))
    }
}

#[derive(Debug)]
struct ManagerState {
    /// The next start timestamp / commit id.
    current_start_timestamp: u64,
    current_transaction_id: u64,
    /// Every running transaction.
    active: Vec<ActiveTransaction>,
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
    /// Commits and rollbacks cleaning up undo logs no transaction needs any more. DuckDB
    /// keeps a transaction, and with it its locks, until it is cleaned up; here its
    /// versions (update chains) are still in place while this is non-zero.
    cleanups: AtomicUsize,
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
            cleanups: AtomicUsize::new(0),
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
        let mut transaction = Transaction::new(start_time, transaction_id);
        state.active.push(ActiveTransaction {
            id: transaction_id,
            start: start_time,
            databases: Arc::clone(&transaction.accessed_databases),
        });
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

    /// Whether undo logs of committed transactions are being cleaned up: their update
    /// chains are still in place, which a checkpoint must not vacuum.
    pub fn cleanup_in_progress(&self) -> bool {
        self.cleanups.load(Ordering::Acquire) > 0
    }

    /// Cleans up the undo logs `remove_transaction` collected.
    fn run_cleanup(&self, cleanup: Vec<CleanupTask>) {
        for committed in cleanup {
            committed.undo.cleanup(committed.lowest_active_start);
        }
        self.cleanups.fetch_sub(1, Ordering::AcqRel);
    }

    /// Number of running transactions.
    pub fn active_count(&self) -> usize {
        self.lock().active.len()
    }

    /// Whether a committed transaction that updated rows or changed the catalog of
    /// `database` is kept for a running transaction other than `except` (by id), which in
    /// DuckDB holds the checkpoint lock shared until it is cleaned up
    /// (`recently_committed_transactions`).
    pub fn retains_changes(&self, except: u64, database: &str) -> bool {
        let state = self.lock();
        // DuckDB keeps a transaction manager per database: only the transactions that
        // looked into `database` run in its manager.
        let lowest_start = state
            .active
            .iter()
            .filter(|active| active.id != except && active.accessed(database))
            .map(|active| active.start)
            .min()
            .unwrap_or(TRANSACTION_ID_START);
        state.recently_committed.iter().any(|committed| {
            committed.blocks_checkpoint
                && committed.commit_id >= lowest_start
                && committed
                    .database
                    .as_deref()
                    .is_none_or(|written| written.eq_ignore_ascii_case(database))
        })
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
                    let properties = undo.properties();
                    state.recently_committed.push_back(CommittedUndo {
                        commit_id,
                        undo,
                        blocks_checkpoint: properties.has_updates || properties.has_catalog_changes,
                        database: transaction.modified_database(),
                    });
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
        self.run_cleanup(cleanup);
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
        self.run_cleanup(cleanup);
    }

    /// Removes a finished transaction from the active list and collects the committed
    /// undo logs no running transaction needs any more (`RemoveTransaction`), to be
    /// cleaned up with [`TransactionManager::run_cleanup`].
    fn remove_transaction(&self, state: &mut ManagerState, transaction_id: u64) -> Vec<CleanupTask> {
        self.cleanups.fetch_add(1, Ordering::AcqRel);
        state.active.retain(|active| active.id != transaction_id);
        let lowest_start = state
            .active
            .iter()
            .map(|active| active.start)
            .min()
            .unwrap_or(TRANSACTION_ID_START);
        let lowest_id = state
            .active
            .iter()
            .map(|active| active.id)
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
