//! The hash join's source role (DuckDB's `HashJoinGlobalSourceState` and
//! `HashJoinLocalSourceState`): after the probe pipeline, RIGHT/FULL joins scan the
//! table for unmatched build rows, and an external join builds the remaining partitions
//! round by round, probing each round's spilled probe rows.
//!
//! The work of each stage (`BUILD`, `PROBE`, `SCAN_HT`) is split into tasks that threads claim;
//! the thread that sees a stage complete prepares the next one, and threads without work
//! wait for that.

use std::ops::Range;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use roastedduck_common::{Error, Result};
use roastedduck_row::{ParallelScanState, PinProperties, ScanState, TupleDataCollection};
use roastedduck_types::{DataChunk, STANDARD_VECTOR_SIZE};

use super::super::{construct_empty_join_result, empty_result_if_rhs_is_empty, propagates_build_side};
use super::{HashJoinGlobalSinkState, PhysicalHashJoin};
use crate::join_hashtable::{
    FullOuterScanState, JoinHashTable, ProbeState, ScanStructure, flatten_sequences,
};
use crate::operator::SinkState;
use crate::{ExecutionContext, ExpressionExecutor, Source, SourceResult};

/// Build chunks one task inserts (DuckDB's `HashJoinFinalizeEvent::CHUNKS_PER_TASK`).
const CHUNKS_PER_TASK: usize = 64;
/// Probe input rows per thread when sizing the source (`parallel_scan_chunk_count`).
const PARALLEL_SCAN_CHUNK_COUNT: usize = 120;

/// The stage of the source (DuckDB's `HashJoinSourceStage`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Init,
    Build,
    Probe,
    ScanHt,
    Done,
}

/// The spilled probe rows of the current round, read by several threads.
#[derive(Debug)]
struct ProbeRound {
    rows: TupleDataCollection,
    scan: ParallelScanState,
}

/// The progress of the current stage, guarded by the global source state's lock.
#[derive(Debug)]
struct Progress {
    stage: Stage,
    build_next: usize,
    build_count: usize,
    build_done: usize,
    probe: Option<Arc<ProbeRound>>,
    probe_count: usize,
    probe_done: usize,
    full_outer_next: usize,
    full_outer_count: usize,
    full_outer_done: usize,
    full_outer_per_task: usize,
}

/// The shared source state (DuckDB's `HashJoinGlobalSourceState`).
#[derive(Debug)]
pub struct HashJoinGlobalSourceState {
    sink: SinkState,
    progress: Mutex<Progress>,
    /// Signalled when a stage's progress changes, waking threads without work.
    changed: Condvar,
    threads: usize,
}

/// The work a thread claimed.
#[derive(Debug)]
enum Task {
    Build(Range<usize>),
    Probe,
    /// A scan for unmatched rows, and the number of chunks it covers.
    ScanHt(Box<FullOuterScanState>, usize),
}

/// One thread's source state (DuckDB's `HashJoinLocalSourceState`).
#[derive(Debug, Default)]
pub struct HashJoinLocalSourceState {
    task: Option<Task>,
    /// The probe round `round_scan` reads.
    round: Option<Arc<ProbeRound>>,
    round_scan: Option<ScanState>,
    probe_executor: Option<ExpressionExecutor>,
    probe_chunk: DataChunk,
    join_keys: DataChunk,
    scan_structure: ScanStructure,
    probe_state: ProbeState,
    empty_ht_probe_in_progress: bool,
}

fn lock(progress: &Mutex<Progress>) -> MutexGuard<'_, Progress> {
    progress.lock().unwrap_or_else(PoisonError::into_inner)
}

impl HashJoinGlobalSourceState {
    fn sink(&self) -> Result<&HashJoinGlobalSinkState> {
        super::super::sink_state::<HashJoinGlobalSinkState>(&self.sink, "hash join")
    }
}

impl PhysicalHashJoin {
    /// Starts the source: merges the probe spill and prepares the first stage
    /// (`Initialize`).
    fn initialize_source(&self, ctx: &ExecutionContext, global: &HashJoinGlobalSourceState) -> Result<()> {
        let mut progress = lock(&global.progress);
        if progress.stage != Stage::Init {
            return Ok(());
        }
        let sink = global.sink()?;
        if let Some(spill) = sink.probe_spill.get() {
            spill.finalize()?;
        }
        // The spilled rows of the first round's partitions were probed by the operator.
        progress.stage = Stage::Probe;
        progress.probe_count = 0;
        progress.probe_done = 0;
        self.try_prepare_next_stage(ctx, global, &mut progress)?;
        global.changed.notify_all();
        Ok(())
    }

