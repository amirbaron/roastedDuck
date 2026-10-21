//! A horizontal partition of a table of up to 122,880 rows (DuckDB's `RowGroup`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_types::{DataChunk, LogicalType, SelectionVector, Vector};

use super::PerColumnMetadataBlocks;
use super::chunk_info::{DeletedRows, DeletedScanType, InsertedScanType, ScanOptions, UpdateScanType};
use super::column_data::{
    ColumnAppendState, ColumnData, ColumnDataType, ColumnScanState, ScanCasts, UpdateRegistration,
};
use super::row_version_manager::RowVersionManager;
use super::scan_state::{CollectionScanState, ScanFilterInfo, StorageIndex};
use super::segment_tree::Segment;
use crate::VECTOR_SIZE;
use crate::checkpoint::pointers::{PersistentColumnData, PersistentRowGroupData};
use crate::checkpoint::table_reader::read_column;
use crate::checkpoint::table_writer::{AppendBlocks, InMemoryBlocks};
use crate::data_table::TableIoManager;
use crate::metadata::{MetaBlockPointer, MetadataManager};
use crate::statistics::{BaseStatistics, FilterPropagateResult};
use crate::table_filter::TableFilter;
use crate::transaction::TransactionData;

/// A delete performed on one vector, to be registered in the undo log
/// (DuckDB's `DeleteInfo`).
#[derive(Debug)]
pub struct DeleteRecord {
    pub version_info: Arc<RowVersionManager>,
    pub vector_index: usize,
    /// Row id of the vector's first row.
    pub base_row: u64,
    pub rows: DeletedRows,
}

impl DeleteRecord {
    /// The row ids of the deleted rows.
    pub fn row_ids(&self) -> Vec<i64> {
        match &self.rows {
            DeletedRows::Consecutive(count) => (0..*count as u64)
                .map(|row| (self.base_row + row) as i64)
                .collect(),
            DeletedRows::Rows(rows) => rows
                .iter()
                .map(|&row| (self.base_row + u64::from(row)) as i64)
                .collect(),
        }
    }
}

/// An update of one column vector, to be registered in the undo log.
#[derive(Debug)]
pub struct UpdateRecord {
    pub column_index: usize,
    pub registration: UpdateRegistration,
}

/// The state of an append to a row group (DuckDB's `RowGroupAppendState`).
#[derive(Debug)]
pub struct RowGroupAppendState {
    pub(crate) states: Vec<ColumnAppendState>,
    /// Rows of the row group before the append started plus rows appended so far.
    pub(crate) offset_in_row_group: u64,
}

/// Which rows a scan outside of any transaction reads (DuckDB's `TableScanType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableScanType {
    /// Every row, including deleted ones; committed updates merged.
    AllRows,
    /// Every row except those no transaction will ever see again.
    OmitPermanentlyDeleted,
    /// The latest committed rows.
    CommittedRows,
}

/// Where the metadata of a row group's columns is stored in the database file, kept so
/// that checkpoints can reuse it (DuckDB's `RowGroup::column_pointers`,
/// `has_metadata_blocks`, `extra_metadata_blocks`, `has_per_column_metadata_blocks` and
/// `per_column_metadata_blocks`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowGroupMetadata {
    /// The start of each column's `PersistentColumnData`; empty when the row group has
    /// no metadata on disk.
    /// Shared with the loader of the columns not built yet.
    pub column_pointers: Arc<[MetaBlockPointer]>,
    /// Whether `extra_metadata_blocks` is known.
    pub has_metadata_blocks: bool,
    /// The metadata blocks of the columns besides those `column_pointers` point into.
    pub extra_metadata_blocks: Vec<u64>,
    pub has_per_column_metadata_blocks: bool,
    /// Per column, the metadata blocks besides its start block.
    pub per_column_metadata_blocks: PerColumnMetadataBlocks,
}

impl RowGroupMetadata {
    /// What a copy of the row group for an ALTER TABLE keeps (`CreateNewRowGroupCopy`):
    /// the column pointers resized to `column_count` and whether per-column metadata
    /// blocks are known, with `columns` giving the source column of each target column.
    fn copied(&self, column_count: usize, columns: impl Fn(usize) -> Option<usize>) -> Self {
        let column_pointers = if self.column_pointers.is_empty() {
            Arc::default()
        } else {
            (0..column_count)
                .map(|target| {
                    columns(target)
                        .and_then(|source| self.column_pointers.get(source).copied())
                        .unwrap_or_else(MetaBlockPointer::invalid)
                })
                .collect()
        };
        Self {
            column_pointers,
            has_per_column_metadata_blocks: self.has_per_column_metadata_blocks,
            ..Self::default()
        }
    }

    /// The metadata of a copy with a new last column (`RowGroup::AddColumn`).
    #[must_use]
    pub fn with_added_column(&self) -> Self {
        let count = self.column_pointers.len();
        let mut result = self.copied(count + 1, |target| (target < count).then_some(target));
        if self.has_per_column_metadata_blocks {
            result.per_column_metadata_blocks = self.per_column_metadata_blocks.clone();
        }
        result
    }

    /// The metadata of a copy without column `removed` (`RowGroup::RemoveColumn`).
    #[must_use]
    pub fn without_column(&self, removed: usize) -> Self {
        let count = self.column_pointers.len().saturating_sub(1);
        let mut result = self.copied(count, |target| {
            Some(if target < removed { target } else { target + 1 })
        });
        if self.has_per_column_metadata_blocks {
            result.per_column_metadata_blocks = self.per_column_metadata_blocks.clone();
            result.per_column_metadata_blocks.remove_column(removed);
        }
        result
    }

