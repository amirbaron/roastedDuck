//! Replacing references to materialized CTEs by the CTE's definition (DuckDB's
//! `CTEInlining`): unreferenced CTEs are dropped, CTEs used once are moved into their
//! reader, and CTEs used a few times are copied where materializing them would be
//! wasteful.

use rustdb_ast::CteMaterialize;
use rustdb_function::Stability;
use rustdb_planner::{Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType};

use crate::logical_operator_deep_copy::deep_copy;
use crate::optimizer::Optimizer;

/// Inlines the CTEs of `op` where possible, the deepest first (DuckDB's `TryInlining`).
pub fn inline_ctes(optimizer: &Optimizer, mut op: LogicalOperator) -> LogicalOperator {
    op.children = std::mem::take(&mut op.children)
        .into_iter()
        .map(|child| inline_ctes(optimizer, child))
        .collect();
    let LogicalKind::MaterializedCte {
        table_index,
        materialize,
        ..
    } = op.kind
    else {
        return op;
    };
    if op.children.len() != 2 {
        return op;
    }
    let reference_count = count_cte_references(&op, table_index);
    if reference_count == 0 {
        // Unreferenced: keep only the query.
        return op.children.pop().unwrap_or(op);
    }
    if materialize == CteMaterialize::CteMaterializeAlways {
        return op;
    }
    let requires_copy = reference_count > 1;
    if requires_copy && materialize != CteMaterialize::CteMaterializeNever {
        if prevents_inlining(&mut op.children[0]) {
            return op;
        }
        let definition = &op.children[0];
        if ends_in_aggregate_or_distinct(definition) {
            return op;
        }
        let base_tables = count_base_table_references(definition);
        if base_tables > 2 && base_tables * reference_count > 10 {
            return op;
        }
        // The materialization cannot stop early for a LIMIT above the readers.
        if !contains_limit(&op.children[1])
            && definition.operator_type() != LogicalOperatorType::LogicalEmptyResult
        {
            return op;
        }
    }
    let Some(definition) = op.children.first_mut() else {
        return op;
    };
    if definition.resolve_operator_types().is_err() {
        return op;
    }
    let mut query = op
        .children
        .pop()
        .unwrap_or_else(|| LogicalOperator::new(LogicalKind::CrossProduct));
    let mut definition = op.children.pop();
    inline(optimizer, &mut query, table_index, &mut definition, requires_copy);
    query
}

/// Replaces the readers of CTE `cte_index` below `op` by the definition, moved into the
/// only reader or copied into each (DuckDB's `Inline`).
fn inline(
    optimizer: &Optimizer,
    op: &mut LogicalOperator,
    cte_index: u64,
    definition: &mut Option<LogicalOperator>,
    requires_copy: bool,
) {
    if let LogicalKind::CteRef {
        table_index,
        cte_index: reference,
        ..
    } = op.kind
    {
        if reference != cte_index {
            return;
        }
        let input = if requires_copy {
            definition
                .as_ref()
                .map(|definition| deep_copy(optimizer, definition))
        } else {
            definition.take()
        };
        let Some(mut input) = input else {
            return;
        };
        if input.resolve_operator_types().is_err() {
            return;
        }
        let expressions = input
            .column_bindings()
            .into_iter()
            .zip(input.types.iter())
            .map(|(binding, ty)| Expression::column_ref(String::new(), ty.clone(), binding, 0))
            .collect();
        let estimate = op.estimated_cardinality;
        *op = LogicalOperator::projection(table_index, expressions, input);
        op.estimated_cardinality = estimate;
        return;
    }
    for child in &mut op.children {
        inline(optimizer, child, cte_index, definition, requires_copy);
    }
}

/// The readers of CTE `cte_index` in `op` (DuckDB's `CountCTEReferences`).
fn count_cte_references(op: &LogicalOperator, cte_index: u64) -> usize {
    if let LogicalKind::CteRef {
        cte_index: reference, ..
    } = op.kind
        && reference == cte_index
    {
        return 1;
    }
    op.children
        .iter()
        .map(|child| count_cte_references(child, cte_index))
        .sum()
}

/// The scans in `op` (DuckDB's `CountBaseTableReferences`).
fn count_base_table_references(op: &LogicalOperator) -> usize {
    usize::from(op.operator_type() == LogicalOperatorType::LogicalGet)
        + op.children.iter().map(count_base_table_references).sum::<usize>()
}

/// Whether a LIMIT or Top-N is on the single-child path below `op` (DuckDB's
/// `ContainsLimit`).
fn contains_limit(op: &LogicalOperator) -> bool {
    if matches!(
        op.operator_type(),
        LogicalOperatorType::LogicalLimit | LogicalOperatorType::LogicalTopN
    ) {
        return true;
    }
    op.children.len() == 1 && op.children.iter().any(contains_limit)
}

/// Whether the definition ends in an aggregate, DISTINCT or window along its
/// single-child path (DuckDB's `EndsInAggregateOrDistinct`).
fn ends_in_aggregate_or_distinct(op: &LogicalOperator) -> bool {
    if matches!(
        op.operator_type(),
        LogicalOperatorType::LogicalAggregateAndGroupBy
            | LogicalOperatorType::LogicalDistinct
            | LogicalOperatorType::LogicalWindow
    ) {
        return true;
    }
    op.children.len() == 1 && op.children.iter().any(ends_in_aggregate_or_distinct)
}

/// Whether the definition calls a volatile function other than `error`, which must not
/// run once per reader (DuckDB's `PreventInlining`).
fn prevents_inlining(op: &mut LogicalOperator) -> bool {
    let mut found = false;
    op.for_each_expression_mut(|expression| found |= has_volatile_function(expression));
    found || op.children.iter_mut().any(prevents_inlining)
}

fn has_volatile_function(expression: &Expression) -> bool {
    if let ExpressionKind::Function(function) = &expression.kind {
        if &*function.function.name == "error" {
            return false;
        }
        if function.function.stability == Stability::Volatile || expression.is_volatile() {
            return true;
        }
    }
    expression.children().into_iter().any(has_volatile_function)
}
