//! SELECT nodes (DuckDB's `bind_select_node.cpp` and `plan_select_node.cpp`).

use rustdb_ast::{
    AggregateHandling, ExpressionType, ParsedExpression, ParsedExpressionKind, QueryNode, QueryNodeKind, SelectNode,
};
use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_types::LogicalType;

use super::modifiers::{OrderTargets, bind_modifiers, prepare_modifiers, visit_query_node};
use super::{BoundSelectNode, plan_subqueries};
use crate::binder::expression::expand_star_expression;
use crate::binder::{BindScope, BoundStatement};
use crate::expression::{Expression, ExpressionKind};
use crate::expression_binder::{
    BoundGroupInformation, ColumnAliasBinder, ExpressionBinder, GroupBinder, SelectBinder, SelectRole, WhereBinder,
    qualify_column_names,
};
use crate::operator::{LogicalAggregate, LogicalKind, LogicalOperator};
use crate::ColumnBinding;

/// Binds a SELECT node: its FROM clause first (DuckDB's `BindNode(SelectNode &)`).
pub fn bind_select(scope: &mut BindScope<'_>, node: &mut QueryNode) -> Result<BoundStatement> {
    let QueryNodeKind::Select(select) = &mut node.kind else {
        return Err(Error::internal("expected a SELECT node"));
    };
    let from_table = crate::binder::tableref::bind_table_ref(scope, &mut select.from_table)?;
    let modifiers = std::mem::take(&mut node.modifiers);
    let QueryNodeKind::Select(select) = &mut node.kind else {
        return Err(Error::internal("expected a SELECT node"));
    };
    bind_select_node(scope, select, modifiers, from_table)

}

/// Expands COLUMNS(...) in WHERE into a conjunction (DuckDB's `BindWhereStarExpression`).
fn bind_where_star_expression(scope: &mut BindScope<'_>, expr: &mut ParsedExpression) -> Result<()> {
    if expr.expression_type == ExpressionType::ConjunctionAnd {
        if let ParsedExpressionKind::Conjunction(conjunction) = &mut expr.kind {
            for child in &mut conjunction.children {
                bind_where_star_expression(scope, child)?;
            }
        }
        return Ok(());
    }
    if let ParsedExpressionKind::Star(star) = &expr.kind
        && !star.columns
    {
        return Err(Error::parser(
            "STAR expression is not allowed in the WHERE clause. Use COLUMNS(*) instead.",
        ));
    }
    let original = std::mem::replace(expr, ParsedExpression::constant(rustdb_types::Value::boolean(true)));
    let mut conditions = Vec::new();
    expand_star_expression(scope, original, &mut conditions)?;
    let mut conditions = conditions.into_iter();
    let Some(first) = conditions.next() else {
        return Err(Error::parser("COLUMNS expansion resulted in empty set of columns"));
    };
    *expr = conditions.fold(first, |left, right| {
        ParsedExpression::new(
            ExpressionType::ConjunctionAnd,
            ParsedExpressionKind::Conjunction(rustdb_ast::ConjunctionExpression {
                children: vec![left, right],
            }),
        )
    });
    Ok(())
}

