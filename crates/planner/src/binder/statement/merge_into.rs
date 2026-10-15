//! `MERGE INTO` and `INSERT ... ON CONFLICT` (DuckDB's `bind_merge_into.cpp` and
//! `Binder::GenerateMergeInto` of `bind_insert.cpp`): an upsert is rewritten into the
//! equivalent MERGE, which joins the source with the target table and runs per-row
//! actions over a projection of the join.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use rustdb_ast::{
    AggregateHandling, BaseTableRef, CommonTableExpressionMap, ComparisonExpression, ConjunctionExpression,
    DistinctModifier, ExpressionType, InsertColumnOrder, InsertStatement, JoinRefType, JoinType,
    MergeActionCondition, MergeActionType, MergeIntoAction, MergeIntoStatement, OnConflictAction,
    ParsedExpression, ParsedExpressionKind, QueryNode, QueryNodeKind, ResultModifier, SelectNode,
    SelectStatement, StarExpression, SubqueryRef, TableRef, TableRefKind, UpdateSetInfo,
};
use rustdb_catalog::{CatalogEntry, TableEntry};
use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_types::{LogicalType, Value};

use super::super::query_node::plan_subqueries;
use super::super::tableref::bind_joinref::{JoinSpec, bind_bound_join};
use super::super::tableref::{bind_table_ref, table_references};
use super::super::{BindScope, Binder, BindingAlias, BoundStatement, Scope};
use super::modify::{
    InsertColumnList, UpdateSet, bind_insert_column_list, bind_row_id_column, bind_update_constraints,
    bind_update_set, check_column_count, expand_default_in_values_list,
};
use super::table::{
    bind_constraints, bind_default_values, bind_returning_with_virtual, expand_default, table_of,
};
use crate::ColumnBinding;
use crate::expression::Expression;
use crate::expression_binder::{
    ExpressionBinder, InsertBinder, ProjectionBinder, for_each_child, is_lambda_function,
};
use crate::operator::{
    BoundConstraint, BoundMergeIntoAction, LogicalKind, LogicalMergeInto, LogicalOperator,
};
use crate::prepared::StatementReturnType;

/// A source column: the alias of its relation and its name.
type SourceColumn = (BindingAlias, String);

/// The bound pieces of the merge the actions share.
struct MergeTarget<'a> {
    table: &'a TableEntry,
    table_name: &'a str,
    return_chunk: bool,
    proj_index: u64,
}

/// A table scan somewhere in `op` with table index `table_index`.
fn find_get(op: &mut LogicalOperator, table_index: u64) -> Option<&mut LogicalOperator> {
    if let LogicalKind::Get(get) = &op.kind
        && get.table_index == table_index
    {
        return Some(op);
    }
    op.children
        .iter_mut()
        .find_map(|child| find_get(child, table_index))
}

/// Column references to the source columns (DuckDB's `GenerateColumnReferences`).
fn column_references(binder: &Binder, source: &[SourceColumn]) -> Vec<ParsedExpression> {
    source
        .iter()
        .map(|(alias, name)| {
            binder
                .bind_context
                .create_column_reference_unexpanded(alias, name)
        })
        .collect()
}

/// `CheckMergeAction`.
fn check_merge_action(condition: MergeActionCondition, action_type: MergeActionType) -> Result<()> {
    if condition == MergeActionCondition::WhenNotMatchedByTarget
        && matches!(
            action_type,
            MergeActionType::MergeUpdate | MergeActionType::MergeDelete
        )
    {
        return Err(Error::parser(
            "WHEN NOT MATCHED (BY TARGET) cannot be combined with UPDATE or DELETE actions - as there is no \
             corresponding row in the target to update or delete.\nDid you mean to use WHEN MATCHED or WHEN \
             NOT MATCHED BY SOURCE?",
        ));
    }
    Ok(())
}

