//! The comparison kernels of the nested loop join (DuckDB's `execution/nested_loop_join`:
//! `NestedLoopJoinInner` and `NestedLoopJoinMark`).
//!
//! Comparisons follow DuckDB's `ComparisonOperationWrapper`: NULL never matches, except
//! for `[NOT] DISTINCT FROM`. Every (type, comparison) pair gets its own monomorphised
//! loop.

use roastedduck_ast::ExpressionType;
use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_planner::operator::JoinCondition;
use roastedduck_types::compare::SqlOrd;
use roastedduck_types::{DataChunk, Interval, PhysicalType, SelectionVector, StringT, Unified, Vector};

use crate::expression_executor::compare_vectors;

/// A value that compares with SQL semantics.
trait SqlKey: Copy {
    fn key_eq(self, other: Self) -> bool;
    fn key_gt(self, other: Self) -> bool;
}

impl<T: SqlOrd> SqlKey for Native<T> {
    #[inline]
    fn key_eq(self, other: Self) -> bool {
        self.0.sql_eq(other.0)
    }
    #[inline]
    fn key_gt(self, other: Self) -> bool {
        self.0.sql_gt(other.0)
    }
}

/// A fixed-width value.
#[derive(Clone, Copy)]
struct Native<T>(T);

/// The bytes of a string value.
#[derive(Clone, Copy)]
struct Bytes<'a>(&'a [u8]);

impl SqlKey for Bytes<'_> {
    #[inline]
    fn key_eq(self, other: Self) -> bool {
        self.0 == other.0
    }
    #[inline]
    fn key_gt(self, other: Self) -> bool {
        self.0 > other.0
    }
}

/// The values of a vector seen through its selection (`UnifiedVectorFormat`).
trait KeyColumn<'a> {
    type Key: SqlKey;
    fn new(vector: &'a Vector) -> Self;
    /// The value of row `row` and whether it is NULL.
    fn get(&self, row: usize) -> (Self::Key, bool);
}

struct NativeColumn<'a, T: Copy>(Unified<'a, T>);

impl<'a, T: SqlOrd> KeyColumn<'a> for NativeColumn<'a, T> {
    type Key = Native<T>;
    fn new(vector: &'a Vector) -> Self {
        Self(vector.unified::<T>())
    }
    #[inline]
    fn get(&self, row: usize) -> (Native<T>, bool) {
        let slot = self.0.slot(row);
        (Native(self.0.values[slot]), !self.0.validity.is_valid(slot))
    }
}

struct StringColumn<'a> {
    values: Unified<'a, StringT>,
    vector: &'a Vector,
}

impl<'a> KeyColumn<'a> for StringColumn<'a> {
    type Key = Bytes<'a>;
    fn new(vector: &'a Vector) -> Self {
        Self {
            values: vector.unified::<StringT>(),
            vector,
        }
    }
    #[inline]
    fn get(&self, row: usize) -> (Bytes<'a>, bool) {
        let slot = self.values.slot(row);
        let value = &self.values.values[slot];
        (
            Bytes(self.vector.string_value(value)),
            !self.values.validity.is_valid(slot),
        )
    }
}

/// A join comparison with DuckDB's NULL semantics (`ComparisonOperationWrapper<OP>`).
trait JoinComparison {
    /// Whether NULLs take part in the comparison (`COMPARE_NULL`).
    const COMPARE_NULL: bool;
    fn compare<K: SqlKey>(left: K, right: K) -> bool;

    #[inline]
    fn operation<K: SqlKey>(left: K, right: K, left_null: bool, right_null: bool) -> bool {
        if left_null || right_null {
            return Self::null_result(left_null, right_null);
        }
        Self::compare(left, right)
    }

    #[inline]
    fn null_result(_left_null: bool, _right_null: bool) -> bool {
        false
    }
}

macro_rules! comparisons {
    ($($name:ident => |$l:ident, $r:ident| $body:expr;)*) => {$(
        struct $name;
        impl JoinComparison for $name {
            const COMPARE_NULL: bool = false;
            #[inline]
            fn compare<K: SqlKey>($l: K, $r: K) -> bool {
                $body
            }
        }
    )*};
}

comparisons! {
    Equals => |l, r| l.key_eq(r);
    NotEquals => |l, r| !l.key_eq(r);
    LessThan => |l, r| r.key_gt(l);
    GreaterThan => |l, r| l.key_gt(r);
    LessThanEquals => |l, r| !l.key_gt(r);
    GreaterThanEquals => |l, r| !r.key_gt(l);
}

struct DistinctFrom;
impl JoinComparison for DistinctFrom {
    const COMPARE_NULL: bool = true;
    #[inline]
    fn compare<K: SqlKey>(left: K, right: K) -> bool {
        !left.key_eq(right)
    }
    #[inline]
    fn null_result(left_null: bool, right_null: bool) -> bool {
        left_null != right_null
    }
}

