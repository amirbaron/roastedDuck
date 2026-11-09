//! Functions that return a row's value from elsewhere in the partition: `lead`, `lag`,
//! `first_value`, `last_value`, `nth_value` and `fill` (DuckDB's
//! `window_value_function.cpp`).
//!
//! Each evaluation first decides, per output row, which collection row supplies the value
//! (or that the default / NULL applies), then gathers all values with one vectorised
//! selection.

use roastedduck_common::{Error, Result};
use roastedduck_planner::{Expression, ExpressionType};
use roastedduck_types::kernels::scatter;
use roastedduck_types::{DataChunk, LogicalTypeId, PhysicalType, SelectionVector, Vector};

use super::WindowExpression;
use super::aggregator::{evaluate_subframes, init_subframes};
use super::boundaries_state::{FrameBounds, WindowBound, WindowBoundariesState, WindowInputExpression};
use super::executor::EvaluateInput;
use super::mask::{WindowMask, find_next_start, find_prev_start};
use super::merge_sort_tree::WindowMergeSortTree;
use super::shared_expressions::WindowSharedExpressions;
use super::shared_orders;
use crate::ExecutionContext;

/// Which value function an executor computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValueKind {
    Lead,
    Lag,
    FirstValue,
    LastValue,
    NthValue,
    Fill,
}

/// A value function (DuckDB's `WindowValueExecutor` and subclasses).
#[derive(Debug)]
pub(crate) struct ValueExecutor {
    kind: ValueKind,
    /// Collection column of the value argument.
    child_idx: Option<usize>,
    /// Evaluation column of `nth_value`'s position.
    nth_idx: Option<usize>,
    /// Evaluation columns of `lead`/`lag`'s offset and default.
    offset_idx: Option<usize>,
    default_idx: Option<usize>,
    /// Sink columns of the `ORDER BY` arguments.
    arg_order_idx: Vec<usize>,
    /// Collection column of `fill`'s secondary ordering.
    order_idx: Option<usize>,
}

/// Per hash group (DuckDB's `WindowValueGlobalState` / `WindowLeadLagGlobalState`).
#[derive(Debug)]
pub(crate) struct ValueGlobalState {
    /// Maps positions in the secondary order to rows (an index tree).
    value_tree: Option<WindowMergeSortTree>,
    /// Ranks rows in the secondary order (a unique token tree, LEAD/LAG/FILL).
    row_tree: Option<WindowMergeSortTree>,
    /// LEAD/LAG within the frame instead of the partition.
    use_framing: bool,
}

/// Per thread: the subframes for `EXCLUDE`.
#[derive(Debug)]
pub(crate) struct ValueLocalState {
    frames: Vec<FrameBounds>,
}

/// Where each output row's value comes from.
#[derive(Debug, Default)]
struct Sources {
    /// The collection row of each output row (`None`: NULL or default).
    rows: Vec<Option<u32>>,
    /// Output rows that take the default value.
    defaults: Vec<u32>,
}

impl Sources {
    fn with_capacity(count: usize) -> Self {
        Self {
            rows: Vec::with_capacity(count),
            defaults: Vec::new(),
        }
    }

    fn row(&mut self, row: usize) {
        self.rows.push(Some(row as u32));
    }

    fn null(&mut self) {
        self.rows.push(None);
    }

    fn default(&mut self) {
        self.defaults.push(self.rows.len() as u32);
        self.rows.push(None);
    }

    /// Gathers the values from `column`, then the defaults from `default`.
    fn gather(self, column: &Vector, default: &WindowInputExpression<'_>) -> Result<Vector> {
        let mut result = column.select_rows(&self.rows);
        if let (false, Some(vector)) = (self.defaults.is_empty(), default.vector()) {
            let sel = if default.is_scalar() {
                SelectionVector::new(vec![0; self.defaults.len()])
            } else {
                SelectionVector::new(self.defaults.clone())
            };
            let defaults = vector.slice(&sel);
            scatter(&defaults, &mut result, &self.defaults)?;
            // `scatter` only writes NULLs; the gathered rows were NULL before.
            let validity = result.validity_mut();
            for (row, &position) in self.defaults.iter().enumerate() {
                if !defaults.is_null(row) {
                    validity.set_valid(position as usize);
                }
            }
        }
        Ok(result)
    }
}

