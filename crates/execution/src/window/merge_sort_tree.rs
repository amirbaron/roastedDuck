//! Merge sort trees (DuckDB's `execution/merge_sort_tree.hpp`) and the window variant
//! that builds its leaves by sorting the hash group (`window_merge_sort_tree.cpp`).
//!
//! Level 0 holds the elements in row order; each higher level holds runs of `FANOUT^level`
//! elements merged into sorted order, with fractional cascading pointers into the level
//! below. This answers "the n-th row of a frame in another order" ([`MergeSortTree::select_nth`])
//! and "how many rows of `[lower, upper)` are below a value" ([`MergeSortTree::aggregate_lower_bound`])
//! in logarithmic time. See Vogelsgesang et al., "Efficient Evaluation of Arbitrarily-Framed
//! Holistic SQL Aggregates and Window Functions" (SIGMOD 2022).

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fmt::Debug;

use rustdb_common::{Error, Result};
use rustdb_planner::expression::BoundOrderByNode;
use rustdb_row::sort::{NullOrder, OrderModifiers, OrderType as SortOrder, Sort, SortColumn, SortConfig};
use rustdb_types::{DataChunk, LogicalType, Vector};

use super::boundaries_state::FrameBounds;
use super::collection::delta_flags;
use super::order_modifiers;
use crate::ExecutionContext;

/// Fan-out of the tree (DuckDB's default `F`).
pub(crate) const FANOUT: usize = 32;
/// Distance between fractional cascading pointers (DuckDB's default `C`).
pub(crate) const CASCADING: usize = 32;

/// An element of a merge sort tree.
pub(crate) trait TreeElement: Copy + Ord + Debug + Default {}

impl TreeElement for u32 {}
impl TreeElement for u64 {}
impl TreeElement for (u64, u64) {}

/// An element or offset that is a row index.
pub(crate) trait TreeIndex: TreeElement {
    fn to_usize(self) -> usize;
    fn from_usize(value: usize) -> Self;
}

impl TreeIndex for u32 {
    #[inline]
    fn to_usize(self) -> usize {
        self as usize
    }
    #[inline]
    fn from_usize(value: usize) -> Self {
        value as u32
    }
}

impl TreeIndex for u64 {
    #[inline]
    fn to_usize(self) -> usize {
        self as usize
    }
    #[inline]
    fn from_usize(value: usize) -> Self {
        value as u64
    }
}

/// One level: its elements and the cascading pointers into the level below.
#[derive(Debug, Default)]
pub(crate) struct Level<E, O> {
    pub elements: Vec<E>,
    pub cascades: Vec<O>,
}

/// A merge sort tree over elements `E` with offsets `O` (DuckDB's `MergeSortTree`).
#[derive(Debug, Default)]
pub(crate) struct MergeSortTree<E, O> {
    pub tree: Vec<Level<E, O>>,
}

/// The lowest level that has cascading pointers (`LowestCascadingLevel`).
pub(crate) const fn lowest_cascading_level() -> usize {
    let mut level = 0;
    let mut level_width = 1;
    while level_width <= CASCADING {
        level += 1;
        level_width *= FANOUT;
    }
    level
}

impl<E: TreeElement, O: TreeIndex> MergeSortTree<E, O> {
    /// Allocates the levels for `count` elements and returns the (unset) lowest level.
    pub(crate) fn allocate(&mut self, count: usize) -> &mut Vec<E> {
        self.tree.push(Level {
            elements: vec![E::default(); count],
            cascades: Vec::new(),
        });
        let mut child_run_length = 1;
        while child_run_length < count {
            let run_length = child_run_length * FANOUT;
            let num_runs = count.div_ceil(run_length);
            // Cascading pointers only if there is room for them.
            let cascades = if run_length > CASCADING {
                vec![O::default(); FANOUT * num_runs * (run_length / CASCADING + 2)]
            } else {
                Vec::new()
            };
            self.tree.push(Level {
                elements: vec![E::default(); count],
                cascades,
            });
            child_run_length = run_length;
        }
        &mut self.tree[0].elements
    }

    /// The elements in row order.
    pub(crate) fn lowest_level(&self) -> &[E] {
        &self.tree[0].elements
    }

    pub(crate) fn lowest_level_mut(&mut self) -> &mut [E] {
        &mut self.tree[0].elements
    }

