//! `DISTINCT` aggregates over arbitrary frames (DuckDB's `window_distinct_aggregator.cpp`).
//!
//! Each row points to the previous row with the same arguments ("prevIdcs"). A value is
//! the first occurrence within a frame `[lower, upper)` iff its previous occurrence is
//! before `lower`. A merge sort tree over the pointers, with a running aggregate per
//! sorted run, answers each frame by combining `O(log n)` states (Vogelsgesang et al.,
//! SIGMOD 2022, algorithm 2).

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::StatePtr;
use rustdb_row::sort::{NullOrder, OrderModifiers, OrderType as SortOrder, Sort, SortColumn, SortConfig};
use rustdb_types::{DataChunk, LogicalType, SelectionVector, Vector};

use super::aggregate_states::{ScratchStates, WindowAggregateStates};
use super::aggregator::{WindowAggregator, evaluate_subframes, init_subframes};
use super::boundaries_state::{FrameBounds, WindowBounds};
use super::collection::delta_flags;
use super::merge_sort_tree::{Level, MergeSortTree};
use crate::ExecutionContext;

/// Whether the distinct strategy applies (DuckDB's `CanAggregate`).
pub(crate) fn can_aggregate(aggregator: &WindowAggregator, has_arg_orders: bool) -> bool {
    aggregator.function.ops.can_aggregate()
        && aggregator.distinct
        && aggregator.exclude_mode == rustdb_ast::WindowExcludeMode::NoOther
        && !has_arg_orders
}

/// The sort of `(arguments, row)` pairs feeding the previous-occurrence pointers.
#[derive(Debug)]
pub(crate) struct DistinctSink {
    sort: Sort,
    global: rustdb_row::sort::SortGlobalSinkState,
    local: rustdb_row::sort::SortLocalSinkState,
    key_count: usize,
}

impl DistinctSink {
    pub(crate) fn new(ctx: &ExecutionContext, aggregator: &WindowAggregator) -> Result<Self> {
        // Sort by the arguments, with the row index as a tie-breaker.
        let mut types = aggregator.arg_types.clone();
        types.push(LogicalType::UBIGINT);
        let orders = (0..types.len())
            .map(|column| SortColumn {
                column,
                modifiers: OrderModifiers::new(SortOrder::Ascending, NullOrder::NullsFirst),
                stats: None,
            })
            .collect();
        let sort = Sort::new(ctx.env.buffer_manager.clone(), orders, &types, None)?;
        let global = sort.global_sink_state(SortConfig {
            threads: 1,
            connections: 1,
            force_external: ctx.force_external(),
            ..SortConfig::default()
        });
        let local = sort.local_sink_state();
        Ok(Self {
            sort,
            global,
            local,
            key_count: aggregator.arg_types.len(),
        })
    }

    /// Adds the arguments of a chunk starting at `input_idx`, or of the `filter` rows.
    pub(crate) fn sink(
        &mut self,
        aggregator: &WindowAggregator,
        coll_chunk: &DataChunk,
        input_idx: usize,
        filter: Option<&[u32]>,
    ) -> Result<()> {
        let count = coll_chunk.len();
        let mut columns: Vec<Vector> = aggregator
            .child_idx
            .iter()
            .map(|&column| coll_chunk.column(column).clone())
            .collect();
        let mut indices = Vector::flat(LogicalType::UBIGINT, count);
        for (i, index) in indices.values_mut::<u64>()[..count].iter_mut().enumerate() {
            *index = (input_idx + i) as u64;
        }
        columns.push(indices);
        let chunk = match filter {
            Some(rows) => {
                let sel = SelectionVector::new(rows.to_vec());
                DataChunk::new(
                    columns.iter().map(|column| column.slice(&sel)).collect(),
                    rows.len(),
                )
            }
            None => DataChunk::new(columns, count),
        };
        if chunk.is_empty() {
            return Ok(());
        }
        self.sort.sink(&chunk, &self.global, &mut self.local)
    }
}

/// The merge sort tree with running aggregates of one hash group.
#[derive(Debug)]
pub(crate) struct DistinctGlobalState {
    /// The previous-occurrence pointers (plus one; 0 for none), sorted per run.
    merge_sort_tree: MergeSortTree<u64, u64>,
    /// One running aggregate per element per level.
    levels_flat_native: WindowAggregateStates,
    levels_flat_start: Vec<usize>,
}