struct NotDistinctFrom;
impl JoinComparison for NotDistinctFrom {
    const COMPARE_NULL: bool = true;
    #[inline]
    fn compare<K: SqlKey>(left: K, right: K) -> bool {
        left.key_eq(right)
    }
    #[inline]
    fn null_result(left_null: bool, right_null: bool) -> bool {
        left_null == right_null
    }
}

/// Calls `$func::<Column, Op>(left_column, right_column, args...)` for the physical
/// type of `$left` and the comparison `$comparison`.
macro_rules! dispatch {
    ($func:ident, $comparison:expr, $left:expr, $right:expr, ($($arg:expr),*), $what:literal) => {{
        macro_rules! with_op {
            ($column:ty) => {
                match $comparison {
                    ExpressionType::CompareEqual => $func::<$column, Equals>(&<$column>::new($left), &<$column>::new($right), $($arg),*),
                    ExpressionType::CompareNotequal => $func::<$column, NotEquals>(&<$column>::new($left), &<$column>::new($right), $($arg),*),
                    ExpressionType::CompareLessthan => $func::<$column, LessThan>(&<$column>::new($left), &<$column>::new($right), $($arg),*),
                    ExpressionType::CompareGreaterthan => $func::<$column, GreaterThan>(&<$column>::new($left), &<$column>::new($right), $($arg),*),
                    ExpressionType::CompareLessthanorequalto => $func::<$column, LessThanEquals>(&<$column>::new($left), &<$column>::new($right), $($arg),*),
                    ExpressionType::CompareGreaterthanorequalto => $func::<$column, GreaterThanEquals>(&<$column>::new($left), &<$column>::new($right), $($arg),*),
                    ExpressionType::CompareDistinctFrom => $func::<$column, DistinctFrom>(&<$column>::new($left), &<$column>::new($right), $($arg),*),
                    ExpressionType::CompareNotDistinctFrom => $func::<$column, NotDistinctFrom>(&<$column>::new($left), &<$column>::new($right), $($arg),*),
                    _ => return Err(Error::not_implemented("Unimplemented comparison type for join!")),
                }
            };
        }
        match $left.logical_type().physical_type() {
            PhysicalType::Bool => with_op!(NativeColumn<'_, bool>),
            PhysicalType::Int8 => with_op!(NativeColumn<'_, i8>),
            PhysicalType::Int16 => with_op!(NativeColumn<'_, i16>),
            PhysicalType::Int32 => with_op!(NativeColumn<'_, i32>),
            PhysicalType::Int64 => with_op!(NativeColumn<'_, i64>),
            PhysicalType::Int128 => with_op!(NativeColumn<'_, i128>),
            PhysicalType::UInt8 => with_op!(NativeColumn<'_, u8>),
            PhysicalType::UInt16 => with_op!(NativeColumn<'_, u16>),
            PhysicalType::UInt32 => with_op!(NativeColumn<'_, u32>),
            PhysicalType::UInt64 => with_op!(NativeColumn<'_, u64>),
            PhysicalType::UInt128 => with_op!(NativeColumn<'_, u128>),
            PhysicalType::Float => with_op!(NativeColumn<'_, f32>),
            PhysicalType::Double => with_op!(NativeColumn<'_, f64>),
            PhysicalType::Interval => with_op!(NativeColumn<'_, Interval>),
            PhysicalType::Varchar => with_op!(StringColumn<'_>),
            _ => return Err(Error::not_implemented($what)),
        }
    }};
}

/// Positions in the left and right condition chunks where a join resumes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NestedLoopPosition {
    /// The next left row.
    pub left: usize,
    /// The current right row.
    pub right: usize,
}

/// Finds matching (left, right) pairs of the first condition, right rows outer, until
/// `STANDARD_VECTOR_SIZE` matches (`InitialNestedLoopJoin`).
fn initial_join<'a, C: KeyColumn<'a>, O: JoinComparison>(
    left: &C,
    right: &C,
    left_size: usize,
    right_size: usize,
    position: &mut NestedLoopPosition,
    lvector: &mut Vec<u32>,
    rvector: &mut Vec<u32>,
) -> usize {
    while position.right < right_size {
        let (right_value, right_null) = right.get(position.right);
        while position.left < left_size {
            if lvector.len() == STANDARD_VECTOR_SIZE {
                return lvector.len();
            }
            let (left_value, left_null) = left.get(position.left);
            if O::operation(left_value, right_value, left_null, right_null) {
                lvector.push(position.left as u32);
                rvector.push(position.right as u32);
            }
            position.left += 1;
        }
        position.left = 0;
        position.right += 1;
    }
    lvector.len()
}

/// Keeps the pairs that also satisfy a further condition (`RefineNestedLoopJoin`).
fn refine_join<'a, C: KeyColumn<'a>, O: JoinComparison>(
    left: &C,
    right: &C,
    lvector: &mut Vec<u32>,
    rvector: &mut Vec<u32>,
) -> usize {
    let mut count = 0;
    for i in 0..lvector.len() {
        let (lidx, ridx) = (lvector[i], rvector[i]);
        let (left_value, left_null) = left.get(lidx as usize);
        let (right_value, right_null) = right.get(ridx as usize);
        if O::operation(left_value, right_value, left_null, right_null) {
            lvector[count] = lidx;
            rvector[count] = ridx;
            count += 1;
        }
    }
    lvector.truncate(count);
    rvector.truncate(count);
    count
}

/// The next (up to `STANDARD_VECTOR_SIZE`) pairs of rows of `left_conditions` and
/// `right_conditions` matching every condition, resuming at `position`
/// (`NestedLoopJoinInner::Perform`). Matching left rows go to `lvector`, right rows to
/// `rvector`.
pub fn perform_inner(
    position: &mut NestedLoopPosition,
    left_conditions: &DataChunk,
    right_conditions: &DataChunk,
    lvector: &mut Vec<u32>,
    rvector: &mut Vec<u32>,
    conditions: &[JoinCondition],
) -> Result<usize> {
    lvector.clear();
    rvector.clear();
    if position.left >= left_conditions.len() || position.right >= right_conditions.len() {
        return Ok(0);
    }
    let (left_size, right_size) = (left_conditions.len(), right_conditions.len());
    let mut match_count = dispatch!(
        initial_join,
        conditions[0].comparison,
        left_conditions.column(0),
        right_conditions.column(0),
        (left_size, right_size, position, lvector, rvector),
        "Unimplemented type for join!"
    );
    for (index, condition) in conditions.iter().enumerate().skip(1) {
        if match_count == 0 {
            return Ok(0);
        }
        match_count = dispatch!(
            refine_join,
            condition.comparison,
            left_conditions.column(index),
            right_conditions.column(index),
            (lvector, rvector),
            "Unimplemented type for join!"
        );
    }
    Ok(match_count)
}

/// Marks the left rows with a matching right row (`TemplatedMarkJoin`).
fn mark_join<'a, C: KeyColumn<'a>, O: JoinComparison>(
    left: &C,
    right: &C,
    left_count: usize,
    right_count: usize,
    found_match: &mut [bool],
) {
    for (i, found) in found_match.iter_mut().enumerate().take(left_count) {
        if *found {
            continue;
        }
        let (left_value, left_null) = left.get(i);
        if !O::COMPARE_NULL && left_null {
            continue;
        }
        for j in 0..right_count {
            let (right_value, right_null) = right.get(j);
            if !O::COMPARE_NULL && right_null {
                continue;
            }
            if O::operation(left_value, right_value, left_null, right_null) {
                *found = true;
                break;
            }
        }
    }
}

/// Marks the left rows matching some row of a nested-type right column
/// (`MarkJoinNested`).
fn mark_join_nested(
    left: &Vector,
    right: &Vector,
    left_count: usize,
    right_count: usize,
    found_match: &mut [bool],
    comparison: ExpressionType,
) -> Result<()> {
    for (i, found) in found_match.iter_mut().enumerate().take(left_count) {
        if *found {
            continue;
        }
        let repeated = left.slice(&SelectionVector::new(vec![i as u32; right_count]));
        let result = compare_vectors(comparison, &repeated, right, right_count)?;
        let values = result.unified::<bool>();
        if (0..right_count).any(|row| values.get(row) == Some(true)) {
            *found = true;
        }
    }
    Ok(())
}

/// Marks in `found_match` the rows of `left` that match some row of the `right` chunk,
/// one condition at a time like DuckDB (`NestedLoopJoinMark::Perform` for one chunk).
pub fn perform_mark(
    left: &DataChunk,
    right: &DataChunk,
    found_match: &mut [bool],
    conditions: &[JoinCondition],
) -> Result<()> {
    for (index, condition) in conditions.iter().enumerate() {
        let (left_column, right_column) = (left.column(index), right.column(index));
        if matches!(
            left_column.logical_type().physical_type(),
            PhysicalType::Struct | PhysicalType::List | PhysicalType::Array
        ) {
            mark_join_nested(
                left_column,
                right_column,
                left.len(),
                right.len(),
                found_match,
                condition.comparison,
            )?;
            continue;
        }
        if condition.comparison == ExpressionType::CompareNotDistinctFrom {
            return Err(Error::not_implemented("Unimplemented comparison type for join!"));
        }
        dispatch!(
            mark_join,
            condition.comparison,
            left_column,
            right_column,
            (left.len(), right.len(), found_match),
            "Unimplemented type for mark join!"
        );
    }
    Ok(())
}