    /// The metadata of a copy where column `changed` is rewritten (`RowGroup::AlterType`).
    #[must_use]
    pub fn with_changed_column(&self, changed: usize) -> Self {
        let count = self.column_pointers.len();
        let mut result = self.copied(count, |target| (target != changed).then_some(target));
        if self.has_per_column_metadata_blocks {
            result.per_column_metadata_blocks = self.per_column_metadata_blocks.clone();
            result.per_column_metadata_blocks.clear_column(changed);
        }
        result
    }

    /// The extra metadata blocks as pointers (`GetExtraMetadataBlockPointers`).
    pub fn extra_metadata_block_pointers(&self) -> Vec<MetaBlockPointer> {
        if self.has_per_column_metadata_blocks {
            self.per_column_metadata_blocks
                .blocks()
                .into_iter()
                .map(|block| MetaBlockPointer::new(block, 0))
                .collect()
        } else {
            self.extra_metadata_blocks
                .iter()
                .map(|&block| MetaBlockPointer::new(block, 0))
                .collect()
        }
    }
}

/// A row group: one column data per column plus the rows' versions.
#[derive(Debug)]
pub struct RowGroup {
    count: AtomicU64,
    /// The columns built so far; a row group read from the database file builds each on
    /// first use (DuckDB's lazily loaded columns, `is_loaded`).
    columns: Vec<OnceLock<Arc<ColumnData>>>,
    /// What the columns not built yet are made from.
    unloaded: Option<Box<UnloadedColumns>>,
    version_info: Mutex<Option<Arc<RowVersionManager>>>,
    /// Where the columns' metadata is on disk.
    metadata: Mutex<RowGroupMetadata>,
    /// Set when the row group was copied for an ALTER TABLE or moved to another
    /// collection: it has to be written again (`has_changes`).
    has_changes: AtomicBool,
}

/// The stored segments of a row group's columns, until each column is first used.
#[derive(Debug)]
struct UnloadedColumns {
    io: Arc<TableIoManager>,
    /// The column types, shared by the row groups of a table.
    types: Arc<[LogicalType]>,
    /// Reads the columns stored as metadata pointers.
    metadata: Option<Arc<MetadataManager>>,
    /// Where each column's `PersistentColumnData` is in the database file's metadata
    /// (shared with the row group's `RowGroupMetadata`): a column without `data` that is
    /// not built yet is read from there when first used, like DuckDB's lazily loaded
    /// `column_pointers` (keeping every column's segments and statistics in memory costs
    /// ~2 KB per column and row group).
    pointers: Arc<[MetaBlockPointer]>,
    /// Per column, its segments as just written by a checkpoint; taken when the column is
    /// built.
    data: Vec<Mutex<Option<Box<PersistentColumnData>>>>,
}

impl Segment for RowGroup {
    fn row_count(&self) -> u64 {
        self.count()
    }
}

impl RowGroup {
    /// An empty row group with a column for each of `types` (`InitializeEmpty`).
    pub fn new_empty(io: &Arc<TableIoManager>, types: &[LogicalType], data_type: ColumnDataType) -> Self {
        let columns = types
            .iter()
            .enumerate()
            .map(|(index, ty)| Arc::new(ColumnData::new(Arc::clone(io), index, ty.clone(), data_type)))
            .collect();
        Self::from_columns(columns, 0)
    }

    /// A row group of `count` rows made of existing columns.
    pub fn from_columns(columns: Vec<Arc<ColumnData>>, count: u64) -> Self {
        Self {
            count: AtomicU64::new(count),
            columns: columns.into_iter().map(OnceLock::from).collect(),
            unloaded: None,
            version_info: Mutex::default(),
            metadata: Mutex::default(),
            has_changes: AtomicBool::new(false),
        }
    }

    /// A row group sharing this one's versions whose columns are now stored as `data`
    /// with their metadata at `metadata` (after a checkpoint wrote them), built when first
    /// used (the `result_row_group` of `RowGroup::WriteToDisk`).
    #[must_use]
    pub fn with_persistent(
        &self,
        io: Arc<TableIoManager>,
        types: Arc<[LogicalType]>,
        data: Vec<PersistentColumnData>,
        metadata: RowGroupMetadata,
    ) -> Self {
        let row_group = Self::from_persistent(io, types, data, self.count());
        row_group
            .version_info_lock()
            .clone_from(&self.version_info_lock());
        row_group.set_metadata(metadata);
        row_group
    }

    /// A row group sharing this one's versions whose columns an optimistic write wrote to
    /// `blocks` as `data` (the `result_row_group` of `RowGroup::WriteToDisk` with the
    /// `PartialBlockManager` of an `OptimisticDataWriter`). Its segments are created now,
    /// so that those in blocks written later can be moved to them.
    pub(crate) fn with_optimistic_write(
        &self,
        io: &Arc<TableIoManager>,
        types: &[LogicalType],
        data: &[PersistentColumnData],
        blocks: &AppendBlocks,
    ) -> Result<Self> {
        let columns = data
            .iter()
            .zip(types)
            .enumerate()
            .map(|(index, (data, ty))| {
                ColumnData::from_optimistic_write(Arc::clone(io), index, ty.clone(), data, blocks)
                    .map(Arc::new)
            })
            .collect::<Result<Vec<_>>>()?;
        let row_group = Self::from_columns(columns, self.count());
        row_group
            .version_info_lock()
            .clone_from(&self.version_info_lock());
        Ok(row_group)
    }

