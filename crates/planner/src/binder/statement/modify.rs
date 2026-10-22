//! `INSERT`, `UPDATE` and `DELETE` (DuckDB's `bind_insert.cpp`, `bind_update.cpp` and
//! `bind_delete.cpp`).

use std::sync::Arc;

use rustdb_ast::{
    DeleteStatement, ExpressionType, InsertColumnOrder, InsertStatement, ParsedExpressionKind, QueryNodeKind,
    TableRefKind, UpdateSetInfo, UpdateStatement,
};
use rustdb_common::string_util::top_n_strings;
use rustdb_common::{Error, Result};
use rustdb_types::LogicalType;

use super::super::query_node::bind_node;
use super::super::query_node::bind_setop_node::cast_operator_to_types;
use super::super::query_node::plan_subqueries;
use super::super::tableref::bind_table_ref;
use super::super::{BindScope, BoundStatement, Scope};
use super::table::{
    bind_constraints, bind_default_values, bind_returning, expand_default, find_table, missing_column_error,
    table_of,
};
use crate::ColumnBinding;
use crate::expression::{Expression, ExpressionKind};
use crate::expression_binder::{ExpressionBinder, UpdateBinder, WhereBinder};
use crate::operator::{
    BoundConstraint, COLUMN_IDENTIFIER_ROW_ID, ColumnIndex, LogicalDelete, LogicalInsert, LogicalKind,
    LogicalOperator, LogicalUpdate,
};
use crate::prepared::StatementReturnType;

fn count_result(plan: LogicalOperator) -> BoundStatement {
    BoundStatement::new(plan, vec!["Count".to_owned()], vec![LogicalType::BIGINT])
}

/// `Binder::CheckInsertColumnCountMismatch`.
pub(super) fn check_column_count(
    expected: usize,
    result: usize,
    columns_provided: bool,
    table: &str,
) -> Result<()> {
    if expected == result {
        return Ok(());
    }
    Err(Error::binder(if columns_provided {
        format!(
            "Column name/value mismatch for insert on {table}: expected {expected} columns but {result} values were supplied"
        )
    } else {
        format!("table {table} has {expected} columns but {result} values were supplied")
    }))
}

/// The VALUES list an INSERT reads, if its query is a plain `VALUES (...)`
/// (`InsertStatement::GetValuesList`).
pub(super) fn values_list(stmt: &mut InsertStatement) -> Option<&mut rustdb_ast::ExpressionListRef> {
    let select = stmt.select_statement.as_mut()?;
    if !select.node.cte_map.map.is_empty() || !select.node.modifiers.is_empty() {
        return None;
    }
    let QueryNodeKind::Select(node) = &mut select.node.kind else {
        return None;
    };
    if node.where_clause.is_some() || node.qualify.is_some() || node.having.is_some() {
        return None;
    }
    if !node.group_expressions.is_empty() || !node.group_sets.is_empty() {
        return None;
    }
    if node.select_list.len() != 1 || node.select_list[0].expression_type != ExpressionType::Star {
        return None;
    }
    match &mut node.from_table.kind {
        TableRefKind::ExpressionList(list) => Some(list),
        _ => None,
    }
}

/// `Binder::ExpandDefaultInValuesList`: a `VALUES` list gets the target columns' types
/// and names, and `DEFAULT` becomes the column's default.
pub(super) fn expand_default_in_values_list(
    stmt: &mut InsertStatement,
    table: &rustdb_catalog::TableEntry,
    table_name: &str,
    named_column_map: &[usize],
) -> Result<()> {
    let expected_columns = if stmt.columns.is_empty() {
        table.physical_columns().count()
    } else {
        stmt.columns.len()
    };
    let columns_provided = !stmt.columns.is_empty();
    let Some(list) = values_list(stmt) else {
        return Ok(());
    };
    list.expected_types
        .resize(expected_columns, LogicalType::default());
    list.expected_names.resize(expected_columns, String::new());
    let first_len = list.values.first().map_or(0, Vec::len);
    check_column_count(expected_columns, first_len, columns_provided, table_name)?;
    for (column_index, &logical) in named_column_map.iter().enumerate().take(expected_columns) {
        let column = &table.columns[logical];
        list.expected_types[column_index] = column.logical_type.clone();
        list.expected_names[column_index].clone_from(&column.name);
        for row in &mut list.values {
            if let Some(value) = row.get_mut(column_index) {
                expand_default(value, column.default_value.as_ref(), &column.logical_type);
            }
        }
    }
    Ok(())
}