    /// Moves on to the next stage if the current one is complete (`TryPrepareNextStage`).
    fn try_prepare_next_stage(
        &self,
        ctx: &ExecutionContext,
        global: &HashJoinGlobalSourceState,
        progress: &mut Progress,
    ) -> Result<bool> {
        let sink = global.sink()?;
        match progress.stage {
            Stage::Build if progress.build_done == progress.build_count => {
                {
                    let mut table = sink.hash_table.write().unwrap_or_else(PoisonError::into_inner);
                    table.finish_parallel_finalize(
                        sink.table_finalize
                            .chains_longer_than_one
                            .swap(false, std::sync::atomic::Ordering::AcqRel),
                    );
                    table.mark_finalized();
                }
                self.prepare_probe(ctx, global, progress)?;
                Ok(true)
            }
            Stage::Probe if progress.probe_done == progress.probe_count => {
                if propagates_build_side(self.join_type) {
                    Self::prepare_scan_ht(global, progress)?;
                } else {
                    self.prepare_build(ctx, global, progress)?;
                }
                Ok(true)
            }
            Stage::ScanHt if progress.full_outer_done == progress.full_outer_count => {
                self.prepare_build(ctx, global, progress)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Puts the next partitions that fit in memory into the table (`PrepareBuild`).
    fn prepare_build(
        &self,
        ctx: &ExecutionContext,
        global: &HashJoinGlobalSourceState,
        progress: &mut Progress,
    ) -> Result<()> {
        let sink = global.sink()?;
        let external = sink.external.load(std::sync::atomic::Ordering::Acquire);
        let mut table = sink.hash_table.write().unwrap_or_else(PoisonError::into_inner);
        loop {
            let probe_side = sink.probe_side_requirement as u64;
            sink.temporary_memory.set_remaining_size_and_update_reservation(
                &sink.memory_context,
                table.remaining_size() as u64 + probe_side,
            );
            let budget = usize::try_from(sink.temporary_memory.reservation().saturating_sub(probe_side))
                .unwrap_or(usize::MAX);
            if !external || !table.prepare_external_finalize(budget)? {
                progress.stage = Stage::Done;
                sink.temporary_memory.set_zero();
                return Ok(());
            }
            // An empty round of a join without output for empty build sides is skipped.
            if table.count() > 0 || !empty_result_if_rhs_is_empty(self.join_type) {
                break;
            }
        }
        let chunks = table.data_collection().chunk_count();
        progress.build_next = 0;
        progress.build_count = chunks;
        progress.build_done = 0;
        table.allocate_pointer_table()?;
        table.initialize_pointer_table(0, table.capacity());
        drop(table);
        progress.stage = Stage::Build;
        if chunks == 0 {
            self.try_prepare_next_stage(ctx, global, progress)?;
        }
        Ok(())
    }

    /// Takes the spilled probe rows of the round's partitions (`PrepareProbe`).
    fn prepare_probe(
        &self,
        ctx: &ExecutionContext,
        global: &HashJoinGlobalSourceState,
        progress: &mut Progress,
    ) -> Result<()> {
        let sink = global.sink()?;
        let spill = sink
            .probe_spill
            .get()
            .ok_or_else(|| Error::internal("external hash join without a probe spill"))?;
        let rows = {
            let table = sink.hash_table.read().unwrap_or_else(PoisonError::into_inner);
            spill.prepare_next_probe(table.current_partitions())?
        };
        progress.probe_count = rows.chunk_count();
        progress.probe_done = 0;
        let scan = rows.initialize_parallel_scan(None, PinProperties::UnpinAfterDone);
        progress.probe = Some(Arc::new(ProbeRound { rows, scan }));
        progress.stage = Stage::Probe;
        if progress.probe_count == 0 {
            self.try_prepare_next_stage(ctx, global, progress)?;
        }
        Ok(())
    }

    /// Prepares the scan for unmatched build rows (`PrepareScanHT`).
    fn prepare_scan_ht(global: &HashJoinGlobalSourceState, progress: &mut Progress) -> Result<()> {
        let sink = global.sink()?;
        let chunks = sink
            .hash_table
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .data_collection()
            .chunk_count();
        progress.full_outer_next = 0;
        progress.full_outer_count = chunks;
        progress.full_outer_done = 0;
        progress.full_outer_per_task = chunks.div_ceil(global.threads).max(1);
        progress.stage = Stage::ScanHt;
        Ok(())
    }

    /// Claims work of the current stage for `local` (`AssignTask`).
    fn assign_task(global: &HashJoinGlobalSourceState, local: &mut HashJoinLocalSourceState) -> Result<bool> {
        let round = {
            let mut progress = lock(&global.progress);
            match progress.stage {
                Stage::Build if progress.build_next != progress.build_count => {
                    let from = progress.build_next;
                    progress.build_next = (from + CHUNKS_PER_TASK).min(progress.build_count);
                    local.task = Some(Task::Build(from..progress.build_next));
                    return Ok(true);
                }
                Stage::ScanHt if progress.full_outer_next != progress.full_outer_count => {
                    let from = progress.full_outer_next;
                    progress.full_outer_next =
                        (from + progress.full_outer_per_task).min(progress.full_outer_count);
                    let to = progress.full_outer_next;
                    drop(progress);
                    let sink = global.sink()?;
                    let table = sink.hash_table.read().unwrap_or_else(PoisonError::into_inner);
                    local.task = Some(Task::ScanHt(
                        Box::new(table.full_outer_scan_state(from..to)),
                        to - from,
                    ));
                    return Ok(true);
                }
                Stage::Probe => progress.probe.clone(),
                _ => None,
            }
        };
        // Probe chunks are claimed from the round's parallel scan, outside the lock.
        let Some(round) = round else {
            return Ok(false);
        };
        if !local
            .round
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &round))
        {
            local.round_scan = Some(round.rows.initialize_local_scan(&round.scan));
            local.round = Some(Arc::clone(&round));
        }
        let scan = local
            .round_scan
            .as_mut()
            .ok_or_else(|| Error::internal("hash join probe round without a scan"))?;
        if round
            .rows
            .parallel_scan(&round.scan, scan, &mut local.probe_chunk)?
        {
            local.task = Some(Task::Probe);
            local.empty_ht_probe_in_progress = false;
            return Ok(true);
        }
        Ok(false)
    }

    /// Does (part of) the claimed work, possibly producing output (`ExecuteTask`).
    fn execute_task(
        &self,
        global: &HashJoinGlobalSourceState,
        local: &mut HashJoinLocalSourceState,
        chunk: &mut DataChunk,
    ) -> Result<()> {
        let sink = global.sink()?;
        match local.task.take() {
            Some(Task::Build(range)) => {
                let len = range.len();
                {
                    let table = sink.hash_table.read().unwrap_or_else(PoisonError::into_inner);
                    if table.insert_chunks(range, true)? {
                        sink.table_finalize
                            .chains_longer_than_one
                            .store(true, std::sync::atomic::Ordering::Release);
                    }
                }
                lock(&global.progress).build_done += len;
                global.changed.notify_all();
            }
            Some(Task::Probe) => {
                let table = sink.hash_table.read().unwrap_or_else(PoisonError::into_inner);
                if self.external_probe(&table, local, chunk)? {
                    local.task = Some(Task::Probe);
                } else {
                    drop(table);
                    lock(&global.progress).probe_done += 1;
                    global.changed.notify_all();
                }
            }
            Some(Task::ScanHt(mut scan, chunks)) => {
                {
                    let table = sink.hash_table.read().unwrap_or_else(PoisonError::into_inner);
                    table.scan_full_outer(&mut scan, &self.types, chunk)?;
                }
                if chunk.is_empty() {
                    lock(&global.progress).full_outer_done += chunks;
                    global.changed.notify_all();
                } else {
                    local.task = Some(Task::ScanHt(scan, chunks));
                }
            }
            None => {}
        }
        Ok(())
    }

    /// Probes the claimed spilled chunk; returns whether the chunk still has output
    /// (`ExternalProbe`).
    fn external_probe(
        &self,
        table: &JoinHashTable,
        local: &mut HashJoinLocalSourceState,
        chunk: &mut DataChunk,
    ) -> Result<bool> {
        let input_width = self.children[0].types().len();
        let probe_input = DataChunk::new(
            local.probe_chunk.columns()[..input_width].to_vec(),
            local.probe_chunk.len(),
        );
        let lhs_output = self.lhs_output(&probe_input);
        if !local.scan_structure.is_null {
            // Output left over from the previous call.
            local
                .scan_structure
                .next(table, &local.join_keys, &lhs_output, &self.types, chunk)?;
            if !chunk.is_empty() || !local.scan_structure.pointers_exhausted() {
                return Ok(true);
            }
        }
        if !local.scan_structure.is_null || local.empty_ht_probe_in_progress {
            // The chunk is done.
            local.scan_structure.is_null = true;
            local.empty_ht_probe_in_progress = false;
            return Ok(false);
        }
        let executor = local
            .probe_executor
            .get_or_insert_with(|| ExpressionExecutor::new(self.left_expressions()));
        executor.execute(&probe_input, &mut local.join_keys)?;
        flatten_sequences(&mut local.join_keys);
        if table.count() == 0 && !empty_result_if_rhs_is_empty(self.join_type) {
            let right_types = &self.types[self.lhs_output_columns.len().min(self.types.len())..];
            construct_empty_join_result(self.join_type, table.has_null, &lhs_output, right_types, chunk)?;
            local.empty_ht_probe_in_progress = true;
            return Ok(true);
        }
        let hashes = local
            .probe_chunk
            .columns()
            .last()
            .ok_or_else(|| Error::internal("spilled probe rows without hashes"))?
            .clone();
        table.probe(
            &mut local.scan_structure,
            &local.join_keys,
            &mut local.probe_state,
            Some(&hashes),
        )?;
        local
            .scan_structure
            .next(table, &local.join_keys, &lhs_output, &self.types, chunk)?;
        Ok(true)
    }
}

impl Source for PhysicalHashJoin {
    type Global = HashJoinGlobalSourceState;
    type Local = HashJoinLocalSourceState;

