//! Planning joins (DuckDB's `plan_joinref.cpp`, `LogicalComparisonJoin::CreateJoin` and
//! `JoinSide`): conditions are split into comparisons between the two sides, filters on
//! one side (pushed below the join where that is safe), and arbitrary predicates.

use std::collections::HashSet;

use roastedduck_ast::{JoinRefType, JoinType};
use roastedduck_common::{Error, Result};
use roastedduck_types::Value;

use crate::binder::query_node::{plan_lateral_join, plan_subqueries, recursive_dependent_join_planner};
use crate::binder::{BindScope, BoundStatement, CorrelatedColumns};
use crate::context::ClientContext;
use crate::expression::{Expression, ExpressionKind};
use crate::expression_binder::reduce_expression_depth;
use crate::operator::{JoinCondition, LogicalComparisonJoin, LogicalKind, LogicalOperator, split_predicates};
use crate::{ExpressionType, LogicalOperatorType};

/// A bound join (DuckDB's `BoundJoinRef`).
#[derive(Debug)]
pub struct BoundJoinRef {
    pub left: BoundStatement,
    pub right: BoundStatement,
    pub join_type: JoinType,
    pub ref_type: JoinRefType,
    pub condition: Option<Expression>,
    /// Whether the right side references the left side.
    pub lateral: bool,
    /// The correlated columns of the right side.
    pub correlated_columns: CorrelatedColumns,
    /// The table index of a MARK join's boolean column.
    pub mark_index: u64,
}

/// Which inputs of a join an expression references (DuckDB's `JoinSide`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinSide {
    None,
    Left,
    Right,
    Both,
}

impl JoinSide {
    /// DuckDB's `JoinSide::CombineJoinSide`.
    #[must_use]
    pub fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::None, side) | (side, Self::None) => side,
            (left, right) if left != right => Self::Both,
            (side, _) => side,
        }
    }

    /// The side a table index belongs to.
    pub fn of_table(table_index: u64, left: &HashSet<u64>) -> Self {
        if left.contains(&table_index) {
            Self::Left
        } else {
            Self::Right
        }
    }

    /// The sides `expression` references (DuckDB's `JoinSide::GetJoinSide`); any table
    /// not in `left` is on the right.
    pub fn of_expression(expression: &Expression, left: &HashSet<u64>) -> Result<Self> {
        if let Some(column) = expression.as_column_ref() {
            if column.depth > 0 {
                return Err(Error::not_implemented(
                    "Non-inner join on correlated columns not supported",
                ));
            }
            return Ok(Self::of_table(column.binding.table_index, left));
        }
        if let ExpressionKind::Subquery(subquery) = &expression.kind {
            let mut side = Self::None;
            for child in &subquery.children {
                side = side.combine(Self::of_expression(child, left)?);
            }
            for column in &subquery.binder.correlated_columns {
                if column.depth > 1 {
                    // It does not refer to any table of this join.
                    return Ok(Self::Both);
                }
                side = side.combine(Self::of_table(column.binding.table_index, left));
            }
            return Ok(side);
        }
        let mut side = Self::None;
        for child in expression.children() {
            side = Self::of_expression(child, left)?.combine(side);
        }
        Ok(side)
    }
}

/// The table indexes of the columns `op` produces (DuckDB's `LogicalJoin::GetTableReferences`).
pub fn table_references(op: &LogicalOperator) -> HashSet<u64> {
    op.column_bindings()
        .iter()
        .map(|binding| binding.table_index)
        .collect()
}

/// The comparison with its sides swapped (DuckDB's `FlipComparisonExpression`).
pub fn flip_comparison(expression_type: ExpressionType) -> Result<ExpressionType> {
    use ExpressionType as E;
    Ok(match expression_type {
        E::CompareNotDistinctFrom | E::CompareDistinctFrom | E::CompareNotequal | E::CompareEqual => {
            expression_type
        }
        E::CompareLessthan => E::CompareGreaterthan,
        E::CompareGreaterthan => E::CompareLessthan,
        E::CompareLessthanorequalto => E::CompareGreaterthanorequalto,
        E::CompareGreaterthanorequalto => E::CompareLessthanorequalto,
        _ => return Err(Error::internal("Unsupported comparison type in flip")),
    })
}

