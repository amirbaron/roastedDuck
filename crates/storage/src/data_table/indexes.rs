//! Index maintenance and constraint enforcement of a table (the index half of DuckDB's
//! `DataTable`: `AddIndex`, `AppendToIndexes`, `RemoveFromIndexes`,
//! `VerifyUniqueIndexes`, `VerifyForeignKeyConstraint`) and index scans
//! (`TryScanIndex` of the table scan).

use std::collections::BTreeSet;
use std::sync::Arc;

use rustdb_common::{Error, ErrorKind, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{DataChunk, LogicalType, Vector};

use super::{DataTable, TableConstraints, constraint_error, has_null};
use crate::index::art::IndexScanPredicate;
use crate::index::conflict_manager::ConflictManager;
use crate::index::table_index_list::ForeignKeyType;
use crate::index::{
    Art, IndexAppendMode, IndexConstraintType, IndexKeyExpressions, IndexRemovalType, TableIndexList,
    VerifyExistenceType,
};
use crate::table::{
    CollectionScanState, FetchType, RowGroup, RowGroupCollection, ScanFilterInfo, StorageIndex, TableScanType,
};
use crate::table_filter::TableFilterSet;
use crate::transaction::{LocalStorage, LocalTableStorage, Transaction, TransactionData};

/// Which side of a foreign key a table is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeignKeyKind {
    /// The table holds the referencing columns: appended keys must exist in the
    /// referenced table (`IsAppendConstraint`).
    Append,
    /// The table is referenced: deleted keys must not be referenced any more
    /// (`IsDeleteConstraint`).
    Delete,
}

/// A foreign key constraint bound to the tables it connects (DuckDB's
/// `BoundForeignKeyConstraint`).
#[derive(Clone, Debug)]
pub struct BoundForeignKey {
    pub kind: ForeignKeyKind,
    /// The other table: the referenced one for [`ForeignKeyKind::Append`], the
    /// referencing one for [`ForeignKeyKind::Delete`].
    pub other: Arc<DataTable>,
    /// The key columns of this table (physical indexes), in key order.
    pub columns: Vec<usize>,
    /// The matching key columns of the other table (physical indexes).
    pub other_columns: Vec<usize>,
}

impl TableIndexList {
    /// Looks up the keys of `chunk` (in the layout of this list's table) in the index
    /// enforcing the foreign key over `keys` (`TableIndexList::VerifyForeignKey`).
    fn verify_foreign_key(
        &self,
        storage: Option<&LocalTableStorage>,
        keys: &[usize],
        chunk: &DataChunk,
        manager: &mut ConflictManager,
    ) -> Result<()> {
        let fk_type = if manager.verify_type() == VerifyExistenceType::AppendFk {
            ForeignKeyType::PrimaryKeyTable
        } else {
            ForeignKeyType::ForeignKeyTable
        };
        let entry = self
            .find_foreign_key_index(keys, fk_type)
            .ok_or_else(|| Error::internal("no index enforces the foreign key"))?;
        let state = entry.lock();
        let delete_entry = storage.and_then(|s| s.delete_indexes.find(entry.name()));
        let delete_guard = delete_entry.as_ref().map(|e| e.lock());
        let delete_arts: Vec<&Art> = delete_guard.iter().map(|g| &g.index).collect();
        state.index.verify_constraint(chunk, &delete_arts, manager)
    }
}

/// Whether the foreign key lookups found an error (`IsForeignKeyConstraintError`).
fn is_foreign_key_error(manager: &ConflictManager, is_append: bool, count: usize) -> bool {
    if is_append {
        manager.conflict_count() != count
    } else {
        manager.conflict_count() != 0
    }
}

/// The input row reported in a foreign key error (`LocateErrorIndex`).
fn locate_error_index(manager: &ConflictManager, is_append: bool, count: usize) -> usize {
    if !is_append {
        return manager.first_index().unwrap_or(0);
    }
    if !manager.has_conflicts() {
        return 0;
    }
    manager.first_invalid_index(count, true).unwrap_or(0)
}

