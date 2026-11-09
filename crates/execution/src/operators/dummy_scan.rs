use std::sync::atomic::{AtomicBool, Ordering};

use roastedduck_common::Result;
use roastedduck_types::{DataChunk, LogicalType, Value};

use crate::operator::{ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Source, SourceResult};

/// Emits exactly one row with a single (unused) column, for `SELECT` without `FROM`.
#[derive(Debug)]
pub struct PhysicalDummyScan {
    types: Vec<LogicalType>,
}

impl PhysicalDummyScan {
    pub fn new(types: Vec<LogicalType>) -> Self {
        Self { types }
    }
}

impl PhysicalOperator for PhysicalDummyScan {
    fn name(&self) -> &'static str {
        "DUMMY_SCAN"
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

impl Source for PhysicalDummyScan {
    /// Whether the single row has been handed out.
    type Global = AtomicBool;
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, _: Option<&SinkState>) -> Result<AtomicBool> {
        Ok(AtomicBool::new(false))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &AtomicBool) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        emitted: &AtomicBool,
        (): &mut (),
    ) -> Result<SourceResult> {
        if !emitted.swap(true, Ordering::Relaxed) {
            // DuckDB's dummy scan emits a single constant NULL row.
            for column in 0..chunk.column_count() {
                chunk
                    .column_mut(column)
                    .set_value(0, &Value::null(self.types[column].clone()))?;
            }
            chunk.set_len(1);
        }
        Ok(SourceResult::Finished)
    }
}