/// `left + right` or `left - right` with DuckDB's overflow errors.
fn offset_row(lead: bool, left: i64, right: i64) -> Result<i64> {
    if lead {
        left.checked_add(right)
            .ok_or_else(|| Error::out_of_range(format!("Overflow in addition of INT64 ({left} + {right})!")))
    } else {
        left.checked_sub(right).ok_or_else(|| {
            Error::out_of_range(format!("Overflow in subtraction of INT64 ({left} - {right})!"))
        })
    }
}

impl ValueExecutor {
    /// Registers the arguments; FILL may add the ORDER BY column to `range_idx`.
    pub(crate) fn new(
        wexpr: &WindowExpression,
        shared: &mut WindowSharedExpressions,
        range_idx: &mut Option<usize>,
    ) -> Result<Self> {
        let kind = match wexpr.expression_type {
            ExpressionType::WindowLead => ValueKind::Lead,
            ExpressionType::WindowLag => ValueKind::Lag,
            ExpressionType::WindowFirstValue => ValueKind::FirstValue,
            ExpressionType::WindowLastValue => ValueKind::LastValue,
            ExpressionType::WindowNthValue => ValueKind::NthValue,
            ExpressionType::WindowFill => ValueKind::Fill,
            other => {
                return Err(Error::internal(format!(
                    "{} is not a value function",
                    other.name()
                )));
            }
        };
        let window = &wexpr.window;
        let mut arg_order_idx: Vec<usize> = window
            .arg_orders
            .iter()
            .filter_map(|order| shared.register_sink(Some(&order.expression)))
            .collect();
        // Only the first argument is read across rows.
        let child_idx = shared.register_collection(window.children.first(), wexpr.ignore_nulls());
        let nth_idx = shared.register_evaluate(window.children.get(1));
        let offset_idx = shared.register_evaluate(window.offset_expr.as_ref());
        let default_idx = shared.register_evaluate(window.default_expr.as_ref());
        let mut order_idx = None;
        if kind == ValueKind::Fill {
            // A secondary order that is a prefix of the partition order is redundant.
            if shared_orders(&window.orders, &window.arg_orders) == window.arg_orders.len() {
                arg_order_idx.clear();
            }
            // Interpolation needs the ordering values.
            if arg_order_idx.is_empty() {
                if range_idx.is_none() {
                    *range_idx = shared
                        .register_collection(window.orders.first().map(|order| &order.expression), false);
                }
            } else {
                order_idx = shared
                    .register_collection(window.arg_orders.first().map(|order| &order.expression), false);
            }
        }
        Ok(Self {
            kind,
            child_idx,
            nth_idx,
            offset_idx,
            default_idx,
            arg_order_idx,
            order_idx,
        })
    }

    /// Whether the executor skips NULL values (FILL never does).
    fn ignore_nulls(&self, wexpr: &WindowExpression) -> bool {
        self.kind != ValueKind::Fill && wexpr.ignore_nulls()
    }

    pub(crate) fn global_state(
        &self,
        ctx: &ExecutionContext,
        wexpr: &WindowExpression,
        count: usize,
    ) -> Result<ValueGlobalState> {
        let arg_orders = &wexpr.window.arg_orders;
        let mut state = ValueGlobalState {
            value_tree: None,
            row_tree: None,
            use_framing: false,
        };
        if !self.arg_order_idx.is_empty() {
            state.value_tree = Some(WindowMergeSortTree::new(
                ctx,
                arg_orders,
                &self.arg_order_idx,
                count,
                false,
            )?);
        }
        if matches!(self.kind, ValueKind::Lead | ValueKind::Lag | ValueKind::Fill)
            && state.value_tree.is_some()
        {
            state.use_framing = true;
            if shared_orders(&wexpr.window.orders, arg_orders) == arg_orders.len() {
                // The partition order already is the secondary order.
                state.value_tree = None;
            } else {
                // The own row's position in the secondary order, ties broken by row.
                state.row_tree = Some(WindowMergeSortTree::new(
                    ctx,
                    arg_orders,
                    &self.arg_order_idx,
                    count,
                    true,
                )?);
            }
        }
        Ok(state)
    }

    pub(crate) fn local_state(
        &self,
        wexpr: &WindowExpression,
        global: &ValueGlobalState,
        bounds_state: &mut WindowBoundariesState,
    ) -> ValueLocalState {
        if self.kind == ValueKind::Fill && global.value_tree.is_none() {
            // FILL over the partition order skips rows outside the valid ORDER BY range.
            bounds_state.required.insert(WindowBound::ValidBegin);
            bounds_state.required.insert(WindowBound::ValidEnd);
        }
        ValueLocalState {
            frames: init_subframes(wexpr.window.exclude_clause),
        }
    }