    /// A row group sharing this one's versions whose columns an in-memory checkpoint
    /// wrote to `blocks` as `data` (the `result_row_group` of `RowGroup::WriteToDisk` with
    /// an `InMemoryRowGroupWriter`).
    pub(crate) fn with_in_memory_checkpoint(
        &self,
        io: &Arc<TableIoManager>,
        types: &[LogicalType],
        data: &[PersistentColumnData],
        blocks: &InMemoryBlocks,
    ) -> Result<Self> {
        let columns = data
            .iter()
            .zip(types)
            .enumerate()
            .map(|(index, (data, ty))| {
                ColumnData::from_in_memory_checkpoint(Arc::clone(io), index, ty.clone(), data, blocks)
                    .map(Arc::new)
            })
            .collect::<Result<Vec<_>>>()?;
        let row_group = Self::from_columns(columns, self.count());
        row_group
            .version_info_lock()
            .clone_from(&self.version_info_lock());
        Ok(row_group)
    }

    /// A row group of `count` rows stored in the database file, whose columns are built
    /// from `data` when first used.
    pub fn from_persistent(
        io: Arc<TableIoManager>,
        types: Arc<[LogicalType]>,
        data: Vec<PersistentColumnData>,
        count: u64,
    ) -> Self {
        Self {
            count: AtomicU64::new(count),
            columns: (0..data.len()).map(|_| OnceLock::new()).collect(),
            unloaded: Some(Box::new(UnloadedColumns {
                io,
                types,
                metadata: None,
                pointers: Arc::default(),
                data: data
                    .into_iter()
                    .map(|column| Mutex::new(Some(Box::new(column))))
                    .collect(),
            })),
            version_info: Mutex::default(),
            metadata: Mutex::default(),
            has_changes: AtomicBool::new(false),
        }
    }

    /// A row group of `count` rows stored in the database file whose columns' data is
    /// read from the metadata at `stored.column_pointers` when first used (DuckDB's
    /// `RowGroup` built from a `RowGroupPointer`); `stored` is kept for checkpoints to
    /// reuse.
    pub fn from_metadata(
        io: Arc<TableIoManager>,
        types: Arc<[LogicalType]>,
        metadata: Arc<MetadataManager>,
        stored: RowGroupMetadata,
        count: u64,
    ) -> Self {
        let pointers = Arc::clone(&stored.column_pointers);
        Self {
            count: AtomicU64::new(count),
            columns: (0..pointers.len()).map(|_| OnceLock::new()).collect(),
            unloaded: Some(Box::new(UnloadedColumns {
                io,
                types,
                metadata: Some(metadata),
                data: (0..pointers.len()).map(|_| Mutex::new(None)).collect(),
                pointers,
            })),
            version_info: Mutex::default(),
            metadata: Mutex::new(stored),
            has_changes: AtomicBool::new(false),
        }
    }

    /// A row group sharing this one's columns and versions, with `columns` replaced
    /// (`CreateNewRowGroupCopy`, used by ALTER TABLE): it has changes.
    #[must_use]
    pub fn with_columns(&self, columns: Vec<Arc<ColumnData>>) -> Self {
        Self {
            count: AtomicU64::new(self.count()),
            columns: columns.into_iter().map(OnceLock::from).collect(),
            unloaded: None,
            version_info: Mutex::new(self.version_info_lock().clone()),
            metadata: Mutex::default(),
            has_changes: AtomicBool::new(true),
        }
    }

    /// Where the columns' metadata is on disk.
    pub fn metadata(&self) -> RowGroupMetadata {
        self.metadata
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Records where the columns' metadata is on disk.
    pub fn set_metadata(&self, metadata: RowGroupMetadata) {
        *self.metadata.lock().unwrap_or_else(PoisonError::into_inner) = metadata;
    }

    /// Publishes the `count` rows a vacuum appended to this new row group: they are
    /// visible to every transaction (`RowGroup(collection, count)`).
    pub(crate) fn set_vacuumed_count(&self, count: u64) {
        self.set_count(count);
    }

    /// Marks the row group as changed (`MoveToCollection`).
    pub fn set_has_changes(&self) {
        self.has_changes.store(true, Ordering::Release);
    }

    /// Whether column `index` was built (`ColumnIsLoaded`).
    pub fn column_is_loaded(&self, index: usize) -> bool {
        self.columns.get(index).is_some_and(|slot| slot.get().is_some())
    }

    /// Whether anything must be written again: the row group was copied or moved,
    /// deletes were committed since the last checkpoint, or a built column has changes
    /// (`HasChanges`; columns not built yet never have changes).
    pub fn has_changes(&self) -> bool {
        if self.has_changes.load(Ordering::Acquire) {
            return true;
        }
        if self
            .version_info()
            .is_some_and(|info| info.has_unserialized_changes())
        {
            return true;
        }
        self.loaded_columns().any(|column| column.has_any_changes())
    }

    /// The stored segments of column `index` when it was not built yet.
    /// Columns stored as metadata pointers are read (without being built).
    pub fn unloaded_column_data(&self, index: usize) -> Result<Option<PersistentColumnData>> {
        if self.column_is_loaded(index) {
            return Ok(None);
        }
        let Some(unloaded) = self.unloaded.as_ref() else {
            return Ok(None);
        };
        let Some(slot) = unloaded.data.get(index) else {
            return Ok(None);
        };
        let data = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(stored) = data.as_ref() {
            return Ok(Some((**stored).clone()));
        }
        // Built meanwhile, or stored as a metadata pointer.
        if self.column_is_loaded(index) {
            return Ok(None);
        }
        let (Some(metadata), Some(pointer)) = (unloaded.metadata.as_ref(), unloaded.pointers.get(index))
        else {
            return Ok(None);
        };
        read_column(metadata, *pointer, &unloaded.types[index]).map(Some)
    }

    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }

