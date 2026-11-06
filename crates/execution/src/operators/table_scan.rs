use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::tree_renderer::RenderParams;
use rustdb_common::{Error, Result};
use rustdb_function::{FunctionData, TableFunction, TablePartitionInfo, TableScanState};
use rustdb_storage::table_filter::DynamicTableFilterSet;
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::operator::{
    ErasedSource, OperatorPartitionData, OperatorPartitionInfo, PhysicalOperator, PhysicalRef, SinkState,
};
use crate::{ExecutionContext, ScanMetrics, Source, SourceResult};

/// Scans a table function (DuckDB's `PhysicalTableScan`): the function produces all of
/// its columns, of which `column_ids` are output.
pub struct PhysicalTableScan {
    types: Vec<LogicalType>,
    function: TableFunction,
    bind_data: Option<Arc<dyn FunctionData>>,
    column_ids: Vec<u64>,
    /// The scanned columns with the fields read of nested ones (DuckDB's
    /// `column_indexes`).
    column_indexes: Vec<rustdb_types::ColumnIndex>,
    /// The positions in `column_ids` that are output; empty for all of them (columns
    /// read only for pushed-down filters are not output).
    projection: Vec<usize>,
    /// The filters the function applies (shown by `EXPLAIN`).
    filters: Option<rustdb_storage::TableFilterSet>,
    /// Join filters populated before this source opens (keyed by scan position).
    dynamic_filters: Option<Arc<DynamicTableFilterSet>>,
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
            column_indexes: column_ids
                .iter()
                .map(|&id| rustdb_types::ColumnIndex::new(id))
                .collect(),
            column_ids,
            projection: Vec::new(),
            filters: None,
            dynamic_filters: None,
            explain: super::ScanExplainInfo::default(),
        }
    }

    /// How the function partitions its data on the output columns `partition_columns`
    /// (the table scan part of DuckDB's `CanUsePartitionedAggregate`). With projection
    /// pushdown, the columns become positions in the column ids, which is how the scan
    /// reports their values; `None` when one is not scanned.
    pub fn partition_info(&self, partition_columns: &mut [u64]) -> Option<TablePartitionInfo> {
        let get_partition_info = self.function.get_partition_info.as_ref()?;
        // Non-pushdown scans output every base column in order.
        let base_columns = if self.function.projection_pushdown {
            if !self.projection.is_empty() {
                for column in partition_columns.iter_mut() {
                    *column = *self.projection.get(*column as usize)? as u64;
                }
            }
            partition_columns
                .iter()
                .map(|&column| self.column_ids.get(column as usize).copied())
                .collect::<Option<Vec<_>>>()?
        } else {
            partition_columns.to_vec()
        };
        Some(get_partition_info(self.bind_data.as_deref(), &base_columns))
    }

    /// The scanned columns with the fields read of nested ones.
    #[must_use]
    pub fn with_column_indexes(mut self, column_indexes: Vec<rustdb_types::ColumnIndex>) -> Self {
        self.column_indexes = column_indexes;
        self
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

    /// Shares run-time join filters with this function scan.
    #[must_use]
    pub fn with_dynamic_filters(mut self, filters: Option<Arc<DynamicTableFilterSet>>) -> Self {
        self.dynamic_filters = filters;
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
    fn preallocate_output(&self) -> bool {
        false
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
    fn parallel_source(&self) -> bool {
        self.function.parallel_scan.is_some()
    }
    fn supports_partitioning(&self, partition_info: &OperatorPartitionInfo) -> bool {
        self.function.parallel_scan.as_ref().is_some_and(|parallel| {
            !partition_info.requires_partition_columns() || parallel.partition_data.is_some()
        })
    }
    fn table_filters(&self) -> Option<&rustdb_storage::TableFilterSet> {
        self.filters.as_ref().filter(|filters| !filters.is_empty())
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
                    .then(|| self.explain.projection_name(index))
                    .flatten()
            };
            let names: Vec<&[u8]> = if self.projection.is_empty() {
                (0..self.explain.column_names.len()).filter_map(name).collect()
            } else {
                self.projection.iter().filter_map(|&index| name(index)).collect()
            };
            result.insert(
                "Projections".to_owned(),
                rustdb_common::ByteString::join(&names, b"\n"),
            );
        }
        if self.function.filter_pushdown {
            let mut add_filters = |label: &str, filters: &rustdb_storage::TableFilterSet| {
                let info: Vec<rustdb_common::ByteString> = filters
                    .filters
                    .iter()
                    .filter_map(|(&column, filter)| {
                        self.explain
                            .column_names
                            .get(column)
                            .map(|name| filter.to_sql(name))
                    })
                    .collect();
                result.insert(label.to_owned(), rustdb_common::ByteString::join(&info, b"\n"));
            };
            if let Some(filters) = &self.filters {
                add_filters("Filters", filters);
            }
            if let Some(dynamic) = &self.dynamic_filters
                && dynamic.has_filters()
                && let Some(filters) = dynamic.final_table_filters(None)
            {
                add_filters("Dynamic Filters", &filters);
            }
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
    /// Per-execution bind data containing the final static and dynamic filters.
    bind_data: Option<Arc<dyn FunctionData>>,
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

    fn global_source_state(&self, ctx: &ExecutionContext, _: Option<&SinkState>) -> Result<TableScanGlobal> {
        let mut bind_data = self.bind_data.clone();
        if let Some(dynamic) = &self.dynamic_filters
            && dynamic.has_filters()
            && let (Some(pushdown), Some(data), Some(filters)) = (
                &self.function.pushdown_filters,
                &bind_data,
                dynamic.final_table_filters(self.filters.as_ref()),
            )
        {
            bind_data = Some(pushdown(data.as_ref(), &self.column_ids, &filters)?);
        }
        let parallel = match &self.function.parallel_scan {
            Some(parallel) => Some((parallel.init_global)(
                bind_data.as_deref(),
                &self.column_ids,
                &self.column_indexes,
                ctx.operator_logger(self).as_ref(),
            )?),
            None => None,
        };
        Ok(TableScanGlobal {
            bind_data,
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

    fn extra_source_params(
        &self,
        global: &TableScanGlobal,
        _: &Self::Local,
    ) -> Vec<(rustdb_common::ByteString, rustdb_common::ByteString)> {
        match (&self.function.parallel_scan, &global.parallel) {
            (Some(parallel), Some(shared)) => parallel
                .dynamic_to_string
                .as_ref()
                .map(|dynamic| dynamic(global.bind_data.as_deref(), shared))
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// A function without scan metrics reports its estimated cardinality as the rows
    /// scanned once the thread exhausted it (`AddEstimatedTableScanRowsScanned`).
    fn scan_metrics(
        &self,
        global: &TableScanGlobal,
        local: &Self::Local,
        exhausted: bool,
    ) -> Option<ScanMetrics> {
        let rows_scanned = if exhausted {
            self.function
                .cardinality
                .as_ref()
                .and_then(|cardinality| cardinality(self.bind_data.as_deref()))
        } else {
            None
        };
        // `get_metrics`: functions that report the row groups they read.
        let row_groups = match (&self.function.parallel_scan, &global.parallel, local) {
            (Some(parallel), Some(shared), Some(local)) => parallel
                .row_group_metrics
                .as_ref()
                .map(|metrics| metrics(shared, local)),
            _ => None,
        };
        Some(ScanMetrics {
            rows_scanned,
            row_groups_scanned: row_groups.map(|(scanned, _)| scanned),
            total_row_groups: row_groups.map(|(_, total)| total),
        })
    }

    fn partition_data(
        &self,
        global: &TableScanGlobal,
        local: &Self::Local,
        partition_info: &OperatorPartitionInfo,
    ) -> Result<OperatorPartitionData> {
        let (Some(parallel), Some(shared), Some(local)) =
            (&self.function.parallel_scan, &global.parallel, local)
        else {
            return Err(Error::internal("table function scan without batch indices"));
        };
        let partition_data = match &parallel.partition_data {
            Some(partition_data) if partition_info.requires_partition_columns() => {
                partition_data(shared, local, &partition_info.partition_columns)?
            }
            _ => Vec::new(),
        };
        Ok(OperatorPartitionData {
            batch_index: (parallel.batch_index)(shared, local),
            partition_data,
        })
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
                global.bind_data.as_deref(),
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
        let more = scan(global.bind_data.as_deref(), &self.column_ids, state, &mut output)?;
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

#[cfg(test)]
mod empty_output_tests {
    use super::*;
    use crate::ClientEnv;
    use rustdb_types::Value;

    #[test]
    fn scan_replaces_empty_output_without_losing_previous_batch_ownership() -> Result<()> {
        let mut function = TableFunction::new("empty_output_scan", Vec::new());
        function.scan = Some(Arc::new(|_, _, state, output| {
            if state.position == 2 {
                return Ok(false);
            }
            let value = Value::varchar(format!("owned-scan-batch-{}-long-string", state.position));
            let mut vector = Vector::flat(LogicalType::VARCHAR, 1);
            vector.set_value(0, &value)?;
            output.set_columns(vec![vector], 1);
            state.position += 1;
            Ok(true)
        }));
        let scan = PhysicalTableScan::new(vec![LogicalType::VARCHAR], function, None, vec![0]);
        let context = ExecutionContext::new(ClientEnv::standalone()?);
        let global = Source::global_source_state(&scan, &context, None)?;
        let mut local = Source::local_source_state(&scan, &context, &global)?;
        let mut output = DataChunk::with_types(&[LogicalType::VARCHAR], 0);
        let mut snapshots = Vec::new();
        for _ in 0..2 {
            assert_eq!(
                Source::get_data(&scan, &context, &mut output, &global, &mut local)?,
                SourceResult::HaveMoreOutput
            );
            snapshots.push(output.clone());
        }
        assert_eq!(
            Source::get_data(&scan, &context, &mut output, &global, &mut local)?,
            SourceResult::Finished
        );
        drop(scan);
        drop(global);
        drop(output);
        for (batch, chunk) in snapshots.iter().enumerate() {
            assert_eq!(chunk.len(), 1);
            assert_eq!(
                chunk.column(0).get_value(0),
                Value::varchar(format!("owned-scan-batch-{batch}-long-string"))
            );
        }
        Ok(())
    }
}