/// `INSERT` (DuckDB's `Binder::Bind(InsertStatement &)`).
pub fn bind_insert(scope: &mut BindScope<'_>, stmt: &mut InsertStatement) -> Result<BoundStatement> {
    let found = find_table(scope, &stmt.catalog, &stmt.schema, &stmt.table)?;
    if stmt.on_conflict_info.is_some() {
        // ON CONFLICT is bound as the equivalent MERGE INTO.
        let mut merge_into = super::merge_into::generate_merge_into(stmt, &found.entry)?;
        return super::merge_into::bind_merge_into(scope, &mut merge_into);
    }
    let entry = Arc::clone(&found.entry);
    let table = table_of(&entry)?;
    let table_name = entry.name().to_owned();
    let table_index = scope.generate_table_index();
    let mut root_select = None;
    if stmt.column_order == InsertColumnOrder::InsertByName {
        if values_list(stmt).is_some() {
            return Err(Error::binder(
                "INSERT BY NAME can only be used when inserting from a SELECT statement",
            ));
        }
        if stmt.default_values {
            return Err(Error::binder(
                "INSERT BY NAME cannot be combined with with DEFAULT VALUES",
            ));
        }
        if !stmt.columns.is_empty() {
            return Err(Error::binder(
                "INSERT BY NAME cannot be combined with an explicit column list",
            ));
        }
        let select = stmt
            .select_statement
            .as_mut()
            .ok_or_else(|| Error::internal("INSERT BY NAME without a query"))?;
        let bound = bind_child_select(scope, &mut select.node)?;
        stmt.columns.clone_from(&bound.names);
        root_select = Some(bound);
    }
    let InsertColumnList {
        named_column_map,
        expected_types,
        column_index_map,
    } = bind_insert_column_list(table, &table_name, &stmt.columns, stmt.default_values)?;
    let bound_defaults = bind_default_values(scope, table)?;
    let bound_constraints = bind_constraints(scope, &table_name, table)?;
    let expected_columns = if stmt.columns.is_empty() {
        table.physical_columns().count()
    } else {
        stmt.columns.len()
    };
    let columns_provided = !stmt.columns.is_empty();
    expand_default_in_values_list(stmt, table, &table_name, &named_column_map)?;
    let mut insert = LogicalOperator::new(LogicalKind::Insert(Box::new(LogicalInsert {
        table: Arc::clone(&entry),
        catalog: found.catalog.clone(),
        schema: found.schema.clone(),
        table_index,
        column_index_map,
        expected_types: expected_types.clone(),
        bound_defaults,
        bound_constraints,
        return_chunk: !stmt.returning_list.is_empty(),
    })));
    if let Some(select) = &mut stmt.select_statement {
        let bound = match root_select {
            Some(bound) => bound,
            None => bind_child_select(scope, &mut select.node)?,
        };
        check_column_count(expected_columns, bound.types.len(), columns_provided, &table_name)?;
        insert.add_child(cast_operator_to_types(
            scope.binder,
            &bound.types,
            &expected_types,
            bound.plan,
        )?);
    } else if stmt.default_values {
        insert.add_child(LogicalOperator::new(LogicalKind::DummyScan {
            table_index: scope.generate_table_index(),
        }));
    }
    // Without a source (COPY FROM) the caller adds the child.
    if !stmt.returning_list.is_empty() {
        // The returned rows get a table index of their own.
        let insert_table_index = scope.generate_table_index();
        if let LogicalKind::Insert(insert) = &mut insert.kind {
            insert.table_index = insert_table_index;
        }
        let alias = stmt
            .table_ref
            .as_ref()
            .map_or_else(String::new, |r| r.alias.clone());
        let returning = std::mem::take(&mut stmt.returning_list);
        return bind_returning(
            scope,
            returning,
            &table_name,
            &entry,
            &alias,
            insert_table_index,
            insert,
        );
    }
    scope.with_properties(|properties| properties.return_type = StatementReturnType::ChangedRows);
    Ok(count_result(insert))
}

