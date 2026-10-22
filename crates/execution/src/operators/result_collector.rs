use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use rustdb_buffer::MemoryTag;
use rustdb_common::{Error, Result};
use rustdb_row::{ColumnDataAppendState, ColumnDataCollection};
use rustdb_types::{DataChunk, LogicalType};

use super::buffered_data::BufferedData;
use crate::operator::{
    ErasedSink, OperatorPartitionInfo, PartitionInfo, PhysicalOperator, PhysicalRef, SinkState,
};
use crate::plan_generator::order_preservation::{preserve_insertion_order, use_batch_index};
use crate::{ExecutionContext, Sink, SinkFinalize, SinkResult};

/// Where a materialized result keeps its rows (DuckDB's `QueryResultMemoryType`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QueryResultMemoryType {
    /// In chunks on the heap, not counted against the memory limit (`IN_MEMORY`).
    #[default]
    InMemory,
    /// In a buffer-managed `ColumnDataCollection`, counted against the memory limit and
    /// spilled to the temporary directory when evicted (`BUFFER_MANAGED`).
    BufferManaged,
}

/// The materialised output of a query.
#[derive(Debug, Default)]
pub struct CollectedResult {
    /// The rows of an in-memory result, and those a streaming collector buffered.
    pub chunks: Vec<DataChunk>,
    /// The rows of a buffer-managed result (`MaterializedCollectorGlobalState::collection`).
    pub collection: Option<ColumnDataCollection>,
    /// The batches of a batch collector, until `finalize` concatenates them in order.
    batches: BTreeMap<u64, Vec<DataChunk>>,
    /// The batches of a buffer-managed batch collector (`BatchedDataCollection::data`).
    collections: BTreeMap<u64, ColumnDataCollection>,
    /// The buffer of a streaming collector (`BufferedCollectorGlobalState::buffered_data`).
    buffered: Option<Arc<BufferedData>>,
}

/// The rows a result collector collected: chunks, or a buffer-managed collection.
#[derive(Debug)]
pub enum CollectedRows {
    Chunks(Vec<DataChunk>),
    Collection(ColumnDataCollection),
}

/// How a result collector keeps the rows' order (the subclasses DuckDB's
/// `PhysicalResultCollector::GetResultCollector` chooses from).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectorKind {
    /// Rows in arrival order (`PhysicalMaterializedCollector`); `parallel` when the order
    /// does not matter, otherwise the final pipeline runs on one thread.
    Materialized { parallel: bool },
    /// Rows in the order of their batch indices, collected by any number of threads
    /// (`PhysicalBatchCollector`).
    Batch,
    /// Rows handed out while the query runs, as soon as they arrive (`PhysicalBufferedCollector`
    /// of a streaming result); `parallel` when the order does not matter.
    Buffered { parallel: bool },
    /// Rows handed out while the query runs, in the order of their batch indices
    /// (`PhysicalBufferedBatchCollector`).
    BufferedBatch,
}

/// Collects the final query result (DuckDB's `PhysicalMaterializedCollector` and
/// `PhysicalBatchCollector`).
#[derive(Debug)]
pub struct PhysicalResultCollector {
    types: Vec<LogicalType>,
    kind: CollectorKind,
    /// Where a materialized result keeps its rows (`PhysicalResultCollector::memory_type`).
    memory_type: QueryResultMemoryType,
    /// The bytes a streaming collector buffers before its sink blocks
    /// (`streaming_buffer_size`).
    buffer_size: usize,
    children: [PhysicalRef; 1],
}

impl PhysicalResultCollector {
    /// A collector that keeps arrival order on a single thread.
    pub fn new(child: PhysicalRef) -> Self {
        Self::with_kind(child, CollectorKind::Materialized { parallel: false })
    }

    pub fn with_kind(child: PhysicalRef, kind: CollectorKind) -> Self {
        Self {
            types: child.types().to_vec(),
            kind,
            memory_type: QueryResultMemoryType::InMemory,
            buffer_size: DEFAULT_STREAMING_BUFFER_SIZE,
            children: [child],
        }
    }

    /// The collector keeping a materialized result as `memory_type` says.
    #[must_use]
    pub fn with_memory_type(mut self, memory_type: QueryResultMemoryType) -> Self {
        self.memory_type = memory_type;
        self
    }

    /// The collector DuckDB picks for `plan` (`GetResultCollector`): parallel when the
    /// plan's order does not matter, by batch index when every source supports it, and
    /// single-threaded otherwise.
    pub fn for_plan(plan: PhysicalRef, threads: usize, preserve_order_setting: bool) -> Self {
        let kind = if !preserve_insertion_order(plan.as_ref(), preserve_order_setting) {
            CollectorKind::Materialized { parallel: true }
        } else if !use_batch_index(plan.as_ref(), threads) {
            CollectorKind::Materialized { parallel: false }
        } else {
            CollectorKind::Batch
        };
        Self::with_kind(plan, kind)
    }

