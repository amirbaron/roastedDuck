//! `PIVOT` / `UNPIVOT` (DuckDB's `bind_pivot.cpp`): both are rewritten into ordinary
//! query nodes and bound as a subquery.
//!
//! - PIVOT with few output columns becomes one filtered aggregate per pivot value
//!   (`sum(x) FILTER (col::VARCHAR IS NOT DISTINCT FROM 'v')`).
//! - PIVOT with many output columns aggregates per (group, pivot value), collects the
//!   aggregates per group with `list`, and spreads the lists into columns with a PIVOT
//!   operator.
//! - UNPIVOT builds one list of values per row with `unpivot_list` and unnests it next to
//!   the list of column names.

use std::collections::{HashMap, HashSet};

use rustdb_ast::visit::{VisitorMut, walk_expression};
use rustdb_ast::{
    AggregateHandling, BoundPivotSpec, CommonTableExpressionMap, ExpressionType, ParsedExpression,
    ParsedExpressionKind, PivotColumnEntry, PivotRef, PivotValueElement, QueryNode, QueryNodeKind,
    SelectNode, SelectStatement, SubqueryRef, TableRef, TableRefKind,
};
use rustdb_catalog::CatalogType;
use rustdb_common::{ByteString, CaseInsensitiveMap, Error, Result};
use rustdb_function::cast::ValueCast;
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use super::{bind_subquery_ref, bind_table_ref, deduplicate_columns};
use crate::binder::expression::expand_star_expression;
use crate::binder::{BindScope, Binding, BoundStatement, DUMMY_NAME};
use crate::expression_binder::{ConstantBinder, ExpressionBinder, parameter_not_resolved};
use crate::operator::{BoundPivotInfo, LogicalKind, LogicalOperator};

/// Binds a PIVOT or UNPIVOT table ref (DuckDB's `Binder::Bind(PivotRef &)`).
pub(super) fn bind_pivot(
    scope: &mut BindScope<'_>,
    pivot: &mut PivotRef,
    alias: &[u8],
) -> Result<BoundStatement> {
    if pivot.bound.is_some() {
        return bind_bound_pivot(scope, pivot, alias);
    }
    // The source is bound once up front so that its columns (and `*`) can be expanded.
    if matches!(pivot.source.kind, TableRefKind::Subquery(_)) && pivot.source.alias.is_empty() {
        pivot.source.alias =
            rustdb_common::byte_format!("__internal_pivot_alias_{}", scope.generate_table_index());
    }
    let mut star_binder = scope.binder.child()?;
    let mut where_clause = None;
    let (unpivoted, all_columns) = {
        let mut star_scope = BindScope {
            binder: &mut star_binder,
            outer: Some(&mut *scope),
        };
        let mut copied_source = (*pivot.source).clone();
        bind_table_ref(&mut star_scope, &mut copied_source)?;
        let mut all_columns = Vec::new();
        expand_star_expression(&mut star_scope, ParsedExpression::star(), &mut all_columns)?;
        if pivot.aggregates.is_empty() {
            let node = bind_unpivot(&mut star_scope, pivot, all_columns, &mut where_clause)?;
            (Some(node), Vec::new())
        } else {
            (None, all_columns)
        }
    };
    let select_node = match unpivoted {
        Some(node) => node,
        None => bind_pivot_node(scope, pivot, all_columns)?,
    };

    let subquery_alias = if alias.is_empty() {
        ByteString::from("__unnamed_pivot")
    } else {
        ByteString::from(alias)
    };
    let column_name_alias = std::mem::take(&mut pivot.column_name_alias);
    let mut subquery = subquery_ref(select_node, column_name_alias.clone());
    let Some(where_clause) = where_clause else {
        return bind_subquery_ref(scope, &mut subquery, &subquery_alias);
    };
    // The WHERE clause of EXCLUDE NULLS applies after the unnest: wrap the query.
    let mut source = table_ref(TableRefKind::Subquery(subquery));
    source.alias.clone_from(&subquery_alias);
    let mut outer = select_node_from(source);
    outer.select_list.push(ParsedExpression::star());
    outer.where_clause = Some(Box::new(where_clause));
    let mut wrapped = subquery_ref(outer, column_name_alias);
    bind_subquery_ref(scope, &mut wrapped, &subquery_alias)
}

fn table_ref(kind: TableRefKind) -> TableRef {
    TableRef {
        external_dependency: rustdb_ast::ExternalDependency::default(),
        alias: ByteString::new(),
        sample: None,
        query_location: None,
        kind,
    }
}

fn select_node_from(from_table: TableRef) -> SelectNode {
    SelectNode {
        select_list: Vec::new(),
        from_table: Box::new(from_table),
        where_clause: None,
        group_expressions: Vec::new(),
        group_sets: Vec::new(),
        aggregate_handling: AggregateHandling::StandardHandling,
        having: None,
        sample: None,
        qualify: None,
    }
}

fn subquery_ref(node: SelectNode, column_name_alias: Vec<ByteString>) -> SubqueryRef {
    SubqueryRef {
        subquery: Box::new(SelectStatement {
            node: Box::new(QueryNode {
                modifiers: Vec::new(),
                cte_map: CommonTableExpressionMap::default(),
                kind: QueryNodeKind::Select(Box::new(node)),
            }),
            named_param_map: CaseInsensitiveMap::default(),
        }),
        column_name_alias,
    }
}