/// The target columns of an INSERT (the outputs of `Binder::BindInsertColumnList`).
#[derive(Debug, Default)]
pub(super) struct InsertColumnList {
    /// The logical table column of each inserted value.
    pub named_column_map: Vec<usize>,
    pub expected_types: Vec<LogicalType>,
    /// For each physical column, the inserted value holding it (`None`: the default);
    /// empty when every column is inserted in order.
    pub column_index_map: Vec<Option<usize>>,
}

/// `Binder::BindInsertColumnList`.
pub(super) fn bind_insert_column_list(
    table: &rustdb_catalog::TableEntry,
    table_name: &str,
    columns: &[String],
    default_values: bool,
) -> Result<InsertColumnList> {
    let mut result = InsertColumnList::default();
    if columns.is_empty() && !default_values {
        for (_, logical, column) in table.physical_columns() {
            result.named_column_map.push(logical);
            result.expected_types.push(column.logical_type.clone());
        }
        return Ok(result);
    }
    for (i, name) in columns.iter().enumerate() {
        if columns[..i].iter().any(|other| other.eq_ignore_ascii_case(name)) {
            return Err(Error::binder(format!(
                "Duplicate column name \"{name}\" in INSERT"
            )));
        }
        if name.eq_ignore_ascii_case("rowid") && table.column(name).is_none() {
            return Err(Error::binder("Cannot explicitly insert values into rowid column"));
        }
        let Some((logical, column)) = table.column(name) else {
            return Err(missing_column_error(table, table_name, name));
        };
        if column.generated.is_some() {
            return Err(Error::binder("Cannot insert into a generated column"));
        }
        result.expected_types.push(column.logical_type.clone());
        result.named_column_map.push(logical);
    }
    for (_, _, column) in table.physical_columns() {
        result.column_index_map.push(
            columns
                .iter()
                .position(|name| name.eq_ignore_ascii_case(&column.name)),
        );
    }
    Ok(result)
}

/// Binds a query in a child binder and moves its correlated columns up.
fn bind_child_select(scope: &mut BindScope<'_>, node: &mut rustdb_ast::QueryNode) -> Result<BoundStatement> {
    let mut child = scope.binder.child()?;
    let bound = {
        let mut child_scope = BindScope {
            binder: &mut child,
            outer: Some(scope as &mut dyn Scope),
        };
        bind_node(&mut child_scope, node)?
    };
    scope.binder.move_correlated_expressions(&mut child);
    Ok(bound)
}

/// Finds the base table scan at the bottom of a DML statement's FROM.
fn base_get(op: &mut LogicalOperator) -> Option<&mut LogicalOperator> {
    if matches!(op.kind, LogicalKind::Get(_)) {
        return Some(op);
    }
    op.children.first_mut().and_then(base_get)
}

