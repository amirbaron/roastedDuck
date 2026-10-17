//! Rows that one thread of a parallel insert appends on its own, before they are merged
//! into the transaction-local storage (DuckDB's `OptimisticWriteCollection` and
//! `LocalStorage::LocalMerge`).
//!
//! DuckDB's `OptimisticDataWriter` also writes full row groups of a persistent table to
//! disk while the insert runs; rustdb keeps them in memory until the checkpoint, like the
//! rows of a serial insert.

use std::sync::Arc;

use rustdb_common::{Result, STANDARD_VECTOR_SIZE};
use rustdb_types::DataChunk;

use super::{DataTable, LocalAppendState};
use crate::MAX_ROW_ID;
use crate::table::{
    CollectionScanState, RowGroupCollection, ScanFilterInfo, ScanOptions, StorageIndex, TableAppendState,
};
use crate::transaction::{Transaction, TransactionData};

/// A thread-local row group collection of a table being inserted into.
#[derive(Debug)]
pub struct OptimisticCollection {
    collection: RowGroupCollection,
    /// The append in progress, if any.
    append: Option<TableAppendState>,
    transaction: TransactionData,
}

impl OptimisticCollection {
    /// Appends `chunk`; returns whether a new row group was started (`Append`).
    pub fn append(&mut self, chunk: &DataChunk) -> Result<bool> {
        if self.append.is_none() {
            self.append = Some(self.collection.initialize_append(self.transaction)?);
        }
        match &mut self.append {
            Some(append) => self.collection.append(chunk, append),
            None => Ok(false),
        }
    }

    /// Publishes the appended rows (`FinalizeAppend`); further appends start a new one.
    pub fn finalize(&mut self) {
        if let Some(mut append) = self.append.take() {
            self.collection.finalize_append(self.transaction, &mut append);
        }
    }

    /// Rows in the collection (finalized appends only).
    pub fn total_rows(&self) -> u64 {
        self.collection.total_rows()
    }

    /// Appends every row of `other` (`CollectionMerger::Flush`).
    pub fn append_collection(&mut self, other: &Self) -> Result<()> {
        other.scan(|chunk| self.append(chunk).map(|_| ()))?;
        self.finalize();
        Ok(())
    }

    /// Calls `f` with the rows in chunks, in order (`Chunks`).
    pub fn scan(&self, mut f: impl FnMut(&DataChunk) -> Result<()>) -> Result<()> {
        let types = self.collection.types();
        let column_ids: Vec<StorageIndex> = (0..types.len()).map(StorageIndex::from).collect();
        let mut filters = ScanFilterInfo::default();
        let mut state = CollectionScanState::default();
        self.collection
            .initialize_scan(&mut state, &column_ids, &mut filters)?;
        let mut chunk = DataChunk::with_types(types, STANDARD_VECTOR_SIZE);
        while RowGroupCollection::scan(
            ScanOptions::new(self.transaction),
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

impl DataTable {
    /// An empty thread-local collection for a parallel insert into this table
    /// (`OptimisticDataWriter::CreateCollection`).
    pub fn create_optimistic_collection(&self, transaction: &Transaction) -> Result<OptimisticCollection> {
        self.check_main_table("insert into")?;
        Ok(OptimisticCollection {
            collection: RowGroupCollection::new(Arc::clone(self.io()), self.types(), MAX_ROW_ID),
            append: None,
            transaction: transaction.data(),
        })
    }

    /// Moves the row groups of a finalized thread-local collection to the end of the
    /// transaction-local storage (`LocalStorage::LocalMerge`). The table must have no
    /// indexes: parallel inserts are only planned for tables without them.
    pub fn local_merge(
        self: &Arc<Self>,
        transaction: &Transaction,
        collection: &OptimisticCollection,
    ) -> Result<()> {
        self.check_main_table("insert into")?;
        let storage = transaction.local_storage().get_or_create(self)?;
        storage.collection().merge_storage(&collection.collection)
    }

    /// Appends the rows of a thread-local collection through a local append, for
    /// collections smaller than a row group (`PhysicalInsert::Combine`).
    pub fn local_append_collection(
        &self,
        state: &mut LocalAppendState,
        collection: &OptimisticCollection,
    ) -> Result<()> {
        collection.scan(|chunk| self.local_append(state, chunk))
    }
}
