//! The row groups of a table or of transaction-local storage (DuckDB's
//! `RowGroupCollection`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use rustdb_common::{Error, Result};
use rustdb_types::{DataChunk, LogicalType, SelectionVector};

use super::chunk_info::ScanOptions;
use super::column_data::ColumnDataType;
use super::row_group::{DeleteRecord, RowGroup, RowGroupAppendState, TableScanType, UpdateRecord};
use super::scan_state::{CollectionScanState, ParallelCollectionScanState, ScanFilterInfo, StorageIndex};
use super::segment_tree::{SegmentNode, SegmentTree};
use crate::data_table::TableIoManager;
use crate::statistics::BaseStatistics;
use crate::transaction::TransactionData;
use crate::{MAX_ROW_ID, VECTOR_SIZE};

/// The row groups of a collection, starting at row id `base_row_id`.
#[derive(Debug)]
pub struct RowGroupSegmentTree {
    base_row_id: u64,
    tree: SegmentTree<RowGroup>,
}

impl RowGroupSegmentTree {
    pub fn new(base_row_id: u64) -> Self {
        Self {
            base_row_id,
            tree: SegmentTree::default(),
        }
    }

    pub fn base_row_id(&self) -> u64 {
        self.base_row_id
    }

    pub fn tree(&self) -> &SegmentTree<RowGroup> {
        &self.tree
    }
}

/// Whether appends start a new row group (DuckDB's `RowGroupAppendMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowGroupAppendMode {
    AppendToExisting,
    /// Start a new row group unless that hurts indexes.
    SuggestNew,
    RequireNew,
}

/// The state of an append to a collection (DuckDB's `TableAppendState`).
#[derive(Debug)]
pub struct TableAppendState {
    pub(crate) row_groups: Arc<RowGroupSegmentTree>,
    /// Position of the row group the append started in.
    start_row_group: usize,
    current: SegmentNode<RowGroup>,
    current_position: usize,
    row_group_append: RowGroupAppendState,
    /// First row id of the append.
    pub row_start: u64,
    /// Row id of the next appended row.
    pub current_row: u64,
    pub total_append_count: u64,
    pub(crate) transaction: TransactionData,
}

/// Per-column statistics of a collection (DuckDB's `TableStatistics`).
#[derive(Debug, Default, Clone)]
pub struct TableStatistics {
    pub columns: Vec<BaseStatistics>,
}

impl TableStatistics {
    pub fn empty(types: &[LogicalType]) -> Self {
        Self {
            columns: types.iter().cloned().map(BaseStatistics::create_empty).collect(),
        }
    }
}

/// Whether row ids of a collection starting at `row_start` are transaction-local.
fn data_type_for(row_start: u64) -> ColumnDataType {
    match row_start.cmp(&MAX_ROW_ID) {
        std::cmp::Ordering::Equal => ColumnDataType::InitialTransactionLocal,
        std::cmp::Ordering::Greater => ColumnDataType::TransactionLocal,
        std::cmp::Ordering::Less => ColumnDataType::MainTable,
    }
}

/// Whether a fetch checks row visibility (DuckDB's `FetchType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchType {
    Transactional,
    Force,
}

/// The row groups of a table.
#[derive(Debug)]
pub struct RowGroupCollection {
    io: Arc<TableIoManager>,
    types: Vec<LogicalType>,
    row_group_size: u64,
    total_rows: AtomicU64,
    row_groups: RwLock<Arc<RowGroupSegmentTree>>,
    stats: Mutex<TableStatistics>,
    append_mode: Mutex<RowGroupAppendMode>,
}

impl RowGroupCollection {
    /// An empty collection whose first row id is `base_row_id`.
    pub fn new(io: Arc<TableIoManager>, types: Vec<LogicalType>, base_row_id: u64) -> Self {
        let row_group_size = io.row_group_size();
        let stats = TableStatistics::empty(&types);
        Self {
            io,
            types,
            row_group_size,
            total_rows: AtomicU64::new(0),
            row_groups: RwLock::new(Arc::new(RowGroupSegmentTree::new(base_row_id))),
            stats: Mutex::new(stats),
            append_mode: Mutex::new(RowGroupAppendMode::AppendToExisting),
        }
    }