    pub(crate) fn sink(
        &self,
        wexpr: &WindowExpression,
        global: &mut ValueGlobalState,
        sink_chunk: &DataChunk,
        coll_chunk: &DataChunk,
        input_idx: usize,
    ) -> Result<()> {
        if let Some(tree) = &mut global.value_tree {
            // With IGNORE NULLS only the non-NULL values take part in the secondary order.
            let child = self.child_idx.map(|index| coll_chunk.column(index));
            let filter: Option<Vec<u32>> = match child {
                Some(child)
                    if self.ignore_nulls(wexpr) && super::collection::has_nulls(child, coll_chunk.len()) =>
                {
                    Some(
                        (0..coll_chunk.len() as u32)
                            .filter(|&row| !child.is_null(row as usize))
                            .collect(),
                    )
                }
                _ => None,
            };
            tree.sink(sink_chunk, input_idx, filter.as_deref())?;
        }
        if let Some(tree) = &mut global.row_tree {
            tree.sink(sink_chunk, input_idx, None)?;
        }
        Ok(())
    }

    pub(crate) fn finalize(global: &mut ValueGlobalState) -> Result<()> {
        if let Some(tree) = &mut global.value_tree {
            tree.build_index_tree()?;
        }
        if let Some(tree) = &mut global.row_tree {
            tree.build_token_tree()?;
        }
        Ok(())
    }

    pub(crate) fn evaluate(
        &self,
        wexpr: &WindowExpression,
        global: &ValueGlobalState,
        local: &mut ValueLocalState,
        input: &EvaluateInput<'_>,
    ) -> Result<Vector> {
        let child_idx = self
            .child_idx
            .ok_or_else(|| Error::internal("window value function without an argument"))?;
        let column = input.group.collection.column(child_idx);
        // IGNORE NULLS skips the rows whose value is NULL.
        let all_valid = WindowMask::all_valid();
        let ignore_nulls = if self.ignore_nulls(wexpr) {
            input.group.collection.validity(child_idx).unwrap_or(&all_valid)
        } else {
            &all_valid
        };
        let default = WindowInputExpression::new(input.eval_chunk, self.default_idx);
        let sources = match self.kind {
            ValueKind::Lead | ValueKind::Lag => self.lead_lag(wexpr, global, local, input, ignore_nulls)?,
            ValueKind::FirstValue => Self::first_value(wexpr, global, local, input, ignore_nulls)?,
            ValueKind::LastValue => Self::last_value(wexpr, global, local, input, ignore_nulls)?,
            ValueKind::NthValue => self.nth_value(wexpr, global, local, input, ignore_nulls)?,
            ValueKind::Fill => return self.fill(wexpr, global, local, input, column),
        };
        sources.gather(column, &default)
    }

