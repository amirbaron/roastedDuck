//! Partition, peer and frame boundaries of each row (DuckDB's `window_boundaries_state.cpp`).
//!
//! For a chunk of rows of a hash group, [`WindowBoundariesState::bounds`] computes up to
//! eight boundary columns (see [`WindowBound`]); which ones depends on the function and
//! its frame. State carried between chunks lets consecutive chunks resume without
//! rescanning the masks.

use std::borrow::Cow;
use std::ops::{Index, IndexMut};

use rustdb_ast::{OrderType, WindowBoundary, WindowExcludeMode};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::ExpressionType;
use rustdb_types::compare::SqlOrd;
use rustdb_types::{DataChunk, Interval, NativeType, PhysicalType, Vector, VectorKind};

use super::WindowExpression;
use super::mask::{WindowMask, find_next_start, find_prev_start};

/// The boundary columns of a [`WindowBounds`] chunk (DuckDB's `WindowBounds`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowBound {
    PartitionBegin,
    PartitionEnd,
    PeerBegin,
    PeerEnd,
    ValidBegin,
    ValidEnd,
    FrameBegin,
    FrameEnd,
}

impl WindowBound {
    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A set of [`WindowBound`]s (DuckDB's `WindowBoundsSet`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowBoundsSet(u8);

impl WindowBoundsSet {
    pub(crate) fn insert(&mut self, bound: WindowBound) {
        self.0 |= bound.bit();
    }

    pub(crate) const fn contains(self, bound: WindowBound) -> bool {
        self.0 & bound.bit() != 0
    }
}

/// A half-open row range `[start, end)` of a hash group (DuckDB's `FrameBounds`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrameBounds {
    pub start: usize,
    pub end: usize,
}

impl FrameBounds {
    pub(crate) const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

/// The boundary columns for one chunk of rows (DuckDB's 8-column UBIGINT `bounds` chunk).
#[derive(Debug)]
pub(crate) struct WindowBounds {
    columns: [Vec<usize>; 8],
}

impl Default for WindowBounds {
    fn default() -> Self {
        Self {
            columns: std::array::from_fn(|_| vec![0; STANDARD_VECTOR_SIZE]),
        }
    }
}

impl Index<WindowBound> for WindowBounds {
    type Output = [usize];

    fn index(&self, bound: WindowBound) -> &[usize] {
        &self.columns[bound as usize]
    }
}

impl IndexMut<WindowBound> for WindowBounds {
    fn index_mut(&mut self, bound: WindowBound) -> &mut [usize] {
        &mut self.columns[bound as usize]
    }
}

impl WindowBounds {
    /// Copies the first `count` values of `source` into `target` (DuckDB references one
    /// bounds vector from another).
    fn copy(&mut self, target: WindowBound, source: WindowBound, count: usize) {
        let (target, source) = (target as usize, source as usize);
        let (from, to) = if source < target {
            let (left, right) = self.columns.split_at_mut(target);
            (&left[source], &mut right[0])
        } else {
            let (left, right) = self.columns.split_at_mut(source);
            (&right[0], &mut left[target])
        };
        to[..count].copy_from_slice(&from[..count]);
    }

    /// Two columns at once, the second one mutable.
    fn pair_mut(&mut self, read: WindowBound, write: WindowBound) -> (&[usize], &mut [usize]) {
        let (read, write) = (read as usize, write as usize);
        if read < write {
            let (left, right) = self.columns.split_at_mut(write);
            (&left[read], &mut right[0])
        } else {
            let (left, right) = self.columns.split_at_mut(read);
            (&right[0], &mut left[write])
        }
    }
}

/// A column of the evaluation chunk read cell by cell (DuckDB's `WindowInputExpression`).
/// Constant vectors are read at row 0; other non-flat vectors are flattened.
#[derive(Debug)]
pub(crate) struct WindowInputExpression<'a> {
    column: Option<Cow<'a, Vector>>,
    scalar: bool,
}

impl<'a> WindowInputExpression<'a> {
    /// Column `index` of `chunk`, if the expression exists.
    pub(crate) fn new(chunk: &'a DataChunk, index: Option<usize>) -> Self {
        let Some(column) = index.and_then(|index| chunk.columns().get(index)) else {
            return Self {
                column: None,
                scalar: true,
            };
        };
        match column.kind() {
            VectorKind::Constant => Self {
                column: Some(Cow::Borrowed(column)),
                scalar: true,
            },
            VectorKind::Flat => Self {
                column: Some(Cow::Borrowed(column)),
                scalar: false,
            },
            VectorKind::Dictionary | VectorKind::Sequence => Self {
                column: Some(Cow::Owned(column.flattened(chunk.len()))),
                scalar: false,
            },
        }
    }