/// Adds the `rowid` column to the scan and returns a reference to it
/// (`Binder::BindRowIdColumns`).
pub(super) fn bind_row_id_column(get: &mut LogicalOperator) -> Result<Expression> {
    let LogicalKind::Get(get) = &mut get.kind else {
        return Err(Error::internal("expected a table scan"));
    };
    let shared = get
        .shared_column_ids
        .clone()
        .ok_or_else(|| Error::internal("table scan without shared column ids"))?;
    let mut ids = shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let position = if let Some(position) = ids.iter().position(|id| id.index == COLUMN_IDENTIFIER_ROW_ID) {
        position
    } else {
        ids.push(ColumnIndex::new(COLUMN_IDENTIFIER_ROW_ID));
        ids.len() - 1
    };
    Ok(Expression::column_ref(
        "rowid",
        LogicalType::BIGINT,
        ColumnBinding::new(get.table_index, position as u64),
        0,
    ))
}

/// Adds column `logical` to the scan (`LogicalGet::AddColumnId`) and returns its position.
fn add_scan_column(get: &mut LogicalOperator, logical: usize) -> Result<(u64, u64)> {
    let LogicalKind::Get(get) = &mut get.kind else {
        return Err(Error::internal("expected a table scan"));
    };
    let shared = get
        .shared_column_ids
        .clone()
        .ok_or_else(|| Error::internal("table scan without shared column ids"))?;
    let mut ids = shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    ids.push(ColumnIndex::new(logical as u64));
    Ok((get.table_index, (ids.len() - 1) as u64))
}

/// The catalog of the table a scan reads.
fn table_catalog(get: &crate::operator::LogicalGet) -> String {
    get.table_schema
        .as_ref()
        .map(|(catalog, _)| catalog.clone())
        .unwrap_or_default()
}

/// `DELETE` (DuckDB's `Binder::Bind(DeleteStatement &)`).
pub fn bind_delete(scope: &mut BindScope<'_>, stmt: &mut DeleteStatement) -> Result<BoundStatement> {
    let bound_table = bind_table_ref(scope, &mut stmt.table)?;
    let mut root = bound_table.plan;
    let (entry, catalog) = match &root.kind {
        LogicalKind::Get(get) => get.table.clone().map(|entry| (entry, table_catalog(get))),
        _ => None,
    }
    .ok_or_else(|| Error::binder("Can only delete from base table"))?;
    let table = table_of(&entry)?;
    let table_name = entry.name().to_owned();
    for using in &mut stmt.using_clauses {
        let mut using_binder = scope.binder.child()?;
        let bound = {
            let mut using_scope = BindScope {
                binder: &mut using_binder,
                outer: Some(scope as &mut dyn Scope),
            };
            bind_table_ref(&mut using_scope, using)?
        };
        root = LogicalOperator::cross_product(root, bound.plan);
        scope
            .binder
            .bind_context
            .add_context(std::mem::take(&mut using_binder.bind_context));
    }
    if let Some(condition) = &mut stmt.condition {
        let mut bound = {
            let (inner, outer) = scope.split();
            let mut binder = WhereBinder::new(inner, outer, None);
            binder.bind(condition, true)?
        };
        plan_subqueries(scope, &mut bound, &mut root)?;
        root = LogicalOperator::filter(bound, root);
    }
    let mut table_index = scope.generate_table_index();
    let bound_constraints = bind_constraints(scope, &table_name, table)?;
    let row_id = {
        let get = base_get(&mut root).ok_or_else(|| Error::binder("Can only delete from base table"))?;
        bind_row_id_column(get)?
    };
    if !stmt.returning_list.is_empty() {
        // The returned rows get a table index of their own.
        table_index = scope.generate_table_index();
    }
    let schema = table
        .storage
        .as_ref()
        .map(|storage| storage.info().schema_name())
        .unwrap_or_default();
    let delete = LogicalOperator::new(LogicalKind::Delete(Box::new(LogicalDelete {
        table: Arc::clone(&entry),
        catalog,
        schema,
        table_index,
        bound_constraints,
        return_chunk: !stmt.returning_list.is_empty(),
    })))
    .with_expressions(vec![row_id])
    .with_children(vec![root]);
    if !stmt.returning_list.is_empty() {
        let returning = std::mem::take(&mut stmt.returning_list);
        let alias = stmt.table.alias.clone();
        return bind_returning(scope, returning, &table_name, &entry, &alias, table_index, delete);
    }
    scope.with_properties(|properties| properties.return_type = StatementReturnType::ChangedRows);
    Ok(count_result(delete))
}

