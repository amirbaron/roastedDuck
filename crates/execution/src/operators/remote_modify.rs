//! Remote inserts materialize input with the engine's normal spillable allocator,
//! then transfer it in one provider operation. Input scans cannot overlap COPY IN.
use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Sink, SinkFinalize, SinkResult, Source, SourceResult};
use roastedduck_catalog::backend::{TableBackend, TableMutation};
use roastedduck_catalog::{Catalog, EntryKind, Transaction};
use roastedduck_common::{Error, Result};
use roastedduck_function::FunctionClient;
use roastedduck_row::{ColumnDataAppendState, ColumnDataCollection};
use roastedduck_types::{DataChunk, LogicalType, Value, Vector};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Debug)]
pub enum RemoteModifyTarget {
    Insert(Arc<dyn TableBackend>),
    Update(Arc<dyn TableBackend>, bool),
    Delete(Arc<dyn TableBackend>, usize),
    Create {
        catalog: Arc<Catalog>,
        info: Box<roastedduck_ast::CreateInfo>,
    },
}
#[derive(Debug)]
pub struct PhysicalRemoteModify {
    child: [PhysicalRef; 1],
    target: RemoteModifyTarget,
    columns: Vec<usize>,
    transaction: Arc<Transaction>,
    client: Arc<FunctionClient>,
    types: Vec<LogicalType>,
    streaming: bool,
}
impl PhysicalRemoteModify {
    pub fn new(
        child: PhysicalRef,
        target: RemoteModifyTarget,
        columns: Vec<usize>,
        transaction: Arc<Transaction>,
        client: Arc<FunctionClient>,
    ) -> Self {
        Self {
            child: [child],
            target,
            columns,
            transaction,
            client,
            types: vec![LogicalType::BIGINT],
            streaming: false,
        }
    }
    #[must_use]
    pub fn with_streaming(mut self) -> Self {
        self.streaming = true;
        self
    }
    fn begin(&self, table: &Arc<dyn TableBackend>) -> Result<Box<dyn TableMutation>> {
        match &self.target {
            RemoteModifyTarget::Insert(_) => {
                table.begin_insert(&self.client, &self.transaction, &self.columns)
            }
            RemoteModifyTarget::Update(_, defaults) => {
                table.begin_update(&self.client, &self.transaction, &self.columns, *defaults)
            }
            RemoteModifyTarget::Delete(_, row_id) => {
                table.begin_delete(&self.client, &self.transaction, *row_id)
            }
            RemoteModifyTarget::Create { .. } => Err(Error::internal("CTAS cannot stream remote mutations")),
        }
    }
}
impl PhysicalOperator for PhysicalRemoteModify {
    fn sink_order_dependent(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        match self.target {
            RemoteModifyTarget::Update(..) => "PG_UPDATE",
            RemoteModifyTarget::Delete(..) => "PG_DELETE",
            _ => "INSERT",
        }
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.child
    }
    fn as_sink(&self) -> Option<&dyn ErasedSink> {
        Some(self)
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
}
#[derive(Debug)]
pub struct Global {
    input: Mutex<(ColumnDataCollection, ColumnDataAppendState)>,
    table: Option<Arc<dyn TableBackend>>,
    skip: bool,
    count: AtomicU64,
    mutation: Mutex<Option<Box<dyn TableMutation>>>,
}
impl Sink for PhysicalRemoteModify {
    type Global = Global;
    type Local = ();
    fn global_sink_state(&self, context: &ExecutionContext) -> Result<Global> {
        let (table, skip) = match &self.target {
            RemoteModifyTarget::Insert(table)
            | RemoteModifyTarget::Update(table, _)
            | RemoteModifyTarget::Delete(table, _) => (Some(table.clone()), false),
            RemoteModifyTarget::Create { catalog, info } => {
                let skip = if info.on_conflict == roastedduck_ast::OnCreateConflict::IgnoreOnConflict {
                    let roastedduck_ast::CreateInfoKind::Table(table) = &info.kind else {
                        return Err(Error::internal("Remote CTAS without table definition"));
                    };
                    catalog
                        .lookup_entry(
                            &self.client,
                            &self.transaction,
                            roastedduck_catalog::CatalogType::Table,
                            &info.schema,
                            &table.table,
                        )?
                        .is_some()
                } else {
                    false
                };
                (None, skip)
            }
        };
        let mutation = if self.streaming {
            table.as_ref().map(|t| self.begin(t)).transpose()?
        } else {
            None
        };
        Ok(Global {
            mutation: Mutex::new(mutation),
            table,
            skip,
            count: AtomicU64::new(0),
            input: Mutex::new((
                ColumnDataCollection::new(
                    context.env.buffer_manager.clone(),
                    self.child[0].types().to_vec(),
                    roastedduck_buffer::MemoryTag::ColumnData,
                )?,
                ColumnDataAppendState::default(),
            )),
        })
    }
    fn local_sink_state(&self, _: &ExecutionContext, _: &Global) -> Result<()> {
        Ok(())
    }
    fn sink(
        &self,
        _: &ExecutionContext,
        chunk: &DataChunk,
        global: &Global,
        (): &mut (),
    ) -> Result<SinkResult> {
        if global.skip {
            return Ok(SinkResult::Finished);
        }
        if self.streaming {
            global
                .mutation
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_mut()
                .ok_or_else(|| Error::internal("Remote streaming mutation has no state"))?
                .sink(chunk)?;
            return Ok(SinkResult::NeedMoreInput);
        }
        let mut input = global.input.lock().unwrap_or_else(PoisonError::into_inner);
        let (collection, state) = &mut *input;
        collection.append(state, chunk)?;
        Ok(SinkResult::NeedMoreInput)
    }
    fn combine(&self, _: &ExecutionContext, _: &Global, (): ()) -> Result<()> {
        Ok(())
    }
    fn finalize(&self, _: &ExecutionContext, global: &mut Global) -> Result<SinkFinalize> {
        if global.skip {
            return Ok(SinkFinalize::Ready);
        }
        if self.streaming {
            let count = global
                .mutation
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_mut()
                .ok_or_else(|| Error::internal("Remote streaming mutation has no state"))?
                .finish()?;
            global.count.store(count, Ordering::Release);
            return Ok(SinkFinalize::Ready);
        }
        // A replacing CTAS must finish reading the old relation before DROP/CREATE.
        // Buffered input also prevents nested source scans from overlapping COPY IN.
        if let RemoteModifyTarget::Create { catalog, info } = &self.target {
            let backend = catalog
                .backend()
                .ok_or_else(|| Error::internal("Remote CREATE without backend"))?;
            global.table = match backend.create(&self.client, &self.transaction, info)? {
                Some(entry) => match &entry.kind {
                    EntryKind::Table(table) => table.backend.clone(),
                    _ => return Err(Error::internal("Remote CREATE returned a non-table")),
                },
                None => None,
            };
        }
        if let Some(table) = &global.table {
            let input = global.input.lock().unwrap_or_else(PoisonError::into_inner);
            let mut state = input.0.initialize_scan();
            let mut chunks = std::iter::from_fn(|| input.0.scan(&mut state).transpose());
            let count = if matches!(
                self.target,
                RemoteModifyTarget::Insert(_) | RemoteModifyTarget::Create { .. }
            ) {
                table.insert(&self.client, &self.transaction, &self.columns, &mut chunks)?
            } else {
                let mut mutation = self.begin(table)?;
                for chunk in chunks {
                    mutation.sink(&chunk?)?;
                }
                mutation.finish()?
            };
            global.count.store(count, Ordering::Release);
        }
        Ok(SinkFinalize::Ready)
    }
}
impl Source for PhysicalRemoteModify {
    type Global = (SinkState, AtomicBool);
    type Local = ();
    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        Ok((
            sink.ok_or_else(|| Error::internal("Remote INSERT without sink state"))?
                .clone(),
            AtomicBool::new(false),
        ))
    }
    fn local_source_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<()> {
        Ok(())
    }
    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        (sink, done): &Self::Global,
        (): &mut (),
    ) -> Result<SourceResult> {
        if done.swap(true, Ordering::AcqRel) {
            chunk.set_len(0);
            return Ok(SourceResult::Finished);
        }
        let global = sink
            .downcast_ref::<Global>()
            .ok_or_else(|| Error::internal("Invalid remote INSERT state"))?;
        *chunk = DataChunk::new(
            vec![Vector::constant(&Value::bigint(
                global.count.load(Ordering::Acquire) as i64,
            ))?],
            1,
        );
        Ok(SourceResult::Finished)
    }
}
