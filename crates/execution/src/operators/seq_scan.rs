use crate::operator::set_estimated_cardinality;
use rustdb_common::tree_renderer::RenderParams;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustdb_common::{Result, STANDARD_VECTOR_SIZE};
use rustdb_function::random::RandomEngine;
use rustdb_storage::table::{ScanRandom, ScanSampling};
use rustdb_storage::table_filter::DynamicTableFilterSet;
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
    /// The filters operators set while the query runs (DuckDB's `dynamic_filters`),
    /// applied with `filters` by scans starting after they were set.
    dynamic_filters: Option<Arc<DynamicTableFilterSet>>,
    /// Positions (into `column_ids`) of the output columns; empty = all, in order
    /// (DuckDB's `projection_ids`).
    projection: Vec<usize>,
    /// `index_scan_max_count` and `index_scan_percentage`; `None` disables index scans.
    index_scan: Option<(u64, f64)>,
    /// What `EXPLAIN` shows about the scan.
    explain: ScanExplainInfo,
    /// SYSTEM sampling pushed into the scan: the fraction of vectors to keep and the
    /// `REPEATABLE` seed (DuckDB's `extra_info.sample_options`).
    sampling: Option<(f64, Option<u64>)>,
}

/// [`RandomEngine`] as the storage's sampling random source.
#[derive(Debug)]
struct SamplingRandom(RandomEngine);

impl ScanRandom for SamplingRandom {
    fn next_random(&mut self) -> f64 {
        self.0.next_random()
    }
}

/// The names `EXPLAIN` shows for a table scan (the bind data and `names` DuckDB's
/// `PhysicalTableScan` keeps).
#[derive(Clone, Debug, Default)]
pub struct ScanExplainInfo {
    /// The qualified table name (`TableScanToString`); empty for table functions.
    pub table: String,
    /// The name of each scanned column, by position in the scan's column list.
    pub column_names: Vec<String>,
    /// Positions of virtual columns (`rowid`), which DuckDB does not list as projections
    /// (`AddProjectionNames` skips ids past the table's columns).
    pub virtual_columns: Vec<usize>,
    /// The pushed-down sampling (`System: 10%`).
    pub sample_method: Option<String>,
    /// `File Filters` / `Scanning Files`.
    pub file_filters: Vec<(String, String)>,
}

impl ScanExplainInfo {
    /// DuckDB's `PhysicalTableScan::ParamsToString` for a scan with projection and filter
    /// pushdown.
    pub fn params(
        &self,
        projection: &[usize],
        filters: Option<&TableFilterSet>,
        dynamic_filters: Option<&TableFilterSet>,
        estimated_cardinality: u64,
    ) -> RenderParams {
        let mut result = RenderParams::new();
        if !self.table.is_empty() {
            result.insert("Table".to_owned(), self.table.clone());
            result.insert("Type".to_owned(), "Sequential Scan".to_owned());
        }
        let projected_name = |index: usize| {
            (!self.virtual_columns.contains(&index))
                .then(|| self.column_names.get(index).map(String::as_str))
                .flatten()
        };
        let projected: Vec<&str> = if projection.is_empty() {
            (0..self.column_names.len()).filter_map(projected_name).collect()
        } else {
            projection
                .iter()
                .filter_map(|&index| projected_name(index))
                .collect()
        };
        result.insert("Projections".to_owned(), projected.join("\n"));
        // DuckDB's `GetFilterInfo`.
        let filter_info = |filters: &TableFilterSet| {
            filters
                .filters
                .iter()
                .filter_map(|(&column, filter)| self.column_names.get(column).map(|name| filter.to_sql(name)))
                .collect::<Vec<String>>()
                .join("\n")
        };
        if let Some(filters) = filters {
            result.insert("Filters".to_owned(), filter_info(filters));
        }
        if let Some(dynamic_filters) = dynamic_filters {
            result.insert("Dynamic Filters".to_owned(), filter_info(dynamic_filters));
        }
        if let Some(sample) = &self.sample_method {
            result.insert("Sample Method".to_owned(), sample.clone());
        }
        for (key, value) in &self.file_filters {
            result.insert(key.clone(), value.clone());
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
            dynamic_filters: None,
            filters,
            projection,
            index_scan: Some((DEFAULT_INDEX_SCAN_MAX_COUNT, DEFAULT_INDEX_SCAN_PERCENTAGE)),
            explain: ScanExplainInfo::default(),
            sampling: None,
        }
    }

    /// Sets the run-time filters operators above set (DuckDB's `dynamic_filters`), keyed
    /// by position in the scanned columns.
    #[must_use]
    pub fn with_dynamic_filters(mut self, dynamic: Option<Arc<DynamicTableFilterSet>>) -> Self {
        self.dynamic_filters = dynamic;
        self
    }

    /// The filters a scan starting now applies: `filters` with the run-time filters set
    /// so far (`TableScanGlobalSourceState`'s `GetFinalTableFilters`).
    fn scan_filters(&self) -> Option<TableFilterSet> {
        match &self.dynamic_filters {
            Some(dynamic) if dynamic.has_filters() => dynamic.final_table_filters(self.filters.as_ref()),
            _ => self.filters.clone(),
        }
    }