    /// The physical type of the column (`INVALID` if there is none).
    pub(crate) fn physical_type(&self) -> PhysicalType {
        self.column.as_ref().map_or(PhysicalType::Invalid, |column| {
            column.logical_type().physical_type()
        })
    }

    fn slot(&self, row: usize) -> usize {
        if self.scalar { 0 } else { row }
    }

    /// The raw value of row `row` (NULL rows hold an unspecified value).
    #[inline]
    pub(crate) fn cell<T: NativeType>(&self, row: usize) -> T {
        self.column
            .as_ref()
            .map_or_else(|| T::default(), |column| column.values::<T>()[self.slot(row)])
    }

    #[inline]
    pub(crate) fn cell_is_null(&self, row: usize) -> bool {
        self.column
            .as_ref()
            .is_none_or(|column| !column.validity().is_valid(self.slot(row)))
    }

    /// The vector, if the expression exists.
    pub(crate) fn vector(&self) -> Option<&Vector> {
        self.column.as_deref()
    }

    /// Whether the column is a constant (DuckDB's `scalar`).
    pub(crate) const fn is_scalar(&self) -> bool {
        self.scalar
    }
}

/// `<` in the ORDER BY sense of a RANGE frame (DuckDB's `LessThan` / `GreaterThan`).
trait RangeValue: NativeType {
    fn less(self, other: Self) -> bool;
}

macro_rules! range_value_sql_ord {
    ($($ty:ty),*) => {$(
        impl RangeValue for $ty {
            #[inline]
            fn less(self, other: Self) -> bool {
                other.sql_gt(self)
            }
        }
    )*};
}
range_value_sql_ord!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128, f32, f64);

impl RangeValue for Interval {
    #[inline]
    fn less(self, other: Self) -> bool {
        self < other
    }
}

/// Finds the RANGE frame bound for `value` in the sorted values `[order_begin, order_end)`
/// (DuckDB's `FindTypedRangeBound`). `from` selects the lower bound (frame start) instead
/// of the upper bound (frame end); `ascending` the ORDER BY sense.
#[allow(clippy::too_many_arguments, reason = "mirrors DuckDB's FindTypedRangeBound")]
fn find_typed_range_bound<T: RangeValue>(
    values: &[T],
    ascending: bool,
    from: bool,
    order_begin: usize,
    order_end: usize,
    range: WindowBoundary,
    value: T,
    prev: FrameBounds,
) -> Result<usize> {
    let comp = |lhs: T, rhs: T| if ascending { lhs.less(rhs) } else { rhs.less(lhs) };
    // Check that the value we are searching for is in range.
    if range == WindowBoundary::ExprPrecedingRange {
        if comp(values[order_end - 1], value) {
            return Err(Error::out_of_range("Invalid RANGE PRECEDING value"));
        }
    } else if comp(value, values[order_begin]) {
        return Err(Error::out_of_range("Invalid RANGE FOLLOWING value"));
    }
    // Reuse the previous bounds to restrict the search, if they were non-empty.
    let (mut begin, mut end) = (order_begin, order_end);
    if prev.start < prev.end {
        if order_begin <= prev.start && prev.start < order_end {
            let first = values[prev.start];
            if from && !comp(value, first) {
                if !comp(first, value) {
                    return Ok(prev.start);
                }
                begin = prev.start;
            }
        }
        if order_begin < prev.end && prev.end < order_end {
            let second = values[prev.end - 1];
            if !comp(second, value) {
                if !from && !comp(value, second) {
                    return Ok(prev.end);
                }
                end -= order_end - prev.end - 1;
            }
        }
    }
    let slice = &values[begin..end];
    let offset = if from {
        slice.partition_point(|&element| comp(element, value))
    } else {
        slice.partition_point(|&element| !comp(value, element))
    };
    Ok(begin + offset)
}

