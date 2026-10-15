//! `rank`, `dense_rank`, `percent_rank` and `cume_dist` (DuckDB's
//! `window_rank_function.cpp`): functions of a row's peer group.

use rustdb_common::{Error, Result};
use rustdb_planner::ExpressionType;
use rustdb_types::{DataChunk, LogicalType, Vector};

use super::WindowExpression;
use super::boundaries_state::WindowBound;
use super::executor::EvaluateInput;
use super::merge_sort_tree::WindowMergeSortTree;
use super::shared_expressions::WindowSharedExpressions;
use super::shared_orders;
use crate::ExecutionContext;

/// Which peer function an executor computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PeerKind {
    Rank,
    DenseRank,
    PercentRank,
    CumeDist,
}

/// A peer-based function (DuckDB's `WindowPeerExecutor` and subclasses).
#[derive(Debug)]
pub(crate) struct PeerExecutor {
    kind: PeerKind,
    /// Sink columns of the `ORDER BY` arguments.
    arg_order_idx: Vec<usize>,
}

/// Per hash group (DuckDB's `WindowPeerGlobalState`).
#[derive(Debug)]
pub(crate) struct PeerGlobalState {
    use_framing: bool,
    token_tree: Option<WindowMergeSortTree>,
}

/// Rank counters carried between chunks (DuckDB's `WindowPeerLocalState`).
#[derive(Debug, Default)]
pub(crate) struct PeerLocalState {
    /// The row after the last evaluated chunk (`None`: nothing evaluated yet).
    row_idx: Option<usize>,
    dense_rank: u64,
    rank_equal: u64,
    rank: u64,
}

impl PeerLocalState {
    fn next_rank(&mut self, partition_begin: usize, peer_begin: usize, row_idx: usize) {
        if partition_begin == row_idx {
            self.dense_rank = 1;
            self.rank = 1;
            self.rank_equal = 0;
        } else if peer_begin == row_idx {
            self.dense_rank += 1;
            self.rank += self.rank_equal;
            self.rank_equal = 0;
        }
        self.rank_equal += 1;
    }
}

/// `(rank - 1) / (rows - 1)`, or 0 for a single row.
fn percent_rank(begin: usize, end: usize, rank: u64) -> f64 {
    let denominator = (end as i64 - begin as i64 - 1) as f64;
    if denominator > 0.0 {
        (rank as f64 - 1.0) / denominator
    } else {
        0.0
    }
}

/// `(rows up to the last peer) / rows`.
fn cume_dist(begin: usize, end: usize, peer_end: usize) -> f64 {
    let denominator = (end as i64 - begin as i64) as f64;
    let numerator = (peer_end.max(begin) - begin) as f64;
    if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    }
}

impl PeerExecutor {
    pub(crate) fn new(wexpr: &WindowExpression, shared: &mut WindowSharedExpressions) -> Result<Self> {
        let kind = match wexpr.expression_type {
            ExpressionType::WindowRank => PeerKind::Rank,
            ExpressionType::WindowRankDense => PeerKind::DenseRank,
            ExpressionType::WindowPercentRank => PeerKind::PercentRank,
            ExpressionType::WindowCumeDist => PeerKind::CumeDist,
            other => {
                return Err(Error::internal(format!(
                    "{} is not a peer function",
                    other.name()
                )));
            }
        };
        let arg_order_idx = wexpr
            .window
            .arg_orders
            .iter()
            .filter_map(|order| shared.register_sink(Some(&order.expression)))
            .collect();
        Ok(Self { kind, arg_order_idx })
    }

    pub(crate) fn global_state(
        &self,
        ctx: &ExecutionContext,
        wexpr: &WindowExpression,
        count: usize,
    ) -> Result<PeerGlobalState> {
        let mut state = PeerGlobalState {
            use_framing: false,
            token_tree: None,
        };
        if !self.arg_order_idx.is_empty() {
            state.use_framing = true;
            // A secondary order that is a prefix of the partition order needs no tree.
            let arg_orders = &wexpr.window.arg_orders;
            if shared_orders(&wexpr.window.orders, arg_orders) != arg_orders.len() {
                state.token_tree = Some(WindowMergeSortTree::new(
                    ctx,
                    arg_orders,
                    &self.arg_order_idx,
                    count,
                    false,
                )?);
            }
        }
        Ok(state)
    }

    pub(crate) fn sink(global: &mut PeerGlobalState, sink_chunk: &DataChunk, input_idx: usize) -> Result<()> {
        if let Some(tree) = &mut global.token_tree {
            tree.sink(sink_chunk, input_idx, None)?;
        }
        Ok(())
    }

    pub(crate) fn finalize(global: &mut PeerGlobalState) -> Result<()> {
        if let Some(tree) = &mut global.token_tree {
            tree.build_token_tree()?;
        }
        Ok(())
    }

    pub(crate) fn evaluate(
        &self,
        global: &PeerGlobalState,
        local: &mut PeerLocalState,
        input: &EvaluateInput<'_>,
    ) -> Vector {
        match self.kind {
            PeerKind::Rank => Self::rank(global, local, input),
            PeerKind::DenseRank => Self::dense_rank(local, input),
            PeerKind::PercentRank => Self::percent_rank(global, local, input),
            PeerKind::CumeDist => Self::cume_dist(global, input),
        }
    }

