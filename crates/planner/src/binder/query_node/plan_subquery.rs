//! Planning subqueries used as expressions and LATERAL joins (DuckDB's
//! `plan_subquery.cpp`). Uncorrelated subqueries become cross products or MARK joins;
//! correlated ones become dependent joins, which the planner flattens afterwards
//! ([`crate::subquery::decorrelate_independent`]).

use roastedduck_ast::{JoinRefType, JoinType, SubqueryType};
use roastedduck_catalog::{CatalogType, EntryKind};
use roastedduck_common::{Error, Result};
use roastedduck_function::cast::ValueCast;
use roastedduck_types::{LogicalType, PhysicalType, Value};

use crate::binder::expression::push_collation;
use crate::binder::tableref::extract_join_conditions_from;
use crate::binder::{BindScope, CorrelatedColumnInfo, CorrelatedColumns};
use crate::context::ClientContext;
use crate::expression::{
    AggregateType, BoundCaseCheck, BoundCaseExpression, BoundSubqueryExpression, Expression, ExpressionKind,
};
use crate::function_binder::{FunctionBinder, add_cast_to_type};
use crate::operator::{
    BoundLimitNode, JoinCondition, LogicalAggregate, LogicalComparisonJoin, LogicalDependentJoin,
    LogicalKind, LogicalOperator,
};
use crate::{ColumnBinding, ExpressionType, LogicalOperatorType};

/// A built-in aggregate the planner adds itself (DuckDB uses e.g. `CountStarFun` and
/// `FirstFunctionGetter` directly), bound to `children`.
pub(crate) fn builtin_aggregate(
    context: &ClientContext,
    name: &str,
    children: Vec<Expression>,
) -> Result<Expression> {
    let entry = context
        .get_entry("system", "main", CatalogType::AggregateFunction, name, true)?
        .ok_or_else(|| Error::not_implemented(format!("aggregate function {name}")))?;
    let EntryKind::AggregateFunction(functions) = &entry.entry.kind else {
        return Err(Error::internal(format!("{name} is not an aggregate function")));
    };
    let binder = FunctionBinder::new(context);
    let types = FunctionBinder::argument_types(&children);
    let mut function = binder.bind_aggregate_overload(functions, &types)??;
    // The function object itself, not its catalog entry's.
    function.clear_catalog_entry();
    binder.bind_aggregate_function(function, children, None, AggregateType::NonDistinct, None)
}

/// A built-in scalar function the planner adds itself (`error`, `row`).
fn builtin_scalar(context: &ClientContext, name: &str, children: Vec<Expression>) -> Result<Expression> {
    let entry = context
        .get_entry("system", "main", CatalogType::ScalarFunction, name, true)?
        .ok_or_else(|| Error::not_implemented(format!("scalar function {name}")))?;
    let EntryKind::ScalarFunction(functions) = &entry.entry.kind else {
        return Err(Error::internal(format!("{name} is not a scalar function")));
    };
    // The function objects themselves, not their catalog entry's.
    let mut functions = functions.clone();
    for function in &mut functions.functions {
        function.clear_catalog_entry();
    }
    FunctionBinder::new(context)
        .bind_scalar_function(&functions, children, false)?
        .map_err(|error| error.error)
}

/// A projection's or aggregate's single row as a new operator on top of `child`.
fn aggregate(
    scope: &BindScope<'_>,
    aggregate_index: u64,
    expressions: Vec<Expression>,
    child: LogicalOperator,
) -> LogicalOperator {
    LogicalOperator::new(LogicalKind::Aggregate(Box::new(LogicalAggregate {
        group_index: scope.binder.generate_table_index(),
        aggregate_index,
        groupings_index: u64::MAX,
        groups: Vec::new(),
        grouping_sets: Vec::new(),
        grouping_functions: Vec::new(),
        group_stats: Vec::new(),
        distinct_validity: crate::TupleDataValidityType::default(),
    })))
    .with_expressions(expressions)
    .with_children(vec![child])
}

