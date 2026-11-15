//! Complete mutation input before running interleaved remote actions on its session.
use crate::operator::{ErasedSink, ErasedSource, PhysicalOperator, PhysicalRef, SinkState};
use crate::{ExecutionContext, Sink, SinkFinalize, SinkResult, Source, SourceResult};
use roastedduck_common::{Error, Result};
use roastedduck_row::{ColumnDataAppendState, ColumnDataCollection};
use roastedduck_types::{DataChunk, LogicalType};
use std::sync::{Mutex, PoisonError};

#[derive(Debug)]
pub struct PhysicalBufferedModify {
    child: [PhysicalRef; 1],
    inner: PhysicalRef,
}
impl PhysicalBufferedModify {
    pub fn new(child: PhysicalRef, inner: PhysicalRef) -> Self {
        Self {
            child: [child],
            inner,
        }
    }
    fn sink(&self) -> Result<&dyn ErasedSink> {
        self.inner
            .as_sink()
            .ok_or_else(|| Error::internal("Buffered mutation without sink"))
    }
    fn source(&self) -> Result<&dyn ErasedSource> {
        self.inner
            .as_source()
            .ok_or_else(|| Error::internal("Buffered mutation without source"))
    }
}
impl PhysicalOperator for PhysicalBufferedModify {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn types(&self) -> &[LogicalType] {
        self.inner.types()
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
    fn sink_order_dependent(&self) -> bool {
        true
    }
}
#[derive(Debug)]
pub struct Global {
    input: Mutex<(ColumnDataCollection, ColumnDataAppendState)>,
    result: Option<SinkState>,
}
impl Sink for PhysicalBufferedModify {
    type Global = Global;
    type Local = ();
    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<Global> {
        Ok(Global {
            input: Mutex::new((
                ColumnDataCollection::new(
                    ctx.env.buffer_manager.clone(),
                    self.child[0].types().to_vec(),
                    roastedduck_buffer::MemoryTag::ColumnData,
                )?,
                ColumnDataAppendState::default(),
            )),
            result: None,
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
        let mut input = global.input.lock().unwrap_or_else(PoisonError::into_inner);
        let (collection, state) = &mut *input;
        collection.append(state, chunk)?;
        Ok(SinkResult::NeedMoreInput)
    }
    fn combine(&self, _: &ExecutionContext, _: &Global, (): ()) -> Result<()> {
        Ok(())
    }
    fn finalize(&self, ctx: &ExecutionContext, global: &mut Global) -> Result<SinkFinalize> {
        let sink = self.sink()?;
        let mut state = sink.global_sink_state(ctx)?;
        let mut local = sink.local_sink_state(ctx, state.as_ref())?;
        let input = global.input.lock().unwrap_or_else(PoisonError::into_inner);
        let mut scan = input.0.initialize_scan();
        while let Some(chunk) = input.0.scan(&mut scan)? {
            if sink.sink(ctx, &chunk, state.as_ref(), local.as_mut())? == SinkResult::Finished {
                break;
            }
        }
        sink.combine(ctx, state.as_ref(), local)?;
        sink.finalize(ctx, state.as_mut())?;
        global.result = Some(SinkState::from(state));
        Ok(SinkFinalize::Ready)
    }
}
impl Source for PhysicalBufferedModify {
    type Global = Box<dyn std::any::Any + Send + Sync>;
    type Local = Box<dyn std::any::Any + Send>;
    fn global_source_state(&self, ctx: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        let global = sink
            .and_then(|s| s.downcast_ref::<Global>())
            .ok_or_else(|| Error::internal("Buffered mutation without finalized state"))?;
        self.source()?.global_source_state(ctx, global.result.as_ref())
    }
    fn local_source_state(&self, ctx: &ExecutionContext, global: &Self::Global) -> Result<Self::Local> {
        self.source()?.local_source_state(ctx, global.as_ref())
    }
    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<SourceResult> {
        self.source()?
            .get_data(ctx, chunk, global.as_ref(), local.as_mut())
    }
}
