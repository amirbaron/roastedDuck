//! Join operators (DuckDB's `execution/operator/join`) and the helpers they share
//! (`PhysicalJoin`, `PhysicalComparisonJoin`, `OuterJoinMarker`).

mod blockwise_nl_join;
mod hash_join;
mod nested_loop_join;
mod outer_join_marker;
mod piecewise_merge_join;
mod range_join;

pub use blockwise_nl_join::{BlockwiseNLJoinGlobalState, BlockwiseNLJoinState, PhysicalBlockwiseNLJoin};
pub use hash_join::{HashJoinGlobalSinkState, HashJoinLocalSinkState, HashJoinProbeState, PhysicalHashJoin};
pub use nested_loop_join::{
    NestedLoopJoinGlobalState, NestedLoopJoinProbeState, PhysicalNestedLoopJoin, RightOuterScanState,
};
pub use outer_join_marker::OuterJoinMarker;
pub use piecewise_merge_join::{
    MergeJoinGlobalState, MergeJoinProbeState, MergeJoinSourceState, PhysicalPiecewiseMergeJoin,
};

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_common::Result;
use rustdb_planner::operator::JoinCondition;
use rustdb_types::{DataChunk, LogicalType, SelectionVector, Vector};

/// Whether unmatched left rows are emitted with NULLs on the right (DuckDB's
/// `IsLeftOuterJoin`).
pub const fn is_left_outer_join(join_type: JoinType) -> bool {
    matches!(join_type, JoinType::Left | JoinType::Outer)
}

/// Whether the join emits build-side rows after probing: unmatched ones for RIGHT and FULL
/// joins, matched or unmatched ones for RIGHT SEMI and RIGHT ANTI joins (DuckDB's
/// `PropagatesBuildSide`).
pub const fn propagates_build_side(join_type: JoinType) -> bool {
    matches!(
        join_type,
        JoinType::Outer | JoinType::Right | JoinType::RightAnti | JoinType::RightSemi
    )
}

/// Whether an empty right (build) side means an empty result
/// (`PhysicalJoin::EmptyResultIfRHSIsEmpty`).
pub const fn empty_result_if_rhs_is_empty(join_type: JoinType) -> bool {
    matches!(
        join_type,
        JoinType::Inner | JoinType::Right | JoinType::Semi | JoinType::RightSemi | JoinType::RightAnti
    )
}

/// Whether a condition compares for equality (the conditions a hash table is keyed on).
pub const fn is_equality(comparison: ExpressionType) -> bool {
    matches!(
        comparison,
        ExpressionType::CompareEqual | ExpressionType::CompareNotDistinctFrom
    )
}

/// Moves the equality conditions in front of the others, keeping their relative order
/// (`PhysicalComparisonJoin::ReorderConditions`).
pub fn reorder_conditions(conditions: &mut Vec<JoinCondition>) {
    let (equal, other): (Vec<_>, Vec<_>) = std::mem::take(conditions)
        .into_iter()
        .partition(|condition| is_equality(condition.comparison));
    conditions.extend(equal);
    conditions.extend(other);
}

/// Whether any row of `chunk` has a NULL in any column (`PhysicalJoin::HasNullValues`).
pub fn has_null_values(chunk: &DataChunk) -> bool {
    chunk.columns().iter().any(|column| {
        let validity = column.validity();
        !validity.is_all_valid() && {
            let sel = column.sel();
            (0..chunk.len()).any(|row| !validity.is_valid(sel.slot(row)))
        }
    })
}

/// Constant NULL columns of `types`.
pub(crate) fn null_columns(types: &[LogicalType]) -> Vec<Vector> {
    types.iter().cloned().map(Vector::constant_null).collect()
}

/// The rows of `left` selected by `sel`.
pub(crate) fn slice_columns(left: &DataChunk, sel: &[u32]) -> Vec<Vector> {
    let sel = SelectionVector::new(sel.to_vec());
    left.columns().iter().map(|column| column.slice(&sel)).collect()
}

/// The result of a join whose build side is empty
/// (`PhysicalComparisonJoin::ConstructEmptyJoinResult`): the input for ANTI joins, the
/// input with a constant mark for MARK joins, and the input padded with NULLs for LEFT,
/// FULL and SINGLE joins. `right_types` are the types of the padding columns.
pub fn construct_empty_join_result(
    join_type: JoinType,
    has_null: bool,
    input: &DataChunk,
    right_types: &[LogicalType],
    result: &mut DataChunk,
) -> Result<()> {
    let mut columns = input.columns().to_vec();
    match join_type {
        JoinType::Anti => {}
        JoinType::Mark => {
            // No rows: every mark is false; only NULL keys: every mark is NULL.
            let mut mark = Vector::flat(LogicalType::BOOLEAN, input.len());
            if has_null {
                mark.set_validity(rustdb_types::ValidityMask::all_invalid(input.len()));
            }
            columns.push(mark);
        }
        JoinType::Left | JoinType::Outer | JoinType::Single => columns.extend(null_columns(right_types)),
        _ => return Ok(()),
    }
    result.set_columns(columns, input.len());
    Ok(())
}

/// The rows of `left` whose `found_match` is `wanted` (`ConstructSemiJoinResult` /
/// `ConstructAntiJoinResult`).
pub(crate) fn construct_semi_or_anti_join_result(
    left: &DataChunk,
    result: &mut DataChunk,
    found_match: &[bool],
    wanted: bool,
) {
    let sel: Vec<u32> = (0..left.len() as u32)
        .filter(|&row| found_match[row as usize] == wanted)
        .collect();
    if sel.is_empty() {
        result.set_columns(Vec::new(), 0);
        return;
    }
    let len = sel.len();
    result.set_columns(slice_columns(left, &sel), len);
}

/// `left` with a boolean mark column: NULL where a join key is NULL, otherwise whether a
/// match was found, and NULL instead of false when the right side has NULLs
/// (`PhysicalJoin::ConstructMarkJoinResult`).
pub(crate) fn construct_mark_join_result(
    join_keys: &DataChunk,
    left: &DataChunk,
    result: &mut DataChunk,
    found_match: Option<&[bool]>,
    has_null: bool,
) {
    let count = left.len();
    let mut mark = Vector::flat(LogicalType::BOOLEAN, count.max(1));
    let mut validity = rustdb_types::ValidityMask::all_valid();
    for column in join_keys.columns() {
        let keys = column.validity();
        if keys.is_all_valid() {
            continue;
        }
        let sel = column.sel();
        for row in 0..join_keys.len() {
            validity.set(row, keys.is_valid(sel.slot(row)), count);
        }
    }
    {
        let values = mark.values_mut::<bool>();
        match found_match {
            Some(found) => values[..count].copy_from_slice(&found[..count]),
            None => values[..count].fill(false),
        }
        if has_null {
            for row in 0..count {
                if !values[row] {
                    validity.set_invalid(row, count);
                }
            }
        }
    }
    mark.set_validity(validity);
    let mut columns = left.columns().to_vec();
    columns.push(mark);
    result.set_columns(columns, count);
}