/// Adds `expression` to the select list of `node` and groups by its position.
fn push_group(node: &mut SelectNode, expression: ParsedExpression) {
    let position = node.select_list.len() as i32 + 1;
    node.group_expressions
        .push(ParsedExpression::constant(Value::integer(position)));
    node.select_list.push(expression);
}

/// DuckDB's `Value::ToByteString`.
fn value_name(value: &Value) -> Result<ByteString> {
    value.to_varchar_bytes()
}

/// Visits the children of an expression without entering subqueries
/// (`ParsedExpressionIterator::EnumerateChildren`).
struct ExpressionsOnly<F: FnMut(&mut ParsedExpression) -> Result<bool>> {
    callback: F,
    error: Option<Error>,
}

impl<F: FnMut(&mut ParsedExpression) -> Result<bool>> VisitorMut for ExpressionsOnly<F> {
    fn visit_expression(&mut self, expression: &mut ParsedExpression) {
        if self.error.is_some() {
            return;
        }
        match (self.callback)(expression) {
            Ok(true) => walk_expression(self, expression),
            Ok(false) => {}
            Err(error) => self.error = Some(error),
        }
    }
    fn visit_query_node(&mut self, _: &mut QueryNode) {}
    fn visit_table_ref(&mut self, _: &mut TableRef) {}
}

/// Calls `callback` on `expression` and its descendants outside subqueries; it returns
/// whether to continue into the children.
fn visit_expressions(
    expression: &mut ParsedExpression,
    callback: impl FnMut(&mut ParsedExpression) -> Result<bool>,
) -> Result<()> {
    let mut visitor = ExpressionsOnly {
        callback,
        error: None,
    };
    visitor.visit_expression(expression);
    visitor.error.map_or(Ok(()), Err)
}

/// Records the columns referenced by a pivot or aggregate expression
/// (`ExtractPivotExpressions`).
fn extract_pivot_columns(
    expression: &mut ParsedExpression,
    handled: &mut HashSet<ByteString>,
    macro_binding: Option<&Binding>,
) -> Result<()> {
    visit_expressions(expression, |child| {
        if let ParsedExpressionKind::ColumnRef(column) = &child.kind {
            if column.column_names.len() > 1 {
                let name = column
                    .column_names
                    .last()
                    .map_or(b"".as_slice(), ByteString::as_bytes);
                if column.column_names[0]
                    .windows(DUMMY_NAME.len())
                    .any(|part| part == DUMMY_NAME.as_bytes())
                    && macro_binding.is_some_and(|binding| binding.has_matching_binding(name))
                {
                    // A macro parameter: bind once the macro is called.
                    return Err(parameter_not_resolved());
                }
                return Err(Error::binder("PIVOT expression cannot contain qualified columns")
                    .with_position(child.query_location));
            }
            if let Some(name) = column.column_names.last() {
                handled.insert(name.to_ascii_lowercase());
            }
        }
        Ok(true)
    })
}

/// Whether `expression` is a call of an aggregate function (looked up like DuckDB's
/// `Catalog::GetEntry` for `AGGREGATE_FUNCTION_ENTRY`).
fn is_aggregate_call(scope: &BindScope<'_>, expression: &ParsedExpression) -> Result<bool> {
    let ParsedExpressionKind::Function(function) = &expression.kind else {
        return Ok(false);
    };
    let entry = scope.context.get_entry(
        &function.catalog,
        &function.schema,
        CatalogType::AggregateFunction,
        &function.function_name,
        false,
    )?;
    Ok(entry.is_some_and(|lookup| lookup.entry.catalog_type() == CatalogType::AggregateFunction))
}

/// The aggregate calls in `expression`, outside other aggregates
/// (`ExtractPivotAggregateExpression`). Column references outside an aggregate are an
/// error.
fn count_pivot_aggregates(scope: &BindScope<'_>, expression: &mut ParsedExpression) -> Result<usize> {
    let mut count = 0;
    visit_expressions(expression, |child| {
        if is_aggregate_call(scope, child)? {
            count += 1;
            return Ok(false);
        }
        if matches!(child.kind, ParsedExpressionKind::ColumnRef(_)) {
            return Err(Error::binder(
                "Columns can only be referenced within the aggregate of a PIVOT expression",
            )
            .with_position(child.query_location));
        }
        Ok(true)
    })?;
    Ok(count)
}

/// Applies `edit` to the single aggregate call in `expression`
/// (`TemplatedHandlePivotAggregate`).
fn with_pivot_aggregate(
    scope: &BindScope<'_>,
    expression: &mut ParsedExpression,
    mut edit: impl FnMut(&mut ParsedExpression),
) -> Result<()> {
    visit_expressions(expression, |child| {
        if is_aggregate_call(scope, child)? {
            edit(child);
            return Ok(false);
        }
        Ok(true)
    })
}

/// The pivot output columns: every combination of the pivot columns' entries
/// (`ConstructPivots`).
fn construct_pivots(
    pivot: &PivotRef,
    pivot_index: usize,
    current: &PivotValueElement,
    result: &mut Vec<PivotValueElement>,
) -> Result<()> {
    let column = &pivot.pivots[pivot_index];
    let last = pivot_index + 1 == pivot.pivots.len();
    for entry in &column.entries {
        let mut value = current.clone();
        let mut name = entry.alias.clone();
        for entry_value in &entry.values {
            value.values.push(entry_value.clone());
            if entry.alias.is_empty() {
                let text = value_name(entry_value)?;
                if name.is_empty() {
                    name = text;
                } else {
                    name = rustdb_common::byte_format!("{name}_{text}");
                }
            }
        }
        value.name = if current.name.is_empty() {
            name
        } else {
            rustdb_common::byte_format!("{}_{name}", current.name)
        };
        if last {
            result.push(value);
        } else {
            construct_pivots(pivot, pivot_index + 1, &value, result)?;
        }
    }
    Ok(())
}