    /// Element `i` of the lowest level.
    pub(crate) fn nth_element(&self, i: usize) -> Option<E> {
        self.tree.first().and_then(|level| level.elements.get(i)).copied()
    }

    /// Merges every level from the one below (DuckDB's `Build`, single-threaded).
    pub(crate) fn build(&mut self) {
        let count = self.tree.first().map_or(0, |level| level.elements.len());
        let mut run_length = FANOUT;
        for level_idx in 1..self.tree.len() {
            let num_runs = count.div_ceil(run_length);
            for run_idx in 0..num_runs {
                self.build_run(level_idx, run_idx);
            }
            run_length *= FANOUT;
        }
    }

    /// Merges the child runs of one run of `level_idx` (DuckDB's `BuildRun`). Ties between
    /// child runs go to the leftmost run, like DuckDB's tournament tree.
    fn build_run(&mut self, level_idx: usize, run_idx: usize) {
        let mut child_run_length = 1;
        let mut run_length = FANOUT;
        for _ in 1..level_idx {
            child_run_length = run_length;
            run_length *= FANOUT;
        }
        let (lower, upper) = self.tree.split_at_mut(level_idx);
        let child = &lower[level_idx - 1].elements;
        let Level { elements, cascades } = &mut upper[0];
        let count = elements.len();

        // Position markers for scanning the children.
        let mut bounds = [(0usize, 0usize); FANOUT];
        let mut players = BinaryHeap::with_capacity(FANOUT);
        let child_base = run_idx * run_length;
        for (child_run, bound) in bounds.iter_mut().enumerate() {
            let child_idx = child_base + child_run * child_run_length;
            *bound = (child_idx.min(count), (child_idx + child_run_length).min(count));
            if bound.0 != bound.1 {
                players.push(Reverse((child[child_idx], child_run)));
            }
        }
        let mut element_idx = child_base;
        let mut cascade_idx = FANOUT * run_idx * (run_length / CASCADING + 2);
        while let Some(Reverse((winner, child_run))) = players.pop() {
            // Fractional cascading pointers on fraction boundaries.
            if !cascades.is_empty() && element_idx.is_multiple_of(CASCADING) {
                for bound in &bounds {
                    cascades[cascade_idx] = O::from_usize(bound.0);
                    cascade_idx += 1;
                }
            }
            elements[element_idx] = winner;
            element_idx += 1;
            let bound = &mut bounds[child_run];
            bound.0 += 1;
            if bound.0 < bound.1 {
                players.push(Reverse((child[bound.0], child_run)));
            }
        }
        // Terminal cascade pointers.
        if !cascades.is_empty() {
            for _ in 0..2 {
                for bound in &bounds {
                    cascades[cascade_idx] = O::from_usize(bound.0);
                    cascade_idx += 1;
                }
            }
        }
    }

