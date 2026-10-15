//! The storage of one table (DuckDB's `DataTable` and `DataTableInfo`).

use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use rustdb_buffer::{BlockManager, BufferManager};
use rustdb_common::{Error, ErrorKind, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::statistics::BaseStatistics;
use crate::table::{
    CollectionScanState, FetchType, ParallelTableScanState, RowGroupCollection, ScanOptions, StorageIndex,
    TableAppendState, TableScanState,
};
use crate::table_filter::TableFilterSet;
use crate::transaction::{LocalTableStorage, Transaction};
use crate::{DEFAULT_ROW_GROUP_SIZE, MAX_ROW_ID};

/// Where a table's blocks live (DuckDB's `TableIOManager`).
#[derive(Debug)]
pub struct TableIoManager {
    buffer_manager: BufferManager,
    block_manager: Arc<dyn BlockManager>,
    row_group_size: u64,
}

impl TableIoManager {
    pub fn new(buffer_manager: BufferManager, block_manager: Arc<dyn BlockManager>) -> Self {
        Self::with_row_group_size(buffer_manager, block_manager, DEFAULT_ROW_GROUP_SIZE)
    }

    pub fn with_row_group_size(
        buffer_manager: BufferManager,
        block_manager: Arc<dyn BlockManager>,
        row_group_size: u64,
    ) -> Self {
        Self {
            buffer_manager,
            block_manager,
            row_group_size,
        }
    }

    pub fn buffer_manager(&self) -> &BufferManager {
        &self.buffer_manager
    }

    pub fn block_manager(&self) -> &Arc<dyn BlockManager> {
        &self.block_manager
    }

    pub fn row_group_size(&self) -> u64 {
        self.row_group_size
    }
}

/// Shared information about a table (DuckDB's `DataTableInfo`).
#[derive(Debug)]
pub struct DataTableInfo {
    io: Arc<TableIoManager>,
    schema: RwLock<String>,
    table: RwLock<String>,
}

impl DataTableInfo {
    pub fn new(io: Arc<TableIoManager>, schema: impl Into<String>, table: impl Into<String>) -> Self {
        Self {
            io,
            schema: RwLock::new(schema.into()),
            table: RwLock::new(table.into()),
        }
    }

    pub fn io(&self) -> &Arc<TableIoManager> {
        &self.io
    }

    pub fn schema_name(&self) -> String {
        self.schema.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn table_name(&self) -> String {
        self.table.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn set_table_name(&self, name: impl Into<String>) {
        *self.table.write().unwrap_or_else(PoisonError::into_inner) = name.into();
    }
}

/// A stored column of a table (the storage part of DuckDB's `ColumnDefinition`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnDefinition {
    pub name: String,
    pub ty: LogicalType,
}

impl ColumnDefinition {
    pub fn new(name: impl Into<String>, ty: LogicalType) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }
}

/// Constraint checks that need expression evaluation (CHECK constraints, generated
/// columns), implemented by the execution layer.
pub trait ConstraintVerifier: Send + Sync + fmt::Debug {
    /// Verifies rows about to be inserted (`VerifyAppendConstraints`).
    fn verify_append(&self, chunk: &DataChunk) -> Result<()>;

    /// Verifies updated values; `column_ids` are the table columns of `chunk`'s columns
    /// (`VerifyUpdateConstraints`).
    fn verify_update(&self, chunk: &DataChunk, column_ids: &[usize]) -> Result<()> {
        let _ = (chunk, column_ids);
        Ok(())
    }
}

/// The constraints a table's storage enforces (DuckDB's bound constraints).
#[derive(Clone, Debug, Default)]
pub struct TableConstraints {
    /// Columns that may not be NULL, by physical index.
    pub not_null: Vec<usize>,
    /// CHECK constraints and other expression-based checks.
    pub verifier: Option<Arc<dyn ConstraintVerifier>>,
}

/// Whether a table is still the current version (DuckDB's `DataTableVersion`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum TableVersion {
    MainTable = 0,
    Altered = 1,
    Dropped = 2,
}

impl TableVersion {
    fn describe(self) -> &'static str {
        match self {
            Self::MainTable => "no changes",
            Self::Altered => "altered",
            Self::Dropped => "dropped",
        }
    }
}

