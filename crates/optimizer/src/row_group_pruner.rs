//! Reading a table's row groups in the order of an ORDER BY column (DuckDB's
//! `RowGroupPruner`).
//!
//! For `ORDER BY column [LIMIT n [OFFSET m]]` over a table scan, the scan reads its row
//! groups by the column's minimum (ascending) or maximum (descending), so a Top-N above it
//! tightens its boundary early; with a LIMIT and no filters, row groups that cannot hold
//! any of the first `n + m` rows are not scanned, and row groups an OFFSET skips entirely
//! are left out with the OFFSET reduced accordingly.

use rustdb_ast::{OrderByNullType, OrderType as AstOrderType};
use rustdb_catalog::EntryKind;
use rustdb_planner::expression::BoundOrderByNode;
use rustdb_planner::operator::LogicalGet;
use rustdb_planner::{BoundLimitNode, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType};
use rustdb_row::sort::{NullOrder, OrderType};
use rustdb_storage::table::row_group_reorderer::offset_after_pruning;
use rustdb_storage::table::{OrderByColumnType, OrderByStatistics, RowGroupOrderOptions, RowGroupPartition};
use rustdb_types::LogicalTypeId;

use crate::Optimizer;
use crate::join_filter_pushdown::{PushdownColumn, PushdownOptions, get_pushdown_filter_targets, operator_at_path};

/// Sets the scan order below every LIMIT over an ORDER BY that allows it
/// (`RowGroupPruner::Optimize`).
pub fn optimize(optimizer: &Optimizer, mut op: LogicalOperator) -> LogicalOperator {
    if !try_optimize(optimizer, &mut op) {
        op.children = std::mem::take(&mut op.children)
            .into_iter()
            .map(|child| optimize(optimizer, child))
            .collect();
    }
    op
}

/// `RowGroupPruner::TryOptimize`.
fn try_optimize(optimizer: &Optimizer, op: &mut LogicalOperator) -> bool {
    let LogicalKind::Limit { limit, offset } = &op.kind else {
        return false;
    };
    // `GetLimitAndOffset`: an unset LIMIT is unbounded; an unset OFFSET is zero.
    let mut row_limit = match limit {
        BoundLimitNode::ConstantValue(limit) => Some(*limit),
        _ => None,
    };
    let mut row_offset = match offset {
        BoundLimitNode::ConstantValue(offset) => Some(*offset),
        BoundLimitNode::Unset => Some(0),
        _ => None,
    };
    let Some(order_path) = find_logical_order(op) else {
        return false;
    };
    let Some(order) = operator_at_path(op, &order_path) else {
        return false;
    };
    // Only reorder row groups if no other limit can change the order.
    let mut current: &LogicalOperator = order;
    while let Some(child) = current.children.first() {
        if current.children.len() > 1 {
            return false;
        }
        match current.operator_type() {
            LogicalOperatorType::LogicalLimit => return false,
            LogicalOperatorType::LogicalFilter
            | LogicalOperatorType::LogicalAggregateAndGroupBy
            | LogicalOperatorType::LogicalDistinct => {
                row_limit = None;
                row_offset = None;
            }
            _ => {}
        }
        current = child;
    }
    let LogicalKind::Order { orders, .. } = &order.kind else {
        return false;
    };
    let Some(primary_order) = orders.first().cloned() else {
        return false;
    };
    let Some((get_path, column)) = find_logical_get(order, &primary_order) else {
        return false;
    };
    let mut path = order_path;
    path.extend(get_path);
    let Some(LogicalKind::Get(get)) = operator_at_path(op, &path).map(|node| &node.kind) else {
        return false;
    };
    // `TryGetStorageIndex`: a stored column of a table.
    let Some(physical) = storage_index(get, column) else {
        return false;
    };
    if !get.table_filters.filters.is_empty() {
        // With filters the row groups are only ordered, not pruned.
        row_limit = None;
        row_offset = None;
    }
    let (options, offset_remainder) =
        create_row_group_reorderer_options(optimizer, row_limit, row_offset, &primary_order, get, physical);
    if let Some(remainder) = offset_remainder
        && let LogicalKind::Limit { offset, .. } = &mut op.kind
    {
        *offset = BoundLimitNode::ConstantValue(remainder);
    }
    if let Some(LogicalKind::Get(get)) = operator_at_path(op, &path).map(|node| &mut node.kind) {
        get.row_group_order = Some(options);
    }
    true
}

/// The path from the LIMIT to the ORDER BY below it, through projections, if its first
/// order is a column of a numeric, temporal or VARCHAR type
/// (`RowGroupPruner::FindLogicalOrder`).
fn find_logical_order(limit: &LogicalOperator) -> Option<Vec<usize>> {
    let mut path = vec![0];
    let mut current = limit.children.first()?;
    while current.operator_type() == LogicalOperatorType::LogicalProjection {
        current = current.children.first()?;
        path.push(0);
    }
    let LogicalKind::Order { orders, .. } = &current.kind else {
        return None;
    };
    let expression = &orders.first()?.expression;
    let ty = &expression.return_type;
    if !ty.is_numeric() && !ty.is_temporal() && ty.id() != LogicalTypeId::Varchar {
        return None;
    }
    matches!(expression.kind, ExpressionKind::ColumnRef(_)).then_some(path)
}