/// Replaces `root` by `f(root)`.
fn replace_root(
    root: &mut LogicalOperator,
    f: impl FnOnce(LogicalOperator) -> Result<LogicalOperator>,
) -> Result<()> {
    let placeholder = LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 });
    let previous = std::mem::replace(root, placeholder);
    *root = f(previous)?;
    Ok(())
}

/// DuckDB's `PlanUncorrelatedSubquery`.
fn plan_uncorrelated_subquery(
    scope: &mut BindScope<'_>,
    subquery: &mut BoundSubqueryExpression,
    name: roastedduck_common::ByteString,
    return_type: LogicalType,
    root: &mut LogicalOperator,
    plan: LogicalOperator,
) -> Result<Expression> {
    let context = scope.binder.context.clone();
    match subquery.subquery_type {
        SubqueryType::Exists => {
            // Only existence matters: LIMIT 1, then COUNT(*) = 1.
            let limit = LogicalOperator::new(LogicalKind::Limit {
                limit: BoundLimitNode::ConstantValue(1),
                offset: BoundLimitNode::Unset,
            })
            .with_children(vec![plan]);
            let count_star = builtin_aggregate(&context, "count_star", Vec::new())?;
            let count_type = count_star.return_type.clone();
            let aggregate_index = scope.binder.generate_table_index();
            let plan = aggregate(scope, aggregate_index, vec![count_star], limit);
            let comparison = Expression::comparison(
                ExpressionType::CompareEqual,
                Expression::column_ref("", count_type.clone(), ColumnBinding::new(aggregate_index, 0), 0),
                Expression::constant(Value::integer(1).default_cast_as(&count_type)?),
            );
            let projection_index = scope.binder.generate_table_index();
            let plan = LogicalOperator::projection(projection_index, vec![comparison], plan);
            replace_root(root, |root| Ok(LogicalOperator::cross_product(root, plan)))?;
            Ok(Expression::column_ref(
                name,
                LogicalType::BOOLEAN,
                ColumnBinding::new(projection_index, 0),
                0,
            ))
        }
        SubqueryType::Scalar => {
            // The first row of the subquery (and an error if there are several).
            let bindings = plan.column_bindings();
            let table_index = bindings
                .first()
                .map(|binding| binding.table_index)
                .ok_or_else(|| Error::internal("scalar subquery without columns"))?;
            let error_on_multiple_rows = context.config.scalar_subquery_error_on_multiple_rows;
            let first_child =
                Expression::column_ref("", return_type.clone(), ColumnBinding::new(table_index, 0), 0);
            let mut expressions = vec![builtin_aggregate(&context, "first", vec![first_child])?];
            if error_on_multiple_rows {
                expressions.push(builtin_aggregate(&context, "count_star", Vec::new())?);
            }
            let first_type = expressions[0].return_type.clone();
            let count_type = expressions.get(1).map(|count| count.return_type.clone());
            let mut aggregate_index = scope.binder.generate_table_index();
            let mut plan = aggregate(scope, aggregate_index, expressions, plan);
            if let Some(count_type) = count_type {
                let projection_index = scope.binder.generate_table_index();
                let first_ref =
                    Expression::column_ref("", first_type.clone(), ColumnBinding::new(aggregate_index, 0), 0);
                let count_ref =
                    Expression::column_ref("", count_type, ColumnBinding::new(aggregate_index, 1), 0);
                let count_check = Expression::comparison(
                    ExpressionType::CompareGreaterthan,
                    count_ref,
                    Expression::constant(Value::bigint(1)),
                );
                let message = Value::varchar(
                    "More than one row returned by a subquery used as an expression - scalar subqueries can only \
                     return a single row.\n\nUse \"SET scalar_subquery_error_on_multiple_rows=false\" to revert to \
                     previous behavior of returning a random row.",
                );
                let mut error = builtin_scalar(&context, "error", vec![Expression::constant(message)])?;
                error.return_type = first_type.clone();
                let case = Expression::new(
                    ExpressionType::CaseExpr,
                    error.return_type.clone(),
                    ExpressionKind::Case(Box::new(BoundCaseExpression {
                        case_checks: vec![BoundCaseCheck {
                            when_expr: count_check,
                            then_expr: error,
                        }],
                        else_expr: first_ref,
                    })),
                );
                plan = LogicalOperator::projection(projection_index, vec![case], plan);
                aggregate_index = projection_index;
            }
            replace_root(root, |root| Ok(LogicalOperator::cross_product(root, plan)))?;
            Ok(Expression::column_ref(
                name,
                return_type,
                ColumnBinding::new(aggregate_index, 0),
                0,
            ))
        }
        _ => {
            // ANY: a MARK join, giving TRUE, FALSE or NULL.
            let plan_columns = plan.column_bindings();
            let mark_index = scope.binder.generate_table_index();
            let mut join =
                LogicalComparisonJoin::new(JoinType::Mark, LogicalOperatorType::LogicalComparisonJoin);
            join.mark_index = mark_index;
            let children = std::mem::take(&mut subquery.children);
            if children.len() == 1 && subquery.child_types.len() > 1 {
                // `(a, b) < ANY(...)`: compare structs built from the subquery columns.
                let mut struct_children = Vec::with_capacity(subquery.child_types.len());
                for (i, (child_type, compare_type)) in subquery
                    .child_types
                    .iter()
                    .zip(&subquery.child_targets)
                    .enumerate()
                {
                    let column = Expression::column_ref("", child_type.clone(), plan_columns[i], 0);
                    struct_children.push(add_cast_to_type(&context, column, compare_type, false)?);
                }
                let right = builtin_scalar(&context, "row", struct_children)?;
                let left = children
                    .into_iter()
                    .next()
                    .ok_or_else(|| Error::internal("ANY without a child"))?;
                let left_type = left.return_type.clone();
                let right_type = right.return_type.clone();
                join.conditions.push(JoinCondition {
                    left: push_collation(&context, left, &left_type)?,
                    right: push_collation(&context, right, &right_type)?,
                    comparison: subquery.comparison_type,
                });
            } else {
                for (i, left) in children.into_iter().enumerate() {
                    let child_type = &subquery.child_types[i];
                    let compare_type = &subquery.child_targets[i];
                    let column = Expression::column_ref("", child_type.clone(), plan_columns[i], 0);
                    let right = add_cast_to_type(&context, column, compare_type, false)?;
                    join.conditions.push(JoinCondition {
                        left: push_collation(&context, left, compare_type)?,
                        right: push_collation(&context, right, compare_type)?,
                        comparison: subquery.comparison_type,
                    });
                }
            }
            replace_root(root, |root| {
                Ok(LogicalOperator::new(LogicalKind::ComparisonJoin(Box::new(join)))
                    .with_children(vec![root, plan]))
            })?;
            Ok(Expression::column_ref(
                name,
                return_type,
                ColumnBinding::new(mark_index, 0),
                0,
            ))
        }
    }
}

