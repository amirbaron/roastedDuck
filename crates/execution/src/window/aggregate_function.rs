//! Aggregates used as window functions (DuckDB's `window_aggregate_function.cpp`):
//! simplification, the choice of evaluation strategy and the FILTER clause.

use rustdb_ast::WindowBoundary;
use rustdb_common::{Error, Result};
use rustdb_function::aggregate::{DistinctDependence, FrameDelta, OrderDependence};
use rustdb_types::{DataChunk, Vector};

use super::WindowExpression;
use super::aggregator::WindowAggregator;
use super::constant_aggregator::{self, ConstantGlobalState, ConstantLocalState};
use super::custom_aggregator::{self, CustomGlobalState, CustomLocalState};
use super::distinct_aggregator::{self, DistinctGlobalState, DistinctLocalState, DistinctSink};
use super::executor::{EvaluateInput, WindowGroup};
use super::mask::WindowMask;
use super::naive_aggregator::{self, NaiveInputs, NaiveLocalState, hash_rows};
use super::segment_tree::{self, SegmentTreeGlobalState, SegmentTreeLocalState, TreeInputs};
use super::shared_expressions::WindowSharedExpressions;
use super::shared_orders;
use crate::ExecutionContext;

/// The evaluation strategy of an aggregate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Strategy {
    /// Frames are whole partitions.
    Constant,
    /// The aggregate's own `window` callback.
    Custom,
    /// A segment tree over the partition.
    SegmentTree,
    /// `DISTINCT` via a merge sort tree.
    Distinct,
    /// Each frame from scratch.
    Naive,
}

/// An aggregate window function (DuckDB's `WindowAggregateExecutor`).
#[derive(Debug)]
pub(crate) struct AggregateExecutor {
    strategy: Strategy,
    aggregator: WindowAggregator,
    /// Sink column of the FILTER expression.
    filter_idx: Option<usize>,
    /// Collection columns of the `ORDER BY` arguments (naive strategy).
    arg_order_idx: Vec<usize>,
    /// The `ORDER BY` arguments.
    arg_orders: Vec<rustdb_planner::expression::BoundOrderByNode>,
    start: WindowBoundary,
    end: WindowBoundary,
}

/// The strategy's data for one hash group.
#[derive(Debug)]
enum StrategyGlobal {
    Constant(ConstantGlobalState),
    /// Built in `finalize`.
    Custom(Option<CustomGlobalState>),
    SegmentTree(Option<SegmentTreeGlobalState>),
    Distinct {
        sink: Option<Box<DistinctSink>>,
        tree: Option<DistinctGlobalState>,
    },
    Naive {
        hashes: Option<Vec<u64>>,
    },
}

/// Per hash group (DuckDB's `WindowAggregateExecutorGlobalState` + the aggregator's).
#[derive(Debug)]
pub(crate) struct AggregateGlobalState {
    /// Rows that pass the FILTER clause.
    filter_mask: WindowMask,
    /// The argument columns of the hash group (collection strategies), set in `finalize`.
    inputs: Vec<Vector>,
    arg_orders: Vec<Vector>,
    strategy: StrategyGlobal,
}

/// The strategy's per-thread state.
#[derive(Debug)]
pub(crate) enum AggregateLocalState {
    Constant(ConstantLocalState),
    Custom(Option<CustomLocalState>),
    SegmentTree(SegmentTreeLocalState),
    Distinct(DistinctLocalState),
    Naive(NaiveLocalState),
}

/// The rows of `sink_chunk` whose FILTER column is true.
fn filter_rows(sink_chunk: &DataChunk, filter_idx: usize) -> Vec<u32> {
    let column = sink_chunk.column(filter_idx).flattened(sink_chunk.len());
    let values = column.unified::<bool>();
    (0..sink_chunk.len() as u32)
        .filter(|&row| values.get(row as usize) == Some(true))
        .collect()
}

