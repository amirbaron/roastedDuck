//! Turning windows of whole-partition aggregates into a join with a grouped aggregate
//! (DuckDB's `WindowSelfJoinOptimizer`): `count(*) OVER (PARTITION BY k)` is the same for
//! every row of a partition, so the window becomes an inner join (on `k IS NOT DISTINCT
//! FROM k`) of its input with the aggregate of a copy of the input grouped by `k`.

use rustdb_ast::{ExpressionType, JoinType, WindowBoundary, WindowExcludeMode};
use rustdb_common::Result;
use rustdb_planner::expression::{
    AggregateType, BoundAggregateExpression, BoundOrderModifier, BoundWindowExpression,
};
use rustdb_planner::operator::{JoinCondition, LogicalAggregate, LogicalComparisonJoin};
use rustdb_planner::{
    ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};

use crate::column_binding_replacer::{ColumnBindingReplacer, ReplacementBinding};
use crate::logical_operator_deep_copy::deep_copy;
use crate::optimizer::Optimizer;

/// Rewrites the windows of `plan` that only compute whole-partition aggregates (DuckDB's
/// `WindowSelfJoinOptimizer::Optimize`).
pub fn optimize_window_self_joins(optimizer: &Optimizer, plan: &mut LogicalOperator) -> Result<()> {
    let mut replacer = ColumnBindingReplacer::default();
    optimize_internal(optimizer, plan, &mut replacer)?;
    if !replacer.replacement_bindings.is_empty() {
        replacer.visit_operator(plan);
    }
    Ok(())
}

/// Whether the window function is an aggregate over its whole partition (DuckDB's
/// `CanOptimize(BoundWindowExpression, BoundWindowExpression)`).
fn can_optimize_window(window: &BoundWindowExpression, first: &BoundWindowExpression) -> bool {
    window.aggregate.is_some()
        && window.orders.is_empty()
        && !window.partitions.is_empty()
        && window.exclude_clause == WindowExcludeMode::NoOther
        && partitions_are_equivalent(window, first)
        && matches!(
            window.start,
            WindowBoundary::UnboundedPreceding
                | WindowBoundary::CurrentRowRange
                | WindowBoundary::CurrentRowGroups
        )
        && matches!(
            window.end,
            WindowBoundary::UnboundedFollowing
                | WindowBoundary::CurrentRowRange
                | WindowBoundary::CurrentRowGroups
        )
}

/// Whether two calls partition by the same expressions, in any order (DuckDB's
/// `BoundWindowExpression::PartitionsAreEquivalent`).
fn partitions_are_equivalent(lhs: &BoundWindowExpression, rhs: &BoundWindowExpression) -> bool {
    lhs.partitions.len() == rhs.partitions.len()
        && lhs
            .partitions
            .iter()
            .all(|partition| rhs.partitions.iter().any(|other| other.equals(partition)))
}

/// Whether the window's input can be copied and evaluated twice (DuckDB's
/// `CanOptimize(LogicalOperator)`): a chain of scans, projections, aggregates and filters.
fn can_optimize_input(op: &LogicalOperator) -> bool {
    match op.operator_type() {
        LogicalOperatorType::LogicalGet
        | LogicalOperatorType::LogicalExpressionGet
        | LogicalOperatorType::LogicalProjection
        | LogicalOperatorType::LogicalAggregateAndGroupBy
        | LogicalOperatorType::LogicalDummyScan
        | LogicalOperatorType::LogicalFilter => op.children.first().is_none_or(can_optimize_input),
        _ => false,
    }
}

/// The window function as a plain aggregate (DuckDB's `TranslateAggregate`).
fn translate_aggregate(
    window: &BoundWindowExpression,
    return_type: &rustdb_types::LogicalType,
) -> Option<Expression> {
    let function = window.aggregate.clone()?;
    let order_bys = (!window.arg_orders.is_empty()).then(|| BoundOrderModifier {
        orders: window.arg_orders.clone(),
    });
    Some(Expression::new(
        ExpressionType::BoundAggregate,
        return_type.clone(),
        ExpressionKind::Aggregate(Box::new(BoundAggregateExpression {
            function,
            children: window.children.clone(),
            bind_info: window.bind_info.clone(),
            aggr_type: if window.distinct {
                AggregateType::Distinct
            } else {
                AggregateType::NonDistinct
            },
            filter: window.filter_expr.clone(),
            order_bys,
        })),
    ))
}