/// `Binder::BindMergeAction`: conditions, new values and error messages are computed by
/// the projection below the merge (`expressions`), and the action refers to them.
#[allow(clippy::too_many_lines, reason = "mirrors Binder::BindMergeAction")]
fn bind_merge_action(
    scope: &mut BindScope<'_>,
    target: &MergeTarget<'_>,
    bound_constraints: &[BoundConstraint],
    get: &mut LogicalOperator,
    expressions: &mut Vec<Expression>,
    action: &mut MergeIntoAction,
    source: &[SourceColumn],
) -> Result<BoundMergeIntoAction> {
    let table = target.table;
    let mut result = BoundMergeIntoAction {
        action_type: action.action_type,
        condition: None,
        columns: Vec::new(),
        expressions: Vec::new(),
        column_index_map: Vec::new(),
        update_is_del_and_insert: false,
    };
    if let Some(condition) = &mut action.condition {
        let (inner, outer) = scope.split();
        let mut binder = ProjectionBinder::new(
            inner,
            outer,
            target.proj_index,
            expressions,
            "WHERE clause",
            LogicalType::BOOLEAN,
        );
        result.condition = Some(binder.bind(condition, true)?);
    }
    match action.action_type {
        MergeActionType::MergeUpdate => {
            let update_info = match &mut action.update_info {
                Some(info) => info,
                none => {
                    let mut info = UpdateSetInfo {
                        condition: None,
                        columns: Vec::new(),
                        expressions: Vec::new(),
                    };
                    if action.column_order == InsertColumnOrder::InsertByName {
                        for (alias, name) in source {
                            if action.exclude_columns.contains(name) {
                                continue;
                            }
                            info.columns.push(name.clone());
                            info.expressions.push(
                                scope
                                    .binder
                                    .bind_context
                                    .create_column_reference_unexpanded(alias, name),
                            );
                        }
                    } else {
                        info.columns = table
                            .physical_columns()
                            .map(|(_, _, column)| column.name.clone())
                            .collect();
                        if source.len() != info.columns.len() {
                            return Err(Error::binder(format!(
                                "Data provided for UPDATE did not match column count in table - expected {} \
                                 columns but got {}",
                                info.columns.len(),
                                source.len()
                            )));
                        }
                        info.expressions = column_references(scope.binder, source);
                    }
                    none.insert(Box::new(info))
                }
            };
            let mut set = UpdateSet {
                proj_index: target.proj_index,
                projection_expressions: std::mem::take(expressions),
                ..UpdateSet::default()
            };
            let bound = bind_update_set(scope, None, update_info, table, &mut set);
            let bound = bound.and_then(|()| {
                bind_update_constraints(table, get, bound_constraints, target.return_chunk, &mut set)
            });
            *expressions = std::mem::take(&mut set.projection_expressions);
            result.update_is_del_and_insert = bound?;
            result.columns = set.columns;
            result.expressions = set.update_expressions;
        }
        MergeActionType::MergeInsert => {
            if action.column_order == InsertColumnOrder::InsertByName {
                if !action.insert_columns.is_empty() {
                    return Err(Error::internal(
                        "INSERT BY NAME cannot be combined with a column list",
                    ));
                }
                action.insert_columns = source.iter().map(|(_, name)| name.clone()).collect();
            }
            let InsertColumnList {
                named_column_map,
                expected_types,
                column_index_map,
            } = bind_insert_column_list(
                table,
                target.table_name,
                &action.insert_columns,
                action.default_values,
            )?;
            result.column_index_map = column_index_map;
            if !action.default_values && action.expressions.is_empty() {
                action.expressions = column_references(scope.binder, source);
            }
            check_column_count(
                expected_types.len(),
                action.expressions.len(),
                !action.insert_columns.is_empty(),
                target.table_name,
            )?;
            let mut insert_expressions = Vec::with_capacity(action.expressions.len());
            for (expression, &logical) in action.expressions.iter_mut().zip(&named_column_map) {
                let column = &table.columns[logical];
                expand_default(expression, column.default_value.as_ref(), &column.logical_type);
                let (inner, outer) = scope.split();
                let mut binder = InsertBinder::new(inner, outer);
                binder.set_target_type(Some(column.logical_type.clone()));
                insert_expressions.push(binder.bind(expression, true)?);
            }
            for expression in insert_expressions {
                result.expressions.push(Expression::column_ref(
                    "",
                    expression.return_type.clone(),
                    ColumnBinding::new(target.proj_index, expressions.len() as u64),
                    0,
                ));
                expressions.push(expression);
            }
        }
        MergeActionType::MergeError => {
            for expression in &mut action.expressions {
                let (inner, outer) = scope.split();
                let mut binder = ProjectionBinder::new(
                    inner,
                    outer,
                    target.proj_index,
                    expressions,
                    "Error Message",
                    LogicalType::VARCHAR,
                );
                result.expressions.push(binder.bind(expression, true)?);
            }
        }
        MergeActionType::MergeDelete | MergeActionType::MergeDoNothing => {}
    }
    Ok(result)
}