    pub fn io(&self) -> &Arc<TableIoManager> {
        &self.io
    }

    pub fn types(&self) -> &[LogicalType] {
        &self.types
    }

    pub fn row_group_size(&self) -> u64 {
        self.row_group_size
    }

    pub fn total_rows(&self) -> u64 {
        self.total_rows.load(Ordering::Acquire)
    }

    /// The current row groups.
    pub fn row_groups(&self) -> Arc<RowGroupSegmentTree> {
        Arc::clone(&self.row_groups.read().unwrap_or_else(PoisonError::into_inner))
    }

    fn set_row_groups(&self, row_groups: Arc<RowGroupSegmentTree>) {
        *self.row_groups.write().unwrap_or_else(PoisonError::into_inner) = row_groups;
    }

    pub fn row_group_count(&self) -> usize {
        self.row_groups().tree.len()
    }

    pub fn is_empty(&self) -> bool {
        self.row_groups().tree.is_empty()
    }

    fn stats(&self) -> MutexGuard<'_, TableStatistics> {
        self.stats.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A copy of the statistics of column `column` (`CopyStats`).
    pub fn column_statistics(&self, column: usize) -> Option<BaseStatistics> {
        self.stats().columns.get(column).cloned()
    }

    /// Replaces the table statistics.
    pub fn set_statistics(&self, stats: TableStatistics) {
        *self.stats() = stats;
    }

    pub fn set_append_mode(&self, mode: RowGroupAppendMode) {
        *self.append_mode.lock().unwrap_or_else(PoisonError::into_inner) = mode;
    }

