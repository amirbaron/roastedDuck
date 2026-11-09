//! Writing the row groups of large appends to the database file while the inserting
//! transaction runs (DuckDB's `OptimisticDataWriter` and `OptimisticWriteCollection`).
//!
//! Every time an append to a transaction-local collection completes a row group, the
//! row group is marked unflushed; once enough of them are buffered (by count or memory),
//! they are compressed into blocks of the database file and replaced by row groups that
//! read those blocks, which frees their memory. When the transaction commits, the written
//! row groups move into the table as they are and the WAL only logs where they are
//! (`ROW_GROUP_DATA`); when it rolls back, their blocks are freed again.
//!
//! Every writer has its partially filled blocks (DuckDB's `PartialBlockManager`), and so
//! has every column of the collections of parallel inserts; the collections' blocks are
//! merged into their writer's once the collection is written, and the threads' writers
//! into the transaction's, which writes the partially filled blocks last
//! (`FinalFlush`). The blocks get their ids when they are written; the transaction's
//! [`AppendBlocks`] records them.

use std::collections::BTreeSet;
use std::sync::Arc;

use roastedduck_common::{Error, Result};

use crate::checkpoint::table_writer::{AppendBlocks, BlockWriter, write_row_groups_to_disk};
use crate::data_table::DataTable;
use crate::table::{RowGroupAppendMode, RowGroupCollection};

/// The settings of optimistic writes (`enable_optimistic_write`,
/// `write_buffer_row_group_count` and `write_buffer_row_group_memory_limit`, with the
/// `threads` and `geometry_minimum_shredding_size` the writes depend on).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OptimisticWriteConfig {
    pub enabled: bool,
    /// Complete row groups buffered before they are written together.
    pub row_group_count: u64,
    /// The memory of complete row groups buffered before they are written together;
    /// `None` is 20% of the memory limit divided by the threads plus one.
    pub memory_limit: Option<u64>,
    pub threads: u64,
    pub geometry_minimum_shredding_size: i64,
}

impl Default for OptimisticWriteConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            row_group_count: 5,
            memory_limit: None,
            threads: std::thread::available_parallelism().map_or(1, |threads| threads.get() as u64),
            geometry_minimum_shredding_size: 30_000,
        }
    }
}

/// A transaction-local row group collection being appended to, with the row groups that
/// are complete but not written yet (DuckDB's `OptimisticWriteCollection`).
#[derive(Debug)]
pub struct OptimisticWriteCollection {
    collection: Arc<RowGroupCollection>,
    unflushed_row_groups: BTreeSet<usize>,
    unflushed_data_size: u64,
    prev_allocated_size: u64,
    complete_row_groups: usize,
    /// The partially filled blocks of each column (`partial_block_managers`), for the
    /// collections of parallel inserts (`OptimisticWritePartialManagers::PER_COLUMN`);
    /// empty when the writer's blocks are used (`GLOBAL`).
    partial_block_managers: Vec<BlockWriter>,
}

impl OptimisticWriteCollection {
    /// A collection whose row groups are written to the writer's partially filled blocks
    /// (`OptimisticWritePartialManagers::GLOBAL`).
    pub(crate) fn new(collection: Arc<RowGroupCollection>) -> Self {
        Self {
            collection,
            unflushed_row_groups: BTreeSet::new(),
            unflushed_data_size: 0,
            prev_allocated_size: 0,
            complete_row_groups: 0,
            partial_block_managers: Vec::new(),
        }
    }

    /// A collection whose columns have partially filled blocks of their own in `blocks`
    /// (`OptimisticWritePartialManagers::PER_COLUMN`).
    pub(crate) fn per_column(
        collection: Arc<RowGroupCollection>,
        blocks: Option<&Arc<AppendBlocks>>,
    ) -> Self {
        let partial_block_managers = match blocks {
            Some(blocks) => (0..collection.types().len())
                .map(|_| BlockWriter::append_to_table(Arc::clone(blocks)))
                .collect(),
            None => Vec::new(),
        };
        Self {
            partial_block_managers,
            ..Self::new(collection)
        }
    }

    /// The row groups.
    pub fn collection(&self) -> &Arc<RowGroupCollection> {
        &self.collection
    }