/// Dispatches [`find_typed_range_bound`] on the physical type of the ORDER BY column.
#[allow(clippy::too_many_arguments, reason = "mirrors DuckDB's FindOrderedRangeBound")]
fn find_ordered_range_bound(
    range_values: &Vector,
    sense: OrderType,
    from: bool,
    order_begin: usize,
    order_end: usize,
    range: WindowBoundary,
    boundary: &WindowInputExpression<'_>,
    row: usize,
    prev: FrameBounds,
) -> Result<usize> {
    let ascending = match sense {
        OrderType::Ascending => true,
        OrderType::Descending => false,
        _ => return Err(Error::internal("Unsupported ORDER BY sense for RANGE")),
    };
    macro_rules! typed {
        ($ty:ty) => {
            find_typed_range_bound::<$ty>(
                range_values.values::<$ty>(),
                ascending,
                from,
                order_begin,
                order_end,
                range,
                boundary.cell::<$ty>(row),
                prev,
            )
        };
    }
    match boundary.physical_type() {
        PhysicalType::Int8 => typed!(i8),
        PhysicalType::Int16 => typed!(i16),
        PhysicalType::Int32 => typed!(i32),
        PhysicalType::Int64 => typed!(i64),
        PhysicalType::UInt8 => typed!(u8),
        PhysicalType::UInt16 => typed!(u16),
        PhysicalType::UInt32 => typed!(u32),
        PhysicalType::UInt64 => typed!(u64),
        PhysicalType::Int128 => typed!(i128),
        PhysicalType::UInt128 => typed!(u128),
        PhysicalType::Float => typed!(f32),
        PhysicalType::Double => typed!(f64),
        PhysicalType::Interval => typed!(Interval),
        _ => Err(Error::internal("Unsupported column type for RANGE")),
    }
}

/// Whether the frame has an `expr PRECEDING` RANGE bound.
pub(crate) fn has_preceding_range(wexpr: &WindowExpression) -> bool {
    wexpr.window.start == WindowBoundary::ExprPrecedingRange
        || wexpr.window.end == WindowBoundary::ExprPrecedingRange
}

/// Whether the frame has an `expr FOLLOWING` RANGE bound.
pub(crate) fn has_following_range(wexpr: &WindowExpression) -> bool {
    wexpr.window.start == WindowBoundary::ExprFollowingRange
        || wexpr.window.end == WindowBoundary::ExprFollowingRange
}

/// The boundary columns a window function needs (DuckDB's `GetWindowBounds`).
#[allow(
    clippy::too_many_lines,
    reason = "one case per function and frame type, as in DuckDB"
)]
pub(crate) fn window_bounds(wexpr: &WindowExpression) -> Result<WindowBoundsSet> {
    use WindowBound as B;
    let window = &wexpr.window;
    let partition_count = window.partitions.len();
    let order_count = window.orders.len();
    let secondary = !window.arg_orders.is_empty();
    let mut result = WindowBoundsSet::default();
    let mut insert = |bounds: &[WindowBound]| bounds.iter().for_each(|&bound| result.insert(bound));
    match wexpr.expression_type {
        ExpressionType::WindowRowNumber => {
            if secondary {
                insert(&[B::FrameBegin, B::FrameEnd]);
            } else {
                insert(&[B::PartitionBegin]);
            }
        }
        ExpressionType::WindowNtile | ExpressionType::WindowLead | ExpressionType::WindowLag => {
            if secondary {
                insert(&[B::FrameBegin, B::FrameEnd]);
            } else {
                insert(&[B::PartitionBegin, B::PartitionEnd]);
            }
        }
        ExpressionType::WindowRank => {
            if secondary {
                insert(&[B::FrameBegin, B::FrameEnd, B::PeerBegin]);
            } else {
                insert(&[B::PartitionBegin, B::PeerBegin]);
            }
        }
        ExpressionType::WindowRankDense => insert(&[B::PartitionBegin, B::PeerBegin]),
        ExpressionType::WindowPercentRank => {
            if secondary {
                insert(&[B::FrameBegin, B::FrameEnd, B::PeerBegin]);
            } else {
                insert(&[B::PartitionBegin, B::PartitionEnd, B::PeerBegin]);
            }
        }
        ExpressionType::WindowCumeDist => {
            if secondary {
                insert(&[B::FrameBegin, B::FrameEnd, B::PeerEnd]);
            } else {
                insert(&[B::PartitionBegin, B::PartitionEnd, B::PeerEnd]);
            }
        }
        ExpressionType::WindowFill => {
            insert(&[B::FrameBegin, B::FrameEnd]);
            if !secondary {
                // FILL uses the validity ranges to skip rows that can't be interpolated.
                insert(&[B::ValidBegin, B::ValidEnd]);
            }
        }
        ExpressionType::WindowFirstValue
        | ExpressionType::WindowLastValue
        | ExpressionType::WindowNthValue
        | ExpressionType::WindowAggregate => insert(&[B::FrameBegin, B::FrameEnd]),
        other => {
            return Err(Error::internal(format!(
                "Window expression type {}",
                other.name()
            )));
        }
    }

    // Internal dependencies
    if result.contains(B::FrameBegin) || result.contains(B::FrameEnd) {
        result.insert(B::PartitionBegin);
        result.insert(B::PartitionEnd);
        // EXCLUDE GROUP / TIES need peer boundaries.
        if window.exclude_clause != WindowExcludeMode::NoOther {
            result.insert(B::PeerBegin);
            result.insert(B::PeerEnd);
        }
        // RANGE and GROUPS frames need peers; RANGE also the valid (non-NULL) range.
        match window.start {
            WindowBoundary::CurrentRowRange
            | WindowBoundary::CurrentRowGroups
            | WindowBoundary::ExprPrecedingGroups
            | WindowBoundary::ExprFollowingGroups => result.insert(B::PeerBegin),
            WindowBoundary::ExprPrecedingRange => {
                result.insert(B::PeerBegin);
                result.insert(B::ValidBegin);
                result.insert(B::ValidEnd);
            }
            WindowBoundary::ExprFollowingRange => {
                result.insert(B::PeerBegin);
                result.insert(B::ValidEnd);
            }
            _ => {}
        }
        match window.end {
            WindowBoundary::CurrentRowRange
            | WindowBoundary::CurrentRowGroups
            | WindowBoundary::ExprPrecedingGroups
            | WindowBoundary::ExprFollowingGroups => result.insert(B::PeerEnd),
            WindowBoundary::ExprPrecedingRange => {
                result.insert(B::PeerEnd);
                result.insert(B::ValidBegin);
            }
            WindowBoundary::ExprFollowingRange => {
                result.insert(B::PeerEnd);
                result.insert(B::ValidBegin);
                result.insert(B::ValidEnd);
            }
            _ => {}
        }
    }

    if result.contains(B::ValidEnd) {
        result.insert(B::PartitionEnd);
        if has_following_range(wexpr) {
            result.insert(B::ValidBegin);
        }
    }
    if result.contains(B::ValidBegin) {
        result.insert(B::PartitionBegin);
        result.insert(B::PartitionEnd);
    }
    if result.contains(B::PeerEnd) {
        result.insert(B::PartitionEnd);
        if order_count > 0 {
            result.insert(B::PeerBegin);
        }
    }
    if result.contains(B::PartitionEnd) && partition_count + order_count > 0 {
        result.insert(B::PartitionBegin);
    }
    Ok(result)
}