    fn append_mode(&self) -> RowGroupAppendMode {
        *self.append_mode.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Appends an empty row group starting at row id `start_row` (`AppendRowGroup`).
    fn append_row_group(&self, row_groups: &RowGroupSegmentTree, start_row: u64) -> SegmentNode<RowGroup> {
        let row_group = Arc::new(RowGroup::new_empty(
            &self.io,
            &self.types,
            data_type_for(start_row),
        ));
        row_groups.tree.push(start_row, Arc::clone(&row_group));
        self.set_append_mode(RowGroupAppendMode::AppendToExisting);
        SegmentNode {
            row_start: start_row,
            node: row_group,
        }
    }

    /// Adds an existing row group at the end of the collection.
    pub fn append_existing_row_group(&self, row_group: Arc<RowGroup>) {
        let row_groups = self.row_groups();
        let start = row_groups.base_row_id + self.total_rows();
        self.total_rows.fetch_add(row_group.count(), Ordering::AcqRel);
        {
            let mut stats = self.stats();
            // Statistics of a whole row group are merged from its columns.
            let _ = row_group.merge_into_statistics(&mut stats.columns);
        }
        row_groups.tree.push(start, row_group);
    }

    // ----------------------------------------------------------------------------------
    // Scans
    // ----------------------------------------------------------------------------------

    /// Starts a scan of the whole collection (`InitializeScan`).
    pub fn initialize_scan(
        &self,
        state: &mut CollectionScanState,
        column_ids: &[StorageIndex],
        filters: &mut ScanFilterInfo,
    ) -> Result<()> {
        let row_groups = self.row_groups();
        state.max_row = row_groups.base_row_id + self.total_rows();
        state.column_scans = RowGroup::column_scan_states(&self.types, column_ids);
        state.row_group = None;
        let mut position = 0;
        while let Some(node) = row_groups.tree.get(position) {
            if node
                .node
                .initialize_scan(state, column_ids, filters, node.row_start, 0)?
            {
                state.row_group = Some((position, node));
                break;
            }
            position += 1;
        }
        state.row_groups = Some(row_groups);
        Ok(())
    }

    /// Starts a scan of rows `start_row..end_row` (`InitializeScanWithOffset`).
    pub fn initialize_scan_with_offset(
        &self,
        state: &mut CollectionScanState,
        column_ids: &[StorageIndex],
        filters: &mut ScanFilterInfo,
        start_row: u64,
        end_row: u64,
    ) -> Result<()> {
        let row_groups = self.row_groups();
        state.max_row = end_row;
        state.column_scans = RowGroup::column_scan_states(&self.types, column_ids);
        let (position, node) = row_groups
            .tree
            .find(start_row)
            .ok_or_else(|| Error::internal("Failed to initialize row group scan with offset"))?;
        let vector = ((start_row - node.row_start) / VECTOR_SIZE) as usize;
        if !node
            .node
            .initialize_scan(state, column_ids, filters, node.row_start, vector)?
        {
            return Err(Error::internal("Failed to initialize row group scan with offset"));
        }
        state.row_group = Some((position, node));
        state.row_groups = Some(row_groups);
        Ok(())
    }

    /// Scans the next chunk visible under `options`; returns `false` when the scan is
    /// exhausted (`CollectionScanState::Scan`).
    pub fn scan(
        options: ScanOptions,
        state: &mut CollectionScanState,
        column_ids: &[StorageIndex],
        filters: &mut ScanFilterInfo,
        result: &mut DataChunk,
    ) -> Result<bool> {
        while let Some((position, node)) = state.row_group.clone() {
            node.node
                .scan(options, state, column_ids, filters, node.row_start, result)?;
            if !result.is_empty() {
                return Ok(true);
            }
            if state.max_row <= node.row_end() {
                state.row_group = None;
                return Ok(false);
            }
            let Some(row_groups) = state.row_groups.clone() else {
                state.row_group = None;
                return Ok(false);
            };
            let mut next = position + 1;
            state.row_group = None;
            while let Some(candidate) = row_groups.tree.get(next) {
                if candidate.row_start >= state.max_row {
                    break;
                }
                if candidate
                    .node
                    .initialize_scan(state, column_ids, filters, candidate.row_start, 0)?
                {
                    state.row_group = Some((next, candidate));
                    break;
                }
                next += 1;
            }
        }
        Ok(false)
    }

    /// Scans with one of the transaction-less scan types.
    pub fn scan_all(
        &self,
        scan_type: TableScanType,
        transaction: TransactionData,
        state: &mut CollectionScanState,
        column_ids: &[StorageIndex],
        filters: &mut ScanFilterInfo,
        result: &mut DataChunk,
    ) -> Result<bool> {
        Self::scan(
            RowGroup::scan_options(scan_type, transaction),
            state,
            column_ids,
            filters,
            result,
        )
    }

    /// Prepares a parallel scan (`InitializeParallelScan`).
    pub fn initialize_parallel_scan(&self, state: &ParallelCollectionScanState) {
        let row_groups = self.row_groups();
        let mut inner = state.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.max_row = row_groups.base_row_id + self.total_rows();
        inner.row_groups = Some(row_groups);
        inner.next_row_group = 0;
        inner.batch_index = 0;
        inner.processed_rows = 0;
    }

    /// Hands the next row group of a parallel scan to `scan_state`; returns `false` when
    /// none is left (`NextParallelScan`).
    pub fn next_parallel_scan(
        &self,
        state: &ParallelCollectionScanState,
        scan_state: &mut CollectionScanState,
        column_ids: &[StorageIndex],
        filters: &mut ScanFilterInfo,
    ) -> Result<bool> {
        loop {
            let (position, node, max_row, row_groups) = {
                let mut inner = state.inner.lock().unwrap_or_else(PoisonError::into_inner);
                let Some(row_groups) = inner.row_groups.clone() else {
                    return Ok(false);
                };
                let position = inner.next_row_group;
                let Some(node) = row_groups.tree.get(position) else {
                    scan_state.batch_index = inner.batch_index;
                    return Ok(false);
                };
                if node.node.count() == 0 || node.row_start >= inner.max_row {
                    scan_state.batch_index = inner.batch_index;
                    return Ok(false);
                }
                inner.next_row_group += 1;
                inner.processed_rows += node.node.count();
                inner.batch_index += 1;
                scan_state.batch_index = inner.batch_index;
                let max_row = node.row_end().min(inner.max_row);
                (position, node, max_row, row_groups)
            };
            scan_state.max_row = max_row;
            if scan_state.column_scans.is_empty() {
                scan_state.column_scans = RowGroup::column_scan_states(&self.types, column_ids);
            }
            scan_state.row_groups = Some(row_groups);
            if node
                .node
                .initialize_scan(scan_state, column_ids, filters, node.row_start, 0)?
            {
                scan_state.row_group = Some((position, node));
                return Ok(true);
            }
        }
    }

    // ----------------------------------------------------------------------------------
    // Fetches
    // ----------------------------------------------------------------------------------

    /// Reads the rows `row_ids` visible to the transaction (all of them with
    /// [`FetchType::Force`]) into `result` (`Fetch`).
    pub fn fetch(
        &self,
        transaction: TransactionData,
        result: &mut DataChunk,
        column_ids: &[StorageIndex],
        row_ids: &[i64],
        fetch_type: FetchType,
    ) -> Result<()> {
        let row_groups = self.row_groups();
        let mut count = 0;
        for &row_id in row_ids {
            let Some((_, node)) = row_groups
                .tree
                .index_of(row_id as u64)
                .and_then(|index| row_groups.tree.get(index).map(|node| (index, node)))
            else {
                continue;
            };
            let row = row_id as u64 - node.row_start;
            if fetch_type == FetchType::Transactional && !node.node.fetch(transaction, row)? {
                continue;
            }
            node.node
                .fetch_row(transaction, column_ids, row, node.row_start, result, count)?;
            count += 1;
        }
        result.set_len(count);
        Ok(())
    }

    /// Whether row `row_id` is visible to the transaction (`CanFetch`).
    pub fn can_fetch(&self, transaction: TransactionData, row_id: i64) -> Result<bool> {
        let row_groups = self.row_groups();
        let Some(index) = row_groups.tree.index_of(row_id as u64) else {
            return Ok(false);
        };
        let Some(node) = row_groups.tree.get(index) else {
            return Ok(false);
        };
        node.node.fetch(transaction, row_id as u64 - node.row_start)
    }

    // ----------------------------------------------------------------------------------
    // Appends
    // ----------------------------------------------------------------------------------

    /// Starts an append (`InitializeAppend`).
    pub fn initialize_append(&self, transaction: TransactionData) -> Result<TableAppendState> {
        let row_groups = self.row_groups();
        let total = self.total_rows();
        let needs_new = match row_groups.tree.last() {
            None => true,
            Some(last) => match self.append_mode() {
                RowGroupAppendMode::RequireNew | RowGroupAppendMode::SuggestNew => true,
                RowGroupAppendMode::AppendToExisting => last.node.count() >= self.row_group_size,
            },
        };
        let (position, current) = if needs_new {
            let node = self.append_row_group(&row_groups, row_groups.base_row_id + total);
            (row_groups.tree.len() - 1, node)
        } else {
            let position = row_groups.tree.len() - 1;
            let node = row_groups
                .tree
                .last()
                .ok_or_else(|| Error::internal("collection without row groups"))?;
            (position, node)
        };
        let row_group_append = current.node.initialize_append()?;
        let row_start = row_groups.base_row_id + total;
        Ok(TableAppendState {
            row_groups,
            start_row_group: position,
            current,
            current_position: position,
            row_group_append,
            row_start,
            current_row: row_start,
            total_append_count: 0,
            transaction,
        })
    }

    /// Appends `chunk`; returns whether a new row group was started (`Append`). The rows
    /// become visible with [`RowGroupCollection::finalize_append`].
    pub fn append(&self, chunk: &DataChunk, state: &mut TableAppendState) -> Result<bool> {
        let mut remaining = chunk.len();
        let mut offset = 0;
        let mut new_row_group = false;
        state.total_append_count += remaining as u64;
        loop {
            let available = self.row_group_size - state.row_group_append.offset_in_row_group;
            let append_count = (remaining as u64).min(available) as usize;
            if append_count > 0 {
                let piece = if offset == 0 && append_count == chunk.len() {
                    chunk.clone()
                } else {
                    let sel = SelectionVector::new(
                        (offset as u32..(offset + append_count) as u32).collect::<Vec<_>>(),
                    );
                    DataChunk::new(
                        chunk.columns().iter().map(|column| column.slice(&sel)).collect(),
                        append_count,
                    )
                };
                state
                    .current
                    .node
                    .append(&mut state.row_group_append, &piece, append_count)?;
                let mut stats = self.stats();
                state.current.node.merge_into_statistics(&mut stats.columns)?;
            }
            remaining -= append_count;
            offset += append_count;
            if remaining == 0 {
                break;
            }
            new_row_group = true;
            let next_start = state.current.row_start + state.row_group_append.offset_in_row_group;
            let node = self.append_row_group(&state.row_groups, next_start);
            state.current_position += 1;
            state.row_group_append = node.node.initialize_append()?;
            state.current = node;
        }
        state.current_row += chunk.len() as u64;
        Ok(new_row_group)
    }

    /// Publishes the appended rows with insert versions of the transaction
    /// (`FinalizeAppend`).
    pub fn finalize_append(&self, transaction: TransactionData, state: &mut TableAppendState) {
        let mut remaining = state.total_append_count;
        let mut position = state.start_row_group;
        while remaining > 0 {
            let Some(node) = state.row_groups.tree.get(position) else {
                break;
            };
            let append_count = remaining.min(self.row_group_size - node.node.count());
            node.node
                .append_version_info(transaction, append_count, self.row_group_size);
            remaining -= append_count;
            position += 1;
        }
        self.total_rows
            .fetch_add(state.total_append_count, Ordering::AcqRel);
        state.total_append_count = 0;
    }

    /// Marks rows `row_start..row_start + count` as committed (`CommitAppend`).
    pub fn commit_append(&self, commit_id: u64, row_start: u64, count: u64) {
        self.for_each_row_group_range(row_start, count, |row_group, start, count| {
            row_group.commit_append(commit_id, start, count);
        });
    }

    /// Drops version info of fully visible appended vectors (`CleanupAppend`).
    pub fn cleanup_append(&self, lowest_transaction: u64, row_start: u64, count: u64) {
        self.for_each_row_group_range(row_start, count, |row_group, start, count| {
            row_group.cleanup_append(lowest_transaction, start, count);
        });
    }

    fn for_each_row_group_range(&self, row_start: u64, count: u64, mut f: impl FnMut(&RowGroup, u64, u64)) {
        let row_groups = self.row_groups();
        let Some((mut position, _)) = row_groups.tree.find(row_start) else {
            return;
        };
        let mut current = row_start;
        let mut remaining = count;
        while remaining > 0 {
            let Some(node) = row_groups.tree.get(position) else {
                break;
            };
            let start = current - node.row_start;
            let rows = (node.node.count() - start).min(remaining);
            f(&node.node, start, rows);
            current += rows;
            remaining -= rows;
            position += 1;
            if rows == 0 {
                break;
            }
        }
    }

    /// Removes every row at and after `new_end` (`RevertAppendInternal`).
    pub fn revert_append(&self, new_end: u64) -> Result<()> {
        let row_groups = self.row_groups();
        let nodes = row_groups.tree.nodes();
        let Some(last) = nodes.last() else { return Ok(()) };
        if last.row_end() <= new_end {
            return Ok(());
        }
        let reverted = RowGroupSegmentTree::new(row_groups.base_row_id);
        for node in nodes {
            if node.row_start >= new_end {
                break;
            }
            if node.row_end() > new_end {
                node.node.revert_append(new_end - node.row_start)?;
            }
            reverted.tree.push(node.row_start, node.node);
        }
        self.set_row_groups(Arc::new(reverted));
        self.total_rows
            .store(new_end - row_groups.base_row_id, Ordering::Release);
        Ok(())
    }

    /// Moves the row groups of `other` (transaction-local data) to the end of this
    /// collection (`MergeStorage`).
    pub fn merge_storage(&self, other: &Self) -> Result<()> {
        let row_groups = self.row_groups();
        let mut index = row_groups.base_row_id + self.total_rows();
        for node in other.row_groups().tree.nodes() {
            node.node.set_data_type(ColumnDataType::MainTable);
            let count = node.node.count();
            row_groups.tree.push(index, node.node);
            index += count;
        }
        {
            let other_stats = other.stats().clone();
            let mut stats = self.stats();
            for (own, theirs) in stats.columns.iter_mut().zip(&other_stats.columns) {
                own.merge(theirs);
            }
        }
        self.total_rows.fetch_add(other.total_rows(), Ordering::AcqRel);
        Ok(())
    }

    // ----------------------------------------------------------------------------------
    // Deletes and updates
    // ----------------------------------------------------------------------------------

    /// Deletes rows `ids` (`Delete`); returns the delete count and undo records.
    pub fn delete(&self, transaction: TransactionData, ids: &[i64]) -> Result<(u64, Vec<DeleteRecord>)> {
        let row_groups = self.row_groups();
        let mut deleted = 0;
        let mut records = Vec::new();
        let mut position = 0;
        while position < ids.len() {
            let start = position;
            let (_, node) = row_groups
                .tree
                .find(ids[start] as u64)
                .ok_or_else(|| Error::internal("deleting a row id that does not exist"))?;
            let (row_start, row_end) = (node.row_start, node.row_end());
            position += 1;
            while position < ids.len() && (row_start..row_end).contains(&(ids[position] as u64)) {
                position += 1;
            }
            let (count, mut group_records) =
                node.node.delete(transaction, &ids[start..position], row_start)?;
            deleted += count;
            records.append(&mut group_records);
        }
        Ok((deleted, records))
    }

    /// Updates rows `ids` of columns `column_ids` to `updates` (`Update`); returns undo
    /// records.
    pub fn update(
        &self,
        transaction: TransactionData,
        ids: &[i64],
        column_ids: &[usize],
        updates: &DataChunk,
    ) -> Result<Vec<UpdateRecord>> {
        let row_groups = self.row_groups();
        let mut records = Vec::new();
        let mut position = 0;
        while position < ids.len() {
            let start = position;
            let (_, node) = row_groups
                .tree
                .find(ids[start] as u64)
                .ok_or_else(|| Error::internal("updating a row id that does not exist"))?;
            let vector_start =
                node.row_start + (ids[start] as u64 - node.row_start) / VECTOR_SIZE * VECTOR_SIZE;
            let vector_end = (vector_start + VECTOR_SIZE).min(node.row_end());
            position += 1;
            while position < ids.len() && (vector_start..vector_end).contains(&(ids[position] as u64)) {
                position += 1;
            }
            let slice = if start == 0 && position == ids.len() {
                updates.clone()
            } else {
                let sel = SelectionVector::new((start as u32..position as u32).collect::<Vec<_>>());
                DataChunk::new(
                    updates
                        .columns()
                        .iter()
                        .map(|column| column.slice(&sel))
                        .collect(),
                    position - start,
                )
            };
            records.extend(node.node.update(
                transaction,
                &slice,
                &ids[start..position],
                column_ids,
                node.row_start,
            )?);
            let mut stats = self.stats();
            for &column in column_ids {
                if let (Some(target), Ok(column_stats)) =
                    (stats.columns.get_mut(column), node.node.column_statistics(column))
                {
                    target.merge(&column_stats);
                }
            }
        }
        Ok(records)
    }

    /// Rows whose delete is not committed, summed over row groups.
    pub fn committed_row_count(&self) -> u64 {
        self.row_groups()
            .tree
            .nodes()
            .iter()
            .map(|node| node.node.committed_row_count())
            .sum()
    }
}