/// A dependent join of `original_plan` with the subquery to be added as its second
/// child (DuckDB's `CreateDuplicateEliminatedJoin`).
fn create_duplicate_eliminated_join(
    correlated_columns: &CorrelatedColumns,
    join_type: JoinType,
    original_plan: LogicalOperator,
    perform_delim: bool,
) -> (LogicalDependentJoin, LogicalOperator) {
    let mut join = LogicalDependentJoin::new(join_type);
    join.correlated_columns = correlated_columns.clone();
    join.perform_delim = perform_delim;
    for column in correlated_columns {
        join.join
            .duplicate_eliminated_columns
            .push(Expression::column_ref(
                "",
                column.logical_type.clone(),
                column.binding,
                0,
            ));
        join.join.mark_types.push(column.logical_type.clone());
    }
    (join, original_plan)
}

/// Whether duplicate elimination can group by a column of this type (DuckDB's
/// `PerformDelimOnType`): not for lists.
fn perform_delim_on_type(ty: &LogicalType) -> bool {
    if ty.physical_type() == PhysicalType::List {
        return false;
    }
    if ty.physical_type() == PhysicalType::Struct
        && let Some(children) = ty.struct_children()
    {
        return children.iter().all(|(_, child)| perform_delim_on_type(child));
    }
    true
}