/// Whether a condition on the left side alone can become a filter below the join.
const fn can_push_to_left_child(join_type: JoinType) -> bool {
    matches!(join_type, JoinType::Inner | JoinType::Semi | JoinType::Right)
}

/// Whether a condition on the right side alone can become a filter below the join.
const fn can_push_to_right_child(join_type: JoinType) -> bool {
    matches!(
        join_type,
        JoinType::Inner | JoinType::Semi | JoinType::Anti | JoinType::Left
    )
}

/// Adds `expression` to the filter on top of `child`, creating one if needed.
fn push_filter_to_child(child: &mut LogicalOperator, expression: Expression) {
    if !matches!(child.kind, LogicalKind::Filter { .. }) {
        let input = std::mem::replace(
            child,
            LogicalOperator::new(LogicalKind::Filter {
                projection_map: Vec::new(),
            }),
        );
        child.children.push(input);
    }
    child.expressions.push(expression);
}

/// Whether a constant condition is TRUE and can be dropped (DuckDB's `CanEliminate`).
fn can_eliminate(context: &ClientContext, join_type: JoinType, expression: &Expression) -> bool {
    if !expression.is_foldable() {
        return false;
    }
    let Ok(value) = context.evaluator.evaluate_scalar(expression) else {
        return false;
    };
    if value.is_null() || value != Value::boolean(true) {
        return false;
    }
    matches!(
        join_type,
        JoinType::Inner
            | JoinType::Left
            | JoinType::Right
            | JoinType::Semi
            | JoinType::Anti
            | JoinType::Outer
    )
}

/// Whether a comparison may be a condition of this kind of join (ASOF joins only take
/// (in)equalities).
const fn is_join_type_condition(ref_type: JoinRefType, expression_type: ExpressionType) -> bool {
    use ExpressionType as E;
    match ref_type {
        JoinRefType::Asof => matches!(
            expression_type,
            E::CompareEqual
                | E::CompareNotDistinctFrom
                | E::CompareGreaterthanorequalto
                | E::CompareGreaterthan
                | E::CompareLessthanorequalto
                | E::CompareLessthan
        ),
        _ => true,
    }
}

const fn is_comparison(expression_type: ExpressionType) -> bool {
    use ExpressionType as E;
    matches!(
        expression_type,
        E::CompareEqual
            | E::CompareNotequal
            | E::CompareLessthan
            | E::CompareGreaterthan
            | E::CompareLessthanorequalto
            | E::CompareGreaterthanorequalto
            | E::CompareNotDistinctFrom
            | E::CompareDistinctFrom
    )
}

/// A comparison whose sides each reference one input becomes a join condition (DuckDB's
/// `CreateJoinCondition`); otherwise the expression is given back.
fn create_join_condition(
    expression: Expression,
    left: &HashSet<u64>,
    conditions: &mut Vec<JoinCondition>,
) -> Result<Option<Expression>> {
    let expression_type = expression.expression_type;
    let ExpressionKind::Comparison(comparison) = &expression.kind else {
        return Ok(Some(expression));
    };
    let left_side = JoinSide::of_expression(&comparison.left, left)?;
    let right_side = JoinSide::of_expression(&comparison.right, left)?;
    if left_side == JoinSide::Both || right_side == JoinSide::Both {
        return Ok(Some(expression));
    }
    let ExpressionKind::Comparison(comparison) = expression.kind else {
        return Err(Error::internal("expected a comparison"));
    };
    let (mut first, mut second, mut comparison_type) = (comparison.left, comparison.right, expression_type);
    if left_side == JoinSide::Right {
        std::mem::swap(&mut first, &mut second);
        comparison_type = flip_comparison(expression_type)?;
    }
    conditions.push(JoinCondition {
        left: first,
        right: second,
        comparison: comparison_type,
    });
    Ok(None)
}

