use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustdb_common::Result;
use rustdb_storage::Transaction;
use rustdb_types::{DataChunk, LogicalType};

use super::insert::TableCreator;
use crate::operator::{ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Source, SourceResult};

/// `CREATE TABLE` (DuckDB's `PhysicalCreateTable`): creates the table's storage and
/// catalog entry through `creator` and returns no rows.
#[derive(Debug)]
pub struct PhysicalCreateTable {
    types: Vec<LogicalType>,
    creator: Arc<dyn TableCreator>,
    transaction: Arc<Transaction>,
}

impl PhysicalCreateTable {
    pub fn new(creator: Arc<dyn TableCreator>, transaction: Arc<Transaction>) -> Self {
        Self {
            types: vec![LogicalType::BIGINT],
            creator,
            transaction,
        }
    }
}

impl PhysicalOperator for PhysicalCreateTable {
    fn name(&self) -> &'static str {
        "CREATE_TABLE"
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

impl Source for PhysicalCreateTable {
    /// Whether the table was created already (by another thread).
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
        created: &AtomicBool,
        (): &mut (),
    ) -> Result<SourceResult> {
        if !created.swap(true, Ordering::AcqRel) {
            self.creator.create(&self.transaction)?;
        }
        chunk.set_len(0);
        Ok(SourceResult::Finished)
    }
}