/// Rewrites references to `source_bindings` into references to the projection
/// `new_table_index` pushed above the source (DuckDB's `RewriteMergeBindings`).
fn rewrite_merge_bindings(
    expression: &mut Expression,
    source_bindings: &[ColumnBinding],
    new_table_index: u64,
) {
    if let crate::expression::ExpressionKind::ColumnRef(column) = &mut expression.kind {
        if let Some(position) = source_bindings
            .iter()
            .position(|binding| *binding == column.binding)
        {
            column.binding = ColumnBinding::new(new_table_index, position as u64);
        }
        return;
    }
    let _: Result<()> = expression.try_for_each_child_mut(|child| {
        rewrite_merge_bindings(child, source_bindings, new_table_index);
        Ok(())
    });
}

/// Rewrites the references of one operator's expressions (not its children's).
fn rewrite_operator_bindings(
    op: &mut LogicalOperator,
    source_bindings: &[ColumnBinding],
    new_table_index: u64,
) {
    op.for_each_expression_mut(|expression| {
        rewrite_merge_bindings(expression, source_bindings, new_table_index);
    });
}

/// The join operator below the filters/projections that binding the join produced.
fn join_operator(root: &mut LogicalOperator) -> Result<&mut LogicalOperator> {
    let mut current = root;
    while current.children.len() == 1 {
        current = &mut current.children[0];
    }
    if current.children.len() != 2 {
        return Err(Error::not_implemented(format!(
            "Expected a join after binding a join operator - but got a {}",
            current.operator_type().name()
        )));
    }
    Ok(current)
}