/// Splits join predicates into conditions, filters pushed to a child, and arbitrary
/// expressions (DuckDB's `LogicalComparisonJoin::ExtractJoinConditions`).
#[allow(clippy::too_many_arguments)]
pub fn extract_join_conditions(
    context: &ClientContext,
    join_type: JoinType,
    ref_type: JoinRefType,
    left_child: &mut LogicalOperator,
    right_child: &mut LogicalOperator,
    expressions: Vec<Expression>,
    conditions: &mut Vec<JoinCondition>,
    arbitrary_expressions: &mut Vec<Expression>,
) -> Result<()> {
    let left = table_references(left_child);
    for expression in expressions {
        let side = JoinSide::of_expression(&expression, &left)?;
        let remaining = match side {
            JoinSide::None if can_eliminate(context, join_type, &expression) => None,
            JoinSide::Left if can_push_to_left_child(join_type) => {
                push_filter_to_child(left_child, expression);
                None
            }
            JoinSide::Right if can_push_to_right_child(join_type) => {
                push_filter_to_child(right_child, expression);
                None
            }
            JoinSide::Both
                if is_comparison(expression.expression_type)
                    && is_join_type_condition(ref_type, expression.expression_type) =>
            {
                create_join_condition(expression, &left, conditions)?
            }
            _ => Some(expression),
        };
        arbitrary_expressions.extend(remaining);
    }
    Ok(())
}

/// `ExtractJoinConditions` for a single condition, split at its ANDs.
#[allow(clippy::too_many_arguments)]
pub fn extract_join_conditions_from(
    context: &ClientContext,
    join_type: JoinType,
    ref_type: JoinRefType,
    left_child: &mut LogicalOperator,
    right_child: &mut LogicalOperator,
    condition: Expression,
    conditions: &mut Vec<JoinCondition>,
    arbitrary_expressions: &mut Vec<Expression>,
) -> Result<()> {
    let mut expressions = vec![condition];
    split_predicates(&mut expressions);
    extract_join_conditions(
        context,
        join_type,
        ref_type,
        left_child,
        right_child,
        expressions,
        conditions,
        arbitrary_expressions,
    )
}

/// `a AND b AND ...` of `expressions` (left-deep, as DuckDB builds it).
fn conjunction(mut expressions: Vec<Expression>) -> Option<Expression> {
    if expressions.is_empty() {
        return None;
    }
    let first = expressions.remove(0);
    Some(expressions.into_iter().fold(first, |left, right| {
        Expression::conjunction(ExpressionType::ConjunctionAnd, vec![left, right])
    }))
}

