use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_common::{Error, ErrorKind, Result};

use crate::transaction::{StorageHook, TRANSACTION_ID_START, Transaction, TransactionExt};
use crate::{CatalogEntry, WalRecord};

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

    /// The entry of this version (`None` for a tombstone).
    pub(crate) fn entry(&self) -> Option<&Arc<CatalogEntry>> {
        self.entry.as_ref()
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
    /// The catalog the set belongs to.
    catalog: String,
    /// The schema the set belongs to (empty for the set of schemas).
    schema: String,
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
    /// An empty set of schema `schema` of catalog `catalog`.
    pub fn new(catalog: impl Into<String>, schema: impl Into<String>) -> Self {
        Self {
            chains: Mutex::default(),
            catalog: catalog.into(),
            schema: schema.into(),
        }
    }

    /// The catalog the set belongs to.
    pub fn catalog_name(&self) -> &str {
        &self.catalog
    }

    /// The schema the set belongs to (empty for the set of schemas).
    pub fn schema_name(&self) -> &str {
        &self.schema
    }

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

    /// Replaces the built-in entry of that name by `entry`, or adds it, visible to every
    /// transaction (an extension altering a built-in, DuckDB's `ALTER_ON_CONFLICT` on the
    /// system catalog). Transactions holding the old entry keep it.
    pub fn replace_committed(&self, entry: CatalogEntry) {
        let mut chains = self.lock();
        chains.insert(key(entry.name()), vec![Version::new(0, Some(Arc::new(entry)))]);
    }

    /// The built-in (committed) entry called `name`, ignoring transactions.
    pub fn get_committed(&self, name: &str) -> Option<Arc<CatalogEntry>> {
        let chains = self.lock();
        chains
            .get(&key(name))
            .and_then(|chain| {
                chain
                    .iter()
                    .find(|version| version.timestamp() < TRANSACTION_ID_START)
            })
            .and_then(|version| version.entry.clone())
    }

    /// Creates `entry` in `transaction`. Returns `false` if a visible entry with that name
    /// already exists (callers turn this into the appropriate "already exists" error).
    pub fn create(self: &Arc<Self>, transaction: &Transaction, entry: CatalogEntry) -> Result<bool> {
        self.create_logged(transaction, entry, WalRecord::Create)
    }

    /// [`CatalogSet::create`], logged as `wal`.
    fn create_logged(
        self: &Arc<Self>,
        transaction: &Transaction,
        entry: CatalogEntry,
        wal: WalRecord,
    ) -> Result<bool> {
        let name = key(entry.name());
        let hook = match &entry.kind {
            crate::EntryKind::Index(index) => index.storage.as_ref().map_or(StorageHook::None, |info| {
                StorageHook::CreatedIndex(Arc::clone(info), entry.name().to_owned())
            }),
            _ => StorageHook::None,
        };
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
        transaction.push_catalog_change(Arc::clone(self), name, version, hook, wal);
        Ok(true)
    }

    /// Replaces the visible entry called `name` by `entry` (DuckDB's `AlterEntry`)
    /// without logging the change. Returns `false` if no visible entry exists.
    pub fn alter(
        self: &Arc<Self>,
        transaction: &Transaction,
        name: &str,
        entry: CatalogEntry,
    ) -> Result<bool> {
        self.alter_with_hook(transaction, name, entry, StorageHook::None, None)
    }

    /// Renames the visible entry called `name` to the name of `entry`, which replaces it
    /// (DuckDB's `AlterEntry` with a rename). Returns `false` if no visible entry exists.
    pub fn rename(
        self: &Arc<Self>,
        transaction: &Transaction,
        name: &str,
        mut entry: CatalogEntry,
        hook: StorageHook,
        info: Option<rustdb_ast::AlterInfo>,
    ) -> Result<bool> {
        let old = key(name);
        let tombstone = Version::new(transaction.transaction_id, None);
        {
            let mut chains = self.lock();
            let Some(chain) = chains.get_mut(&old) else {
                return Ok(false);
            };
            let head = &chain[0];
            if conflicts(transaction, head.timestamp()) {
                return Err(Error::new(
                    ErrorKind::Transaction,
                    format!(
                        "Catalog write-write conflict on alter with \"{}\"",
                        display_name(head, &old)
                    ),
                ));
            }
            let Some(current) = &head.entry else {
                return Ok(false);
            };
            // A renamed entry keeps its identity.
            entry.oid = current.oid;
            chain.insert(0, Arc::clone(&tombstone));
        }
        transaction.push_catalog_change(Arc::clone(self), old, tombstone, hook, WalRecord::None);
        let wal = info.map_or(WalRecord::None, |info| WalRecord::Alter(Box::new(info)));
        if !self.create_logged(transaction, entry, wal)? {
            return Err(Error::internal("rename target exists"));
        }
        Ok(true)
    }

    /// [`CatalogSet::alter`] with the storage change of the alter, undone on rollback,
    /// logged as `info`.
    pub fn alter_with_hook(
        self: &Arc<Self>,
        transaction: &Transaction,
        name: &str,
        entry: CatalogEntry,
        hook: StorageHook,
        info: Option<rustdb_ast::AlterInfo>,
    ) -> Result<bool> {
        let name = key(name);
        let mut entry = entry;
        let version;
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
            let Some(current) = &head.entry else {
                return Ok(false);
            };
            // An altered entry keeps its identity.
            entry.oid = current.oid;
            version = Version::new(transaction.transaction_id, Some(Arc::new(entry)));
            chain.insert(0, Arc::clone(&version));
        }
        let wal = info.map_or(WalRecord::None, |info| WalRecord::Alter(Box::new(info)));
        transaction.push_catalog_change(Arc::clone(self), name, version, hook, wal);
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
        let mut hook = StorageHook::None;
        let dropped;
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
            dropped = Arc::clone(entry);
            if entry.internal && !allow_internal {
                return Err(Error::catalog(format!(
                    "Cannot drop entry \"{}\" because it is an internal system entry",
                    entry.name()
                )));
            }
            match &entry.kind {
                crate::EntryKind::Table(table) => {
                    if let Some(storage) = &table.storage {
                        // The storage is marked as dropped when the drop commits.
                        transaction.local_storage().drop_table(storage);
                        hook = StorageHook::DroppedTable(Arc::clone(storage));
                    }
                }
                crate::EntryKind::Index(index) => {
                    if let Some(info) = &index.storage {
                        hook = StorageHook::DroppedIndex(Arc::clone(info), entry.name().to_owned());
                    }
                }
                _ => {}
            }
            chain.insert(0, Arc::clone(&version));
        }
        transaction.push_catalog_change(Arc::clone(self), name, version, hook, WalRecord::Drop(dropped));
        Ok(true)
    }

    /// Fails if another transaction made a version of `name` that `transaction` may not
    /// build on (the conflict check of `CatalogSet::GetEntryInternal`).
    pub(crate) fn check_write_conflict(&self, transaction: &Transaction, name: &str) -> Result<()> {
        let chains = self.lock();
        let Some(head) = chains.get(&key(name)).and_then(|chain| chain.first()) else {
            return Ok(());
        };
        if conflicts(transaction, head.timestamp()) {
            return Err(Error::new(
                ErrorKind::Transaction,
                format!(
                    "Catalog write-write conflict on alter with \"{}\"",
                    display_name(head, &key(name))
                ),
            ));
        }
        Ok(())
    }

    /// The name of an entry matching `filter` that another transaction committed into
    /// this set after `start_time`.
    pub(crate) fn created_since(
        &self,
        start_time: u64,
        filter: impl Fn(&CatalogEntry) -> bool,
    ) -> Option<String> {
        self.lock().values().find_map(|chain| {
            let head = chain.first()?;
            let timestamp = head.timestamp();
            let entry = head.entry.as_ref()?;
            (timestamp < TRANSACTION_ID_START && timestamp > start_time && filter(entry))
                .then(|| entry.name().to_owned())
        })
    }

    /// Whether another transaction committed a drop of `name` after `start_time`.
    pub(crate) fn dropped_since(&self, name: &str, start_time: u64) -> bool {
        self.lock()
            .get(&key(name))
            .and_then(|chain| chain.first())
            .is_some_and(|head| {
                let timestamp = head.timestamp();
                head.is_tombstone() && timestamp < TRANSACTION_ID_START && timestamp > start_time
            })
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

    /// Drops the versions older than the committed `version`, which no transaction can
    /// see any more (`CatalogSet::CleanupEntry`); a drop that is the newest version
    /// removes the whole chain. Old versions hold the storage of altered tables.
    pub(crate) fn cleanup(&self, name: &str, version: &Arc<Version>) {
        let mut chains = self.lock();
        let Some(chain) = chains.get_mut(name) else { return };
        let Some(position) = chain.iter().position(|v| Arc::ptr_eq(v, version)) else {
            return;
        };
        if position == 0 && version.is_tombstone() {
            chains.remove(name);
            return;
        }
        chain.truncate(position + 1);
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
