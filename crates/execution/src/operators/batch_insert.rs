//! Parallel inserts that keep the input's order (DuckDB's `PhysicalBatchInsert`).

use std::collections::VecDeque;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roastedduck_common::{Error, Result};
use roastedduck_storage::{
    DataTable, OptimisticCollection, OptimisticDataWriter, TableConstraints, Transaction,
};
use roastedduck_types::{DataChunk, LogicalType};

use super::batch_memory_manager::BatchMemoryManager;
use super::insert::{InsertTarget, append_collections};
use super::modify::{ModifyResult, count_types};
use crate::operator::{
    ErasedSink, ErasedSource, OperatorPartitionInfo, PartitionInfo, PhysicalOperator, PhysicalRef, SinkState,
};
use crate::{ExecutionContext, Sink, SinkFinalize, SinkResult, Source, SourceResult};

/// Appends its input to a table in the order of the input's batch indices while every
/// thread appends on its own (DuckDB's `PhysicalBatchInsert`).
///
/// Each thread appends a batch to a collection of its own and hands it to the global
/// state when its batch ends. Consecutive small collections are merged into collections
/// of about a row group (by the sinking threads, as soon as every batch before them is
/// complete); `finalize` moves the collections into the transaction-local storage in batch
/// order. Complete row groups, and the collections of a row group or more, are written to
/// the database file by the optimistic writer of the thread that built them. The
/// collections not written yet count against the memory a `BatchMemoryManager` obtains;
/// when they take all of it, threads ahead of the lowest batch block until it moves on,
/// and the number of threads is capped by that memory (`MaxThreads`).
#[derive(Debug)]
pub struct PhysicalBatchInsert {
    types: Vec<LogicalType>,
    children: [PhysicalRef; 1],
    target: InsertTarget,
    transaction: Arc<Transaction>,
    constraints: TableConstraints,
}

impl PhysicalBatchInsert {
    pub fn new(
        child: PhysicalRef,
        target: InsertTarget,
        transaction: Arc<Transaction>,
        constraints: TableConstraints,
    ) -> Self {
        Self {
            types: count_types(),
            children: [child],
            target,
            transaction,
            constraints,
        }
    }
}

impl PhysicalOperator for PhysicalBatchInsert {
    fn name(&self) -> &'static str {
        match self.target {
            InsertTarget::Table(_) => "BATCH_INSERT",
            InsertTarget::Create(_) => "BATCH_CREATE_TABLE_AS",
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
        true
    }
    fn required_partition_info(&self) -> OperatorPartitionInfo {
        OperatorPartitionInfo::batch_index()
    }
}

/// One batch's rows, or several merged batches' (DuckDB's `RowGroupBatchEntry`).
#[derive(Debug)]
struct BatchEntry {
    batch_index: u64,
    total_rows: u64,
    /// The memory the rows take while not written (`unflushed_memory`).
    unflushed_memory: u64,
    /// `None` while a merge task holds the rows.
    collection: Option<OptimisticCollection>,
    /// Whether the rows are about a row group or more and no longer merged with others
    /// (DuckDB's `RowGroupBatchType::FLUSHED`).
    flushed: bool,
}

/// Collections to merge into one, which then replaces the entry of `merged_batch_index`
/// (DuckDB's `MergeCollectionTask`).
#[derive(Debug)]
struct MergeTask {
    collections: Vec<OptimisticCollection>,
    merged_batch_index: u64,
    /// The unflushed memory of the collections, released once they are written.
    unflushed_memory: u64,
}

#[derive(Debug, Default)]
struct BatchState {
    /// The collections handed over, by batch index.
    entries: Vec<BatchEntry>,
    /// The first entry that may still be merged (`next_start`).
    next_start: usize,
    /// Whether collections were merged while sinking (`optimistically_written`).
    merged: bool,
}

/// The global state of a batch insert.
#[derive(Debug)]
pub struct BatchInsertGlobal {
    table: Option<Arc<DataTable>>,
    row_group_size: u64,
    memory_manager: BatchMemoryManager,
    /// 4 MiB per column (`minimum_memory_per_thread`).
    minimum_memory_per_thread: u64,
    state: Mutex<BatchState>,
    tasks: Mutex<VecDeque<MergeTask>>,
    result: ModifyResult,
}

impl BatchInsertGlobal {
    fn state(&self) -> MutexGuard<'_, BatchState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `count` rows fill row groups well (`ReadyToMerge`).
    fn ready_to_merge(&self, count: u64) -> bool {
        let size = self.row_group_size;
        (count >= size / 10 * 9 && count <= size)
            || (count >= size / 10 * 18 && count <= size * 2)
            || (count >= size / 10 * 27 && count <= size * 3)
            || count >= size / 10 * 36
    }