/// The state of a transaction-local append (DuckDB's `LocalAppendState`).
#[derive(Debug)]
pub struct LocalAppendState {
    storage: Arc<LocalTableStorage>,
    append: TableAppendState,
    constraints: TableConstraints,
}

/// The storage of a table: its row groups and the locks around appends.
#[derive(Debug)]
pub struct DataTable {
    info: Arc<DataTableInfo>,
    columns: Vec<ColumnDefinition>,
    row_groups: Arc<RowGroupCollection>,
    append_lock: Mutex<()>,
    version: AtomicU8,
}

fn constraint_error(message: String) -> Error {
    Error::new(ErrorKind::Constraint, message)
}

impl DataTable {
    /// An empty table.
    pub fn new(
        io: Arc<TableIoManager>,
        schema: impl Into<String>,
        table: impl Into<String>,
        columns: Vec<ColumnDefinition>,
    ) -> Arc<Self> {
        let types: Vec<LogicalType> = columns.iter().map(|column| column.ty.clone()).collect();
        let row_groups = Arc::new(RowGroupCollection::new(Arc::clone(&io), types, 0));
        Arc::new(Self {
            info: Arc::new(DataTableInfo::new(io, schema, table)),
            columns,
            row_groups,
            append_lock: Mutex::new(()),
            version: AtomicU8::new(TableVersion::MainTable as u8),
        })
    }

    pub fn info(&self) -> &Arc<DataTableInfo> {
        &self.info
    }

    pub fn io(&self) -> &Arc<TableIoManager> {
        self.info.io()
    }

    pub fn columns(&self) -> &[ColumnDefinition] {
        &self.columns
    }

    pub fn types(&self) -> Vec<LogicalType> {
        self.columns.iter().map(|column| column.ty.clone()).collect()
    }

    pub fn name(&self) -> String {
        self.info.table_name()
    }

    pub fn row_groups(&self) -> &Arc<RowGroupCollection> {
        &self.row_groups
    }

    /// Committed and uncommitted rows of the base table (deleted ones included).
    pub fn total_rows(&self) -> u64 {
        self.row_groups.total_rows()
    }

    pub fn row_group_count(&self) -> usize {
        self.row_groups.row_group_count()
    }

    fn version(&self) -> TableVersion {
        match self.version.load(Ordering::Acquire) {
            1 => TableVersion::Altered,
            2 => TableVersion::Dropped,
            _ => TableVersion::MainTable,
        }
    }

    /// Whether this is the current version of the table (`IsMainTable`).
    pub fn is_main_table(&self) -> bool {
        self.version() == TableVersion::MainTable
    }

    /// Marks the table as replaced by an altered version.
    pub fn set_as_altered(&self) {
        self.version.store(TableVersion::Altered as u8, Ordering::Release);
    }

    /// Marks the table as dropped.
    pub fn set_as_dropped(&self) {
        self.version.store(TableVersion::Dropped as u8, Ordering::Release);
    }

    /// Reverts [`DataTable::set_as_altered`] / [`DataTable::set_as_dropped`].
    pub fn set_as_main_table(&self) {
        self.version
            .store(TableVersion::MainTable as u8, Ordering::Release);
    }

