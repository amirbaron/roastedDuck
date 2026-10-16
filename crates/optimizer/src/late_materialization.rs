//! Late materialization (DuckDB's `LateMaterialization`): a Top-N, LIMIT with OFFSET or
//! row-count SAMPLE over a table scan only needs a few columns to pick its rows. The scan
//! below it then reads just those columns plus the row ids, and a semi join on the row id
//! with a second scan of the table fetches the remaining columns for the chosen rows.

use std::collections::BTreeSet;

use rustdb_ast::{ExpressionType, JoinType, OrderByNullType, OrderType};
use rustdb_common::{Error, Result};
use rustdb_planner::expression::BoundOrderByNode;
use rustdb_planner::operator::{
    COLUMN_IDENTIFIER_ROW_ID, ColumnIndex, JoinCondition, LogicalComparisonJoin, LogicalGet,
};
use rustdb_planner::{
    BoundLimitNode, ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator,
    LogicalOperatorType,
};
use rustdb_types::LogicalType;

use crate::expression_util::visit_column_refs;
use crate::optimizer::Optimizer;
use crate::remove_unused_columns::remove_unused_columns;

/// The name of the row id column (the table scan's virtual column).
const ROW_ID_NAME: &str = "rowid";

/// Limits above this many rows are only rewritten without filters (`LIMIT_MAX_VAL`).
const LIMIT_MAX_VAL: u64 = 1_000_000;

/// Rewrites the Top-N, LIMIT and SAMPLE operators of `plan` that benefit (DuckDB's
/// `LateMaterialization::Optimize`).
pub fn late_materialize(optimizer: &Optimizer, plan: &mut LogicalOperator) -> Result<()> {
    let max_row_count = optimizer.config.late_materialization_max_rows;
    let candidate = match &plan.kind {
        LogicalKind::Limit {
            limit: BoundLimitNode::ConstantValue(limit_val),
            offset,
        } => {
            let has_offset = !matches!(offset, BoundLimitNode::Unset);
            if *limit_val > max_row_count {
                // Large limits are worth it over consecutive row ids only.
                optimize_large_limit(optimizer, plan, *limit_val, has_offset)
            } else {
                // Small limits only gain with an offset.
                has_offset
            }
        }
        LogicalKind::TopN { limit, .. } => *limit <= max_row_count,
        LogicalKind::Sample { sample_options } => {
            !sample_options.is_percentage
                && sample_options
                    .sample_size
                    .as_i128()
                    .and_then(|size| u64::try_from(size).ok())
                    .is_some_and(|size| size <= max_row_count)
        }
        _ => false,
    };
    if candidate && try_late_materialization(optimizer, plan)? {
        return Ok(());
    }
    for child in &mut plan.children {
        late_materialize(optimizer, child)?;
    }
    Ok(())
}

/// DuckDB's `OptimizeLargeLimit`: only projections over an unfiltered scan, and only
/// while the insertion order is kept (or with an offset).
fn optimize_large_limit(
    optimizer: &Optimizer,
    limit: &LogicalOperator,
    limit_val: u64,
    has_offset: bool,
) -> bool {
    if !has_offset && !optimizer.config.preserve_insertion_order {
        return false;
    }
    if limit_val > LIMIT_MAX_VAL {
        return false;
    }
    let mut current = limit.children.first();
    while let Some(op) = current {
        match &op.kind {
            LogicalKind::Get(get) => return get.table_filters.is_empty(),
            LogicalKind::Projection { .. } => current = op.children.first(),
            _ => return false,
        }
    }
    false
}

/// The operator at `depth` steps down the first-child chain.
fn chain(op: &LogicalOperator, depth: usize) -> Option<&LogicalOperator> {
    let mut node = op;
    for _ in 0..depth {
        node = node.children.first()?;
    }
    Some(node)
}

