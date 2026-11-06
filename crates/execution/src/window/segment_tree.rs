//! Segment tree aggregation for arbitrary frames (DuckDB's `window_segment_tree.cpp`; see
//! Leis et al., "Efficient Processing of Window Functions in Analytical SQL Queries",
//! VLDB 2015).
//!
//! Level 1 nodes aggregate `TREE_FANOUT` input rows, higher nodes `TREE_FANOUT` nodes of
//! the level below. A frame is aggregated from the nodes covering its middle plus the
//! ragged input rows at its ends. The order in which values and nodes are combined is the
//! same as DuckDB's, so floating-point results match bit for bit.

use rustdb_ast::WindowExcludeMode;
use rustdb_common::{Result, STANDARD_VECTOR_SIZE};
use rustdb_function::StatePtr;
use rustdb_function::aggregate::OrderDependence;
use rustdb_types::Vector;

use super::aggregate_states::{ScratchStates, WindowAggregateStates};
use super::aggregator::{FlushBuffer, WindowAggregator};
use super::boundaries_state::{WindowBound, WindowBounds};
use super::mask::WindowMask;

/// Fan-out of the tree; divides `STANDARD_VECTOR_SIZE`.
const TREE_FANOUT: usize = 16;

/// Whether the segment tree can compute `aggregator` (DuckDB's `CanAggregate`).
pub(crate) fn can_aggregate(aggregator: &WindowAggregator, has_arg_orders: bool) -> bool {
    aggregator.function.ops.can_aggregate() && !aggregator.distinct && !has_arg_orders
}

/// The tree of one hash group (DuckDB's `WindowSegmentTreeGlobalState`).
#[derive(Debug)]
pub(crate) struct SegmentTreeGlobalState {
    /// The internal nodes of all levels, level by level.
    levels_flat_native: WindowAggregateStates,
    /// For each level, the index of its first node in `levels_flat_native`.
    levels_flat_start: Vec<usize>,
}

/// The data a tree part reads.
pub(crate) struct TreeInputs<'a> {
    aggregator: &'a WindowAggregator,
    /// The argument columns of the whole hash group.
    inputs: &'a [Vector],
    filter_mask: &'a WindowMask,
    levels: &'a SegmentTreeGlobalState,
}

impl<'a> TreeInputs<'a> {
    /// The inputs of an evaluation against the built tree `levels`.
    pub(crate) fn new(
        aggregator: &'a WindowAggregator,
        inputs: &'a [Vector],
        filter_mask: &'a WindowMask,
        levels: &'a SegmentTreeGlobalState,
    ) -> Self {
        Self {
            aggregator,
            inputs,
            filter_mask,
            levels,
        }
    }

    /// Node `index` of the level stored at `level_start`.
    fn node(&self, level_start: usize, index: usize) -> StatePtr {
        self.levels
            .levels_flat_native
            .state(self.levels.levels_flat_start[level_start] + index)
    }
}

/// Which part of an `EXCLUDE`-split frame is evaluated (DuckDB's `FramePart`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FramePart {
    Full,
    Left,
    Right,
}

/// Evaluation of frames against the tree (DuckDB's `WindowSegmentTreePart`).
#[derive(Debug)]
pub(crate) struct SegmentTreePart {
    order_insensitive: bool,
    exclude_mode: WindowExcludeMode,
    /// One result state per output row.
    states: ScratchStates,
    buffer: FlushBuffer,
    /// Right-side ranges of order-sensitive aggregates, per level.
    right_stack: Vec<(usize, usize)>,
}

/// A thread's state (DuckDB's `WindowSegmentTreeLocalState`).
#[derive(Debug, Default)]
pub(crate) struct SegmentTreeLocalState {
    part: Option<SegmentTreePart>,
    /// The part right of the excluded rows, for `EXCLUDE`.
    right_part: Option<SegmentTreePart>,
}

impl SegmentTreePart {
    fn new(aggregator: &WindowAggregator) -> Result<Self> {
        Ok(Self {
            order_insensitive: aggregator.function.order_dependence == OrderDependence::NotOrderDependent,
            exclude_mode: aggregator.exclude_mode,
            states: ScratchStates::new(aggregator.function.ops.clone(), STANDARD_VECTOR_SIZE)?,
            buffer: FlushBuffer::default(),
            right_stack: Vec::new(),
        })
    }

