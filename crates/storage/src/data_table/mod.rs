//! The storage of one table (DuckDB's `DataTable` and `DataTableInfo`).

mod indexes;
mod optimistic;

pub use indexes::{BoundForeignKey, ForeignKeyKind};
pub use optimistic::OptimisticCollection;

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use rustdb_buffer::{BlockManager, BufferManager};
use rustdb_common::{Error, ErrorKind, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::index::TableIndexList;
use crate::statistics::BaseStatistics;
use crate::table::{
    CollectionScanState, FetchType, ParallelTableScanState, RowGroupCollection, ScanOptions, StorageIndex,
    TableAppendState, TableScanState,
};
use crate::table_filter::TableFilterSet;
use crate::transaction::{LocalStorage, LocalTableStorage, Transaction, TransactionData};
use crate::{DEFAULT_ROW_GROUP_SIZE, MAX_ROW_ID};

/// Where a table's blocks live (DuckDB's `TableIOManager`).
#[derive(Debug)]
pub struct TableIoManager {
    buffer_manager: BufferManager,
    block_manager: Arc<dyn BlockManager>,
    row_group_size: u64,
    /// The serialization version of the database (`StorageManager::GetStorageVersion`);
    /// in-memory databases use the latest.
    storage_version: u64,
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
            storage_version: rustdb_serde::SerializationCompatibility::latest().serialization_version,
        }
    }

    /// Sets the serialization version of the database file.
    #[must_use]
    pub fn with_storage_version(mut self, storage_version: u64) -> Self {
        self.storage_version = storage_version;
        self
    }

    /// The serialization version of the database (`StorageManager::GetStorageVersion`).
    pub fn storage_version(&self) -> u64 {
        self.storage_version
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
    /// The indexes, shared by every version of the table.
    indexes: TableIndexList,
    /// The storage versions recorded with the indexes read from the database file, until
    /// the indexes are built (`IndexStorageInfo::options`).
    stored_index_versions: Mutex<HashMap<String, Option<u64>>>,
}

impl DataTableInfo {
    pub fn new(io: Arc<TableIoManager>, schema: impl Into<String>, table: impl Into<String>) -> Self {
        Self {
            io,
            schema: RwLock::new(schema.into()),
            table: RwLock::new(table.into()),
            indexes: TableIndexList::default(),
            stored_index_versions: Mutex::default(),
        }
    }

    /// Records the storage versions of the indexes read with the table.
    pub fn set_stored_index_versions(&self, versions: HashMap<String, Option<u64>>) {
        *self
            .stored_index_versions
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = versions;
    }

    /// The storage version index `name` was stored with, once (`Some(None)` when it was
    /// stored without one); `None` for indexes not read from the file.
    pub(crate) fn take_stored_index_version(&self, name: &str) -> Option<Option<u64>> {
        self.stored_index_versions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name)
    }

    /// The table's indexes.
    pub fn indexes(&self) -> &TableIndexList {
        &self.indexes
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
    /// `USING COMPRESSION` (`ColumnDefinition::CompressionType`); `Auto` by default.
    pub compression: crate::compression::CompressionType,
}

impl ColumnDefinition {
    pub fn new(name: impl Into<String>, ty: LogicalType) -> Self {
        Self {
            name: name.into(),
            ty,
            compression: crate::compression::CompressionType::Auto,
        }
    }