/// Narrows a frame delta by what the boundary type implies (DuckDB's `ApplyWindowStats`
/// without expression statistics).
fn apply_window_stats(boundary: WindowBoundary, delta: &mut FrameDelta, is_start: bool) -> Result<()> {
    match boundary {
        WindowBoundary::UnboundedPreceding if is_start => delta.end = 0,
        WindowBoundary::UnboundedFollowing if !is_start => delta.begin = 0,
        WindowBoundary::CurrentRowRows => {
            delta.begin = 0;
            delta.end = 0;
        }
        WindowBoundary::ExprPrecedingRows
        | WindowBoundary::ExprFollowingRows
        | WindowBoundary::CurrentRowRange
        | WindowBoundary::ExprPrecedingRange
        | WindowBoundary::ExprFollowingRange
        | WindowBoundary::CurrentRowGroups
        | WindowBoundary::ExprPrecedingGroups
        | WindowBoundary::ExprFollowingGroups => {}
        WindowBoundary::Invalid => {
            return Err(Error::internal(if is_start {
                "Unknown window start boundary"
            } else {
                "Unknown window end boundary"
            }));
        }
        _ => {
            return Err(Error::internal(if is_start {
                "Unsupported window start boundary"
            } else {
                "Unsupported window end boundary"
            }));
        }
    }
    Ok(())
}

impl AggregateExecutor {
    /// Drops modifiers that cannot change the result (DuckDB's
    /// `SimplifyWindowedAggregate`; the optimizer is always enabled).
    pub(crate) fn simplify(wexpr: &mut WindowExpression) {
        let Some(aggregate) = &wexpr.window.aggregate else {
            return;
        };
        if aggregate.distinct_dependence != DistinctDependence::DistinctDependent {
            wexpr.window.distinct = false;
        }
        if aggregate.order_dependence != OrderDependence::OrderDependent
            // An argument order that is a prefix of the partition order is implied.
            || shared_orders(&wexpr.window.orders, &wexpr.window.arg_orders) == wexpr.window.arg_orders.len()
        {
            wexpr.window.arg_orders.clear();
        }
    }

    /// Chooses the strategy and registers the expressions it needs.
    pub(crate) fn new(wexpr: &WindowExpression, shared: &mut WindowSharedExpressions) -> Result<Self> {
        let probe = WindowAggregator::new(wexpr)?;
        let has_arg_orders = !wexpr.window.arg_orders.is_empty();
        let strategy = if distinct_aggregator::can_aggregate(&probe, has_arg_orders) {
            Strategy::Distinct
        } else if constant_aggregator::can_aggregate(&probe, wexpr) && !has_arg_orders {
            // DuckDB rebinds ordered aggregates as sorted aggregates here; without them,
            // ordered constant frames take the naive path below.
            Strategy::Constant
        } else if custom_aggregator::can_aggregate(&probe, has_arg_orders) {
            Strategy::Custom
        } else if segment_tree::can_aggregate(&probe, has_arg_orders) && !wexpr.window.children.is_empty() {
            // Argument-less aggregates (COUNT(*)) need a `window` callback in a segment tree.
            Strategy::SegmentTree
        } else if naive_aggregator::can_aggregate(&probe) {
            Strategy::Naive
        } else {
            return Err(Error::internal(
                "Could not create a window aggregator with the given parameters!",
            ));
        };
        let mut aggregator = if strategy == Strategy::Constant {
            // The constant aggregator only needs the arguments while sinking.
            let mut aggregator = probe;
            aggregator.child_idx = wexpr
                .window
                .children
                .iter()
                .filter_map(|child| shared.register_sink(Some(child)))
                .collect();
            aggregator
        } else {
            WindowAggregator::with_collection_arguments(wexpr, shared)?
        };
        let arg_order_idx = if strategy == Strategy::Naive {
            wexpr
                .window
                .arg_orders
                .iter()
                .filter_map(|order| shared.register_collection(Some(&order.expression), false))
                .collect()
        } else {
            Vec::new()
        };
        aggregator.has_filter = wexpr.window.filter_expr.is_some();
        let filter_idx = shared.register_sink(wexpr.window.filter_expr.as_ref());
        Ok(Self {
            strategy,
            aggregator,
            filter_idx,
            arg_order_idx,
            arg_orders: wexpr.window.arg_orders.clone(),
            start: wexpr.window.start,
            end: wexpr.window.end,
        })
    }