/// Whether values of `ty` can be updated in place (`LogicalType::SupportsRegularUpdate`).
fn supports_regular_update(ty: &LogicalType) -> bool {
    use rustdb_types::LogicalTypeId as Id;
    match ty.id() {
        Id::List | Id::Array | Id::Map | Id::Union | Id::Variant | Id::Geometry => false,
        Id::Struct => ty
            .struct_children()
            .unwrap_or_default()
            .iter()
            .all(|(_, child)| supports_regular_update(child)),
        _ => true,
    }
}

/// `UPDATE` (DuckDB's `Binder::Bind(UpdateStatement &)`).
pub fn bind_update(scope: &mut BindScope<'_>, stmt: &mut UpdateStatement) -> Result<BoundStatement> {
    let bound_table = bind_table_ref(scope, &mut stmt.table)?;
    let (entry, (catalog, schema)) = match &bound_table.plan.kind {
        LogicalKind::Get(get) => get
            .table
            .clone()
            .map(|entry| (entry, get.table_schema.clone().unwrap_or_default())),
        _ => None,
    }
    .ok_or_else(|| Error::binder("Can only update base table"))?;
    let table = table_of(&entry)?;
    let table_name = entry.name().to_owned();
    let mut root = bound_table.plan;
    if let Some(from) = &mut stmt.from_table {
        let mut from_binder = scope.binder.child()?;
        let bound = {
            let mut from_scope = BindScope {
                binder: &mut from_binder,
                outer: Some(scope as &mut dyn Scope),
            };
            bind_table_ref(&mut from_scope, from)?
        };
        root = LogicalOperator::new(LogicalKind::CrossProduct).with_children(vec![root, bound.plan]);
        scope
            .binder
            .bind_context
            .add_context(std::mem::take(&mut from_binder.bind_context));
    }
    let return_chunk = !stmt.returning_list.is_empty();
    let bound_defaults = bind_default_values(scope, table)?;
    let bound_constraints = bind_constraints(scope, &table_name, table)?;
    if let Some(condition) = &mut stmt.set_info.condition {
        let mut bound = {
            let (inner, outer) = scope.split();
            let mut binder = WhereBinder::new(inner, outer, None);
            binder.bind(condition, true)?
        };
        plan_subqueries(scope, &mut bound, &mut root)?;
        root = LogicalOperator::filter(bound, root);
    }
    // `BindUpdateSet`
    let proj_index = scope.generate_table_index();
    let mut set = UpdateSet {
        proj_index,
        ..UpdateSet::default()
    };
    // `prioritize_table_when_binding` (the UPDATE of a materialized ADD COLUMN default):
    // names resolve in the table's catalog and schema first (`CreateBinderWithSearchPath`).
    let saved_context = if stmt.prioritize_table_when_binding
        && let TableRefKind::BaseTable(base) = &stmt.table.kind
    {
        let catalog = if base.catalog_name.is_empty() {
            scope.context.default_catalog()
        } else {
            base.catalog_name.clone()
        };
        let schema = if base.schema_name.is_empty() {
            "main".to_owned()
        } else {
            base.schema_name.clone()
        };
        let mut context = (*scope.binder.context).clone();
        context.search_path = vec![(catalog.clone(), schema.clone())];
        if !schema.eq_ignore_ascii_case("main") {
            context.search_path.push((catalog, "main".to_owned()));
        }
        Some(std::mem::replace(&mut scope.binder.context, Arc::new(context)))
    } else {
        None
    };
    let bound = bind_update_set(scope, Some(&mut root), &mut stmt.set_info, table, &mut set);
    if let Some(context) = saved_context {
        scope.binder.context = context;
    }
    bound?;
    let get = base_get(&mut root).ok_or_else(|| Error::binder("Can only update base table"))?;
    let update_is_del_and_insert =
        bind_update_constraints(table, get, &bound_constraints, return_chunk, &mut set)?;
    let UpdateSet {
        columns,
        update_expressions,
        mut projection_expressions,
        ..
    } = set;
    let row_id = bind_row_id_column(get)?;
    projection_expressions.push(row_id);
    let projection = LogicalOperator::projection(proj_index, projection_expressions, root);
    let table_index = scope.generate_table_index();
    let update = LogicalOperator::new(LogicalKind::Update(Box::new(LogicalUpdate {
        table: Arc::clone(&entry),
        catalog,
        schema,
        table_index,
        columns,
        bound_defaults,
        bound_constraints,
        update_is_del_and_insert,
        return_chunk,
    })))
    .with_expressions(update_expressions)
    .with_children(vec![projection]);
    if return_chunk {
        let returning = std::mem::take(&mut stmt.returning_list);
        let alias = stmt.table.alias.clone();
        return bind_returning(scope, returning, &table_name, &entry, &alias, table_index, update);
    }
    scope.with_properties(|properties| properties.return_type = StatementReturnType::ChangedRows);
    Ok(count_result(update))
}