    /// Replaces the row groups (after an ALTER TABLE rewrote them).
    pub(crate) fn set_collection(&mut self, collection: Arc<RowGroupCollection>) {
        self.collection = collection;
    }

    /// The row groups no append changes any more.
    pub fn complete_row_groups(&self) -> usize {
        self.complete_row_groups
    }

    /// Moves the row groups of `other` to the end of this collection, taking over which of
    /// them are complete and unflushed (`MergeStorage`): the incomplete row groups at the
    /// end of this collection are complete from now on.
    pub(crate) fn merge_storage(&mut self, other: &Self) -> Result<()> {
        if other.collection.total_rows() == 0 {
            return Ok(());
        }
        let current_row_group_count = self.collection.row_group_count();
        if self.complete_row_groups > current_row_group_count {
            return Err(Error::internal(
                "MergeStorage - complete row groups > total_row_groups",
            ));
        }
        for index in self.complete_row_groups..current_row_group_count {
            self.unflushed_row_groups.insert(index);
            self.complete_row_groups += 1;
        }
        for index in &other.unflushed_row_groups {
            self.unflushed_row_groups.insert(current_row_group_count + index);
        }
        self.complete_row_groups += other.complete_row_groups;
        self.collection.merge_storage(&other.collection)?;
        // Appends must not go to a row group that was written.
        if self.complete_row_groups == self.collection.row_group_count() {
            self.collection.set_append_mode(RowGroupAppendMode::RequireNew);
        }
        Ok(())
    }
}

/// Writes complete row groups of transaction-local collections of a table to the database
/// file (DuckDB's `OptimisticDataWriter`). The transaction-local storage has one for
/// serial appends, and each thread of a parallel insert one of its own.
#[derive(Debug)]
pub struct OptimisticDataWriter {
    table: Arc<DataTable>,
    /// The blocks of the transaction's optimistic writes to the table; `None` when the
    /// table is not stored in a database file (temporary tables, in-memory databases).
    blocks: Option<Arc<AppendBlocks>>,
    /// The writer of the row groups' segments, set up by the first write.
    partial_manager: Option<BlockWriter>,
}

impl OptimisticDataWriter {
    pub(crate) fn new(table: Arc<DataTable>, blocks: Option<Arc<AppendBlocks>>) -> Self {
        Self {
            table,
            blocks,
            partial_manager: None,
        }
    }

    /// Makes the writer write the data of `table` (after an ALTER TABLE changed it).
    pub(crate) fn set_table(&mut self, table: Arc<DataTable>) {
        self.table = table;
    }

    /// Whether row groups are written: optimistic writes are enabled and the table is
    /// stored in a database file that can be written (`PrepareWrite`). Sets up the block
    /// writer.
    fn prepare_write(&mut self) -> bool {
        if !self.table.io().optimistic_write_config().enabled {
            return false;
        }
        let Some(blocks) = &self.blocks else {
            return false;
        };
        if self
            .table
            .io()
            .file_block_manager()
            .is_none_or(|manager| manager.is_read_only())
        {
            return false;
        }
        if self.partial_manager.is_none() {
            self.partial_manager = Some(BlockWriter::append_to_table(Arc::clone(blocks)));
        }
        true
    }

    /// Records that an append to `row_groups` completed a row group, and writes the
    /// complete row groups once enough are buffered (`WriteNewRowGroup`).
    pub fn write_new_row_group(&mut self, row_groups: &mut OptimisticWriteCollection) -> Result<()> {
        if !self.prepare_write() {
            return Ok(());
        }
        row_groups
            .unflushed_row_groups
            .insert(row_groups.complete_row_groups);
        row_groups.complete_row_groups += 1;
        let allocated_size = row_groups.collection.allocation_size();
        if row_groups.prev_allocated_size > allocated_size {
            return Err(Error::internal(
                "Row group prev allocated size is larger than currently allocated size",
            ));
        }
        row_groups.unflushed_data_size += allocated_size - row_groups.prev_allocated_size;
        row_groups.prev_allocated_size = allocated_size;
        let config = self.table.io().optimistic_write_config();
        let need_to_flush = row_groups.unflushed_row_groups.len() as u64 >= config.row_group_count || {
            let memory_limit = config
                .memory_limit
                .unwrap_or_else(|| self.table.io().buffer_manager().max_memory() / 5 / (config.threads + 1));
            row_groups.unflushed_data_size >= memory_limit
        };
        if need_to_flush {
            let unflushed: Vec<usize> = row_groups.unflushed_row_groups.iter().copied().collect();
            self.flush_to_disk(row_groups, &unflushed)?;
            row_groups.unflushed_row_groups.clear();
            row_groups.unflushed_data_size = 0;
        }
        Ok(())
    }