    /// Adds rows `[begin, end)` of level `level` to `target` (DuckDB's
    /// `WindowSegmentValue`): input rows (passing the filter) at level 0, tree nodes above.
    fn segment_value(
        buffer: &mut FlushBuffer,
        tree: &TreeInputs<'_>,
        level: usize,
        begin: usize,
        end: usize,
        target: StatePtr,
    ) -> Result<()> {
        if begin == end || tree.inputs.is_empty() {
            return Ok(());
        }
        if level == 0 {
            for row in begin..end {
                if tree.filter_mask.is_valid(row) {
                    buffer.update(tree.aggregator, tree.inputs, target, row)?;
                }
            }
        } else {
            for index in begin..end {
                buffer.combine(tree.aggregator, tree.node(level - 1, index), target)?;
            }
        }
        Ok(())
    }

    /// Aggregates frames `[begins, ends)` clamped to `bounds` into the result states.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors DuckDB's WindowSegmentTreePart::Evaluate"
    )]
    fn evaluate(
        &mut self,
        tree: &TreeInputs<'_>,
        begins: &[usize],
        ends: &[usize],
        bounds: Option<&[usize]>,
        count: usize,
        row_idx: usize,
        frame_part: FramePart,
    ) -> Result<()> {
        self.states.begin(count)?;
        if self.order_insensitive {
            // Shared tree nodes first, then the ragged leaves.
            self.evaluate_upper_levels(tree, begins, ends, bounds, count, row_idx, frame_part)?;
            self.evaluate_leaves(
                tree,
                begins,
                ends,
                bounds,
                count,
                row_idx,
                frame_part,
                FramePart::Full,
            )
        } else {
            // In order: left leaves, nodes, right leaves.
            self.evaluate_leaves(
                tree,
                begins,
                ends,
                bounds,
                count,
                row_idx,
                frame_part,
                FramePart::Left,
            )?;
            self.evaluate_upper_levels(tree, begins, ends, bounds, count, row_idx, frame_part)?;
            self.evaluate_leaves(
                tree,
                begins,
                ends,
                bounds,
                count,
                row_idx,
                frame_part,
                FramePart::Right,
            )
        }
    }

    /// The full frame range used for clamping (DuckDB's `FrameBegins` / `FrameEnds`).
    fn frame_ranges<'a>(
        begins: &'a [usize],
        ends: &'a [usize],
        bounds: Option<&'a [usize]>,
        frame_part: FramePart,
    ) -> (&'a [usize], &'a [usize]) {
        match (frame_part, bounds) {
            (FramePart::Right, Some(bounds)) => (bounds, ends),
            (FramePart::Left, Some(bounds)) => (begins, bounds),
            _ => (begins, ends),
        }
    }

    #[allow(clippy::too_many_arguments, reason = "mirrors DuckDB's EvaluateUpperLevels")]
    fn evaluate_upper_levels(
        &mut self,
        tree: &TreeInputs<'_>,
        begins: &[usize],
        ends: &[usize],
        bounds: Option<&[usize]>,
        count: usize,
        row_idx: usize,
        frame_part: FramePart,
    ) -> Result<()> {
        let exclude_current = self.exclude_mode == WindowExcludeMode::CurrentRow;
        let begin_on_curr_row = frame_part == FramePart::Right && exclude_current;
        let end_on_curr_row = frame_part == FramePart::Left && exclude_current;
        let (frame_begins, frame_ends) = Self::frame_ranges(begins, ends, bounds, frame_part);
        let max_level = tree.levels.levels_flat_start.len() + 1;
        self.right_stack.clear();
        self.right_stack.resize(max_level, (0, 0));

        // Share adjacent identical tree aggregations.
        let (mut prev_begin, mut prev_end) = (1, 0);
        let mut prev_state: Option<StatePtr> = None;
        for rid in 0..count {
            let cur_row = row_idx + rid;
            let state = self.states.state(rid);
            let mut begin = (if begin_on_curr_row {
                cur_row + 1
            } else {
                begins[rid]
            })
            .max(frame_begins[rid]);
            let mut end = (if end_on_curr_row { cur_row } else { ends[rid] }).min(frame_ends[rid]);
            if begin >= end {
                continue;
            }
            let mut right_max = 0;
            for level in 0..max_level {
                let mut parent_begin = begin / TREE_FANOUT;
                let parent_end = end / TREE_FANOUT;
                if let Some(previous) = prev_state
                    && level == 1
                    && begin == prev_begin
                    && end == prev_end
                {
                    // Just combine the previous row's tree aggregate.
                    self.buffer.combine(tree.aggregator, previous, state)?;
                    break;
                }
                if self.order_insensitive && level == 1 {
                    prev_state = Some(state);
                    prev_begin = begin;
                    prev_end = end;
                }
                if parent_begin == parent_end {
                    if level > 0 {
                        Self::segment_value(&mut self.buffer, tree, level, begin, end, state)?;
                    }
                    break;
                }
                let group_begin = parent_begin * TREE_FANOUT;
                if begin != group_begin {
                    if level > 0 {
                        Self::segment_value(
                            &mut self.buffer,
                            tree,
                            level,
                            begin,
                            group_begin + TREE_FANOUT,
                            state,
                        )?;
                    }
                    parent_begin += 1;
                }
                let group_end = parent_end * TREE_FANOUT;
                if end != group_end && level > 0 {
                    if self.order_insensitive {
                        Self::segment_value(&mut self.buffer, tree, level, group_end, end, state)?;
                    } else {
                        self.right_stack[level] = (group_end, end);
                        right_max = level;
                    }
                }
                begin = parent_begin;
                end = parent_end;
            }
            // Order-sensitive aggregates add the right side ranges left to right.
            for level in (1..=right_max).rev() {
                let (group_end, end) = self.right_stack[level];
                if end != 0 {
                    Self::segment_value(&mut self.buffer, tree, level, group_end, end, state)?;
                    self.right_stack[level] = (0, 0);
                }
            }
        }
        self.buffer.flush_combines(tree.aggregator)
    }

    #[allow(clippy::too_many_arguments, reason = "mirrors DuckDB's EvaluateLeaves")]
    fn evaluate_leaves(
        &mut self,
        tree: &TreeInputs<'_>,
        begins: &[usize],
        ends: &[usize],
        bounds: Option<&[usize]>,
        count: usize,
        row_idx: usize,
        frame_part: FramePart,
        leaf_part: FramePart,
    ) -> Result<()> {
        // Order-sensitive aggregates add the left leaves before the tree nodes and the
        // right leaves after; the current row is the leftmost value of the right side.
        let compute_left = leaf_part != FramePart::Right;
        let compute_right = leaf_part != FramePart::Left;
        let exclude_current = self.exclude_mode == WindowExcludeMode::CurrentRow;
        let begin_on_curr_row = frame_part == FramePart::Right && exclude_current;
        let end_on_curr_row = frame_part == FramePart::Left && exclude_current;
        // EXCLUDE TIES keeps the current row besides the part right of the peer group.
        let add_curr_row =
            compute_left && frame_part == FramePart::Right && self.exclude_mode == WindowExcludeMode::Ties;
        let (frame_begins, frame_ends) = Self::frame_ranges(begins, ends, bounds, frame_part);
        for rid in 0..count {
            let cur_row = row_idx + rid;
            let state = self.states.state(rid);
            let frame_begin = frame_begins[rid];
            let begin = (if begin_on_curr_row {
                cur_row + 1
            } else {
                begins[rid]
            })
            .max(frame_begin);
            let frame_end = frame_ends[rid];
            let end = (if end_on_curr_row { cur_row } else { ends[rid] }).min(frame_end);
            if add_curr_row && frame_begin <= cur_row && cur_row < frame_end {
                Self::segment_value(&mut self.buffer, tree, 0, cur_row, cur_row + 1, state)?;
            }
            if begin >= end {
                continue;
            }
            let parent_begin = begin / TREE_FANOUT;
            let parent_end = end / TREE_FANOUT;
            if parent_begin == parent_end {
                if compute_left {
                    Self::segment_value(&mut self.buffer, tree, 0, begin, end, state)?;
                }
                continue;
            }
            let group_begin = parent_begin * TREE_FANOUT;
            if begin != group_begin && compute_left {
                Self::segment_value(&mut self.buffer, tree, 0, begin, group_begin + TREE_FANOUT, state)?;
            }
            let group_end = parent_end * TREE_FANOUT;
            if end != group_end && compute_right {
                Self::segment_value(&mut self.buffer, tree, 0, group_end, end, state)?;
            }
        }
        self.buffer.flush_updates(tree.aggregator, tree.inputs)
    }
}

