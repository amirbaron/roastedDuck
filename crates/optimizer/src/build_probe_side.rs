//! Choosing the build side of joins (DuckDB's `BuildProbeSideOptimizer`): the right
//! child of a hash join is materialized in the hash table, so the children are swapped
//! when the left one is cheaper to build on.

use rustdb_ast::JoinType;
use rustdb_common::Result;
use rustdb_planner::{ColumnBinding, ExpressionType, LogicalKind, LogicalOperator, LogicalOperatorType};
use rustdb_types::{LogicalType, LogicalTypeId, PhysicalType};

use crate::expression_util::flip_comparison;

/// Extra row width per (nested) column (DuckDB's `COLUMN_COUNT_PENALTY`).
const COLUMN_COUNT_PENALTY: usize = 2;
/// Extra build cost of a base table when the other side already has joins (DuckDB's
/// `PREFER_RIGHT_DEEP_PENALTY`).
const PREFER_RIGHT_DEEP_PENALTY: f64 = 0.15;
/// `sizeof(ht_entry_t)`.
const HT_ENTRY_SIZE: usize = 8;

/// Picks the build side of every join in `plan`.
pub fn optimize_build_probe_side(plan: &mut LogicalOperator, prefer_range_joins: bool) -> Result<()> {
    let mut preferred_on_probe_side = Vec::new();
    row_id_bindings(plan, &mut preferred_on_probe_side);
    plan.resolve_operator_types()?;
    let optimizer = BuildProbeSideOptimizer {
        preferred_on_probe_side,
        prefer_range_joins,
    };
    optimizer.visit_operator(plan);
    Ok(())
}

/// The bindings of scans that read row ids (likely the target of an UPDATE or DELETE),
/// which are preferably kept on the probe side (DuckDB's `GetRowidBindings`).
fn row_id_bindings(op: &LogicalOperator, bindings: &mut Vec<ColumnBinding>) {
    if let LogicalKind::Get(get) = &op.kind
        && get.column_ids.iter().any(rustdb_planner::operator::ColumnIndex::is_row_id_column)
    {
        bindings.extend(op.column_bindings());
    }
    for child in &op.children {
        row_id_bindings(child, bindings);
    }
}

/// The inverse join type for swapped children; `None` for join types that cannot be
/// swapped (DuckDB's `HasInverseJoinType` / `InverseJoinType`).
pub(crate) const fn inverse_join_type(join_type: JoinType) -> Option<JoinType> {
    Some(match join_type {
        JoinType::Left => JoinType::Right,
        JoinType::Right => JoinType::Left,
        JoinType::Semi => JoinType::RightSemi,
        JoinType::Anti => JoinType::RightAnti,
        JoinType::RightSemi => JoinType::Semi,
        JoinType::RightAnti => JoinType::Anti,
        JoinType::Single | JoinType::Mark => return None,
        other => other,
    })
}

struct BuildProbeSideOptimizer {
    preferred_on_probe_side: Vec<ColumnBinding>,
    prefer_range_joins: bool,
}

impl BuildProbeSideOptimizer {
    fn visit_operator(&self, op: &mut LogicalOperator) {
        match &mut op.kind {
            LogicalKind::ComparisonJoin(join) => match join.operator_type {
                LogicalOperatorType::LogicalDelimJoin => {
                    if inverse_join_type(join.join_type).is_some() {
                        let flipped = self.try_flip_join_children(op);
                        if let LogicalKind::ComparisonJoin(join) = &mut op.kind {
                            join.delim_flipped = flipped;
                        }
                    }
                }
                LogicalOperatorType::LogicalComparisonJoin => match join.join_type {
                    JoinType::Semi | JoinType::Anti => {
                        // There is no inequality RIGHT_SEMI/RIGHT_ANTI join (yet).
                        let has_equality = join.conditions.iter().any(|condition| {
                            matches!(
                                condition.comparison,
                                ExpressionType::CompareEqual | ExpressionType::CompareNotDistinctFrom
                            )
                        });
                        if has_equality && !self.prefer_range_joins {
                            self.try_flip_join_children(op);
                        }
                    }
                    join_type => {
                        if inverse_join_type(join_type).is_some() {
                            self.try_flip_join_children(op);
                        }
                    }
                },
                _ => {}
            },
            LogicalKind::AnyJoin { join_type, .. } => {
                // RIGHT_SEMI/RIGHT_ANTI are not supported for these.
                if !matches!(join_type, JoinType::Semi | JoinType::Anti) {
                    self.try_flip_join_children(op);
                }
            }
            LogicalKind::CrossProduct => {
                self.try_flip_join_children(op);
            }
            _ => {}
        }
        for child in &mut op.children {
            self.visit_operator(child);
        }
    }