/// The values of a `PIVOT ... IN (...)` entry (`BindPivotInList`).
fn bind_pivot_in_list(
    scope: &mut BindScope<'_>,
    expression: &mut ParsedExpression,
    values: &mut Vec<Value>,
) -> Result<()> {
    match &mut expression.kind {
        ParsedExpressionKind::ColumnRef(column) => {
            if column.column_names.len() > 1 {
                return Err(
                    Error::binder("PIVOT IN list cannot contain qualified column references")
                        .with_position(expression.query_location),
                );
            }
            values.push(Value::try_varchar(
                column
                    .column_names
                    .last()
                    .map_or(b"".as_slice(), ByteString::as_bytes),
            )?);
        }
        ParsedExpressionKind::Function(function) => {
            if function.function_name != "row" {
                return Err(
                    Error::binder("PIVOT IN list must contain columns or lists of columns")
                        .with_position(expression.query_location),
                );
            }
            for child in &mut function.children {
                bind_pivot_in_list(scope, child, values)?;
            }
        }
        _ => {
            let location = expression.query_location;
            let bound = {
                let (inner, outer) = scope.split();
                let mut binder = ConstantBinder::new(inner, outer, "PIVOT IN list");
                binder.bind(expression, true)?
            };
            if !bound.is_foldable() {
                return Err(
                    Error::binder("PIVOT IN list must contain constant expressions").with_position(location)
                );
            }
            values.push(scope.context.evaluator.evaluate_scalar(&bound)?);
        }
    }
    Ok(())
}

/// The first stage shared by both PIVOT strategies: the source grouped by the columns
/// that are neither pivoted nor aggregated, or by the explicit `GROUP BY` columns
/// (`ConstructInitialGrouping`).
fn construct_initial_grouping(
    pivot: &mut PivotRef,
    all_columns: Vec<ParsedExpression>,
    handled: &HashSet<ByteString>,
) -> Result<SelectNode> {
    let source = std::mem::replace(&mut pivot.source, Box::new(table_ref(TableRefKind::Empty)));
    let mut node = select_node_from(*source);
    if pivot.groups.is_empty() {
        for column in all_columns {
            // `GetResolvedColumnExpression`: a USING column of a FULL JOIN is a COALESCE
            // of the joined columns.
            let Some(name) = crate::binder::expression::bind_star::resolved_column_name(&column) else {
                return Err(Error::internal(
                    "Unexpected child of pivot source - not a ColumnRef",
                ));
            };
            if !handled.contains(&name.to_ascii_lowercase()) {
                push_group(&mut node, ParsedExpression::column_ref([name]));
            }
        }
    } else {
        for group in &pivot.groups {
            push_group(&mut node, ParsedExpression::column_ref([group.clone()]));
        }
    }
    Ok(node)
}

/// One filtered aggregate per pivot value and aggregate (`PivotFilteredAggregate`).
fn pivot_filtered_aggregate(
    scope: &BindScope<'_>,
    pivot: &mut PivotRef,
    all_columns: Vec<ParsedExpression>,
    handled: &HashSet<ByteString>,
    pivot_values: &[PivotValueElement],
) -> Result<SelectNode> {
    let mut node = construct_initial_grouping(pivot, all_columns, handled)?;
    for pivot_value in pivot_values {
        let mut filter: Option<ParsedExpression> = None;
        let mut value_index = 0;
        for column in &pivot.pivots {
            for pivot_expression in &column.pivot_expressions {
                let left = ParsedExpression::cast(pivot_expression.clone(), LogicalType::VARCHAR);
                let value = pivot_value.values[value_index].default_cast_as(&LogicalType::VARCHAR)?;
                value_index += 1;
                let comparison = ParsedExpression::comparison(
                    ExpressionType::CompareNotDistinctFrom,
                    left,
                    ParsedExpression::constant(value),
                );
                filter = Some(match filter {
                    Some(filter) => ParsedExpression::and(filter, comparison),
                    None => comparison,
                });
            }
        }
        for aggregate in &pivot.aggregates {
            let mut copied = aggregate.clone();
            with_pivot_aggregate(scope, &mut copied, |call| {
                if let ParsedExpressionKind::Function(function) = &mut call.kind {
                    function.filter = filter.clone().map(Box::new);
                }
            })?;
            let mut name = pivot_value.name.clone();
            if pivot.aggregates.len() > 1 || !aggregate.alias.is_empty() {
                // With several aggregates, the column names carry the aggregate's name too.
                let aggregate_name = if aggregate.alias.is_empty() {
                    aggregate.name()
                } else {
                    aggregate.alias.clone()
                };
                name = rustdb_common::byte_format!("{name}_{aggregate_name}");
            }
            copied.alias = name;
            node.select_list.push(copied);
        }
    }
    Ok(node)
}

/// Names produced while rewriting a PIVOT into the LIST-based plan (`PivotBindState`).
#[derive(Default)]
struct PivotBindState {
    internal_groups: Vec<ByteString>,
    group_names: Vec<ByteString>,
    aggregate_names: Vec<ByteString>,
    internal_aggregates: Vec<ByteString>,
}