    fn lead_lag(
        &self,
        wexpr: &WindowExpression,
        global: &ValueGlobalState,
        local: &mut ValueLocalState,
        input: &EvaluateInput<'_>,
        ignore_nulls: &WindowMask,
    ) -> Result<Sources> {
        let EvaluateInput {
            bounds,
            row_idx,
            count,
            eval_chunk,
            ..
        } = *input;
        let lead = self.kind == ValueKind::Lead;
        let offsets = WindowInputExpression::new(eval_chunk, self.offset_idx);
        let has_offset = wexpr.window.offset_expr.is_some();
        let has_default = wexpr.window.default_expr.is_some();
        let frame_begins = &bounds[WindowBound::FrameBegin];
        let frame_ends = &bounds[WindowBound::FrameEnd];
        let mut sources = Sources::with_capacity(count);

        if let (Some(row_tree), Some(value_tree)) = (&global.row_tree, &global.value_tree) {
            local.frames.truncate(1);
            local.frames.resize(1, FrameBounds::default());
            for i in 0..count {
                let row = row_idx + i;
                let mut offset = 1;
                if has_offset {
                    if offsets.cell_is_null(i) {
                        sources.null();
                        continue;
                    }
                    offset = offsets.cell::<i64>(i);
                }
                // (1) the ROW_NUMBER of the own row, (2) shifted by the offset,
                // (3) the row at that position, (4) its value.
                let frame = FrameBounds::new(frame_begins[i], frame_ends[i]);
                local.frames[0] = frame;
                let own_row = row_tree.rank(frame.start, frame.end, row) - 1;
                let value_idx = offset_row(lead, own_row as i64, offset)?;
                let frame_width = (frame.end - frame.start) as i64;
                if (0..frame_width).contains(&value_idx) {
                    let (nth, overflow) = value_tree.select_nth(&local.frames, value_idx as usize);
                    if overflow == 0 {
                        sources.row(nth);
                    } else {
                        sources.null();
                    }
                } else if has_default {
                    sources.default();
                } else {
                    sources.null();
                }
            }
            return Ok(sources);
        }

        // Shift within the frame only with a shared secondary ordering.
        let (begins, ends) = if global.use_framing {
            (frame_begins, frame_ends)
        } else {
            (
                &bounds[WindowBound::PartitionBegin],
                &bounds[WindowBound::PartitionEnd],
            )
        };
        // Rows can be shifted in runs unless NULLs are skipped (rows are not contiguous),
        // the frame moves per row, or the offset / default vary per row.
        let window = &wexpr.window;
        let can_shift = ignore_nulls.is_all_valid()
            && !global.use_framing
            && window.offset_expr.as_ref().is_none_or(Expression::is_foldable)
            && window.default_expr.as_ref().is_none_or(Expression::is_foldable);
        let row_end = row_idx + count;
        let mut i = 0;
        while i < count {
            let row = row_idx + i;
            let mut offset = 1;
            if has_offset {
                if offsets.cell_is_null(i) {
                    sources.null();
                    i += 1;
                    continue;
                }
                offset = offsets.cell::<i64>(i);
            }
            let mut value_idx = offset_row(lead, row as i64, offset)?;
            let mut delta = 0;
            // Unsigned differences, as in DuckDB.
            if value_idx < row as i64 {
                // Count backwards (skipping NULLs if requested).
                delta = (row as u64).wrapping_sub(value_idx as u64) as usize;
                value_idx = find_prev_start(ignore_nulls, begins[i], row, &mut delta) as i64;
            } else if value_idx > row as i64 {
                delta = (value_idx as u64).wrapping_sub(row as u64) as usize;
                value_idx = find_next_start(ignore_nulls, row + 1, ends[i], &mut delta) as i64;
            }
            if !can_shift {
                if delta == 0 {
                    sources.row(value_idx as usize);
                } else if has_default {
                    sources.default();
                } else {
                    sources.null();
                }
                i += 1;
                continue;
            }
            // A run of rows up to the end of the partition (or chunk) shifts together.
            let target_limit = ends[i].min(row_end) - row;
            let width = if delta == 0 {
                let index = value_idx as usize;
                let width = (ends[i] - index).min(target_limit);
                for source in index..index + width {
                    sources.row(source);
                }
                width
            } else {
                let width = delta.min(target_limit);
                for _ in 0..width {
                    if has_default {
                        sources.default();
                    } else {
                        sources.null();
                    }
                }
                width
            };
            i += width;
        }
        Ok(sources)
    }

    /// The total width of the subframes.
    fn frame_width(frames: &[FrameBounds]) -> usize {
        frames.iter().map(|frame| frame.end - frame.start).sum()
    }

    fn first_value(
        wexpr: &WindowExpression,
        global: &ValueGlobalState,
        local: &mut ValueLocalState,
        input: &EvaluateInput<'_>,
        ignore_nulls: &WindowMask,
    ) -> Result<Sources> {
        let collection_size = input.group.collection.size();
        let mut sources = Sources::with_capacity(input.count);
        evaluate_subframes(
            input.bounds,
            wexpr.window.exclude_clause,
            input.count,
            input.row_idx,
            &mut local.frames,
            |_, frames| {
                if let Some(tree) = &global.value_tree {
                    if Self::frame_width(frames) == 0 {
                        sources.null();
                        return Ok(());
                    }
                    match tree.select_nth(frames, 0) {
                        (first, 0) if first < collection_size => sources.row(first),
                        _ => sources.null(),
                    }
                    return Ok(());
                }
                for frame in frames.iter().filter(|frame| frame.start < frame.end) {
                    // NTH_VALUE(.., 1)
                    let mut n = 1;
                    let first = find_next_start(ignore_nulls, frame.start, frame.end, &mut n);
                    if n == 0 {
                        sources.row(first);
                        return Ok(());
                    }
                }
                sources.null();
                Ok(())
            },
        )?;
        Ok(sources)
    }