/// The window calls of a window operator, if all are whole-partition aggregates.
fn optimizable_windows(op: &LogicalOperator) -> Option<Vec<&BoundWindowExpression>> {
    let windows: Vec<&BoundWindowExpression> = op
        .expressions
        .iter()
        .map(|expression| match &expression.kind {
            ExpressionKind::Window(window)
                if expression.expression_type == ExpressionType::WindowAggregate =>
            {
                Some(&**window)
            }
            _ => None,
        })
        .collect::<Option<_>>()?;
    let first = *windows.first()?;
    windows
        .iter()
        .all(|window| can_optimize_window(window, first))
        .then_some(windows)
}

/// DuckDB's `OptimizeInternal`.
fn optimize_internal(
    optimizer: &Optimizer,
    op: &mut LogicalOperator,
    replacer: &mut ColumnBindingReplacer,
) -> Result<()> {
    let LogicalKind::Window { window_index } = op.kind else {
        for child in &mut op.children {
            optimize_internal(optimizer, child, replacer)?;
        }
        return Ok(());
    };
    let Some(child) = op.children.first_mut() else {
        return Ok(());
    };
    optimize_internal(optimizer, child, replacer)?;
    if !can_optimize_input(child) {
        return Ok(());
    }
    let Some(windows) = optimizable_windows(op) else {
        return Ok(());
    };
    let partitions = windows[0].partitions.clone();
    let aggregates: Option<Vec<Expression>> = windows
        .iter()
        .zip(&op.expressions)
        .map(|(window, expression)| translate_aggregate(window, &expression.return_type))
        .collect();
    let Some(aggregates) = aggregates else {
        return Ok(());
    };
    let window_count = op.expressions.len();

    // The aggregate reads a copy of the input, whose bindings differ only in table index.
    let original = op.children.swap_remove(0);
    let copy = deep_copy(optimizer, &original);
    let local_replacer = ColumnBindingReplacer {
        replacement_bindings: original
            .column_bindings()
            .into_iter()
            .zip(copy.column_bindings())
            .map(|(old, new)| ReplacementBinding::new(old, new))
            .collect(),
    };
    let aggregate_index = optimizer.generate_table_index();
    let group_index = optimizer.generate_table_index();
    let mut aggregate = LogicalOperator::new(LogicalKind::Aggregate(Box::new(LogicalAggregate {
        group_index,
        aggregate_index,
        groupings_index: u64::MAX,
        groups: partitions.clone(),
        grouping_sets: Vec::new(),
        grouping_functions: Vec::new(),
        group_stats: Vec::new(),
        distinct_validity: rustdb_planner::TupleDataValidityType::default(),
    })))
    .with_expressions(aggregates)
    .with_children(vec![copy]);
    // The groups and aggregates read the copy (not its children).
    aggregate.for_each_expression_mut(|expression| local_replacer.visit_expression(expression));
    aggregate.resolve_operator_types()?;

    let mut join = LogicalComparisonJoin::new(JoinType::Inner, LogicalOperatorType::LogicalComparisonJoin);
    for (index, partition) in partitions.iter().enumerate() {
        join.conditions.push(JoinCondition {
            left: partition.clone(),
            right: Expression::column_ref(
                String::new(),
                partition.return_type.clone(),
                ColumnBinding::new(group_index, index as u64),
                0,
            ),
            comparison: ExpressionType::CompareNotDistinctFrom,
        });
    }
    let mut join = LogicalOperator::new(LogicalKind::ComparisonJoin(Box::new(join)))
        .with_children(vec![original, aggregate]);
    join.resolve_operator_types()?;
    *op = join;
    // The window's columns are now the aggregate's.
    for column in 0..window_count as u64 {
        replacer.replacement_bindings.push(ReplacementBinding::new(
            ColumnBinding::new(window_index, column),
            ColumnBinding::new(aggregate_index, column),
        ));
    }
    Ok(())
}