/// Pending updates and combines, applied updates first like DuckDB's `BuildRun`.
#[derive(Default)]
struct BuildBuffer {
    update_targets: Vec<StatePtr>,
    update_rows: Vec<u32>,
    combine_sources: Vec<StatePtr>,
    combine_targets: Vec<StatePtr>,
}

impl BuildBuffer {
    fn flush(&mut self, aggregator: &WindowAggregator, inputs: &[Vector]) -> Result<()> {
        let ops = &aggregator.function.ops;
        if !self.update_targets.is_empty() {
            let sel = SelectionVector::new(std::mem::take(&mut self.update_rows));
            let leaves: Vec<Vector> = inputs.iter().map(|input| input.slice(&sel)).collect();
            // SAFETY: the targets are initialised tree states owned by the caller.
            unsafe {
                ops.update(
                    &leaves,
                    aggregator.input(),
                    &self.update_targets,
                    self.update_targets.len(),
                )?;
            }
            self.update_targets.clear();
        }
        if !self.combine_targets.is_empty() {
            // SAFETY: sources and targets are distinct initialised tree states.
            unsafe { ops.combine(&self.combine_sources, &self.combine_targets, aggregator.input())? };
            self.combine_sources.clear();
            self.combine_targets.clear();
        }
        Ok(())
    }
}

impl DistinctGlobalState {
    /// Sorts the arguments, computes the previous-occurrence pointers and builds the tree
    /// with its running aggregates (DuckDB's `Finalize`, `Sorted` and `BuildRun`).
    pub(crate) fn build(
        aggregator: &WindowAggregator,
        sink: DistinctSink,
        inputs: &[Vector],
        group_count: usize,
    ) -> Result<Self> {
        let DistinctSink {
            sort,
            global,
            local,
            key_count,
        } = sink;
        sort.combine(&global, local)?;
        let sorted = match sort.finalize(global)? {
            Some(source) => source.collect()?,
            None => Vec::new(),
        };

        // prevIdcs: (previous occurrence + 1 or 0, row). Filtered rows point to themselves
        // so they are never counted.
        let mut zipped: MergeSortTree<(u64, u64), u64> = MergeSortTree::default();
        let prev_idcs = zipped.allocate(group_count);
        for (i, entry) in prev_idcs.iter_mut().enumerate() {
            *entry = (i as u64 + 1, i as u64);
        }
        let sorted_count: usize = sorted.iter().map(DataChunk::len).sum();
        let mut distinct = vec![false; sorted_count];
        let keys: Vec<Vec<&Vector>> = sorted
            .iter()
            .map(|chunk| chunk.columns()[..key_count].iter().collect())
            .collect();
        let lens: Vec<usize> = sorted.iter().map(DataChunk::len).collect();
        delta_flags(&keys, &lens, &mut distinct)?;
        let mut position = 0;
        let mut previous: Option<u64> = None;
        for chunk in &sorted {
            let indices = chunk
                .columns()
                .last()
                .ok_or_else(|| Error::internal("distinct sort without row index"))?
                .flattened(chunk.len());
            for &row in &indices.values::<u64>()[..chunk.len()] {
                let entry = match previous {
                    Some(prev) if !distinct[position] => (prev + 1, row),
                    _ => (0, row),
                };
                zipped.lowest_level_mut()[row as usize] = entry;
                previous = Some(row);
                position += 1;
            }
        }
        zipped.build();

        // One running aggregate per element per level.
        let mut levels_flat_start = vec![0];
        let mut internal_nodes = 0;
        for level in &zipped.tree {
            internal_nodes += level.elements.len();
            levels_flat_start.push(internal_nodes);
        }
        let levels_flat_native = WindowAggregateStates::new(aggregator.function.ops.clone(), internal_nodes)?;

        let mut merge_sort_tree: MergeSortTree<u64, u64> = MergeSortTree::default();
        let mut buffer = BuildBuffer::default();
        let mut run_length = 1;
        for (level_nr, zipped_level) in zipped.tree.iter_mut().enumerate() {
            let size = zipped_level.elements.len();
            // The tree only keeps the previous-occurrence pointers.
            let level: Vec<u64> = zipped_level
                .elements
                .iter()
                .map(|&(prev_idx, _)| prev_idx)
                .collect();
            for run_start in (0..size).step_by(run_length.max(1)) {
                let mut prev_state: Option<StatePtr> = None;
                let limit = size.min(run_start + run_length);
                for (j, &(prev_idx, update_idx)) in zipped_level
                    .elements
                    .iter()
                    .enumerate()
                    .take(limit)
                    .skip(run_start)
                {
                    let curr_state = levels_flat_native.state(level_nr * size + j);
                    // Count the value if its previous occurrence is before the run.
                    if prev_idx < run_start as u64 + 1 {
                        buffer.update_targets.push(curr_state);
                        buffer.update_rows.push(update_idx as u32);
                    }
                    // Running aggregate: merge the previous element's state.
                    if let Some(prev) = prev_state {
                        buffer.combine_sources.push(prev);
                        buffer.combine_targets.push(curr_state);
                    }
                    prev_state = Some(curr_state);
                    if buffer.update_targets.len().max(buffer.combine_targets.len()) >= STANDARD_VECTOR_SIZE {
                        buffer.flush(aggregator, inputs)?;
                    }
                }
                buffer.flush(aggregator, inputs)?;
            }
            merge_sort_tree.tree.push(Level {
                elements: level,
                cascades: std::mem::take(&mut zipped_level.cascades),
            });
            run_length *= super::merge_sort_tree::FANOUT;
        }
        Ok(Self {
            merge_sort_tree,
            levels_flat_native,
            levels_flat_start,
        })
    }
}

