use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::tree_renderer::RenderParams;
use rustdb_common::{Error, Result};
use rustdb_function::{FunctionData, TableFunction, TableScanState};
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::operator::{ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Source, SourceResult};

/// Scans a table function (DuckDB's `PhysicalTableScan`): the function produces all of
/// its columns, of which `column_ids` are output.
pub struct PhysicalTableScan {
    types: Vec<LogicalType>,
    function: TableFunction,
    bind_data: Option<Arc<dyn FunctionData>>,
    column_ids: Vec<u64>,
    /// The positions in `column_ids` that are output; empty for all of them (columns
    /// read only for pushed-down filters are not output).
    projection: Vec<usize>,
    /// The filters the function applies (shown by `EXPLAIN`).
    filters: Option<rustdb_storage::TableFilterSet>,
    /// What `EXPLAIN` shows about the scan.
    explain: super::ScanExplainInfo,
}

impl std::fmt::Debug for PhysicalTableScan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhysicalTableScan")
            .field("function", &self.function.name)
            .field("column_ids", &self.column_ids)
            .finish_non_exhaustive()
    }
}

impl PhysicalTableScan {
    pub fn new(
        types: Vec<LogicalType>,
        function: TableFunction,
        bind_data: Option<Arc<dyn FunctionData>>,
        column_ids: Vec<u64>,
    ) -> Self {
        Self {
            types,
            function,
            bind_data,
            column_ids,
            projection: Vec::new(),
            filters: None,
            explain: super::ScanExplainInfo::default(),
        }
    }

    /// Outputs only the columns at `projection` (positions in the column ids).
    #[must_use]
    pub fn with_projection(mut self, projection: Vec<usize>) -> Self {
        self.projection = projection;
        self
    }

    /// Records the filters pushed into the function.
    #[must_use]
    pub fn with_filters(mut self, filters: Option<rustdb_storage::TableFilterSet>) -> Self {
        self.filters = filters;
        self
    }

    /// Sets what `EXPLAIN` shows about the scan.
    #[must_use]
    pub fn with_explain_info(mut self, explain: super::ScanExplainInfo) -> Self {
        self.explain = explain;
        self
    }
}

impl PhysicalOperator for PhysicalTableScan {
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
        self.function.parallel_scan.is_some()
    }
    fn supports_batch_index(&self) -> bool {
        self.function.parallel_scan.is_some()
    }

    fn explain_name(&self) -> String {
        self.function.name.to_ascii_uppercase()
    }
    fn profiler_name(&self) -> String {
        "TABLE_SCAN".to_owned()
    }
    fn params_to_string(&self, estimated_cardinality: u64) -> RenderParams {
        let mut result = RenderParams::new();
        result.insert("Function".to_owned(), self.function.name.to_ascii_uppercase());
        if self.function.projection_pushdown {
            // Virtual columns are not listed (`column_id >= names.size()`).
            let name = |index: usize| {
                (!self.explain.virtual_columns.contains(&index))
                    .then(|| self.explain.column_names.get(index).map(String::as_str))
                    .flatten()
            };
            let names: Vec<&str> = if self.projection.is_empty() {
                (0..self.explain.column_names.len()).filter_map(name).collect()
            } else {
                self.projection.iter().filter_map(|&index| name(index)).collect()
            };
            result.insert("Projections".to_owned(), names.join("\n"));
        }
        if let Some(filters) = &self.filters
            && self.function.filter_pushdown
        {
            let info: Vec<String> = filters
                .filters
                .iter()
                .filter_map(|(&column, filter)| {
                    self.explain
                        .column_names
                        .get(column)
                        .map(|name| filter.to_sql(name))
                })
                .collect();
            result.insert("Filters".to_owned(), info.join("\n"));
        }
        for (key, value) in &self.explain.file_filters {
            result.insert(key.clone(), value.clone());
        }
        crate::operator::set_estimated_cardinality(&mut result, estimated_cardinality);
        result
    }
}

/// The scan's cursor (one thread produces at a time), or the shared state of a parallel
/// scan.
#[derive(Default)]
pub struct TableScanGlobal {
    state: Mutex<(TableScanState, bool)>,
    parallel: Option<rustdb_function::ParallelScanGlobal>,
}