/// `MERGE INTO` (DuckDB's `Binder::Bind(MergeIntoStatement &)`).
#[allow(clippy::too_many_lines, reason = "mirrors Binder::Bind(MergeIntoStatement &)")]
pub fn bind_merge_into(scope: &mut BindScope<'_>, stmt: &mut MergeIntoStatement) -> Result<BoundStatement> {
    // The target table.
    let mut target_binder = scope.binder.child()?;
    let table_alias = stmt.target.alias.clone();
    let mut bound_table = {
        let mut target_scope = BindScope {
            binder: &mut target_binder,
            outer: Some(scope as &mut dyn Scope),
        };
        bind_table_ref(&mut target_scope, &mut stmt.target)?
    };
    let (entry, target_index) = match &bound_table.plan.kind {
        LogicalKind::Get(get) => (get.table.clone(), get.table_index),
        _ => (None, 0),
    };
    let entry: Arc<CatalogEntry> = entry.ok_or_else(|| Error::binder("Can only merge into base tables!"))?;
    let table = table_of(&entry)?;
    let table_name = entry.name().to_owned();

    // The source.
    let mut source_binder = scope.binder.child()?;
    let source_bound = {
        let mut source_scope = BindScope {
            binder: &mut source_binder,
            outer: Some(scope as &mut dyn Scope),
        };
        bind_table_ref(&mut source_scope, &mut stmt.source)?
    };
    let source: Vec<SourceColumn> = source_binder
        .bind_context
        .bindings()
        .iter()
        .flat_map(|binding| {
            binding
                .names
                .iter()
                .map(|name| (binding.alias.clone(), name.clone()))
        })
        .collect();

    let return_chunk = !stmt.returning_list.is_empty();
    let proj_index = scope.generate_table_index();
    let merge_table_index = scope.generate_table_index();
    let target = MergeTarget {
        table,
        table_name: &table_name,
        return_chunk,
        proj_index,
    };
    let mut projection_expressions = Vec::new();
    let mut actions: BTreeMap<MergeActionCondition, Vec<BoundMergeIntoAction>> = BTreeMap::new();

    // WHEN NOT MATCHED BY SOURCE / TARGET only see one side.
    for (&condition, condition_actions) in &mut stmt.actions {
        if condition == MergeActionCondition::WhenMatched {
            continue;
        }
        let side_binder = if condition == MergeActionCondition::WhenNotMatchedByTarget {
            &mut source_binder
        } else {
            &mut target_binder
        };
        let mut bound_actions = Vec::with_capacity(condition_actions.len());
        for action in condition_actions {
            check_merge_action(condition, action.action_type)?;
            let mut side_scope = BindScope {
                binder: &mut *side_binder,
                outer: Some(scope as &mut dyn Scope),
            };
            // The table's constraints are bound after these actions (as in DuckDB).
            bound_actions.push(bind_merge_action(
                &mut side_scope,
                &target,
                &[],
                &mut bound_table.plan,
                &mut projection_expressions,
                action,
                &source,
            )?);
        }
        actions.insert(condition, bound_actions);
    }

    // The join between source and target: unmatched source rows need an outer source
    // side, unmatched target rows an outer target side.
    let has_not_matched_by_source = stmt
        .actions
        .contains_key(&MergeActionCondition::WhenNotMatchedBySource);
    let has_not_matched_by_target = stmt
        .actions
        .contains_key(&MergeActionCondition::WhenNotMatchedByTarget);
    let join_type = match (has_not_matched_by_source, has_not_matched_by_target) {
        (true, true) => JoinType::Outer,
        (true, false) => JoinType::Right,
        (false, true) => JoinType::Left,
        (false, false) => JoinType::Inner,
    };
    let using_columns = if stmt.join_condition.is_some() {
        Vec::new()
    } else {
        std::mem::take(&mut stmt.using_columns)
    };
    let mut join_condition = stmt.join_condition.take();
    let bound_join = bind_bound_join(
        scope,
        JoinSpec {
            join_type,
            ref_type: JoinRefType::Regular,
            using_columns: &using_columns,
            condition: &mut join_condition,
            location: None,
        },
        (source_binder, source_bound),
        (target_binder, bound_table),
    )?;
    let mut root = bound_join.plan;

    // The table's defaults and constraints, which the actions may need.
    let bound_defaults = bind_default_values(scope, table)?;
    let bound_constraints = bind_constraints(scope, &table_name, table)?;

    // WHEN MATCHED sees both sides.
    if let Some(condition_actions) = stmt.actions.get_mut(&MergeActionCondition::WhenMatched) {
        let mut bound_actions = Vec::with_capacity(condition_actions.len());
        for action in condition_actions {
            check_merge_action(MergeActionCondition::WhenMatched, action.action_type)?;
            let get = find_get(&mut root, target_index)
                .ok_or_else(|| Error::internal("MERGE target scan not found"))?;
            bound_actions.push(bind_merge_action(
                scope,
                &target,
                &bound_constraints,
                get,
                &mut projection_expressions,
                action,
                &source,
            )?);
        }
        actions.insert(MergeActionCondition::WhenMatched, bound_actions);
    }

    // Subqueries of the actions.
    for expression in &mut projection_expressions {
        plan_subqueries(scope, expression, &mut root)?;
    }

    let mut merge = LogicalMergeInto {
        table: Arc::clone(&entry),
        table_index: merge_table_index,
        bound_defaults,
        row_id_start: 0,
        source_marker: None,
        bound_constraints,
        return_chunk,
        actions,
    };

    if has_not_matched_by_source {
        // A marker pushed into the source tells whether a row has a source match.
        let new_proj_index = scope.generate_table_index();
        let root_is_join = root.children.len() != 1;
        let join = join_operator(&mut root)?;
        let source_side = usize::from(table_references(&join.children[0]).contains(&target_index));
        let source_plan = &mut join.children[source_side];
        source_plan.resolve_operator_types()?;
        let source_bindings = source_plan.column_bindings();
        let mut select_list: Vec<Expression> = source_bindings
            .iter()
            .zip(&source_plan.types)
            .map(|(&binding, ty)| Expression::column_ref("", ty.clone(), binding, 0))
            .collect();
        let marker_binding = ColumnBinding::new(new_proj_index, select_list.len() as u64);
        let mut marker = Expression::constant(Value::integer(42));
        "source_marker".clone_into(&mut marker.alias);
        select_list.push(marker);
        let source_op = std::mem::replace(source_plan, LogicalOperator::new(LogicalKind::CrossProduct));
        *source_plan = LogicalOperator::projection(new_proj_index, select_list, source_op);

        for expression in &mut projection_expressions {
            rewrite_merge_bindings(expression, &source_bindings, new_proj_index);
        }
        rewrite_operator_bindings(join, &source_bindings, new_proj_index);
        if !root_is_join {
            rewrite_operator_bindings(&mut root, &source_bindings, new_proj_index);
        }
        for action in merge.actions.values_mut().flatten() {
            if let Some(condition) = &mut action.condition {
                rewrite_merge_bindings(condition, &source_bindings, new_proj_index);
            }
            for expression in &mut action.expressions {
                rewrite_merge_bindings(expression, &source_bindings, new_proj_index);
            }
        }
        merge.source_marker = Some(projection_expressions.len());
        let mut marker_ref = Expression::column_ref("source_marker", LogicalType::INTEGER, marker_binding, 0);
        "source_marker".clone_into(&mut marker_ref.alias);
        projection_expressions.push(marker_ref);
    }

    // The target's row id ends the projection (`BindRowIdColumns`).
    merge.row_id_start = projection_expressions.len();
    let get =
        find_get(&mut root, target_index).ok_or_else(|| Error::internal("MERGE target scan not found"))?;
    projection_expressions.push(bind_row_id_column(get)?);

    let projection = LogicalOperator::projection(proj_index, projection_expressions, root);
    let plan = LogicalOperator::new(LogicalKind::MergeInto(Box::new(merge))).with_children(vec![projection]);

    if return_chunk {
        let returning = std::mem::take(&mut stmt.returning_list);
        return bind_returning_with_virtual(
            scope,
            returning,
            &table_name,
            &entry,
            &table_alias,
            merge_table_index,
            plan,
            Some(("merge_action", LogicalType::VARCHAR)),
        );
    }
    scope.with_properties(|properties| properties.return_type = StatementReturnType::ChangedRows);
    Ok(BoundStatement::new(
        plan,
        vec!["Count".to_owned()],
        vec![LogicalType::BIGINT],
    ))
}

