use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustdb_common::Result;
use rustdb_types::{DataChunk, LogicalType};

use crate::operator::{ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Source, SourceResult};

/// Scans materialised chunks (DuckDB's `PhysicalColumnDataScan`), e.g. for `VALUES`.
#[derive(Debug)]
pub struct PhysicalColumnDataScan {
    types: Vec<LogicalType>,
    chunks: Arc<[DataChunk]>,
}

impl PhysicalColumnDataScan {
    pub fn new(types: Vec<LogicalType>, chunks: Arc<[DataChunk]>) -> Self {
        Self { types, chunks }
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
}