impl SegmentTreeGlobalState {
    /// Builds the tree over the `inputs` of a hash group of `group_count` rows (DuckDB's
    /// global state constructor plus `WindowSegmentTreeLocalState::Finalize`).
    pub(crate) fn build(
        aggregator: &WindowAggregator,
        inputs: &[Vector],
        filter_mask: &WindowMask,
        group_count: usize,
    ) -> Result<Self> {
        // The node count of every level; level 0 is the data itself.
        let mut levels_flat_start = vec![0];
        let mut levels_flat_offset = 0;
        loop {
            let level = levels_flat_start.len() - 1;
            let level_size = if level == 0 {
                group_count
            } else {
                levels_flat_offset - levels_flat_start[level - 1]
            };
            if level_size <= 1 {
                break;
            }
            levels_flat_offset += level_size.div_ceil(TREE_FANOUT);
            levels_flat_start.push(levels_flat_offset);
        }
        // Corner case: a single element.
        let nodes = levels_flat_offset.max(1);
        let tree = Self {
            levels_flat_native: WindowAggregateStates::new(aggregator.function.ops.clone(), nodes)?,
            levels_flat_start,
        };

        let mut buffer = FlushBuffer::default();
        let builder = TreeInputs {
            aggregator,
            inputs,
            filter_mask,
            levels: &tree,
        };
        for level in 0..tree.levels_flat_start.len() {
            let level_size = if level == 0 {
                group_count
            } else {
                tree.levels_flat_start[level] - tree.levels_flat_start[level - 1]
            };
            if level_size <= 1 {
                break;
            }
            for build_idx in 0..level_size.div_ceil(TREE_FANOUT) {
                let pos = build_idx * TREE_FANOUT;
                let target = tree
                    .levels_flat_native
                    .state(tree.levels_flat_start[level] + build_idx);
                SegmentTreePart::segment_value(
                    &mut buffer,
                    &builder,
                    level,
                    pos,
                    level_size.min(pos + TREE_FANOUT),
                    target,
                )?;
                if level > 0 {
                    buffer.flush_combines(aggregator)?;
                } else {
                    buffer.flush_updates(aggregator, inputs)?;
                }
            }
        }
        Ok(tree)
    }
}