    /// Hands a finalized batch collection over (`AddCollection`); from `next_batch`, with
    /// the thread's `writer`, a collection of a row group or more is written and merges of
    /// the complete batches are scheduled.
    fn add_collection(
        &self,
        batch_index: u64,
        min_batch_index: u64,
        mut collection: OptimisticCollection,
        writer: Option<&mut OptimisticDataWriter>,
    ) -> Result<()> {
        if batch_index < min_batch_index {
            return Err(Error::internal(format!(
                "Batch index of the added collection ({batch_index}) is smaller than the min batch index ({min_batch_index})"
            )));
        }
        let total_rows = collection.total_rows();
        let flushed = total_rows >= self.row_group_size;
        let schedule = writer.is_some();
        if flushed && let Some(writer) = writer {
            collection.write_unflushed_row_groups(writer)?;
        }
        let mut state = self.state();
        self.result.add_count(total_rows);
        let unflushed_memory = if flushed { 0 } else { collection.allocation_size() };
        if !flushed {
            self.memory_manager.increase_unflushed_memory(unflushed_memory);
        }
        let position = state
            .entries
            .partition_point(|entry| entry.batch_index < batch_index);
        if state
            .entries
            .get(position)
            .is_some_and(|entry| entry.batch_index == batch_index)
        {
            return Err(Error::internal(format!(
                "PhysicalBatchInsert::AddCollection error: batch index {batch_index} is present in multiple collections. This occurs when batch indexes are not uniquely distributed over threads"
            )));
        }
        state.entries.insert(
            position,
            BatchEntry {
                batch_index,
                total_rows,
                unflushed_memory,
                collection: Some(collection),
                flushed,
            },
        );
        if schedule {
            self.schedule_merge_tasks(&mut state, min_batch_index)?;
        }
        Ok(())
    }

    /// Turns runs of small collections of complete batches that together fill row groups
    /// into merge tasks (`ScheduleMergeTasks`).
    fn schedule_merge_tasks(&self, state: &mut BatchState, min_batch_index: u64) -> Result<()> {
        // (start, end, total rows) of each run to merge.
        let mut runs = Vec::new();
        let mut start = state.next_start;
        let mut total = 0;
        let mut index = start;
        while index < state.entries.len() {
            let entry = &state.entries[index];
            if entry.batch_index > min_batch_index {
                // Later batches may still arrive before this entry.
                if self.ready_to_merge(total) {
                    runs.push((start, index, total));
                }
                break;
            }
            if entry.flushed {
                if total > 0 {
                    runs.push((start, index, total));
                }
                start = index + 1;
                state.next_start = state.next_start.max(start);
                total = 0;
                index += 1;
                continue;
            }
            total += entry.total_rows;
            if self.ready_to_merge(total) {
                runs.push((start, index + 1, total));
                start = index + 1;
                total = 0;
            }
            index += 1;
        }
        if runs.is_empty() {
            return Ok(());
        }
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        for &(start, end, total) in &runs {
            let merged_batch_index = state.entries[start].batch_index;
            let mut collections = Vec::with_capacity(end - start);
            let mut unflushed_memory = 0;
            for entry in &mut state.entries[start..end] {
                if entry.flushed {
                    return Err(Error::internal(
                        "Adding a row group collection that should not be flushed",
                    ));
                }
                collections.push(
                    entry
                        .collection
                        .take()
                        .ok_or_else(|| Error::internal("batch insert collection merged twice"))?,
                );
                unflushed_memory += entry.unflushed_memory;
                entry.total_rows = total;
                entry.flushed = true;
                entry.unflushed_memory = 0;
            }
            tasks.push_back(MergeTask {
                collections,
                merged_batch_index,
                unflushed_memory,
            });
        }
        // Every merged run keeps only its first entry, which receives the merged rows.
        for &(start, end, _) in runs.iter().rev() {
            if start + 1 < end {
                state.entries.drain(start + 1..end);
            }
        }
        Ok(())
    }

