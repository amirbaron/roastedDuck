//! The window executor interface (DuckDB's `window_executor.cpp`).
//!
//! An executor evaluates one window function over the hash groups of a window operator.
//! Per hash group it has a global state (shared structures such as segment trees, built
//! while sinking and finalizing) and a local state per evaluating thread (boundary state,
//! cursors and scratch space). Sinking and finalizing a hash group take the global state
//! mutably, so a hash group is built by one thread; evaluation only reads it.

use rustdb_common::{Error, Result};
use rustdb_planner::ExpressionType;
use rustdb_types::{DataChunk, Vector};

use super::WindowExpression;
use super::aggregate_function::{AggregateExecutor, AggregateGlobalState, AggregateLocalState};
use super::boundaries_state::{
    BoundsInput, WindowBoundariesState, WindowBounds, WindowInputExpression, has_following_range,
    has_preceding_range,
};
use super::collection::WindowCollection;
use super::mask::WindowMask;
use super::rank_function::{PeerExecutor, PeerGlobalState, PeerLocalState};
use super::rownumber_function::{RowNumberExecutor, RowNumberGlobalState};
use super::shared_expressions::WindowSharedExpressions;
use super::value_function::{ValueExecutor, ValueGlobalState, ValueLocalState};
use crate::ExecutionContext;

/// The hash group data an executor reads.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WindowGroup<'a> {
    /// Rows in the hash group.
    pub count: usize,
    /// Rows that start a partition.
    pub partition_mask: &'a WindowMask,
    /// Rows that start a peer group of the executor's ORDER BY.
    pub order_mask: &'a WindowMask,
    /// The materialised shared expressions.
    pub collection: &'a WindowCollection,
}

/// What a function sees while evaluating one chunk.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EvaluateInput<'a> {
    pub group: WindowGroup<'a>,
    /// The boundary columns of the chunk's rows.
    pub bounds: &'a WindowBounds,
    /// The ORDER BY values of the hash group, if the frame needs them.
    pub range: Option<&'a Vector>,
    /// The hash group row of the chunk's first row.
    pub row_idx: usize,
    pub count: usize,
    /// The shared evaluation expressions of the chunk.
    pub eval_chunk: &'a DataChunk,
}

/// The function-specific part of an executor.
#[derive(Debug)]
enum Function {
    RowNumber(RowNumberExecutor),
    Peer(PeerExecutor),
    Value(ValueExecutor),
    Aggregate(Box<AggregateExecutor>),
}

/// Evaluates one window function (DuckDB's `WindowExecutor`).
#[derive(Debug)]
pub(crate) struct WindowExecutor {
    pub(crate) wexpr: WindowExpression,
    /// Evaluation columns of the frame boundary expressions.
    boundary_start_idx: Option<usize>,
    boundary_end_idx: Option<usize>,
    /// Collection column of the ORDER BY values, for RANGE frames (and FILL).
    range_idx: Option<usize>,
    function: Function,
}

/// An executor's state for one hash group.
#[derive(Debug)]
pub(crate) enum ExecutorGlobalState {
    RowNumber(Box<RowNumberGlobalState>),
    Peer(Box<PeerGlobalState>),
    Value(Box<ValueGlobalState>),
    Aggregate(Box<AggregateGlobalState>),
}

/// The function-specific part of a local state.
#[derive(Debug)]
enum FunctionLocal {
    None,
    Peer(PeerLocalState),
    Value(ValueLocalState),
    Aggregate(Box<AggregateLocalState>),
}

/// An executor's per-thread state for one hash group.
#[derive(Debug)]
pub(crate) struct ExecutorLocalState {
    bounds: WindowBounds,
    state: WindowBoundariesState,
    function: FunctionLocal,
}

fn state_mismatch() -> Error {
    Error::internal("window executor state of the wrong kind")
}