/// Inputs of one [`WindowBoundariesState::bounds`] call.
#[derive(Debug)]
pub(crate) struct BoundsInput<'a> {
    pub row_idx: usize,
    pub count: usize,
    /// The ORDER BY values of the hash group, for RANGE frames.
    pub range: Option<&'a Vector>,
    pub boundary_start: &'a WindowInputExpression<'a>,
    pub boundary_end: &'a WindowInputExpression<'a>,
    pub partition_mask: &'a WindowMask,
    pub order_mask: &'a WindowMask,
}

/// Computes boundary columns chunk by chunk (DuckDB's `WindowBoundariesState`).
#[derive(Debug)]
pub(crate) struct WindowBoundariesState {
    pub(crate) required: WindowBoundsSet,
    input_size: usize,
    start_boundary: WindowBoundary,
    end_boundary: WindowBoundary,
    partition_count: usize,
    order_count: usize,
    range_sense: OrderType,

    // Carried between chunks
    next_pos: usize,
    partition_start: usize,
    partition_end: usize,
    peer_start: usize,
    valid_start: usize,
    valid_end: usize,
    prev: FrameBounds,
}

impl WindowBoundariesState {
    pub(crate) fn new(wexpr: &WindowExpression, input_size: usize) -> Result<Self> {
        let window = &wexpr.window;
        Ok(Self {
            required: window_bounds(wexpr)?,
            input_size,
            start_boundary: window.start,
            end_boundary: window.end,
            partition_count: window.partitions.len(),
            order_count: window.orders.len(),
            range_sense: window
                .orders
                .first()
                .map_or(OrderType::Invalid, |order| order.order_type),
            next_pos: 0,
            partition_start: 0,
            partition_end: 0,
            peer_start: 0,
            valid_start: 0,
            valid_end: 0,
            prev: FrameBounds::default(),
        })
    }