/// The join operator for the extracted conditions (DuckDB's
/// `LogicalComparisonJoin::CreateJoin`).
pub fn create_join(
    join_type: JoinType,
    ref_type: JoinRefType,
    left_child: LogicalOperator,
    right_child: LogicalOperator,
    conditions: Vec<JoinCondition>,
    mut arbitrary_expressions: Vec<Expression>,
) -> Result<LogicalOperator> {
    let mut is_asof = ref_type == JoinRefType::Asof;
    if is_asof {
        match join_type {
            JoinType::Right | JoinType::Outer if !arbitrary_expressions.is_empty() => {
                return Err(Error::not_implemented(format!(
                    "Unsupported ASOF JOIN type ({}) with arbitrary predicate",
                    join_type.name()
                )));
            }
            JoinType::Semi | JoinType::Anti => {
                // The RHS match does not matter: a regular join, but the ASOF checks apply.
                is_asof = false;
            }
            _ => {}
        }
        let mut asof_index = None;
        for condition in &conditions {
            use ExpressionType as E;
            match condition.comparison {
                E::CompareEqual | E::CompareNotDistinctFrom => {}
                E::CompareGreaterthanorequalto
                | E::CompareGreaterthan
                | E::CompareLessthanorequalto
                | E::CompareLessthan => {
                    if asof_index.is_some() {
                        return Err(Error::binder("Multiple ASOF JOIN inequalities"));
                    }
                    asof_index = Some(());
                }
                _ => return Err(Error::binder("Invalid ASOF JOIN comparison")),
            }
        }
        if asof_index.is_none() {
            return Err(Error::binder("Missing ASOF JOIN inequality"));
        }
    }
    if is_asof {
        let mut join = LogicalComparisonJoin::new(join_type, LogicalOperatorType::LogicalAsofJoin);
        join.conditions = conditions;
        join.predicate = conjunction(arbitrary_expressions);
        return Ok(LogicalOperator::new(LogicalKind::ComparisonJoin(Box::new(join)))
            .with_children(vec![left_child, right_child]));
    }
    if conditions.is_empty() {
        if arbitrary_expressions.is_empty() {
            arbitrary_expressions.push(Expression::constant(Value::boolean(true)));
        }
        let condition =
            conjunction(arbitrary_expressions).ok_or_else(|| Error::internal("empty join condition"))?;
        return Ok(LogicalOperator::new(LogicalKind::AnyJoin {
            join_type,
            condition: Box::new(condition),
        })
        .with_children(vec![left_child, right_child]));
    }
    if !arbitrary_expressions.is_empty() {
        if join_type == JoinType::Inner {
            // A comparison join with a filter on top.
            let mut join = LogicalComparisonJoin::new(join_type, LogicalOperatorType::LogicalComparisonJoin);
            join.conditions = conditions;
            let join = LogicalOperator::new(LogicalKind::ComparisonJoin(Box::new(join)))
                .with_children(vec![left_child, right_child]);
            return Ok(LogicalOperator::new(LogicalKind::Filter {
                projection_map: Vec::new(),
            })
            .with_expressions(arbitrary_expressions)
            .with_children(vec![join]));
        }
        let mut predicates: Vec<Expression> = conditions
            .into_iter()
            .map(JoinCondition::into_expression)
            .collect();
        predicates.extend(arbitrary_expressions);
        let condition = conjunction(predicates).ok_or_else(|| Error::internal("empty join condition"))?;
        return Ok(LogicalOperator::new(LogicalKind::AnyJoin {
            join_type,
            condition: Box::new(condition),
        })
        .with_children(vec![left_child, right_child]));
    }
    let mut join = LogicalComparisonJoin::new(join_type, LogicalOperatorType::LogicalComparisonJoin);
    join.conditions = conditions;
    Ok(LogicalOperator::new(LogicalKind::ComparisonJoin(Box::new(join)))
        .with_children(vec![left_child, right_child]))
}

/// DuckDB's `LogicalComparisonJoin::CreateJoin(context, type, reftype, left, right, condition)`.
pub fn create_join_from_condition(
    context: &ClientContext,
    join_type: JoinType,
    ref_type: JoinRefType,
    mut left_child: LogicalOperator,
    mut right_child: LogicalOperator,
    condition: Expression,
) -> Result<LogicalOperator> {
    let mut conditions = Vec::new();
    let mut arbitrary_expressions = Vec::new();
    extract_join_conditions_from(
        context,
        join_type,
        ref_type,
        &mut left_child,
        &mut right_child,
        condition,
        &mut conditions,
        &mut arbitrary_expressions,
    )?;
    create_join(
        join_type,
        ref_type,
        left_child,
        right_child,
        conditions,
        arbitrary_expressions,
    )
}

/// Whether `expression` references columns of enclosing queries.
fn has_correlated_columns(expression: &Expression) -> bool {
    expression.as_column_ref().is_some_and(|column| column.depth > 0)
        || expression.children().into_iter().any(has_correlated_columns)
}

