//! Pushing filters into materialized CTEs (DuckDB's `CTEFilterPusher`): when every
//! reference of a materialized CTE is filtered, the OR of those filters (rewritten to the
//! CTE definition's bindings) is pushed into the definition.

use roastedduck_ast::ExpressionType;
use roastedduck_common::Result;
use roastedduck_planner::{Expression, LogicalKind, LogicalOperator};

use crate::column_binding_replacer::{ColumnBindingReplacer, ReplacementBinding};
use crate::expression_rewriter::ExpressionRewriter;
use crate::filter_pushdown::FilterPushdown;
use crate::join_filter_pushdown::operator_at_path;
use crate::optimizer::Optimizer;

/// A materialized CTE and its references (DuckDB's `MaterializedCTEInfo`).
struct CteInfo {
    /// The CTE's table index.
    table_index: u64,
    /// The path of the `MATERIALIZED_CTE` operator.
    cte_path: Vec<usize>,
    /// The paths of the filters directly above its references.
    filter_paths: Vec<Vec<usize>>,
    all_cte_refs_are_filtered: bool,
}

/// Pushes filters into the materialized CTEs of `plan`, deepest first (DuckDB's
/// `CTEFilterPusher::Optimize`).
pub fn push_cte_filters(optimizer: &Optimizer, mut plan: LogicalOperator) -> Result<LogicalOperator> {
    let mut ctes = Vec::new();
    find_candidates(&plan, &mut Vec::new(), &mut ctes);
    for cte in ctes.iter().rev() {
        if !cte.all_cte_refs_are_filtered {
            continue;
        }
        // Earlier pushes move operators, so the paths are found again.
        let mut current = Vec::new();
        find_candidates(&plan, &mut Vec::new(), &mut current);
        if let Some(info) = current
            .into_iter()
            .find(|info| info.table_index == cte.table_index)
        {
            push_filter_into_cte(optimizer, &mut plan, &info)?;
        }
    }
    Ok(plan)
}

/// Collects the materialized CTEs in pre-order and their (filtered) references (DuckDB's
/// `FindCandidates`).
fn find_candidates(op: &LogicalOperator, path: &mut Vec<usize>, ctes: &mut Vec<CteInfo>) {
    match &op.kind {
        LogicalKind::MaterializedCte { table_index, .. } => {
            if !ctes.iter().any(|info| info.table_index == *table_index) {
                ctes.push(CteInfo {
                    table_index: *table_index,
                    cte_path: path.clone(),
                    filter_paths: Vec::new(),
                    all_cte_refs_are_filtered: true,
                });
            }
        }
        LogicalKind::Filter { .. } => {
            if let Some(LogicalKind::CteRef { cte_index, .. }) = op.children.first().map(|child| &child.kind)
            {
                if let Some(info) = ctes.iter_mut().find(|info| info.table_index == *cte_index) {
                    info.filter_paths.push(path.clone());
                }
                return;
            }
        }
        LogicalKind::CteRef { cte_index, .. } => {
            if let Some(info) = ctes.iter_mut().find(|info| info.table_index == *cte_index) {
                info.all_cte_refs_are_filtered = false;
            }
            return;
        }
        _ => {}
    }
    for (index, child) in op.children.iter().enumerate() {
        path.push(index);
        find_candidates(child, path, ctes);
        path.pop();
    }
}

/// The operator at `path` below `op`.
fn operator_at<'a>(op: &'a LogicalOperator, path: &[usize]) -> Option<&'a LogicalOperator> {
    path.iter().try_fold(op, |node, &index| node.children.get(index))
}

/// Combines `left` and `right` with a binary conjunction (DuckDB's two-child
/// `BoundConjunctionExpression`).
fn combine(expression_type: ExpressionType, left: Option<Expression>, right: Expression) -> Expression {
    match left {
        Some(left) => Expression::conjunction(expression_type, vec![left, right]),
        None => right,
    }
}

/// DuckDB's `PushFilterIntoCTE`.
fn push_filter_into_cte(optimizer: &Optimizer, plan: &mut LogicalOperator, info: &CteInfo) -> Result<()> {
    if info.filter_paths.is_empty() {
        return Ok(());
    }
    let Some(definition) = operator_at(plan, &info.cte_path).and_then(|cte| cte.children.first()) else {
        return Ok(());
    };
    let new_bindings = definition.column_bindings();
    // The OR over the references of the AND of each reference's filters.
    let mut outer = None;
    for filter_path in &info.filter_paths {
        let Some(filter) = operator_at(plan, filter_path) else {
            continue;
        };
        let Some(cte_ref) = filter.children.first() else {
            continue;
        };
        let replacer = ColumnBindingReplacer {
            replacement_bindings: cte_ref
                .column_bindings()
                .into_iter()
                .zip(new_bindings.iter().copied())
                .map(|(old, new)| ReplacementBinding::new(old, new))
                .collect(),
        };
        let mut inner = None;
        for expression in &filter.expressions {
            let mut copy = expression.clone();
            replacer.visit_expression(&mut copy);
            inner = Some(combine(ExpressionType::ConjunctionAnd, inner, copy));
        }
        if let Some(inner) = inner {
            outer = Some(combine(ExpressionType::ConjunctionOr, outer, inner));
        }
    }
    let Some(outer) = outer else {
        return Ok(());
    };
    let Some(cte) = operator_at_path(plan, &info.cte_path) else {
        return Ok(());
    };
    if cte.children.is_empty() {
        return Ok(());
    }
    // The filter's expressions are rewritten before it gets its (already rewritten) child.
    let mut new_cte = LogicalOperator::new(LogicalKind::Filter {
        projection_map: Vec::new(),
    })
    .with_expressions(vec![outer]);
    roastedduck_planner::operator::split_predicates(&mut new_cte.expressions);
    ExpressionRewriter::new(optimizer).visit_operator(&mut new_cte)?;
    new_cte.children.push(cte.children.remove(0));
    let new_cte = FilterPushdown::new(optimizer, true).rewrite(new_cte)?;
    cte.children.insert(0, new_cte);
    Ok(())
}