/// The operator at `depth` steps down the first-child chain, mutably.
fn chain_mut(op: &mut LogicalOperator, depth: usize) -> Option<&mut LogicalOperator> {
    let mut node = op;
    for _ in 0..depth {
        node = node.children.first_mut()?;
    }
    Some(node)
}

/// The column references of `op`'s own expressions.
fn add_references(op: &mut LogicalOperator, references: &mut BTreeSet<ColumnBinding>) {
    op.for_each_expression_mut(|expression| {
        visit_column_refs(expression, &mut |column| {
            references.insert(column.binding);
        });
    });
}

/// The row id's position in the scan's columns, adding it if it is not scanned yet
/// (DuckDB's `LateMaterializationHelper::GetOrInsertRowIds`).
pub(crate) fn get_or_insert_row_id(get: &mut LogicalGet) -> u64 {
    if let Some(position) = get.column_ids.iter().position(ColumnIndex::is_row_id_column) {
        return position as u64;
    }
    get.column_ids.push(ColumnIndex::new(COLUMN_IDENTIFIER_ROW_ID));
    let position = get.column_ids.len() as u64 - 1;
    if !get.projection_ids.is_empty() {
        get.projection_ids.push(position);
    }
    position
}

/// A reference to the row id.
fn row_id_ref(binding: ColumnBinding) -> Expression {
    Expression::column_ref(ROW_ID_NAME, LogicalType::BIGINT, binding, 0)
}

/// Column `column_index` of `op` as an expression over `op`'s input (DuckDB's
/// `GetExpression`): a projection's expression or a reference to a scanned column.
fn get_expression(op: &LogicalOperator, column_index: u64) -> Result<Expression> {
    match &op.kind {
        LogicalKind::Get(get) => {
            let column = get
                .column_ids
                .get(column_index as usize)
                .ok_or_else(|| Error::internal("late materialization: column out of range"))?;
            let name = if column.is_row_id_column() {
                ROW_ID_NAME.to_owned()
            } else {
                get.names.get(column.index as usize).cloned().unwrap_or_default()
            };
            Ok(Expression::column_ref(
                name,
                get.column_type(column),
                ColumnBinding::new(get.table_index, column_index),
                0,
            ))
        }
        LogicalKind::Projection { .. } => op
            .expressions
            .get(column_index as usize)
            .cloned()
            .ok_or_else(|| Error::internal("late materialization: projection column out of range")),
        _ => Err(Error::internal(
            "Unsupported operator type for LateMaterialization::GetExpression",
        )),
    }
}

/// Replaces every column reference in `expression` by the expression `next` computes
/// for it (DuckDB's `ReplaceExpressionReferences`).
fn replace_expression_references(next: &LogicalOperator, expression: &mut Expression) -> Result<()> {
    if let ExpressionKind::ColumnRef(column) = &expression.kind {
        *expression = get_expression(next, column.binding.column_index)?;
        return Ok(());
    }
    let mut result = Ok(());
    expression.for_each_child_mut(|child| {
        if result.is_ok() {
            result = replace_expression_references(next, child);
        }
    });
    result
}

/// Sets the table index of every column reference in `expression` (DuckDB's
/// `ReplaceTableReferences`).
fn replace_table_references(expression: &mut Expression, table_index: u64) {
    if let ExpressionKind::ColumnRef(column) = &mut expression.kind {
        column.binding.table_index = table_index;
    }
    expression.for_each_child_mut(|child| replace_table_references(child, table_index));
}