    pub(crate) fn global_state(
        &self,
        ctx: &ExecutionContext,
        count: usize,
        partition_mask: &WindowMask,
    ) -> Result<AggregateGlobalState> {
        let strategy = match self.strategy {
            Strategy::Constant => {
                StrategyGlobal::Constant(ConstantGlobalState::new(&self.aggregator, count, partition_mask)?)
            }
            Strategy::Custom => StrategyGlobal::Custom(None),
            Strategy::SegmentTree => StrategyGlobal::SegmentTree(None),
            Strategy::Distinct => StrategyGlobal::Distinct {
                sink: Some(Box::new(DistinctSink::new(ctx, &self.aggregator)?)),
                tree: None,
            },
            Strategy::Naive => StrategyGlobal::Naive { hashes: None },
        };
        Ok(AggregateGlobalState {
            // Start with all rows filtered out and mark those that pass.
            filter_mask: if self.filter_idx.is_some() {
                WindowMask::all_invalid(count)
            } else {
                WindowMask::all_valid()
            },
            inputs: Vec::new(),
            arg_orders: Vec::new(),
            strategy,
        })
    }

    pub(crate) fn local_state(&self, ctx: &ExecutionContext) -> Result<AggregateLocalState> {
        Ok(match self.strategy {
            Strategy::Constant => AggregateLocalState::Constant(ConstantLocalState::default()),
            Strategy::Custom => AggregateLocalState::Custom(None),
            Strategy::SegmentTree => AggregateLocalState::SegmentTree(SegmentTreeLocalState::default()),
            Strategy::Distinct => AggregateLocalState::Distinct(DistinctLocalState::new(&self.aggregator)?),
            Strategy::Naive => {
                AggregateLocalState::Naive(NaiveLocalState::new(ctx, &self.aggregator, &self.arg_orders)?)
            }
        })
    }

    pub(crate) fn sink(
        &self,
        global: &mut AggregateGlobalState,
        sink_chunk: &DataChunk,
        coll_chunk: &DataChunk,
        input_idx: usize,
    ) -> Result<()> {
        let filter = self.filter_idx.map(|index| filter_rows(sink_chunk, index));
        if let Some(rows) = &filter {
            for &row in rows {
                global.filter_mask.set_valid(input_idx + row as usize);
            }
        }
        match &mut global.strategy {
            StrategyGlobal::Constant(state) => {
                state.sink(&self.aggregator, sink_chunk, input_idx, filter.as_deref())
            }
            StrategyGlobal::Distinct { sink: Some(sink), .. } => {
                sink.sink(&self.aggregator, coll_chunk, input_idx, filter.as_deref())
            }
            _ => Ok(()),
        }
    }

    /// The frame statistics (DuckDB's `FrameStats`; without expression statistics the
    /// frame defaults to the whole partition).
    fn frame_stats(&self, count: usize) -> Result<[FrameDelta; 2]> {
        let count = count as i64;
        let mut stats = [FrameDelta {
            begin: -count,
            end: count,
        }; 2];
        apply_window_stats(self.start, &mut stats[0], true)?;
        apply_window_stats(self.end, &mut stats[1], false)?;
        Ok(stats)
    }