    fn last_value(
        wexpr: &WindowExpression,
        global: &ValueGlobalState,
        local: &mut ValueLocalState,
        input: &EvaluateInput<'_>,
        ignore_nulls: &WindowMask,
    ) -> Result<Sources> {
        let collection_size = input.group.collection.size();
        let mut sources = Sources::with_capacity(input.count);
        evaluate_subframes(
            input.bounds,
            wexpr.window.exclude_clause,
            input.count,
            input.row_idx,
            &mut local.frames,
            |_, frames| {
                if let Some(tree) = &global.value_tree {
                    let width = Self::frame_width(frames);
                    if width == 0 {
                        sources.null();
                        return Ok(());
                    }
                    let mut n = width - 1;
                    let mut last = tree.select_nth(frames, n);
                    if last.1 != 0 && last.1 <= n {
                        // The frame is wider than the data: back off by the overflow.
                        n -= last.1;
                        last = tree.select_nth(frames, n);
                    }
                    match last {
                        (row, 0) if row < collection_size => sources.row(row),
                        _ => sources.null(),
                    }
                    return Ok(());
                }
                for frame in frames.iter().rev().filter(|frame| frame.start < frame.end) {
                    let mut n = 1;
                    let last = find_prev_start(ignore_nulls, frame.start, frame.end, &mut n);
                    if n == 0 {
                        sources.row(last);
                        return Ok(());
                    }
                }
                sources.null();
                Ok(())
            },
        )?;
        Ok(sources)
    }

    fn nth_value(
        &self,
        wexpr: &WindowExpression,
        global: &ValueGlobalState,
        local: &mut ValueLocalState,
        input: &EvaluateInput<'_>,
        ignore_nulls: &WindowMask,
    ) -> Result<Sources> {
        let collection_size = input.group.collection.size();
        let nth = WindowInputExpression::new(input.eval_chunk, self.nth_idx);
        let mut sources = Sources::with_capacity(input.count);
        evaluate_subframes(
            input.bounds,
            wexpr.window.exclude_clause,
            input.count,
            input.row_idx,
            &mut local.frames,
            |i, frames| {
                // The n'th row of the frame (counting from 1), or NULL.
                if nth.cell_is_null(i) {
                    sources.null();
                    return Ok(());
                }
                let n_param = nth.cell::<i64>(i);
                if n_param < 1 {
                    sources.null();
                    return Ok(());
                }
                let mut n = n_param as usize;
                if let Some(tree) = &global.value_tree {
                    n -= 1;
                    if n < Self::frame_width(frames) {
                        match tree.select_nth(frames, n) {
                            (row, 0) if row < collection_size => sources.row(row),
                            _ => sources.null(),
                        }
                    } else {
                        sources.null();
                    }
                    return Ok(());
                }
                for frame in frames.iter().filter(|frame| frame.start < frame.end) {
                    let row = find_next_start(ignore_nulls, frame.start, frame.end, &mut n);
                    if n == 0 {
                        sources.row(row);
                        return Ok(());
                    }
                }
                sources.null();
                Ok(())
            },
        )?;
        Ok(sources)
    }