    /// The column with compression `compression`.
    #[must_use]
    pub fn with_compression(mut self, compression: crate::compression::CompressionType) -> Self {
        self.compression = compression;
        self
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
///
/// PRIMARY KEY and UNIQUE constraints are enforced by the table's indexes and need no
/// entry here. Appends verify unique indexes, then NOT NULL, then `verifier`, then the
/// append-side foreign keys; deletes verify the delete-side foreign keys.
#[derive(Clone, Debug, Default)]
pub struct TableConstraints {
    /// Columns that may not be NULL, by physical index.
    pub not_null: Vec<usize>,
    /// CHECK constraints and other expression-based checks.
    pub verifier: Option<Arc<dyn ConstraintVerifier>>,
    /// Foreign keys in which the table takes part.
    pub foreign_keys: Vec<BoundForeignKey>,
    /// The catalog names of the physical columns, for error messages (empty: the names
    /// the storage was created with; columns can be renamed since).
    pub column_names: Vec<String>,
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
    local_storage: Arc<LocalStorage>,
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

pub(crate) fn constraint_error(message: String) -> Error {
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

    /// A table over existing row groups (read from a checkpoint).
    pub fn from_collection(
        io: Arc<TableIoManager>,
        schema: impl Into<String>,
        table: impl Into<String>,
        columns: Vec<ColumnDefinition>,
        row_groups: RowGroupCollection,
    ) -> Arc<Self> {
        Arc::new(Self {
            info: Arc::new(DataTableInfo::new(io, schema, table)),
            columns,
            row_groups: Arc::new(row_groups),
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
    /// How many threads can usefully scan the table: one per row group
    /// (`DataTable::MaxThreads`).
    pub fn max_threads(&self) -> usize {
        let vector_size = rustdb_common::STANDARD_VECTOR_SIZE as u64;
        let parallel_scan_tuple_count = (self.row_groups.row_group_size() / vector_size).max(1) * vector_size;
        usize::try_from(self.total_rows() / parallel_scan_tuple_count + 1).unwrap_or(usize::MAX)
    }

    pub fn total_rows(&self) -> u64 {
        self.row_groups.total_rows()
    }

    pub fn row_group_count(&self) -> usize {
        self.row_groups.row_group_count()
    }

    /// The row groups of the table and of the transaction's local rows
    /// (`GetRowGroupCountWithLocalStorage`).
    pub fn row_group_count_with_local_storage(self: &Arc<Self>, transaction: &Transaction) -> usize {
        self.row_group_count()
            + transaction
                .local_storage()
                .storage(self)
                .map_or(0, |local| local.collection().filled_row_group_count())
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

    /// Prepares a parallel scan of only the transaction's local rows (the local phase of
    /// an index scan).
    pub fn initialize_local_parallel_scan(
        self: &Arc<Self>,
        transaction: &Transaction,
    ) -> ParallelTableScanState {
        let state = ParallelTableScanState::default();
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
        // The local rows' batches follow the table's (`TableScanGetPartitionData`); the
        // rows scanned so far stay counted.
        state.table_state = CollectionScanState {
            batch_index: state.table_state.batch_index,
            rows_scanned: state.table_state.rows_scanned,
            ..CollectionScanState::default()
        };
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

    /// Calls `function` with the rows `row_start..row_start + count` of the base table,
    /// regardless of their versions, in chunks of at most one vector
    /// (`ScanTableSegment`); the WAL logs committed appends with it.
    pub fn scan_table_segment(
        &self,
        transaction: TransactionData,
        row_start: u64,
        count: u64,
        mut function: impl FnMut(&DataChunk) -> Result<()>,
    ) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let end = row_start + count;
        let column_ids: Vec<StorageIndex> = (0..self.columns.len()).map(StorageIndex::from).collect();
        let types = self.types();
        let mut filters = crate::table::ScanFilterInfo::new(None, &column_ids);
        let mut state = CollectionScanState::default();
        self.row_groups
            .initialize_scan_with_offset(&mut state, &column_ids, &mut filters, row_start, end)?;
        let mut current_row = state.row_group.as_ref().map_or(row_start, |(_, node)| {
            node.row_start + state.vector_index as u64 * crate::VECTOR_SIZE
        });
        let mut chunk = DataChunk::with_types(&types, STANDARD_VECTOR_SIZE);
        while current_row < end {
            chunk.reset();
            if !self.row_groups.scan_all(
                crate::table::TableScanType::AllRows,
                transaction,
                &mut state,
                &column_ids,
                &mut filters,
                &mut chunk,
            )? || chunk.is_empty()
            {
                break;
            }
            let end_row = current_row + chunk.len() as u64;
            let chunk_start = current_row.max(row_start);
            let chunk_end = end_row.min(end);
            let chunk_count = (chunk_end - chunk_start) as usize;
            if chunk_count == chunk.len() {
                function(&chunk)?;
            } else {
                let offset = (chunk_start - current_row) as u32;
                let sel = rustdb_types::SelectionVector::new(
                    (offset..offset + chunk_count as u32).collect::<Vec<_>>(),
                );
                let columns = chunk
                    .columns()
                    .iter()
                    .map(|column| column.slice(&sel).flattened(chunk_count))
                    .collect();
                function(&DataChunk::new(columns, chunk_count))?;
            }
            current_row = end_row;
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
        let storage = transaction.local_storage().get_or_create(self)?;
        let append = storage.collection().initialize_append(transaction.data())?;
        Ok(LocalAppendState {
            storage,
            local_storage: transaction.local_storage_arc(),
            append,
            constraints,
        })
    }

    /// Verifies NOT NULL and CHECK constraints of rows about to be inserted
    /// (`VerifyAppendConstraints`).
    pub fn verify_append_constraints(&self, constraints: &TableConstraints, chunk: &DataChunk) -> Result<()> {
        for &column in &constraints.not_null {
            if has_null(chunk.column(column), chunk.len()) {
                return Err(self.not_null_error(column, &constraints.column_names));
            }
        }
        if let Some(verifier) = &constraints.verifier {
            verifier.verify_append(chunk)?;
        }
        Ok(())
    }

    /// "NOT NULL constraint failed: t.c", naming the column by `names` if given.
    fn not_null_error(&self, column: usize, names: &[String]) -> Error {
        let name = names.get(column).map_or_else(
            || {
                self.columns
                    .get(column)
                    .map_or("", |definition| definition.name.as_str())
            },
            String::as_str,
        );
        constraint_error(format!("NOT NULL constraint failed: {}.{name}", self.name()))
    }

    /// Appends `chunk` to the transaction-local storage after verifying the constraints
    /// (`LocalAppend`).
    pub fn local_append(&self, state: &mut LocalAppendState, chunk: &DataChunk) -> Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        self.check_main_table("insert into")?;
        if self.has_unique_indexes() {
            self.verify_unique_indexes(Some(&state.storage), chunk)?;
        }
        self.verify_append_constraints(&state.constraints, chunk)?;
        Self::verify_append_foreign_keys(
            &state.local_storage,
            Some(&state.storage),
            &state.constraints,
            chunk,
        )?;
        if !state.storage.append_indexes.is_empty() {
            Self::append_to_indexes(
                &state.storage.append_indexes,
                Some(&state.storage.delete_indexes),
                chunk,
                state.append.current_row as i64,
                state.storage.index_append_mode(),
            )?;
        }
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

    /// Removes the rows of a rolled-back append from the indexes and the table
    /// (`RevertAppend`).
    pub fn revert_append(&self, row_start: u64, count: u64) -> Result<()> {
        let _lock = self.append_lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.revert_index_append(row_start, count)?;
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
        self.delete_with_constraints(transaction, row_ids, &TableConstraints::default())
    }

    /// [`DataTable::delete`] that also verifies the delete-side foreign keys of
    /// `constraints` (`PhysicalDelete` with `VerifyDeleteConstraints`).
    pub fn delete_with_constraints(
        self: &Arc<Self>,
        transaction: &Transaction,
        row_ids: &[i64],
        constraints: &TableConstraints,
    ) -> Result<u64> {
        let has_delete_constraints = constraints
            .foreign_keys
            .iter()
            .any(|fk| fk.kind == ForeignKeyKind::Delete);
        let storage = if self.has_unique_indexes() {
            let storage = transaction.local_storage().get_or_create(self)?;
            self.append_to_delete_indexes(&storage, row_ids)?;
            Some(storage)
        } else {
            transaction.local_storage().storage(self)
        };
        let all_columns: Vec<StorageIndex> = (0..self.columns.len()).map(StorageIndex::from).collect();
        let mut deleted = 0;
        let mut position = 0;
        while position < row_ids.len() {
            let start = position;
            let local = row_ids[start] as u64 >= MAX_ROW_ID;
            while position < row_ids.len() && (row_ids[position] as u64 >= MAX_ROW_ID) == local {
                position += 1;
            }
            let ids = &row_ids[start..position];
            if has_delete_constraints {
                let mut rows = DataChunk::default();
                self.fetch(transaction, &mut rows, &all_columns, ids)?;
                Self::verify_delete_foreign_keys(
                    transaction.local_storage(),
                    storage.as_deref(),
                    constraints,
                    &rows,
                )?;
            }
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

    /// Updates the sub-column at `column_path` of the rows `row_ids` to `values`
    /// without verifying constraints (`DataTable::UpdateColumn`, used by WAL replay).
    pub fn update_column(
        self: &Arc<Self>,
        transaction: &Transaction,
        row_ids: &[i64],
        column_path: &[usize],
        values: &Vector,
    ) -> Result<()> {
        if row_ids.is_empty() {
            return Ok(());
        }
        self.check_main_table("update")?;
        if row_ids
            .iter()
            .any(|&id| id < 0 || id as u64 >= self.row_groups.total_rows())
        {
            return Err(Error::internal(
                "Corrupt WAL: update of a row that does not exist",
            ));
        }
        let records = self
            .row_groups
            .update_column(transaction.data(), row_ids, column_path, values)?;
        transaction.push_updates(self, records);
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
                return Err(self.not_null_error(column, &constraints.column_names));
            }
        }
        if let Some(verifier) = &constraints.verifier {
            verifier.verify_update(chunk, column_ids)?;
        }
        Ok(())
    }
}

impl DataTable {
    /// A new version of `parent` (sharing its info) with `columns` stored in `row_groups`.
    fn derived(
        parent: &Arc<Self>,
        columns: Vec<ColumnDefinition>,
        row_groups: Arc<RowGroupCollection>,
    ) -> Arc<Self> {
        Arc::new(Self {
            info: Arc::clone(&parent.info),
            columns,
            row_groups,
            append_lock: Mutex::new(()),
            version: AtomicU8::new(TableVersion::MainTable as u8),
        })
    }

    /// `ALTER TABLE ADD COLUMN`: a new version of the table with `column` appended,
    /// filled by `default` (given a row count, returns that many values). This table is
    /// marked as altered (`DataTable(parent, new_column, default_value)`).
    pub fn add_column(
        self: &Arc<Self>,
        transaction: &Transaction,
        column: &ColumnDefinition,
        default: &dyn Fn(usize) -> Result<Vector>,
    ) -> Result<Arc<Self>> {
        let _lock = self.append_lock();
        let row_groups = Arc::new(self.row_groups.add_column(&column.ty, default)?);
        let mut columns = self.columns.clone();
        columns.push(column.clone());
        let table = Self::derived(self, columns, row_groups);
        transaction.local_storage().alter(self, &table, |collection| {
            collection.add_column(&column.ty, default)
        })?;
        self.set_as_altered();
        Ok(table)
    }

    /// `ALTER TABLE DROP COLUMN` of physical column `column` (`DataTable(parent,
    /// removed_column)`).
    pub fn remove_column(self: &Arc<Self>, transaction: &Transaction, column: usize) -> Result<Arc<Self>> {
        let _lock = self.append_lock();
        self.verify_remove_column(column)?;
        let row_groups = Arc::new(self.row_groups.remove_column(column)?);
        let mut columns = self.columns.clone();
        columns.remove(column);
        let table = Self::derived(self, columns, row_groups);
        transaction
            .local_storage()
            .alter(self, &table, |collection| collection.remove_column(column))?;
        self.set_as_altered();
        Ok(table)
    }

    /// Fails if physical column `column` cannot be dropped because an index reads it or
    /// a column after it.
    pub fn verify_remove_column(&self, column: usize) -> Result<()> {
        if column >= self.columns.len() {
            return Err(Error::internal("dropping a column that does not exist"));
        }
        for entry in self.indexes().entries() {
            for &indexed in entry.column_ids() {
                if indexed == column {
                    return Err(Error::catalog("Cannot drop this column: an index depends on it!"));
                }
                if indexed > column {
                    return Err(Error::catalog(
                        "Cannot drop this column: an index depends on a column after it!",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Fails if the type of physical column `column` cannot change because an index
    /// reads it.
    pub fn verify_alter_type(&self, column: usize) -> Result<()> {
        if self
            .indexes()
            .entries()
            .iter()
            .any(|entry| entry.column_ids().contains(&column))
        {
            return Err(Error::catalog(
                "Cannot change the type of this column: an index depends on it!",
            ));
        }
        Ok(())
    }

    /// `ALTER TABLE ALTER COLUMN TYPE`: column `column` becomes `target`, computed by
    /// `cast` from the scanned `bound_columns` (`DataTable(parent, changed_idx, ...)`).
    pub fn alter_type(
        self: &Arc<Self>,
        transaction: &Transaction,
        column: usize,
        target: &LogicalType,
        bound_columns: &[StorageIndex],
        cast: &dyn Fn(&DataChunk) -> Result<Vector>,
    ) -> Result<Arc<Self>> {
        let _lock = self.append_lock();
        self.verify_alter_type(column)?;
        let row_groups =
            Arc::new(
                self.row_groups
                    .alter_type(transaction.data(), column, target, bound_columns, cast)?,
            );
        let mut columns = self.columns.clone();
        columns[column].ty = target.clone();
        let table = Self::derived(self, columns, row_groups);
        transaction.local_storage().alter(self, &table, |collection| {
            collection.alter_type(TransactionData::local(), column, target, bound_columns, cast)
        })?;
        self.set_as_altered();
        Ok(table)
    }

    /// `ALTER TABLE ADD PRIMARY KEY`: the new version of the table, whose index the
    /// caller adds (`DataTable(parent, constraint)` with a UNIQUE constraint, which is
    /// verified by building the index).
    pub fn add_unique_constraint(self: &Arc<Self>, transaction: &Transaction) -> Arc<Self> {
        let _lock = self.append_lock();
        let table = Self::derived(self, self.columns.clone(), Arc::clone(&self.row_groups));
        transaction.local_storage().move_storage(self, &table);
        self.set_as_altered();
        table
    }

    /// `ALTER TABLE ALTER COLUMN SET NOT NULL`: verifies that column `column` holds no
    /// NULLs and returns the new version of the table (`DataTable(parent, constraint)`).
    pub fn add_not_null_constraint(
        self: &Arc<Self>,
        transaction: &Transaction,
        column: usize,
    ) -> Result<Arc<Self>> {
        self.add_not_null_constraint_named(transaction, column, &[])
    }

    /// [`DataTable::add_not_null_constraint`] with the catalog names of the columns for
    /// the error message.
    pub fn add_not_null_constraint_named(
        self: &Arc<Self>,
        transaction: &Transaction,
        column: usize,
        names: &[String],
    ) -> Result<Arc<Self>> {
        let _lock = self.append_lock();
        let local = transaction.local_storage().storage(self);
        let local_has_null = match &local {
            Some(storage) => storage.collection().column_has_null(transaction.data(), column)?,
            None => false,
        };
        if local_has_null || self.row_groups.column_has_null(transaction.data(), column)? {
            return Err(self.not_null_error(column, names));
        }
        let table = Self::derived(self, self.columns.clone(), Arc::clone(&self.row_groups));
        transaction.local_storage().move_storage(self, &table);
        self.set_as_altered();
        Ok(table)
    }
}

/// Whether any of the first `count` rows of `vector` is NULL (`VectorOperations::HasNull`).
pub(crate) fn has_null(vector: &Vector, count: usize) -> bool {
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