/// Stage 1: `SELECT {groups}, {pivots}, {aggregates} FROM source GROUP BY {groups},
/// {pivots}` (`PivotInitialAggregate`). The pivot expressions and aggregates of `pivot`
/// are replaced by references to this stage's columns.
fn pivot_initial_aggregate(
    scope: &BindScope<'_>,
    state: &mut PivotBindState,
    pivot: &mut PivotRef,
    all_columns: Vec<ParsedExpression>,
    handled: &HashSet<ByteString>,
) -> Result<SelectNode> {
    let mut node = construct_initial_grouping(pivot, all_columns, handled)?;
    for (count, expression) in node.select_list.iter_mut().enumerate() {
        state.group_names.push(expression.name());
        if expression.alias.is_empty() {
            expression.alias = rustdb_common::byte_format!("__internal_pivot_group{}", count + 1);
        }
        state.internal_groups.push(expression.alias.clone());
    }
    let mut pivot_count = 0;
    for column in &mut pivot.pivots {
        for pivot_expression in &mut column.pivot_expressions {
            if pivot_expression.alias.is_empty() {
                pivot_count += 1;
                pivot_expression.alias = rustdb_common::byte_format!("__internal_pivot_ref{pivot_count}");
            }
            let alias = pivot_expression.alias.clone();
            let expression = std::mem::replace(pivot_expression, ParsedExpression::column_ref([alias]));
            push_group(&mut node, expression);
        }
    }
    let aggregate_count = pivot.aggregates.len();
    for (index, aggregate) in pivot.aggregates.iter_mut().enumerate() {
        let internal_name = rustdb_common::byte_format!("__internal_pivot_aggregate{}", index + 1);
        let mut name = aggregate.alias.clone();
        if name.is_empty() && aggregate_count > 1 {
            name = aggregate.to_sql_bytes();
        }
        if !name.is_empty() {
            name = rustdb_common::byte_format!("_{name}");
        }
        // The aggregate call moves into stage 1; the expression keeps a reference to it.
        let mut call = None;
        let reference = ParsedExpression::column_ref([internal_name.clone()]);
        with_pivot_aggregate(scope, aggregate, |found| {
            if call.is_none() {
                call = Some(std::mem::replace(found, reference.clone()));
            }
        })?;
        let call = call.ok_or_else(|| Error::internal("pivot aggregate disappeared"))?;
        state.aggregate_names.push(name);
        state.internal_aggregates.push(internal_name.clone());
        node.select_list.push(call.with_alias(internal_name));
    }
    Ok(node)
}

/// Stage 2: `SELECT {groups}, list({aggregates}), list({pivot names}) FROM stage1 GROUP
/// BY {groups}` (`PivotListAggregate`).
fn pivot_list_aggregate(state: &PivotBindState, pivot: &mut PivotRef, stage1: SelectNode) -> SelectNode {
    let mut source = table_ref(TableRefKind::Subquery(subquery_ref(stage1, Vec::new())));
    source.alias = ByteString::new();
    let mut node = select_node_from(source);
    for name in &state.internal_groups {
        push_group(
            &mut node,
            ParsedExpression::column_ref([name.clone()]).with_alias(name.clone()),
        );
    }
    for name in &state.internal_aggregates {
        let list = ParsedExpression::function("list", vec![ParsedExpression::column_ref([name.clone()])]);
        node.select_list.push(list.with_alias(name.clone()));
    }
    // list(coalesce(p1::VARCHAR, 'NULL') || '_' || ...)
    let mut name_expression: Option<ParsedExpression> = None;
    for column in &mut pivot.pivots {
        for pivot_expression in column.pivot_expressions.drain(..) {
            let coalesce = ParsedExpression::operator(
                ExpressionType::OperatorCoalesce,
                vec![
                    ParsedExpression::cast(pivot_expression, LogicalType::VARCHAR),
                    ParsedExpression::constant(Value::varchar("NULL")),
                ],
            );
            name_expression = Some(match name_expression {
                None => coalesce,
                Some(previous) => ParsedExpression::function(
                    "concat",
                    vec![
                        previous,
                        ParsedExpression::constant(Value::varchar("_")),
                        coalesce,
                    ],
                ),
            });
        }
    }
    let names = ParsedExpression::function("list", name_expression.into_iter().collect());
    node.select_list.push(names.with_alias("__internal_pivot_name"));
    node
}

/// Renames every column reference in `expression` to `name` (`ReplacePivotColumnRef`).
fn replace_pivot_column_ref(expression: &mut ParsedExpression, name: &[u8]) -> Result<()> {
    visit_expressions(expression, |child| {
        if let ParsedExpressionKind::ColumnRef(column) = &mut child.kind
            && let Some(first) = column.column_names.first_mut()
        {
            *first = name.into();
        }
        Ok(true)
    })
}