    /// Fills the required boundary columns for rows `[row_idx, row_idx + count)`.
    pub(crate) fn bounds(&mut self, bounds: &mut WindowBounds, input: &BoundsInput<'_>) -> Result<()> {
        use WindowBound as B;
        let BoundsInput { row_idx, count, .. } = *input;
        // Have we jumped from the previous position?
        let is_jump = self.next_pos != row_idx;
        if is_jump {
            self.next_pos = row_idx;
        }
        let required = self.required;
        if required.contains(B::PartitionBegin) {
            self.partition_begin(bounds, row_idx, count, is_jump, input.partition_mask);
        }
        if required.contains(B::PartitionEnd) {
            self.partition_end(bounds, row_idx, count, is_jump, input.partition_mask);
        }
        if required.contains(B::PeerBegin) {
            self.peer_begin(bounds, input, is_jump);
        }
        if required.contains(B::PeerEnd) {
            self.peer_end(bounds, count, input.order_mask);
        }
        if required.contains(B::ValidBegin) {
            self.valid_begin(bounds, input, is_jump)?;
        }
        if required.contains(B::ValidEnd) {
            self.valid_end(bounds, input, is_jump)?;
        }
        if required.contains(B::FrameBegin) {
            self.frame_begin(bounds, input)?;
        }
        if required.contains(B::FrameEnd) {
            self.frame_end(bounds, input)?;
        }
        self.next_pos += count;
        Ok(())
    }

    fn partition_begin(
        &mut self,
        bounds: &mut WindowBounds,
        row_idx: usize,
        count: usize,
        mut is_jump: bool,
        partition_mask: &WindowMask,
    ) {
        let data = &mut bounds[WindowBound::PartitionBegin][..count];
        // OVER()
        if self.partition_count + self.order_count == 0 {
            data.fill(0);
            return;
        }
        for (i, value) in data.iter_mut().enumerate() {
            let row = row_idx + i;
            // When the partition changes, recompute the boundaries.
            if partition_mask.is_valid(row) || is_jump {
                if is_jump {
                    let mut n = 1;
                    self.partition_start = find_prev_start(partition_mask, 0, row + 1, &mut n);
                    is_jump = false;
                } else {
                    self.partition_start = row;
                }
            }
            *value = self.partition_start;
        }
    }

    fn partition_end(
        &mut self,
        bounds: &mut WindowBounds,
        row_idx: usize,
        count: usize,
        mut is_jump: bool,
        partition_mask: &WindowMask,
    ) {
        // OVER()
        if self.partition_count + self.order_count == 0 {
            bounds[WindowBound::PartitionEnd][..count].fill(self.input_size);
            return;
        }
        let (begins, ends) = bounds.pair_mut(WindowBound::PartitionBegin, WindowBound::PartitionEnd);
        for i in 0..count {
            let row = row_idx + i;
            if partition_mask.is_valid(row) || is_jump {
                self.partition_end = self.input_size;
                if self.partition_count > 0 {
                    let mut n = 1;
                    self.partition_end =
                        find_next_start(partition_mask, begins[i] + 1, self.input_size, &mut n);
                }
                is_jump = false;
            }
            ends[i] = self.partition_end;
        }
    }

    fn peer_begin(&mut self, bounds: &mut WindowBounds, input: &BoundsInput<'_>, mut is_jump: bool) {
        let BoundsInput {
            row_idx,
            count,
            partition_mask,
            order_mask,
            ..
        } = *input;
        let data = &mut bounds[WindowBound::PeerBegin][..count];
        // OVER()
        if self.partition_count + self.order_count == 0 {
            data.fill(0);
            return;
        }
        for (i, value) in data.iter_mut().enumerate() {
            let row = row_idx + i;
            if partition_mask.is_valid(row) || is_jump {
                if is_jump {
                    let mut n = 1;
                    self.peer_start = find_prev_start(order_mask, 0, row + 1, &mut n);
                } else {
                    self.peer_start = row;
                }
                is_jump = false;
            } else if order_mask.is_valid(row) {
                self.peer_start = row;
            }
            *value = self.peer_start;
        }
    }

    fn peer_end(&self, bounds: &mut WindowBounds, count: usize, order_mask: &WindowMask) {
        // OVER()
        if self.order_count == 0 {
            bounds.copy(WindowBound::PeerEnd, WindowBound::PartitionEnd, count);
            return;
        }
        let mut peer_ends = std::mem::take(&mut bounds.columns[WindowBound::PeerEnd as usize]);
        let partition_ends = &bounds[WindowBound::PartitionEnd];
        let peer_begins = &bounds[WindowBound::PeerBegin];
        let mut prev_end = peer_begins[0];
        for i in 0..count {
            let peer_start = peer_begins[i];
            if peer_start >= prev_end {
                let mut n = 1;
                prev_end = find_next_start(order_mask, peer_start + 1, partition_ends[i], &mut n);
            }
            peer_ends[i] = prev_end;
        }
        bounds.columns[WindowBound::PeerEnd as usize] = peer_ends;
    }