    /// Sets what `EXPLAIN` shows about the scan.
    #[must_use]
    pub fn with_explain_info(mut self, explain: ScanExplainInfo) -> Self {
        self.explain = explain;
        self
    }

    /// Pushes SYSTEM sampling into the scan: each vector is kept with probability
    /// `sample_rate`, drawn from an engine seeded with `seed` if given.
    #[must_use]
    pub fn with_sampling(mut self, sample_rate: f64, seed: Option<u64>) -> Self {
        self.sampling = Some((sample_rate, seed));
        self
    }

    /// Sets the index scan thresholds (`index_scan_max_count`, `index_scan_percentage`),
    /// or disables index scans with `None`.
    #[must_use]
    pub fn with_index_scan(mut self, thresholds: Option<(u64, f64)>) -> Self {
        self.index_scan = thresholds;
        self
    }

    /// The row ids an index scan applying `filters` yields, if one applies
    /// (`TableScanInitGlobal`).
    fn index_scan_row_ids(&self, filters: Option<&TableFilterSet>) -> Result<Option<Vec<i64>>> {
        let (Some((max_count, percentage)), Some(filters)) = (self.index_scan, filters) else {
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
    fn parallel_source(&self) -> bool {
        true
    }
    fn supports_batch_index(&self) -> bool {
        true
    }

    fn explain_name(&self) -> String {
        "SEQ_SCAN".to_owned()
    }
    fn profiler_name(&self) -> String {
        "TABLE_SCAN".to_owned()
    }
    fn params_to_string(&self, estimated_cardinality: u64) -> RenderParams {
        let dynamic = self
            .dynamic_filters
            .as_ref()
            .filter(|dynamic| dynamic.has_filters())
            .and_then(|dynamic| dynamic.final_table_filters(None));
        self.explain.params(
            &self.projection,
            self.filters.as_ref(),
            dynamic.as_ref(),
            estimated_cardinality,
        )
    }
}

/// The shared state of a scan: the parallel table scan, and for an index scan the row
/// ids still to fetch.
#[derive(Debug)]
pub struct SeqScanGlobal {
    parallel: ParallelTableScanState,
    index_rows: Option<Vec<i64>>,
    next_index_row: AtomicUsize,
    /// The filters the scan applies, with the run-time filters set when it started.
    filters: Option<TableFilterSet>,
}

/// A thread's scan: its storage scan state and, with a projection, the scanned chunk.
#[derive(Debug)]
pub struct SeqScanLocal {
    state: TableScanState,
    has_morsel: bool,
    scan_chunk: DataChunk,
    /// The vector of indexed rows last fetched, while the scan is in its index phase.
    index_batch: Option<u64>,
}

impl Source for PhysicalSeqScan {
    type Global = SeqScanGlobal;
    type Local = SeqScanLocal;

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<Self::Global> {
        let filters = self.scan_filters();
        if let Some(row_ids) = self.index_scan_row_ids(filters.as_ref())? {
            // Fetch the indexed rows, then scan only the transaction-local rows.
            return Ok(SeqScanGlobal {
                parallel: self.table.initialize_local_parallel_scan(&self.transaction),
                index_rows: Some(row_ids),
                next_index_row: AtomicUsize::new(0),
                filters,
            });
        }
        Ok(SeqScanGlobal {
            parallel: self.table.initialize_parallel_scan(&self.transaction),
            index_rows: None,
            next_index_row: AtomicUsize::new(0),
            filters,
        })
    }

    fn local_source_state(&self, _: &ExecutionContext, global: &Self::Global) -> Result<Self::Local> {
        let mut state = TableScanState::default();
        state.initialize(self.column_ids.clone(), global.filters.as_ref());
        if let Some((sample_rate, seed)) = self.sampling {
            // DuckDB seeds the committed data's engine only.
            let table_random = seed.map_or_else(RandomEngine::default, RandomEngine::new);
            state.table_state.sampling = Some(ScanSampling {
                sample_rate,
                random: Box::new(SamplingRandom(table_random)),
            });
            state.local_state.sampling = Some(ScanSampling {
                sample_rate,
                random: Box::new(SamplingRandom(RandomEngine::default())),
            });
        }
        Ok(SeqScanLocal {
            state,
            has_morsel: false,
            scan_chunk: DataChunk::default(),
            index_batch: None,
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
                local.index_batch = Some((start / STANDARD_VECTOR_SIZE) as u64);
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
        local.index_batch = None;
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

    fn max_threads(&self, _: &Self::Global) -> usize {
        self.table.max_threads()
    }

    fn batch_index(&self, global: &Self::Global, local: &Self::Local) -> Result<u64> {
        if let Some(batch) = local.index_batch {
            return Ok(batch);
        }
        // After an index scan, the local rows' batches follow the indexed rows' vectors.
        let index_batches = global
            .index_rows
            .as_ref()
            .map_or(0, |rows| rows.len().div_ceil(STANDARD_VECTOR_SIZE) as u64);
        Ok(index_batches + local.state.batch_index())
    }
}
