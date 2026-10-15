use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use rustdb_common::{Error, Result};
use rustdb_storage::{DataTable, Transaction};
use rustdb_types::{DataChunk, LogicalType};

use super::modify::{ModifyResult, all_columns, count_types, row_ids};
use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Sink, SinkResult, Source, SourceResult};

/// Deletes the rows whose row ids its input carries (DuckDB's `PhysicalDelete`).
///
/// Returns the number of deleted rows as a BIGINT, or with `RETURNING` the deleted rows
/// (all physical columns, fetched before the delete).
#[derive(Debug)]
pub struct PhysicalDelete {
    types: Vec<LogicalType>,
    children: [PhysicalRef; 1],
    table: Arc<DataTable>,
    transaction: Arc<Transaction>,
    /// Position of the BIGINT row id column in the input.
    row_id_index: usize,
    return_chunk: bool,
}

impl PhysicalDelete {
    pub fn new(
        child: PhysicalRef,
        table: Arc<DataTable>,
        transaction: Arc<Transaction>,
        row_id_index: usize,
        return_chunk: bool,
    ) -> Self {
        let types = if return_chunk {
            table.types()
        } else {
            count_types()
        };
        Self {
            types,
            children: [child],
            table,
            transaction,
            row_id_index,
            return_chunk,
        }
    }
}

impl PhysicalOperator for PhysicalDelete {
    fn name(&self) -> &'static str {
        "DELETE"
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn as_sink(&self) -> Option<&dyn ErasedSink> {
        Some(self)
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
}

impl Sink for PhysicalDelete {
    type Global = ModifyResult;
    type Local = ();

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        Ok(ModifyResult::default())
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<()> {
        Ok(())
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &Self::Global,
        (): &mut (),
    ) -> Result<SinkResult> {
        let ids = row_ids(chunk.column(self.row_id_index), chunk.len())?;
        if self.return_chunk {
            let mut deleted = DataChunk::default();
            let columns = all_columns(self.table.columns().len());
            self.table
                .fetch(&self.transaction, &mut deleted, &columns, &ids)?;
            global.push_returned(deleted);
        }
        let count = self.table.delete(&self.transaction, &ids)?;
        global.add_count(count);
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, _: &Self::Global, (): ()) -> Result<()> {
        Ok(())
    }
}

impl Source for PhysicalDelete {
    type Global = (SinkState, AtomicUsize);
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        let sink = sink.ok_or_else(|| Error::internal("delete source requires its sink state"))?;
        Ok((Arc::clone(sink), AtomicUsize::new(0)))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        (sink, next): &Self::Global,
        (): &mut (),
    ) -> Result<SourceResult> {
        let result = sink
            .downcast_ref::<ModifyResult>()
            .ok_or_else(|| Error::internal("delete sink state has the wrong type"))?;
        Ok(if result.emit(self.return_chunk, next, chunk)? {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }
}
