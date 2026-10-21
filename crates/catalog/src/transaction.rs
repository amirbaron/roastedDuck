//! Transactions are owned by the storage layer (`DuckTransaction`); the catalog records
//! its version changes in the transaction's undo log so that they commit, roll back and
//! are logged in order with the data changes.

use std::sync::Arc;

use rustdb_common::Result;
use rustdb_storage::CatalogUndo;
pub use rustdb_storage::transaction::{
    MAX_TRANSACTION_ID, TRANSACTION_ID_START, Transaction, TransactionManager,
};
use rustdb_storage::wal::WalBatch;

use crate::WalRecord;
use crate::catalog_set::{CatalogSet, Version};

/// What a catalog change does to storage when it commits or rolls back.
#[derive(Debug)]
pub enum StorageHook {
    None,
    /// The previous storage of an altered table, made the main table again on rollback
    /// (`DuckTableEntry::SetAsRoot`).
    AlteredTable(Arc<rustdb_storage::DataTable>),
    /// A renamed table's storage and its old name, restored on rollback
    /// (`DuckTableEntry::UndoAlter`).
    RenamedTable(Arc<rustdb_storage::DataTableInfo>, String),
    /// The storage of a dropped table, restored as the main table on rollback
    /// (`DuckTableEntry::OnDrop` / `SetAsRoot`).
    DroppedTable(Arc<rustdb_storage::DataTable>),
    /// A created index, removed from its table on rollback (`DuckIndexEntry::Rollback`).
    CreatedIndex(Arc<rustdb_storage::DataTableInfo>, String),
    /// The previous storage of a table given a primary key and the key's index: the
    /// storage is made the main table again and the index removed on rollback.
    AddedPrimaryKey(Arc<rustdb_storage::DataTable>, String),
    /// A dropped index, removed from its table on commit (`DuckIndexEntry::CommitDrop`).
    DroppedIndex(Arc<rustdb_storage::DataTableInfo>, String),
}

/// A created or dropped catalog entry version, as an undo-log entry.
#[derive(Debug)]
struct CatalogChange {
    set: Arc<CatalogSet>,
    name: String,
    version: Arc<Version>,
    hook: StorageHook,
    wal: WalRecord,
}

impl CatalogUndo for CatalogChange {
    fn commit(&self, commit_id: u64) {
        self.version.commit(commit_id);
        match &self.hook {
            StorageHook::DroppedIndex(info, name) => {
                info.indexes().remove(name);
            }
            // `CatalogSet::CommitDrop` (`DuckTableEntry::OnDrop`): writes of other
            // transactions to the table now fail to commit.
            StorageHook::DroppedTable(table) => table.set_as_dropped(),
            _ => {}
        }
    }

    fn revert_commit(&self, transaction_id: u64) {
        self.version.commit(transaction_id);
        if let StorageHook::DroppedTable(table) = &self.hook {
            table.set_as_main_table();
        }
    }

    fn rollback(&self) {
        self.set.undo(&self.name, &self.version);
        match &self.hook {
            StorageHook::DroppedTable(table) | StorageHook::AlteredTable(table) => table.set_as_main_table(),
            StorageHook::RenamedTable(info, name) => info.set_table_name(name.clone()),
            StorageHook::CreatedIndex(info, name) => {
                info.indexes().remove(name);
            }
            StorageHook::AddedPrimaryKey(table, name) => {
                table.set_as_main_table();
                table.info().indexes().remove(name);
            }
            StorageHook::None | StorageHook::DroppedIndex(..) => {}
        }
    }

    fn cleanup(&self) {
        self.set.cleanup(&self.name, &self.version);
    }

    fn is_drop(&self) -> bool {
        self.version.is_tombstone()
    }

    fn estimated_size(&self) -> u64 {
        match &self.hook {
            StorageHook::CreatedIndex(info, name) => info
                .indexes()
                .find(name)
                .map_or(0, |entry| entry.lock().index.in_memory_size() as u64),
            _ => 0,
        }
    }