impl DataTable {
    /// The table's indexes.
    pub fn indexes(&self) -> &TableIndexList {
        self.info.indexes()
    }

    pub fn has_indexes(&self) -> bool {
        !self.indexes().is_empty()
    }

    /// Whether some index enforces uniqueness (`HasUniqueIndexes`).
    pub fn has_unique_indexes(&self) -> bool {
        self.indexes().has_unique()
    }

    /// Whether updating `columns` changes the key of some index; such updates must be
    /// executed as a delete plus an insert (`IndexIsUpdated`).
    pub fn index_columns_updated(&self, columns: &[usize]) -> bool {
        self.indexes().any_index_on(columns)
    }

    /// A new empty ART over plain columns of this table.
    pub fn new_art(&self, name: &str, constraint: IndexConstraintType, column_ids: &[usize]) -> Result<Art> {
        let key_types: Vec<LogicalType> = column_ids
            .iter()
            .map(|&c| {
                self.columns
                    .get(c)
                    .map(|column| column.ty.clone())
                    .ok_or_else(|| Error::internal("index column out of range"))
            })
            .collect::<Result<_>>()?;
        let names = column_ids.iter().map(|&c| self.columns[c].name.clone()).collect();
        self.new_art_with_expressions(
            name,
            constraint,
            column_ids,
            key_types,
            IndexKeyExpressions::Columns(names),
        )
    }

    /// A new empty ART whose keys are computed by `expressions` from `column_ids`.
    pub fn new_art_with_expressions(
        &self,
        name: &str,
        constraint: IndexConstraintType,
        column_ids: &[usize],
        key_types: Vec<LogicalType>,
        expressions: IndexKeyExpressions,
    ) -> Result<Art> {
        let io = self.io();
        let mut art = Art::new(
            name,
            constraint,
            column_ids.to_vec(),
            key_types,
            expressions,
            io.buffer_manager().clone(),
            io.block_manager().block_size(),
        )?;
        // A new ART has the storage version of its database, one read from the file the
        // version it was stored with (`ART::ART`).
        art.set_storage_version(self.info().index_storage_version(name, io.storage_version()));
        Ok(art)
    }

    /// Adds an index enforcing a PRIMARY KEY, UNIQUE or FOREIGN KEY constraint over
    /// `column_ids` at CREATE TABLE (`DataTable::AddIndex`). Its name is
    /// `<TYPE>_<table>_<constraint index>`.
    pub fn add_constraint_index(
        &self,
        constraint: IndexConstraintType,
        column_ids: &[usize],
        constraint_index: usize,
    ) -> Result<()> {
        let name = format!("{}_{}_{constraint_index}", constraint.name(), self.name());
        let art = self.new_art(&name, constraint, column_ids)?;
        self.add_index(art)
    }

    /// Like [`DataTable::add_constraint_index`], for a table with rows (read from a
    /// checkpoint): the index is built over them.
    pub fn load_constraint_index(
        &self,
        constraint: IndexConstraintType,
        column_ids: &[usize],
        constraint_index: usize,
    ) -> Result<()> {
        let name = format!("{}_{}_{constraint_index}", constraint.name(), self.name());
        let mut art = self.new_art(&name, constraint, column_ids)?;
        self.build_index(&Transaction::system(), &mut art, false)?;
        self.add_index(art)
    }

    /// Adds a built index to the table (`DataTable::AddIndex`).
    pub fn add_index(&self, index: Art) -> Result<()> {
        self.check_main_table("add an index to")?;
        self.indexes().add(index);
        Ok(())
    }

    /// Adds an index read from storage whose expressions are bound later
    /// (`TableIndexList::AddIndex` of an `UnboundIndex`).
    pub fn add_unbound_index(&self, index: crate::index::UnboundIndex) -> Result<()> {
        self.check_main_table("add an index to")?;
        self.indexes().add_unbound(index);
        Ok(())
    }