    fn global_source_state(
        &self,
        ctx: &ExecutionContext,
        sink: Option<&SinkState>,
    ) -> Result<HashJoinGlobalSourceState> {
        let sink = sink.ok_or_else(|| Error::internal("hash join source requires its sink state"))?;
        Ok(HashJoinGlobalSourceState {
            sink: Arc::clone(sink),
            progress: Mutex::new(Progress {
                stage: Stage::Init,
                build_next: 0,
                build_count: 0,
                build_done: 0,
                probe: None,
                probe_count: 0,
                probe_done: 0,
                full_outer_next: 0,
                full_outer_count: 0,
                full_outer_done: 0,
                full_outer_per_task: 1,
            }),
            changed: Condvar::new(),
            threads: ctx.env.threads().max(1),
        })
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &HashJoinGlobalSourceState) -> Result<Self::Local> {
        Ok(HashJoinLocalSourceState {
            scan_structure: ScanStructure::default(),
            ..HashJoinLocalSourceState::default()
        })
    }

    /// Produces output until the chunk has rows or every stage is done (`GetData`):
    /// threads claim the current stage's work, the thread that completes a stage prepares
    /// the next, and threads without work wait for that.
    fn get_data(
        &self,
        ctx: &ExecutionContext,
        chunk: &mut DataChunk,
        global: &HashJoinGlobalSourceState,
        local: &mut HashJoinLocalSourceState,
    ) -> Result<SourceResult> {
        let sink = global.sink()?;
        if !sink.external.load(std::sync::atomic::Ordering::Acquire) && !propagates_build_side(self.join_type)
        {
            return Ok(SourceResult::Finished);
        }
        self.initialize_source(ctx, global)?;
        chunk.set_len(0);
        while chunk.is_empty() {
            ctx.check_interrupted()?;
            if local.task.is_some() || Self::assign_task(global, local)? {
                self.execute_task(global, local, chunk)?;
                continue;
            }
            let mut progress = lock(&global.progress);
            if progress.stage == Stage::Done {
                break;
            }
            if self.try_prepare_next_stage(ctx, global, &mut progress)? {
                global.changed.notify_all();
                continue;
            }
            // Other threads still work on this stage: wait for them.
            drop(
                global
                    .changed
                    .wait(progress)
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
        Ok(if chunk.is_empty() {
            SourceResult::Finished
        } else {
            SourceResult::HaveMoreOutput
        })
    }

    /// One thread per batch of probe chunks of an external join, or per batch of build
    /// rows to scan for unmatched rows (`MaxThreads`).
    fn max_threads(&self, global: &HashJoinGlobalSourceState) -> usize {
        let Ok(sink) = global.sink() else {
            return 1;
        };
        let count = if sink.external.load(std::sync::atomic::Ordering::Acquire) {
            return global.threads;
        } else if propagates_build_side(self.join_type) {
            sink.hash_table
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .count()
        } else {
            return 1;
        };
        (count / (STANDARD_VECTOR_SIZE * PARALLEL_SCAN_CHUNK_COUNT)).max(1)
    }
}
