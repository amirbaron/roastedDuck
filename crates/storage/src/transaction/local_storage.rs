//! Transaction-local storage (DuckDB's `LocalStorage` and `LocalTableStorage`).
//!
//! Rows a transaction inserts are kept in a private row group collection per table, with
//! row ids starting at [`MAX_ROW_ID`]. Deletes and updates of those rows apply directly.
//! At commit the rows are appended to the table (or, for bulk loads into an empty table,
//! their row groups are moved over without copying).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_common::Result;
use rustdb_types::DataChunk;

use super::{Transaction, TransactionData};
use crate::MAX_ROW_ID;
use crate::data_table::DataTable;
use crate::table::{CollectionScanState, RowGroupCollection, ScanFilterInfo, ScanOptions, StorageIndex};

/// The uncommitted inserts of one transaction into one table.
#[derive(Debug)]
pub struct LocalTableStorage {
    table: Mutex<Arc<DataTable>>,
    collection: Mutex<Arc<RowGroupCollection>>,
    deleted_rows: AtomicU64,
    dropped: AtomicBool,
}

impl LocalTableStorage {
    fn new(table: &Arc<DataTable>) -> Self {
        let collection = RowGroupCollection::new(Arc::clone(table.io()), table.types(), MAX_ROW_ID);
        Self {
            table: Mutex::new(Arc::clone(table)),
            collection: Mutex::new(Arc::new(collection)),
            deleted_rows: AtomicU64::new(0),
            dropped: AtomicBool::new(false),
        }
    }

