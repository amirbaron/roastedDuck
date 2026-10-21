//! Scan states of tables and row group collections (DuckDB's `scan_state.hpp`).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::adaptive_filter::AdaptiveFilter;
use super::column_data::ColumnScanState;
use super::row_group::RowGroup;
use super::row_group_collection::RowGroupSegmentTree;
use super::segment_tree::SegmentNode;
use crate::COLUMN_IDENTIFIER_ROW_ID;
use crate::table_filter::{TableFilter, TableFilterSet};

/// A column to scan, possibly a field path into a STRUCT (DuckDB's `StorageIndex`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StorageIndex {
    /// The column's physical index in the table, or [`COLUMN_IDENTIFIER_ROW_ID`].
    pub index: u64,
    /// Struct fields to scan (all when empty).
    pub children: Vec<StorageIndex>,
}

impl StorageIndex {
    pub const fn new(index: u64) -> Self {
        Self {
            index,
            children: Vec::new(),
        }
    }

    /// The `rowid` pseudo column.
    pub const fn row_id() -> Self {
        Self::new(COLUMN_IDENTIFIER_ROW_ID)
    }

    pub const fn is_row_id(&self) -> bool {
        self.index == COLUMN_IDENTIFIER_ROW_ID
    }

    /// The physical column index as a `usize`.
    pub fn primary_index(&self) -> usize {
        usize::try_from(self.index).unwrap_or(usize::MAX)
    }
}

impl From<usize> for StorageIndex {
    fn from(index: usize) -> Self {
        Self::new(index as u64)
    }
}

/// A filter of a scan bound to the scanned columns (DuckDB's `ScanFilter`).
#[derive(Debug)]
pub struct ScanFilter {
    /// Position of the filtered column in the scan's column list.
    pub scan_column_index: usize,
    /// The filtered column in the table.
    pub table_column_index: StorageIndex,
    pub filter: TableFilter,
    /// Set when statistics prove the filter true for the current row group.
    pub always_true: bool,
}

/// The filters of a scan, which of them are still evaluated and in which order (DuckDB's
/// `ScanFilterInfo`).
#[derive(Debug)]
pub struct ScanFilterInfo {
    filters: Vec<ScanFilter>,
    /// Per scanned column: whether it has a filter that is still evaluated.
    column_has_filter: Vec<bool>,
    /// Per scanned column: whether it has a filter at all.
    base_column_has_filter: Vec<bool>,
    always_true_filters: usize,
    /// Per scanned column: whether only its filter needs it, not the scan's output
    /// (DuckDB's columns outside `projection_ids`).
    filter_only: Vec<bool>,
    /// The order in which the filters are evaluated.
    adaptive_filter: AdaptiveFilter,
}

impl Default for ScanFilterInfo {
    fn default() -> Self {
        Self::new(None, &[])
    }
}

impl ScanFilterInfo {
    /// Binds `filters` (keyed by scan column) to `column_ids`.
    pub fn new(filters: Option<&TableFilterSet>, column_ids: &[StorageIndex]) -> Self {
        let mut info = Self {
            filters: Vec::new(),
            column_has_filter: vec![false; column_ids.len()],
            base_column_has_filter: vec![false; column_ids.len()],
            always_true_filters: 0,
            filter_only: Vec::new(),
            adaptive_filter: AdaptiveFilter::new(std::iter::empty()),
        };
        for (&scan_column_index, filter) in filters.map(|set| &set.filters).into_iter().flatten() {
            let Some(table_column_index) = column_ids.get(scan_column_index) else {
                continue;
            };
            info.filters.push(ScanFilter {
                scan_column_index,
                table_column_index: table_column_index.clone(),
                filter: filter.clone(),
                always_true: false,
            });
            info.column_has_filter[scan_column_index] = true;
            info.base_column_has_filter[scan_column_index] = true;
        }
        info.adaptive_filter = AdaptiveFilter::new(info.filters.iter().map(|filter| &filter.filter));
        info
    }

    /// The filters in the order to evaluate them (`GetAdaptiveFilter()->permutation`).
    pub fn permutation(&self) -> &[usize] {
        &self.adaptive_filter.permutation
    }

    /// Starts measuring the filtering of a vector (`BeginFilter`).
    pub fn begin_filter(&self) -> Option<Instant> {
        self.adaptive_filter.begin_filter()
    }

    /// Ends measuring the filtering of a vector, adapting the order of the filters
    /// (`EndFilter`).
    pub fn end_filter(&mut self, start: Option<Instant>) {
        self.adaptive_filter.end_filter(start);
    }

    pub fn filters(&self) -> &[ScanFilter] {
        &self.filters
    }

    /// Marks the scanned columns outside `projection` (positions into the column list;
    /// empty = every column is output) as needed by their filters only.
    pub fn set_projection(&mut self, projection: &[usize]) {
        self.filter_only = (0..self.base_column_has_filter.len())
            .map(|column| !projection.is_empty() && !projection.contains(&column))
            .collect();
    }

    /// Whether scan column `column` is only scanned for its filter: its values need not
    /// be produced.
    pub fn is_filter_only(&self, column: usize) -> bool {
        self.filter_only.get(column).copied().unwrap_or(false)
    }

    /// Whether any filter must still be evaluated.
    pub fn has_filters(&self) -> bool {
        self.always_true_filters < self.filters.len()
    }

    /// Whether scan column `column` has a filter that is evaluated.
    pub fn column_has_filters(&self, column: usize) -> bool {
        self.column_has_filter.get(column).copied().unwrap_or(false)
    }

