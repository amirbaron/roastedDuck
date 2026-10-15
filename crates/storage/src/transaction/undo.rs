//! The undo log of a transaction (DuckDB's `UndoBuffer` with its `CommitState`,
//! `RollbackState` and `CleanupState`).

use std::fmt;
use std::sync::Arc;

use rustdb_common::Result;

use crate::data_table::DataTable;
use crate::index::IndexRemovalType;
use crate::table::{DeleteRecord, UpdateRegistration};
use crate::transaction::NOT_DELETED_ID;

/// A catalog change in the undo log; implemented by the catalog for its version records
/// (DuckDB's `CATALOG_ENTRY` undo entries).
pub trait CatalogUndo: Send + Sync + fmt::Debug {
    /// Marks the change as committed at `commit_id`.
    fn commit(&self, commit_id: u64);

    /// Undoes a commit that failed later on, restoring the transaction id.
    fn revert_commit(&self, transaction_id: u64) {
        let _ = transaction_id;
    }

    /// Discards the uncommitted change.
    fn rollback(&self);

    /// Releases versions no transaction can see any more.
    fn cleanup(&self) {}

    /// Whether the change drops an entry (affects checkpoint decisions).
    fn is_drop(&self) -> bool {
        false
    }
}

/// One entry of the undo log (DuckDB's `UndoFlags` + payload).
#[derive(Debug)]
pub(crate) enum UndoEntry {
    Catalog(Box<dyn CatalogUndo>),
    /// Rows `start_row..start_row + count` appended to `table` at commit.
    Append {
        table: Arc<DataTable>,
        start_row: u64,
        count: u64,
    },
    Delete {
        table: Arc<DataTable>,
        record: DeleteRecord,
    },
    Update {
        table: Arc<DataTable>,
        registration: UpdateRegistration,
    },
}

/// What the undo log contains, for commit decisions (DuckDB's `UndoBufferProperties`).
#[allow(clippy::struct_excessive_bools, reason = "independent flags, as in DuckDB")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UndoProperties {
    pub has_updates: bool,
    pub has_deletes: bool,
    pub has_catalog_changes: bool,
    pub has_dropped_entries: bool,
}

/// The ordered undo log of a transaction.
#[derive(Debug, Default)]
pub(crate) struct UndoBuffer {
    entries: Vec<UndoEntry>,
}

impl UndoBuffer {
    pub fn push(&mut self, entry: UndoEntry) {
        self.entries.push(entry);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn properties(&self) -> UndoProperties {
        let mut properties = UndoProperties::default();
        for entry in &self.entries {
            match entry {
                UndoEntry::Catalog(change) => {
                    properties.has_catalog_changes = true;
                    properties.has_dropped_entries |= change.is_drop();
                }
                UndoEntry::Delete { .. } => properties.has_deletes = true,
                UndoEntry::Update { .. } => properties.has_updates = true,
                UndoEntry::Append { .. } => {}
            }
        }
        properties
    }

    /// Applies `commit_id` to every entry, in order (`UndoBuffer::Commit`). Deleted rows
    /// leave the indexes; with `others_active` they are kept in the indexes' side index of
    /// deleted rows still in use. On failure the already committed entries are reverted to
    /// `transaction_id` and the error returned.
    pub fn commit(&self, commit_id: u64, transaction_id: u64, others_active: bool) -> Result<()> {
        let removal = if others_active {
            IndexRemovalType::MainIndex
        } else {
            IndexRemovalType::MainIndexOnly
        };
        for (index, entry) in self.entries.iter().enumerate() {
            if let Err(error) = Self::commit_entry(entry, commit_id, removal) {
                let revert = if others_active {
                    IndexRemovalType::RevertMainIndex
                } else {
                    IndexRemovalType::RevertMainIndexOnly
                };
                for committed in &self.entries[..index] {
                    Self::revert_entry(committed, transaction_id, revert)?;
                }
                return Err(error);
            }
        }
        Ok(())
    }

    fn commit_entry(entry: &UndoEntry, commit_id: u64, removal: IndexRemovalType) -> Result<()> {
        match entry {
            UndoEntry::Catalog(change) => change.commit(commit_id),
            UndoEntry::Append {
                table,
                start_row,
                count,
            } => {
                table.check_main_table_for_commit()?;
                table.commit_append(commit_id, *start_row, *count);
            }
            UndoEntry::Delete { table, record } => {
                table.check_main_table_for_commit()?;
                record
                    .version_info
                    .commit_delete(record.vector_index, commit_id, &record.rows);
                table
                    .remove_from_indexes(&record.row_ids(), removal)
                    .map_err(|e| {
                        rustdb_common::Error::new(rustdb_common::ErrorKind::Fatal, e.raw_message().to_owned())
                    })?;
            }
            UndoEntry::Update {
                table, registration, ..
            } => {
                table.check_main_table_for_commit()?;
                registration.version.set(commit_id);
            }
        }
        Ok(())
    }

    fn revert_entry(entry: &UndoEntry, transaction_id: u64, removal: IndexRemovalType) -> Result<()> {
        match entry {
            UndoEntry::Catalog(change) => change.revert_commit(transaction_id),
            UndoEntry::Append { table, start_row, .. } => {
                // The append is reverted by the rollback that follows a failed commit.
                let _ = (table, start_row);
            }
            UndoEntry::Delete { table, record } => {
                record
                    .version_info
                    .commit_delete(record.vector_index, transaction_id, &record.rows);
                table.remove_from_indexes(&record.row_ids(), removal)?;
            }
            UndoEntry::Update { registration, .. } => registration.version.set(transaction_id),
        }
        Ok(())
    }

    /// Undoes every entry, newest first (`UndoBuffer::Rollback`).
    pub fn rollback(&self) -> Result<()> {
        for entry in self.entries.iter().rev() {
            match entry {
                UndoEntry::Catalog(change) => change.rollback(),
                UndoEntry::Append {
                    table,
                    start_row,
                    count,
                } => table.revert_append(*start_row, *count)?,
                UndoEntry::Delete { record, .. } => {
                    record
                        .version_info
                        .commit_delete(record.vector_index, NOT_DELETED_ID, &record.rows);
                }
                UndoEntry::Update { registration, .. } => {
                    registration
                        .segment
                        .rollback_update(registration.vector_index, &registration.version);
                }
            }
        }
        Ok(())
    }

    /// Releases versions once no active transaction needs them (`UndoBuffer::Cleanup`).
    pub fn cleanup(&self, lowest_active_transaction: u64) {
        for entry in &self.entries {
            match entry {
                UndoEntry::Catalog(change) => change.cleanup(),
                UndoEntry::Append {
                    table,
                    start_row,
                    count,
                } => table.cleanup_append(lowest_active_transaction, *start_row, *count),
                UndoEntry::Update { registration, .. } => {
                    registration
                        .segment
                        .cleanup_update(registration.vector_index, &registration.version);
                }
                UndoEntry::Delete { table, record } => {
                    // Older transactions no longer need the deleted rows' keys. Failures
                    // only leave stale entries in the side index, which scans tolerate.
                    let _ = table.remove_from_indexes(&record.row_ids(), IndexRemovalType::DeletedRowsInUse);
                }
            }
        }
    }
}