    /// Linear interpolation of NULL values from the nearest non-NULL neighbours in the
    /// ORDER BY (DuckDB's `WindowFillExecutor::EvaluateInternal`).
    #[allow(clippy::too_many_lines, reason = "a direct port of DuckDB's FILL evaluation")]
    fn fill(
        &self,
        wexpr: &WindowExpression,
        global: &ValueGlobalState,
        local: &mut ValueLocalState,
        input: &EvaluateInput<'_>,
        column: &Vector,
    ) -> Result<Vector> {
        let EvaluateInput {
            bounds,
            mut row_idx,
            count,
            group,
            ..
        } = *input;
        // Assume the best: copy all the values.
        let rows: Vec<Option<u32>> = (row_idx..row_idx + count).map(|row| Some(row as u32)).collect();
        let mut result = column.select_rows(&rows);
        if (0..count).all(|i| !result.is_null(i)) {
            return Ok(result);
        }
        let value_ok = |row: usize| fill_value_ok(column, row);
        let partition_begins = &bounds[WindowBound::PartitionBegin];
        let partition_ends = &bounds[WindowBound::PartitionEnd];

        // Secondary sort: use the merge sort trees (LEAD/LAG IGNORE NULLS, essentially).
        if let (Some(value_tree), Some(row_tree)) = (&global.value_tree, &global.row_tree) {
            let order_column = self
                .order_idx
                .map(|index| group.collection.column(index))
                .ok_or_else(|| Error::internal("FILL without ordering values"))?;
            let order_ok = |row: usize| fill_value_ok(order_column, row);
            local.frames.truncate(1);
            local.frames.resize(1, FrameBounds::default());
            let mut i = 0;
            while i < count {
                if !result.is_null(i) || !order_ok(row_idx) {
                    i += 1;
                    row_idx += 1;
                    continue;
                }
                // The frame is the entire partition.
                let frame = FrameBounds::new(partition_begins[i], partition_ends[i]);
                local.frames[0] = frame;
                let frames = &local.frames;
                let frame_width = frame.end - frame.start;
                let row_at = |n: usize| value_tree.select_nth(frames, n).0;
                let own_row = row_tree.rank(frame.start, frame.end, row_idx) - 1;
                // The previous usable value, scanning backwards in the secondary order...
                let mut prev = None;
                for n in (0..own_row).rev() {
                    let j = row_at(n);
                    if !order_ok(j) {
                        break;
                    }
                    if value_ok(j) {
                        prev = Some((j, n));
                        break;
                    }
                }
                // ... or forwards if there is none.
                if prev.is_none() {
                    for n in own_row + 1..frame_width {
                        let j = row_at(n);
                        if !order_ok(j) {
                            break;
                        }
                        if value_ok(j) {
                            prev = Some((j, n));
                            break;
                        }
                    }
                }
                let Some((mut prev_valid, prev_n)) = prev else {
                    // No usable values: skip to the next partition.
                    let skip = partition_ends[i] - row_idx - 1;
                    i += skip;
                    if i >= count {
                        return Ok(result);
                    }
                    row_idx = partition_ends[i] - 1;
                    i += 1;
                    row_idx += 1;
                    continue;
                };
                // The next usable value after the previous one, or else before it.
                let mut next_valid = None;
                for n in prev_n + 1..frame_width {
                    let j = row_at(n);
                    if !order_ok(j) {
                        break;
                    }
                    if value_ok(j) {
                        next_valid = Some(j);
                        break;
                    }
                }
                if next_valid.is_none() && prev_n > 0 {
                    for n in (0..prev_n).rev() {
                        let j = row_at(n);
                        if !order_ok(j) {
                            break;
                        }
                        if value_ok(j) {
                            next_valid = Some(prev_valid);
                            prev_valid = j;
                            break;
                        }
                    }
                }
                match next_valid {
                    None => fill_copy(&mut result, i, column, prev_valid)?,
                    Some(next_valid) => {
                        let slope = fill_slope(order_column, row_idx, prev_valid, next_valid)?;
                        fill_interpolate(&mut result, i, column, prev_valid, next_valid, slope)?;
                    }
                }
                i += 1;
                row_idx += 1;
            }
            return Ok(result);
        }

        let order_column = wexpr
            .window
            .orders
            .first()
            .and(input.range)
            .ok_or_else(|| Error::internal("FILL without ordering values"))?;
        let order_ok = |row: usize| fill_value_ok(order_column, row);
        let valid_begins = &bounds[WindowBound::ValidBegin];
        let valid_ends = &bounds[WindowBound::ValidEnd];
        let mut prev_partition = None;
        let mut prev_valid: Option<usize> = None;
        let mut i = 0;
        while i < count {
            let row = row_idx;
            // Did we change partitions?
            if prev_partition != Some(partition_begins[i]) {
                prev_partition = Some(partition_begins[i]);
                prev_valid = None;
            }
            // Outside the valid range of the ordering we can't fix the value.
            if row < valid_begins[i] || valid_ends[i] <= row || !order_ok(row) {
                i += 1;
                row_idx += 1;
                continue;
            }
            if !result.is_null(i) {
                // A usable value is remembered for the next gap.
                if value_ok(row) {
                    prev_valid = Some(row);
                }
                i += 1;
                row_idx += 1;
                continue;
            }
            // Missing value: the previous usable value, scanning backwards ...
            if prev_valid.is_none() {
                for j in (valid_begins[i]..row).rev() {
                    if !order_ok(j) {
                        break;
                    }
                    if value_ok(j) {
                        prev_valid = Some(j);
                        break;
                    }
                }
            }
            // ... or forwards if there is nothing behind us.
            if prev_valid.is_none() {
                for j in row + 1..valid_ends[i] {
                    if !order_ok(j) {
                        break;
                    }
                    if value_ok(j) {
                        prev_valid = Some(j);
                        break;
                    }
                }
            }
            let Some(mut previous) = prev_valid else {
                // No usable values: skip to the next partition.
                i += partition_ends[i] - row - 1;
                if i >= count {
                    break;
                }
                row_idx = partition_ends[i] - 1;
                i += 1;
                row_idx += 1;
                continue;
            };
            // The next usable value after the previous one, or else before it.
            let mut next_valid = None;
            for j in previous + 1..valid_ends[i] {
                if !order_ok(j) {
                    break;
                }
                if value_ok(j) {
                    next_valid = Some(j);
                    break;
                }
            }
            if next_valid.is_none() {
                for j in (valid_begins[i]..previous).rev() {
                    if !order_ok(j) {
                        break;
                    }
                    if value_ok(j) {
                        next_valid = Some(previous);
                        previous = j;
                        prev_valid = Some(j);
                        break;
                    }
                }
            }
            match next_valid {
                None => fill_copy(&mut result, i, column, previous)?,
                Some(next) => {
                    let slope = fill_slope(order_column, row, previous, next)?;
                    fill_interpolate(&mut result, i, column, previous, next, slope)?;
                }
            }
            i += 1;
            row_idx += 1;
        }
        Ok(result)
    }
}

