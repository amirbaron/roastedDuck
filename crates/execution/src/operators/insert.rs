use std::fmt;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_storage::{DataTable, LocalAppendState, TableConstraints, Transaction};
use rustdb_types::{DataChunk, LogicalType};

use super::modify::{ModifyResult, count_types};
use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// Creates the target table of a `CREATE TABLE ... AS SELECT` when the insert starts;
/// returns `None` when nothing must be inserted (`IF NOT EXISTS` on an existing table).
pub trait TableCreator: Send + Sync + fmt::Debug {
    fn create(&self, transaction: &Transaction) -> Result<Option<Arc<DataTable>>>;
}

/// The table an insert writes to.
#[derive(Debug, Clone)]
pub enum InsertTarget {
    Table(Arc<DataTable>),
    /// `CREATE TABLE AS`: created by the hook in the sink's global state.
    Create(Arc<dyn TableCreator>),
}

/// Appends its input to a table's transaction-local storage (DuckDB's `PhysicalInsert`).
///
/// The input holds one column per physical table column, in table order (defaults are
/// computed by a projection below). Without `RETURNING` the operator returns the number
/// of inserted rows as a BIGINT; with it, the inserted rows (a projection above computes
/// the `RETURNING` expressions).
#[derive(Debug)]
pub struct PhysicalInsert {
    types: Vec<LogicalType>,
    children: [PhysicalRef; 1],
    target: InsertTarget,
    transaction: Arc<Transaction>,
    constraints: TableConstraints,
    return_chunk: bool,
}

impl PhysicalInsert {
    pub fn new(
        child: PhysicalRef,
        target: InsertTarget,
        transaction: Arc<Transaction>,
        constraints: TableConstraints,
        return_chunk: bool,
    ) -> Self {
        let types = if return_chunk {
            child.types().to_vec()
        } else {
            count_types()
        };
        Self {
            types,
            children: [child],
            target,
            transaction,
            constraints,
            return_chunk,
        }
    }
}

impl PhysicalOperator for PhysicalInsert {
    fn name(&self) -> &'static str {
        "INSERT"
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

/// The shared state of an insert: the table, one append into its local storage (DuckDB
/// serialises non-parallel inserts the same way) and the result.
#[derive(Debug)]
pub struct InsertGlobal {
    table: Option<Arc<DataTable>>,
    append: Mutex<Option<LocalAppendState>>,
    result: ModifyResult,
}

impl Sink for PhysicalInsert {
    type Global = InsertGlobal;
    type Local = ();

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        let table = match &self.target {
            InsertTarget::Table(table) => Some(Arc::clone(table)),
            InsertTarget::Create(creator) => creator.create(&self.transaction)?,
        };
        let append = match &table {
            Some(table) => Some(table.initialize_local_append(&self.transaction, self.constraints.clone())?),
            None => None,
        };
        Ok(InsertGlobal {
            table,
            append: Mutex::new(append),
            result: ModifyResult::default(),
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
        let Some(table) = &global.table else {
            return Ok(SinkResult::Finished);
        };
        let mut append = global.append.lock().unwrap_or_else(PoisonError::into_inner);
        let state = append
            .as_mut()
            .ok_or_else(|| Error::internal("insert after its append was finalized"))?;
        table.local_append(state, chunk)?;
        global.result.add_count(chunk.len() as u64);
        if self.return_chunk {
            global.result.push_returned(chunk.clone());
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, _: &ExecutionContext, _: &Self::Global, (): ()) -> Result<()> {
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut Self::Global) -> Result<SinkFinalize> {
        let append = global
            .append
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let (Some(table), Some(append)) = (&global.table, append) {
            table.finalize_local_append(append);
        }
        Ok(SinkFinalize::Ready)
    }
}

impl Source for PhysicalInsert {
    type Global = (SinkState, AtomicUsize);
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        let sink = sink.ok_or_else(|| Error::internal("insert source requires its sink state"))?;
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
            .downcast_ref::<InsertGlobal>()
            .ok_or_else(|| Error::internal("insert sink state has the wrong type"))?;
        Ok(if global.result.emit(self.return_chunk, next, chunk)? {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }
}
