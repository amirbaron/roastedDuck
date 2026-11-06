//! Aggregates whose frame is the whole partition (DuckDB's
//! `window_constant_aggregator.cpp`): one aggregate per partition, computed while sinking.

use rustdb_ast::{WindowBoundary, WindowExcludeMode};
use rustdb_common::Result;
use rustdb_types::{DataChunk, SelectionVector, Vector};

use super::WindowExpression;
use super::aggregate_states::WindowAggregateStates;
use super::aggregator::WindowAggregator;
use super::boundaries_state::{WindowBound, WindowBounds};
use super::mask::{BITS_PER_ENTRY, WindowMask};

/// Whether every frame of `wexpr` is its whole partition (DuckDB's `CanAggregate`).
pub(crate) fn can_aggregate(aggregator: &WindowAggregator, wexpr: &WindowExpression) -> bool {
    let window = &wexpr.window;
    if !aggregator.function.ops.can_aggregate()
        || window.exclude_clause != WindowExcludeMode::NoOther
        || window.distinct
        // COUNT(*) is handled efficiently elsewhere.
        || window.children.is_empty()
    {
        return false;
    }
    // RANGE ... CURRENT ROW is the whole partition only without ORDER BY.
    let start_ok = match window.start {
        WindowBoundary::UnboundedPreceding => true,
        WindowBoundary::CurrentRowRange => window.orders.is_empty(),
        _ => false,
    };
    let end_ok = match window.end {
        WindowBoundary::UnboundedFollowing => true,
        WindowBoundary::CurrentRowRange => window.orders.is_empty(),
        _ => false,
    };
    start_ok && end_ok
}

/// One state and result per partition of a hash group.
#[derive(Debug)]
pub(crate) struct ConstantGlobalState {
    /// Partition starts, with the group size as a final guard.
    partition_offsets: Vec<usize>,
    states: WindowAggregateStates,
    /// The partition results, once finalized.
    results: Option<Vector>,
}

/// The partition cursor of a thread's evaluation.
#[derive(Debug, Default)]
pub(crate) struct ConstantLocalState {
    partition: usize,
}

impl ConstantGlobalState {
    pub(crate) fn new(
        aggregator: &WindowAggregator,
        group_count: usize,
        partition_mask: &WindowMask,
    ) -> Result<Self> {
        // Locate the partition boundaries.
        let mut partition_offsets = Vec::new();
        if partition_mask.is_all_valid() {
            partition_offsets.push(0);
        } else {
            let mut start = 0;
            while start < group_count {
                let entry = partition_mask.entry(start / BITS_PER_ENTRY);
                if entry == 0 && start.is_multiple_of(BITS_PER_ENTRY) {
                    // Skip a blank block.
                    start += BITS_PER_ENTRY;
                    continue;
                }
                if entry >> (start % BITS_PER_ENTRY) & 1 == 1 {
                    partition_offsets.push(start);
                }
                start += 1;
            }
        }
        let states = WindowAggregateStates::new(aggregator.function.ops.clone(), partition_offsets.len())?;
        partition_offsets.push(group_count);
        Ok(Self {
            partition_offsets,
            states,
            results: None,
        })
    }

    /// Adds the arguments of a chunk starting at row `row` to the partition states
    /// (DuckDB's `WindowConstantAggregatorLocalState::Sink`); `filter` holds the chunk
    /// rows passing the FILTER clause.
    pub(crate) fn sink(
        &mut self,
        aggregator: &WindowAggregator,
        sink_chunk: &DataChunk,
        row: usize,
        filter: Option<&[u32]>,
    ) -> Result<()> {
        let chunk_begin = row;
        let chunk_end = chunk_begin + sink_chunk.len();
        let payload: Vec<Vector> = aggregator
            .child_idx
            .iter()
            .map(|&column| sink_chunk.column(column).clone())
            .collect();
        let mut partition = self.partition_offsets.partition_point(|&offset| offset <= row) - 1;
        let mut row = row;
        let mut begin = 0;
        let mut filter_idx = 0;
        let mut partition_end = self.partition_offsets[partition + 1];
        while row < chunk_end {
            if row == partition_end {
                partition += 1;
                partition_end = self.partition_offsets[partition + 1];
            }
            partition_end = partition_end.min(chunk_end);
            let end = partition_end - chunk_begin;
            // The rows of [begin, end) (passing the filter).
            let (inputs, count): (Vec<Vector>, usize) = match filter {
                Some(filter) => {
                    while filter_idx < filter.len() && (filter[filter_idx] as usize) < begin {
                        filter_idx += 1;
                    }
                    let first = filter_idx;
                    while filter_idx < filter.len() && (filter[filter_idx] as usize) < end {
                        filter_idx += 1;
                    }
                    let sel = SelectionVector::new(filter[first..filter_idx].to_vec());
                    (
                        payload.iter().map(|column| column.slice(&sel)).collect(),
                        filter_idx - first,
                    )
                }
                None if begin == 0 => (payload.clone(), end),
                None => {
                    let sel = SelectionVector::new((begin as u32..end as u32).collect::<Vec<_>>());
                    (
                        payload.iter().map(|column| column.slice(&sel)).collect(),
                        end - begin,
                    )
                }
            };
            // No row of the partition passed the FILTER: the state stays as it is.
            if count > 0 {
                // SAFETY: the state is initialised and owned by this hash group.
                unsafe {
                    aggregator.function.ops.simple_update(
                        &inputs,
                        aggregator.input(),
                        self.states.state(partition),
                        count,
                    )?;
                }
            }
            // Skip filtered rows too.
            row += end - begin;
            begin = end;
        }
        Ok(())
    }

    /// Computes the partition results (DuckDB's `Finalize`).
    pub(crate) fn finalize(&mut self, aggregator: &WindowAggregator) -> Result<()> {
        let mut results = Vector::flat(aggregator.result_type.clone(), self.partition_offsets.len() - 1);
        self.states.finalize(aggregator.input(), &mut results)?;
        self.results = Some(results);
        Ok(())
    }

    /// Copies each row's partition result (DuckDB's `Evaluate`).
    pub(crate) fn evaluate(
        &self,
        local: &mut ConstantLocalState,
        bounds: &WindowBounds,
        count: usize,
    ) -> Result<Vector> {
        let results = self
            .results
            .as_ref()
            .ok_or_else(|| rustdb_common::Error::internal("constant window aggregate not finalized"))?;
        let begins = &bounds[WindowBound::FrameBegin];
        let mut rows = Vec::with_capacity(count);
        for &begin in &begins[..count] {
            // Find the partition containing the frame.
            while self.partition_offsets[local.partition + 1] <= begin {
                local.partition += 1;
            }
            rows.push(Some(local.partition as u32));
        }
        if rows
            .first()
            .is_some_and(|first| rows.iter().all(|row| row == first))
        {
            // The whole chunk is one partition: a constant result.
            let mut result = results.select_rows(&rows[..1]);
            result.make_constant();
            return Ok(result);
        }
        Ok(results.select_rows(&rows))
    }
}