    /// Runs queued merge tasks until there are none, writing the merged collections with
    /// the thread's `writer` (`ExecuteTasks`).
    fn execute_tasks(&self, writer: &mut OptimisticDataWriter) -> Result<()> {
        loop {
            let task = self
                .tasks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front();
            let Some(task) = task else { return Ok(()) };
            // `MergeCollections`: the written rows no longer count as unflushed.
            self.memory_manager
                .reduce_unflushed_memory(task.unflushed_memory)?;
            let merged = merge(task.collections, false, writer)?;
            let mut state = self.state();
            state.merged = true;
            let entry = state
                .entries
                .iter_mut()
                .find(|entry| entry.batch_index == task.merged_batch_index)
                .ok_or_else(|| Error::internal("Merged batch index was no longer present in collection"))?;
            entry.collection = merged;
        }
    }
}

/// Appends every collection after the first to the first and writes the rows of the
/// result not written yet with `writer`, unless it is a single collection that was
/// `flushed` already (`CollectionMerger::Flush`).
fn merge(
    collections: Vec<OptimisticCollection>,
    flushed: bool,
    writer: &mut OptimisticDataWriter,
) -> Result<Option<OptimisticCollection>> {
    let merge_count = collections.len();
    let mut collections = collections.into_iter();
    let Some(mut result) = collections.next() else {
        return Ok(None);
    };
    for collection in collections {
        result.append_collection(&collection, writer)?;
    }
    if merge_count > 1 || !flushed {
        result.write_unflushed_row_groups(writer)?;
    }
    Ok(Some(result))
}

/// A thread's state: the collection of the batch it is appending and the writer of the
/// row groups it completes.
#[derive(Debug, Default)]
pub struct BatchInsertLocal {
    current_index: u64,
    collection: Option<OptimisticCollection>,
    writer: Option<OptimisticDataWriter>,
}

impl BatchInsertLocal {
    /// The thread's optimistic writer, created on first use.
    fn writer(
        &mut self,
        table: &Arc<DataTable>,
        transaction: &Transaction,
    ) -> Result<&mut OptimisticDataWriter> {
        if self.writer.is_none() {
            self.writer = Some(table.create_optimistic_writer(transaction)?);
        }
        self.writer
            .as_mut()
            .ok_or_else(|| Error::internal("batch insert without an optimistic writer"))
    }
}