    /// Calls `aggregate(level, run_begin, run_pos)` for the runs covering `[lower, upper)`,
    /// where `run_pos` is the position of the first element `>= needle` in the run
    /// (DuckDB's `AggregateLowerBound`).
    #[allow(
        clippy::too_many_lines,
        reason = "a direct port of DuckDB's AggregateLowerBound"
    )]
    pub(crate) fn aggregate_lower_bound(
        &self,
        lower: usize,
        upper: usize,
        needle: E,
        mut aggregate: impl FnMut(usize, usize, usize),
    ) {
        if lower >= upper {
            return;
        }
        let lower_bound =
            |data: &[E], begin: usize, end: usize| begin + data[begin..end].partition_point(|e| *e < needle);
        let cascade = |cascades: &[O], index: usize| cascades[index].to_usize();

        // Find the entry point into the tree.
        let mut run_idx = (lower, upper - 1);
        let mut level_width = 1;
        let mut level = 0;
        let mut prev_run_idx = (0, 0);
        let mut curr;
        if run_idx.0 == run_idx.1 {
            curr = (run_idx.0, run_idx.0);
        } else {
            loop {
                prev_run_idx.1 = run_idx.1;
                run_idx.0 /= FANOUT;
                run_idx.1 /= FANOUT;
                level_width *= FANOUT;
                level += 1;
                if run_idx.0 == run_idx.1 {
                    break;
                }
            }
            curr = (prev_run_idx.1 * level_width / FANOUT, 0);
            curr.1 = curr.0;
        }

        // Aggregate the levels with cascading pointers.
        if level > lowest_cascading_level() {
            let mut cascading_idx;
            {
                let entry_first = run_idx.0 * level_width;
                let entry_second = (entry_first + level_width).min(self.tree[0].elements.len());
                let entry_idx = lower_bound(&self.tree[level].elements, entry_first, entry_second);
                let idx = (entry_idx / CASCADING + 2 * (entry_first / level_width)) * FANOUT;
                // Shift the initial pointers: the top level is not on a run boundary.
                let correction = prev_run_idx.1 - run_idx.1 * FANOUT;
                // Unsigned wrap-around as in DuckDB; the left side moves back into range below.
                cascading_idx = (idx.wrapping_sub(FANOUT - correction), idx + correction);
            }
            loop {
                level -= 1;
                level_width /= FANOUT;
                let level_data = &self.tree[level].elements;
                let cascades = &self.tree[level + 1].cascades;
                // Left side: completely contained runs.
                cascading_idx.0 = cascading_idx.0.wrapping_add(FANOUT - 1);
                while curr.0 - lower >= level_width {
                    let run_pos = lower_bound(
                        level_data,
                        cascade(cascades, cascading_idx.0),
                        cascade(cascades, cascading_idx.0 + FANOUT),
                    );
                    aggregate(level, curr.0 - level_width, run_pos);
                    curr.0 -= level_width;
                    cascading_idx.0 = cascading_idx.0.wrapping_sub(1);
                }
                // The partial run gives the cascading entry point of the next level.
                if curr.0 != lower {
                    let idx = lower_bound(
                        level_data,
                        cascade(cascades, cascading_idx.0),
                        cascade(cascades, cascading_idx.0 + FANOUT),
                    );
                    cascading_idx.0 = (idx / CASCADING + 2 * (lower / level_width)) * FANOUT;
                }
                // Right side: completely contained runs.
                while upper - curr.1 >= level_width {
                    let run_pos = lower_bound(
                        level_data,
                        cascade(cascades, cascading_idx.1),
                        cascade(cascades, cascading_idx.1 + FANOUT),
                    );
                    aggregate(level, curr.1, run_pos);
                    curr.1 += level_width;
                    cascading_idx.1 += 1;
                }
                if curr.1 != upper {
                    let idx = lower_bound(
                        level_data,
                        cascade(cascades, cascading_idx.1),
                        cascade(cascades, cascading_idx.1 + FANOUT),
                    );
                    cascading_idx.1 = (idx / CASCADING + 2 * (upper / level_width)) * FANOUT;
                }
                if level < lowest_cascading_level() {
                    break;
                }
            }
        }

        // Lower levels without cascading pointers.
        if level > 0 {
            loop {
                level -= 1;
                if level == 0 {
                    break;
                }
                level_width /= FANOUT;
                let level_data = &self.tree[level].elements;
                while curr.0 - lower >= level_width {
                    let begin = curr.0 - level_width;
                    aggregate(level, begin, lower_bound(level_data, begin, curr.0));
                    curr.0 -= level_width;
                }
                while upper - curr.1 >= level_width {
                    let begin = curr.1;
                    aggregate(level, begin, lower_bound(level_data, begin, begin + level_width));
                    curr.1 += level_width;
                }
            }
        }

        // The last level, element by element.
        let level_data = &self.tree[0].elements;
        for (run_begin, &element) in level_data.iter().enumerate().take(curr.0).skip(lower) {
            aggregate(level, run_begin, run_begin + usize::from(element < needle));
        }
        while curr.1 != upper {
            let run_begin = curr.1;
            aggregate(
                level,
                run_begin,
                run_begin + usize::from(level_data[run_begin] < needle),
            );
            curr.1 += 1;
        }
    }
}