/// A parsed `relation.column` reference.
fn qualified_column(relation: &str, column: &str) -> ParsedExpression {
    ParsedExpression::column_ref([relation.to_owned(), column.to_owned()])
}

/// `DoUpdateSetQualify`: unqualified column references in `DO UPDATE SET` refer to the
/// target table, so they are qualified with it to avoid ambiguity with `excluded`.
fn do_update_set_qualify(
    expr: &mut ParsedExpression,
    table_name: &str,
    lambda_params: &mut Vec<HashSet<String>>,
) -> Result<()> {
    match &mut expr.kind {
        ParsedExpressionKind::ColumnRef(column) => {
            if column.column_names.len() > 1 {
                return Ok(());
            }
            let name = column.column_names[0].clone();
            if lambda_params.iter().any(|level| level.contains(&name)) {
                return Ok(());
            }
            *expr = qualified_column(table_name, &name);
            return Ok(());
        }
        ParsedExpressionKind::Function(function) if is_lambda_function(function) => {
            for child in &mut function.children {
                let ParsedExpressionKind::Lambda(lambda) = &mut child.kind else {
                    do_update_set_qualify(child, table_name, lambda_params)?;
                    continue;
                };
                let Some(parameters) = rustdb_ast::lambda_parameters(lambda) else {
                    // Possibly a JSON function: qualify both sides.
                    for_each_child(&mut lambda.lhs, &mut |child| {
                        do_update_set_qualify(child, table_name, lambda_params)
                    })?;
                    for_each_child(&mut lambda.expr, &mut |child| {
                        do_update_set_qualify(child, table_name, lambda_params)
                    })?;
                    continue;
                };
                lambda_params.push(parameters.iter().map(|parameter| parameter.name()).collect());
                for_each_child(&mut lambda.expr, &mut |child| {
                    do_update_set_qualify(child, table_name, lambda_params)
                })?;
                lambda_params.pop();
            }
            return Ok(());
        }
        ParsedExpressionKind::Subquery(_) => {
            return Err(Error::binder("DO UPDATE SET clause cannot contain a subquery"));
        }
        _ => {}
    }
    for_each_child(expr, &mut |child| {
        do_update_set_qualify(child, table_name, lambda_params)
    })
}