    pub(crate) fn finalize(
        &self,
        global: &mut AggregateGlobalState,
        local: &mut AggregateLocalState,
        group: WindowGroup<'_>,
    ) -> Result<()> {
        let collection = group.collection;
        if self.strategy != Strategy::Constant {
            global.inputs = self
                .aggregator
                .child_idx
                .iter()
                .map(|&column| collection.column(column).clone())
                .collect();
        }
        global.arg_orders = self
            .arg_order_idx
            .iter()
            .map(|&column| collection.column(column).clone())
            .collect();
        let stats = self.frame_stats(group.count)?;
        match (&mut global.strategy, local) {
            (StrategyGlobal::Constant(state), _) => state.finalize(&self.aggregator),
            (StrategyGlobal::Custom(state), AggregateLocalState::Custom(local)) => {
                *state = Some(CustomGlobalState::new(
                    &self.aggregator,
                    collection,
                    &global.filter_mask,
                    stats,
                )?);
                *local = Some(CustomLocalState::new(&self.aggregator)?);
                Ok(())
            }
            (StrategyGlobal::SegmentTree(tree), _) => {
                *tree = Some(SegmentTreeGlobalState::build(
                    &self.aggregator,
                    &global.inputs,
                    &global.filter_mask,
                    group.count,
                )?);
                Ok(())
            }
            (StrategyGlobal::Distinct { sink, tree }, _) => {
                let sink = sink
                    .take()
                    .ok_or_else(|| Error::internal("distinct window aggregate finalized twice"))?;
                *tree = Some(DistinctGlobalState::build(
                    &self.aggregator,
                    *sink,
                    &global.inputs,
                    group.count,
                )?);
                Ok(())
            }
            (StrategyGlobal::Naive { hashes }, _) => {
                if self.aggregator.distinct {
                    *hashes = Some(hash_rows(&global.inputs, group.count)?);
                }
                Ok(())
            }
            _ => Err(Error::internal("window aggregate state of the wrong kind")),
        }
    }

    pub(crate) fn evaluate(
        &self,
        global: &AggregateGlobalState,
        local: &mut AggregateLocalState,
        input: &EvaluateInput<'_>,
    ) -> Result<Vector> {
        let EvaluateInput {
            bounds,
            row_idx,
            count,
            ..
        } = *input;
        let not_finalized = || Error::internal("window aggregate evaluated before finalize");
        match (&global.strategy, local) {
            (StrategyGlobal::Constant(state), AggregateLocalState::Constant(local)) => {
                state.evaluate(local, bounds, count)
            }
            (StrategyGlobal::Custom(state), AggregateLocalState::Custom(local)) => {
                let Some(state) = state else {
                    return Err(not_finalized());
                };
                // A thread evaluating without having finalized sets up its state now.
                let local = match local {
                    Some(local) => local,
                    None => local.insert(CustomLocalState::new(&self.aggregator)?),
                };
                state.evaluate(&self.aggregator, local, bounds, count, row_idx)
            }
            (StrategyGlobal::SegmentTree(tree), AggregateLocalState::SegmentTree(local)) => {
                let tree = tree.as_ref().ok_or_else(not_finalized)?;
                let tree = TreeInputs::new(&self.aggregator, &global.inputs, &global.filter_mask, tree);
                local.evaluate(&tree, bounds, count, row_idx)
            }
            (StrategyGlobal::Distinct { tree, .. }, AggregateLocalState::Distinct(local)) => {
                let tree = tree.as_ref().ok_or_else(not_finalized)?;
                local.evaluate(&self.aggregator, tree, bounds, count, row_idx)
            }
            (StrategyGlobal::Naive { hashes }, AggregateLocalState::Naive(local)) => {
                let inputs = NaiveInputs {
                    inputs: &global.inputs,
                    arg_orders: &global.arg_orders,
                    filter_mask: &global.filter_mask,
                    hashes: hashes.as_deref(),
                };
                local.evaluate(&self.aggregator, &inputs, bounds, count, row_idx)
            }
            _ => Err(Error::internal("window aggregate state of the wrong kind")),
        }
    }
}
