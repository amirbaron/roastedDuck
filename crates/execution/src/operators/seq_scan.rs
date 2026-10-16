use crate::operator::set_estimated_cardinality;
use rustdb_common::tree_renderer::RenderParams;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustdb_common::{Result, STANDARD_VECTOR_SIZE};
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
///
/// With a single pushed-down filter on a column with an ART index, the scan looks the
/// matching row ids up in the index and fetches them, if fewer than
/// `max(index_scan_max_count, index_scan_percentage * rows)` match (DuckDB's index scan
/// in `TableScanInitGlobal`); the transaction's local rows are scanned as usual.
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
    /// `index_scan_max_count` and `index_scan_percentage`; `None` disables index scans.
    index_scan: Option<(u64, f64)>,
    /// What `EXPLAIN` shows about the scan.
    explain: ScanExplainInfo,
}

/// The names `EXPLAIN` shows for a table scan (the bind data and `names` DuckDB's
/// `PhysicalTableScan` keeps).
#[derive(Clone, Debug, Default)]
pub struct ScanExplainInfo {
    /// The qualified table name (`TableScanToString`); empty for table functions.
    pub table: String,
    /// The name of each scanned column, by position in the scan's column list.
    pub column_names: Vec<String>,
}

impl ScanExplainInfo {
    /// DuckDB's `PhysicalTableScan::ParamsToString` for a scan with projection and filter
    /// pushdown.
    pub fn params(
        &self,
        projection: &[usize],
        filters: Option<&TableFilterSet>,
        estimated_cardinality: u64,
    ) -> RenderParams {
        let mut result = RenderParams::new();
        if !self.table.is_empty() {
            result.insert("Table".to_owned(), self.table.clone());
            result.insert("Type".to_owned(), "Sequential Scan".to_owned());
        }
        let projected: Vec<&str> = if projection.is_empty() {
            self.column_names.iter().map(String::as_str).collect()
        } else {
            projection
                .iter()
                .filter_map(|&index| self.column_names.get(index).map(String::as_str))
                .collect()
        };
        result.insert("Projections".to_owned(), projected.join("\n"));
        if let Some(filters) = filters {
            let filters: Vec<String> = filters
                .filters
                .iter()
                .filter_map(|(&column, filter)| self.column_names.get(column).map(|name| filter.to_sql(name)))
                .collect();
            result.insert("Filters".to_owned(), filters.join("\n"));
        }
        set_estimated_cardinality(&mut result, estimated_cardinality);
        result
    }
}

/// DuckDB's default `index_scan_max_count`.
pub const DEFAULT_INDEX_SCAN_MAX_COUNT: u64 = 2048;
/// DuckDB's default `index_scan_percentage`.
pub const DEFAULT_INDEX_SCAN_PERCENTAGE: f64 = 0.001;

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
            index_scan: Some((DEFAULT_INDEX_SCAN_MAX_COUNT, DEFAULT_INDEX_SCAN_PERCENTAGE)),
            explain: ScanExplainInfo::default(),
        }
    }

    /// Sets what `EXPLAIN` shows about the scan.
    #[must_use]
    pub fn with_explain_info(mut self, explain: ScanExplainInfo) -> Self {
        self.explain = explain;
        self
    }

    /// Sets the index scan thresholds (`index_scan_max_count`, `index_scan_percentage`),
    /// or disables index scans with `None`.
    #[must_use]
    pub fn with_index_scan(mut self, thresholds: Option<(u64, f64)>) -> Self {
        self.index_scan = thresholds;
        self
    }

    /// The row ids an index scan yields, if one applies (`TableScanInitGlobal`).
    fn index_scan_row_ids(&self) -> Result<Option<Vec<i64>>> {
        let (Some((max_count, percentage)), Some(filters)) = (self.index_scan, &self.filters) else {
            return Ok(None);
        };
        if filters.filters.len() != 1 || !self.table.has_indexes() {
            return Ok(None);
        }
        let from_percentage = (self.table.total_rows() as f64 * percentage) as u64;
        let max_count = max_count.max(from_percentage);
        self.table.try_index_scan(
            &self.column_ids,
            filters,
            usize::try_from(max_count).unwrap_or(usize::MAX),
        )
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

    fn explain_name(&self) -> String {
        "SEQ_SCAN".to_owned()
    }
    fn params_to_string(&self, estimated_cardinality: u64) -> RenderParams {
        self.explain
            .params(&self.projection, self.filters.as_ref(), estimated_cardinality)
    }
}

/// The shared state of a scan: the parallel table scan, and for an index scan the row
/// ids still to fetch.
#[derive(Debug)]
pub struct SeqScanGlobal {
    parallel: ParallelTableScanState,
    index_rows: Option<Vec<i64>>,
    next_index_row: AtomicUsize,
}

/// A thread's scan: its storage scan state and, with a projection, the scanned chunk.
#[derive(Debug)]
pub struct SeqScanLocal {
    state: TableScanState,
    has_morsel: bool,
    scan_chunk: DataChunk,
}

impl Source for PhysicalSeqScan {
    type Global = SeqScanGlobal;
    type Local = SeqScanLocal;

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<Self::Global> {
        if let Some(row_ids) = self.index_scan_row_ids()? {
            // Fetch the indexed rows, then scan only the transaction-local rows.
            return Ok(SeqScanGlobal {
                parallel: self.table.initialize_local_parallel_scan(&self.transaction),
                index_rows: Some(row_ids),
                next_index_row: AtomicUsize::new(0),
            });
        }
        Ok(SeqScanGlobal {
            parallel: self.table.initialize_parallel_scan(&self.transaction),
            index_rows: None,
            next_index_row: AtomicUsize::new(0),
        })
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
        if let Some(row_ids) = &global.index_rows {
            loop {
                let start = global
                    .next_index_row
                    .fetch_add(STANDARD_VECTOR_SIZE, Ordering::Relaxed);
                if start >= row_ids.len() {
                    break;
                }
                let end = (start + STANDARD_VECTOR_SIZE).min(row_ids.len());
                let target = if self.projection.is_empty() {
                    &mut *chunk
                } else {
                    &mut local.scan_chunk
                };
                self.table
                    .fetch(&self.transaction, target, &self.column_ids, &row_ids[start..end])?;
                if target.is_empty() {
                    continue;
                }
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
            local.has_morsel =
                self.table
                    .next_parallel_scan(&self.transaction, &global.parallel, &mut local.state)?;
            if !local.has_morsel {
                chunk.set_len(0);
                return Ok(SourceResult::Finished);
            }
        }
    }
}