/// The bound `SET` list of an UPDATE: the new value of each updated column references a
/// column of the projection being built below the update.
#[derive(Debug, Default)]
pub(super) struct UpdateSet {
    pub proj_index: u64,
    /// The updated physical columns.
    pub columns: Vec<usize>,
    /// The new value of each column in `columns`.
    pub update_expressions: Vec<Expression>,
    /// The expressions of the projection below the update.
    pub projection_expressions: Vec<Expression>,
}

/// `TableCatalogEntry::BindUpdateConstraints`: CHECK constraints over several columns and
/// RETURNING need every involved column in the update; updates of indexed or nested
/// columns become a delete plus an insert of whole rows. Returns whether the update is a
/// delete plus insert.
pub(super) fn bind_update_constraints(
    table: &rustdb_catalog::TableEntry,
    get: &mut LogicalOperator,
    bound_constraints: &[BoundConstraint],
    return_chunk: bool,
    set: &mut UpdateSet,
) -> Result<bool> {
    for constraint in bound_constraints {
        if let BoundConstraint::Check { bound_columns, .. } = constraint {
            bind_extra_columns(table, get, set, bound_columns)?;
        }
    }
    let all_columns: Vec<usize> = table.physical_columns().map(|(p, _, _)| p).collect();
    if return_chunk {
        bind_extra_columns(table, get, set, &all_columns)?;
    }
    let mut update_is_del_and_insert = false;
    if let Some(storage) = &table.storage
        && storage.index_columns_updated(&set.columns)
    {
        update_is_del_and_insert = true;
    }
    for &column in &set.columns {
        if let Some(logical) = table.physical_to_logical(column)
            && !supports_regular_update(&table.columns[logical].logical_type)
        {
            update_is_del_and_insert = true;
        }
    }
    if update_is_del_and_insert {
        bind_extra_columns(table, get, set, &all_columns)?;
    }
    Ok(update_is_del_and_insert)
}

