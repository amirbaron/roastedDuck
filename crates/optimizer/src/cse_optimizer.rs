//! Computing repeated subexpressions of a projection or aggregate once (DuckDB's
//! `CommonSubExpressionOptimizer`): they move into a projection below the operator,
//! which then references their results.

use roastedduck_planner::{ColumnBinding, Expression, ExpressionKind, LogicalOperator, LogicalOperatorType};

use crate::optimizer::Optimizer;

/// A repeated subexpression (DuckDB's `CSENode`).
struct CseNode {
    expression: Expression,
    count: usize,
    /// Its column in the new projection, once pushed there.
    column_index: Option<u64>,
}

/// DuckDB's `CSEReplacementState`.
#[derive(Default)]
struct CseState {
    projection_index: u64,
    nodes: Vec<CseNode>,
    /// The projection column of each referenced binding.
    column_map: Vec<(ColumnBinding, u64)>,
    /// The expressions of the new projection.
    expressions: Vec<Expression>,
    /// Inside a short-circuiting expression past its first child.
    short_circuited: bool,
}

impl CseState {
    fn node_index(&self, expression: &Expression) -> Option<usize> {
        self.nodes
            .iter()
            .position(|node| node.expression.equals(expression))
    }
}

/// Extracts the common subexpressions of every projection and aggregate in `plan`.
pub fn extract_common_subexpressions(optimizer: &Optimizer, plan: &mut LogicalOperator) {
    if matches!(
        plan.operator_type(),
        LogicalOperatorType::LogicalProjection | LogicalOperatorType::LogicalAggregateAndGroupBy
    ) {
        extract(optimizer, plan);
    }
    for child in &mut plan.children {
        extract_common_subexpressions(optimizer, child);
    }
}

/// Counts the occurrences of the subexpressions with children (DuckDB's
/// `CountExpressions`).
fn count_expressions(expression: &Expression, state: &mut CseState) {
    if matches!(
        expression.kind,
        ExpressionKind::ColumnRef(_) | ExpressionKind::Constant(_) | ExpressionKind::Parameter(_)
    ) {
        return;
    }
    // Aggregates cannot move into a projection; only their arguments can.
    if !matches!(expression.kind, ExpressionKind::Aggregate(_)) && !expression.is_volatile() {
        match state.node_index(expression) {
            Some(index) => state.nodes[index].count += 1,
            None if !state.short_circuited => state.nodes.push(CseNode {
                expression: expression.clone(),
                count: 1,
                column_index: None,
            }),
            None => {}
        }
    }
    // Only the first argument of a short-circuiting expression always runs.
    if matches!(
        expression.kind,
        ExpressionKind::Conjunction(_) | ExpressionKind::Case(_)
    ) {
        let saved = state.short_circuited;
        for child in expression.children() {
            count_expressions(child, state);
            state.short_circuited = true;
        }
        state.short_circuited = saved;
    } else {
        for child in expression.children() {
            count_expressions(child, state);
        }
    }
}

/// Replaces repeated subexpressions and column references by references to the new
/// projection (DuckDB's `PerformCSEReplacement`).
fn perform_replacement(expression: &mut Expression, state: &mut CseState) {
    if let ExpressionKind::ColumnRef(column) = &mut expression.kind {
        let existing = state
            .column_map
            .iter()
            .find(|(binding, _)| *binding == column.binding)
            .map(|&(_, index)| index);
        let index = existing.unwrap_or_else(|| {
            let index = state.expressions.len() as u64;
            state.column_map.push((column.binding, index));
            state.expressions.push(Expression::column_ref(
                expression.alias.clone(),
                expression.return_type.clone(),
                column.binding,
                0,
            ));
            index
        });
        if let ExpressionKind::ColumnRef(column) = &mut expression.kind {
            column.binding = ColumnBinding::new(state.projection_index, index);
        }
        return;
    }
    if let Some(node_index) = state.node_index(expression)
        && state.nodes[node_index].count > 1
    {
        let column_index = if let Some(index) = state.nodes[node_index].column_index {
            index
        } else {
            let index = state.expressions.len() as u64;
            state.nodes[node_index].column_index = Some(index);
            state.expressions.push(expression.clone());
            index
        };
        *expression = Expression::column_ref(
            expression.alias.clone(),
            expression.return_type.clone(),
            ColumnBinding::new(state.projection_index, column_index),
            0,
        );
        return;
    }
    expression.for_each_child_mut(|child| perform_replacement(child, state));
}

/// DuckDB's `ExtractCommonSubExpresions`.
fn extract(optimizer: &Optimizer, op: &mut LogicalOperator) {
    if op.children.len() != 1 {
        return;
    }
    let mut state = CseState::default();
    op.for_each_expression_mut(|expression| count_expressions(expression, &mut state));
    if !state.nodes.iter().any(|node| node.count > 1) {
        return;
    }
    state.projection_index = optimizer.generate_table_index();
    op.for_each_expression_mut(|expression| perform_replacement(expression, &mut state));
    let input = op.children.remove(0);
    let estimate = input.estimated_cardinality;
    let mut projection = LogicalOperator::projection(
        state.projection_index,
        std::mem::take(&mut state.expressions),
        input,
    );
    projection.estimated_cardinality = estimate;
    op.children.push(projection);
}