/// Plans subqueries in the filters directly below a join and in its conditions.
fn plan_join_subqueries(scope: &mut BindScope<'_>, join: &mut LogicalOperator) -> Result<()> {
    for child in &mut join.children {
        if matches!(child.kind, LogicalKind::Filter { .. }) {
            let LogicalOperator {
                expressions,
                children,
                ..
            } = child;
            if let Some(input) = children.first_mut() {
                for expression in expressions {
                    plan_subqueries(scope, expression, input)?;
                }
            }
        }
    }
    let LogicalOperator { kind, children, .. } = join;
    match kind {
        LogicalKind::ComparisonJoin(comparison_join) => {
            let [left, right] = children.as_mut_slice() else {
                return Err(Error::internal("join without two inputs"));
            };
            for condition in &mut comparison_join.conditions {
                plan_subqueries(scope, &mut condition.left, left)?;
                plan_subqueries(scope, &mut condition.right, right)?;
            }
        }
        LogicalKind::AnyJoin { condition, .. } if condition.has_subquery() => {
            return Err(Error::not_implemented(
                "Cannot perform non-inner join on subquery!",
            ));
        }
        _ => {}
    }
    Ok(())
}

/// The plan of a bound join (DuckDB's `Binder::CreatePlan(BoundJoinRef &)`).
pub fn create_join_plan(scope: &mut BindScope<'_>, bound: BoundJoinRef) -> Result<LogicalOperator> {
    let BoundJoinRef {
        left,
        right,
        mut join_type,
        ref_type,
        condition,
        lateral,
        mut correlated_columns,
        mark_index,
    } = bound;
    let mut left = left.plan;
    let mut right = right.plan;
    // The right side was bound one level deeper (below the lateral binder); without
    // correlation to the left side its correlated columns are one level too deep.
    if !lateral && !correlated_columns.is_empty() {
        reduce_expression_depth(&mut right, &correlated_columns);
    }
    if join_type == JoinType::Right && ref_type != JoinRefType::Asof && scope.context.config.flip_right_joins
    {
        // Right outer joins become left outer joins with the sides swapped (for the
        // build/probe side optimizer).
        join_type = JoinType::Left;
        std::mem::swap(&mut left, &mut right);
    }
    if lateral {
        let mut plan = plan_lateral_join(scope, left, right, &mut correlated_columns, join_type, condition)?;
        if scope.binder.has_unplanned_dependent_joins {
            recursive_dependent_join_planner(scope, &mut plan)?;
        }
        return Ok(plan);
    }
    match ref_type {
        JoinRefType::Cross => return Ok(LogicalOperator::cross_product(left, right)),
        JoinRefType::Positional => {
            return Ok(LogicalOperator::new(LogicalKind::PositionalJoin).with_children(vec![left, right]));
        }
        _ => {}
    }
    let condition = condition.ok_or_else(|| Error::internal("join without a condition"))?;
    if join_type == JoinType::Inner
        && (condition.has_subquery() || has_correlated_columns(&condition))
        && ref_type == JoinRefType::Regular
    {
        // A cross product with a filter; the optimizer turns it into a join.
        let mut root = LogicalOperator::cross_product(left, right);
        let mut condition = condition;
        plan_subqueries(scope, &mut condition, &mut root)?;
        return Ok(LogicalOperator::filter(condition, root));
    }
    let context = scope.binder.context.clone();
    let mut result = create_join_from_condition(&context, join_type, ref_type, left, right, condition)?;
    let join = if matches!(result.kind, LogicalKind::Filter { .. }) {
        result
            .children
            .first_mut()
            .ok_or_else(|| Error::internal("filter without input"))?
    } else {
        &mut result
    };
    if join_type == JoinType::Mark
        && let LogicalKind::ComparisonJoin(comparison_join) = &mut join.kind
    {
        comparison_join.mark_index = mark_index;
    }
    plan_join_subqueries(scope, join)?;
    Ok(result)
}