/// Stage 3: the PIVOT operator over stage 2, and the aggregate expressions over its
/// columns (`PivotFinalOperator`).
fn pivot_final_operator(
    state: PivotBindState,
    pivot: &PivotRef,
    stage2: SelectNode,
    pivot_values: Vec<PivotValueElement>,
) -> Result<SelectNode> {
    let mut aggregate_names = Vec::new();
    for pivot_value in &pivot_values {
        for name in &state.aggregate_names {
            aggregate_names.push(rustdb_common::byte_format!("{}{name}", pivot_value.name));
        }
    }
    deduplicate_columns(&mut aggregate_names);
    let source = table_ref(TableRefKind::Subquery(subquery_ref(stage2, Vec::new())));
    let bound_pivot = PivotRef {
        source: Box::new(source),
        aggregates: Vec::new(),
        unpivot_names: Vec::new(),
        pivots: Vec::new(),
        groups: Vec::new(),
        column_name_alias: Vec::new(),
        include_nulls: false,
        bound: Some(Box::new(BoundPivotSpec {
            pivot_values,
            group_names: state.group_names.clone(),
            aggregate_names: state.aggregate_names,
        })),
    };
    let mut node = select_node_from(table_ref(TableRefKind::Pivot(Box::new(bound_pivot))));
    for name in &state.group_names {
        node.select_list
            .push(ParsedExpression::column_ref([name.clone()]));
    }
    let mut names = aggregate_names.into_iter();
    let value_count = node_pivot_value_count(&node);
    for _ in 0..value_count {
        for aggregate in &pivot.aggregates {
            let name = names
                .next()
                .ok_or_else(|| Error::internal("pivot aggregate name missing"))?;
            let mut aggregate = aggregate.clone();
            replace_pivot_column_ref(&mut aggregate, &name)?;
            aggregate.alias = name;
            node.select_list.push(aggregate);
        }
    }
    Ok(node)
}

fn node_pivot_value_count(node: &SelectNode) -> usize {
    match &node.from_table.kind {
        TableRefKind::Pivot(pivot) => pivot.bound.as_ref().map_or(0, |bound| bound.pivot_values.len()),
        _ => 0,
    }
}

/// Rewrites a PIVOT into a query node (`Binder::BindPivot`).
fn bind_pivot_node(
    scope: &mut BindScope<'_>,
    pivot: &mut PivotRef,
    all_columns: Vec<ParsedExpression>,
) -> Result<SelectNode> {
    let mut handled = HashSet::new();
    let macro_binding = scope.binder.macro_binding.clone();
    for aggregate in &mut pivot.aggregates {
        if contains_subquery(aggregate) {
            return Err(Error::binder("Pivot expression cannot contain subqueries")
                .with_position(aggregate.query_location));
        }
        if contains_window(aggregate) {
            return Err(Error::binder("Pivot expression cannot contain window functions")
                .with_position(aggregate.query_location));
        }
        let count = count_pivot_aggregates(scope, aggregate)?;
        if count != 1 {
            let found = if count == 0 {
                ByteString::from("but no aggregates were found")
            } else {
                rustdb_common::byte_format!("but {count} were found")
            };
            return Err(Error::binder(rustdb_common::byte_format!(
                "Pivot expression must contain exactly one aggregate, {found}"
            ))
            .with_position(aggregate.query_location));
        }
        with_pivot_aggregate(scope, aggregate, |call| {
            // Errors here only concern qualified names, reported below on the copy.
            let _ = extract_pivot_columns(call, &mut handled, macro_binding.as_ref());
        })?;
    }
    for aggregate in &mut pivot.aggregates {
        let mut copy = aggregate.clone();
        let mut calls = Vec::new();
        with_pivot_aggregate(scope, &mut copy, |call| calls.push(call.clone()))?;
        for mut call in calls {
            extract_pivot_columns(&mut call, &mut handled, macro_binding.as_ref())?;
        }
    }

    // Resolve the IN lists to values.
    for column in &mut pivot.pivots {
        let expected = column.pivot_expressions.len();
        for entry in &mut column.entries {
            if !entry.values.is_empty() {
                continue;
            }
            let Some(mut expression) = entry.star_expr.take() else {
                continue;
            };
            bind_pivot_in_list(scope, &mut expression, &mut entry.values)?;
            if entry.values.len() != expected {
                return Err(Error::binder(rustdb_common::byte_format!(
                    "PIVOT IN list - inconsistent amount of rows - expected {expected} but got {}",
                    entry.values.len()
                )));
            }
        }
    }

    let mut total_pivots: u64 = 1;
    for column in &mut pivot.pivots {
        if !column.pivot_enum.is_empty() {
            expand_pivot_enum(scope, column)?;
        }
        total_pivots = total_pivots.saturating_mul(column.entries.len() as u64);
        for pivot_expression in &mut column.pivot_expressions {
            extract_pivot_columns(pivot_expression, &mut handled, macro_binding.as_ref())?;
        }
        let mut seen = HashSet::new();
        for entry in &column.entries {
            let value = if entry.values.len() == 1 {
                entry.values[0].clone()
            } else {
                Value::list(LogicalType::VARCHAR, entry.values.clone())
            };
            let key = value_name(&value)?;
            if !seen.insert((key.clone(), value.is_null())) {
                return Err(Error::binder(rustdb_common::byte_format!(
                    "The value \"{key}\" was specified multiple times in the IN clause"
                )));
            }
            if entry.values.len() != column.pivot_expressions.len() {
                return Err(Error::binder(rustdb_common::byte_format!(
                    "PIVOT IN list - inconsistent amount of rows - expected {} but got {}",
                    column.pivot_expressions.len(),
                    entry.values.len()
                )));
            }
        }
    }
    let pivot_limit = scope.context.config.pivot_limit;
    if total_pivots >= pivot_limit {
        return Err(Error::binder(rustdb_common::byte_format!(
            "Pivot column limit of {pivot_limit} exceeded. Use SET pivot_limit=X to increase the limit."
        )));
    }

    let mut pivot_values = Vec::new();
    construct_pivots(pivot, 0, &PivotValueElement::default(), &mut pivot_values)?;

    if pivot_values.len() as u64 <= scope.context.config.pivot_filter_threshold {
        return pivot_filtered_aggregate(scope, pivot, all_columns, &handled, &pivot_values);
    }
    let mut state = PivotBindState::default();
    let stage1 = pivot_initial_aggregate(scope, &mut state, pivot, all_columns, &handled)?;
    let stage2 = pivot_list_aggregate(&state, pivot, stage1);
    pivot_final_operator(state, pivot, stage2, pivot_values)
}