    /// Binds every unbound index with `bind`, which returns the index built over the
    /// table's rows (`DataTableInfo::BindIndexes`).
    pub fn bind_indexes(
        &self,
        bind: &mut dyn FnMut(&crate::index::UnboundIndex) -> Result<Art>,
    ) -> Result<()> {
        self.indexes().bind(bind)
    }

    /// Compacts the memory of every bound index (`DataTable::VacuumIndexes`).
    pub fn vacuum_indexes(&self) -> Result<()> {
        for entry in self.indexes().entries() {
            entry.lock().index.vacuum()?;
        }
        Ok(())
    }

    /// Removes the index `name` (DROP INDEX, or rollback of CREATE INDEX).
    pub fn remove_index(&self, name: &str) -> bool {
        self.indexes().remove(name)
    }

    /// Builds `index` over the table's rows and adds it (`PhysicalCreateIndex`). Rows the
    /// transaction inserted are added to the index when it commits. With
    /// `check_unique_name`, another index of the table with the same name (e.g. the one of
    /// a primary key) is an error (`DuckSchemaEntry::CreateIndex`).
    pub fn create_index(
        self: &Arc<Self>,
        transaction: &Transaction,
        mut index: Art,
        check_unique_name: bool,
    ) -> Result<()> {
        self.build_index(transaction, &mut index, false)?;
        self.check_main_table_for_index_creation()?;
        if check_unique_name && self.indexes().contains(index.name()) {
            return Err(Error::catalog(format!(
                "An index with the name {} already exists!",
                index.name()
            )));
        }
        self.indexes().add(index);
        Ok(())
    }

    /// Fills `index` with the table's committed rows. With `reject_nulls` (ALTER TABLE
    /// ADD PRIMARY KEY), a NULL key is an error naming the index.
    pub fn build_index(&self, transaction: &Transaction, index: &mut Art, reject_nulls: bool) -> Result<()> {
        let columns = Self::index_columns(index);
        let column_ids: Vec<StorageIndex> = columns
            .iter()
            .map(|&c| StorageIndex::from(c))
            .chain(std::iter::once(StorageIndex::row_id()))
            .collect();
        let mut types: Vec<LogicalType> = columns.iter().map(|&c| self.columns[c].ty.clone()).collect();
        types.push(LogicalType::BIGINT);
        let mut state = CollectionScanState::default();
        let mut filters = ScanFilterInfo::default();
        self.row_groups
            .initialize_scan(&mut state, &column_ids, &mut filters)?;
        let mut chunk = DataChunk::with_types(&types, STANDARD_VECTOR_SIZE);
        while self.row_groups.scan_all(
            TableScanType::OmitPermanentlyDeleted,
            transaction.oldest_active_snapshot(),
            &mut state,
            &column_ids,
            &mut filters,
            &mut chunk,
        )? {
            let row_id_column = chunk.column(columns.len()).flattened(chunk.len());
            let row_ids = row_id_column.values::<i64>()[..chunk.len()].to_vec();
            let data = DataChunk::new(chunk.columns()[..columns.len()].to_vec(), chunk.len());
            if reject_nulls && data.columns().iter().any(|column| has_null(column, data.len())) {
                return Err(constraint_error(format!(
                    "NOT NULL constraint failed: {}",
                    index.name()
                )));
            }
            let table_chunk = self.index_table_chunk(&columns, &data);
            if let Err(error) = index.append(&table_chunk, &row_ids, &[], IndexAppendMode::Default) {
                if error.kind() == ErrorKind::Constraint {
                    return Err(constraint_error(
                        "Data contains duplicates on indexed column(s)".to_owned(),
                    ));
                }
                return Err(error);
            }
        }
        // `PhysicalCreateIndex::Finalize`: vacuum the excess memory of the build.
        index.vacuum()
    }

