//! The indexes of a table (DuckDB's `TableIndexList` and `IndexEntry`).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use roastedduck_common::{ByteString, Result};
use roastedduck_types::DataChunk;

use super::Art;
use super::unbound_index::{BufferedIndexReplay, UnboundIndex};

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
    name: ByteString,
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

    pub fn name(&self) -> &ByteString {
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

/// One slot of the index list: a bound index, or one whose expressions are not bound
/// yet (DuckDB's `IndexEntry` with its `IndexBindState`).
#[derive(Clone, Debug)]
pub enum IndexSlot {
    Bound(Arc<IndexEntry>),
    Unbound(Arc<UnboundIndex>),
}

impl IndexSlot {
    /// A bound index staged outside the active list during WAL replay.
    pub fn from_bound(index: Art) -> Self {
        Self::Bound(Arc::new(IndexEntry::new(index)))
    }

    pub fn name(&self) -> &ByteString {
        match self {
            Self::Bound(entry) => entry.name(),
            Self::Unbound(index) => index.name(),
        }
    }

    pub fn column_ids(&self) -> &[usize] {
        match self {
            Self::Bound(entry) => entry.column_ids(),
            Self::Unbound(index) => index.column_ids(),
        }
    }

    pub fn is_unique(&self) -> bool {
        match self {
            Self::Bound(entry) => entry.is_unique(),
            Self::Unbound(index) => index.is_unique(),
        }
    }

    fn is_constraint(&self) -> bool {
        match self {
            Self::Bound(entry) => entry.is_primary || entry.is_foreign || entry.is_unique,
            Self::Unbound(index) => index.constraint_type() != super::IndexConstraintType::None,
        }
    }

    fn reset(&self) {
        // An unbound index owns no memory; its blocks are released by the next
        // checkpoint (`UnboundIndex::ResetStorage`).
        if let Self::Bound(entry) = self {
            entry.lock().index.reset();
        }
    }
}

/// The indexes of a table.
#[derive(Debug, Default)]
pub struct TableIndexList {
    entries: RwLock<Vec<IndexSlot>>,
    /// Serializes binding (the `BINDING` state of `TableIndexList::Bind`).
    bind_lock: Mutex<()>,
}

impl TableIndexList {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Vec<IndexSlot>> {
        self.entries.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Vec<IndexSlot>> {
        self.entries.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds an index (`AddIndex`).
    pub fn add(&self, index: Art) -> Arc<IndexEntry> {
        let entry = Arc::new(IndexEntry::new(index));
        self.write().push(IndexSlot::Bound(Arc::clone(&entry)));
        entry
    }

    /// Adds an index whose expressions are bound later (`AddIndex` of an `UnboundIndex`).
    pub fn add_unbound(&self, index: UnboundIndex) {
        self.write().push(IndexSlot::Unbound(Arc::new(index)));
    }

    /// Takes ownership without resetting buffers, to stage a WAL-created index
    /// until its creating transaction has committed.
    pub fn take_for_replay(&self, name: impl AsRef<[u8]>) -> Option<IndexSlot> {
        let mut entries = self.write();
        let position = entries
            .iter()
            .position(|entry| entry.name().as_bytes() == name.as_ref())?;
        Some(entries.remove(position))
    }

    /// Publishes the native index after the replayed transaction has committed.
    pub fn publish_replayed(&self, index: IndexSlot) {
        self.write().push(index);
    }

    /// Removes the index named `name` and frees its memory (`RemoveIndex`).
    pub fn remove(&self, name: impl AsRef<[u8]>) -> bool {
        let mut entries = self.write();
        let Some(pos) = entries.iter().position(|e| e.name().as_bytes() == name.as_ref()) else {
            return false;
        };
        let entry = entries.remove(pos);
        entry.reset();
        true
    }

    /// A snapshot of the bound indexes. Unbound indexes receive the same mutations
    /// through `buffer_chunk`, for application after their expressions are bound.
    pub fn entries(&self) -> Vec<Arc<IndexEntry>> {
        self.read()
            .iter()
            .filter_map(|slot| match slot {
                IndexSlot::Bound(entry) => Some(Arc::clone(entry)),
                IndexSlot::Unbound(_) => None,
            })
            .collect()
    }

    /// A snapshot of every index, bound or not, in order.
    pub fn slots(&self) -> Vec<IndexSlot> {
        self.read().clone()
    }

    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    /// The number of indexes, bound or not (`Count`).
    pub fn len(&self) -> usize {
        self.read().len()
    }

    /// Whether some index is not bound yet (`unbound_count != 0`).
    pub fn has_unbound(&self) -> bool {
        self.read()
            .iter()
            .any(|slot| matches!(slot, IndexSlot::Unbound(_)))
    }

    /// Whether any index enforces uniqueness.
    pub fn has_unique(&self) -> bool {
        self.read().iter().any(IndexSlot::is_unique)
    }

    /// The bound index named `name` (`Find`).
    pub fn find(&self, name: impl AsRef<[u8]>) -> Option<Arc<IndexEntry>> {
        self.read().iter().find_map(|slot| match slot {
            IndexSlot::Bound(entry) if entry.name.as_bytes() == name.as_ref() => Some(Arc::clone(entry)),
            _ => None,
        })
    }

    /// Whether an index, bound or not, is named `name`.
    pub fn contains(&self, name: impl AsRef<[u8]>) -> bool {
        self.read()
            .iter()
            .any(|slot| slot.name().as_bytes() == name.as_ref())
    }

    /// Whether no constraint index is named `name` (`NameIsUnique`).
    pub fn name_is_unique(&self, name: impl AsRef<[u8]>) -> bool {
        !self
            .read()
            .iter()
            .any(|slot| slot.is_constraint() && slot.name().as_bytes() == name.as_ref())
    }

    /// The index enforcing a foreign key over `keys` (`FindForeignKeyIndex`).
    pub fn find_foreign_key_index(&self, keys: &[usize], fk_type: ForeignKeyType) -> Option<Arc<IndexEntry>> {
        self.read().iter().find_map(|slot| match slot {
            IndexSlot::Bound(entry) if entry.is_foreign_key_index(keys, fk_type) => Some(Arc::clone(entry)),
            _ => None,
        })
    }

    /// Every physical column needed by bound or unbound indexes, sorted.
    pub fn required_columns(&self) -> Vec<usize> {
        self.read()
            .iter()
            .flat_map(|slot| slot.column_ids().iter().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Retains WAL mutations in each unbound index's own physical-column layout.
    /// Validate every replay before publishing any of them.
    pub(crate) fn buffer_chunk(
        &self,
        chunk: &DataChunk,
        row_ids: &[i64],
        kind: BufferedIndexReplay,
    ) -> Result<()> {
        let entries = self.read();
        let mut pending = Vec::new();
        for slot in entries.iter() {
            if let IndexSlot::Unbound(index) = slot {
                pending.push((index, index.prepare_replay(chunk, row_ids, kind)?));
            }
        }
        for (index, replay) in pending {
            index.push_replay(replay);
        }
        Ok(())
    }

    /// The key columns of each unique index, bound or not, in index order.
    pub fn unique_column_sets(&self) -> Vec<Vec<usize>> {
        self.read()
            .iter()
            .filter(|slot| slot.is_unique())
            .map(|slot| slot.column_ids().to_vec())
            .collect()
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
            entry.reset();
        }
        entries.clear();
    }

    /// Whether some index, bound or not, reads one of `columns` (`IndexIsUpdated`).
    pub fn any_index_on(&self, columns: &[usize]) -> bool {
        self.read()
            .iter()
            .any(|slot| slot.column_ids().iter().any(|c| columns.contains(c)))
    }

    /// Binds every unbound index with `bind`, which returns the built index
    /// (`TableIndexList::Bind`). One caller binds at a time; the others wait and find the
    /// indexes bound.
    pub fn bind(&self, bind: &mut dyn FnMut(&UnboundIndex) -> Result<Art>) -> Result<()> {
        if !self.has_unbound() {
            return Ok(());
        }
        let _binding = self.bind_lock.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let next = self.read().iter().find_map(|slot| match slot {
                IndexSlot::Unbound(index) => Some(Arc::clone(index)),
                IndexSlot::Bound(_) => None,
            });
            let Some(unbound) = next else {
                return Ok(());
            };
            let index = bind(&unbound)?;
            let mut entries = self.write();
            // The index may have been dropped meanwhile.
            if let Some(slot) = entries
                .iter_mut()
                .find(|slot| matches!(slot, IndexSlot::Unbound(other) if Arc::ptr_eq(other, &unbound)))
            {
                *slot = IndexSlot::Bound(Arc::new(IndexEntry::new(index)));
            }
        }
    }
}