/// `Binder::BindUpdateSet`: binds each `column = expression`; subqueries are planned on
/// `root` if given.
pub(super) fn bind_update_set(
    scope: &mut BindScope<'_>,
    mut root: Option<&mut LogicalOperator>,
    set_info: &mut UpdateSetInfo,
    table: &rustdb_catalog::TableEntry,
    set: &mut UpdateSet,
) -> Result<()> {
    let proj_index = set.proj_index;
    let UpdateSet {
        columns,
        update_expressions,
        projection_expressions,
        ..
    } = set;
    for (name, expression) in set_info.columns.iter().zip(set_info.expressions.iter_mut()) {
        let Some((logical, column)) = table.column(name) else {
            let candidates: Vec<String> = table.physical_columns().map(|(_, _, c)| c.name.clone()).collect();
            let scores = candidates
                .iter()
                .map(|candidate| {
                    (
                        candidate.clone(),
                        rustdb_common::string_util::similarity_rating(candidate, name),
                    )
                })
                .collect();
            let suggestions = top_n_strings(scores, 5, 0.5);
            let message = if suggestions.is_empty() {
                String::new()
            } else {
                let quoted: Vec<String> = suggestions.iter().map(|s| format!("\"{s}\"")).collect();
                format!("Did you mean: {}", quoted.join(", "))
            };
            return Err(Error::binder(format!(
                "Referenced update column {name} not found in table!\n{message}"
            )));
        };
        if column.generated.is_some() {
            return Err(Error::binder(format!(
                "Cant update column \"{}\" because it is a generated column!",
                column.name
            )));
        }
        let physical = table
            .logical_to_physical(logical)
            .ok_or_else(|| Error::internal("generated column in UPDATE"))?;
        if columns.contains(&physical) {
            return Err(Error::binder(format!(
                "Multiple assignments to same column \"{name}\""
            )));
        }
        columns.push(physical);
        if matches!(expression.kind, ParsedExpressionKind::Default) {
            update_expressions.push(Expression::new(
                ExpressionType::ValueDefault,
                column.logical_type.clone(),
                ExpressionKind::Default,
            ));
            continue;
        }
        let mut bound = {
            let (inner, outer) = scope.split();
            let mut binder = UpdateBinder::new(inner, outer, column.logical_type.clone());
            binder.bind(expression, true)?
        };
        if let Some(root) = root.as_deref_mut() {
            plan_subqueries(scope, &mut bound, root)?;
        }
        update_expressions.push(Expression::column_ref(
            "",
            bound.return_type.clone(),
            ColumnBinding::new(proj_index, projection_expressions.len() as u64),
            0,
        ));
        projection_expressions.push(bound);
    }
    Ok(())
}

/// The columns of a `physical_index_set_t` filled from `columns`, in libc++'s iteration
/// order (`std::hash<idx_t>` is the identity).
fn physical_index_set(columns: &[usize]) -> Vec<usize> {
    let mut set = rustdb_common::libcxx::UnorderedOrder::default();
    for &column in columns {
        set.insert(column as u64, column, |a, b| a == b);
    }
    set.order().into_iter().map(|node| set.keys()[node]).collect()
}

/// `LogicalUpdate::BindExtraColumns`: adds `c = c` updates for columns in `bound_columns`
/// that are not updated yet.
fn bind_extra_columns(
    table: &rustdb_catalog::TableEntry,
    get: &mut LogicalOperator,
    set: &mut UpdateSet,
    bound_columns: &[usize],
) -> Result<()> {
    let proj_index = set.proj_index;
    let UpdateSet {
        columns,
        update_expressions,
        projection_expressions,
        ..
    } = set;
    let bound_columns = physical_index_set(bound_columns);
    if bound_columns.len() <= 1 {
        return Ok(());
    }
    if bound_columns.iter().all(|column| columns.contains(column)) {
        return Ok(());
    }
    for physical in bound_columns {
        if columns.contains(&physical) {
            continue;
        }
        let logical = table
            .physical_to_logical(physical)
            .ok_or_else(|| Error::internal("unknown physical column"))?;
        let ty = table.columns[logical].logical_type.clone();
        update_expressions.push(Expression::column_ref(
            "",
            ty.clone(),
            ColumnBinding::new(proj_index, projection_expressions.len() as u64),
            0,
        ));
        let (table_index, position) = add_scan_column(get, logical)?;
        projection_expressions.push(Expression::column_ref(
            "",
            ty,
            ColumnBinding::new(table_index, position),
            0,
        ));
        columns.push(physical);
    }
    Ok(())
}