/// The physical columns of each unique index of `table`, in index order.
fn unique_index_columns(table: &TableEntry) -> Vec<HashSet<usize>> {
    table.storage.as_ref().map_or_else(Vec::new, |storage| {
        storage
            .indexes()
            .entries()
            .iter()
            .filter(|index| index.is_unique())
            .map(|index| index.column_ids().iter().copied().collect())
            .collect()
    })
}

/// `CreateSetInfoForReplace`: `INSERT OR REPLACE` updates every inserted column that is
/// not part of a unique key to its `excluded` value.
fn create_set_info_for_replace(
    table: &TableEntry,
    insert: &InsertStatement,
    unique: &[HashSet<usize>],
) -> UpdateSetInfo {
    let conflict: HashSet<usize> = unique.iter().flatten().copied().collect();
    let mut columns = Vec::new();
    if insert.columns.is_empty() {
        for (physical, _, column) in table.physical_columns() {
            if !conflict.contains(&physical) {
                columns.push(column.name.clone());
            }
        }
    } else {
        for name in &insert.columns {
            let physical = table
                .column(name)
                .and_then(|(logical, _)| table.logical_to_physical(logical));
            if physical.is_some_and(|physical| conflict.contains(&physical)) {
                continue;
            }
            columns.push(name.clone());
        }
    }
    let expressions = columns
        .iter()
        .map(|column| qualified_column("excluded", column))
        .collect();
    UpdateSetInfo {
        condition: None,
        columns,
        expressions,
    }
}

/// A `SELECT ... FROM source` wrapped as the subquery `excluded`.
fn excluded_subquery(
    select_list: Vec<ParsedExpression>,
    from: TableRef,
    modifiers: Vec<ResultModifier>,
) -> TableRef {
    let node = QueryNode {
        modifiers,
        cte_map: CommonTableExpressionMap::default(),
        kind: QueryNodeKind::Select(Box::new(SelectNode {
            select_list,
            from_table: Box::new(from),
            where_clause: None,
            group_expressions: Vec::new(),
            group_sets: Vec::new(),
            aggregate_handling: AggregateHandling::StandardHandling,
            having: None,
            sample: None,
            qualify: None,
        })),
    };
    subquery_ref(
        SelectStatement {
            node: Box::new(node),
            named_param_map: CaseInsensitiveMap::default(),
        },
        Vec::new(),
    )
}

fn subquery_ref(subquery: SelectStatement, column_name_alias: Vec<String>) -> TableRef {
    TableRef {
        alias: "excluded".to_owned(),
        sample: None,
        query_location: None,
        kind: TableRefKind::Subquery(SubqueryRef {
            subquery: Box::new(subquery),
            column_name_alias,
        }),
    }
}

/// Sets the column aliases of a source table ref (`TableRef::column_name_alias`).
fn column_name_alias(source: &mut TableRef) -> Option<&mut Vec<String>> {
    match &mut source.kind {
        TableRefKind::Subquery(subquery) => Some(&mut subquery.column_name_alias),
        TableRefKind::BaseTable(base) => Some(&mut base.column_name_alias),
        _ => None,
    }
}

fn star() -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::Star,
        ParsedExpressionKind::Star(Box::new(StarExpression {
            relation_name: String::new(),
            exclude_list: Vec::new(),
            replace_list: CaseInsensitiveMap::default(),
            rename_list: rustdb_common::OrderedMap::default(),
            columns: false,
            expr: None,
        })),
    )
}

/// `left = right`, AND-ed or OR-ed lists.
fn comparison(left: ParsedExpression, right: ParsedExpression) -> ParsedExpression {
    ParsedExpression::new(
        ExpressionType::CompareEqual,
        ParsedExpressionKind::Comparison(ComparisonExpression {
            left: Box::new(left),
            right: Box::new(right),
        }),
    )
}

fn conjunction(expression_type: ExpressionType, mut children: Vec<ParsedExpression>) -> ParsedExpression {
    if children.len() == 1 {
        return children.remove(0);
    }
    ParsedExpression::new(
        expression_type,
        ParsedExpressionKind::Conjunction(ConjunctionExpression { children }),
    )
}