    /// The collector of a streaming result for `plan` (`GetResultCollector` with
    /// `ALLOW_STREAMING`): parallel when the plan's order does not matter, by batch index
    /// when every source supports it, and single-threaded otherwise. Its sink blocks once
    /// `buffer_size` bytes are buffered.
    pub fn streaming(
        plan: PhysicalRef,
        threads: usize,
        preserve_order_setting: bool,
        buffer_size: usize,
    ) -> Self {
        let kind = if !preserve_insertion_order(plan.as_ref(), preserve_order_setting) {
            CollectorKind::Buffered { parallel: true }
        } else if !use_batch_index(plan.as_ref(), threads) {
            CollectorKind::Buffered { parallel: false }
        } else {
            CollectorKind::BufferedBatch
        };
        let mut collector = Self::with_kind(plan, kind);
        collector.buffer_size = buffer_size;
        collector
    }

    /// Whether the collector hands rows out while the query runs (`IsStreaming`).
    pub fn is_streaming(&self) -> bool {
        matches!(
            self.kind,
            CollectorKind::Buffered { .. } | CollectorKind::BufferedBatch
        )
    }

    /// The buffer of a streaming collector, from its global state (`GetResult`'s
    /// `buffered_data`).
    pub fn buffered_data(global: &SinkState) -> Result<Option<Arc<BufferedData>>> {
        let collected = global
            .downcast_ref::<Mutex<CollectedResult>>()
            .ok_or_else(|| Error::internal("result collector state has an unexpected type"))?;
        Ok(collected.lock().map_err(|_| poisoned())?.buffered.clone())
    }

    pub fn kind(&self) -> CollectorKind {
        self.kind
    }

    /// Whether the rows go to buffer-managed collections (`CreateCollection` with
    /// `BUFFER_MANAGED`). Results without columns stay on the heap.
    fn buffer_managed(&self) -> bool {
        self.memory_type == QueryResultMemoryType::BufferManaged
            && !self.types.is_empty()
            && !self.is_streaming()
    }

    /// A new buffer-managed collection for the result's rows, ready for appending.
    fn create_collection(
        &self,
        ctx: &ExecutionContext,
    ) -> Result<(ColumnDataCollection, ColumnDataAppendState)> {
        let mut collection = ColumnDataCollection::new(
            ctx.env.buffer_manager.clone(),
            self.types.clone(),
            MemoryTag::ColumnData,
        )?;
        let mut append = ColumnDataAppendState::default();
        collection.initialize_append(&mut append)?;
        Ok((collection, append))
    }

    /// Takes the chunks collected so far from the collector's global state: those a
    /// streaming collector buffered while the query runs (`BufferedData::Scan`), or the
    /// whole result once the query finished.
    pub fn take_chunks(global: &SinkState) -> Result<Vec<DataChunk>> {
        let collected = global
            .downcast_ref::<Mutex<CollectedResult>>()
            .ok_or_else(|| Error::internal("result collector state has an unexpected type"))?;
        Ok(std::mem::take(
            &mut collected.lock().map_err(|_| poisoned())?.chunks,
        ))
    }

    /// Takes the rows of a finished query from the collector's global state
    /// (`GetResult`): its buffer-managed collection, if it made one, else its chunks.
    pub fn take_rows(global: &SinkState) -> Result<CollectedRows> {
        let collected = global
            .downcast_ref::<Mutex<CollectedResult>>()
            .ok_or_else(|| Error::internal("result collector state has an unexpected type"))?;
        let mut collected = collected.lock().map_err(|_| poisoned())?;
        Ok(match collected.collection.take() {
            Some(collection) => CollectedRows::Collection(collection),
            None => CollectedRows::Chunks(std::mem::take(&mut collected.chunks)),
        })
    }
}

impl PhysicalOperator for PhysicalResultCollector {
    fn name(&self) -> &'static str {
        "RESULT_COLLECTOR"
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
    fn parallel_sink(&self) -> bool {
        match self.kind {
            CollectorKind::Materialized { parallel } | CollectorKind::Buffered { parallel } => parallel,
            CollectorKind::Batch | CollectorKind::BufferedBatch => true,
        }
    }
    fn sink_order_dependent(&self) -> bool {
        matches!(
            self.kind,
            CollectorKind::Materialized { .. } | CollectorKind::Buffered { .. }
        )
    }
    fn required_partition_info(&self) -> OperatorPartitionInfo {
        OperatorPartitionInfo {
            batch_index: matches!(self.kind, CollectorKind::Batch | CollectorKind::BufferedBatch),
            partition_columns: Vec::new(),
        }
    }
}

