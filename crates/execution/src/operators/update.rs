use std::collections::HashSet;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_storage::{DataTable, LocalAppendState, TableConstraints, Transaction};
use rustdb_types::{DataChunk, LogicalType, SelectionVector};

use super::modify::{ModifyResult, all_columns, count_types, row_ids};
use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// Updates table rows (DuckDB's `PhysicalUpdate`).
///
/// The input holds the new value of each updated column (in `columns` order) followed by
/// the BIGINT row id. Updates of indexed or nested columns are executed as a delete plus
/// an insert (`update_is_del_and_insert`), each row at most once per statement.
#[derive(Debug)]
pub struct PhysicalUpdate {
    types: Vec<LogicalType>,
    children: [PhysicalRef; 1],
    table: Arc<DataTable>,
    transaction: Arc<Transaction>,
    /// The physical table columns updated by the input's columns.
    columns: Vec<usize>,
    constraints: TableConstraints,
    del_and_insert: bool,
    return_chunk: bool,
}

impl PhysicalUpdate {
    pub fn new(
        child: PhysicalRef,
        table: Arc<DataTable>,
        transaction: Arc<Transaction>,
        columns: Vec<usize>,
        constraints: TableConstraints,
        del_and_insert: bool,
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
            columns,
            constraints,
            del_and_insert,
            return_chunk,
        }
    }
}

impl PhysicalOperator for PhysicalUpdate {
    fn name(&self) -> &'static str {
        "UPDATE"
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

/// The shared state of an update.
#[derive(Debug)]
pub struct UpdateGlobal {
    result: ModifyResult,
    /// Rows already deleted and re-inserted by this statement (delete + insert mode).
    updated_rows: Mutex<HashSet<i64>>,
    /// The append of re-inserted rows (delete + insert mode).
    append: Mutex<Option<LocalAppendState>>,
}

impl PhysicalUpdate {
    fn sink_in_place(&self, chunk: &DataChunk, ids: &[i64], global: &UpdateGlobal) -> Result<()> {
        let updates = DataChunk::new(chunk.columns()[..self.columns.len()].to_vec(), chunk.len());
        self.table
            .update(&self.transaction, ids, &self.columns, &updates, &self.constraints)?;
        if self.return_chunk {
            let mut rows = DataChunk::default();
            self.table.fetch(
                &self.transaction,
                &mut rows,
                &all_columns(self.table.columns().len()),
                ids,
            )?;
            global.result.push_returned(rows);
        }
        Ok(())
    }

    fn sink_delete_and_insert(&self, chunk: &DataChunk, ids: &[i64], global: &UpdateGlobal) -> Result<()> {
        let mut updated = global.updated_rows.lock().unwrap_or_else(PoisonError::into_inner);
        let fresh: Vec<u32> = (0..ids.len())
            .filter(|&row| updated.insert(ids[row]))
            .map(|row| row as u32)
            .collect();
        drop(updated);
        if fresh.is_empty() {
            return Ok(());
        }
        let fresh_ids: Vec<i64> = fresh.iter().map(|&row| ids[row as usize]).collect();
        // Fetch the current rows, delete them, and insert them with the new values.
        let mut rows = DataChunk::default();
        self.table.fetch(
            &self.transaction,
            &mut rows,
            &all_columns(self.table.columns().len()),
            &fresh_ids,
        )?;
        if rows.len() != fresh_ids.len() {
            return Err(Error::internal("update could not fetch every row it deletes"));
        }
        self.table
            .delete_with_constraints(&self.transaction, &fresh_ids, &self.constraints)?;
        let sel = SelectionVector::new(fresh);
        let mut columns = rows.into_columns();
        for (index, &column) in self.columns.iter().enumerate() {
            columns[column] = chunk.column(index).slice(&sel).flattened(fresh_ids.len());
        }
        let new_rows = DataChunk::new(columns, fresh_ids.len());
        let mut append = global.append.lock().unwrap_or_else(PoisonError::into_inner);
        if append.is_none() {
            *append = Some(
                self.table
                    .initialize_local_append(&self.transaction, self.constraints.clone())?,
            );
        }
        let state = append
            .as_mut()
            .ok_or_else(|| Error::internal("update append state missing"))?;
        self.table.local_append(state, &new_rows)?;
        if self.return_chunk {
            global.result.push_returned(new_rows);
        }
        Ok(())
    }
}

impl Sink for PhysicalUpdate {
    type Global = UpdateGlobal;
    type Local = ();

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        Ok(UpdateGlobal {
            result: ModifyResult::default(),
            updated_rows: Mutex::default(),
            append: Mutex::default(),
        })
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
        let row_id_column = chunk
            .column_count()
            .checked_sub(1)
            .ok_or_else(|| Error::internal("update input without a row id column"))?;
        let ids = row_ids(chunk.column(row_id_column), chunk.len())?;
        if self.del_and_insert {
            self.sink_delete_and_insert(chunk, &ids, global)?;
        } else {
            self.sink_in_place(chunk, &ids, global)?;
        }
        global.result.add_count(chunk.len() as u64);
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, _: &Self::Global, (): ()) -> Result<()> {
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut Self::Global) -> Result<SinkFinalize> {
        if let Some(append) = global
            .append
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            self.table.finalize_local_append(append);
        }
        Ok(SinkFinalize::Ready)
    }
}

impl Source for PhysicalUpdate {
    type Global = (SinkState, AtomicUsize);
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        let sink = sink.ok_or_else(|| Error::internal("update source requires its sink state"))?;
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
            .downcast_ref::<UpdateGlobal>()
            .ok_or_else(|| Error::internal("update sink state has the wrong type"))?;
        Ok(if global.result.emit(self.return_chunk, next, chunk)? {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }
}