    fn set_count(&self, count: u64) {
        self.count.store(count, Ordering::Release);
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// Column `index`, built from its stored segments on first use (`GetColumn`).
    pub fn column(&self, index: usize) -> Result<&Arc<ColumnData>> {
        let slot = self
            .columns
            .get(index)
            .ok_or_else(|| Error::internal(format!("row group has no column {index}")))?;
        if let Some(column) = slot.get() {
            return Ok(column);
        }
        let unloaded = self
            .unloaded
            .as_ref()
            .ok_or_else(|| Error::internal("row group column neither built nor stored"))?;
        // Built under the column's lock, so it is built once.
        let mut data = unloaded.data[index]
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(column) = slot.get() {
            return Ok(column);
        }
        let ty = &unloaded.types[index];
        let read;
        let stored = match (
            data.as_ref(),
            unloaded.metadata.as_ref(),
            unloaded.pointers.get(index),
        ) {
            (Some(stored), _, _) => stored,
            (None, Some(metadata), Some(pointer)) => {
                read = read_column(metadata, *pointer, ty)?;
                &read
            }
            (None, _, _) => return Err(Error::internal("row group column stored data already taken")),
        };
        let column = ColumnData::from_persistent(Arc::clone(&unloaded.io), index, ty.clone(), stored)?;
        let _ = slot.set(Arc::new(column));
        *data = None;
        drop(data);
        slot.get()
            .ok_or_else(|| Error::internal("row group column was not built"))
    }

    /// Every column, building those not built yet.
    pub fn columns(&self) -> Result<Vec<Arc<ColumnData>>> {
        (0..self.columns.len())
            .map(|index| self.column(index).map(Arc::clone))
            .collect()
    }

    /// The columns built so far (a column not built yet has no updates, appends or
    /// transient data).
    fn loaded_columns(&self) -> impl Iterator<Item = &Arc<ColumnData>> {
        self.columns.iter().filter_map(OnceLock::get)
    }

    /// Whether any column has updates (`HasChanges` on the loaded columns: unloaded
    /// columns can never have changes).
    pub fn has_any_updates(&self) -> bool {
        self.loaded_columns().any(|column| column.has_any_updates())
    }

    /// Bytes of transient segments allocated by appends.
    pub fn allocation_size(&self) -> u64 {
        self.loaded_columns().map(|column| column.allocation_size()).sum()
    }

    fn version_info_lock(&self) -> MutexGuard<'_, Option<Arc<RowVersionManager>>> {
        self.version_info.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The version info, if any row has version information.
    pub fn version_info(&self) -> Option<Arc<RowVersionManager>> {
        self.version_info_lock().clone()
    }

    pub fn set_version_info(&self, info: Arc<RowVersionManager>) {
        *self.version_info_lock() = Some(info);
    }

    /// The version info, created if needed, for a change; its per-row ids are charged to
    /// the buffer manager.
    fn get_or_create_version_info(&self) -> Arc<RowVersionManager> {
        let info = Arc::clone(self.version_info_lock().get_or_insert_with(Arc::default));
        let io = self.unloaded.as_ref().map(|unloaded| &unloaded.io).or_else(|| {
            self.columns
                .iter()
                .find_map(|column| column.get().map(|column| column.io_manager()))
        });
        if let Some(io) = io {
            info.track_memory(io.buffer_manager());
        }
        info
    }

    // ----------------------------------------------------------------------------------
    // Scans
    // ----------------------------------------------------------------------------------

    /// Checks the filters against the row group's statistics; returns `false` if the row
    /// group can be skipped. Filters proven true are disabled for this row group
    /// (`CheckZonemap`).
    fn check_zonemap(&self, filters: &mut ScanFilterInfo, row_start: u64) -> bool {
        filters.check_all_filters();
        let mut always_true = Vec::new();
        for (index, entry) in filters.filters().iter().enumerate() {
            let result = if entry.table_column_index.is_row_id() {
                check_row_id_filter(&entry.filter, row_start, row_start + self.count())
            } else if entry.table_column_index.pushdown_extract {
                // The statistics are those of the whole column, not the extracted field.
                FilterPropagateResult::NoPruningPossible
            } else {
                match self.column(entry.table_column_index.primary_index()) {
                    Ok(column) => column.check_zonemap_column(&entry.filter),
                    Err(_) => FilterPropagateResult::NoPruningPossible,
                }
            };
            if result == FilterPropagateResult::FilterAlwaysFalse {
                return false;
            }
            if entry.filter.is_optional() || result == FilterPropagateResult::FilterAlwaysTrue {
                always_true.push(index);
            }
        }
        for index in always_true {
            filters.set_filter_always_true(index);
        }
        true
    }

    /// Prepares the column scans of `state` to scan this row group (at `row_start`) from
    /// vector `vector_offset`; returns `false` if nothing must be scanned
    /// (`InitializeScanWithOffset`). The caller records the row group in `state`.
    pub(crate) fn initialize_scan(
        &self,
        state: &mut CollectionScanState,
        column_ids: &[StorageIndex],
        filters: &mut ScanFilterInfo,
        row_start: u64,
        vector_offset: usize,
    ) -> Result<bool> {
        if !self.check_zonemap(filters, row_start) {
            return Ok(false);
        }
        state.vector_index = vector_offset;
        state.max_row_group_row = if row_start > state.max_row {
            0
        } else {
            self.count().min(state.max_row - row_start)
        };
        if state.max_row_group_row == 0 {
            return Ok(false);
        }
        let row = vector_offset as u64 * VECTOR_SIZE;
        for (column_id, column_state) in column_ids.iter().zip(&mut state.column_scans) {
            if column_id.is_row_id() {
                continue;
            }
            let column = self.column(column_id.primary_index())?;
            column.initialize_scan_with_offset(column_state, row)?;
        }
        Ok(true)
    }

    /// Moves every column scan to the next vector (`NextVector`).
    fn next_vector(&self, state: &mut CollectionScanState, column_ids: &[StorageIndex]) -> Result<()> {
        state.vector_index += 1;
        for (column_id, column_state) in column_ids.iter().zip(&mut state.column_scans) {
            if column_id.is_row_id() {
                continue;
            }
            self.column(column_id.primary_index())?
                .skip(column_state, VECTOR_SIZE)?;
        }
        Ok(())
    }

    /// Fills `sel` with the rows of vector `vector_index` the scan sees
    /// (`GetSelVector`); returns `max_count` (with `sel` empty) when all are visible.
    pub fn sel_vector(
        &self,
        options: ScanOptions,
        vector_index: usize,
        sel: &mut Vec<u32>,
        max_count: usize,
    ) -> usize {
        if options.insert_type == InsertedScanType::AllRows
            && options.delete_type == DeletedScanType::IncludeAllDeleted
        {
            sel.clear();
            return max_count;
        }
        if let Some(info) = self.version_info() {
            return info.sel_vector(options, vector_index, sel, max_count);
        }
        sel.clear();
        max_count
    }

    /// Skips vectors whose segments' zone maps rule out a filter; returns `false` if the
    /// current vector was skipped (`CheckZonemapSegments`).
    fn check_zonemap_segments(
        &self,
        state: &mut CollectionScanState,
        column_ids: &[StorageIndex],
        filters: &ScanFilterInfo,
        row_start: u64,
    ) -> Result<bool> {
        let mut target_vector: Option<usize> = None;
        for entry in filters.filters().iter().filter(|entry| !entry.always_true) {
            if entry.table_column_index.is_row_id() {
                continue;
            }
            let column = self.column(entry.table_column_index.primary_index())?;
            let column_state = &mut state.column_scans[entry.scan_column_index];
            if column.check_zonemap(column_state, &entry.filter) != FilterPropagateResult::FilterAlwaysFalse {
                continue;
            }
            // skip to the end of the current segment
            let Some(segment) = column
                .segments()
                .find(column_state.offset_in_column)
                .map(|(_, node)| node)
            else {
                continue;
            };
            let segment_end = (row_start + segment.row_end()).min(state.max_row);
            let target = ((segment_end.saturating_sub(row_start)) / VECTOR_SIZE) as usize;
            target_vector = Some(target_vector.map_or(target, |current| current.max(target)));
        }
        let Some(target) = target_vector else {
            return Ok(true);
        };
        if state.vector_index >= target {
            return Ok(true);
        }
        while state.vector_index < target {
            self.next_vector(state, column_ids)?;
        }
        Ok(false)
    }

    /// Scans the next vector with visible rows into `result` (`RowGroup::Scan`); leaves
    /// `result` empty when the row group is exhausted.
    pub(crate) fn scan(
        &self,
        options: ScanOptions,
        state: &mut CollectionScanState,
        column_ids: &[StorageIndex],
        filters: &mut ScanFilterInfo,
        row_start: u64,
        result: &mut DataChunk,
    ) -> Result<()> {
        let transaction = options.transaction;
        loop {
            let current_row = state.vector_index as u64 * VECTOR_SIZE;
            if current_row >= state.max_row_group_row {
                result.set_len(0);
                return Ok(());
            }
            let max_count = (state.max_row_group_row - current_row).min(VECTOR_SIZE) as usize;
            // SYSTEM sampling skips whole vectors.
            if let Some(sampling) = &mut state.sampling
                && sampling.random.next_random() > sampling.sample_rate
            {
                self.next_vector(state, column_ids)?;
                continue;
            }
            if !self.check_zonemap_segments(state, column_ids, filters, row_start)? {
                continue;
            }
            let mut valid_sel = std::mem::take(&mut state.valid_sel);
            let count = self.sel_vector(options, state.vector_index, &mut valid_sel, max_count);
            if count == 0 {
                state.valid_sel = valid_sel;
                self.next_vector(state, column_ids)?;
                continue;
            }
            state.rows_scanned += count as u64;
            let vector_index = state.vector_index;
            let row_id_start = row_start + current_row;
            let has_filters = filters.has_filters();
            if count == max_count && !has_filters {
                for (index, column_id) in column_ids.iter().enumerate() {
                    let vector = result.column_mut(index);
                    if column_id.is_row_id() {
                        *vector = row_id_vector(row_id_start, max_count);
                        continue;
                    }
                    let column_state = &mut state.column_scans[index];
                    column_state.update_scan_type = options.update_type;
                    self.column(column_id.primary_index())?.scan(
                        transaction,
                        vector_index,
                        column_state,
                        vector,
                        max_count,
                    )?;
                }
                state.valid_sel = valid_sel;
                result.set_len(max_count);
                state.vector_index += 1;
                return Ok(());
            }
            let mut sel = std::mem::take(&mut state.filter_sel);
            sel.clear();
            if count == max_count {
                sel.extend(0..max_count as u32);
            } else {
                sel.extend_from_slice(&valid_sel);
            }
            state.valid_sel = valid_sel;
            let filter_start = filters.begin_filter();
            if has_filters {
                for &filter_index in filters.permutation() {
                    let entry = &filters.filters()[filter_index];
                    if entry.always_true {
                        continue;
                    }
                    let index = entry.scan_column_index;
                    if sel.is_empty() {
                        if !entry.table_column_index.is_row_id() {
                            self.column(entry.table_column_index.primary_index())?
                                .skip(&mut state.column_scans[index], VECTOR_SIZE)?;
                        }
                        continue;
                    }
                    let vector = result.column_mut(index);
                    if entry.table_column_index.is_row_id() {
                        *vector = row_id_vector(row_id_start, max_count);
                        entry.filter.select(vector, max_count, &mut sel)?;
                        continue;
                    }
                    let column = self.column(entry.table_column_index.primary_index())?;
                    let column_state = &mut state.column_scans[index];
                    column_state.update_scan_type = options.update_type;
                    column.filter(
                        transaction,
                        vector_index,
                        column_state,
                        vector,
                        &mut sel,
                        &entry.filter,
                        !filters.is_filter_only(index),
                    )?;
                }
            }
            if sel.is_empty() {
                // every row was filtered out: skip the vector in the columns not scanned yet
                for (index, column_id) in column_ids.iter().enumerate() {
                    if (has_filters && filters.column_has_filters(index)) || column_id.is_row_id() {
                        continue;
                    }
                    self.column(column_id.primary_index())?
                        .skip(&mut state.column_scans[index], VECTOR_SIZE)?;
                }
                state.filter_sel = sel;
                state.vector_index += 1;
                continue;
            }
            if sel.len() == max_count {
                // Every row passed: the filtered columns hold the whole vector, the others
                // are scanned without a selection.
                for (index, column_id) in column_ids.iter().enumerate() {
                    if has_filters && filters.column_has_filters(index) {
                        continue;
                    }
                    let vector = result.column_mut(index);
                    if column_id.is_row_id() {
                        *vector = row_id_vector(row_id_start, max_count);
                        continue;
                    }
                    let column_state = &mut state.column_scans[index];
                    column_state.update_scan_type = options.update_type;
                    self.column(column_id.primary_index())?.scan(
                        transaction,
                        vector_index,
                        column_state,
                        vector,
                        max_count,
                    )?;
                }
                filters.end_filter(filter_start);
                result.set_len(max_count);
                state.filter_sel = sel;
                state.vector_index += 1;
                return Ok(());
            }
            let selection = SelectionVector::new(&sel[..]);
            for (index, column_id) in column_ids.iter().enumerate() {
                let vector = result.column_mut(index);
                if has_filters && filters.column_has_filters(index) {
                    *vector = vector.slice(&selection);
                    continue;
                }
                if column_id.is_row_id() {
                    *vector = row_id_vector(row_id_start, max_count).slice(&selection);
                    continue;
                }
                let column_state = &mut state.column_scans[index];
                column_state.update_scan_type = options.update_type;
                self.column(column_id.primary_index())?.select(
                    transaction,
                    vector_index,
                    column_state,
                    vector,
                    &selection,
                )?;
            }
            filters.end_filter(filter_start);
            result.set_len(sel.len());
            state.filter_sel = sel;
            state.vector_index += 1;
            return Ok(());
        }
    }

    // ----------------------------------------------------------------------------------
    // Fetches
    // ----------------------------------------------------------------------------------

    /// Whether row `row` (within the row group) is visible to the transaction.
    pub fn fetch(&self, transaction: TransactionData, row: u64) -> Result<bool> {
        if row > self.count() {
            return Err(Error::internal(
                "RowGroup::Fetch - row_id out of range for row group",
            ));
        }
        Ok(self
            .version_info()
            .is_none_or(|info| info.fetch(transaction, row)))
    }

    /// Reads row `row` of the columns `column_ids` into `result[result_index]`
    /// (`FetchRow`).
    pub fn fetch_row(
        &self,
        transaction: TransactionData,
        column_ids: &[StorageIndex],
        row: u64,
        row_start: u64,
        result: &mut DataChunk,
        result_index: usize,
    ) -> Result<()> {
        for (index, column_id) in column_ids.iter().enumerate() {
            let vector = result.column_mut(index);
            if vector.row_capacity() <= result_index {
                vector.resize((result_index + 1).max(vector.row_capacity() * 2));
            }
            if column_id.is_row_id() {
                vector.values_mut::<i64>()[result_index] = (row_start + row) as i64;
                continue;
            }
            self.column(column_id.primary_index())?.fetch_row_index(
                transaction,
                column_id,
                row,
                vector,
                result_index,
            )?;
        }
        Ok(())
    }

    // ----------------------------------------------------------------------------------
    // Appends
    // ----------------------------------------------------------------------------------

    /// Prepares appending to every column (`InitializeAppend`).
    pub fn initialize_append(&self) -> Result<RowGroupAppendState> {
        Ok(RowGroupAppendState {
            states: self
                .columns()?
                .iter()
                .map(|column| column.initialize_append())
                .collect::<Result<_>>()?,
            offset_in_row_group: self.count(),
        })
    }

    /// Appends the first `count` rows of `chunk` (`Append`). The rows only become part of
    /// the row group with [`RowGroup::append_version_info`].
    pub fn append(&self, state: &mut RowGroupAppendState, chunk: &DataChunk, count: usize) -> Result<()> {
        for ((column, column_state), vector) in
            self.columns()?.iter().zip(&mut state.states).zip(chunk.columns())
        {
            column.append(column_state, vector, count)?;
        }
        state.offset_in_row_group += count as u64;
        Ok(())
    }

    /// Publishes `count` appended rows as inserted by the transaction, up to the row group
    /// size (`AppendVersionInfo`).
    pub fn append_version_info(&self, transaction: TransactionData, count: u64, row_group_size: u64) {
        let start = self.count();
        let end = (start + count).min(row_group_size);
        self.get_or_create_version_info()
            .append_version_info(transaction, start, end);
        self.set_count(end);
    }

    pub fn commit_append(&self, commit_id: u64, start: u64, count: u64) {
        self.get_or_create_version_info()
            .commit_append(commit_id, start, count);
    }

    /// Truncates the row group to `new_count` rows (`RevertAppend`).
    pub fn revert_append(&self, new_count: u64) -> Result<()> {
        if new_count > self.count() {
            return Err(Error::internal("RowGroup::RevertAppend new_count out of range"));
        }
        self.get_or_create_version_info().revert_append(new_count);
        for column in self.loaded_columns() {
            column.revert_append(new_count)?;
        }
        self.set_count(new_count);
        Ok(())
    }

    pub fn cleanup_append(&self, lowest_transaction: u64, start: u64, count: u64) {
        self.get_or_create_version_info()
            .cleanup_append(lowest_transaction, start, count);
    }

    // ----------------------------------------------------------------------------------
    // Deletes and updates
    // ----------------------------------------------------------------------------------

    /// Deletes rows `ids` (row ids within this row group starting at `row_group_start`)
    /// in the transaction (`RowGroup::Delete`). Returns the number of deleted rows and one
    /// record per touched vector for the undo log.
    pub fn delete(
        &self,
        transaction: TransactionData,
        ids: &[i64],
        row_group_start: u64,
    ) -> Result<(u64, Vec<DeleteRecord>)> {
        let info = self.get_or_create_version_info();
        let mut records = Vec::new();
        let mut deleted = 0;
        let mut current: Option<usize> = None;
        let mut rows: Vec<u16> = Vec::new();
        let flush = |vector: usize, rows: &mut Vec<u16>, records: &mut Vec<DeleteRecord>| -> Result<u64> {
            if rows.is_empty() {
                return Ok(0);
            }
            let count = info.delete_rows(vector, transaction.transaction_id, rows)?;
            if count > 0 && transaction.transaction_id != 0 {
                let consecutive = rows
                    .iter()
                    .enumerate()
                    .all(|(index, &row)| usize::from(row) == index);
                records.push(DeleteRecord {
                    version_info: Arc::clone(&info),
                    vector_index: vector,
                    base_row: row_group_start + vector as u64 * VECTOR_SIZE,
                    rows: if consecutive {
                        DeletedRows::Consecutive(count)
                    } else {
                        DeletedRows::Rows(rows.clone())
                    },
                });
            }
            rows.clear();
            Ok(count as u64)
        };
        for &id in ids {
            let row = (id as u64)
                .checked_sub(row_group_start)
                .filter(|row| *row < self.count())
                .ok_or_else(|| Error::internal("row id out of range of the row group"))?;
            let vector = (row / VECTOR_SIZE) as usize;
            if current != Some(vector) {
                if let Some(previous) = current {
                    deleted += flush(previous, &mut rows, &mut records)?;
                }
                current = Some(vector);
            }
            rows.push((row % VECTOR_SIZE) as u16);
        }
        if let Some(previous) = current {
            deleted += flush(previous, &mut rows, &mut records)?;
        }
        Ok((deleted, records))
    }

    /// Updates the sub-column at `column_path` (top-level column first) of rows `ids` (all
    /// in one vector) to `update` (`RowGroup::UpdateColumn`).
    pub fn update_column(
        &self,
        transaction: TransactionData,
        update: &Vector,
        ids: &[i64],
        column_path: &[usize],
        row_group_start: u64,
    ) -> Result<Vec<UpdateRecord>> {
        let (&column_index, rest) = column_path
            .split_first()
            .ok_or_else(|| Error::internal("empty column path"))?;
        let column = self.column(column_index)?;
        let registrations = column.update_column(transaction, rest, update, ids, row_group_start)?;
        if let Some(stats) = column.update_statistics() {
            column.merge_statistics(&stats)?;
        }
        Ok(registrations
            .into_iter()
            .map(|registration| UpdateRecord {
                column_index,
                registration,
            })
            .collect())
    }

    /// Updates `count` rows starting at `offset` of `ids`/`updates` (all in one vector)
    /// (`RowGroup::Update`).
    pub fn update(
        &self,
        transaction: TransactionData,
        updates: &DataChunk,
        ids: &[i64],
        column_ids: &[usize],
        row_group_start: u64,
    ) -> Result<Vec<UpdateRecord>> {
        let mut records = Vec::new();
        for (vector, &column_index) in updates.columns().iter().zip(column_ids) {
            let column = self.column(column_index)?;
            let registrations = column.update(transaction, vector, ids, row_group_start)?;
            if let Some(stats) = column.update_statistics() {
                column.merge_statistics(&stats)?;
            }
            records.extend(registrations.into_iter().map(|registration| UpdateRecord {
                column_index,
                registration,
            }));
        }
        Ok(records)
    }

    // ----------------------------------------------------------------------------------
    // Statistics
    // ----------------------------------------------------------------------------------

    /// Rows whose delete is not committed.
    pub fn committed_row_count(&self) -> u64 {
        let count = self.count();
        self.version_info()
            .map_or(count, |info| count - info.committed_deleted_count(count))
    }

    pub fn column_statistics(&self, column: usize) -> Result<BaseStatistics> {
        self.column(column)?.statistics()
    }

    /// Merges every built column's statistics into `stats` (a row group read from the
    /// file comes with the table's statistics, which cover the columns not built yet).
    pub fn merge_into_statistics(&self, stats: &mut [BaseStatistics]) -> Result<()> {
        for (slot, column_stats) in self.columns.iter().zip(stats) {
            if let Some(column) = slot.get() {
                column.merge_into_statistics(column_stats)?;
            }
        }
        Ok(())
    }

    /// Whether every column is stored in blocks only: an optimistic write wrote the row
    /// group (`IsPersistent`). Columns not built yet are stored.
    pub fn is_persistent(&self) -> bool {
        self.loaded_columns().all(|column| column.is_persistent())
    }

    /// The stored segments of every column of a row group that is persistent, with
    /// whether a column has updates, which keep it from being logged this way
    /// (`SerializeRowGroupInfo`).
    pub fn serialize_row_group_info(&self, start: u64) -> Result<(PersistentRowGroupData, bool)> {
        let mut has_updates = false;
        let mut column_data = Vec::with_capacity(self.column_count());
        for index in 0..self.column_count() {
            if let Some(stored) = self.unloaded_column_data(index)? {
                column_data.push(stored);
                continue;
            }
            let column = self.column(index)?;
            has_updates |= column.has_any_updates();
            column_data.push(column.persistent_data(0)?);
        }
        let data = PersistentRowGroupData {
            types: Vec::new(),
            column_data,
            start,
            count: self.count(),
        };
        Ok((data, has_updates))
    }

    /// Changes who owns the row group's data (`MoveToCollection`); columns not built yet
    /// are built as main table columns.
    pub fn set_data_type(&self, data_type: ColumnDataType) {
        for column in self.loaded_columns() {
            column.set_data_type(data_type);
        }
    }

    /// Scans the whole row group (every row, including deleted ones) of the given columns
    /// with the newest committed values (`ScanCommittedRange` per column).
    pub fn scan_committed(
        &self,
        column_ids: &[usize],
        mut f: impl FnMut(&DataChunk) -> Result<()>,
    ) -> Result<()> {
        let count = self.count();
        let mut offset = 0;
        while offset < count {
            let rows = (count - offset).min(VECTOR_SIZE) as usize;
            let mut columns = Vec::with_capacity(column_ids.len());
            for &index in column_ids {
                let column = self.column(index)?;
                let mut vector = Vector::flat(column.logical_type().clone(), rows);
                column.scan_committed_range(offset, rows, &mut vector)?;
                columns.push(vector);
            }
            f(&DataChunk::new(columns, rows))?;
            offset += rows as u64;
        }
        Ok(())
    }

    /// Column scan states for scanning `column_ids` of a table with `types`
    /// (`CollectionScanState::Initialize`); `casts` binds the casts of pushed-down
    /// extracts.
    pub(crate) fn column_scan_states(
        types: &[LogicalType],
        column_ids: &[StorageIndex],
        casts: Option<&ScanCasts>,
    ) -> Result<Vec<ColumnScanState>> {
        column_ids
            .iter()
            .map(|column_id| {
                if column_id.is_row_id() {
                    return Ok(ColumnScanState::default());
                }
                let ty = types.get(column_id.primary_index()).cloned().unwrap_or_default();
                ColumnScanState::initialize(&ty, column_id, casts)
            })
            .collect()
    }

    /// Scan options for scans that are not part of a transaction (`RowGroup::Scan` with a
    /// `TableScanType`).
    pub fn scan_options(scan_type: TableScanType, transaction: TransactionData) -> ScanOptions {
        let mut options = ScanOptions::new(transaction);
        options.insert_type = InsertedScanType::AllRows;
        match scan_type {
            TableScanType::AllRows => options.delete_type = DeletedScanType::IncludeAllDeleted,
            TableScanType::OmitPermanentlyDeleted | TableScanType::CommittedRows => {
                options.delete_type = DeletedScanType::OmitCommittedDeletes;
                options.update_type = UpdateScanType::DisallowUpdates;
            }
        }
        options
    }
}

/// A BIGINT vector of the row ids `start..start + count`.
fn row_id_vector(start: u64, count: usize) -> Vector {
    let mut vector = Vector::flat(LogicalType::BIGINT, count.max(1));
    for (row, value) in vector.values_mut::<i64>().iter_mut().take(count).enumerate() {
        *value = (start + row as u64) as i64;
    }
    vector
}

/// Checks a filter on `rowid` against the row id range of a row group
/// (`CheckRowIdFilter`).
fn check_row_id_filter(filter: &TableFilter, start: u64, end: u64) -> FilterPropagateResult {
    let mut stats = BaseStatistics::create_empty(LogicalType::BIGINT);
    stats.set_has_no_null_fast();
    if let Some(numeric) = stats.numeric_mut() {
        numeric.min = Some(crate::statistics::NumericValue::I64(start as i64));
        numeric.max = Some(crate::statistics::NumericValue::I64(end as i64));
    }
    filter.check_statistics(&stats)
}
