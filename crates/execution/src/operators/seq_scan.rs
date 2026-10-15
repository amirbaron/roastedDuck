use std::sync::Arc;

use rustdb_common::Result;
use rustdb_storage::{
    DataTable, ParallelTableScanState, StorageIndex, TableFilterSet, TableScanState, Transaction,
};
use rustdb_types::{DataChunk, LogicalType};

use crate::operator::{ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Source, SourceResult};

/// Scans a table's storage in parallel, one row group per morsel (DuckDB's
/// `PhysicalTableScan` running the `seq_scan` table function, `TableScanFunction`).
///
/// Rows are those visible to `transaction`, including its uncommitted inserts. Filters
/// are pushed into the storage scan; columns scanned only for filtering are dropped
/// through `projection`.
#[derive(Debug)]
pub struct PhysicalSeqScan {
    types: Vec<LogicalType>,
    table: Arc<DataTable>,
    transaction: Arc<Transaction>,
    column_ids: Vec<StorageIndex>,
    filters: Option<TableFilterSet>,
    /// Positions (into `column_ids`) of the output columns; empty = all, in order
    /// (DuckDB's `projection_ids`).
    projection: Vec<usize>,
}

impl PhysicalSeqScan {
    /// A scan of `column_ids` of `table` producing `types`, which are the types of the
    /// projected columns.
    pub fn new(
        types: Vec<LogicalType>,
        table: Arc<DataTable>,
        transaction: Arc<Transaction>,
        column_ids: Vec<StorageIndex>,
        filters: Option<TableFilterSet>,
        projection: Vec<usize>,
    ) -> Self {
        Self {
            types,
            table,
            transaction,
            column_ids,
            filters,
            projection,
        }
    }

    pub fn table(&self) -> &Arc<DataTable> {
        &self.table
    }

    pub fn column_ids(&self) -> &[StorageIndex] {
        &self.column_ids
    }

    pub fn filters(&self) -> Option<&TableFilterSet> {
        self.filters.as_ref()
    }
}

impl PhysicalOperator for PhysicalSeqScan {
    fn name(&self) -> &'static str {
        "TABLE_SCAN"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &[]
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
}

/// A thread's scan: its storage scan state and, with a projection, the scanned chunk.
#[derive(Debug)]
pub struct SeqScanLocal {
    state: TableScanState,
    has_morsel: bool,
    scan_chunk: DataChunk,
}

impl Source for PhysicalSeqScan {
    type Global = ParallelTableScanState;
    type Local = SeqScanLocal;

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<Self::Global> {
        Ok(self.table.initialize_parallel_scan(&self.transaction))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<Self::Local> {
        let mut state = TableScanState::default();
        state.initialize(self.column_ids.clone(), self.filters.as_ref());
        Ok(SeqScanLocal {
            state,
            has_morsel: false,
            scan_chunk: DataChunk::default(),
        })
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<SourceResult> {
        loop {
            if local.has_morsel {
                let target = if self.projection.is_empty() {
                    &mut *chunk
                } else {
                    &mut local.scan_chunk
                };
                if self
                    .table
                    .scan_morsel(&self.transaction, target, &mut local.state)?
                {
                    if !self.projection.is_empty() {
                        let columns = self
                            .projection
                            .iter()
                            .map(|&index| local.scan_chunk.column(index).clone())
                            .collect();
                        chunk.set_columns(columns, local.scan_chunk.len());
                    }
                    return Ok(SourceResult::HaveMoreOutput);
                }
            }
            local.has_morsel = self
                .table
                .next_parallel_scan(&self.transaction, global, &mut local.state)?;
            if !local.has_morsel {
                chunk.set_len(0);
                return Ok(SourceResult::Finished);
            }
        }
    }
}