impl<E: TreeIndex, O: TreeIndex> MergeSortTree<E, O> {
    /// The position of the `n`th (0-based) element that falls in one of `frames`, found by
    /// a top-down traversal. Returns `(position, 0)`, or `(_, overflow)` when there are not
    /// enough elements (DuckDB's `SelectNth`).
    #[allow(clippy::too_many_lines, reason = "a direct port of DuckDB's SelectNth")]
    pub(crate) fn select_nth(&self, frames: &[FrameBounds], mut n: usize) -> (usize, usize) {
        let in_frames = |value: usize| {
            frames
                .iter()
                .filter(|f| value >= f.start && value < f.end)
                .count()
        };
        let below = |data: &[E], begin: usize, end: usize, bound: usize| {
            begin + data[begin..end].partition_point(|e| e.to_usize() < bound)
        };
        let mut result = 0;

        // A 0/1-element tree.
        if self.tree.len() < 2 {
            n += 1;
            let Some(&first) = self.tree.first().and_then(|level| level.elements.first()) else {
                return (result, n);
            };
            n -= in_frames(first.to_usize());
            return (result, n);
        }

        // The first level contains a single run, so only its cascading pointers matter.
        let mut level_no = self.tree.len() - 2;
        let mut level_width = FANOUT.pow(level_no as u32);

        // Levels with cascading pointers.
        let min_cascaded = lowest_cascading_level();
        if level_no > min_cascaded {
            let mut cascades = [(0usize, 0usize); 3];
            let level = &self.tree[level_no + 1].elements;
            for (frame, cascade) in frames.iter().zip(&mut cascades) {
                let lower_idx = below(level, 0, level.len(), frame.start);
                let upper_idx = below(level, 0, level.len(), frame.end);
                *cascade = (lower_idx / CASCADING * FANOUT, upper_idx / CASCADING * FANOUT);
            }
            while level_no >= min_cascaded {
                let level_cascades = &self.tree[level_no + 1].cascades;
                let level_data = &self.tree[level_no].elements;
                loop {
                    let mut matched = 0;
                    let mut matches = [(0usize, 0usize); 3];
                    for ((frame, cascade), found) in frames.iter().zip(&cascades).zip(&mut matches) {
                        let lower_begin = level_cascades[cascade.0].to_usize();
                        let lower_end = level_cascades[cascade.0 + FANOUT].to_usize();
                        found.0 = below(level_data, lower_begin, lower_end, frame.start);
                        let upper_begin = level_cascades[cascade.1].to_usize();
                        let upper_end = level_cascades[cascade.1 + FANOUT].to_usize();
                        found.1 = below(level_data, upper_begin, upper_end, frame.end);
                        matched += found.1 - found.0;
                    }
                    if matched > n {
                        // Too many: move down to the leftmost child candidate.
                        for (cascade, found) in cascades.iter_mut().zip(&matches).take(frames.len()) {
                            cascade.0 = (found.0 / CASCADING + 2 * result) * FANOUT;
                            cascade.1 = (found.1 / CASCADING + 2 * result) * FANOUT;
                        }
                        break;
                    }
                    // Not enough in this child: move right.
                    for cascade in cascades.iter_mut().take(frames.len()) {
                        cascade.0 += 1;
                        cascade.1 += 1;
                    }
                    result += 1;
                    n -= matched;
                }
                result *= FANOUT;
                level_width /= FANOUT;
                level_no -= 1;
            }
        }

        // Levels without cascading pointers (except the lowest).
        while level_no > 0 {
            let level = &self.tree[level_no].elements;
            let mut range_begin = result * level_width;
            let mut range_end = range_begin + level_width;
            while range_end < level.len() {
                let mut matched = 0;
                for frame in frames {
                    let lower_match = below(level, range_begin, range_end, frame.start);
                    let upper_match = below(level, lower_match, range_end, frame.end);
                    matched += upper_match - lower_match;
                }
                if matched > n {
                    break;
                }
                range_begin = range_end;
                range_end += level_width;
                result += 1;
                n -= matched;
            }
            result *= FANOUT;
            level_width /= FANOUT;
            level_no -= 1;
        }

        // The lowest level.
        let level_data = &self.tree[0].elements;
        n += 1;
        let limit = (result + FANOUT).min(level_data.len());
        while result < limit {
            n -= in_frames(level_data[result].to_usize());
            if n == 0 {
                break;
            }
            result += 1;
        }
        (result, n)
    }
}

/// The merge sort tree of a [`WindowMergeSortTree`], sized by the row count.
#[derive(Debug)]
pub(crate) enum IndexTree {
    U32(MergeSortTree<u32, u32>),
    U64(MergeSortTree<u64, u64>),
}