    /// Fails if another transaction altered or dropped the table.
    pub(crate) fn check_main_table(&self, action: &str) -> Result<()> {
        let version = self.version();
        if version == TableVersion::MainTable {
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Transaction,
            format!(
                "Transaction conflict: attempting to {action} table \"{}\" but it has been {} by a different transaction",
                self.name(),
                version.describe()
            ),
        ))
    }

    pub(crate) fn check_main_table_for_commit(&self) -> Result<()> {
        let version = self.version();
        if version == TableVersion::MainTable {
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Transaction,
            format!(
                "Attempting to modify table {} but another transaction has {} this table",
                self.name(),
                version.describe()
            ),
        ))
    }

    pub(crate) fn append_lock(&self) -> MutexGuard<'_, ()> {
        self.append_lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // ----------------------------------------------------------------------------------
    // Scans
    // ----------------------------------------------------------------------------------

    /// Starts a scan of the table and the transaction's local rows (`InitializeScan`).
    pub fn initialize_scan(
        self: &Arc<Self>,
        transaction: &Transaction,
        state: &mut TableScanState,
        column_ids: Vec<StorageIndex>,
        filters: Option<&TableFilterSet>,
    ) -> Result<()> {
        state.initialize(column_ids, filters);
        self.row_groups
            .initialize_scan(&mut state.table_state, &state.column_ids, &mut state.filters)?;
        state.local_state = CollectionScanState::default();
        if let Some(local) = transaction.local_storage().storage(self) {
            let collection = local.collection();
            if collection.total_rows() > 0 {
                collection.initialize_scan(&mut state.local_state, &state.column_ids, &mut state.filters)?;
            }
        }
        Ok(())
    }

    /// Scans the next chunk visible to the transaction; leaves `result` empty at the end
    /// (`DataTable::Scan`).
    pub fn scan(
        &self,
        transaction: &Transaction,
        result: &mut DataChunk,
        state: &mut TableScanState,
    ) -> Result<()> {
        Self::prepare_chunk(result, &state.column_ids, &self.row_groups);
        let options = ScanOptions::new(transaction.data());
        if RowGroupCollection::scan(
            options,
            &mut state.table_state,
            &state.column_ids,
            &mut state.filters,
            result,
        )? {
            return Ok(());
        }
        RowGroupCollection::scan(
            options,
            &mut state.local_state,
            &state.column_ids,
            &mut state.filters,
            result,
        )?;
        Ok(())
    }

    /// Makes `result` hold one vector per scanned column.
    fn prepare_chunk(result: &mut DataChunk, column_ids: &[StorageIndex], collection: &RowGroupCollection) {
        if result.column_count() == column_ids.len() {
            result.set_len(0);
            return;
        }
        let types: Vec<LogicalType> = column_ids
            .iter()
            .map(|column| {
                if column.is_row_id() {
                    LogicalType::BIGINT
                } else {
                    collection
                        .types()
                        .get(column.primary_index())
                        .cloned()
                        .unwrap_or_default()
                }
            })
            .collect();
        *result = DataChunk::with_types(&types, STANDARD_VECTOR_SIZE);
    }

    /// Prepares a parallel scan of the table and the transaction's local rows
    /// (`InitializeParallelScan`).
    pub fn initialize_parallel_scan(self: &Arc<Self>, transaction: &Transaction) -> ParallelTableScanState {
        let state = ParallelTableScanState::default();
        self.row_groups.initialize_parallel_scan(&state.scan_state);
        if let Some(local) = transaction.local_storage().storage(self) {
            local.collection().initialize_parallel_scan(&state.local_state);
        }
        state
    }

    /// Hands the next morsel (row group) to `state`; returns `false` when there is none
    /// (`NextParallelScan`). Scan it with [`DataTable::scan_morsel`].
    pub fn next_parallel_scan(
        self: &Arc<Self>,
        transaction: &Transaction,
        parallel: &ParallelTableScanState,
        state: &mut TableScanState,
    ) -> Result<bool> {
        if self.row_groups.next_parallel_scan(
            &parallel.scan_state,
            &mut state.table_state,
            &state.column_ids,
            &mut state.filters,
        )? {
            state.local_state = CollectionScanState::default();
            return Ok(true);
        }
        state.table_state = CollectionScanState::default();
        if let Some(local) = transaction.local_storage().storage(self) {
            return local.collection().next_parallel_scan(
                &parallel.local_state,
                &mut state.local_state,
                &state.column_ids,
                &mut state.filters,
            );
        }
        Ok(false)
    }

    /// Scans the next chunk of the current morsel; returns `false` when the morsel is
    /// exhausted.
    pub fn scan_morsel(
        &self,
        transaction: &Transaction,
        result: &mut DataChunk,
        state: &mut TableScanState,
    ) -> Result<bool> {
        Self::prepare_chunk(result, &state.column_ids, &self.row_groups);
        let options = ScanOptions::new(transaction.data());
        let collection_state = if state.table_state.row_group.is_some() {
            &mut state.table_state
        } else {
            &mut state.local_state
        };
        RowGroupCollection::scan(
            options,
            collection_state,
            &state.column_ids,
            &mut state.filters,
            result,
        )
    }

    /// Reads the rows `row_ids` visible to the transaction (`Fetch`).
    pub fn fetch(
        self: &Arc<Self>,
        transaction: &Transaction,
        result: &mut DataChunk,
        column_ids: &[StorageIndex],
        row_ids: &[i64],
    ) -> Result<()> {
        Self::prepare_chunk(result, column_ids, &self.row_groups);
        let (local_ids, table_ids): (Vec<i64>, Vec<i64>) =
            row_ids.iter().partition(|&&id| id as u64 >= MAX_ROW_ID);
        self.row_groups.fetch(
            transaction.data(),
            result,
            column_ids,
            &table_ids,
            FetchType::Transactional,
        )?;
        if !local_ids.is_empty()
            && let Some(local) = transaction.local_storage().storage(self)
        {
            let mut local_chunk = DataChunk::default();
            Self::prepare_chunk(&mut local_chunk, column_ids, &self.row_groups);
            local.collection().fetch(
                transaction.data(),
                &mut local_chunk,
                column_ids,
                &local_ids,
                FetchType::Transactional,
            )?;
            append_rows(result, &local_chunk)?;
        }
        Ok(())
    }

    /// Statistics of column `column` (`GetStatistics`).
    pub fn column_statistics(&self, column: usize) -> Option<BaseStatistics> {
        self.row_groups.column_statistics(column)
    }

    // ----------------------------------------------------------------------------------
    // Appends
    // ----------------------------------------------------------------------------------

    /// Starts appending to the transaction-local storage (`InitializeLocalAppend`).
    pub fn initialize_local_append(
        self: &Arc<Self>,
        transaction: &Transaction,
        constraints: TableConstraints,
    ) -> Result<LocalAppendState> {
        self.check_main_table("insert into")?;
        let storage = transaction.local_storage().get_or_create(self);
        let append = storage.collection().initialize_append(transaction.data())?;
        Ok(LocalAppendState {
            storage,
            append,
            constraints,
        })
    }

    /// Verifies NOT NULL and CHECK constraints of rows about to be inserted
    /// (`VerifyAppendConstraints`).
    pub fn verify_append_constraints(&self, constraints: &TableConstraints, chunk: &DataChunk) -> Result<()> {
        if let Some(verifier) = &constraints.verifier {
            verifier.verify_append(chunk)?;
        }
        for &column in &constraints.not_null {
            if has_null(chunk.column(column), chunk.len()) {
                return Err(self.not_null_error(column));
            }
        }
        Ok(())
    }

    fn not_null_error(&self, column: usize) -> Error {
        let name = self
            .columns
            .get(column)
            .map_or("", |definition| definition.name.as_str());
        constraint_error(format!("NOT NULL constraint failed: {}.{name}", self.name()))
    }

    /// Appends `chunk` to the transaction-local storage after verifying the constraints
    /// (`LocalAppend`).
    pub fn local_append(&self, state: &mut LocalAppendState, chunk: &DataChunk) -> Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        self.check_main_table("insert into")?;
        self.verify_append_constraints(&state.constraints, chunk)?;
        state.storage.collection().append(chunk, &mut state.append)?;
        Ok(())
    }

    /// Publishes the appended rows to the transaction (`FinalizeLocalAppend`).
    pub fn finalize_local_append(&self, mut state: LocalAppendState) {
        let transaction = state.append.transaction;
        state
            .storage
            .collection()
            .finalize_append(transaction, &mut state.append);
    }

    /// Appends `chunk` in one go (`DataTable::LocalAppend` with its own state).
    pub fn append_chunk(
        self: &Arc<Self>,
        transaction: &Transaction,
        chunk: &DataChunk,
        constraints: TableConstraints,
    ) -> Result<()> {
        let mut state = self.initialize_local_append(transaction, constraints)?;
        let result = self.local_append(&mut state, chunk);
        self.finalize_local_append(state);
        result
    }

    /// Marks appended rows as committed (`CommitAppend`).
    pub fn commit_append(&self, commit_id: u64, row_start: u64, count: u64) {
        let _lock = self.append_lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.row_groups.commit_append(commit_id, row_start, count);
    }

    /// Removes the rows of a rolled-back append (`RevertAppend`).
    pub fn revert_append(&self, row_start: u64, count: u64) -> Result<()> {
        let _ = count;
        let _lock = self.append_lock.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.is_main_table() {
            self.set_as_main_table();
        }
        self.row_groups.revert_append(row_start)
    }

    /// Drops version info of appended rows visible to everyone (`CleanupAppend`).
    pub fn cleanup_append(&self, lowest_transaction: u64, row_start: u64, count: u64) {
        self.row_groups
            .cleanup_append(lowest_transaction, row_start, count);
    }

    // ----------------------------------------------------------------------------------
    // Deletes and updates
    // ----------------------------------------------------------------------------------

    /// Deletes the rows `row_ids` in the transaction; returns how many were deleted
    /// (`DataTable::Delete`). Rows deleted by a concurrent transaction are a conflict.
    pub fn delete(self: &Arc<Self>, transaction: &Transaction, row_ids: &[i64]) -> Result<u64> {
        let mut deleted = 0;
        let mut position = 0;
        while position < row_ids.len() {
            let start = position;
            let local = row_ids[start] as u64 >= MAX_ROW_ID;
            while position < row_ids.len() && (row_ids[position] as u64 >= MAX_ROW_ID) == local {
                position += 1;
            }
            let ids = &row_ids[start..position];
            if local {
                deleted += transaction.local_storage().delete(self, ids)?;
            } else {
                let (count, records) = self.row_groups.delete(transaction.data(), ids)?;
                transaction.push_deletes(self, records);
                deleted += count;
            }
        }
        Ok(deleted)
    }

    /// Updates columns `column_ids` of the rows `row_ids` to the rows of `updates`
    /// (`DataTable::Update`).
    pub fn update(
        self: &Arc<Self>,
        transaction: &Transaction,
        row_ids: &[i64],
        column_ids: &[usize],
        updates: &DataChunk,
        constraints: &TableConstraints,
    ) -> Result<()> {
        if updates.is_empty() {
            return Ok(());
        }
        self.check_main_table("update")?;
        self.verify_update_constraints(constraints, updates, column_ids)?;
        let (local, global): (Vec<usize>, Vec<usize>) =
            (0..updates.len()).partition(|&row| row_ids[row] as u64 >= MAX_ROW_ID);
        let slice = |rows: &[usize]| -> (Vec<i64>, DataChunk) {
            let sel =
                rustdb_types::SelectionVector::new(rows.iter().map(|&row| row as u32).collect::<Vec<_>>());
            let ids = rows.iter().map(|&row| row_ids[row]).collect();
            let columns = updates
                .columns()
                .iter()
                .map(|column| column.slice(&sel).flattened(rows.len()))
                .collect();
            (ids, DataChunk::new(columns, rows.len()))
        };
        if !local.is_empty() {
            let (ids, chunk) = slice(&local);
            transaction
                .local_storage()
                .update(self, &ids, column_ids, &chunk)?;
        }
        if !global.is_empty() {
            let (ids, chunk) = if local.is_empty() {
                (row_ids[..updates.len()].to_vec(), updates.clone())
            } else {
                slice(&global)
            };
            let records = self
                .row_groups
                .update(transaction.data(), &ids, column_ids, &chunk)?;
            transaction.push_updates(self, records);
        }
        Ok(())
    }

    /// Verifies NOT NULL and CHECK constraints of updated values
    /// (`VerifyUpdateConstraints`).
    pub fn verify_update_constraints(
        &self,
        constraints: &TableConstraints,
        chunk: &DataChunk,
        column_ids: &[usize],
    ) -> Result<()> {
        for &column in &constraints.not_null {
            if let Some(position) = column_ids.iter().position(|&id| id == column)
                && has_null(chunk.column(position), chunk.len())
            {
                return Err(self.not_null_error(column));
            }
        }
        if let Some(verifier) = &constraints.verifier {
            verifier.verify_update(chunk, column_ids)?;
        }
        Ok(())
    }
}

/// Whether any of the first `count` rows of `vector` is NULL (`VectorOperations::HasNull`).
fn has_null(vector: &Vector, count: usize) -> bool {
    if vector.kind() == rustdb_types::VectorKind::Sequence {
        return false;
    }
    let validity = vector.validity();
    if validity.is_all_valid() {
        return false;
    }
    let sel = vector.sel();
    (0..count).any(|row| !validity.is_valid(sel.slot(row)))
}

/// Appends the rows of `source` to `target` (both with the same columns).
fn append_rows(target: &mut DataChunk, source: &DataChunk) -> Result<()> {
    let base = target.len();
    let total = base + source.len();
    for index in 0..target.column_count() {
        let vector = target.column_mut(index);
        if vector.row_capacity() < total {
            vector.resize(total);
        }
        for row in 0..source.len() {
            vector.set_value(base + row, &source.column(index).get_value(row))?;
        }
    }
    target.set_len(total);
    Ok(())
}