/// Whether row `row` holds a usable (non-NULL, finite) FILL value (DuckDB's
/// `FillValueFunction`).
fn fill_value_ok(column: &Vector, row: usize) -> bool {
    if column.is_null(row) {
        return false;
    }
    match column.logical_type().id() {
        LogicalTypeId::Date => {
            let value = column.values::<i32>()[row];
            value != i32::MAX && value != -i32::MAX
        }
        LogicalTypeId::Timestamp
        | LogicalTypeId::TimestampTz
        | LogicalTypeId::TimestampSec
        | LogicalTypeId::TimestampMs
        | LogicalTypeId::TimestampNs => {
            let value = column.values::<i64>()[row];
            value != i64::MAX && value != -i64::MAX
        }
        _ => match column.logical_type().physical_type() {
            PhysicalType::Float => column.values::<f32>()[row].is_finite(),
            PhysicalType::Double => column.values::<f64>()[row].is_finite(),
            _ => true,
        },
    }
}

/// A numeric FILL value (DuckDB's `FillSlopeFunc`, `InterpolateOperator` and
/// `TryExtrapolateOperator` instantiations).
trait FillValue: roastedduck_types::NativeType {
    /// `LossyFillCast<double>`.
    fn to_f64(self) -> f64;
    /// `InterpolateOperator::Operation`: `lo + (hi - lo) * d`.
    fn interpolate(lo: Self, d: f64, hi: Self) -> Self;
    /// `TryExtrapolateOperator::Operation`, `None` on overflow.
    fn extrapolate(lo: Self, d: f64, hi: Self) -> Option<Self>;
}

/// DuckDB's `TryCast` from DOUBLE to an integer: in range, then rounded half to even.
macro_rules! try_cast_double {
    ($value:expr, $ty:ty) => {{
        let value: f64 = $value;
        if value.is_finite()
            && value >= <$ty>::MIN as f64
            && value < (-(<$ty>::MIN as f64)).max(<$ty>::MAX as f64 + 1.0)
        {
            Some(value.round_ties_even() as $ty)
        } else {
            None
        }
    }};
}

macro_rules! fill_integer {
    ($($ty:ty => $wide:ty),*) => {$(
        impl FillValue for $ty {
            fn to_f64(self) -> f64 {
                self as f64
            }
            fn interpolate(lo: Self, d: f64, hi: Self) -> Self {
                // C++ arithmetic: the difference in the promoted type, the step truncated.
                let delta = (hi as $wide).wrapping_sub(lo as $wide) as f64;
                let step = (delta * d) as $ty;
                (lo as $wide).wrapping_add(step as $wide) as $ty
            }
            fn extrapolate(lo: Self, d: f64, hi: Self) -> Option<Self> {
                if lo > hi {
                    return Self::extrapolate(hi, -d, lo);
                }
                let delta = (hi as $wide).wrapping_sub(lo as $wide) as f64;
                if d < 0.0 {
                    let offset = try_cast_double!(delta * -d, $ty)?;
                    lo.checked_sub(offset)
                } else {
                    let offset = try_cast_double!(delta * d, $ty)?;
                    lo.checked_add(offset)
                }
            }
        }
    )*};
}
fill_integer!(i8 => i32, i16 => i32, i32 => i32, i64 => i64, u8 => i32, u16 => i32, u32 => u32, u64 => u64);

