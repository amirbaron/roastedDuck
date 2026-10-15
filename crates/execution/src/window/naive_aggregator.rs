//! Aggregation frame by frame (DuckDB's `window_naive_aggregator.cpp`): the fallback for
//! combinations no accelerator handles, such as ordered aggregates with arbitrary frames.

use std::collections::HashMap;

use rustdb_common::{Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::expression::BoundOrderByNode;
use rustdb_row::sort::{Sort, SortColumn, SortConfig};
use rustdb_types::{DataChunk, LogicalType, SelectionVector, Vector};

use super::aggregate_states::ScratchStates;
use super::aggregator::{FlushBuffer, WindowAggregator, evaluate_subframes, init_subframes};
use super::boundaries_state::{FrameBounds, WindowBounds};
use super::mask::WindowMask;
use super::order_modifiers;
use crate::ExecutionContext;

/// Whether the naive strategy can compute the aggregate (DuckDB's `CanAggregate`).
pub(crate) fn can_aggregate(aggregator: &WindowAggregator) -> bool {
    aggregator.function.ops.can_aggregate()
}

/// The inputs of a naive evaluation.
#[derive(Debug)]
pub(crate) struct NaiveInputs<'a> {
    /// The argument columns of the hash group.
    pub inputs: &'a [Vector],
    /// The `ORDER BY` argument columns of the hash group.
    pub arg_orders: &'a [Vector],
    pub filter_mask: &'a WindowMask,
    /// Per-row hashes of the arguments, for DISTINCT.
    pub hashes: Option<&'a [u64]>,
}

/// A thread's state (DuckDB's `WindowNaiveLocalState`).
#[derive(Debug)]
pub(crate) struct NaiveLocalState {
    states: ScratchStates,
    buffer: FlushBuffer,
    frames: Vec<FrameBounds>,
    /// Sorts each frame by the `ORDER BY` arguments, producing row numbers.
    sort: Option<Sort>,
}

impl NaiveLocalState {
    pub(crate) fn new(
        ctx: &ExecutionContext,
        aggregator: &WindowAggregator,
        arg_orders: &[BoundOrderByNode],
    ) -> Result<Self> {
        let sort = if arg_orders.is_empty() {
            None
        } else {
            // The ORDER BY values are precomputed; sort them with the row number as payload.
            let mut types: Vec<LogicalType> = arg_orders
                .iter()
                .map(|order| order.expression.return_type.clone())
                .collect();
            types.push(LogicalType::UBIGINT);
            let orders = arg_orders
                .iter()
                .enumerate()
                .map(|(column, order)| {
                    Ok(SortColumn {
                        column,
                        modifiers: order_modifiers(order)?,
                        stats: None,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let projection = vec![types.len() - 1];
            Some(Sort::new(
                ctx.env.buffer_manager.clone(),
                orders,
                &types,
                Some(projection),
            )?)
        };
        Ok(Self {
            states: ScratchStates::new(aggregator.function.ops.clone(), STANDARD_VECTOR_SIZE)?,
            buffer: FlushBuffer::default(),
            frames: init_subframes(aggregator.exclude_mode),
            sort,
        })
    }

    /// The frame rows passing the filter, sorted by the `ORDER BY` arguments.
    fn sorted_rows(sort: &Sort, input: &NaiveInputs<'_>, frames: &[FrameBounds]) -> Result<Vec<usize>> {
        let rows: Vec<u32> = frames
            .iter()
            .flat_map(|frame| frame.start..frame.end)
            .filter(|&row| input.filter_mask.is_valid(row))
            .map(|row| row as u32)
            .collect();
        let global = sort.global_sink_state(SortConfig {
            threads: 1,
            connections: 1,
            ..SortConfig::default()
        });
        let mut local = sort.local_sink_state();
        for batch in rows.chunks(STANDARD_VECTOR_SIZE) {
            let sel = SelectionVector::new(batch.to_vec());
            let mut columns: Vec<Vector> = input.arg_orders.iter().map(|column| column.slice(&sel)).collect();
            let mut numbers = Vector::flat(LogicalType::UBIGINT, batch.len());
            for (number, &row) in numbers.values_mut::<u64>().iter_mut().zip(batch) {
                *number = u64::from(row);
            }
            columns.push(numbers);
            sort.sink(&DataChunk::new(columns, batch.len()), &global, &mut local)?;
        }
        sort.combine(&global, local)?;
        let Some(source) = sort.finalize(global)? else {
            return Ok(Vec::new());
        };
        let mut sorted = Vec::with_capacity(rows.len());
        for chunk in source.collect()? {
            let numbers = chunk.column(0).flattened(chunk.len());
            sorted.extend(
                numbers.values::<u64>()[..chunk.len()]
                    .iter()
                    .map(|&row| row as usize),
            );
        }
        Ok(sorted)
    }

    /// Aggregates each row's frame from scratch (DuckDB's `WindowNaiveLocalState::Evaluate`).
    pub(crate) fn evaluate(
        &mut self,
        aggregator: &WindowAggregator,
        input: &NaiveInputs<'_>,
        bounds: &WindowBounds,
        count: usize,
        row_idx: usize,
    ) -> Result<Vector> {
        self.states.begin(count)?;
        let Self {
            states,
            buffer,
            frames,
            sort,
        } = self;
        // DISTINCT: rows already aggregated, by argument hash.
        let mut seen: HashMap<u64, Vec<usize>> = HashMap::new();
        evaluate_subframes(
            bounds,
            aggregator.exclude_mode,
            count,
            row_idx,
            frames,
            |rid, frames| {
                let state = states.state(rid);
                seen.clear();
                let rows: Vec<usize> = match sort {
                    Some(sort) => Self::sorted_rows(sort, input, frames)?,
                    None => frames
                        .iter()
                        .flat_map(|frame| frame.start..frame.end)
                        .filter(|&row| input.filter_mask.is_valid(row))
                        .collect(),
                };
                for row in rows {
                    // Filter out duplicates.
                    if let Some(hashes) = input.hashes {
                        let bucket = seen.entry(hashes[row]).or_default();
                        let mut duplicate = false;
                        for &other in bucket.iter() {
                            if rows_equal(input.inputs, other, row)? {
                                duplicate = true;
                                break;
                            }
                        }
                        if duplicate {
                            continue;
                        }
                        bucket.push(row);
                    }
                    buffer.update(aggregator, input.inputs, state, row)?;
                }
                Ok(())
            },
        )?;
        buffer.flush_updates(aggregator, input.inputs)?;
        let mut result = Vector::flat(aggregator.result_type.clone(), count);
        states.finish(aggregator.input(), &mut result)?;
        Ok(result)
    }
}

/// Whether two rows have the same arguments (`IS NOT DISTINCT FROM` on every column).
fn rows_equal(columns: &[Vector], left: usize, right: usize) -> Result<bool> {
    for column in columns {
        if rustdb_row::values_distinct(column, left, column, right)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Hashes the argument rows of a hash group for DISTINCT.
pub(crate) fn hash_rows(inputs: &[Vector], count: usize) -> Result<Vec<u64>> {
    let columns: Vec<&Vector> = inputs.iter().collect();
    let hashes = rustdb_row::hash::hash_columns(&columns, count)?;
    Ok(hashes.values::<u64>()[..count].to_vec())
}