/// DuckDB's `PerformDuplicateElimination`: when a correlated column cannot be grouped,
/// a `delim_index` row number column is added and used instead.
fn perform_duplicate_elimination(scope: &BindScope<'_>, correlated_columns: &mut CorrelatedColumns) -> bool {
    if correlated_columns
        .iter()
        .all(|column| perform_delim_on_type(&column.logical_type))
    {
        return true;
    }
    correlated_columns.add_column(CorrelatedColumnInfo {
        binding: ColumnBinding::new(scope.binder.generate_table_index(), 0),
        logical_type: LogicalType::BIGINT,
        name: "delim_index".into(),
        depth: 0,
    });
    correlated_columns.set_delim_index_to_zero();
    false
}

/// DuckDB's `PlanCorrelatedSubquery`: a dependent join of `root` with the subquery.
fn plan_correlated_subquery(
    scope: &mut BindScope<'_>,
    subquery: &mut BoundSubqueryExpression,
    name: roastedduck_common::ByteString,
    return_type: LogicalType,
    root: &mut LogicalOperator,
    plan: LogicalOperator,
) -> Result<Expression> {
    let subquery_type = subquery.subquery_type;
    let perform_delim = if subquery_type == SubqueryType::Any {
        true
    } else {
        perform_duplicate_elimination(scope, &mut subquery.binder.correlated_columns)
    };
    let correlated_columns = subquery.binder.correlated_columns.clone();
    match subquery_type {
        SubqueryType::Scalar => {
            // A SINGLE join on the correlated columns only (NULLs compare equal).
            let plan_column = *plan
                .column_bindings()
                .last()
                .ok_or_else(|| Error::internal("scalar subquery without columns"))?;
            replace_root(root, |root| {
                let (mut join, root) = create_duplicate_eliminated_join(
                    &correlated_columns,
                    JoinType::Single,
                    root,
                    perform_delim,
                );
                join.subquery_type = SubqueryType::Scalar;
                join.any_join = false;
                Ok(LogicalOperator::new(LogicalKind::DependentJoin(Box::new(join)))
                    .with_children(vec![root, plan]))
            })?;
            Ok(Expression::column_ref(name, return_type, plan_column, 0))
        }
        SubqueryType::Exists => {
            let mark_index = scope.binder.generate_table_index();
            replace_root(root, |root| {
                let (mut join, root) = create_duplicate_eliminated_join(
                    &correlated_columns,
                    JoinType::Mark,
                    root,
                    perform_delim,
                );
                join.subquery_type = SubqueryType::Exists;
                join.join.mark_index = mark_index;
                join.any_join = true;
                Ok(LogicalOperator::new(LogicalKind::DependentJoin(Box::new(join)))
                    .with_children(vec![root, plan]))
            })?;
            Ok(Expression::column_ref(
                name,
                return_type,
                ColumnBinding::new(mark_index, 0),
                0,
            ))
        }
        _ => {
            let mark_index = scope.binder.generate_table_index();
            if subquery.children.len() > 1 {
                return Err(Error::not_implemented(
                    "Correlated IN/ANY/ALL with multiple columns not yet supported",
                ));
            }
            let children = std::mem::take(&mut subquery.children);
            let child_types = subquery.child_types.clone();
            let child_targets = subquery.child_targets.clone();
            let comparison_type = subquery.comparison_type;
            replace_root(root, |root| {
                let (mut join, root) = create_duplicate_eliminated_join(
                    &correlated_columns,
                    JoinType::Mark,
                    root,
                    perform_delim,
                );
                join.subquery_type = SubqueryType::Any;
                join.join.mark_index = mark_index;
                join.any_join = true;
                join.expression_children = children;
                join.child_types = child_types;
                join.child_targets = child_targets;
                join.comparison_type = comparison_type;
                Ok(LogicalOperator::new(LogicalKind::DependentJoin(Box::new(join)))
                    .with_children(vec![root, plan]))
            })?;
            Ok(Expression::column_ref(
                name,
                return_type,
                ColumnBinding::new(mark_index, 0),
                0,
            ))
        }
    }
}