/// `PIVOT ... ON col IN enum_type`: one entry per member of the ENUM type.
fn expand_pivot_enum(scope: &BindScope<'_>, column: &mut rustdb_ast::PivotColumn) -> Result<()> {
    // The built-in types are catalog entries too (`DefaultTypeGenerator`); a bare ENUM
    // has no members.
    if let Some(id) = rustdb_types::builtin_type_id(&column.pivot_enum) {
        if id == LogicalTypeId::Enum {
            return Err(Error::binder("ENUM type is incomplete"));
        }
        return Err(Error::binder(rustdb_common::byte_format!(
            "Pivot must reference an ENUM type: \"{}\" is of type \"{}\"",
            column.pivot_enum,
            LogicalType::simple(id)
        )));
    }
    let lookup = scope
        .context
        .get_entry("", "", CatalogType::Type, &column.pivot_enum, false)?
        .ok_or_else(|| {
            Error::catalog(rustdb_common::byte_format!(
                "Type with name {} does not exist!",
                column.pivot_enum
            ))
        })?;
    let rustdb_catalog::EntryKind::Type(type_entry) = &lookup.entry.kind else {
        return Err(Error::internal("type lookup returned a non-type entry"));
    };
    let ty = &type_entry.user_type;
    if ty.id() != LogicalTypeId::Enum {
        return Err(Error::binder(rustdb_common::byte_format!(
            "Pivot must reference an ENUM type: \"{}\" is of type \"{ty}\"",
            column.pivot_enum
        )));
    }
    for member in ty.enum_values().map(<[_]>::to_vec).unwrap_or_default() {
        column.entries.push(PivotColumnEntry {
            values: vec![Value::try_varchar(&member)?],
            star_expr: None,
            alias: member,
        });
    }
    Ok(())
}

fn contains_subquery(expression: &ParsedExpression) -> bool {
    let mut copy = expression.clone();
    let mut found = false;
    let _ = visit_expressions(&mut copy, |child| {
        found |= matches!(child.kind, ParsedExpressionKind::Subquery(_));
        Ok(!found)
    });
    found
}

fn contains_window(expression: &ParsedExpression) -> bool {
    let mut copy = expression.clone();
    let mut found = false;
    let _ = visit_expressions(&mut copy, |child| {
        found |= matches!(child.kind, ParsedExpressionKind::Window(_));
        Ok(!found)
    });
    found
}

/// One output row group of an UNPIVOT entry (`UnpivotEntry`).
struct UnpivotEntry {
    alias: ByteString,
    column_names: Vec<ByteString>,
    expressions: Vec<ParsedExpression>,
}

/// The unpivoted columns of one `UNPIVOT ... ON` entry, `*` expanded
/// (`Binder::ExtractUnpivotEntries`).
fn extract_unpivot_entries(
    scope: &mut BindScope<'_>,
    entry: &mut PivotColumnEntry,
    entries: &mut Vec<UnpivotEntry>,
) -> Result<()> {
    if let Some(expression) = &entry.star_expr {
        let mut copy = (**expression).clone();
        let mut values = Vec::new();
        if bind_pivot_in_list(scope, &mut copy, &mut values).is_ok() {
            entry.values = values;
            entry.star_expr = None;
        }
    }
    let Some(expression) = entry.star_expr.take() else {
        let mut expressions = Vec::with_capacity(entry.values.len());
        for value in &entry.values {
            let column_name = value_name(value)?;
            if column_name.is_empty() {
                return Err(Error::binder("UNPIVOT - empty column name not supported"));
            }
            expressions.push(ParsedExpression::column_ref([column_name]));
        }
        entries.push(UnpivotEntry {
            alias: entry.alias.clone(),
            column_names: Vec::new(),
            expressions,
        });
        return Ok(());
    };
    let mut star_columns = Vec::new();
    expand_star_expression(scope, *expression, &mut star_columns)?;
    for expression in star_columns {
        entries.push(UnpivotEntry {
            alias: expression.alias.clone(),
            column_names: Vec::new(),
            expressions: vec![expression],
        });
    }
    Ok(())
}

/// The column names referenced by an UNPIVOT expression (`ExtractUnpivotColumnName`).
fn extract_unpivot_column_names(
    expression: &mut ParsedExpression,
    result: &mut Vec<ByteString>,
) -> Result<()> {
    visit_expressions(expression, |child| match &child.kind {
        ParsedExpressionKind::ColumnRef(column) => {
            result.push(column.column_names.last().cloned().unwrap_or_default());
            Ok(false)
        }
        ParsedExpressionKind::Subquery(_) => {
            Err(Error::binder("UNPIVOT list cannot contain subqueries").with_position(child.query_location))
        }
        _ => Ok(true),
    })
}