impl SegmentTreeLocalState {
    /// Aggregates the frames of `count` rows into a result vector (DuckDB's
    /// `WindowSegmentTreeLocalState::Evaluate`).
    pub(crate) fn evaluate(
        &mut self,
        tree: &TreeInputs<'_>,
        bounds: &WindowBounds,
        count: usize,
        row_idx: usize,
    ) -> Result<Vector> {
        let aggregator = tree.aggregator;
        let window_begin = &bounds[WindowBound::FrameBegin];
        let window_end = &bounds[WindowBound::FrameEnd];
        let peer_begin = &bounds[WindowBound::PeerBegin];
        let peer_end = &bounds[WindowBound::PeerEnd];
        if self.part.is_none() {
            self.part = Some(SegmentTreePart::new(aggregator)?);
        }
        let Some(part) = &mut self.part else {
            unreachable!("created above")
        };
        if aggregator.exclude_mode == WindowExcludeMode::NoOther {
            part.evaluate(
                tree,
                window_begin,
                window_end,
                None,
                count,
                row_idx,
                FramePart::Full,
            )?;
        } else {
            // With EXCLUDE CURRENT ROW both sides need the current row's peers.
            let exclude_current = aggregator.exclude_mode == WindowExcludeMode::CurrentRow;
            // 1. The part left of the excluded rows.
            let middle = if exclude_current { peer_end } else { peer_begin };
            part.evaluate(
                tree,
                window_begin,
                middle,
                Some(window_end),
                count,
                row_idx,
                FramePart::Left,
            )?;
            // 2. The part right of them, in a second set of states.
            if self.right_part.is_none() {
                self.right_part = Some(SegmentTreePart::new(aggregator)?);
            }
            let Some(right) = &mut self.right_part else {
                unreachable!("created above")
            };
            let middle = if exclude_current { peer_begin } else { peer_end };
            right.evaluate(
                tree,
                middle,
                window_end,
                Some(window_begin),
                count,
                row_idx,
                FramePart::Right,
            )?;
            // 3. Combine the right part into the left.
            part.states.combine_from(&right.states, aggregator.input())?;
            right.states.destroy();
        }
        let mut result = Vector::flat(aggregator.result_type.clone(), count);
        part.states.finish(aggregator.input(), &mut result)?;
        Ok(result)
    }
}