    /// `DataTable::RebuildIndexes`: the table's indexes built anew over the committed rows
    /// of `layout`, the row groups a vacuum leaves with their first rows (relative to the
    /// base row id), so that they hold the row ids the rows have after it.
    pub(crate) fn rebuild_indexes(&self, layout: &[(Arc<RowGroup>, u64)]) -> Result<Vec<Art>> {
        let base = self.row_groups.row_groups().base_row_id();
        let options = RowGroup::scan_options(
            TableScanType::CommittedRows,
            TransactionData::new(0, crate::transaction::TRANSACTION_ID_START),
        );
        let mut rebuilt = Vec::new();
        for entry in self.indexes().entries() {
            let mut index = entry.lock().index.empty_like()?;
            let columns = Self::index_columns(&index);
            let column_ids: Vec<StorageIndex> = columns
                .iter()
                .map(|&c| StorageIndex::from(c))
                .chain(std::iter::once(StorageIndex::row_id()))
                .collect();
            let mut types: Vec<LogicalType> = columns.iter().map(|&c| self.columns[c].ty.clone()).collect();
            types.push(LogicalType::BIGINT);
            for (row_group, row_start) in layout {
                let mut filters = ScanFilterInfo::default();
                let mut scan = CollectionScanState {
                    max_row: u64::MAX,
                    column_scans: RowGroup::column_scan_states(self.row_groups.types(), &column_ids, None)?,
                    ..CollectionScanState::default()
                };
                if !row_group.initialize_scan(&mut scan, &column_ids, &mut filters, 0, 0)? {
                    continue;
                }
                let mut chunk = DataChunk::with_types(&types, STANDARD_VECTOR_SIZE);
                loop {
                    chunk.reset();
                    row_group.scan(options, &mut scan, &column_ids, &mut filters, base + row_start, &mut chunk)?;
                    if chunk.is_empty() {
                        break;
                    }
                    let row_id_column = chunk.column(columns.len()).flattened(chunk.len());
                    let row_ids = row_id_column.values::<i64>()[..chunk.len()].to_vec();
                    let data = DataChunk::new(chunk.columns()[..columns.len()].to_vec(), chunk.len());
                    let table_chunk = self.index_table_chunk(&columns, &data);
                    index
                        .append(&table_chunk, &row_ids, &[], IndexAppendMode::Default)
                        .map_err(|error| {
                            Error::internal(format!(
                                "Failed to rebuild index '{}' after vacuum: {}",
                                index.name(),
                                error.raw_message()
                            ))
                        })?;
                }
            }
            rebuilt.push(index);
        }
        Ok(rebuilt)
    }

    /// Replaces the table's indexes by `rebuilt` (from [`DataTable::rebuild_indexes`], in
    /// the same order) once the vacuumed row groups are the table's.
    pub(crate) fn install_rebuilt_indexes(&self, rebuilt: Vec<Art>) {
        for (entry, index) in self.indexes().entries().into_iter().zip(rebuilt) {
            let mut state = entry.lock();
            state.index = index;
            state.deleted_rows_in_use = None;
        }
    }

    /// The distinct columns an index reads, sorted.
    fn index_columns(index: &Art) -> Vec<usize> {
        let mut columns = index.column_ids().to_vec();
        columns.sort_unstable();
        columns.dedup();
        columns
    }

