use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_common::{Error, ErrorKind, Result};

use crate::CatalogEntry;
use crate::transaction::{TRANSACTION_ID_START, Transaction, TransactionExt};

/// One version in an entry chain.
#[derive(Debug)]
pub(crate) struct Version {
    /// The creating transaction's id while uncommitted, then its commit timestamp.
    timestamp: AtomicU64,
    /// `None` is a tombstone (a dropped entry, or the root of a chain created by an
    /// uncommitted transaction).
    entry: Option<Arc<CatalogEntry>>,
}

impl Version {
    fn new(timestamp: u64, entry: Option<Arc<CatalogEntry>>) -> Arc<Self> {
        Arc::new(Self {
            timestamp: AtomicU64::new(timestamp),
            entry,
        })
    }

    fn timestamp(&self) -> u64 {
        self.timestamp.load(Ordering::Acquire)
    }

    pub(crate) fn commit(&self, commit_id: u64) {
        self.timestamp.store(commit_id, Ordering::Release);
    }

    /// Whether this version is a drop (no entry).
    pub(crate) fn is_tombstone(&self) -> bool {
        self.entry.is_none()
    }
}

/// Outcome of a detailed lookup (DuckDB's `EntryLookup::FailureReason`).
#[derive(Debug)]
pub enum Lookup {
    Found(Arc<CatalogEntry>),
    /// No entry by this name ever existed.
    NotPresent,
    /// The visible version is a drop.
    Deleted,
    /// Only versions invisible to this transaction exist.
    Invisible,
}

/// Name-keyed, versioned storage for catalog entries (DuckDB's `CatalogSet`).
///
/// Names are case-insensitive. Each chain is ordered newest first.
#[derive(Debug, Default)]
pub struct CatalogSet {
    chains: Mutex<BTreeMap<String, Vec<Arc<Version>>>>,
}

fn key(name: &str) -> String {
    name.to_lowercase()
}

/// We created this version, or it was committed before we started.
fn visible(transaction: &Transaction, timestamp: u64) -> bool {
    timestamp == transaction.transaction_id || timestamp < transaction.start_time
}

/// Another transaction wrote this version and it is not part of our snapshot.
fn conflicts(transaction: &Transaction, timestamp: u64) -> bool {
    let other_active = timestamp >= TRANSACTION_ID_START && timestamp != transaction.transaction_id;
    let committed_after_start = timestamp < TRANSACTION_ID_START && timestamp > transaction.start_time;
    other_active || committed_after_start
}