    /// Swaps the children when the right one is more expensive to build a hash table on
    /// (DuckDB's `TryFlipJoinChildren`). Returns whether they were swapped.
    fn try_flip_join_children(&self, op: &mut LogicalOperator) -> bool {
        if op.children.len() != 2 {
            return false;
        }
        let lhs_cardinality = known_or_estimated_cardinality(&mut op.children[0]);
        let rhs_cardinality = known_or_estimated_cardinality(&mut op.children[1]);
        let left_side_build_cost = build_size(&op.children[0].types, lhs_cardinality);
        let mut right_side_build_cost = build_size(&op.children[1].types, rhs_cardinality);
        let left_child_joins = child_has_joins(&op.children[0]);
        let right_child_joins = child_has_joins(&op.children[1]);
        // A base table on the right against joins on the left: building on the
        // already-joined tuples is cheaper.
        if right_child_joins == 0 && left_child_joins > 0 {
            right_side_build_cost *= 1.0 + PREFER_RIGHT_DEEP_PENALTY;
        }
        let mut swap = right_side_build_cost > left_side_build_cost;
        if rhs_cardinality == lhs_cardinality && !self.preferred_on_probe_side.is_empty() {
            // Keep row id bindings on the probe side.
            let in_left = overlapping_bindings(&op.children[0].column_bindings(), &self.preferred_on_probe_side);
            let in_right = overlapping_bindings(&op.children[1].column_bindings(), &self.preferred_on_probe_side);
            if (swap && in_left > in_right) || (!swap && in_right > in_left) {
                swap = !swap;
            }
        }
        if swap {
            flip_children(op);
        }
        swap
    }
}

/// The cardinality the join order optimizer estimated, else the simple estimate.
fn known_or_estimated_cardinality(op: &mut LogicalOperator) -> u64 {
    match op.estimated_cardinality {
        Some(estimate) => estimate,
        None => op.estimate_cardinality(),
    }
}

/// Swaps the children of a join (DuckDB's `FlipChildren`).
fn flip_children(op: &mut LogicalOperator) {
    op.children.swap(0, 1);
    match &mut op.kind {
        LogicalKind::ComparisonJoin(join) => {
            if let Some(inverse) = inverse_join_type(join.join_type) {
                join.join_type = inverse;
            }
            for condition in &mut join.conditions {
                std::mem::swap(&mut condition.left, &mut condition.right);
                condition.comparison = flip_comparison(condition.comparison);
            }
            std::mem::swap(&mut join.left_projection_map, &mut join.right_projection_map);
        }
        LogicalKind::AnyJoin { join_type, .. } => {
            if let Some(inverse) = inverse_join_type(*join_type) {
                *join_type = inverse;
            }
        }
        _ => {}
    }
}

/// How many of `needles` are in `haystack` (DuckDB's `ComputeOverlappingBindings`).
fn overlapping_bindings(haystack: &[ColumnBinding], needles: &[ColumnBinding]) -> usize {
    needles.iter().filter(|needle| haystack.contains(needle)).count()
}

/// The number of joins below `op` along its first children (DuckDB's `ChildHasJoins`).
fn child_has_joins(op: &LogicalOperator) -> usize {
    let Some(first) = op.children.first() else {
        return 0;
    };
    let is_join = match &op.kind {
        LogicalKind::ComparisonJoin(join) => matches!(
            join.operator_type,
            LogicalOperatorType::LogicalComparisonJoin | LogicalOperatorType::LogicalAsofJoin
        ),
        LogicalKind::CrossProduct => true,
        _ => false,
    };
    if is_join && let Some(second) = op.children.get(1) {
        return 1 + child_has_joins(first) + child_has_joins(second);
    }
    child_has_joins(first)
}

