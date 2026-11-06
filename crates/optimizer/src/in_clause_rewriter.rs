//! Rewriting `IN` lists in filters and projections (DuckDB's `InClauseRewriter`): a
//! single value becomes `=`, short or non-constant lists a chain of `OR`ed equalities,
//! and long constant lists a MARK join with the values.

use std::sync::Arc;

use rustdb_ast::{ExpressionType, JoinType};
use rustdb_common::{Result, STANDARD_VECTOR_SIZE};
use rustdb_planner::operator::{JoinCondition, LogicalComparisonJoin};
use rustdb_planner::{
    ColumnBinding, Expression, ExpressionKind, LogicalKind, LogicalOperator, LogicalOperatorType,
};
use rustdb_types::{DataChunk, LogicalType};

use crate::expression_util::try_evaluate_scalar;
use crate::optimizer::Optimizer;

/// Rewrites the IN lists of the filters and projections in `op` (DuckDB's
/// `InClauseRewriter::Rewrite`).
pub fn rewrite_in_clauses(optimizer: &Optimizer, op: &mut LogicalOperator) -> Result<()> {
    if matches!(
        op.operator_type(),
        LogicalOperatorType::LogicalProjection | LogicalOperatorType::LogicalFilter
    ) && !op.children.is_empty()
    {
        let mut root = std::mem::replace(
            &mut op.children[0],
            LogicalOperator::new(LogicalKind::CrossProduct),
        );
        let is_filter = op.operator_type() == LogicalOperatorType::LogicalFilter;
        let mut mark_indexes = Vec::new();
        let mut expressions = std::mem::take(&mut op.expressions);
        for expression in &mut expressions {
            rewrite_expression(optimizer, expression, &mut root, &mut mark_indexes)?;
        }
        op.expressions = expressions;
        if is_filter
            && !mark_indexes.is_empty()
            && let LogicalKind::Filter { projection_map } = &mut op.kind
            && projection_map.is_empty()
        {
            // Project the mark columns out again.
            *projection_map = root
                .column_bindings()
                .iter()
                .enumerate()
                .filter(|(_, binding)| !mark_indexes.contains(&binding.table_index))
                .map(|(index, _)| index as u64)
                .collect();
        }
        op.children[0] = root;
    }
    for child in &mut op.children {
        rewrite_in_clauses(optimizer, child)?;
    }
    Ok(())
}

/// Replaces the IN lists in `expression`, children first, stacking MARK joins on `root`
/// (DuckDB's `VisitReplace(BoundOperatorExpression)`).
fn rewrite_expression(
    optimizer: &Optimizer,
    expression: &mut Expression,
    root: &mut LogicalOperator,
    mark_indexes: &mut Vec<u64>,
) -> Result<()> {
    let is_in = matches!(
        expression.expression_type,
        ExpressionType::CompareIn | ExpressionType::CompareNotIn
    );
    if !is_in {
        let mut result = Ok(());
        expression.for_each_child_mut(|child| {
            if result.is_ok() {
                result = rewrite_expression(optimizer, child, root, mark_indexes);
            }
        });
        return result;
    }
    let is_regular_in = expression.expression_type == ExpressionType::CompareIn;
    let ExpressionKind::Operator(operator) = &mut expression.kind else {
        return Ok(());
    };
    if operator.children.len() < 2 {
        return Ok(());
    }
    let comparison = if is_regular_in {
        ExpressionType::CompareEqual
    } else {
        ExpressionType::CompareNotequal
    };
    if operator.children.len() == 2 {
        // `x IN (1)` is `x = 1`.
        let right = operator.children.pop();
        let left = operator.children.pop();
        if let (Some(left), Some(right)) = (left, right) {
            *expression = Expression::comparison(comparison, left, right);
        }
        return Ok(());
    }
    let all_scalar = operator.children[1..].iter().all(Expression::is_foldable);
    if operator.children.len() < 6 || !all_scalar {
        // A short or non-constant list: `x = 1 OR x = 2 ...`.
        let mut children = std::mem::take(&mut operator.children).into_iter();
        let Some(input) = children.next() else {
            return Ok(());
        };
        let comparisons = children
            .map(|value| Expression::comparison(comparison, input.clone(), value))
            .collect();
        let conjunction = if is_regular_in {
            ExpressionType::ConjunctionOr
        } else {
            ExpressionType::ConjunctionAnd
        };
        *expression = Expression::conjunction(conjunction, comparisons);
        return Ok(());
    }
    // A long constant list: a MARK join with the values.
    let in_type = operator.children[0].return_type.clone();
    let mut values = Vec::with_capacity(operator.children.len() - 1);
    for value in &operator.children[1..] {
        let Some(value) = try_evaluate_scalar(optimizer.context(), value) else {
            return Ok(());
        };
        values.push(value);
    }
    let mut chunks = Vec::new();
    for batch in values.chunks(STANDARD_VECTOR_SIZE) {
        let mut chunk = DataChunk::with_types(std::slice::from_ref(&in_type), batch.len());
        for (row, value) in batch.iter().enumerate() {
            let value = if value.logical_type() == &in_type {
                value.clone()
            } else {
                use rustdb_function::cast::ValueCast;
                value.try_cast(&in_type)?
            };
            chunk.column_mut(0).set_value(row, &value)?;
        }
        chunk.set_len(batch.len());
        chunks.push(chunk);
    }
    let chunk_index = optimizer.generate_table_index();
    let chunk_scan = LogicalOperator::new(LogicalKind::ChunkGet {
        table_index: chunk_index,
        chunk_types: vec![in_type.clone()],
        collection: Arc::from(chunks),
    });
    let mark_index = optimizer.generate_table_index();
    let mut join = LogicalComparisonJoin::new(JoinType::Mark, LogicalOperatorType::LogicalComparisonJoin);
    join.mark_index = mark_index;
    let input = operator.children.swap_remove(0);
    join.conditions.push(JoinCondition {
        left: input,
        right: Expression::column_ref(String::new(), in_type, ColumnBinding::new(chunk_index, 0), 0),
        comparison: ExpressionType::CompareEqual,
    });
    let left = std::mem::replace(root, LogicalOperator::new(LogicalKind::CrossProduct));
    *root = LogicalOperator::new(LogicalKind::ComparisonJoin(Box::new(join)))
        .with_children(vec![left, chunk_scan]);
    mark_indexes.push(mark_index);
    let mark = Expression::column_ref(
        "IN (...)",
        LogicalType::BOOLEAN,
        ColumnBinding::new(mark_index, 0),
        0,
    );
    *expression = if is_regular_in {
        mark
    } else {
        Expression::operator(ExpressionType::OperatorNot, LogicalType::BOOLEAN, vec![mark])
    };
    Ok(())
}