/// A merge sort tree whose leaves are built by sorting the hash group by an ORDER BY
/// (DuckDB's `WindowMergeSortTree`). The sort payload is the row index.
#[derive(Debug)]
pub(crate) struct WindowMergeSortTree {
    /// Sink chunk columns of the ORDER BY expressions.
    order_idx: Vec<usize>,
    /// Types of the sorted data: the ORDER BY columns, then the row index.
    scan_types: Vec<LogicalType>,
    /// Number of key columns (the ORDER BY columns, plus the index for unique trees).
    key_count: usize,
    sort: Sort,
    sink: Option<(
        rustdb_row::sort::SortGlobalSinkState,
        rustdb_row::sort::SortLocalSinkState,
    )>,
    pub(crate) tree: IndexTree,
}

impl WindowMergeSortTree {
    /// A tree over `count` rows sorted by `orders` (computed into sink columns
    /// `order_idx`); `unique` breaks ties by row index.
    pub(crate) fn new(
        ctx: &ExecutionContext,
        orders: &[BoundOrderByNode],
        order_idx: &[usize],
        count: usize,
        unique: bool,
    ) -> Result<Self> {
        let (index_type, tree) = if count < u32::MAX as usize {
            (LogicalType::INTEGER, IndexTree::U32(MergeSortTree::default()))
        } else {
            (LogicalType::BIGINT, IndexTree::U64(MergeSortTree::default()))
        };
        let mut scan_types = Vec::with_capacity(orders.len() + 1);
        let mut sort_columns = Vec::with_capacity(orders.len() + 1);
        for (column, order) in orders.iter().enumerate() {
            scan_types.push(order.expression.return_type.clone());
            sort_columns.push(SortColumn {
                column,
                modifiers: order_modifiers(order)?,
                stats: None,
            });
        }
        scan_types.push(index_type);
        // Disambiguate equal keys by the row index if requested.
        if unique {
            sort_columns.push(SortColumn {
                column: orders.len(),
                modifiers: OrderModifiers::new(SortOrder::Ascending, NullOrder::NullsLast),
                stats: None,
            });
        }
        let key_count = sort_columns.len();
        let sort = Sort::new(ctx.env.buffer_manager.clone(), sort_columns, &scan_types, None)?;
        let config = SortConfig {
            threads: 1,
            connections: 1,
            ..SortConfig::default()
        };
        let global = sort.global_sink_state(config);
        let local = sort.local_sink_state();
        Ok(Self {
            order_idx: order_idx.to_vec(),
            scan_types,
            key_count,
            sort,
            sink: Some((global, local)),
            tree,
        })
    }

    /// Adds the rows of `chunk` (starting at row `row_idx` of the hash group), or only
    /// those in `filter` (DuckDB's `WindowMergeSortTreeLocalState::Sink`).
    pub(crate) fn sink(&mut self, chunk: &DataChunk, row_idx: usize, filter: Option<&[u32]>) -> Result<()> {
        let Some((global, local)) = &mut self.sink else {
            return Err(Error::internal("merge sort tree sink after finalize"));
        };
        let count = chunk.len();
        let mut columns: Vec<Vector> = self
            .order_idx
            .iter()
            .map(|&column| chunk.column(column).clone())
            .collect();
        let index_type = self.scan_types.last().cloned().unwrap_or(LogicalType::BIGINT);
        let mut indices = Vector::flat(index_type.clone(), count);
        if index_type == LogicalType::INTEGER {
            for (i, value) in indices.values_mut::<i32>()[..count].iter_mut().enumerate() {
                *value = (row_idx + i) as i32;
            }
        } else {
            for (i, value) in indices.values_mut::<i64>()[..count].iter_mut().enumerate() {
                *value = (row_idx + i) as i64;
            }
        }
        columns.push(indices);
        let sort_chunk = match filter {
            Some(rows) => {
                let sel = rustdb_types::SelectionVector::new(rows.to_vec());
                DataChunk::new(
                    columns.iter().map(|column| column.slice(&sel)).collect(),
                    rows.len(),
                )
            }
            None => DataChunk::new(columns, count),
        };
        if sort_chunk.is_empty() {
            return Ok(());
        }
        self.sort.sink(&sort_chunk, global, local)
    }