impl CatalogSet {
    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Vec<Arc<Version>>>> {
        self.chains.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds an entry visible to every transaction (built-in objects).
    pub fn create_committed(&self, entry: CatalogEntry) -> Result<()> {
        let mut chains = self.lock();
        let name = key(entry.name());
        if chains.contains_key(&name) {
            return Err(Error::internal(format!(
                "Entry with name \"{}\" already exists",
                entry.name()
            )));
        }
        chains.insert(name, vec![Version::new(0, Some(Arc::new(entry)))]);
        Ok(())
    }

    /// Creates `entry` in `transaction`. Returns `false` if a visible entry with that name
    /// already exists (callers turn this into the appropriate "already exists" error).
    pub fn create(self: &Arc<Self>, transaction: &Transaction, entry: CatalogEntry) -> Result<bool> {
        let name = key(entry.name());
        let version = Version::new(transaction.transaction_id, Some(Arc::new(entry)));
        {
            let mut chains = self.lock();
            match chains.get_mut(&name) {
                None => {
                    // Start the chain with a tombstone so concurrent transactions see
                    // "nothing" instead of our uncommitted entry.
                    chains.insert(name.clone(), vec![Arc::clone(&version), Version::new(0, None)]);
                }
                Some(chain) => {
                    let head = &chain[0];
                    if conflicts(transaction, head.timestamp()) {
                        return Err(Error::new(
                            ErrorKind::Transaction,
                            format!(
                                "Catalog write-write conflict on create with \"{}\"",
                                display_name(head, &name)
                            ),
                        ));
                    }
                    if head.entry.is_some() {
                        return Ok(false);
                    }
                    chain.insert(0, Arc::clone(&version));
                }
            }
        }
        transaction.push_catalog_change(Arc::clone(self), name, version, None);
        Ok(true)
    }

    /// Replaces the visible entry called `name` by `entry` (DuckDB's `AlterEntry`).
    /// Returns `false` if no visible entry exists.
    pub fn alter(self: &Arc<Self>, transaction: &Transaction, name: &str, entry: CatalogEntry) -> Result<bool> {
        let name = key(name);
        let version = Version::new(transaction.transaction_id, Some(Arc::new(entry)));
        {
            let mut chains = self.lock();
            let Some(chain) = chains.get_mut(&name) else {
                return Ok(false);
            };
            let head = &chain[0];
            if conflicts(transaction, head.timestamp()) {
                return Err(Error::new(
                    ErrorKind::Transaction,
                    format!(
                        "Catalog write-write conflict on alter with \"{}\"",
                        display_name(head, &name)
                    ),
                ));
            }
            if head.entry.is_none() {
                return Ok(false);
            }
            chain.insert(0, Arc::clone(&version));
        }
        transaction.push_catalog_change(Arc::clone(self), name, version, None);
        Ok(true)
    }

    /// Drops the entry called `name`. Returns `false` if no visible entry exists.
    pub fn drop_entry(
        self: &Arc<Self>,
        transaction: &Transaction,
        name: &str,
        allow_internal: bool,
    ) -> Result<bool> {
        let name = key(name);
        let version = Version::new(transaction.transaction_id, None);
        let mut dropped_table = None;
        {
            let mut chains = self.lock();
            let Some(chain) = chains.get_mut(&name) else {
                return Ok(false);
            };
            let head = &chain[0];
            if conflicts(transaction, head.timestamp()) {
                return Err(Error::new(
                    ErrorKind::Transaction,
                    format!(
                        "Catalog write-write conflict on alter with \"{}\"",
                        display_name(head, &name)
                    ),
                ));
            }
            let Some(entry) = &head.entry else {
                return Ok(false);
            };
            if entry.internal && !allow_internal {
                return Err(Error::catalog(format!(
                    "Cannot drop entry \"{}\" because it is an internal system entry",
                    entry.name()
                )));
            }
            if let crate::EntryKind::Table(table) = &entry.kind
                && let Some(storage) = &table.storage
            {
                // Concurrent writers of the table now fail (`DuckTableEntry::OnDrop`).
                storage.set_as_dropped();
                transaction.local_storage().drop_table(storage);
                dropped_table = Some(Arc::clone(storage));
            }
            chain.insert(0, Arc::clone(&version));
        }
        transaction.push_catalog_change(Arc::clone(self), name, version, dropped_table);
        Ok(true)
    }

    /// Looks up `name` as seen by `transaction`.
    pub fn lookup(&self, transaction: &Transaction, name: &str) -> Lookup {
        let chains = self.lock();
        let Some(chain) = chains.get(&key(name)) else {
            return Lookup::NotPresent;
        };
        let (version, is_visible) = version_for(transaction, chain);
        match (&version.entry, is_visible) {
            (Some(entry), _) => Lookup::Found(Arc::clone(entry)),
            (None, true) => Lookup::Deleted,
            (None, false) => Lookup::Invisible,
        }
    }

    /// The entry called `name`, if visible to `transaction`.
    pub fn get(&self, transaction: &Transaction, name: &str) -> Option<Arc<CatalogEntry>> {
        match self.lookup(transaction, name) {
            Lookup::Found(entry) => Some(entry),
            Lookup::NotPresent | Lookup::Deleted | Lookup::Invisible => None,
        }
    }

    /// Every entry visible to `transaction`, in name order.
    pub fn scan(&self, transaction: &Transaction) -> Vec<Arc<CatalogEntry>> {
        let chains = self.lock();
        chains
            .values()
            .filter_map(|chain| version_for(transaction, chain).0.entry.clone())
            .collect()
    }

    /// Removes an uncommitted version (rollback).
    pub(crate) fn undo(&self, name: &str, version: &Arc<Version>) {
        let mut chains = self.lock();
        let Some(chain) = chains.get_mut(name) else { return };
        chain.retain(|v| !Arc::ptr_eq(v, version));
        // A chain left with only its tombstone root never existed.
        if chain.iter().all(|v| v.entry.is_none() && v.timestamp() == 0) {
            chains.remove(name);
        }
    }
}

/// The version of a chain that `transaction` should use (DuckDB's
/// `GetEntryForTransaction`): the newest visible one, falling back to the chain's oldest
/// version, which is reported as not visible.
fn version_for<'a>(transaction: &Transaction, chain: &'a [Arc<Version>]) -> (&'a Version, bool) {
    let (root, newer) = chain
        .split_last()
        .unwrap_or_else(|| unreachable!("chains are never empty"));
    match newer
        .iter()
        .find(|version| visible(transaction, version.timestamp()))
    {
        Some(version) => (version, true),
        None => (root, false),
    }
}

fn display_name(version: &Version, fallback: &str) -> String {
    version
        .entry
        .as_ref()
        .map_or_else(|| fallback.to_owned(), |entry| entry.name().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntryKind, TransactionManager};

    fn entry(name: &str) -> CatalogEntry {
        CatalogEntry::new(name, EntryKind::Schema(crate::SchemaEntry::default()))
    }

    #[test]
    fn snapshot_isolation_for_creates_and_drops() {
        let manager = TransactionManager::default();
        let set = Arc::new(CatalogSet::default());
        let t1 = manager.begin();
        let t2 = manager.begin();
        assert!(set.create(&t1, entry("S")).unwrap());
        assert!(set.get(&t1, "s").is_some(), "names are case-insensitive");
        assert!(matches!(set.lookup(&t2, "s"), Lookup::Invisible));
        let conflict = set.create(&t2, entry("s")).unwrap_err();
        assert_eq!(
            conflict.to_string(),
            "TransactionContext Error: Catalog write-write conflict on create with \"S\""
        );
        manager.commit(&t1).unwrap();
        // t2 started before the commit and still cannot see it.
        assert!(set.get(&t2, "s").is_none());
        let t3 = manager.begin();
        assert!(set.get(&t3, "s").is_some());
        assert!(set.drop_entry(&t3, "S", false).unwrap());
        assert!(matches!(set.lookup(&t3, "s"), Lookup::Deleted));
        manager.rollback(&t3);
        let t4 = manager.begin();
        assert!(set.get(&t4, "s").is_some());
    }

    #[test]
    fn rollback_of_create_removes_the_chain() {
        let manager = TransactionManager::default();
        let set = Arc::new(CatalogSet::default());
        let t1 = manager.begin();
        set.create(&t1, entry("x")).unwrap();
        manager.rollback(&t1);
        let t2 = manager.begin();
        assert!(matches!(set.lookup(&t2, "x"), Lookup::NotPresent));
    }
}