impl WindowExecutor {
    /// Builds the executor for `wexpr`, registering its expressions in `shared`
    /// (DuckDB's `WindowExecutorFactory`).
    pub(crate) fn new(mut wexpr: WindowExpression, shared: &mut WindowSharedExpressions) -> Result<Self> {
        if wexpr.expression_type == ExpressionType::WindowAggregate {
            AggregateExecutor::simplify(&mut wexpr);
        }
        let mut range_idx = if has_preceding_range(&wexpr) || has_following_range(&wexpr) {
            shared.register_collection(wexpr.window.orders.first().map(|order| &order.expression), false)
        } else {
            None
        };
        let boundary_start_idx = shared.register_evaluate(wexpr.window.start_expr.as_ref());
        let boundary_end_idx = shared.register_evaluate(wexpr.window.end_expr.as_ref());
        let function = match wexpr.expression_type {
            ExpressionType::WindowAggregate => {
                Function::Aggregate(Box::new(AggregateExecutor::new(&wexpr, shared)?))
            }
            ExpressionType::WindowRowNumber | ExpressionType::WindowNtile => {
                Function::RowNumber(RowNumberExecutor::new(&wexpr, shared))
            }
            ExpressionType::WindowRank
            | ExpressionType::WindowRankDense
            | ExpressionType::WindowPercentRank
            | ExpressionType::WindowCumeDist => Function::Peer(PeerExecutor::new(&wexpr, shared)?),
            ExpressionType::WindowLead
            | ExpressionType::WindowLag
            | ExpressionType::WindowFirstValue
            | ExpressionType::WindowLastValue
            | ExpressionType::WindowNthValue
            | ExpressionType::WindowFill => {
                Function::Value(ValueExecutor::new(&wexpr, shared, &mut range_idx)?)
            }
            other => {
                return Err(Error::internal(format!("Window aggregate type {}", other.name())));
            }
        };
        Ok(Self {
            wexpr,
            boundary_start_idx,
            boundary_end_idx,
            range_idx,
            function,
        })
    }

    /// The state for a hash group of `count` rows (DuckDB's `GetGlobalState`).
    pub(crate) fn global_state(
        &self,
        ctx: &ExecutionContext,
        count: usize,
        partition_mask: &WindowMask,
    ) -> Result<ExecutorGlobalState> {
        Ok(match &self.function {
            Function::RowNumber(function) => {
                ExecutorGlobalState::RowNumber(Box::new(function.global_state(ctx, &self.wexpr, count)?))
            }
            Function::Peer(function) => {
                ExecutorGlobalState::Peer(Box::new(function.global_state(ctx, &self.wexpr, count)?))
            }
            Function::Value(function) => {
                ExecutorGlobalState::Value(Box::new(function.global_state(ctx, &self.wexpr, count)?))
            }
            Function::Aggregate(function) => {
                ExecutorGlobalState::Aggregate(Box::new(function.global_state(ctx, count, partition_mask)?))
            }
        })
    }

    /// A thread's state for a hash group (DuckDB's `GetLocalState`).
    pub(crate) fn local_state(
        &self,
        ctx: &ExecutionContext,
        global: &ExecutorGlobalState,
        count: usize,
    ) -> Result<ExecutorLocalState> {
        let mut state = WindowBoundariesState::new(&self.wexpr, count)?;
        let function = match (&self.function, global) {
            (Function::RowNumber(_), ExecutorGlobalState::RowNumber(_)) => FunctionLocal::None,
            (Function::Peer(_), ExecutorGlobalState::Peer(_)) => {
                FunctionLocal::Peer(PeerLocalState::default())
            }
            (Function::Value(function), ExecutorGlobalState::Value(global)) => {
                FunctionLocal::Value(function.local_state(&self.wexpr, global, &mut state))
            }
            (Function::Aggregate(function), ExecutorGlobalState::Aggregate(_)) => {
                FunctionLocal::Aggregate(Box::new(function.local_state(ctx)?))
            }
            _ => return Err(state_mismatch()),
        };
        Ok(ExecutorLocalState {
            bounds: WindowBounds::default(),
            state,
            function,
        })
    }