    /// Re-enables every filter (a new row group starts) (`CheckAllFilters`).
    pub fn check_all_filters(&mut self) {
        self.always_true_filters = 0;
        self.column_has_filter.clone_from(&self.base_column_has_filter);
        for filter in &mut self.filters {
            filter.always_true = false;
        }
    }

    /// Stops evaluating filter `index` for the current row group (`SetFilterAlwaysTrue`).
    pub fn set_filter_always_true(&mut self, index: usize) {
        let Some(filter) = self.filters.get_mut(index) else {
            return;
        };
        if filter.always_true {
            return;
        }
        filter.always_true = true;
        self.always_true_filters += 1;
        let column = filter.scan_column_index;
        let still_filtered = self
            .filters
            .iter()
            .any(|other| other.scan_column_index == column && !other.always_true);
        self.column_has_filter[column] = still_filtered;
    }
}

/// A scan of a row group collection (DuckDB's `CollectionScanState`).
#[derive(Debug, Default)]
pub struct CollectionScanState {
    /// The row group being scanned and its position in the collection.
    pub(crate) row_group: Option<(usize, SegmentNode<RowGroup>)>,
    /// Vector of the row group the scan is at.
    pub(crate) vector_index: usize,
    /// Rows of the row group to scan.
    pub(crate) max_row_group_row: u64,
    pub(crate) column_scans: Vec<ColumnScanState>,
    pub(crate) row_groups: Option<Arc<RowGroupSegmentTree>>,
    /// One past the last row (of the whole collection) to scan.
    pub(crate) max_row: u64,
    pub(crate) batch_index: u64,
    pub(crate) valid_sel: Vec<u32>,
    /// Scratch for the rows that pass the filters of the current vector.
    pub(crate) filter_sel: Vec<u32>,
    /// Rows considered before filters were applied.
    pub rows_scanned: u64,
    /// SYSTEM sampling pushed into the scan (DuckDB's `ScanSamplingInfo` with the
    /// state's `random`): the vectors to skip.
    pub sampling: Option<ScanSampling>,
}

/// A source of random numbers in `[0, 1)` for scan sampling (DuckDB's `RandomEngine`,
/// which lives with the functions).
pub trait ScanRandom: std::fmt::Debug + Send + Sync {
    /// The next random number in `[0, 1)` (`RandomEngine::NextRandom`).
    fn next_random(&mut self) -> f64;
}

/// SYSTEM sampling of a scan: each vector is kept with probability `sample_rate`
/// (DuckDB's `ScanSamplingInfo`).
#[derive(Debug)]
pub struct ScanSampling {
    pub sample_rate: f64,
    pub random: Box<dyn ScanRandom>,
}

/// Options of a table scan (DuckDB's `TableScanOptions`).
#[derive(Clone, Copy, Debug, Default)]
pub struct TableScanOptions {
    /// Fetch rows one at a time instead of scanning vectors (a verification mode).
    pub force_fetch_row: bool,
}

/// A scan of a table: its committed data and the transaction-local data (DuckDB's
/// `TableScanState`).
#[derive(Debug, Default)]
pub struct TableScanState {
    pub table_state: CollectionScanState,
    pub local_state: CollectionScanState,
    pub options: TableScanOptions,
    pub(crate) filters: ScanFilterInfo,
    pub(crate) column_ids: Vec<StorageIndex>,
}

impl TableScanState {
    /// Sets the scanned columns and filters (`TableScanState::Initialize`).
    pub fn initialize(&mut self, column_ids: Vec<StorageIndex>, filters: Option<&TableFilterSet>) {
        self.filters = ScanFilterInfo::new(filters, &column_ids);
        self.column_ids = column_ids;
    }

    /// See [`ScanFilterInfo::set_projection`].
    pub fn set_projection(&mut self, projection: &[usize]) {
        self.filters.set_projection(projection);
    }

    pub fn column_ids(&self) -> &[StorageIndex] {
        &self.column_ids
    }

    /// The batch index of the morsel being scanned: row groups of the table count from
    /// 1, the transaction-local row groups continue after them
    /// (`TableScanGetPartitionData`).
    pub fn batch_index(&self) -> u64 {
        if self.table_state.row_group.is_some() {
            self.table_state.batch_index
        } else if self.local_state.row_group.is_some() {
            self.table_state.batch_index + self.local_state.batch_index
        } else {
            0
        }
    }
}

/// The shared position of a parallel scan of a collection (DuckDB's
/// `ParallelCollectionScanState`).
#[derive(Debug, Default)]
pub struct ParallelCollectionScanState {
    pub(crate) inner: Mutex<ParallelScanPosition>,
}

impl ParallelCollectionScanState {
    /// The rows of the row groups handed out so far (`processed_rows`), which a table
    /// scan reports its progress with.
    pub fn processed_rows(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .processed_rows
    }
}

#[derive(Debug, Default)]
pub(crate) struct ParallelScanPosition {
    pub row_groups: Option<Arc<RowGroupSegmentTree>>,
    /// The positions of the row groups in the order to hand them out, when the scan reads
    /// them by a column's statistics (DuckDB's `reorderer`).
    pub order: Option<Vec<usize>>,
    /// The next row group to hand out: its position, or its index in `order`.
    pub next_row_group: usize,
    pub max_row: u64,
    pub batch_index: u64,
    pub processed_rows: u64,
}

/// The shared state of a parallel table scan (DuckDB's `ParallelTableScanState`).
#[derive(Debug, Default)]
pub struct ParallelTableScanState {
    pub scan_state: ParallelCollectionScanState,
    pub local_state: ParallelCollectionScanState,
}
