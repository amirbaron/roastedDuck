use crate::operator::set_estimated_cardinality;
use roastedduck_common::tree_renderer::RenderParams;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use roastedduck_common::Result;
use roastedduck_types::{DataChunk, LogicalType};

use crate::operator::{ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Source, SourceResult};

/// Scans materialised chunks (DuckDB's `PhysicalColumnDataScan`), e.g. for `VALUES`.
#[derive(Debug)]
pub struct PhysicalColumnDataScan {
    types: Vec<LogicalType>,
    chunks: Arc<[DataChunk]>,
    /// Whether this stands for an empty result (DuckDB's `PhysicalEmptyResult`).
    empty_result: bool,
}

impl PhysicalColumnDataScan {
    pub fn new(types: Vec<LogicalType>, chunks: Arc<[DataChunk]>) -> Self {
        Self {
            types,
            chunks,
            empty_result: false,
        }
    }

    /// A scan without rows planned for a `LOGICAL_EMPTY_RESULT`.
    pub fn empty_result(types: Vec<LogicalType>) -> Self {
        Self {
            types,
            chunks: Arc::from([]),
            empty_result: true,
        }
    }
}

impl PhysicalOperator for PhysicalColumnDataScan {
    fn name(&self) -> &'static str {
        "COLUMN_DATA_SCAN"
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

    fn explain_name(&self) -> String {
        if self.empty_result {
            "EMPTY_RESULT"
        } else {
            self.name()
        }
        .to_owned()
    }

    fn params_to_string(&self, estimated_cardinality: u64) -> RenderParams {
        let mut result = RenderParams::new();
        if self.empty_result {
            return result;
        }
        set_estimated_cardinality(&mut result, estimated_cardinality);
        result
    }
}

impl Source for PhysicalColumnDataScan {
    /// Index of the next chunk to hand out; threads claim chunks atomically.
    type Global = AtomicUsize;
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<AtomicUsize> {
        Ok(AtomicUsize::new(0))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &AtomicUsize) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        next: &AtomicUsize,
        (): &mut (),
    ) -> Result<SourceResult> {
        let index = next.fetch_add(1, Ordering::Relaxed);
        if let Some(source) = self.chunks.get(index) {
            chunk.set_columns(source.columns().to_vec(), source.len());
        }
        Ok(if index + 1 < self.chunks.len() {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }

    fn max_threads(&self, _: &AtomicUsize) -> usize {
        self.chunks.len().max(1)
    }
}