    /// The table the rows belong to.
    pub fn table(&self) -> Arc<DataTable> {
        Arc::clone(&self.table.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// The local rows.
    pub fn collection(&self) -> Arc<RowGroupCollection> {
        Arc::clone(&self.collection.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Replaces the local rows (after ALTER TABLE rewrote them).
    pub fn set_collection(&self, collection: Arc<RowGroupCollection>) {
        *self.collection.lock().unwrap_or_else(PoisonError::into_inner) = collection;
    }

    fn set_table(&self, table: Arc<DataTable>) {
        *self.table.lock().unwrap_or_else(PoisonError::into_inner) = table;
    }

    /// Local rows deleted again.
    pub fn deleted_rows(&self) -> u64 {
        self.deleted_rows.load(Ordering::Acquire)
    }

    /// Inserted rows that are not deleted.
    pub fn added_rows(&self) -> u64 {
        self.collection().total_rows() - self.deleted_rows()
    }

    /// Appends the local rows to the table and registers the append in the undo log
    /// (`LocalStorage::Flush`).
    fn flush(&self, transaction: &Transaction) -> Result<()> {
        if self.dropped.load(Ordering::Acquire) {
            return Ok(());
        }
        let collection = self.collection();
        let deleted = self.deleted_rows();
        if collection.total_rows() <= deleted {
            return Ok(());
        }
        let append_count = collection.total_rows() - deleted;
        let table = self.table();
        let append_lock = table.append_lock();
        table.check_main_table("insert into")?;
        let row_start = table.total_rows();
        let bulk = row_start == 0 || collection.total_rows() >= collection.row_group_size();
        if bulk && deleted == 0 {
            table.row_groups().merge_storage(&collection)?;
        } else {
            let mut append = table.row_groups().initialize_append(transaction.data())?;
            let result = Self::scan_visible(transaction.data(), &collection, |chunk| {
                table.row_groups().append(chunk, &mut append).map(|_| ())
            });
            table
                .row_groups()
                .finalize_append(transaction.data(), &mut append);
            result?;
        }
        drop(append_lock);
        transaction.push_append(Arc::clone(&table), row_start, append_count);
        Ok(())
    }

    /// Scans every local row visible to the transaction (`Chunks`).
    fn scan_visible(
        transaction: TransactionData,
        collection: &RowGroupCollection,
        mut f: impl FnMut(&DataChunk) -> Result<()>,
    ) -> Result<()> {
        let column_ids: Vec<StorageIndex> = (0..collection.types().len()).map(StorageIndex::from).collect();
        let mut filters = ScanFilterInfo::default();
        let mut state = CollectionScanState::default();
        collection.initialize_scan(&mut state, &column_ids, &mut filters)?;
        let mut chunk = DataChunk::with_types(collection.types(), rustdb_common::STANDARD_VECTOR_SIZE);
        while RowGroupCollection::scan(
            ScanOptions::new(transaction),
            &mut state,
            &column_ids,
            &mut filters,
            &mut chunk,
        )? {
            f(&chunk)?;
        }
        Ok(())
    }
}

/// The transaction-local storage of every table a transaction inserted into.
#[derive(Debug, Default)]
pub struct LocalStorage {
    tables: Mutex<Vec<Arc<LocalTableStorage>>>,
}

impl LocalStorage {
    fn tables(&self) -> MutexGuard<'_, Vec<Arc<LocalTableStorage>>> {
        self.tables.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The local storage of `table`, if the transaction inserted into it.
    pub fn storage(&self, table: &Arc<DataTable>) -> Option<Arc<LocalTableStorage>> {
        self.tables()
            .iter()
            .find(|storage| Arc::ptr_eq(&storage.table(), table))
            .cloned()
    }

    /// The local storage of `table`, created on first use.
    pub fn get_or_create(&self, table: &Arc<DataTable>) -> Arc<LocalTableStorage> {
        let mut tables = self.tables();
        if let Some(storage) = tables.iter().find(|storage| Arc::ptr_eq(&storage.table(), table)) {
            return Arc::clone(storage);
        }
        let storage = Arc::new(LocalTableStorage::new(table));
        tables.push(Arc::clone(&storage));
        storage
    }

    /// Whether any table has local data (`ChangesMade`).
    pub fn changes_made(&self) -> bool {
        !self.tables().is_empty()
    }

    /// Rows the transaction inserted into `table` and did not delete (`AddedRows`).
    pub fn added_rows(&self, table: &Arc<DataTable>) -> u64 {
        self.storage(table).map_or(0, |storage| storage.added_rows())
    }

    /// Deletes local rows (`LocalStorage::Delete`); `ids` are local row ids.
    pub fn delete(&self, table: &Arc<DataTable>, ids: &[i64]) -> Result<u64> {
        let Some(storage) = self.storage(table) else {
            return Ok(0);
        };
        let (count, _) = storage.collection().delete(TransactionData::local(), ids)?;
        storage.deleted_rows.fetch_add(count, Ordering::AcqRel);
        Ok(count)
    }

    /// Updates local rows (`LocalStorage::Update`).
    pub fn update(
        &self,
        table: &Arc<DataTable>,
        ids: &[i64],
        column_ids: &[usize],
        updates: &DataChunk,
    ) -> Result<()> {
        let Some(storage) = self.storage(table) else {
            return Ok(());
        };
        storage
            .collection()
            .update(TransactionData::local(), ids, column_ids, updates)?;
        Ok(())
    }

    /// Marks the local data of a dropped table as discarded (`DropTable`).
    pub fn drop_table(&self, table: &Arc<DataTable>) {
        if let Some(storage) = self.storage(table) {
            storage.dropped.store(true, Ordering::Release);
        }
    }

    /// Moves the local data of `old` to `new` after an ALTER TABLE (`MoveStorage`).
    pub fn move_storage(&self, old: &Arc<DataTable>, new: &Arc<DataTable>) {
        if let Some(storage) = self.storage(old) {
            storage.set_table(Arc::clone(new));
        }
    }

    /// Flushes every table's local data into the table (`LocalStorage::Commit`).
    pub(crate) fn commit(&self, transaction: &Transaction) -> Result<()> {
        let tables = std::mem::take(&mut *self.tables());
        for storage in tables {
            storage.flush(transaction)?;
        }
        Ok(())
    }

    /// Discards all local data (`LocalStorage::Rollback`).
    pub(crate) fn rollback(&self) {
        self.tables().clear();
    }
}