impl Sink for PhysicalBatchInsert {
    type Global = BatchInsertGlobal;
    type Local = BatchInsertLocal;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<Self::Global> {
        // Heuristic: 4 MiB of cache space per column to start with.
        const MINIMUM_MEMORY_PER_COLUMN: u64 = 4 * 1024 * 1024;
        let table = self.target.table(&self.transaction)?;
        let row_group_size = table
            .as_ref()
            .map_or(roastedduck_storage::DEFAULT_ROW_GROUP_SIZE, |table| {
                table.row_groups().row_group_size()
            });
        let column_count = table.as_ref().map_or(1, |table| table.types().len()) as u64;
        let minimum_memory_per_thread = column_count * MINIMUM_MEMORY_PER_COLUMN;
        Ok(BatchInsertGlobal {
            table,
            row_group_size,
            memory_manager: BatchMemoryManager::new(
                ctx.env.buffer_manager.clone(),
                ctx.env.threads(),
                minimum_memory_per_thread,
            ),
            minimum_memory_per_thread,
            state: Mutex::default(),
            tasks: Mutex::default(),
            result: ModifyResult::default(),
        })
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<Self::Local> {
        Ok(BatchInsertLocal::default())
    }

    fn next_batch(
        &self,
        _: &ExecutionContext,
        global: &Self::Global,
        local: &mut Self::Local,
        partition: &PartitionInfo,
    ) -> Result<()> {
        if let (Some(table), Some(mut collection)) = (&global.table, local.collection.take()) {
            if local.current_index == partition.batch_index {
                return Err(Error::internal("NextBatch called with the same batch index?"));
            }
            collection.finalize();
            let current_index = local.current_index;
            let writer = local.writer(table, &self.transaction)?;
            global.add_collection(
                current_index,
                partition.min_batch_index,
                collection,
                Some(&mut *writer),
            )?;
            if !global.memory_manager.unblock_tasks() {
                global.execute_tasks(writer)?;
            }
        }
        local.current_index = partition.batch_index;
        // Unblock any blocked tasks.
        global.memory_manager.unblock_tasks();
        Ok(())
    }

    fn max_threads(&self, global: &Self::Global, source_max: usize) -> usize {
        // Try to get 4 MiB per column per thread, and cap the threads by the memory
        // obtained.
        let manager = &global.memory_manager;
        manager.request_memory(source_max as u64 * global.minimum_memory_per_thread);
        source_max.min((manager.available_memory() / global.minimum_memory_per_thread) as usize + 1)
    }

    fn sink(
        &self,
        ctx: &ExecutionContext,
        chunk: &DataChunk,
        global: &Self::Global,
        local: &mut Self::Local,
    ) -> Result<SinkResult> {
        let Some(table) = &global.table else {
            return Ok(SinkResult::Finished);
        };
        let manager = &global.memory_manager;
        let batch_index = ctx.partition_info.batch_index;
        if !manager.is_minimum_batch_index(batch_index) {
            manager.update_min_batch_index(ctx.partition_info.min_batch_index);
            // Not the lowest batch: wait while the unflushed batches take all the memory.
            if manager.out_of_memory(batch_index) {
                // Merge collections while waiting, if there are any to merge.
                global.execute_tasks(local.writer(table, &self.transaction)?)?;
                if let Some(blocked) = manager.block_unless_minimum(batch_index, &ctx.interrupt_state) {
                    return Ok(blocked);
                }
            }
        }
        if local.current_index != batch_index {
            return Err(Error::internal(
                "Current batch differs from batch - but NextBatch was not called!?",
            ));
        }
        table.verify_append_constraints(&self.constraints, chunk)?;
        if local.collection.is_none() {
            local.collection = Some(table.create_optimistic_collection(&self.transaction)?);
        }
        local.writer(table, &self.transaction)?;
        if let (Some(collection), Some(writer)) = (&mut local.collection, &mut local.writer) {
            collection.append(chunk, writer)?;
        }
        Ok(SinkResult::NeedMoreInput)
    }

    fn combine(&self, ctx: &ExecutionContext, global: &Self::Global, local: Self::Local) -> Result<()> {
        let min_batch_index = ctx.partition_info.min_batch_index;
        global.memory_manager.update_min_batch_index(min_batch_index);
        if let Some(mut collection) = local.collection {
            collection.finalize();
            if collection.total_rows() > 0 {
                global.add_collection(local.current_index, min_batch_index, collection, None)?;
            }
        }
        // The thread's partially filled blocks go to the transaction's writer.
        if let (Some(table), Some(mut writer)) = (&global.table, local.writer) {
            let _merge = global.state();
            table.merge_optimistic_writer(&self.transaction, &mut writer)?;
        }
        // Unblock any blocked tasks.
        global.memory_manager.unblock_tasks();
        Ok(())
    }

    fn finalize(&self, _: &ExecutionContext, global: &mut Self::Global) -> Result<SinkFinalize> {
        let Some(table) = global.table.clone() else {
            return Ok(SinkFinalize::Ready);
        };
        let mut writer = table.create_optimistic_writer(&self.transaction)?;
        global.execute_tasks(&mut writer)?;
        let state = std::mem::take(&mut *global.state());
        let collections = state
            .entries
            .into_iter()
            .map(|entry| {
                // The remaining collections are written or appended below.
                global
                    .memory_manager
                    .reduce_unflushed_memory(entry.unflushed_memory)?;
                entry
                    .collection
                    .map(|collection| (entry.flushed, collection))
                    .ok_or_else(|| Error::internal("batch insert collection still being merged"))
            })
            .collect::<Result<Vec<_>>>()?;
        global.memory_manager.final_check()?;
        if state.merged || global.result.count() >= global.row_group_size {
            // Merge the runs of small collections, then move every collection into the
            // local storage in batch order.
            let mut finals = Vec::new();
            let mut small = Vec::new();
            for (flushed, collection) in collections {
                if flushed {
                    finals.extend(merge(std::mem::take(&mut small), false, &mut writer)?);
                    finals.extend(merge(vec![collection], true, &mut writer)?);
                } else {
                    small.push(collection);
                }
            }
            finals.extend(merge(small, false, &mut writer)?);
            for collection in &finals {
                table.local_merge(&self.transaction, collection)?;
            }
            table.merge_optimistic_writer(&self.transaction, &mut writer)?;
            table.final_flush_optimistic_writer(&self.transaction)?;
        } else {
            // Few rows: append them to the local storage directly.
            append_collections(
                &table,
                &self.transaction,
                &self.constraints,
                collections.iter().map(|(_, collection)| collection),
            )?;
        }
        Ok(SinkFinalize::Ready)
    }
}

impl Source for PhysicalBatchInsert {
    type Global = (SinkState, AtomicUsize);
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        let sink = sink.ok_or_else(|| Error::internal("batch insert source requires its sink state"))?;
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
            .downcast_ref::<BatchInsertGlobal>()
            .ok_or_else(|| Error::internal("batch insert sink state has the wrong type"))?;
        global.result.emit(false, next, chunk)?;
        Ok(SourceResult::Finished)
    }
}