/// Rewrites an UNPIVOT into `SELECT {other columns}, unnest(names), unnest(values)...`
/// over a subquery building the lists (`Binder::BindUnpivot`).
fn bind_unpivot(
    scope: &mut BindScope<'_>,
    pivot: &mut PivotRef,
    all_columns: Vec<ParsedExpression>,
    where_clause: &mut Option<ParsedExpression>,
) -> Result<SelectNode> {
    let source = std::mem::replace(&mut pivot.source, Box::new(table_ref(TableRefKind::Empty)));
    let mut select_node = select_node_from(*source);
    let unpivot = pivot
        .pivots
        .first_mut()
        .ok_or_else(|| Error::internal("UNPIVOT without a pivot column"))?;
    let mut entries = Vec::new();
    for entry in &mut unpivot.entries {
        extract_unpivot_entries(scope, entry, &mut entries)?;
    }
    if entries.is_empty() {
        return Err(Error::binder(
            "UNPIVOT clause must unpivot on at least one column - zero were provided",
        ));
    }
    let mut handled: Vec<ByteString> = Vec::new();
    for entry in &mut entries {
        for expression in &mut entry.expressions {
            let mut names = Vec::new();
            extract_unpivot_column_names(expression, &mut names)?;
            match names.as_slice() {
                [] => {
                    return Err(Error::binder(rustdb_common::byte_format!(
                        "UNPIVOT clause must contain exactly one column - expression \"{expression}\" does not \
                         contain any"
                    ))
                    .with_position(expression.query_location));
                }
                [name] => {
                    if !handled.iter().any(|h| h.eq_ignore_ascii_case(name)) {
                        handled.push(name.clone());
                    }
                    entry.column_names.push(name.clone());
                }
                _ => {
                    return Err(Error::binder(rustdb_common::byte_format!(
                        "UNPIVOT clause must contain exactly one column - expression \"{expression}\" contains \
                         multiple ({})",
                        ByteString::join(&names, ", ")
                    ))
                    .with_position(expression.query_location));
                }
            }
        }
    }
    let mut name_map: HashMap<ByteString, ByteString> = HashMap::new();
    let mut select_names = Vec::new();
    for column in all_columns {
        let ParsedExpressionKind::ColumnRef(reference) = &column.kind else {
            return Err(Error::internal(
                "Unexpected child of pivot source - not a ColumnRef",
            ));
        };
        let column_name = reference.column_names.last().cloned().unwrap_or_default();
        if let Some(position) = handled.iter().position(|h| h.eq_ignore_ascii_case(&column_name)) {
            name_map.insert(column_name.to_ascii_lowercase(), column_name);
            handled.remove(position);
        } else {
            select_names.push(column.name());
            select_node.select_list.push(column);
        }
    }
    if let Some(missing) = handled.first() {
        return Err(Error::binder(rustdb_common::byte_format!(
            "Column \"{missing}\" referenced in UNPIVOT but no matching entry was found in the table"
        )));
    }
    let mut unpivot_names = Vec::with_capacity(entries.len());
    for entry in &entries {
        let mut generated = ByteString::new();
        for column_name in &entry.column_names {
            let name = name_map
                .get(&column_name.to_ascii_lowercase())
                .ok_or_else(|| Error::internal("Unpivot - could not find column name in name map"))?;
            if !generated.is_empty() {
                generated.push(b'_');
            }
            generated.push_bytes(name);
        }
        unpivot_names.push(Value::try_varchar(if entry.alias.is_empty() {
            &generated
        } else {
            &entry.alias
        })?);
    }
    let value_count = entries[0].expressions.len();
    if let Some(entry) = entries
        .iter()
        .find(|entry| entry.expressions.len() != value_count)
    {
        return Err(Error::binder(rustdb_common::byte_format!(
            "UNPIVOT value count mismatch - entry has {} values, but expected all entries to have {value_count} \
             values",
            entry.expressions.len()
        )));
    }
    let mut unpivot_expressions: Vec<Vec<ParsedExpression>> = (0..value_count).map(|_| Vec::new()).collect();
    for entry in entries {
        for (index, expression) in entry.expressions.into_iter().enumerate() {
            unpivot_expressions[index].push(expression);
        }
    }

    let column_count = select_names.len();
    let unnest_count = unpivot_expressions.len();
    select_names.push(ByteString::from("unpivot_names"));
    for i in 0..unnest_count {
        select_names.push(if i > 0 {
            rustdb_common::byte_format!("unpivot_list_{}", i + 1)
        } else {
            ByteString::from("unpivot_list")
        });
    }
    deduplicate_columns(&mut select_names);

    let names_list = Value::list(LogicalType::VARCHAR, unpivot_names);
    select_node
        .select_list
        .push(ParsedExpression::constant(names_list).with_alias(select_names[column_count].clone()));
    if pivot.unpivot_names.len() != unpivot_expressions.len() {
        return Err(Error::binder(rustdb_common::byte_format!(
            "UNPIVOT name count mismatch - got {} names but {} expressions",
            pivot.unpivot_names.len(),
            unpivot_expressions.len()
        )));
    }
    for (i, expressions) in unpivot_expressions.into_iter().enumerate() {
        let list = ParsedExpression::function("unpivot_list", expressions);
        select_node
            .select_list
            .push(list.with_alias(select_names[column_count + 1 + i].clone()));
    }

    let mut subquery = table_ref(TableRefKind::Subquery(subquery_ref(select_node, Vec::new())));
    subquery.alias = "unpivot".into();
    let mut result = select_node_from(subquery);
    for name in &select_names[..column_count] {
        result
            .select_list
            .push(ParsedExpression::column_ref([name.clone()]));
    }
    let name_column = unpivot
        .unpivot_names
        .first()
        .cloned()
        .ok_or_else(|| Error::internal("UNPIVOT without a name column"))?;
    let unnest_names = ParsedExpression::function(
        "unnest",
        vec![ParsedExpression::column_ref([select_names[column_count].clone()])],
    );
    result.select_list.push(unnest_names.with_alias(name_column));
    for i in 0..unnest_count {
        let internal = select_names[column_count + 1 + i].clone();
        let unnest = ParsedExpression::function("unnest", vec![ParsedExpression::column_ref([internal])]);
        let name = pivot
            .column_name_alias
            .get(i)
            .or_else(|| pivot.unpivot_names.get(i))
            .cloned()
            .unwrap_or_default();
        result.select_list.push(unnest.with_alias(name.clone()));
        if !pivot.include_nulls {
            // EXCLUDE NULLS filters the unnested values.
            let filter = ParsedExpression::operator(
                ExpressionType::OperatorIsNotNull,
                vec![ParsedExpression::column_ref([name])],
            );
            *where_clause = Some(match where_clause.take() {
                Some(previous) => ParsedExpression::and(previous, filter),
                None => filter,
            });
        }
    }
    Ok(result)
}