    fn write_to_wal(&self, batch: &mut WalBatch) -> Result<()> {
        let catalog = self.set.catalog_name();
        let schema = self.set.schema_name();
        match &self.wal {
            WalRecord::Create => match self.version.entry() {
                Some(entry) => crate::wal::write_create(batch, &catalog, schema, entry),
                None => Ok(()),
            },
            WalRecord::Drop(entry) => crate::wal::write_drop(batch, schema, entry),
            WalRecord::Alter(info) => {
                let temporary = self.version.entry().is_some_and(|entry| entry.temporary);
                if temporary {
                    return Ok(());
                }
                crate::wal::write_alter(batch, info, self.version.entry().map(AsRef::as_ref))
            }
            WalRecord::None => Ok(()),
        }
    }
}

/// A check made when a transaction commits, without changes of its own.
#[derive(Debug)]
pub(crate) enum CommitCheck {
    /// A dropped schema: no other transaction may have created entries in it since this
    /// one started (`VerifyCommitDrop`; DuckDB's entries depend on their schema).
    SchemaDropped {
        name: String,
        sets: Vec<Arc<CatalogSet>>,
    },
    /// A dropped entry: no other transaction may have committed an entry depending on
    /// it or owned by it since this one started (`VerifyCommitDrop`).
    EntryDropped {
        entry: rustdb_ast::CatalogEntryInfo,
        sets: Vec<Arc<CatalogSet>>,
    },
    /// An entry created in a schema, or an entry given an owner: no other transaction
    /// may have dropped the schema or the owner `name` of `set` since
    /// (`VerifyExistence`).
    SubjectExists { name: String, set: Arc<CatalogSet> },
}

impl CatalogUndo for CommitCheck {
    fn commit(&self, _: u64) {}

    fn rollback(&self) {}

    fn verify_commit(&self, start_time: u64) -> Result<()> {
        match self {
            Self::SchemaDropped { name, sets } => {
                if sets
                    .iter()
                    .any(|set| set.created_since(start_time, |_| true).is_some())
                {
                    return Err(rustdb_common::Error::new(
                        rustdb_common::ErrorKind::Dependency,
                        format!(
                            "Could not commit DROP of \"{name}\" because a dependency was created after the transaction started"
                        ),
                    ));
                }
            }
            Self::EntryDropped { entry, sets } => {
                let refers = |candidate: &crate::CatalogEntry| {
                    candidate
                        .dependencies
                        .iter()
                        .any(|dependency| crate::entry::same_entry(&dependency.entry, entry))
                        || candidate
                            .owned_by
                            .as_ref()
                            .is_some_and(|owner| crate::entry::same_entry(owner, entry))
                };
                if sets
                    .iter()
                    .any(|set| set.created_since(start_time, refers).is_some())
                {
                    return Err(rustdb_common::Error::new(
                        rustdb_common::ErrorKind::Dependency,
                        format!(
                            "Could not commit DROP of \"{}\" because a dependency was created after the transaction started",
                            entry.name
                        ),
                    ));
                }
            }
            Self::SubjectExists { name, set } => {
                if set.dropped_since(name, start_time) {
                    return Err(rustdb_common::Error::new(
                        rustdb_common::ErrorKind::Dependency,
                        format!(
                            "Could not commit creation of dependency, subject \"{name}\" has been deleted"
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Registers catalog changes in a transaction's undo log.
pub(crate) trait TransactionExt {
    fn push_catalog_change(
        &self,
        set: Arc<CatalogSet>,
        name: String,
        version: Arc<Version>,
        hook: StorageHook,
        wal: WalRecord,
    );
}

impl TransactionExt for Transaction {
    fn push_catalog_change(
        &self,
        set: Arc<CatalogSet>,
        name: String,
        version: Arc<Version>,
        hook: StorageHook,
        wal: WalRecord,
    ) {
        self.push_catalog_undo(Box::new(CatalogChange {
            set,
            name,
            version,
            hook,
            wal,
        }));
    }
}