    /// Writes every row group of `row_groups` not written yet, including the last,
    /// incomplete one (`WriteUnflushedRowGroups`); appends start a new row group
    /// afterwards.
    pub fn write_unflushed_row_groups(&mut self, row_groups: &mut OptimisticWriteCollection) -> Result<()> {
        if !self.prepare_write() {
            return Ok(());
        }
        let total_row_groups = row_groups.collection.row_group_count();
        if row_groups.complete_row_groups > total_row_groups {
            return Err(Error::internal(
                "WriteUnflushedRowGroups - complete row groups > total_row_groups",
            ));
        }
        for index in row_groups.complete_row_groups..total_row_groups {
            row_groups.unflushed_row_groups.insert(index);
            row_groups.complete_row_groups += 1;
        }
        if !row_groups.unflushed_row_groups.is_empty() {
            let unflushed: Vec<usize> = row_groups.unflushed_row_groups.iter().copied().collect();
            self.flush_to_disk(row_groups, &unflushed)?;
        }
        for manager in std::mem::take(&mut row_groups.partial_block_managers) {
            self.merge_manager(manager)?;
        }
        row_groups.unflushed_row_groups.clear();
        row_groups.unflushed_data_size = 0;
        // Further appends must not change a row group that was written.
        row_groups
            .collection
            .set_append_mode(RowGroupAppendMode::RequireNew);
        Ok(())
    }

    /// Writes the row groups at `indexes` and installs the row groups reading the
    /// written blocks in their place (`FlushToDisk`). Row groups without rows have
    /// nothing to write.
    fn flush_to_disk(&mut self, row_groups: &mut OptimisticWriteCollection, indexes: &[usize]) -> Result<()> {
        let Some(blocks) = self.partial_manager.as_mut() else {
            return Ok(());
        };
        let tree = row_groups.collection.row_groups();
        let mut positions = Vec::with_capacity(indexes.len());
        let mut to_flush = Vec::with_capacity(indexes.len());
        for &index in indexes {
            let node = tree
                .tree()
                .get(index)
                .ok_or_else(|| Error::internal(format!("optimistic write of missing row group {index}")))?;
            let has_rows = (0..node.node.column_count())
                .any(|column| node.node.column(column).is_ok_and(|column| column.count() > 0));
            if has_rows {
                positions.push(index);
                to_flush.push(node.node);
            }
        }
        let config = self.table.io().optimistic_write_config();
        let written = write_row_groups_to_disk(
            &self.table,
            &row_groups.collection,
            &to_flush,
            blocks,
            &mut row_groups.partial_block_managers,
            config.geometry_minimum_shredding_size,
        )?;
        for (index, row_group) in positions.into_iter().zip(written) {
            tree.tree().set(index, row_group);
        }
        Ok(())
    }

    /// Moves partially filled blocks into this writer's (`Merge`).
    fn merge_manager(&mut self, other: BlockWriter) -> Result<()> {
        if let Some(manager) = &mut self.partial_manager {
            return manager.merge(other);
        }
        self.partial_manager = Some(other);
        Ok(())
    }

    /// Moves the partially filled blocks of `other` into this writer (`Merge`).
    pub fn merge(&mut self, other: &mut OptimisticDataWriter) -> Result<()> {
        match other.partial_manager.take() {
            Some(manager) => self.merge_manager(manager),
            None => Ok(()),
        }
    }

    /// Discards the partially filled blocks (`Rollback`).
    pub(crate) fn rollback(&mut self) {
        self.partial_manager = None;
    }

    /// Writes the partially filled blocks (`FinalFlush`).
    pub fn final_flush(&mut self) -> Result<()> {
        match self.partial_manager.take() {
            Some(mut manager) => manager.flush(),
            None => Ok(()),
        }
    }
}