/// DuckDB's `Binder::BindSelectNode`.
pub fn bind_select_node(
    scope: &mut BindScope<'_>,
    statement: &mut SelectNode,
    mut modifiers: Vec<rustdb_ast::ResultModifier>,
    from_table: BoundStatement,
) -> Result<BoundStatement> {
    let mut result = BoundSelectNode {
        projection_index: scope.generate_table_index(),
        group_index: scope.generate_table_index(),
        aggregate_index: scope.generate_table_index(),
        groupings_index: scope.generate_table_index(),
        window_index: scope.generate_table_index(),
        prune_index: scope.generate_table_index(),
        sample_options: statement.sample.take(),
        ..BoundSelectNode::default()
    };

    // Expand `*` in the select list.
    let select_list = std::mem::take(&mut statement.select_list);
    let mut expanded = Vec::with_capacity(select_list.len());
    for expr in select_list {
        expand_star_expression(scope, expr, &mut expanded)?;
    }
    if expanded.is_empty() {
        return Err(Error::binder("SELECT list is empty after resolving * expressions!"));
    }
    let mut select_list = expanded;
    for (i, expr) in select_list.iter_mut().enumerate() {
        result.names.push(expr.name());
        qualify_column_names(scope.binder, expr)?;
        if !expr.alias.is_empty() {
            result.bind_state.alias_map.insert(expr.alias.clone(), i);
            result.names[i].clone_from(&expr.alias);
        }
        result.bind_state.projection_map.insert(expr, i);
        result.bind_state.original_expressions.push(expr.clone());
    }
    result.column_count = select_list.len();

    // WHERE
    if let Some(mut condition) = statement.where_clause.take() {
        bind_where_star_expression(scope, &mut condition)?;
        let (inner, outer) = scope.split();
        let mut where_binder = WhereBinder::new(inner, outer, Some(&mut result.bind_state));
        result.where_clause = Some(where_binder.bind(&mut condition, true)?);
    }

    // DISTINCT, ORDER BY and LIMIT targets (may append to the select list).
    let column_count = result.names.len();
    let mut bound_modifiers = Vec::new();
    prepare_modifiers(
        scope,
        &mut modifiers,
        OrderTargets::SelectList(&mut select_list),
        &mut result.bind_state,
        column_count,
        &mut bound_modifiers,
    )?;
    result.modifiers = bound_modifiers;

    // GROUP BY
    let mut info = BoundGroupInformation::default();
    if !statement.group_expressions.is_empty() {
        let previous_can_contain_nulls = scope.binder.can_contain_nulls;
        scope.binder.can_contain_nulls = true;
        let groups = std::mem::take(&mut statement.group_expressions);
        let mut bound_groups = Vec::with_capacity(groups.len());
        let mut unbound_groups = Vec::with_capacity(groups.len());
        {
            let (inner, outer) = scope.split();
            let mut group_alias_map = CaseInsensitiveMap::default();
            let mut group_binder =
                GroupBinder::new(inner, outer, &mut select_list, &result.bind_state, &mut group_alias_map);
            for (i, mut group) in groups.into_iter().enumerate() {
                group_binder.unbound_expression = Some(group.clone());
                group_binder.bind_index = i;
                let bound = group_binder.bind(&mut group, true)?;
                if bound.has_subquery() {
                    // A group with a subquery cannot be copied; no collation handling.
                }
                bound_groups.push(bound);
                unbound_groups.push(group_binder.unbound_expression.take().unwrap_or(group));
            }
            drop(group_binder);
            info.alias_map = group_alias_map;
        }
        scope.binder.can_contain_nulls = previous_can_contain_nulls;
        for (i, mut unbound) in unbound_groups.into_iter().enumerate() {
            qualify_column_names(scope.binder, &mut unbound)?;
            info.map.insert(&unbound, i);
        }
        result.groups.group_expressions = bound_groups;
    }
    result.groups.grouping_sets = statement.group_sets.clone();

    // HAVING
    if let Some(mut having) = statement.having.take() {
        let (inner, outer) = scope.split();
        let mut having_binder = SelectBinder::new(
            inner,
            outer,
            &mut result,
            &info,
            SelectRole::Having {
                aggregate_handling: statement.aggregate_handling,
                alias_binder: ColumnAliasBinder::default(),
            },
        );
        crate::expression_binder::ColumnQualifier::qualify_column_names(&having_binder, &mut having)?;
        let bound = having_binder.bind(&mut having, true)?;
        drop(having_binder);
        result.having = Some(bound);
    }

    // QUALIFY
    let mut bound_qualify_columns = Vec::new();
    if let Some(mut qualify) = statement.qualify.take() {
        if statement.aggregate_handling == AggregateHandling::ForceAggregates {
            return Err(Error::binder("Combining QUALIFY with GROUP BY ALL is not supported yet"));
        }
        qualify_column_names(scope.binder, &mut qualify)?;
        let (inner, outer) = scope.split();
        let mut qualify_binder = SelectBinder::new(
            inner,
            outer,
            &mut result,
            &info,
            SelectRole::Qualify {
                alias_binder: ColumnAliasBinder::default(),
            },
        );
        let bound = qualify_binder.bind(&mut qualify, true)?;
        if qualify_binder.has_bound_columns() {
            if qualify_binder.bound_aggregates() {
                return Err(Error::binder("Cannot mix aggregates with non-aggregated columns!"));
            }
            bound_qualify_columns = qualify_binder.state_ref().bound_columns.clone();
        }
        drop(qualify_binder);
        result.qualify = Some(bound);
    }

    // The select list.
    let mut group_by_all_indexes = Vec::new();
    let mut new_names = Vec::new();
    let mut internal_types = Vec::new();
    let mut select_columns = Vec::new();
    let mut select_bound_columns = Vec::new();
    let mut original_names = std::mem::take(&mut result.names);
    {
        let (inner, outer) = scope.split();
        let column_count = result.column_count;
        let mut select_binder = SelectBinder::new(inner, outer, &mut result, &info, SelectRole::Select);
        for (i, expr) in select_list.iter_mut().enumerate() {
            let is_window = matches!(expr.kind, ParsedExpressionKind::Window(_));
            let bound = select_binder.bind(expr, true)?;
            let is_original_column = i < column_count;
            let can_group_by_all =
                statement.aggregate_handling == AggregateHandling::ForceAggregates && is_original_column;
            select_binder.node_mut().bound_column_count += 1;
            if let ExpressionKind::Expanded(expanded) = bound.kind {
                if !is_original_column {
                    return Err(Error::binder("UNNEST of struct cannot be used in ORDER BY/DISTINCT ON clause"));
                }
                if statement.aggregate_handling == AggregateHandling::ForceAggregates {
                    return Err(Error::binder("UNNEST of struct cannot be combined with GROUP BY ALL"));
                }
                let count = expanded.expanded_expressions.len();
                for struct_expr in expanded.expanded_expressions {
                    new_names.push(struct_expr.name());
                    select_binder.node_mut().types.push(struct_expr.return_type.clone());
                    internal_types.push(struct_expr.return_type.clone());
                    select_columns.push(struct_expr);
                }
                select_binder.node_mut().bind_state.add_expanded_column(count);
                continue;
            }
            if bound.is_volatile() {
                select_binder.node_mut().bind_state.set_expression_is_volatile(i)?;
            }
            if bound.has_subquery() {
                select_binder.node_mut().bind_state.set_expression_has_subquery(i);
            }
            select_binder.node_mut().bind_state.add_regular_column();
            if can_group_by_all && select_binder.has_bound_columns() {
                if select_binder.bound_aggregates() {
                    return Err(Error::binder("Cannot mix aggregates with non-aggregated columns!"));
                }
                if is_window {
                    return Err(Error::binder("Cannot group on a window clause"));
                }
                group_by_all_indexes.push(i);
            }
            let result_type = bound.return_type.clone();
            select_columns.push(bound);
            if is_original_column {
                new_names.push(std::mem::take(&mut original_names[i]));
                select_binder.node_mut().types.push(result_type.clone());
            }
            internal_types.push(result_type);
            if can_group_by_all {
                select_binder.reset_bindings();
            }
        }
        if select_binder.has_bound_columns() {
            select_bound_columns = select_binder.state_ref().bound_columns.clone();
        }
    }
    result.select_list = select_columns;

    // GROUP BY ALL: the selected non-aggregates become the groups.
    for index in &group_by_all_indexes {
        let expr = &mut result.select_list[*index];
        let group_ref = Expression::column_ref(
            "",
            expr.return_type.clone(),
            ColumnBinding::new(result.group_index, result.groups.group_expressions.len() as u64),
            0,
        );
        let group = std::mem::replace(expr, group_ref);
        result.groups.group_expressions.push(group);
    }
    if !group_by_all_indexes.is_empty() {
        let set = (0..result.groups.group_expressions.len() as u64).collect();
        result.groups.grouping_sets.push(set);
    }
    result.column_count = new_names.len();
    result.names = new_names;
    result.need_prune = result.select_list.len() > result.column_count;

    // Non-aggregated columns of an aggregating query are an error (PostgreSQL behaviour).
    let aggregating = !result.groups.group_expressions.is_empty()
        || !result.aggregates.is_empty()
        || result.having.is_some()
        || !result.groups.grouping_sets.is_empty();
    if aggregating {
        if statement.aggregate_handling == AggregateHandling::NoAggregatesAllowed {
            return Err(Error::binder("Aggregates cannot be present in a Project relation!"));
        }
        let mut bound_columns = select_bound_columns;
        bound_columns.extend(bound_qualify_columns);
        if let Some(column) = bound_columns.first() {
            let name = &column.name;
            let mut message = format!(
                "column \"{name}\" must appear in the GROUP BY clause or must be part of an aggregate function."
            );
            if statement.aggregate_handling == AggregateHandling::ForceAggregates {
                message.push_str(
                    "\nGROUP BY ALL will only group entries in the SELECT list. Add it to the SELECT list or GROUP BY \
                     this entry explicitly.",
                );
            } else {
                message.push_str(&format!(
                    "\nEither add it to the GROUP BY list, or use \"ANY_VALUE({name})\" if the exact value of \"{name}\" \
                     is not important."
                ));
            }
            return Err(Error::binder(message).with_position(column.query_location));
        }
    }
    if result.qualify.is_some() && result.windows.is_empty() {
        return Err(Error::binder(
            "at least one window function must appear in the SELECT column or QUALIFY clause",
        ));
    }

    let names = result.names.clone();
    let mut bound_modifiers = std::mem::take(&mut result.modifiers);
    bind_modifiers(&mut bound_modifiers, result.projection_index, &names, &internal_types, &result.bind_state)?;
    result.modifiers = bound_modifiers;
    let original_expressions = std::mem::take(&mut result.bind_state.original_expressions);

    let types = result.types.clone();
    let plan = create_plan(scope, result, from_table)?;
    let mut statement_result = BoundStatement::new(plan, names, types);
    statement_result.extra_info.original_expressions = original_expressions;
    Ok(statement_result)
}