    /// Feeds a chunk of the hash group starting at row `input_idx` (DuckDB's `Sink`).
    pub(crate) fn sink(
        &self,
        global: &mut ExecutorGlobalState,
        local: &mut ExecutorLocalState,
        sink_chunk: &DataChunk,
        coll_chunk: &DataChunk,
        input_idx: usize,
    ) -> Result<()> {
        match (&self.function, global, &mut local.function) {
            (Function::RowNumber(_), ExecutorGlobalState::RowNumber(global), _) => {
                RowNumberExecutor::sink(global, sink_chunk, input_idx)
            }
            (Function::Peer(_), ExecutorGlobalState::Peer(global), _) => {
                PeerExecutor::sink(global, sink_chunk, input_idx)
            }
            (Function::Value(function), ExecutorGlobalState::Value(global), _) => {
                function.sink(&self.wexpr, global, sink_chunk, coll_chunk, input_idx)
            }
            (Function::Aggregate(function), ExecutorGlobalState::Aggregate(global), _) => {
                function.sink(global, sink_chunk, coll_chunk, input_idx)
            }
            _ => Err(state_mismatch()),
        }
    }

    /// Builds the acceleration structures once all rows are sunk and the collection is
    /// complete (DuckDB's `Finalize`).
    pub(crate) fn finalize(
        &self,
        global: &mut ExecutorGlobalState,
        local: &mut ExecutorLocalState,
        group: WindowGroup<'_>,
    ) -> Result<()> {
        match (&self.function, global, &mut local.function) {
            (Function::RowNumber(_), ExecutorGlobalState::RowNumber(global), _) => {
                RowNumberExecutor::finalize(global)
            }
            (Function::Peer(_), ExecutorGlobalState::Peer(global), _) => PeerExecutor::finalize(global),
            (Function::Value(_), ExecutorGlobalState::Value(global), _) => ValueExecutor::finalize(global),
            (
                Function::Aggregate(function),
                ExecutorGlobalState::Aggregate(global),
                FunctionLocal::Aggregate(local),
            ) => function.finalize(global, local, group),
            _ => Err(state_mismatch()),
        }
    }

    /// Computes the function for `eval_chunk.len()` rows starting at hash group row
    /// `row_idx` (DuckDB's `Evaluate`).
    pub(crate) fn evaluate(
        &self,
        global: &ExecutorGlobalState,
        local: &mut ExecutorLocalState,
        group: WindowGroup<'_>,
        row_idx: usize,
        eval_chunk: &DataChunk,
    ) -> Result<Vector> {
        let count = eval_chunk.len();
        let boundary_start = WindowInputExpression::new(eval_chunk, self.boundary_start_idx);
        let boundary_end = WindowInputExpression::new(eval_chunk, self.boundary_end_idx);
        let range = self.range_idx.map(|index| group.collection.column(index));
        local.state.bounds(
            &mut local.bounds,
            &BoundsInput {
                row_idx,
                count,
                range,
                boundary_start: &boundary_start,
                boundary_end: &boundary_end,
                partition_mask: group.partition_mask,
                order_mask: group.order_mask,
            },
        )?;
        let input = EvaluateInput {
            group,
            bounds: &local.bounds,
            range,
            row_idx,
            count,
            eval_chunk,
        };
        match (&self.function, global, &mut local.function) {
            (Function::RowNumber(function), ExecutorGlobalState::RowNumber(global), _) => {
                function.evaluate(&self.wexpr, global, &input)
            }
            (Function::Peer(function), ExecutorGlobalState::Peer(global), FunctionLocal::Peer(local)) => {
                Ok(function.evaluate(global, local, &input))
            }
            (Function::Value(function), ExecutorGlobalState::Value(global), FunctionLocal::Value(local)) => {
                function.evaluate(&self.wexpr, global, local, &input)
            }
            (
                Function::Aggregate(function),
                ExecutorGlobalState::Aggregate(global),
                FunctionLocal::Aggregate(local),
            ) => function.evaluate(global, local, &input),
            _ => Err(state_mismatch()),
        }
    }

    /// The partition plus ORDER BY width: which peer mask the executor reads.
    pub(crate) fn key_count(&self) -> usize {
        self.wexpr.window.partitions.len() + self.wexpr.window.orders.len()
    }
}