    /// Sorts the sunk rows and returns them in order, with the index column last.
    fn sorted(&mut self) -> Result<Vec<DataChunk>> {
        let (global, local) = self
            .sink
            .take()
            .ok_or_else(|| Error::internal("merge sort tree finalized twice"))?;
        self.sort.combine(&global, local)?;
        match self.sort.finalize(global)? {
            Some(source) => source.collect(),
            None => Ok(Vec::new()),
        }
    }

    /// Builds the tree with the sorted row indices as leaves (DuckDB's
    /// `WindowIndexTreeLocalState::BuildLeaves` + `Build`).
    pub(crate) fn build_index_tree(&mut self) -> Result<()> {
        let sorted = self.sorted()?;
        let count: usize = sorted.iter().map(DataChunk::len).sum();
        match &mut self.tree {
            IndexTree::U32(tree) => {
                let leaves = tree.allocate(count);
                let mut offset = 0;
                for chunk in &sorted {
                    let indices = index_column(chunk)?;
                    for (leaf, &index) in leaves[offset..offset + chunk.len()]
                        .iter_mut()
                        .zip(indices.values::<i32>())
                    {
                        *leaf = index as u32;
                    }
                    offset += chunk.len();
                }
                tree.build();
            }
            IndexTree::U64(tree) => {
                let leaves = tree.allocate(count);
                let mut offset = 0;
                for chunk in &sorted {
                    let indices = index_column(chunk)?;
                    for (leaf, &index) in leaves[offset..offset + chunk.len()]
                        .iter_mut()
                        .zip(indices.values::<i64>())
                    {
                        *leaf = index as u64;
                    }
                    offset += chunk.len();
                }
                tree.build();
            }
        }
        Ok(())
    }

    /// Builds the tree with dense peer-group tokens as leaves, indexed by row (DuckDB's
    /// `WindowTokenTree`: `BuildLeaves` computes deltas, `Finished` turns them into tokens).
    pub(crate) fn build_token_tree(&mut self) -> Result<()> {
        let sorted = self.sorted()?;
        let count: usize = sorted.iter().map(DataChunk::len).sum();
        // deltas[i]: whether sorted row i starts a new peer group.
        let mut deltas = vec![false; count];
        let keys: Vec<Vec<&Vector>> = sorted
            .iter()
            .map(|chunk| chunk.columns()[..self.key_count].iter().collect())
            .collect();
        delta_flags(
            &keys,
            &sorted.iter().map(DataChunk::len).collect::<Vec<_>>(),
            &mut deltas,
        )?;
        if let Some(first) = deltas.first_mut() {
            *first = false;
        }
        match &mut self.tree {
            IndexTree::U32(tree) => {
                let tokens = tree.allocate(count);
                assign_tokens(&sorted, &deltas, tokens)?;
                tree.build();
            }
            IndexTree::U64(tree) => {
                let tokens = tree.allocate(count);
                assign_tokens(&sorted, &deltas, tokens)?;
                tree.build();
            }
        }
        Ok(())
    }

    /// The `n`th row of `frames` in the tree's order: `(row, 0)` or `(_, overflow)`
    /// (DuckDB's `WindowIndexTree::SelectNth`).
    pub(crate) fn select_nth(&self, frames: &[FrameBounds], n: usize) -> (usize, usize) {
        match &self.tree {
            IndexTree::U32(tree) => {
                let nth = tree.select_nth(frames, n);
                if nth.1 != 0 {
                    nth
                } else {
                    (tree.nth_element(nth.0).map_or(usize::MAX, TreeIndex::to_usize), 0)
                }
            }
            IndexTree::U64(tree) => {
                let nth = tree.select_nth(frames, n);
                if nth.1 != 0 {
                    nth
                } else {
                    (tree.nth_element(nth.0).map_or(usize::MAX, TreeIndex::to_usize), 0)
                }
            }
        }
    }

    /// The rank of row `row_idx` among `[lower, upper)` (DuckDB's `WindowTokenTree::Rank`).
    pub(crate) fn rank(&self, lower: usize, upper: usize, row_idx: usize) -> usize {
        fn rank<T: TreeIndex>(
            tree: &MergeSortTree<T, T>,
            lower: usize,
            upper: usize,
            row_idx: usize,
        ) -> usize {
            let mut rank = 1;
            let needle = tree.lowest_level()[row_idx];
            tree.aggregate_lower_bound(lower, upper, needle, |_, run_begin, run_pos| {
                rank += run_pos - run_begin;
            });
            rank
        }
        match &self.tree {
            IndexTree::U32(tree) => rank(tree, lower, upper, row_idx),
            IndexTree::U64(tree) => rank(tree, lower, upper, row_idx),
        }
    }