/// Gives the top of the chain below `op` (the first projection or the scan) the table
/// index `new_index`, and points the references on the way at it (DuckDB's
/// `ReplaceTopLevelTableIndex`).
fn replace_top_level_table_index(op: &mut LogicalOperator, new_index: u64) -> Result<()> {
    let mut current = op;
    loop {
        match &mut current.kind {
            LogicalKind::Projection { table_index } => {
                *table_index = new_index;
                return Ok(());
            }
            LogicalKind::Get(get) => {
                get.table_index = new_index;
                return Ok(());
            }
            LogicalKind::TopN { orders, .. } => {
                for order in orders {
                    replace_table_references(&mut order.expression, new_index);
                }
            }
            LogicalKind::Filter { .. } | LogicalKind::Sample { .. } | LogicalKind::Limit { .. } => {
                for expression in &mut current.expressions {
                    replace_table_references(expression, new_index);
                }
            }
            _ => {
                return Err(Error::internal(
                    "Unsupported operator type in LateMaterialization::ReplaceTopLevelTableIndex",
                ));
            }
        }
        current = current
            .children
            .first_mut()
            .ok_or_else(|| Error::internal("late materialization: chain without a scan"))?;
    }
}

/// Projects the row id up from the scan at `depth` below `op` through the operators in
/// between (DuckDB's `ConstructRHS`); returns its binding at the top of the chain.
fn construct_rhs(op: &mut LogicalOperator, depth: usize) -> Result<ColumnBinding> {
    let scan = chain_mut(op, depth).ok_or_else(|| Error::internal("late materialization: missing scan"))?;
    let LogicalKind::Get(get) = &mut scan.kind else {
        return Err(Error::internal("late materialization: missing scan"));
    };
    let row_id_index = get_or_insert_row_id(get);
    let mut column_count = if get.projection_ids.is_empty() {
        get.column_ids.len()
    } else {
        get.projection_ids.len()
    };
    let mut binding = ColumnBinding::new(get.table_index, row_id_index);
    for level in (1..depth).rev() {
        let node =
            chain_mut(op, level).ok_or_else(|| Error::internal("late materialization: broken chain"))?;
        match &mut node.kind {
            LogicalKind::Projection { table_index } => {
                let table_index = *table_index;
                node.expressions.push(row_id_ref(binding));
                binding = ColumnBinding::new(table_index, node.expressions.len() as u64 - 1);
                column_count = node.expressions.len();
            }
            LogicalKind::Filter { projection_map } => {
                if !projection_map.is_empty() {
                    projection_map.push(column_count as u64 - 1);
                }
            }
            _ => {
                return Err(Error::internal(
                    "Unsupported logical operator in LateMaterialization::ConstructRHS",
                ));
            }
        }
    }
    Ok(binding)
}