    /// Rejects adding an index to a version of the table that another transaction
    /// altered or dropped (`PhysicalCreateIndex::Finalize`).
    pub fn check_main_table_for_index_creation(&self) -> Result<()> {
        if self.is_main_table() {
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Transaction,
            "Transaction conflict: cannot add an index to a table that has been altered or dropped",
        ))
    }

    /// A chunk with every table column where `columns` come from `data` and the others
    /// are NULL constants (the "mock" table chunk handed to indexes).
    pub(crate) fn index_table_chunk(&self, columns: &[usize], data: &DataChunk) -> DataChunk {
        let mut vectors: Vec<Vector> = self
            .columns
            .iter()
            .map(|column| Vector::constant_null(column.ty.clone()))
            .collect();
        for (position, &column) in columns.iter().enumerate() {
            if let (Some(slot), Some(vector)) = (vectors.get_mut(column), data.columns().get(position)) {
                *slot = vector.clone();
            }
        }
        DataChunk::new(vectors, data.len())
    }

    /// Reads columns `columns` of the rows `row_ids` of `collection`, deleted or not.
    pub(crate) fn fetch_columns(
        &self,
        collection: &RowGroupCollection,
        columns: &[usize],
        row_ids: &[i64],
    ) -> Result<DataChunk> {
        let column_ids: Vec<StorageIndex> = columns.iter().map(|&c| StorageIndex::from(c)).collect();
        let types: Vec<LogicalType> = columns.iter().map(|&c| self.columns[c].ty.clone()).collect();
        let mut result = DataChunk::with_types(&types, row_ids.len().max(1));
        collection.fetch(
            TransactionData::latest_committed(),
            &mut result,
            &column_ids,
            row_ids,
            FetchType::Force,
        )?;
        Ok(result)
    }

    /// Appends the rows of `table_chunk` with row ids from `row_start` to every index of
    /// `indexes`; if one fails, the others are reverted (`DataTable::AppendToIndexes`).
    pub(crate) fn append_to_indexes(
        indexes: &TableIndexList,
        delete_indexes: Option<&TableIndexList>,
        table_chunk: &DataChunk,
        row_start: i64,
        mode: IndexAppendMode,
    ) -> Result<()> {
        let row_ids: Vec<i64> = (0..table_chunk.len() as i64).map(|i| row_start + i).collect();
        let mut appended: Vec<Arc<crate::index::IndexEntry>> = Vec::new();
        for entry in indexes.entries() {
            let result = {
                let mut state = entry.lock();
                let delete_entry = if state.index.is_unique() {
                    delete_indexes.and_then(|list| list.find(entry.name()))
                } else {
                    None
                };
                let delete_guard = delete_entry.as_ref().map(|e| e.lock());
                let delete_arts: Vec<&Art> = delete_guard.iter().map(|g| &g.index).collect();
                state.index.append(table_chunk, &row_ids, &delete_arts, mode)
            };
            if let Err(error) = result {
                for previous in &appended {
                    previous.lock().index.try_delete(table_chunk, &row_ids)?;
                }
                return Err(error);
            }
            appended.push(entry);
        }
        Ok(())
    }

    /// Verifies that the rows of `chunk` do not violate a unique index; keys deleted by
    /// the transaction (in `storage`'s delete indexes) do not count
    /// (`VerifyUniqueIndexes` without a conflict manager).
    pub(crate) fn verify_unique_indexes(
        &self,
        storage: Option<&LocalTableStorage>,
        chunk: &DataChunk,
    ) -> Result<()> {
        for entry in self.indexes().entries() {
            if !entry.is_unique() {
                continue;
            }
            let state = entry.lock();
            let delete_entry = storage.and_then(|s| s.delete_indexes.find(entry.name()));
            let delete_guard = delete_entry.as_ref().map(|e| e.lock());
            let delete_arts: Vec<&Art> = delete_guard.iter().map(|g| &g.index).collect();
            state.index.verify_append(chunk, &delete_arts, None)?;
        }
        Ok(())
    }

    /// Finds the committed rows that the rows of `chunk` conflict with on the unique
    /// indexes over `conflict_target` (all unique indexes if `None`), for `INSERT ... ON
    /// CONFLICT`. Conflicts on other unique indexes raise the usual error. The result maps
    /// input rows to the row ids they conflict with.
    pub fn find_conflicts(
        self: &Arc<Self>,
        transaction: &Transaction,
        chunk: &DataChunk,
        conflict_target: Option<&[usize]>,
    ) -> Result<Vec<(usize, i64)>> {
        let storage = transaction.local_storage().storage(self);
        let matches_target = |entry: &crate::index::IndexEntry| match conflict_target {
            None => true,
            Some(columns) => {
                let mut a = entry.column_ids().to_vec();
                let mut b = columns.to_vec();
                a.sort_unstable();
                b.sort_unstable();
                a == b
            }
        };
        let single_index = conflict_target.is_some();
        let mut manager = ConflictManager::scanning(VerifyExistenceType::Append, chunk.len(), single_index);
        let entries: Vec<_> = self
            .indexes()
            .entries()
            .into_iter()
            .filter(|e| e.is_unique())
            .collect();
        for pass_matching in [true, false] {
            if !pass_matching {
                manager.set_mode(crate::index::conflict_manager::ConflictManagerMode::Throw);
            }
            for entry in &entries {
                if matches_target(entry) != pass_matching {
                    continue;
                }
                if pass_matching {
                    manager.add_index(entry.name());
                }
                let state = entry.lock();
                let delete_entry = storage.as_ref().and_then(|s| s.delete_indexes.find(entry.name()));
                let delete_guard = delete_entry.as_ref().map(|e| e.lock());
                let delete_arts: Vec<&Art> = delete_guard.iter().map(|g| &g.index).collect();
                state
                    .index
                    .verify_append(chunk, &delete_arts, Some(&mut manager))?;
            }
        }
        let data = transaction.data();
        let mut visible = |row_id: i64| self.row_groups.can_fetch(data, row_id).unwrap_or(false);
        manager.finalize(&mut visible);
        Ok(manager.conflicts())
    }

    /// Verifies a foreign key for the rows of `chunk` (all columns of this table)
    /// (`DataTable::VerifyForeignKeyConstraint`).
    pub(crate) fn verify_foreign_key(
        local_storage: &LocalStorage,
        storage: Option<&LocalTableStorage>,
        foreign_key: &BoundForeignKey,
        chunk: &DataChunk,
    ) -> Result<()> {
        let is_append = foreign_key.kind == ForeignKeyKind::Append;
        let verify_type = if is_append {
            VerifyExistenceType::AppendFk
        } else {
            VerifyExistenceType::DeleteFk
        };
        let count = chunk.len();
        if count == 0 {
            return Ok(());
        }
        let other = &foreign_key.other;
        let mut vectors: Vec<Vector> = other.types().into_iter().map(Vector::constant_null).collect();
        for (&source, &target) in foreign_key.columns.iter().zip(&foreign_key.other_columns) {
            if let Some(slot) = vectors.get_mut(target) {
                *slot = chunk.column(source).clone();
            }
        }
        let dst_chunk = DataChunk::new(vectors, count);
        let keys = &foreign_key.other_columns;

        let mut global = ConflictManager::scanning(verify_type, count, false);
        other
            .indexes()
            .verify_foreign_key(storage, keys, &dst_chunk, &mut global)?;
        let other_local = local_storage.storage(other);
        let mut local = ConflictManager::scanning(verify_type, count, false);
        let local_error = match &other_local {
            Some(other_local) => {
                other_local
                    .append_indexes
                    .verify_foreign_key(storage, keys, &dst_chunk, &mut local)?;
                is_foreign_key_error(&local, is_append, count)
            }
            None => false,
        };
        let global_error = is_foreign_key_error(&global, is_append, count);
        if !global_error && !local_error {
            return Ok(());
        }
        let fk_type = if is_append {
            ForeignKeyType::PrimaryKeyTable
        } else {
            ForeignKeyType::ForeignKeyTable
        };
        let index_entry = other
            .indexes()
            .find_foreign_key_index(keys, fk_type)
            .ok_or_else(|| Error::internal("no index enforces the foreign key"))?;
        let message = |entry: &crate::index::IndexEntry, conflict: usize| -> Result<Error> {
            let text = entry
                .lock()
                .index
                .constraint_violation_message(verify_type, conflict, &dst_chunk)?;
            Ok(constraint_error(text))
        };
        let Some(other_local) = other_local else {
            let conflict = locate_error_index(&global, is_append, count);
            return Err(message(&index_entry, conflict)?);
        };
        if local_error && global_error && is_append {
            let conflict = match (global.has_conflicts(), local.has_conflicts()) {
                (false, false) => Some(0),
                (false, true) => local.first_invalid_index(count, false),
                (true, false) => global.first_invalid_index(count, false),
                (true, true) => (0..count).find(|&row| !global.is_hit(row) && !local.is_hit(row)),
            };
            return match conflict {
                Some(conflict) => Err(message(&index_entry, conflict)?),
                None => Ok(()),
            };
        }
        if !is_append {
            if global_error {
                let conflict = locate_error_index(&global, false, count);
                return Err(message(&index_entry, conflict)?);
            }
            let local_entry = other_local
                .append_indexes
                .find_foreign_key_index(keys, fk_type)
                .ok_or_else(|| Error::internal("no local index enforces the foreign key"))?;
            let conflict = locate_error_index(&local, false, count);
            return Err(message(&local_entry, conflict)?);
        }
        Ok(())
    }

    /// Verifies every append-side foreign key of `constraints` for `chunk`.
    pub(crate) fn verify_append_foreign_keys(
        local_storage: &LocalStorage,
        storage: Option<&LocalTableStorage>,
        constraints: &TableConstraints,
        chunk: &DataChunk,
    ) -> Result<()> {
        for foreign_key in &constraints.foreign_keys {
            if foreign_key.kind == ForeignKeyKind::Append {
                Self::verify_foreign_key(local_storage, storage, foreign_key, chunk)?;
            }
        }
        Ok(())
    }

    /// Verifies every delete-side foreign key of `constraints` for the deleted rows
    /// `chunk` (`VerifyDeleteConstraints`).
    pub(crate) fn verify_delete_foreign_keys(
        local_storage: &LocalStorage,
        storage: Option<&LocalTableStorage>,
        constraints: &TableConstraints,
        chunk: &DataChunk,
    ) -> Result<()> {
        for foreign_key in &constraints.foreign_keys {
            if foreign_key.kind == ForeignKeyKind::Delete {
                Self::verify_foreign_key(local_storage, storage, foreign_key, chunk)?;
            }
        }
        Ok(())
    }

    /// Records the keys of committed rows deleted by the transaction in its delete
    /// indexes, so that they can be inserted again (`AppendToDeleteIndexes`).
    pub(crate) fn append_to_delete_indexes(
        &self,
        storage: &LocalTableStorage,
        row_ids: &[i64],
    ) -> Result<()> {
        let committed: Vec<i64> = row_ids
            .iter()
            .copied()
            .filter(|&id| (id as u64) < crate::MAX_ROW_ID)
            .collect();
        if committed.is_empty() || storage.delete_indexes.is_empty() {
            return Ok(());
        }
        let columns = storage.delete_indexes.required_columns();
        let fetched = self.fetch_columns(&self.row_groups, &columns, &committed)?;
        let table_chunk = self.index_table_chunk(&columns, &fetched);
        for entry in storage.delete_indexes.entries() {
            if !entry.is_unique() {
                continue;
            }
            entry
                .lock()
                .index
                .append(&table_chunk, &committed, &[], IndexAppendMode::IgnoreDuplicates)
                .map_err(|e| {
                    Error::internal(format!(
                        "unexpected constraint violation on delete ART: {}",
                        e.raw_message()
                    ))
                })?;
        }
        Ok(())
    }

    /// Removes deleted rows from the indexes at commit, or re-adds them when the commit
    /// is reverted (`RemoveFromIndexes` with the targets of `GetIndexRemovalTargets`).
    pub(crate) fn remove_from_indexes(&self, row_ids: &[i64], removal: IndexRemovalType) -> Result<()> {
        if !self.has_indexes() || row_ids.is_empty() {
            return Ok(());
        }
        // A checkpoint writing the table writes the indexes as of when it started: the
        // removal waits for it.
        let _checkpoint =
            (removal != IndexRemovalType::DeletedRowsInUse).then(|| self.info().checkpoint_lock().shared());
        let columns = self.indexes().required_columns();
        let fetched = self.fetch_columns(&self.row_groups, &columns, row_ids)?;
        let table_chunk = self.index_table_chunk(&columns, &fetched);
        for entry in self.indexes().entries() {
            let mut state = entry.lock();
            let state = &mut *state;
            match removal {
                IndexRemovalType::MainIndexOnly => {
                    state.index.delete(&table_chunk, row_ids)?;
                }
                IndexRemovalType::RevertMainIndexOnly => {
                    state
                        .index
                        .append(&table_chunk, row_ids, &[], IndexAppendMode::InsertDuplicates)?;
                }
                IndexRemovalType::MainIndex => {
                    if state.deleted_rows_in_use.is_none() {
                        state.deleted_rows_in_use = Some(state.index.delta(true)?);
                    }
                    if let Some(in_use) = state.deleted_rows_in_use.as_mut() {
                        in_use.append(&table_chunk, row_ids, &[], IndexAppendMode::Default)?;
                    }
                    state.index.delete(&table_chunk, row_ids)?;
                }
                IndexRemovalType::RevertMainIndex => {
                    state
                        .index
                        .append(&table_chunk, row_ids, &[], IndexAppendMode::InsertDuplicates)?;
                    if let Some(in_use) = state.deleted_rows_in_use.as_mut() {
                        in_use.delete(&table_chunk, row_ids)?;
                    }
                }
                IndexRemovalType::DeletedRowsInUse => {
                    if let Some(in_use) = state.deleted_rows_in_use.as_mut() {
                        in_use.try_delete(&table_chunk, row_ids)?;
                        if in_use.is_empty() {
                            state.deleted_rows_in_use = None;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Removes rows `row_start..row_start + count` from the indexes before their append is
    /// reverted (the index part of `DataTable::RevertAppend`).
    pub(crate) fn revert_index_append(&self, row_start: u64, count: u64) -> Result<()> {
        if !self.has_indexes() {
            return Ok(());
        }
        let total = self.row_groups.total_rows();
        let end = (row_start + count).min(total);
        let columns = self.indexes().required_columns();
        let mut start = row_start;
        while start < end {
            let batch = (end - start).min(STANDARD_VECTOR_SIZE as u64);
            let row_ids: Vec<i64> = (start..start + batch).map(|id| id as i64).collect();
            let fetched = self.fetch_columns(&self.row_groups, &columns, &row_ids)?;
            let table_chunk = self.index_table_chunk(&columns, &fetched);
            for entry in self.indexes().entries() {
                entry.lock().index.try_delete(&table_chunk, &row_ids)?;
            }
            start += batch;
        }
        Ok(())
    }

    /// The row ids an index scan yields for the filters of a scan over `column_ids`, or
    /// `None` if no index applies or more than `max_count` rows match (`TryScanIndex`).
    /// Rows deleted by committed transactions that older transactions still see are
    /// included; fetch the row ids with [`DataTable::fetch`] for visibility, and scan the
    /// transaction-local rows separately.
    pub fn try_index_scan(
        &self,
        column_ids: &[StorageIndex],
        filters: &TableFilterSet,
        max_count: usize,
    ) -> Result<Option<Vec<i64>>> {
        if filters.filters.len() != 1 {
            return Ok(None);
        }
        for entry in self.indexes().entries() {
            if entry.column_ids().len() != 1 {
                continue;
            }
            let state = entry.lock();
            // No index scans on compound keys.
            if state.index.key_types().len() != 1 {
                continue;
            }
            let indexed = entry.column_ids()[0];
            let Some(position) = column_ids
                .iter()
                .position(|c| !c.is_row_id() && c.primary_index() == indexed && c.children.is_empty())
            else {
                continue;
            };
            let Some(filter) = filters.filters.get(&position) else {
                continue;
            };
            let predicates = match state.index.expressions() {
                IndexKeyExpressions::Columns(_) => IndexScanPredicate::from_filter(filter),
                IndexKeyExpressions::Custom(executor) => executor.scan_predicates(filter),
            };
            let Some(predicates) = predicates else {
                continue;
            };
            let mut row_ids = BTreeSet::new();
            let mut complete = true;
            for predicate in &predicates {
                let arts = std::iter::once(&state.index).chain(state.deleted_rows_in_use.as_ref());
                for art in arts {
                    if !art.scan(predicate, max_count, &mut row_ids)? {
                        complete = false;
                    }
                }
            }
            if !complete {
                continue;
            }
            return Ok(Some(row_ids.into_iter().collect()));
        }
        Ok(None)
    }
}