/// `Binder::GenerateMergeInto`: the MERGE INTO statement equivalent to an
/// `INSERT ... ON CONFLICT`.
#[allow(clippy::too_many_lines, reason = "mirrors Binder::GenerateMergeInto")]
pub fn generate_merge_into(stmt: &mut InsertStatement, entry: &CatalogEntry) -> Result<MergeIntoStatement> {
    let table = table_of(entry)?;
    let mut on_conflict_info = stmt
        .on_conflict_info
        .take()
        .ok_or_else(|| Error::internal("GenerateMergeInto without ON CONFLICT"))?;
    let target = match stmt.table_ref.take() {
        Some(target) => target,
        None => Box::new(TableRef {
            alias: String::new(),
            sample: None,
            query_location: None,
            kind: TableRefKind::BaseTable(Box::new(BaseTableRef {
                schema_name: stmt.schema.clone(),
                table_name: stmt.table.clone(),
                column_name_alias: Vec::new(),
                catalog_name: stmt.catalog.clone(),
                at_clause: None,
            })),
        }),
    };
    let table_name = if target.alias.is_empty() {
        stmt.table.clone()
    } else {
        target.alias.clone()
    };
    let unique = unique_index_columns(table);

    // The join between the target and `excluded`.
    let mut all_distinct_on_columns: Vec<Vec<String>> = Vec::new();
    let mut join_condition = None;
    let mut using_columns = Vec::new();
    if on_conflict_info.indexed_columns.is_empty() {
        // Without a conflict target, match on any unique key.
        let mut join_conditions = Vec::new();
        let mut found_matching_indexes = 0;
        for indexed_columns in &unique {
            let mut and_children = Vec::new();
            let mut distinct_on_columns = Vec::new();
            for (physical, _, column) in table.physical_columns() {
                if !indexed_columns.contains(&physical) {
                    continue;
                }
                and_children.push(comparison(
                    qualified_column(&table_name, &column.name),
                    qualified_column("excluded", &column.name),
                ));
                distinct_on_columns.push(column.name.clone());
            }
            all_distinct_on_columns.push(distinct_on_columns);
            if and_children.is_empty() {
                continue;
            }
            join_conditions.push(conjunction(ExpressionType::ConjunctionAnd, and_children));
            found_matching_indexes += 1;
        }
        if found_matching_indexes == 0 {
            return Err(Error::binder(
                "There are no UNIQUE/PRIMARY KEY constraints that refer to this table, specify ON CONFLICT \
                 columns manually",
            ));
        }
        if found_matching_indexes != 1 && on_conflict_info.action_type != OnConflictAction::Nothing {
            return Err(Error::binder(
                "Conflict target has to be provided for a DO UPDATE operation when the table has multiple \
                 UNIQUE/PRIMARY KEY constraints",
            ));
        }
        join_condition = Some(Box::new(conjunction(
            ExpressionType::ConjunctionOr,
            join_conditions,
        )));
    } else {
        let mut on_conflict_filter = HashSet::new();
        for name in &on_conflict_info.indexed_columns {
            let Some((logical, column)) = table.column(name) else {
                if name.eq_ignore_ascii_case("rowid") {
                    return Err(Error::binder("Cannot specify ROWID as ON CONFLICT target"));
                }
                return Err(super::table::missing_column_error(table, &stmt.table, name));
            };
            if column.generated.is_some() {
                return Err(Error::binder(
                    "Cannot specify a generated column as ON CONFLICT target",
                ));
            }
            if let Some(physical) = table.logical_to_physical(logical) {
                on_conflict_filter.insert(physical);
            }
        }
        if !unique.contains(&on_conflict_filter) {
            return Err(Error::binder(
                "The specified columns as conflict target are not referenced by a UNIQUE/PRIMARY KEY CONSTRAINT \
                 or INDEX",
            ));
        }
        all_distinct_on_columns.push(on_conflict_info.indexed_columns.clone());
        using_columns = std::mem::take(&mut on_conflict_info.indexed_columns);
    }

    // DEFAULT in a VALUES list.
    if super::modify::values_list(stmt).is_some() {
        let mut named_column_map = Vec::new();
        if stmt.columns.is_empty() {
            named_column_map.extend(table.physical_columns().map(|(_, logical, _)| logical));
        } else {
            for name in &stmt.columns {
                let (logical, _) = table
                    .column(name)
                    .ok_or_else(|| super::table::missing_column_error(table, &stmt.table, name))?;
                named_column_map.push(logical);
            }
        }
        expand_default_in_values_list(stmt, table, &stmt.table.clone(), &named_column_map)?;
    }

    // The source `excluded`.
    let mut source = match stmt.select_statement.take() {
        Some(select) => subquery_ref(*select, Vec::new()),
        None => TableRef {
            alias: String::new(),
            sample: None,
            query_location: None,
            kind: TableRefKind::Empty,
        },
    };
    if stmt.column_order == InsertColumnOrder::InsertByPosition {
        if !stmt.columns.is_empty() || stmt.default_values {
            // Only some columns are inserted: the others get their defaults.
            if let Some(aliases) = column_name_alias(&mut source) {
                aliases.clone_from(&stmt.columns);
            }
            let set_columns: HashSet<String> = stmt.columns.iter().map(|c| c.to_lowercase()).collect();
            let mut select_list = Vec::new();
            for (_, _, column) in table.physical_columns() {
                select_list.push(if set_columns.contains(&column.name.to_lowercase()) {
                    ParsedExpression::column_ref([column.name.clone()])
                } else {
                    column.default_value.clone().unwrap_or_else(|| {
                        ParsedExpression::constant(Value::null(column.logical_type.clone()))
                    })
                });
            }
            source = excluded_subquery(select_list, source, Vec::new());
        }
        if let Some(aliases) = column_name_alias(&mut source) {
            aliases.extend(table.physical_columns().map(|(_, _, column)| column.name.clone()));
        }
    }
    // DISTINCT ON each key: one row per key value is inserted or updated.
    for distinct_on_columns in all_distinct_on_columns {
        let distinct = DistinctModifier {
            distinct_on_targets: distinct_on_columns
                .into_iter()
                .map(|column| ParsedExpression::column_ref([column]))
                .collect(),
        };
        source = excluded_subquery(vec![star()], source, vec![ResultModifier::Distinct(distinct)]);
    }

    if on_conflict_info.action_type == OnConflictAction::Replace {
        if stmt.column_order != InsertColumnOrder::InsertByName {
            on_conflict_info.set_info = Some(Box::new(create_set_info_for_replace(table, stmt, &unique)));
        }
        on_conflict_info.action_type = OnConflictAction::Update;
    }
    let mut actions = BTreeMap::new();
    actions.insert(
        MergeActionCondition::WhenNotMatchedByTarget,
        vec![MergeIntoAction {
            action_type: MergeActionType::MergeInsert,
            condition: None,
            update_info: None,
            insert_columns: Vec::new(),
            expressions: Vec::new(),
            column_order: stmt.column_order,
            default_values: false,
            exclude_columns: std::collections::BTreeSet::new(),
        }],
    );
    if on_conflict_info.condition.is_some() {
        return Err(Error::binder(
            "ON CONFLICT WHERE clause is only supported in DO UPDATE SET ... WHERE ...\nThe WHERE clause after \
             the conflict columns is used for partial indexes which are not supported.",
        ));
    }
    if on_conflict_info.action_type == OnConflictAction::Update {
        let conflict: HashSet<usize> = unique.iter().flatten().copied().collect();
        let exclude_columns = table
            .physical_columns()
            .filter(|(physical, _, _)| conflict.contains(physical))
            .map(|(_, _, column)| column.name.clone())
            .collect();
        let mut update_action = MergeIntoAction {
            action_type: MergeActionType::MergeUpdate,
            condition: None,
            update_info: None,
            insert_columns: Vec::new(),
            expressions: Vec::new(),
            column_order: stmt.column_order,
            default_values: false,
            exclude_columns,
        };
        if let Some(mut set_info) = on_conflict_info.set_info.take() {
            for expression in &mut set_info.expressions {
                do_update_set_qualify(expression, &table_name, &mut Vec::new())?;
            }
            if let Some(mut condition) = set_info.condition.take() {
                do_update_set_qualify(&mut condition, &table_name, &mut Vec::new())?;
                update_action.condition = Some(condition);
            }
            update_action.update_info = Some(set_info);
        }
        actions.insert(MergeActionCondition::WhenMatched, vec![update_action]);
    }
    Ok(MergeIntoStatement {
        target,
        source: Box::new(source),
        join_condition,
        using_columns,
        actions,
        returning_list: std::mem::take(&mut stmt.returning_list),
        cte_map: std::mem::take(&mut stmt.cte_map),
    })
}