/// DuckDB's `TryLateMaterialization`.
fn try_late_materialization(optimizer: &Optimizer, op: &mut LogicalOperator) -> Result<bool> {
    // The columns the root needs, traced down through projections and filters.
    let mut references = BTreeSet::new();
    add_references(op, &mut references);
    let mut source_depths = Vec::new();
    let mut depth = 1;
    let scan_depth = loop {
        let Some(child) = chain_mut(op, depth) else {
            return Ok(false);
        };
        match child.operator_type() {
            LogicalOperatorType::LogicalGet => break depth,
            LogicalOperatorType::LogicalProjection => {
                let LogicalKind::Projection { table_index } = child.kind else {
                    return Ok(false);
                };
                source_depths.push(depth);
                if child.expressions.iter().any(Expression::is_volatile) {
                    return Ok(false);
                }
                let referenced: BTreeSet<u64> = references
                    .iter()
                    .filter(|binding| binding.table_index == table_index)
                    .map(|binding| binding.column_index)
                    .collect();
                references.clear();
                for index in referenced {
                    if let Some(expression) = child.expressions.get(index as usize) {
                        visit_column_refs(expression, &mut |column| {
                            references.insert(column.binding);
                        });
                    }
                }
            }
            LogicalOperatorType::LogicalFilter => add_references(child, &mut references),
            _ => return Ok(false),
        }
        depth += 1;
    };
    let Some(LogicalKind::Get(get)) = chain(op, scan_depth).map(|scan| &scan.kind) else {
        return Ok(false);
    };
    // Only table scans fetch rows by row id; and the rewrite must drop columns.
    if references.len() >= get.column_ids.len() || get.table.is_none() {
        return Ok(false);
    }

    // The second scan of the table, which fetches the rows' columns (DuckDB's
    // `CreateLHSGet`): the same columns, without the filters.
    let mut lhs_get = get.clone();
    lhs_get.table_index = optimizer.generate_table_index();
    lhs_get.table_filters = rustdb_storage::TableFilterSet::default();
    lhs_get.dynamic_filters = None;
    let lhs_index = lhs_get.table_index;
    let lhs_columns = lhs_get.column_ids.len();
    let lhs_row_id = ColumnBinding::new(lhs_index, get_or_insert_row_id(&mut lhs_get));
    let lhs = LogicalOperator::new(LogicalKind::Get(lhs_get));

    // The final columns: the root projection inlined down to the second scan.
    let mut final_proj_list = Vec::new();
    if let Some(&root_depth) = source_depths.first() {
        let root =
            chain(op, root_depth).ok_or_else(|| Error::internal("late materialization: broken chain"))?;
        final_proj_list.clone_from(&root.expressions);
        for (i, _) in source_depths.iter().enumerate() {
            let next = match source_depths.get(i + 1) {
                Some(&next_depth) => chain(op, next_depth)
                    .ok_or_else(|| Error::internal("late materialization: broken chain"))?,
                None => &lhs,
            };
            for expression in &mut final_proj_list {
                replace_expression_references(next, expression)?;
            }
        }
    } else {
        for column in 0..lhs_columns {
            final_proj_list.push(get_expression(&lhs, column as u64)?);
        }
    }

    // The order to restore: the Top-N's, or the row ids'.
    let is_top_n = op.operator_type() == LogicalOperatorType::LogicalTopN;
    let mut final_orders: Vec<BoundOrderByNode> = match &op.kind {
        LogicalKind::TopN { orders, .. } => orders.clone(),
        _ => vec![BoundOrderByNode::new(
            OrderType::Ascending,
            OrderByNullType::NullsLast,
            row_id_ref(lhs_row_id),
        )],
    };

    // The original plan, with the row id projected up to it.
    let mut rhs_row_id = construct_rhs(op, scan_depth)?;
    // The final projection keeps the original top-level table index; the original chain
    // gets a new one.
    let final_index = rhs_row_id.table_index;
    let rhs_table_index = optimizer.generate_table_index();
    rhs_row_id.table_index = rhs_table_index;
    replace_top_level_table_index(op, rhs_table_index)?;

    let original = std::mem::replace(op, LogicalOperator::new(LogicalKind::CrossProduct));
    let mut join = LogicalComparisonJoin::new(JoinType::Semi, LogicalOperatorType::LogicalComparisonJoin);
    join.conditions.push(JoinCondition {
        left: row_id_ref(lhs_row_id),
        right: row_id_ref(rhs_row_id),
        comparison: ExpressionType::CompareEqual,
    });
    let join =
        LogicalOperator::new(LogicalKind::ComparisonJoin(Box::new(join))).with_children(vec![lhs, original]);
    let order = |orders: Vec<BoundOrderByNode>, child: LogicalOperator| {
        LogicalOperator::new(LogicalKind::Order {
            orders,
            projection_map: Vec::new(),
        })
        .with_children(vec![child])
    };
    *op = if is_top_n {
        // Top-N orders on expressions: order after the final projection.
        let projection = LogicalOperator::projection(final_index, final_proj_list, join);
        for order in &mut final_orders {
            replace_table_references(&mut order.expression, final_index);
        }
        order(final_orders, projection)
    } else {
        // LIMIT and SAMPLE order on the row ids, which the final projection drops.
        LogicalOperator::projection(final_index, final_proj_list, order(final_orders, join))
    };
    remove_unused_columns(optimizer, op)?;
    Ok(true)
}
