use std::collections::BTreeMap;
use std::sync::Mutex;

use rustdb_common::{Error, Result};
use rustdb_buffer::MemoryTag;
use rustdb_row::{ColumnDataAppendState, ColumnDataCollection};
use rustdb_types::{DataChunk, LogicalType};

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
}

/// Collects the final query result (DuckDB's `PhysicalMaterializedCollector` and
/// `PhysicalBatchCollector`).
#[derive(Debug)]
pub struct PhysicalResultCollector {
    types: Vec<LogicalType>,
    kind: CollectorKind,
    /// Where a materialized result keeps its rows (`PhysicalResultCollector::memory_type`).
    memory_type: QueryResultMemoryType,
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
    /// `ALLOW_STREAMING`): parallel when the plan's order does not matter, otherwise the
    /// final pipeline runs on one thread so that rows arrive in order (DuckDB's
    /// `PhysicalBufferedBatchCollector` reorders the batches of several threads instead).
    pub fn streaming(plan: PhysicalRef, preserve_order_setting: bool) -> Self {
        let parallel = !preserve_insertion_order(plan.as_ref(), preserve_order_setting);
        Self::with_kind(plan, CollectorKind::Buffered { parallel })
    }

    pub fn kind(&self) -> CollectorKind {
        self.kind
    }

    /// Whether the rows go to buffer-managed collections (`CreateCollection` with
    /// `BUFFER_MANAGED`). Results without columns stay on the heap.
    fn buffer_managed(&self) -> bool {
        self.memory_type == QueryResultMemoryType::BufferManaged
            && !self.types.is_empty()
            && !matches!(self.kind, CollectorKind::Buffered { .. })
    }

    /// A new buffer-managed collection for the result's rows, ready for appending.
    fn create_collection(&self, ctx: &ExecutionContext) -> Result<(ColumnDataCollection, ColumnDataAppendState)> {
        let mut collection = ColumnDataCollection::new(ctx.env.buffer_manager.clone(), self.types.clone(), MemoryTag::ColumnData)?;
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
            CollectorKind::Batch => true,
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
            batch_index: self.kind == CollectorKind::Batch,
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
                None => ColumnDataCollection::new(ctx.env.buffer_manager.clone(), collector.types.clone(), MemoryTag::ColumnData)?,
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
        Ok(Mutex::default())
    }

    fn local_sink_state(&self, ctx: &ExecutionContext, _: &Self::Global) -> Result<Self::Local> {
        let mut local = CollectorLocal::default();
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
        global: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<SinkResult> {
        if let CollectorKind::Buffered { .. } = self.kind {
            global
                .lock()
                .map_err(|_| poisoned())?
                .chunks
                .push(chunk.compacted());
        } else if !self.buffer_managed() {
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
        _: &ExecutionContext,
        _: &Self::Global,
        local: &mut Self::Local,
        partition: &PartitionInfo,
    ) -> Result<()> {
        local.flush_batch();
        local.batch_index = partition.batch_index;
        Ok(())
    }

    fn combine(&self, _: &ExecutionContext, global: &Self::Global, mut local: Self::Local) -> Result<()> {
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
                    None => ColumnDataCollection::new(ctx.env.buffer_manager.clone(), self.types.clone(), MemoryTag::ColumnData)?,
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

fn duplicate_batch_error(batch_index: u64) -> Error {
    Error::internal(format!(
        "BatchedDataCollection::Merge error - batch index {batch_index} is present in both collections. This occurs when batch indexes are not uniquely distributed over threads"
    ))
}