    /// The end of row `row_idx`'s peer group within `[lower, upper)` (DuckDB's
    /// `WindowTokenTree::PeerEnd`).
    pub(crate) fn peer_end(&self, lower: usize, upper: usize, row_idx: usize) -> usize {
        fn next_peer<T: TreeIndex>(
            tree: &MergeSortTree<T, T>,
            lower: usize,
            upper: usize,
            row_idx: usize,
        ) -> usize {
            let mut idx = lower;
            // Tokens are dense, so the next peer group starts at the next token.
            let needle = T::from_usize(tree.lowest_level()[row_idx].to_usize() + 1);
            tree.aggregate_lower_bound(lower, upper, needle, |_, run_begin, run_pos| {
                idx += run_pos - run_begin;
            });
            idx
        }
        match &self.tree {
            IndexTree::U32(tree) => next_peer(tree, lower, upper, row_idx),
            IndexTree::U64(tree) => next_peer(tree, lower, upper, row_idx),
        }
    }
}

/// The row index column (the last one) of a sorted chunk.
fn index_column(chunk: &DataChunk) -> Result<Vector> {
    let column = chunk
        .columns()
        .last()
        .ok_or_else(|| Error::internal("sorted merge sort tree data without index"))?;
    Ok(column.flattened(chunk.len()))
}

/// Writes the token of each sorted row at the row's index (DuckDB's `BuildTokens`).
fn assign_tokens<T: TreeIndex>(sorted: &[DataChunk], deltas: &[bool], tokens: &mut [T]) -> Result<()> {
    let mut token = 0usize;
    let mut position = 0;
    for chunk in sorted {
        let column = index_column(chunk)?;
        let rows: Vec<usize> = if column.logical_type().physical_type() == rustdb_types::PhysicalType::Int32 {
            column.values::<i32>()[..chunk.len()]
                .iter()
                .map(|&row| row as usize)
                .collect()
        } else {
            column.values::<i64>()[..chunk.len()]
                .iter()
                .map(|&row| row as usize)
                .collect()
        };
        for row in rows {
            token += usize::from(deltas[position]);
            position += 1;
            tokens[row] = T::from_usize(token);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree_of(values: &[u32]) -> MergeSortTree<u32, u32> {
        let mut tree = MergeSortTree::default();
        tree.allocate(values.len()).copy_from_slice(values);
        tree.build();
        tree
    }

    #[test]
    fn select_nth_finds_rows_in_sorted_order() {
        // Leaves are row numbers in a secondary order; select the n-th visible one.
        let values: Vec<u32> = (0..5000u32).map(|i| (i * 7919) % 5000).collect();
        let tree = tree_of(&values);
        let frames = [FrameBounds::new(100, 3000)];
        for n in [0, 1, 17, 1000, 2899] {
            let expected = values
                .iter()
                .enumerate()
                .filter(|&(_, &v)| (100..3000).contains(&v))
                .nth(n)
                .map(|(position, _)| position);
            let (position, overflow) = tree.select_nth(&frames, n);
            assert_eq!(overflow, 0);
            assert_eq!(Some(position), expected);
        }
        assert_ne!(tree.select_nth(&frames, 2900).1, 0);
    }

    #[test]
    fn aggregate_lower_bound_counts_smaller_elements() {
        let values: Vec<u32> = (0..3000u32).map(|i| (i * 31 + 7) % 1000).collect();
        let tree = tree_of(&values);
        for (lower, upper, needle) in [(0, 3000, 500), (17, 2047, 3), (1023, 1025, 999), (5, 6, 1000)] {
            let mut counted = 0;
            tree.aggregate_lower_bound(lower, upper, needle, |_, begin, pos| counted += pos - begin);
            let expected = values[lower..upper].iter().filter(|&&v| v < needle).count();
            assert_eq!(counted, expected, "[{lower}, {upper}) < {needle}");
        }
    }
}