impl std::fmt::Debug for TableScanGlobal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TableScanGlobal")
            .field("parallel", &self.parallel.is_some())
            .finish_non_exhaustive()
    }
}

impl PhysicalTableScan {
    /// The output columns of a chunk the function produced: the scanned columns (with
    /// NULLs for virtual columns the function lacks), restricted to `projection`.
    fn project(&self, output: DataChunk) -> Result<(Vec<Vector>, usize)> {
        let count = output.len();
        let mut columns = output.into_columns();
        let mut projected = if self.function.projected_output {
            // One column per scanned column id already.
            columns
        } else {
            let mut projected = Vec::with_capacity(self.column_ids.len());
            for (position, &id) in self.column_ids.iter().enumerate() {
                // A virtual column (such as `rowid`) of a function without one is NULL
                // (DuckDB's `PlanGet` projects a constant for it).
                if let Some(column) = usize::try_from(id).ok().and_then(|index| columns.get_mut(index)) {
                    projected.push(column.clone());
                } else {
                    let ty = self.types.get(position).cloned().unwrap_or(LogicalType::BIGINT);
                    projected.push(Vector::constant_null(ty));
                }
            }
            projected
        };
        if !self.projection.is_empty() {
            let mut all: Vec<Option<Vector>> = projected.into_iter().map(Some).collect();
            projected = self
                .projection
                .iter()
                .map(|&index| {
                    all.get_mut(index)
                        .and_then(Option::take)
                        .ok_or_else(|| Error::internal("table scan projection out of range"))
                })
                .collect::<Result<Vec<_>>>()?;
        }
        Ok((projected, count))
    }
}

impl Source for PhysicalTableScan {
    type Global = TableScanGlobal;
    type Local = Option<rustdb_function::ParallelScanLocal>;

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<TableScanGlobal> {
        let parallel = match &self.function.parallel_scan {
            Some(parallel) => Some((parallel.init_global)(
                self.bind_data.as_deref(),
                &self.column_ids,
            )?),
            None => None,
        };
        Ok(TableScanGlobal {
            state: Mutex::default(),
            parallel,
        })
    }

    fn local_source_state(
        &self,
        _: &ExecutionContext,
        global: &TableScanGlobal,
    ) -> Result<Option<rustdb_function::ParallelScanLocal>> {
        match (&self.function.parallel_scan, &global.parallel) {
            (Some(parallel), Some(shared)) => Ok(Some((parallel.init_local)(shared)?)),
            _ => Ok(None),
        }
    }

    fn max_threads(&self, global: &TableScanGlobal) -> usize {
        match (&self.function.parallel_scan, &global.parallel) {
            (Some(parallel), Some(shared)) => (parallel.max_threads)(shared),
            _ => 1,
        }
    }

    fn batch_index(&self, global: &TableScanGlobal, local: &Self::Local) -> Result<u64> {
        match (&self.function.parallel_scan, &global.parallel, local) {
            (Some(parallel), Some(shared), Some(local)) => Ok((parallel.batch_index)(shared, local)),
            _ => Err(Error::internal("table function scan without batch indices")),
        }
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &TableScanGlobal,
        local: &mut Self::Local,
    ) -> Result<SourceResult> {
        if let (Some(parallel), Some(shared), Some(local)) =
            (&self.function.parallel_scan, &global.parallel, local)
        {
            let mut output = DataChunk::default();
            if !(parallel.scan)(
                self.bind_data.as_deref(),
                &self.column_ids,
                shared,
                local,
                &mut output,
            )? {
                chunk.set_columns(Vec::new(), 0);
                return Ok(SourceResult::Finished);
            }
            let (projected, count) = self.project(output)?;
            chunk.set_columns(projected, count);
            return Ok(SourceResult::HaveMoreOutput);
        }
        let Some(scan) = &self.function.scan else {
            return Err(Error::internal(format!(
                "table function {} cannot be scanned",
                self.function.name
            )));
        };
        let mut guard = global.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (state, finished) = &mut *guard;
        if *finished {
            chunk.set_columns(Vec::new(), 0);
            return Ok(SourceResult::Finished);
        }
        let mut output = DataChunk::default();
        let more = scan(self.bind_data.as_deref(), &self.column_ids, state, &mut output)?;
        *finished = !more;
        let (projected, count) = self.project(output)?;
        chunk.set_columns(projected, count);
        Ok(if more {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }
}