/// DuckDB's `Binder::CreatePlan(BoundSelectNode &)`.
fn create_plan(scope: &mut BindScope<'_>, mut statement: BoundSelectNode, from_table: BoundStatement) -> Result<LogicalOperator> {
    let mut root = from_table.plan;
    if let Some(sample_options) = statement.sample_options.take() {
        root = LogicalOperator::new(LogicalKind::Sample { sample_options }).with_children(vec![root]);
    }
    if let Some(mut condition) = statement.where_clause.take() {
        plan_subqueries(scope, &mut condition, &mut root)?;
        root = LogicalOperator::filter(condition, root);
    }
    if !statement.aggregates.is_empty() || !statement.groups.group_expressions.is_empty() || statement.having.is_some() {
        for group in &mut statement.groups.group_expressions {
            plan_subqueries(scope, group, &mut root)?;
        }
        for aggregate in &mut statement.aggregates {
            plan_subqueries(scope, aggregate, &mut root)?;
        }
        let aggregate = LogicalAggregate {
            group_index: statement.group_index,
            aggregate_index: statement.aggregate_index,
            groupings_index: statement.groupings_index,
            groups: std::mem::take(&mut statement.groups.group_expressions),
            grouping_sets: std::mem::take(&mut statement.groups.grouping_sets),
            grouping_functions: std::mem::take(&mut statement.grouping_functions),
        };
        root = LogicalOperator::new(LogicalKind::Aggregate(Box::new(aggregate)))
            .with_expressions(std::mem::take(&mut statement.aggregates))
            .with_children(vec![root]);
    } else if !statement.groups.grouping_sets.is_empty() {
        // Grouping sets without groups or aggregates (`GROUP BY ()`): a single row.
        root = LogicalOperator::new(LogicalKind::DummyScan {
            table_index: statement.group_index,
        });
    }
    if let Some(mut having) = statement.having.take() {
        plan_subqueries(scope, &mut having, &mut root)?;
        root = LogicalOperator::filter(having, root);
    }
    if !statement.windows.is_empty() {
        let mut windows = std::mem::take(&mut statement.windows);
        for window in &mut windows {
            plan_subqueries(scope, window, &mut root)?;
        }
        root = LogicalOperator::new(LogicalKind::Window {
            window_index: statement.window_index,
        })
        .with_expressions(windows)
        .with_children(vec![root]);
    }
    if let Some(mut qualify) = statement.qualify.take() {
        plan_subqueries(scope, &mut qualify, &mut root)?;
        root = LogicalOperator::filter(qualify, root);
    }
    let unnests = std::mem::take(&mut statement.unnests);
    for (_, mut unnest) in unnests.into_iter().rev() {
        for expr in &mut unnest.expressions {
            plan_subqueries(scope, expr, &mut root)?;
        }
        root = LogicalOperator::new(LogicalKind::Unnest {
            unnest_index: unnest.index,
        })
        .with_expressions(unnest.expressions)
        .with_children(vec![root]);
    }
    let mut select_list = std::mem::take(&mut statement.select_list);
    for expr in &mut select_list {
        plan_subqueries(scope, expr, &mut root)?;
    }
    let projection_types: Vec<LogicalType> = select_list.iter().map(|e| e.return_type.clone()).collect();
    root = LogicalOperator::projection(statement.projection_index, select_list, root);
    root = visit_query_node(std::mem::take(&mut statement.modifiers), root);
    if statement.need_prune {
        let prune = (0..statement.column_count)
            .map(|i| {
                Expression::column_ref(
                    "",
                    projection_types[i].clone(),
                    ColumnBinding::new(statement.projection_index, i as u64),
                    0,
                )
            })
            .collect();
        root = LogicalOperator::projection(statement.prune_index, prune, root);
    }
    Ok(root)
}