/// The aggregates of stage 1, found below the stage 2 aggregate (`ExtractPivotAggregates`).
fn extract_pivot_aggregates(plan: &LogicalOperator) -> Result<Vec<crate::Expression>> {
    let mut op = plan;
    let mut found_first = false;
    loop {
        if matches!(op.kind, LogicalKind::Aggregate(_)) {
            if found_first {
                break;
            }
            found_first = true;
        }
        let [child] = op.children.as_slice() else {
            return Err(Error::internal("Pivot - expected an aggregate"));
        };
        op = child;
    }
    Ok(op
        .expressions
        .iter()
        .filter(|aggregate| aggregate.alias != "__collated_group")
        .cloned()
        .collect())
}

/// Binds the PIVOT operator of the LIST-based plan (`Binder::BindBoundPivot`).
fn bind_bound_pivot(scope: &mut BindScope<'_>, pivot: &mut PivotRef, alias: &[u8]) -> Result<BoundStatement> {
    let bound = pivot
        .bound
        .take()
        .ok_or_else(|| Error::internal("bound pivot without its values"))?;
    let bind_index = scope.generate_table_index();
    let mut child_binder = scope.binder.child()?;
    let child = {
        let mut child_scope = BindScope {
            binder: &mut child_binder,
            outer: Some(&mut *scope),
        };
        bind_table_ref(&mut child_scope, &mut pivot.source)?
    };
    let mut aggregates = extract_pivot_aggregates(&child.plan)?;
    if aggregates.len() < bound.aggregate_names.len() {
        // Identical aggregates were merged: expand them back by name.
        let mut unique_names: Vec<&ByteString> = Vec::new();
        for name in &bound.aggregate_names {
            if !unique_names.contains(&name) {
                unique_names.push(name);
            }
        }
        if unique_names.len() != aggregates.len() {
            return Err(Error::internal(rustdb_common::byte_format!(
                "Pivot aggregate mismatch: {} unique names, {} aggregates",
                unique_names.len(),
                aggregates.len()
            )));
        }
        aggregates = bound
            .aggregate_names
            .iter()
            .map(|name| {
                let position = unique_names
                    .iter()
                    .position(|unique| *unique == name)
                    .unwrap_or(0);
                aggregates[position].clone()
            })
            .collect();
    }
    if aggregates.len() != bound.aggregate_names.len() {
        return Err(Error::internal(rustdb_common::byte_format!(
            "Pivot aggregate count mismatch (expected {}, found {})",
            bound.aggregate_names.len(),
            aggregates.len()
        )));
    }
    let child_types = child.types.clone();
    let mut names = Vec::new();
    let mut types = Vec::new();
    for (i, name) in bound.group_names.iter().enumerate() {
        names.push(name.clone());
        types.push(child_types.get(i).cloned().unwrap_or(LogicalType::SQLNULL));
    }
    let mut pivot_values = Vec::new();
    for pivot_value in &bound.pivot_values {
        for (aggregate, aggregate_name) in aggregates.iter().zip(&bound.aggregate_names) {
            let mut text = ByteString::new();
            for value in &pivot_value.values {
                let value_text = value_name(value)?;
                if text.is_empty() {
                    text = value_text;
                } else {
                    text = rustdb_common::byte_format!("{text}_{value_text}");
                }
            }
            pivot_values.push(text);
            names.push(rustdb_common::byte_format!(
                "{}{aggregate_name}",
                pivot_value.name
            ));
            types.push(aggregate.return_type.clone());
        }
    }
    let subquery_alias = if alias.is_empty() {
        b"__unnamed_pivot".as_slice()
    } else {
        alias
    };
    deduplicate_columns(&mut names);
    scope
        .bind_context
        .add_generic_binding(bind_index, subquery_alias, names.clone(), types.clone())?;
    scope.binder.move_correlated_expressions(&mut child_binder);
    let info = BoundPivotInfo {
        group_count: bound.group_names.len() as u64,
        types: types.clone(),
        pivot_values,
        aggregates,
    };
    let plan = LogicalOperator::new(LogicalKind::Pivot {
        pivot_index: bind_index,
        bound_pivot: Box::new(info),
    })
    .with_children(vec![child.plan]);
    Ok(BoundStatement::new(plan, names, types))
}