/// A thread's result states (DuckDB's `WindowDistinctAggregatorLocalState`).
#[derive(Debug)]
pub(crate) struct DistinctLocalState {
    states: ScratchStates,
    frames: Vec<FrameBounds>,
    sources: Vec<StatePtr>,
    targets: Vec<StatePtr>,
}

impl DistinctLocalState {
    pub(crate) fn new(aggregator: &WindowAggregator) -> Result<Self> {
        Ok(Self {
            states: ScratchStates::new(aggregator.function.ops.clone(), STANDARD_VECTOR_SIZE)?,
            frames: init_subframes(aggregator.exclude_mode),
            sources: Vec::new(),
            targets: Vec::new(),
        })
    }

    fn flush(&mut self, aggregator: &WindowAggregator) -> Result<()> {
        if self.targets.is_empty() {
            return Ok(());
        }
        // SAFETY: the sources are finished tree states, the targets this thread's states.
        unsafe {
            aggregator
                .function
                .ops
                .combine(&self.sources, &self.targets, aggregator.input())?;
        }
        self.sources.clear();
        self.targets.clear();
        Ok(())
    }

    /// Combines, for each frame, the running aggregates of the first occurrences
    /// (DuckDB's `Evaluate`).
    pub(crate) fn evaluate(
        &mut self,
        aggregator: &WindowAggregator,
        global: &DistinctGlobalState,
        bounds: &WindowBounds,
        count: usize,
        row_idx: usize,
    ) -> Result<Vector> {
        self.states.begin(count)?;
        let mut frames = std::mem::take(&mut self.frames);
        let evaluated = evaluate_subframes(
            bounds,
            aggregator.exclude_mode,
            count,
            row_idx,
            &mut frames,
            |rid, frames| {
                let target = self.states.state(rid);
                let (lower, upper) = (frames[0].start, frames[0].end);
                let mut pending = Vec::new();
                global.merge_sort_tree.aggregate_lower_bound(
                    lower,
                    upper,
                    lower as u64 + 1,
                    |level, run_begin, run_pos| {
                        if run_pos != run_begin {
                            // The running aggregate up to the last first occurrence.
                            let index = global.levels_flat_start[level] + run_pos - 1;
                            pending.push(global.levels_flat_native.state(index));
                        }
                    },
                );
                for source in pending {
                    self.sources.push(source);
                    self.targets.push(target);
                    if self.targets.len() >= STANDARD_VECTOR_SIZE {
                        self.flush(aggregator)?;
                    }
                }
                Ok(())
            },
        );
        self.frames = frames;
        evaluated?;
        self.flush(aggregator)?;
        let mut result = Vector::flat(aggregator.result_type.clone(), count);
        self.states.finish(aggregator.input(), &mut result)?;
        Ok(result)
    }
}