fn poisoned() -> Error {
    Error::internal("result collector lock poisoned")
}

/// One thread's collected chunks.
#[derive(Debug, Default)]
pub struct CollectorLocal {
    /// The batch the incoming chunks belong to (batch collectors).
    batch_index: u64,
    chunks: Vec<DataChunk>,
    /// Completed batches (batch collectors).
    batches: BTreeMap<u64, Vec<DataChunk>>,
    /// The buffer-managed rows of a materialized collector
    /// (`MaterializedCollectorLocalState::collection`), or of the batch `collection_batch`
    /// of a batch collector (`BatchedDataCollection::last_collection`).
    collection: Option<(ColumnDataCollection, ColumnDataAppendState)>,
    collection_batch: Option<u64>,
    /// The completed buffer-managed batches of a batch collector
    /// (`BatchedDataCollection::data`).
    collections: BTreeMap<u64, ColumnDataCollection>,
    /// The buffer of a streaming collector.
    buffered: Option<Arc<BufferedData>>,
    /// The batch a streaming batch collector sinks (`BufferedBatchCollectorLocalState`).
    current_batch: u64,
}

impl CollectorLocal {
    /// Files the chunks of the current batch under its index.
    fn flush_batch(&mut self) {
        if !self.chunks.is_empty() {
            self.batches
                .entry(self.batch_index)
                .or_default()
                .append(&mut self.chunks);
        }
    }

    /// Files the buffer-managed collection of the current batch under its index.
    fn flush_collection(&mut self) {
        if let (Some((collection, _)), Some(batch)) = (self.collection.take(), self.collection_batch.take()) {
            self.collections.insert(batch, collection);
        }
    }

    /// Appends `chunk` to the buffer-managed collection of the current batch: the one of
    /// the last chunk, or a new one sharing its blocks (`BatchedDataCollection::Append`).
    fn append_batch(
        &mut self,
        collector: &PhysicalResultCollector,
        ctx: &ExecutionContext,
        chunk: &DataChunk,
    ) -> Result<()> {
        if self.collection_batch != Some(self.batch_index) {
            let previous = self.collection.take();
            let mut collection = match previous {
                Some((mut last, _)) => {
                    let next = last.share_allocator();
                    if let Some(last_batch) = self.collection_batch {
                        self.collections.insert(last_batch, last);
                    }
                    next
                }
                None => ColumnDataCollection::new(
                    ctx.env.buffer_manager.clone(),
                    collector.types.clone(),
                    MemoryTag::ColumnData,
                )?,
            };
            let mut append = ColumnDataAppendState::default();
            collection.initialize_append(&mut append)?;
            self.collection = Some((collection, append));
            self.collection_batch = Some(self.batch_index);
        }
        let Some((collection, append)) = self.collection.as_mut() else {
            unreachable!("a collection was just ensured")
        };
        collection.append(append, chunk)
    }
}

