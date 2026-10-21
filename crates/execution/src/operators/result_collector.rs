use std::collections::BTreeMap;
use std::sync::Mutex;

use rustdb_common::{Error, Result};
use rustdb_types::{DataChunk, LogicalType};

use crate::operator::{ErasedSink, PartitionInfo, PhysicalOperator, PhysicalRef, SinkState};
use crate::plan_generator::order_preservation::{preserve_insertion_order, use_batch_index};
use crate::{ExecutionContext, Sink, SinkFinalize, SinkResult};

/// The materialised output of a query.
#[derive(Debug, Default)]
pub struct CollectedResult {
    pub chunks: Vec<DataChunk>,
    /// The batches of a batch collector, until `finalize` concatenates them in order.
    batches: BTreeMap<u64, Vec<DataChunk>>,
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
            children: [child],
        }
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
    fn requires_batch_index(&self) -> bool {
        self.kind == CollectorKind::Batch
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
}

impl Sink for PhysicalResultCollector {
    type Global = Mutex<CollectedResult>;
    type Local = CollectorLocal;

    fn global_sink_state(&self, _: &ExecutionContext) -> Result<Self::Global> {
        Ok(Mutex::default())
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<Self::Local> {
        Ok(CollectorLocal::default())
    }

    fn sink(
        &self,
        _: &ExecutionContext,
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
        } else {
            local.chunks.push(chunk.compacted());
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn next_batch(
        &self,
        _: &ExecutionContext,
        _: &Self::Global,
        local: &mut Self::Local,
        partition: PartitionInfo,
    ) -> Result<()> {
        local.flush_batch();
        local.batch_index = partition.batch_index;
        Ok(())
    }

    fn combine(&self, _: &ExecutionContext, global: &Self::Global, mut local: Self::Local) -> Result<()> {
        let mut global = global.lock().map_err(|_| poisoned())?;
        if self.kind != CollectorKind::Batch {
            global.chunks.append(&mut local.chunks);
            return Ok(());
        }
        local.flush_batch();
        for (batch_index, chunks) in local.batches {
            if global.batches.insert(batch_index, chunks).is_some() {
                return Err(Error::internal(format!(
                    "BatchedDataCollection::Merge error - batch index {batch_index} is present in both collections. This occurs when batch indexes are not uniquely distributed over threads"
                )));
            }
        }
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut Self::Global) -> Result<SinkFinalize> {
        let global = global.get_mut().map_err(|_| poisoned())?;
        for (_, mut chunks) in std::mem::take(&mut global.batches) {
            global.chunks.append(&mut chunks);
        }
        Ok(SinkFinalize::Ready)
    }
}
