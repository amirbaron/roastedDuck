//! A horizontal partition of a table of up to 122,880 rows (DuckDB's `RowGroup`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_types::{DataChunk, LogicalType, SelectionVector, Vector};

use super::chunk_info::{DeletedRows, DeletedScanType, InsertedScanType, ScanOptions, UpdateScanType};
use super::column_data::{
    ColumnAppendState, ColumnData, ColumnDataType, ColumnScanState, UpdateRegistration,
};
use super::row_version_manager::RowVersionManager;
use super::scan_state::{CollectionScanState, ScanFilterInfo, StorageIndex};
use super::segment_tree::Segment;
use crate::VECTOR_SIZE;
use crate::data_table::TableIoManager;
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

/// A row group: one column data per column plus the rows' versions.
#[derive(Debug)]
pub struct RowGroup {
    count: AtomicU64,
    columns: Vec<Arc<ColumnData>>,
    version_info: Mutex<Option<Arc<RowVersionManager>>>,
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
            columns,
            version_info: Mutex::default(),
        }
    }

    /// A row group sharing this one's columns and versions, with `columns` replaced
    /// (`CreateNewRowGroupCopy`, used by ALTER TABLE).
    #[must_use]
    pub fn with_columns(&self, columns: Vec<Arc<ColumnData>>) -> Self {
        Self {
            count: AtomicU64::new(self.count()),
            columns,
            version_info: Mutex::new(self.version_info_lock().clone()),
        }
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

    pub fn column(&self, index: usize) -> Result<&Arc<ColumnData>> {
        self.columns
            .get(index)
            .ok_or_else(|| Error::internal(format!("row group has no column {index}")))
    }

    pub fn columns(&self) -> &[Arc<ColumnData>] {
        &self.columns
    }

    /// Bytes of transient segments allocated by appends.
    pub fn allocation_size(&self) -> u64 {
        self.columns.iter().map(|column| column.allocation_size()).sum()
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

    fn get_or_create_version_info(&self) -> Arc<RowVersionManager> {
        Arc::clone(self.version_info_lock().get_or_insert_with(Arc::default))
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
            } else {
                match self.columns.get(entry.table_column_index.primary_index()) {
                    Some(column) => column.check_zonemap_column(&entry.filter),
                    None => FilterPropagateResult::NoPruningPossible,
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
            let mut sel: Vec<u32> = if count == max_count {
                (0..max_count as u32).collect()
            } else {
                valid_sel.clone()
            };
            state.valid_sel = valid_sel;
            if has_filters {
                for entry in filters.filters().iter().filter(|entry| !entry.always_true) {
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
                state.vector_index += 1;
                continue;
            }
            let selection = SelectionVector::new(sel.clone());
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
            result.set_len(sel.len());
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
            self.column(column_id.primary_index())?
                .fetch_row(transaction, row, vector, result_index)?;
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
                .columns
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
            self.columns.iter().zip(&mut state.states).zip(chunk.columns())
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
        for column in &self.columns {
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

    /// Merges every column's statistics into `stats`.
    pub fn merge_into_statistics(&self, stats: &mut [BaseStatistics]) -> Result<()> {
        for (column, column_stats) in self.columns.iter().zip(stats) {
            column.merge_into_statistics(column_stats)?;
        }
        Ok(())
    }

    /// Changes who owns the row group's data (`MoveToCollection`).
    pub fn set_data_type(&self, data_type: ColumnDataType) {
        for column in &self.columns {
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

    /// Column scan states for scanning `column_ids` of a table with `types`.
    pub(crate) fn column_scan_states(
        types: &[LogicalType],
        column_ids: &[StorageIndex],
    ) -> Vec<ColumnScanState> {
        column_ids
            .iter()
            .map(|column_id| {
                if column_id.is_row_id() {
                    return ColumnScanState::default();
                }
                let ty = types.get(column_id.primary_index()).cloned().unwrap_or_default();
                let mut state = ColumnScanState::for_type(&ty);
                if !column_id.children.is_empty() {
                    let fields = ty.struct_children().map_or(0, <[_]>::len);
                    let mut scan = vec![false; fields];
                    for child in &column_id.children {
                        if let Some(flag) = scan.get_mut(child.primary_index()) {
                            *flag = true;
                        }
                    }
                    state.scan_child_column = scan;
                }
                state
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
