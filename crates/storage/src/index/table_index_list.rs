//! The indexes of a table (DuckDB's `TableIndexList` and `IndexEntry`).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use rustdb_common::Result;

use super::Art;

/// Which side of a foreign key an index lookup is for (`ForeignKeyType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeignKeyType {
    /// The referenced (primary key) table: a unique index over the key columns.
    PrimaryKeyTable,
    /// The referencing table: a foreign key index over the key columns.
    ForeignKeyTable,
}

/// The mutable state of an index entry, guarded by the entry lock.
#[derive(Debug)]
pub struct IndexEntryState {
    /// The index.
    pub index: Art,
    /// Rows deleted by committed transactions that older transactions may still see
    /// (`deleted_rows_in_use`).
    pub deleted_rows_in_use: Option<Art>,
}

/// One index of a table.
#[derive(Debug)]
pub struct IndexEntry {
    name: String,
    column_ids: Vec<usize>,
    is_unique: bool,
    is_primary: bool,
    is_foreign: bool,
    state: Mutex<IndexEntryState>,
}

impl IndexEntry {
    fn new(index: Art) -> Self {
        Self {
            name: index.name().to_owned(),
            column_ids: index.column_ids().to_vec(),
            is_unique: index.is_unique(),
            is_primary: index.is_primary(),
            is_foreign: index.is_foreign(),
            state: Mutex::new(IndexEntryState {
                index,
                deleted_rows_in_use: None,
            }),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn column_ids(&self) -> &[usize] {
        &self.column_ids
    }

    pub fn is_unique(&self) -> bool {
        self.is_unique
    }

    pub fn is_primary(&self) -> bool {
        self.is_primary
    }

    pub fn is_foreign(&self) -> bool {
        self.is_foreign
    }

    /// Locks the index (`IndexEntry::lock` together with the index lock).
    pub fn lock(&self) -> MutexGuard<'_, IndexEntryState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `IsForeignKeyIndex`.
    fn is_foreign_key_index(&self, keys: &[usize], fk_type: ForeignKeyType) -> bool {
        let kind_matches = match fk_type {
            ForeignKeyType::PrimaryKeyTable => self.is_unique,
            ForeignKeyType::ForeignKeyTable => self.is_foreign,
        };
        kind_matches
            && keys.len() == self.column_ids.len()
            && keys.iter().all(|k| self.column_ids.contains(k))
    }
}

/// The indexes of a table.
#[derive(Debug, Default)]
pub struct TableIndexList {
    entries: RwLock<Vec<Arc<IndexEntry>>>,
}

impl TableIndexList {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Vec<Arc<IndexEntry>>> {
        self.entries.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Vec<Arc<IndexEntry>>> {
        self.entries.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds an index (`AddIndex`).
    pub fn add(&self, index: Art) -> Arc<IndexEntry> {
        let entry = Arc::new(IndexEntry::new(index));
        self.write().push(Arc::clone(&entry));
        entry
    }

    /// Removes the index named `name` and frees its memory (`RemoveIndex`).
    pub fn remove(&self, name: &str) -> bool {
        let mut entries = self.write();
        let Some(pos) = entries.iter().position(|e| e.name == name) else {
            return false;
        };
        let entry = entries.remove(pos);
        entry.lock().index.reset();
        true
    }

    /// A snapshot of the entries (`IndexEntries`).
    pub fn entries(&self) -> Vec<Arc<IndexEntry>> {
        self.read().clone()
    }

    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    pub fn len(&self) -> usize {
        self.read().len()
    }

    /// Whether any index enforces uniqueness.
    pub fn has_unique(&self) -> bool {
        self.read().iter().any(|e| e.is_unique)
    }

    /// The index named `name` (`Find`).
    pub fn find(&self, name: &str) -> Option<Arc<IndexEntry>> {
        self.read().iter().find(|e| e.name == name).cloned()
    }

    /// Whether no constraint index is named `name` (`NameIsUnique`).
    pub fn name_is_unique(&self, name: &str) -> bool {
        !self
            .read()
            .iter()
            .any(|e| (e.is_primary || e.is_foreign || e.is_unique) && e.name == name)
    }

    /// The index enforcing a foreign key over `keys` (`FindForeignKeyIndex`).
    pub fn find_foreign_key_index(&self, keys: &[usize], fk_type: ForeignKeyType) -> Option<Arc<IndexEntry>> {
        self.read()
            .iter()
            .find(|e| e.is_foreign_key_index(keys, fk_type))
            .cloned()
    }

    /// Every column some index reads (`GetRequiredColumns`), sorted.
    pub fn required_columns(&self) -> Vec<usize> {
        let columns: BTreeSet<usize> = self
            .read()
            .iter()
            .flat_map(|e| e.column_ids.iter().copied())
            .collect();
        columns.into_iter().collect()
    }

    /// Empty copies of every constraint index, for transaction-local appends or deletes
    /// (`LocalTableStorage` constructor).
    pub fn delta_list(&self) -> Result<Self> {
        let list = Self::default();
        for entry in self.entries() {
            let state = entry.lock();
            if state.index.constraint_type() == super::IndexConstraintType::None {
                continue;
            }
            list.add(state.index.delta(false)?);
        }
        Ok(list)
    }

    /// Removes every index (dropped table).
    pub fn clear(&self) {
        let mut entries = self.write();
        for entry in entries.iter() {
            entry.lock().index.reset();
        }
        entries.clear();
    }

    /// Whether some index reads one of `columns` (`IndexIsUpdated`).
    pub fn any_index_on(&self, columns: &[usize]) -> bool {
        self.read()
            .iter()
            .any(|e| e.column_ids.iter().any(|c| columns.contains(c)))
    }
}