    /// Resets the rank counters to the row before `row_idx`.
    fn reset(local: &mut PeerLocalState, input: &EvaluateInput<'_>) {
        let bounds = input.bounds;
        let (partition_begin, peer_begin) = (
            bounds[WindowBound::PartitionBegin][0],
            bounds[WindowBound::PeerBegin][0],
        );
        local.rank = (peer_begin - partition_begin) as u64 + 1;
        local.rank_equal = (input.row_idx - peer_begin) as u64;
    }

    fn rank(global: &PeerGlobalState, local: &mut PeerLocalState, input: &EvaluateInput<'_>) -> Vector {
        let EvaluateInput {
            bounds,
            row_idx,
            count,
            ..
        } = *input;
        let mut result = Vector::flat(LogicalType::BIGINT, count);
        let data = &mut result.values_mut::<i64>()[..count];
        if global.use_framing {
            let begins = &bounds[WindowBound::FrameBegin];
            let ends = &bounds[WindowBound::FrameEnd];
            let peers = &bounds[WindowBound::PeerBegin];
            for (i, value) in data.iter_mut().enumerate() {
                *value = match &global.token_tree {
                    Some(tree) => tree.rank(begins[i], ends[i], row_idx + i) as i64,
                    // Clamp the peer group to the frame.
                    None => (begins[i].max(peers[i]) - begins[i] + 1) as i64,
                };
            }
            return result;
        }
        Self::reset(local, input);
        let partition_begins = &bounds[WindowBound::PartitionBegin];
        let peer_begins = &bounds[WindowBound::PeerBegin];
        for (i, value) in data.iter_mut().enumerate() {
            local.next_rank(partition_begins[i], peer_begins[i], row_idx + i);
            *value = local.rank as i64;
        }
        result
    }

    fn dense_rank(local: &mut PeerLocalState, input: &EvaluateInput<'_>) -> Vector {
        let EvaluateInput {
            bounds,
            row_idx,
            count,
            group,
            ..
        } = *input;
        let partition_begins = &bounds[WindowBound::PartitionBegin];
        let peer_begins = &bounds[WindowBound::PeerBegin];
        // Resetting rescans the mask, so only do it when not resuming where we left off.
        if local.row_idx != Some(row_idx) {
            Self::reset(local, input);
            // The previous dense rank is the number of peer group starts before the row.
            local.dense_rank = group.order_mask.count_valid(partition_begins[0], row_idx) as u64;
        }
        let mut result = Vector::flat(LogicalType::BIGINT, count);
        let data = &mut result.values_mut::<i64>()[..count];
        for (i, value) in data.iter_mut().enumerate() {
            local.next_rank(partition_begins[i], peer_begins[i], row_idx + i);
            *value = local.dense_rank as i64;
        }
        local.row_idx = Some(row_idx + count);
        result
    }

    fn percent_rank(
        global: &PeerGlobalState,
        local: &mut PeerLocalState,
        input: &EvaluateInput<'_>,
    ) -> Vector {
        let EvaluateInput {
            bounds,
            row_idx,
            count,
            ..
        } = *input;
        let mut result = Vector::flat(LogicalType::DOUBLE, count);
        let data = &mut result.values_mut::<f64>()[..count];
        if global.use_framing {
            let begins = &bounds[WindowBound::FrameBegin];
            let ends = &bounds[WindowBound::FrameEnd];
            let peers = &bounds[WindowBound::PeerBegin];
            for (i, value) in data.iter_mut().enumerate() {
                let rank = match &global.token_tree {
                    Some(tree) => tree.rank(begins[i], ends[i], row_idx + i) as u64,
                    None => (begins[i].max(peers[i]) - begins[i] + 1) as u64,
                };
                *value = percent_rank(begins[i], ends[i], rank);
            }
            return result;
        }
        Self::reset(local, input);
        let partition_begins = &bounds[WindowBound::PartitionBegin];
        let partition_ends = &bounds[WindowBound::PartitionEnd];
        let peer_begins = &bounds[WindowBound::PeerBegin];
        for (i, value) in data.iter_mut().enumerate() {
            local.next_rank(partition_begins[i], peer_begins[i], row_idx + i);
            *value = percent_rank(partition_begins[i], partition_ends[i], local.rank);
        }
        result
    }

    fn cume_dist(global: &PeerGlobalState, input: &EvaluateInput<'_>) -> Vector {
        let EvaluateInput {
            bounds,
            row_idx,
            count,
            ..
        } = *input;
        let mut result = Vector::flat(LogicalType::DOUBLE, count);
        let data = &mut result.values_mut::<f64>()[..count];
        let peer_ends = &bounds[WindowBound::PeerEnd];
        if global.use_framing {
            let begins = &bounds[WindowBound::FrameBegin];
            let ends = &bounds[WindowBound::FrameEnd];
            for (i, value) in data.iter_mut().enumerate() {
                let peer_end = match &global.token_tree {
                    Some(tree) => tree.peer_end(begins[i], ends[i], row_idx + i),
                    // Clamp the peer end to the frame.
                    None => peer_ends[i].min(ends[i]),
                };
                *value = cume_dist(begins[i], ends[i], peer_end);
            }
            return result;
        }
        let begins = &bounds[WindowBound::PartitionBegin];
        let ends = &bounds[WindowBound::PartitionEnd];
        for (i, value) in data.iter_mut().enumerate() {
            *value = cume_dist(begins[i], ends[i], peer_ends[i]);
        }
        result
    }
}