/// The estimated size of a hash table on rows of `types` (DuckDB's `GetBuildSize`): the
/// row width of the tuple layout, penalties for variable-size and nested columns, and
/// the hash table entries.
#[allow(clippy::cast_precision_loss)]
fn build_size(types: &[LogicalType], cardinality: u64) -> f64 {
    let mut types = types.to_vec();
    types.push(LogicalType::UBIGINT); // the hash
    let mut row_width = tuple_row_width(&types, true);
    for ty in &types {
        visit_type(ty, &mut |visited| {
            match physical_type(visited) {
                PhysicalType::Varchar => row_width += 8,
                PhysicalType::List | PhysicalType::Array => row_width += 32,
                _ => {}
            }
            row_width += COLUMN_COUNT_PENALTY;
        });
    }
    row_width += 3 * HT_ENTRY_SIZE;
    (row_width as u64 * cardinality) as f64
}

/// DuckDB's physical type, where BIT values are strings.
fn physical_type(ty: &LogicalType) -> PhysicalType {
    match ty.physical_type() {
        PhysicalType::Bit => PhysicalType::Varchar,
        other => other,
    }
}

/// DuckDB's `TypeIsConstantSize`.
const fn is_constant_size(ty: PhysicalType) -> bool {
    matches!(
        ty,
        PhysicalType::Bool
            | PhysicalType::Int8
            | PhysicalType::Int16
            | PhysicalType::Int32
            | PhysicalType::Int64
            | PhysicalType::UInt8
            | PhysicalType::UInt16
            | PhysicalType::UInt32
            | PhysicalType::UInt64
            | PhysicalType::Int128
            | PhysicalType::UInt128
            | PhysicalType::Float
            | PhysicalType::Double
            | PhysicalType::Interval
    )
}

/// Whether every column of a tuple layout has a constant size.
fn all_constant(types: &[LogicalType]) -> bool {
    types.iter().all(|ty| match physical_type(ty) {
        PhysicalType::Struct => all_constant(&struct_child_types(ty)),
        other => is_constant_size(other),
    })
}

/// The child types of a STRUCT-backed type (structs and unions, including the tag).
fn struct_child_types(ty: &LogicalType) -> Vec<LogicalType> {
    ty.struct_children()
        .map(|children| children.iter().map(|(_, child)| child.clone()).collect())
        .unwrap_or_default()
}

/// The row width of DuckDB's `TupleDataLayout` for `types` (nullable columns, no
/// aggregates).
fn tuple_row_width(types: &[LogicalType], top_level: bool) -> usize {
    let mut row_width = types.len().div_ceil(8);
    if top_level && !all_constant(types) {
        row_width += 8; // the heap size offset
    }
    for ty in types {
        let physical = physical_type(ty);
        if is_constant_size(physical) || physical == PhysicalType::Varchar {
            row_width += physical.size();
        } else if physical == PhysicalType::Struct {
            row_width += tuple_row_width(&struct_child_types(ty), false);
        } else {
            row_width += 8;
        }
    }
    row_width
}

/// Calls `f` on every type nested in `ty`, then on `ty` (DuckDB's
/// `TypeVisitor::VisitReplace`).
fn visit_type(ty: &LogicalType, f: &mut impl FnMut(&LogicalType)) {
    match ty.id() {
        LogicalTypeId::Struct => {
            for (_, child) in ty.struct_children().unwrap_or_default() {
                visit_type(child, f);
            }
        }
        LogicalTypeId::Union => {
            for (_, member) in ty.union_members().unwrap_or_default() {
                visit_type(member, f);
            }
        }
        LogicalTypeId::List | LogicalTypeId::Array => {
            if let Some(child) = ty.child_type() {
                visit_type(child, f);
            }
        }
        LogicalTypeId::Map => {
            if let Some((key, value)) = ty.map_key_value() {
                visit_type(key, f);
                visit_type(value, f);
            }
        }
        _ => {}
    }
    f(ty);
}
