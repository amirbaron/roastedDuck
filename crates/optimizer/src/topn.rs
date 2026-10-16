//! Turning `ORDER BY` + `LIMIT` into a Top-N (DuckDB's `TopN` optimizer).

use std::sync::Arc;

use rustdb_ast::OrderByNullType;
use rustdb_planner::{BoundLimitNode, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType};
use rustdb_storage::TableFilter;
use rustdb_storage::table_filter::DynamicFilterData;
use rustdb_types::{LogicalTypeId, PhysicalType};

use crate::join_filter_pushdown::{
    PushdownColumn, PushdownOptions, get_pushdown_filter_targets, operator_at_path,
};

/// Whether `op` is a constant LIMIT (with an optional constant OFFSET) over an ORDER BY,
/// possibly through projections, that is cheaper as a Top-N (DuckDB's
/// `TopN::CanOptimize`; the child's cardinality is used when already estimated).
pub fn can_optimize(op: &LogicalOperator) -> bool {
    let LogicalKind::Limit { limit, offset } = &op.kind else {
        return false;
    };
    let BoundLimitNode::ConstantValue(limit) = limit else {
        return false;
    };
    let offset = match offset {
        BoundLimitNode::ExpressionValue(_) => return false,
        BoundLimitNode::ConstantValue(offset) => Some(*offset),
        _ => None,
    };
    let Some(mut child) = op.children.first() else {
        return false;
    };
    if let Some(child_cardinality) = child.estimated_cardinality {
        // Above 0.7% of the input (and 5000 rows) sorting everything is faster.
        #[allow(clippy::cast_precision_loss)]
        let constant_limit = *limit as f64 + offset.map_or(0.0, |offset| offset as f64);
        #[allow(clippy::cast_precision_loss)]
        let child_cardinality = child_cardinality as f64;
        if constant_limit > child_cardinality * 0.007 && constant_limit > 5000.0 {
            return false;
        }
    }
    while child.operator_type() == LogicalOperatorType::LogicalProjection {
        let Some(next) = child.children.first() else {
            return false;
        };
        child = next;
    }
    child.operator_type() == LogicalOperatorType::LogicalOrderBy
}

/// Turns every LIMIT over an ORDER BY into a Top-N, keeping the projections between
/// them above it (DuckDB's `TopN::Optimize`).
pub fn optimize(mut op: LogicalOperator) -> LogicalOperator {
    if let Some(child) = op.children.first_mut() {
        // DuckDB estimates the child first to decide on the cheaper strategy.
        child.estimate_cardinality();
    }
    if can_optimize(&op)
        && let LogicalKind::Limit { limit, offset } = &op.kind
        && let BoundLimitNode::ConstantValue(limit) = *limit
    {
        let offset = match offset {
            BoundLimitNode::ConstantValue(offset) => *offset,
            _ => 0,
        };
        let mut projections = Vec::new();
        let mut child = op.children.pop();
        while let Some(mut node) =
            child.take_if(|node| node.operator_type() == LogicalOperatorType::LogicalProjection)
        {
            child = node.children.pop();
            projections.push(node);
        }
        if let Some(order) = child
            && let LogicalKind::Order { orders, .. } = order.kind
        {
            let input = order.children.into_iter().next();
            let estimate = input
                .as_ref()
                .and_then(|input| input.estimated_cardinality)
                .map_or(limit, |estimate| estimate.min(limit));
            let mut top_n = LogicalOperator::new(LogicalKind::TopN {
                orders,
                limit,
                offset,
                dynamic_filter: None,
            })
            .with_children(input.into_iter().collect());
            top_n.estimated_cardinality = Some(estimate);
            op = top_n;
            while let Some(mut projection) = projections.pop() {
                projection.children = vec![op];
                op = projection;
            }
        }
    }
    if op.operator_type() == LogicalOperatorType::LogicalTopN {
        pushdown_dynamic_filters(&mut op);
    }
    op.children = std::mem::take(&mut op.children)
        .into_iter()
        .map(optimize)
        .collect();
    op
}

/// Pushes a run-time filter on the first ORDER BY column into the scans below a Top-N:
/// once the heap is full, rows beyond its boundary cannot be in the result (DuckDB's
/// `TopN::PushdownDynamicFilters`). Integral and VARCHAR column references only.
fn pushdown_dynamic_filters(op: &mut LogicalOperator) {
    let LogicalKind::TopN {
        orders,
        dynamic_filter,
        ..
    } = &op.kind
    else {
        return;
    };
    let Some(first) = orders.first() else {
        return;
    };
    let ty = &first.expression.return_type;
    let integral = matches!(
        ty.physical_type(),
        PhysicalType::Int8
            | PhysicalType::Int16
            | PhysicalType::Int32
            | PhysicalType::Int64
            | PhysicalType::Int128
            | PhysicalType::UInt8
            | PhysicalType::UInt16
            | PhysicalType::UInt32
            | PhysicalType::UInt64
            | PhysicalType::UInt128
    );
    if !integral && ty.id() != LogicalTypeId::Varchar {
        return;
    }
    let ExpressionKind::ColumnRef(column) = &first.expression.kind else {
        return;
    };
    if dynamic_filter.is_some() {
        return;
    }
    let nulls_first = first.null_order == OrderByNullType::NullsFirst;
    let mut targets = Vec::new();
    let mut path = vec![0];
    if let Some(child) = op.children.first() {
        let columns = vec![PushdownColumn {
            binding: column.binding,
            storage_type: None,
        }];
        let options = PushdownOptions {
            allow_casts: false,
            through_limits: true,
        };
        get_pushdown_filter_targets(child, columns, options, &mut path, &mut targets);
    }
    if targets.is_empty() {
        return;
    }
    let data = Arc::new(DynamicFilterData::default());
    if let LogicalKind::TopN { dynamic_filter, .. } = &mut op.kind {
        *dynamic_filter = Some(Arc::clone(&data));
    }
    for target in targets {
        let Some(LogicalKind::Get(get)) = operator_at_path(op, &target.path).map(|node| &mut node.kind)
        else {
            continue;
        };
        let Some(column) = target
            .columns
            .first()
            .and_then(|column| get.column_ids.get(column.binding.column_index as usize))
        else {
            continue;
        };
        let column = usize::try_from(column.index).unwrap_or(usize::MAX);
        let mut pushed = TableFilter::Dynamic(Arc::clone(&data));
        if nulls_first {
            pushed = TableFilter::Or(vec![TableFilter::IsNull, pushed]);
        }
        get.table_filters
            .push_filter(column, TableFilter::Optional(Some(Box::new(pushed))));
    }
}