macro_rules! fill_huge {
    ($($ty:ty),*) => {$(
        impl FillValue for $ty {
            fn to_f64(self) -> f64 {
                self as f64
            }
            fn interpolate(lo: Self, d: f64, hi: Self) -> Self {
                f64::interpolate(lo as f64, d, hi as f64) as $ty
            }
            fn extrapolate(lo: Self, d: f64, hi: Self) -> Option<Self> {
                let value = f64::interpolate(lo as f64, d, hi as f64);
                try_cast_double!(value, $ty)
            }
        }
    )*};
}
fill_huge!(i128, u128);

impl FillValue for f64 {
    fn to_f64(self) -> f64 {
        self
    }
    fn interpolate(lo: Self, d: f64, hi: Self) -> Self {
        // `lo * (1.0 - d) + hi * d` as DuckDB's release build computes it (contracted to a
        // fused multiply-add).
        lo.mul_add(1.0 - d, hi * d)
    }
    fn extrapolate(lo: Self, d: f64, hi: Self) -> Option<Self> {
        Some(Self::interpolate(lo, d, hi))
    }
}

impl FillValue for f32 {
    fn to_f64(self) -> f64 {
        f64::from(self)
    }
    fn interpolate(lo: Self, d: f64, hi: Self) -> Self {
        let delta = f64::from(hi - lo);
        lo + (delta * d) as f32
    }
    fn extrapolate(lo: Self, d: f64, hi: Self) -> Option<Self> {
        Some(Self::interpolate(lo, d, hi))
    }
}

macro_rules! fill_dispatch {
    ($physical:expr, $error:expr, |$ty:ident| $body:expr) => {
        match $physical {
            PhysicalType::UInt8 => {
                type $ty = u8;
                $body
            }
            PhysicalType::UInt16 => {
                type $ty = u16;
                $body
            }
            PhysicalType::UInt32 => {
                type $ty = u32;
                $body
            }
            PhysicalType::UInt64 => {
                type $ty = u64;
                $body
            }
            PhysicalType::UInt128 => {
                type $ty = u128;
                $body
            }
            PhysicalType::Int8 => {
                type $ty = i8;
                $body
            }
            PhysicalType::Int16 => {
                type $ty = i16;
                $body
            }
            PhysicalType::Int32 => {
                type $ty = i32;
                $body
            }
            PhysicalType::Int64 => {
                type $ty = i64;
                $body
            }
            PhysicalType::Int128 => {
                type $ty = i128;
                $body
            }
            PhysicalType::Float => {
                type $ty = f32;
                $body
            }
            PhysicalType::Double => {
                type $ty = f64;
                $body
            }
            _ => Err(Error::internal($error)),
        }
    };
}

/// `(x - x0) / (x1 - x0)` on the ordering values, 0 for duplicate x values (DuckDB's
/// `FillSlopeFunc`).
fn fill_slope(order: &Vector, row: usize, prev: usize, next: usize) -> Result<f64> {
    fill_dispatch!(
        order.logical_type().physical_type(),
        "Unsupported FILL slope type.",
        |T| {
            let values = order.values::<T>();
            let (x, x0, x1) = (values[row].to_f64(), values[prev].to_f64(), values[next].to_f64());
            let den = x1 - x0;
            Ok(if den == 0.0 { 0.0 } else { (x - x0) / den })
        }
    )
}

/// Writes the interpolation (or extrapolation) between rows `lo` and `hi` of `column`
/// into row `i` of `result` (DuckDB's `FillInterpolateFunc`).
fn fill_interpolate(
    result: &mut Vector,
    i: usize,
    column: &Vector,
    lo: usize,
    hi: usize,
    slope: f64,
) -> Result<()> {
    fill_dispatch!(
        column.logical_type().physical_type(),
        "Unsupported FILL interpolation type.",
        |T| {
            let values = column.values::<T>();
            let (y0, y1) = (values[lo], values[hi]);
            let value = if (0.0..=1.0).contains(&slope) {
                Some(T::interpolate(y0, slope, y1))
            } else {
                T::extrapolate(y0, slope, y1)
            };
            if let Some(value) = value {
                result.values_mut::<T>()[i] = value;
                result.validity_mut().set_valid(i);
            }
            Ok(())
        }
    )
}

/// Copies row `row` of `column` into row `i` of `result`.
fn fill_copy(result: &mut Vector, i: usize, column: &Vector, row: usize) -> Result<()> {
    let width = column.logical_type().physical_type().size();
    if width == 0 {
        return Err(Error::internal("Unsupported FILL value type."));
    }
    let source = column.data_bytes()[row * width..(row + 1) * width].to_vec();
    result.data_bytes_mut()[i * width..(i + 1) * width].copy_from_slice(&source);
    result.validity_mut().set_valid(i);
    Ok(())
}
