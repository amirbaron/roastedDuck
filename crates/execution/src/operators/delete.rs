use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_storage::{DataTable, TableConstraints, Transaction};
use rustdb_types::{DataChunk, LogicalType, SelectionVector};

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
    /// Foreign keys referencing the table (verified before rows are deleted).
    constraints: TableConstraints,
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
            constraints: TableConstraints::default(),
        }
    }

    /// The rows of `deleted` (fetched for `ids`) not returned before, each once. When
    /// some are dropped, the rest come in row id order, as DuckDB collects them in a
    /// `map<row_t, idx_t>`.
    fn newly_deleted(global: &DeleteGlobal, ids: &[i64], deleted: DataChunk) -> DataChunk {
        let mut returned = global.deleted_row_ids.lock().unwrap_or_else(PoisonError::into_inner);
        let mut new_rows = BTreeMap::new();
        for (row, &id) in ids.iter().enumerate() {
            if !returned.contains(&id) && !new_rows.contains_key(&id) {
                new_rows.insert(id, row as u32);
                returned.insert(id);
            }
        }
        if new_rows.len() == deleted.len() {
            return deleted;
        }
        let sel = SelectionVector::new(new_rows.into_values().collect::<Vec<u32>>());
        let len = sel.len();
        DataChunk::new(deleted.columns().iter().map(|column| column.slice(&sel)).collect(), len)
    }

    /// Verifies the delete-side foreign keys of `constraints` before deleting.
    #[must_use]
    pub fn with_constraints(mut self, constraints: TableConstraints) -> Self {
        self.constraints = constraints;
        self
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

/// The shared state of a delete.
#[derive(Debug, Default)]
pub struct DeleteGlobal {
    result: ModifyResult,
    /// The rows this statement returned as deleted (with `RETURNING`): a join can
    /// produce a row more than once (DuckDB's `deleted_row_ids`).
    deleted_row_ids: Mutex<HashSet<i64>>,
}

impl Sink for PhysicalDelete {
    type Global = DeleteGlobal;
    type Local = ();

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        Ok(DeleteGlobal::default())
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
        let returned = if self.return_chunk {
            let mut deleted = DataChunk::default();
            let columns = all_columns(self.table.columns().len());
            self.table
                .fetch(&self.transaction, &mut deleted, &columns, &ids)?;
            Some(deleted)
        } else {
            None
        };
        let count = self
            .table
            .delete_with_constraints(&self.transaction, &ids, &self.constraints)?;
        global.result.add_count(count);
        if let Some(deleted) = returned {
            global.result.push_returned(Self::newly_deleted(global, &ids, deleted));
        }
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
        let global = sink
            .downcast_ref::<DeleteGlobal>()
            .ok_or_else(|| Error::internal("delete sink state has the wrong type"))?;
        Ok(if global.result.emit(self.return_chunk, next, chunk)? {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }
}