impl Sink for PhysicalResultCollector {
    type Global = Mutex<CollectedResult>;
    type Local = CollectorLocal;

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        let buffered = match self.kind {
            CollectorKind::Buffered { .. } => Some(Arc::new(BufferedData::simple(self.buffer_size))),
            CollectorKind::BufferedBatch => Some(Arc::new(BufferedData::batched(self.buffer_size))),
            CollectorKind::Materialized { .. } | CollectorKind::Batch => None,
        };
        Ok(Mutex::new(CollectedResult {
            buffered,
            ..CollectedResult::default()
        }))
    }

    fn local_sink_state(&self, ctx: &ExecutionContext, global: &Self::Global) -> Result<Self::Local> {
        let mut local = CollectorLocal {
            buffered: global.lock().map_err(|_| poisoned())?.buffered.clone(),
            ..CollectorLocal::default()
        };
        if self.buffer_managed() && matches!(self.kind, CollectorKind::Materialized { .. }) {
            // `MaterializedCollectorLocalState`: the collection's first chunk is allocated
            // right away.
            local.collection = Some(self.create_collection(ctx)?);
        }
        Ok(local)
    }

    fn sink(
        &self,
        ctx: &ExecutionContext,
        chunk: &DataChunk,
        _: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<SinkResult> {
        if let Some(buffered) = &local.buffered {
            let appended = match buffered.as_ref() {
                // `PhysicalBufferedCollector::Sink`.
                BufferedData::Simple(data) => data.sink(chunk, &ctx.interrupt_state),
                // `PhysicalBufferedBatchCollector::Sink`.
                BufferedData::Batched(data) => {
                    let batch = ctx.partition_info.batch_index;
                    local.current_batch = batch;
                    data.update_min_batch_index(ctx.partition_info.min_batch_index, &ctx.interrupt_state)?;
                    data.sink(chunk, batch, &ctx.interrupt_state)?
                }
            };
            return Ok(if appended {
                SinkResult::NeedMoreInput
            } else {
                SinkResult::Blocked
            });
        }
        if !self.buffer_managed() {
            local.chunks.push(chunk.compacted());
        } else if self.kind == CollectorKind::Batch {
            local.append_batch(self, ctx, chunk)?;
        } else if let Some((collection, append)) = local.collection.as_mut() {
            collection.append(append, chunk)?;
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn next_batch(
        &self,
        ctx: &ExecutionContext,
        _: &Self::Global,
        local: &mut Self::Local,
        partition: &PartitionInfo,
    ) -> Result<()> {
        if let Some(BufferedData::Batched(data)) = local.buffered.as_deref() {
            // `PhysicalBufferedBatchCollector::NextBatch`.
            data.complete_batch(local.current_batch);
            local.current_batch = partition.batch_index;
            return data.update_min_batch_index(partition.min_batch_index, &ctx.interrupt_state);
        }
        local.flush_batch();
        local.batch_index = partition.batch_index;
        Ok(())
    }

    fn combine(&self, ctx: &ExecutionContext, global: &Self::Global, mut local: Self::Local) -> Result<()> {
        if let Some(buffered) = &local.buffered {
            if let BufferedData::Batched(data) = buffered.as_ref() {
                // `PhysicalBufferedBatchCollector::Combine`.
                data.update_min_batch_index(ctx.partition_info.min_batch_index, &ctx.interrupt_state)?;
            }
            return Ok(());
        }
        if self.buffer_managed() {
            if self.kind == CollectorKind::Batch {
                // `BatchedDataCollection::Merge`.
                local.flush_collection();
                let mut global = global.lock().map_err(|_| poisoned())?;
                for (batch_index, collection) in local.collections {
                    if global.collections.insert(batch_index, collection).is_some() {
                        return Err(duplicate_batch_error(batch_index));
                    }
                }
                return Ok(());
            }
            let Some((collection, append)) = local.collection.take() else {
                return Ok(());
            };
            drop(append);
            if collection.count() == 0 {
                return Ok(());
            }
            let mut global = global.lock().map_err(|_| poisoned())?;
            match global.collection.as_mut() {
                Some(existing) => existing.combine(collection)?,
                None => global.collection = Some(collection),
            }
            return Ok(());
        }
        let mut global = global.lock().map_err(|_| poisoned())?;
        if self.kind != CollectorKind::Batch {
            global.chunks.append(&mut local.chunks);
            return Ok(());
        }
        local.flush_batch();
        for (batch_index, chunks) in local.batches {
            if global.batches.insert(batch_index, chunks).is_some() {
                return Err(duplicate_batch_error(batch_index));
            }
        }
        Ok(())
    }

    fn finalize(&self, ctx: &ExecutionContext, global: &mut Self::Global) -> Result<SinkFinalize> {
        let global = global.get_mut().map_err(|_| poisoned())?;
        if self.buffer_managed() {
            if self.kind == CollectorKind::Batch {
                // `BatchedDataCollection::FetchCollection`.
                let mut result: Option<ColumnDataCollection> = None;
                for (_, collection) in std::mem::take(&mut global.collections) {
                    match result.as_mut() {
                        Some(result) => result.combine(collection)?,
                        None => result = Some(collection),
                    }
                }
                global.collection = Some(match result {
                    Some(result) => result,
                    None => ColumnDataCollection::new(
                        ctx.env.buffer_manager.clone(),
                        self.types.clone(),
                        MemoryTag::ColumnData,
                    )?,
                });
            } else if global.collection.is_none() {
                // `PhysicalMaterializedCollector::GetResult` of an empty result.
                global.collection = Some(ColumnDataCollection::new(
                    ctx.env.buffer_manager.clone(),
                    self.types.clone(),
                    MemoryTag::ColumnData,
                )?);
            }
            return Ok(SinkFinalize::Ready);
        }
        for (_, mut chunks) in std::mem::take(&mut global.batches) {
            global.chunks.append(&mut chunks);
        }
        Ok(SinkFinalize::Ready)
    }
}

/// The default `streaming_buffer_size` (1 MB).
const DEFAULT_STREAMING_BUFFER_SIZE: usize = 1_000_000;

fn duplicate_batch_error(batch_index: u64) -> Error {
    Error::internal(format!(
        "BatchedDataCollection::Merge error - batch index {batch_index} is present in both collections. This occurs when batch indexes are not uniquely distributed over threads"
    ))
}
