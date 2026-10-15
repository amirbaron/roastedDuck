use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_function::{FunctionData, TableFunction, TableScanState};
use rustdb_types::{DataChunk, LogicalType};

use crate::operator::{ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Source, SourceResult};

/// Scans a table function (DuckDB's `PhysicalTableScan`): the function produces all of
/// its columns, of which `column_ids` are output.
pub struct PhysicalTableScan {
    types: Vec<LogicalType>,
    function: TableFunction,
    bind_data: Option<Arc<dyn FunctionData>>,
    column_ids: Vec<u64>,
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
        }
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
}

/// The scan's cursor; one thread produces at a time.
#[derive(Debug, Default)]
pub struct TableScanGlobal {
    state: Mutex<(TableScanState, bool)>,
}

impl Source for PhysicalTableScan {
    type Global = TableScanGlobal;
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<TableScanGlobal> {
        Ok(TableScanGlobal::default())
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &TableScanGlobal) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &TableScanGlobal,
        (): &mut (),
    ) -> Result<SourceResult> {
        let Some(scan) = &self.function.scan else {
            return Err(Error::internal(format!("table function {} cannot be scanned", self.function.name)));
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
        let count = output.len();
        let mut columns = output.into_columns();
        let mut projected = Vec::with_capacity(self.column_ids.len());
        for &id in &self.column_ids {
            let index = usize::try_from(id).map_err(|_| Error::not_implemented("virtual columns of table functions"))?;
            let column = columns
                .get_mut(index)
                .ok_or_else(|| Error::not_implemented("virtual columns of table functions"))?;
            projected.push(column.clone());
        }
        chunk.set_columns(projected, count);
        Ok(if more {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }
}