/// The scan the first order column comes from: the path below the ORDER BY and the
/// position of the column in the scan's columns (`RowGroupPruner::FindLogicalGet`).
fn find_logical_get(order: &LogicalOperator, primary_order: &BoundOrderByNode) -> Option<(Vec<usize>, usize)> {
    let ExpressionKind::ColumnRef(column) = &primary_order.expression.kind else {
        return None;
    };
    let columns = vec![PushdownColumn {
        binding: column.binding,
        storage_type: None,
    }];
    let options = PushdownOptions {
        allow_casts: false,
        through_limits: true,
    };
    let mut path = vec![0];
    let mut targets = Vec::new();
    get_pushdown_filter_targets(order.children.first()?, columns, options, &mut path, &mut targets);
    let [target] = &targets[..] else {
        return None;
    };
    let mut node = order;
    for &index in &target.path {
        node = node.children.get(index)?;
    }
    // Only table scans can order their row groups (`set_scan_order`).
    let LogicalKind::Get(get) = &node.kind else {
        return None;
    };
    get.table.as_ref()?;
    let position = target.columns.first()?.binding.column_index as usize;
    Some((target.path.clone(), position))
}

/// The physical index of scanned column `position` of `get`'s table.
fn storage_index(get: &LogicalGet, position: usize) -> Option<usize> {
    let column = get.column_ids.get(position)?;
    if column.is_row_id_column() || !column.child_indexes.is_empty() {
        return None;
    }
    let EntryKind::Table(table) = &get.table.as_ref()?.kind else {
        return None;
    };
    table.logical_to_physical(usize::try_from(column.index).ok()?)
}

/// The scan order of `primary_order` on physical column `column`, and the OFFSET left after
/// pruning the row groups it skips, if any (`CreateRowGroupReordererOptions`).
fn create_row_group_reorderer_options(
    optimizer: &Optimizer,
    row_limit: Option<u64>,
    row_offset: Option<u64>,
    primary_order: &BoundOrderByNode,
    get: &LogicalGet,
    column: usize,
) -> (RowGroupOrderOptions, Option<u64>) {
    let column_type = if primary_order.expression.return_type.id() == LogicalTypeId::Varchar {
        OrderByColumnType::String
    } else {
        OrderByColumnType::Numeric
    };
    let order_type = if primary_order.order_type == AstOrderType::Ascending {
        OrderType::Ascending
    } else {
        OrderType::Descending
    };
    let null_order = if primary_order.null_order == OrderByNullType::NullsFirst {
        NullOrder::NullsFirst
    } else {
        NullOrder::NullsLast
    };
    let order_by = if order_type == OrderType::Ascending {
        OrderByStatistics::Min
    } else {
        OrderByStatistics::Max
    };
    let mut options = RowGroupOrderOptions {
        column,
        order_by,
        order_type,
        null_order,
        column_type,
        row_limit: row_limit.map(|limit| limit + row_offset.unwrap_or(0)),
        row_group_offset: 0,
        leading_null_group_offset: 0,
    };
    if let Some(offset) = row_offset.filter(|&offset| offset > 0)
        && let Some(partitions) = partition_stats(optimizer, get, column)
    {
        let result = offset_after_pruning(order_by, column_type, order_type, null_order, offset, partitions);
        if result.pruned_row_group_count > 0 || result.leading_null_group_offset > 0 {
            // Row groups and NULL-only row groups the OFFSET skips are not scanned.
            options.row_limit = row_limit.map(|limit| limit + result.offset_remainder);
            options.row_group_offset = result.pruned_row_group_count;
            options.leading_null_group_offset = result.leading_null_group_offset;
            return (options, Some(result.offset_remainder));
        }
    }
    (options, None)
}

/// The row groups of `get`'s table with the statistics of physical column `column`
/// (`TableScanGetPartitionStats`); `None` when there are none. Rows the transaction added
/// have version information, so their counts are approximate.
fn partition_stats(optimizer: &Optimizer, get: &LogicalGet, column: usize) -> Option<Vec<RowGroupPartition>> {
    let EntryKind::Table(table) = &get.table.as_ref()?.kind else {
        return None;
    };
    let storage = table.storage.as_ref()?;
    let local_rows = optimizer.context().transaction.local_storage().added_rows(storage);
    let mut partitions: Vec<RowGroupPartition> = storage
        .row_groups()
        .row_groups()
        .tree()
        .nodes()
        .into_iter()
        .map(|node| RowGroupPartition {
            count: node.node.count(),
            exact: node.node.version_info().is_none(),
            stats: node.node.column_statistics(column).ok(),
        })
        .collect();
    if local_rows > 0 {
        partitions.push(RowGroupPartition {
            count: local_rows,
            exact: false,
            stats: None,
        });
    }
    (!partitions.is_empty()).then_some(partitions)
}
