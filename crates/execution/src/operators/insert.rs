use std::fmt;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, PoisonError};

use roastedduck_common::{Error, Result};
use roastedduck_storage::{
    DataTable, OptimisticCollection, OptimisticDataWriter, TableConstraints, Transaction,
};
use roastedduck_types::{DataChunk, LogicalType};

use super::modify::{ModifyResult, SharedAppend, count_types};
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

impl InsertTarget {
    /// The table, created by a `CREATE TABLE AS` target.
    pub(crate) fn table(&self, transaction: &Transaction) -> Result<Option<Arc<DataTable>>> {
        match self {
            Self::Table(table) => Ok(Some(Arc::clone(table))),
            Self::Create(creator) => creator.create(transaction),
        }
    }
}

/// Appends its input to a table's transaction-local storage (DuckDB's `PhysicalInsert`).
///
/// The input holds one column per physical table column, in table order (defaults are
/// computed by a projection below). Without `RETURNING` the operator returns the number
/// of inserted rows as a BIGINT; with it, the inserted rows (a projection above computes
/// the `RETURNING` expressions).
///
/// A parallel insert (whose input order does not matter) appends each thread's rows to a
/// collection of its own, whose complete row groups the thread's optimistic writer writes
/// to the database file, and merges the collections into the local storage in `combine`.
#[derive(Debug)]
pub struct PhysicalInsert {
    types: Vec<LogicalType>,
    children: [PhysicalRef; 1],
    target: InsertTarget,
    transaction: Arc<Transaction>,
    constraints: TableConstraints,
    return_chunk: bool,
    /// An append shared with other operators writing the same table (the actions of a
    /// MERGE), which must not run interleaved appends.
    shared_append: Option<SharedAppend>,
    /// Whether threads append in parallel to collections of their own (DuckDB's
    /// `parallel`).
    parallel: bool,
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
            shared_append: None,
            parallel: false,
        }
    }

    /// Appends through `append`, shared with other operators on the same table.
    #[must_use]
    pub fn with_shared_append(mut self, append: SharedAppend) -> Self {
        self.shared_append = Some(append);
        self.parallel = false;
        self
    }

    /// Lets threads append in parallel. Only planned for inserts without `RETURNING`
    /// into tables without indexes or foreign keys, whose input order does not matter.
    #[must_use]
    pub fn with_parallel(mut self, parallel: bool) -> Self {
        self.parallel = parallel && !self.return_chunk && self.shared_append.is_none();
        self
    }
}

impl PhysicalOperator for PhysicalInsert {
    fn name(&self) -> &'static str {
        match self.target {
            InsertTarget::Table(_) => "INSERT",
            InsertTarget::Create(_) => "CREATE_TABLE_AS",
        }
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
    fn parallel_sink(&self) -> bool {
        self.parallel
    }
}

/// The shared state of an insert: the table, one append into its local storage (DuckDB
/// serialises non-parallel inserts the same way) and the result.
#[derive(Debug)]
pub struct InsertGlobal {
    table: Option<Arc<DataTable>>,
    append: SharedAppend,
    result: ModifyResult,
    /// Serialises the merges of a parallel insert's threads (DuckDB's `lock`).
    merge_lock: Mutex<()>,
}

/// A thread's rows of a parallel insert, and the writer of their complete row groups.
#[derive(Debug, Default)]
pub struct InsertLocal {
    collection: Option<OptimisticCollection>,
    writer: Option<OptimisticDataWriter>,
}

impl Sink for PhysicalInsert {
    type Global = InsertGlobal;
    type Local = InsertLocal;

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        let table = self.target.table(&self.transaction)?;
        let append = match (&self.shared_append, &table) {
            (Some(shared), _) => Arc::clone(shared),
            // A parallel insert appends small collections with appends of their own.
            (None, Some(_)) if self.parallel => SharedAppend::default(),
            (None, Some(table)) => Arc::new(Mutex::new(Some(
                table.initialize_local_append(&self.transaction, self.constraints.clone())?,
            ))),
            (None, None) => SharedAppend::default(),
        };
        Ok(InsertGlobal {
            table,
            append,
            result: ModifyResult::default(),
            merge_lock: Mutex::new(()),
        })
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<InsertLocal> {
        Ok(InsertLocal::default())
    }

    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &Self::Global,
        local: &mut InsertLocal,
    ) -> Result<SinkResult> {
        let Some(table) = &global.table else {
            return Ok(SinkResult::Finished);
        };
        if self.parallel {
            table.verify_append_constraints(&self.constraints, chunk)?;
            if local.collection.is_none() {
                local.writer = Some(table.create_optimistic_writer(&self.transaction)?);
                local.collection = Some(table.create_optimistic_collection(&self.transaction)?);
            }
            if let (Some(collection), Some(writer)) = (&mut local.collection, &mut local.writer) {
                collection.append(chunk, writer)?;
            }
            return Ok(SinkResult::NeedMoreInput);
        }
        let mut append = global.append.lock().unwrap_or_else(PoisonError::into_inner);
        if append.is_none() {
            *append = Some(table.initialize_local_append(&self.transaction, self.constraints.clone())?);
        }
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

    fn combine(&self, _: &ExecutionContext, global: &Self::Global, local: InsertLocal) -> Result<()> {
        let (Some(table), Some(mut collection), Some(mut writer)) =
            (&global.table, local.collection, local.writer)
        else {
            return Ok(());
        };
        collection.finalize();
        let count = collection.total_rows();
        let row_group_size = table.row_groups().row_group_size();
        if count >= row_group_size {
            // Write the thread's remaining rows and its partially filled blocks.
            collection.write_unflushed_row_groups(&mut writer)?;
            writer.final_flush()?;
        }
        let _merge = global.merge_lock.lock().unwrap_or_else(PoisonError::into_inner);
        global.result.add_count(count);
        if count < row_group_size {
            // Few rows: append them to the local storage directly.
            append_collections(
                table,
                &self.transaction,
                &self.constraints,
                std::iter::once(&collection),
            )
        } else {
            // The rows were written: move their row groups into the local storage.
            table.local_merge(&self.transaction, &collection)?;
            table.merge_optimistic_writer(&self.transaction, &mut writer)
        }
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut Self::Global) -> Result<SinkFinalize> {
        let append = global
            .append
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let (Some(table), Some(append)) = (&global.table, append) {
            table.finalize_local_append(append);
        }
        Ok(SinkFinalize::Ready)
    }
}

/// Appends the rows of `collections`, in order, to the local storage through one local
/// append (`InitializeLocalAppend` ... `FinalizeLocalAppend`).
pub(crate) fn append_collections<'a>(
    table: &Arc<DataTable>,
    transaction: &Transaction,
    constraints: &TableConstraints,
    collections: impl IntoIterator<Item = &'a OptimisticCollection>,
) -> Result<()> {
    let mut append = table.initialize_local_append(transaction, constraints.clone())?;
    let mut appended = Ok(());
    for collection in collections {
        appended = table.local_append_collection(&mut append, collection);
        if appended.is_err() {
            break;
        }
    }
    table.finalize_local_append(append);
    appended
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