    fn valid_begin(
        &mut self,
        bounds: &mut WindowBounds,
        input: &BoundsInput<'_>,
        mut is_jump: bool,
    ) -> Result<()> {
        let range = input
            .range
            .ok_or_else(|| Error::internal("window valid range without ORDER BY values"))?;
        let mut valid_begins = std::mem::take(&mut bounds.columns[WindowBound::ValidBegin as usize]);
        let partition_begins = &bounds[WindowBound::PartitionBegin];
        let partition_ends = &bounds[WindowBound::PartitionEnd];
        for i in 0..input.count {
            let row = input.row_idx + i;
            if input.partition_mask.is_valid(row) || is_jump {
                is_jump = false;
                // Exclude leading NULLs of the ORDER BY column from RANGE computations.
                self.valid_start = partition_begins[i];
                let valid_end = partition_ends[i];
                if self.valid_start < valid_end && range.is_null(self.valid_start) {
                    let mut n = 1;
                    self.valid_start =
                        find_next_start(input.order_mask, self.valid_start + 1, valid_end, &mut n);
                }
            }
            valid_begins[i] = self.valid_start;
        }
        bounds.columns[WindowBound::ValidBegin as usize] = valid_begins;
        Ok(())
    }

    fn valid_end(
        &mut self,
        bounds: &mut WindowBounds,
        input: &BoundsInput<'_>,
        mut is_jump: bool,
    ) -> Result<()> {
        let range = input
            .range
            .ok_or_else(|| Error::internal("window valid range without ORDER BY values"))?;
        let mut valid_ends = std::mem::take(&mut bounds.columns[WindowBound::ValidEnd as usize]);
        let partition_ends = &bounds[WindowBound::PartitionEnd];
        let valid_begins = &bounds[WindowBound::ValidBegin];
        for i in 0..input.count {
            let row = input.row_idx + i;
            if input.partition_mask.is_valid(row) || is_jump {
                is_jump = false;
                // Exclude trailing NULLs of the ORDER BY column from RANGE computations.
                let valid_start = valid_begins[i];
                self.valid_end = partition_ends[i];
                if valid_start < self.valid_end && range.is_null(self.valid_end - 1) {
                    let mut n = 1;
                    self.valid_end = find_prev_start(input.order_mask, valid_start, self.valid_end, &mut n);
                }
            }
            valid_ends[i] = self.valid_end;
        }
        bounds.columns[WindowBound::ValidEnd as usize] = valid_ends;
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one case per frame start type, as in DuckDB"
    )]
    fn frame_begin(&mut self, bounds: &mut WindowBounds, input: &BoundsInput<'_>) -> Result<()> {
        use WindowBound as B;
        let BoundsInput {
            row_idx,
            count,
            order_mask,
            boundary_start: boundary,
            ..
        } = *input;
        let mut frame = std::mem::take(&mut bounds.columns[B::FrameBegin as usize]);
        let partition_begins = &bounds[B::PartitionBegin];
        let partition_ends = &bounds[B::PartitionEnd];
        let peer_begins = &bounds[B::PeerBegin];
        let valid_begins = &bounds[B::ValidBegin];
        let valid_ends = &bounds[B::ValidEnd];

        // Reset the previous range hints.
        let mut prev_partition = partition_begins[0];
        self.prev = FrameBounds::new(valid_begins[0], valid_ends[0]);

        let mut clamp = true;
        match self.start_boundary {
            WindowBoundary::UnboundedPreceding => {
                frame[..count].copy_from_slice(&partition_begins[..count]);
                clamp = false;
            }
            WindowBoundary::CurrentRowRows => {
                for (i, value) in frame[..count].iter_mut().enumerate() {
                    *value = row_idx + i;
                }
            }
            WindowBoundary::CurrentRowRange | WindowBoundary::CurrentRowGroups => {
                // The current row's first peer.
                frame[..count].copy_from_slice(&peer_begins[..count]);
            }
            WindowBoundary::ExprPrecedingRows | WindowBoundary::ExprFollowingRows => {
                let preceding = self.start_boundary == WindowBoundary::ExprPrecedingRows;
                for (i, value) in frame[..count].iter_mut().enumerate() {
                    let row = (row_idx + i) as i64;
                    let delta = boundary.cell::<i64>(i);
                    let computed = if preceding {
                        row.checked_sub(delta)
                    } else {
                        row.checked_add(delta)
                    };
                    *value = computed.map_or(partition_begins[i], |start| start.max(0) as usize);
                }
            }
            WindowBoundary::ExprPrecedingRange => {
                let range = range_values(input)?;
                for i in 0..count {
                    frame[i] = if boundary.cell_is_null(i) {
                        peer_begins[i]
                    } else {
                        let valid_start = valid_begins[i];
                        self.prev.end = valid_ends[i];
                        if partition_begins[i] != prev_partition {
                            self.prev.start = valid_start;
                            prev_partition = partition_begins[i];
                        }
                        let start = find_ordered_range_bound(
                            range,
                            self.range_sense,
                            true,
                            valid_start,
                            row_idx + i + 1,
                            self.start_boundary,
                            boundary,
                            i,
                            self.prev,
                        )?;
                        self.prev.start = start;
                        start
                    };
                }
            }
            WindowBoundary::ExprFollowingRange => {
                let range = range_values(input)?;
                for i in 0..count {
                    let peer_begin = peer_begins[i];
                    frame[i] = if boundary.cell_is_null(i) {
                        peer_begin
                    } else {
                        let valid_end = valid_ends[i];
                        self.prev.end = valid_end;
                        if partition_begins[i] != prev_partition {
                            self.prev.start = valid_begins[i];
                            prev_partition = partition_begins[i];
                        }
                        let start = find_ordered_range_bound(
                            range,
                            self.range_sense,
                            true,
                            peer_begin,
                            valid_end,
                            self.start_boundary,
                            boundary,
                            i,
                            self.prev,
                        )?;
                        self.prev.start = start;
                        start
                    };
                }
            }
            WindowBoundary::ExprPrecedingGroups => {
                // Count peer groups backwards.
                for i in 0..count {
                    frame[i] = if boundary.cell_is_null(i) {
                        peer_begins[i]
                    } else {
                        let delta = boundary.cell::<i64>(i);
                        if delta < 0 {
                            return Err(Error::out_of_range("Invalid GROUPS PRECEDING value"));
                        }
                        if delta == 0 {
                            peer_begins[i]
                        } else {
                            let mut n = delta as usize;
                            find_prev_start(order_mask, partition_begins[i], peer_begins[i], &mut n)
                        }
                    };
                }
            }
            WindowBoundary::ExprFollowingGroups => {
                // Count peer groups forward.
                for i in 0..count {
                    frame[i] = if boundary.cell_is_null(i) {
                        peer_begins[i]
                    } else {
                        let delta = boundary.cell::<i64>(i);
                        if delta < 0 {
                            return Err(Error::out_of_range("Invalid GROUPS FOLLOWING value"));
                        }
                        if delta == 0 {
                            peer_begins[i]
                        } else {
                            let mut n = delta as usize;
                            find_next_start(order_mask, peer_begins[i] + 1, partition_ends[i], &mut n)
                        }
                    };
                }
            }
            WindowBoundary::UnboundedFollowing | WindowBoundary::Invalid => {
                return Err(Error::internal("Unsupported window start boundary"));
            }
        }
        if clamp {
            clamp_frame(&mut frame[..count], partition_begins, partition_ends);
        }
        bounds.columns[B::FrameBegin as usize] = frame;
        Ok(())
    }

    #[allow(clippy::too_many_lines, reason = "one case per frame end type, as in DuckDB")]
    fn frame_end(&mut self, bounds: &mut WindowBounds, input: &BoundsInput<'_>) -> Result<()> {
        use WindowBound as B;
        let BoundsInput {
            row_idx,
            count,
            order_mask,
            boundary_end: boundary,
            ..
        } = *input;
        let mut frame = std::mem::take(&mut bounds.columns[B::FrameEnd as usize]);
        let partition_begins = &bounds[B::PartitionBegin];
        let partition_ends = &bounds[B::PartitionEnd];
        let peer_ends = &bounds[B::PeerEnd];
        let valid_begins = &bounds[B::ValidBegin];
        let valid_ends = &bounds[B::ValidEnd];

        // Reset the previous range hints.
        let mut prev_partition = partition_begins[0];
        self.prev = FrameBounds::new(valid_begins[0], valid_ends[0]);

        let mut clamp = true;
        match self.end_boundary {
            WindowBoundary::CurrentRowRows => {
                for (i, value) in frame[..count].iter_mut().enumerate() {
                    *value = row_idx + i + 1;
                }
            }
            WindowBoundary::CurrentRowRange | WindowBoundary::CurrentRowGroups => {
                // The current row's last peer.
                frame[..count].copy_from_slice(&peer_ends[..count]);
            }
            WindowBoundary::UnboundedFollowing => {
                frame[..count].copy_from_slice(&partition_ends[..count]);
                clamp = false;
            }
            WindowBoundary::ExprPrecedingRows | WindowBoundary::ExprFollowingRows => {
                let preceding = self.end_boundary == WindowBoundary::ExprPrecedingRows;
                for (i, value) in frame[..count].iter_mut().enumerate() {
                    let row = (row_idx + i + 1) as i64;
                    let delta = boundary.cell::<i64>(i);
                    let computed = if preceding {
                        row.checked_sub(delta)
                    } else {
                        row.checked_add(delta)
                    };
                    *value = computed.map_or(partition_ends[i], |end| end.max(0) as usize);
                }
            }
            WindowBoundary::ExprPrecedingRange => {
                let range = range_values(input)?;
                for i in 0..count {
                    let peer_end = peer_ends[i];
                    frame[i] = if boundary.cell_is_null(i) {
                        peer_end
                    } else {
                        let valid_start = valid_begins[i];
                        self.prev.start = valid_start;
                        if partition_begins[i] != prev_partition {
                            // DuckDB resets to the state's carried valid end here.
                            self.prev.end = self.valid_end;
                            prev_partition = partition_begins[i];
                        }
                        let end = find_ordered_range_bound(
                            range,
                            self.range_sense,
                            false,
                            valid_start,
                            peer_end,
                            self.end_boundary,
                            boundary,
                            i,
                            self.prev,
                        )?;
                        self.prev.end = end;
                        end
                    };
                }
            }
            WindowBoundary::ExprFollowingRange => {
                let range = range_values(input)?;
                for i in 0..count {
                    frame[i] = if boundary.cell_is_null(i) {
                        peer_ends[i]
                    } else {
                        let valid_end = valid_ends[i];
                        self.prev.start = valid_begins[i];
                        if partition_begins[i] != prev_partition {
                            self.prev.end = valid_end;
                            prev_partition = partition_begins[i];
                        }
                        let end = find_ordered_range_bound(
                            range,
                            self.range_sense,
                            false,
                            row_idx + i,
                            valid_end,
                            self.end_boundary,
                            boundary,
                            i,
                            self.prev,
                        )?;
                        self.prev.end = end;
                        end
                    };
                }
            }
            WindowBoundary::ExprPrecedingGroups => {
                for i in 0..count {
                    frame[i] = if boundary.cell_is_null(i) {
                        peer_ends[i]
                    } else {
                        let delta = boundary.cell::<i64>(i);
                        if delta < 0 {
                            return Err(Error::out_of_range("Invalid GROUPS PRECEDING value"));
                        }
                        if delta == 0 {
                            peer_ends[i]
                        } else {
                            let mut n = delta as usize;
                            find_prev_start(order_mask, partition_begins[i], peer_ends[i], &mut n)
                        }
                    };
                }
            }
            WindowBoundary::ExprFollowingGroups => {
                for i in 0..count {
                    frame[i] = if boundary.cell_is_null(i) {
                        peer_ends[i]
                    } else {
                        let delta = boundary.cell::<i64>(i);
                        if delta < 0 {
                            return Err(Error::out_of_range("Invalid GROUPS FOLLOWING value"));
                        }
                        if delta == 0 {
                            peer_ends[i]
                        } else {
                            let mut n = delta as usize;
                            find_next_start(order_mask, peer_ends[i] + 1, partition_ends[i], &mut n)
                        }
                    };
                }
            }
            WindowBoundary::UnboundedPreceding | WindowBoundary::Invalid => {
                return Err(Error::internal("Unsupported window end boundary"));
            }
        }
        if clamp {
            clamp_frame(&mut frame[..count], partition_begins, partition_ends);
        }
        bounds.columns[B::FrameEnd as usize] = frame;
        Ok(())
    }
}

/// The ORDER BY values a RANGE frame searches.
fn range_values<'a>(input: &BoundsInput<'a>) -> Result<&'a Vector> {
    input
        .range
        .ok_or_else(|| Error::internal("RANGE frame without ORDER BY values"))
}

/// Clamps frame bounds to their partition (DuckDB's `ClampFrame`).
fn clamp_frame(values: &mut [usize], begins: &[usize], ends: &[usize]) {
    for ((value, &begin), &end) in values.iter_mut().zip(begins).zip(ends) {
        *value = (*value).max(begin).min(end);
    }
}