/// Plans one subquery below `root` (DuckDB's `Binder::PlanSubquery`).
fn plan_subquery(
    scope: &mut BindScope<'_>,
    expression: Expression,
    root: &mut LogicalOperator,
) -> Result<Expression> {
    let name = expression.name();
    let return_type = expression.return_type.clone();
    let ExpressionKind::Subquery(mut subquery) = expression.kind else {
        return Err(Error::internal("expected a subquery"));
    };
    let plan = std::mem::replace(
        &mut subquery.subquery.plan,
        LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 }),
    );
    let result = if subquery.is_correlated() {
        plan_correlated_subquery(scope, &mut subquery, name, return_type, root, plan)?
    } else {
        plan_uncorrelated_subquery(scope, &mut subquery, name, return_type, root, plan)?
    };
    scope.binder.increase_depth()?;
    Ok(result)
}

/// Plans the subqueries in `expression` as joins below `root` and replaces them with
/// references to the join's output (DuckDB's `Binder::PlanSubqueries`).
pub fn plan_subqueries(
    scope: &mut BindScope<'_>,
    expression: &mut Expression,
    root: &mut LogicalOperator,
) -> Result<()> {
    if !expression.has_subquery() {
        return Ok(());
    }
    expression.try_for_each_child_mut(|child| plan_subqueries(scope, child, root))?;
    if matches!(expression.kind, ExpressionKind::Subquery(_)) {
        let placeholder = Expression::constant(Value::null(LogicalType::SQLNULL));
        let subquery = std::mem::replace(expression, placeholder);
        *expression = plan_subquery(scope, subquery, root)?;
    }
    Ok(())
}

/// A LATERAL join: a dependent join of `left` and `right` (DuckDB's
/// `Binder::PlanLateralJoin`).
pub fn plan_lateral_join(
    scope: &mut BindScope<'_>,
    mut left: LogicalOperator,
    mut right: LogicalOperator,
    correlated: &mut CorrelatedColumns,
    join_type: JoinType,
    condition: Option<Expression>,
) -> Result<LogicalOperator> {
    let mut conditions = Vec::new();
    let mut arbitrary_expressions = Vec::new();
    if let Some(condition) = condition {
        if condition.has_subquery() {
            return Err(
                Error::binder("Subqueries are not supported in LATERAL join conditions")
                    .with_position(condition.query_location),
            );
        }
        let context = scope.binder.context.clone();
        extract_join_conditions_from(
            &context,
            join_type,
            JoinRefType::Regular,
            &mut left,
            &mut right,
            condition,
            &mut conditions,
            &mut arbitrary_expressions,
        )?;
    }
    let perform_delim = perform_duplicate_elimination(scope, correlated);
    let (mut join, left) = create_duplicate_eliminated_join(correlated, join_type, left, perform_delim);
    join.perform_delim = perform_delim;
    join.any_join = false;
    join.propagate_null_values = join_type != JoinType::Inner;
    join.is_lateral_join = true;
    join.arbitrary_expressions = arbitrary_expressions;
    join.join.conditions = conditions;
    Ok(LogicalOperator::new(LogicalKind::DependentJoin(Box::new(join))).with_children(vec![left, right]))
}

/// Plans subqueries left in the expressions of `op` and its descendants (DuckDB's
/// `RecursiveDependentJoinPlanner`): each operator's subqueries are planned on top of
/// its children.
pub fn recursive_dependent_join_planner(scope: &mut BindScope<'_>, op: &mut LogicalOperator) -> Result<()> {
    if op.children.is_empty() {
        return Ok(());
    }
    for i in 0..op.children.len() {
        let mut child = std::mem::replace(
            &mut op.children[i],
            LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 }),
        );
        let planned =
            op.try_for_each_expression_mut(|expression| plan_subqueries(scope, expression, &mut child));
        op.children[i] = child;
        planned?;
    }
    for child in &mut op.children {
        recursive_dependent_join_planner(scope, child)?;
    }
    Ok(())
}
