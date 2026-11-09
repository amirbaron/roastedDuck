//! Rows that one thread of a parallel insert appends on its own, before they are merged
//! into the transaction-local storage (DuckDB's `OptimisticWriteCollection` as
//! `PhysicalInsert` and `PhysicalBatchInsert` use it, and `LocalStorage::LocalMerge`).
//! The row groups the thread completes go to its [`OptimisticDataWriter`].

use std::sync::Arc;

use roastedduck_common::{Result, STANDARD_VECTOR_SIZE};
use roastedduck_types::DataChunk;

use super::{DataTable, LocalAppendState};
use crate::MAX_ROW_ID;
use crate::optimistic_data_writer::{OptimisticDataWriter, OptimisticWriteCollection};
use crate::table::{
    CollectionScanState, RowGroupCollection, ScanFilterInfo, ScanOptions, StorageIndex, TableAppendState,
};
use crate::transaction::{Transaction, TransactionData};

/// A thread-local row group collection of a table being inserted into.
#[derive(Debug)]
pub struct OptimisticCollection {
    row_groups: OptimisticWriteCollection,
    /// The append in progress, if any.
    append: Option<TableAppendState>,
    transaction: TransactionData,
}

impl OptimisticCollection {
    /// Appends `chunk`; a row group it completes goes to `writer` (`Append` and
    /// `WriteNewRowGroup`).
    pub fn append(&mut self, chunk: &DataChunk, writer: &mut OptimisticDataWriter) -> Result<()> {
        let collection = Arc::clone(self.row_groups.collection());
        let append = match &mut self.append {
            Some(append) => append,
            None => self
                .append
                .insert(collection.initialize_append(self.transaction)?),
        };
        if collection.append(chunk, append)? {
            writer.write_new_row_group(&mut self.row_groups)?;
        }
        Ok(())
    }

    /// Publishes the appended rows (`FinalizeAppend`); further appends start a new one.
    pub fn finalize(&mut self) {
        if let Some(mut append) = self.append.take() {
            self.row_groups
                .collection()
                .finalize_append(self.transaction, &mut append);
        }
    }

    /// Rows in the collection (finalized appends only).
    pub fn total_rows(&self) -> u64 {
        self.row_groups.collection().total_rows()
    }

    /// The bytes the collection's row groups take in memory (`GetAllocationSize`).
    pub fn allocation_size(&self) -> u64 {
        self.row_groups.collection().allocation_size()
    }

    /// Writes every row group not written yet with `writer` (`WriteUnflushedRowGroups`).
    pub fn write_unflushed_row_groups(&mut self, writer: &mut OptimisticDataWriter) -> Result<()> {
        writer.write_unflushed_row_groups(&mut self.row_groups)
    }

    /// Appends every row of `other`, handing the row groups it completes to `writer`
    /// (the merge of `CollectionMerger::Flush`).
    pub fn append_collection(&mut self, other: &Self, writer: &mut OptimisticDataWriter) -> Result<()> {
        other.scan(|chunk| self.append(chunk, writer))?;
        self.finalize();
        Ok(())
    }

    /// Calls `f` with the rows in chunks, in order (`Chunks`).
    pub fn scan(&self, mut f: impl FnMut(&DataChunk) -> Result<()>) -> Result<()> {
        let collection = self.row_groups.collection();
        let types = collection.types();
        let column_ids: Vec<StorageIndex> = (0..types.len()).map(StorageIndex::from).collect();
        let mut filters = ScanFilterInfo::default();
        let mut state = CollectionScanState::default();
        collection.initialize_scan(&mut state, &column_ids, &mut filters)?;
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
    pub fn create_optimistic_collection(
        self: &Arc<Self>,
        transaction: &Transaction,
    ) -> Result<OptimisticCollection> {
        self.check_main_table("insert into")?;
        let storage = transaction.local_storage().get_or_create(self)?;
        let collection = RowGroupCollection::new(Arc::clone(self.io()), self.types(), MAX_ROW_ID);
        Ok(OptimisticCollection {
            row_groups: OptimisticWriteCollection::per_column(Arc::new(collection), storage.append_blocks()),
            append: None,
            transaction: transaction.data(),
        })
    }

    /// A writer for the row groups a thread of a parallel insert into this table
    /// completes (`OptimisticDataWriter(context, table)`).
    pub fn create_optimistic_writer(
        self: &Arc<Self>,
        transaction: &Transaction,
    ) -> Result<OptimisticDataWriter> {
        self.check_main_table("insert into")?;
        let storage = transaction.local_storage().get_or_create(self)?;
        Ok(storage.create_optimistic_writer())
    }

    /// Moves the partially filled blocks of a thread's optimistic writer into the
    /// transaction-local storage's writer (`GetOptimisticWriter(context).Merge(writer)`).
    pub fn merge_optimistic_writer(
        self: &Arc<Self>,
        transaction: &Transaction,
        writer: &mut OptimisticDataWriter,
    ) -> Result<()> {
        self.check_main_table("insert into")?;
        let storage = transaction.local_storage().get_or_create(self)?;
        storage.merge_optimistic_writer(writer)
    }

    /// Writes the partially filled blocks of the transaction-local storage's writer
    /// (`GetOptimisticWriter(context).FinalFlush()`).
    pub fn final_flush_optimistic_writer(self: &Arc<Self>, transaction: &Transaction) -> Result<()> {
        self.check_main_table("insert into")?;
        let storage = transaction.local_storage().get_or_create(self)?;
        storage.final_flush_optimistic_writer()
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
        storage.local_merge(&collection.row_groups)
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
