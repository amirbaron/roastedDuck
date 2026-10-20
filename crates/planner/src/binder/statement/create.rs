//! `CREATE SCHEMA`, `CREATE TABLE [AS]`, `CREATE VIEW` and `DROP` (DuckDB's
//! `bind_create.cpp`, `bind_create_table.cpp` and `bind_drop.cpp`).

use std::sync::Arc;

use rustdb_ast::{
    CatalogEntryInfo, CatalogType as AstCatalogType, ColumnDefinition, CompressionType, Constraint,
    CreateInfo, CreateInfoKind, DropInfo, ForeignKeyConstraint, ForeignKeyType, LogicalDependency,
    LogicalDependencyList, NotNullConstraint, OnEntryNotFound, TableColumnType,
};
use rustdb_catalog::{CatalogType, EntryKind, TableEntry};
use rustdb_common::{Error, LogicalIndex, PhysicalIndex, Result};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use super::super::expression::{bind_logical_type, bind_logical_type_in_schema};
use super::super::query_node::bind_node;
use super::super::{BindScope, BoundStatement, Scope};
use super::table::{bind_default_value_in, bind_schema_or_catalog, table_of};
use crate::expression_binder::{CheckColumn, ExpressionBinder};
use crate::generated::enums::LogicalOperatorType;
use crate::operator::{BoundCreateTableInfo, LogicalKind, LogicalOperator};
use crate::prepared::StatementReturnType;

fn count_result(plan: LogicalOperator) -> BoundStatement {
    BoundStatement::new(plan, vec!["Count".to_owned()], vec![LogicalType::BIGINT])
}

/// `CREATE ...` (DuckDB's `Binder::Bind(CreateStatement &)`).
pub fn bind_create(scope: &mut BindScope<'_>, info: &mut CreateInfo) -> Result<BoundStatement> {
    let mut return_type = StatementReturnType::Nothing;
    let plan = match &info.kind {
        CreateInfoKind::Schema => {
            let catalog = bind_catalog(scope, &info.catalog);
            // `PhysicalCreateSchema` and `CatalogSet::CheckCatalogEntryInvariants`.
            if catalog.eq_ignore_ascii_case("system") {
                return Err(Error::binder("Cannot create schema in system catalog"));
            }
            if catalog.eq_ignore_ascii_case(rustdb_catalog::TEMP_CATALOG)
                && !info.schema.eq_ignore_ascii_case(rustdb_catalog::DEFAULT_SCHEMA)
            {
                return Err(Error::invalid_input(format!(
                    "Cannot create non-temporary entry \"{}\" in temporary catalog",
                    info.schema
                )));
            }
            info.catalog = catalog;
            LogicalOperator::new(LogicalKind::Create {
                operator_type: LogicalOperatorType::LogicalCreateSchema,
                info: Box::new(info.clone()),
            })
        }
        CreateInfoKind::View(_) => {
            bind_create_schema(scope, info)?;
            bind_create_view_info(scope, info)?;
            LogicalOperator::new(LogicalKind::Create {
                operator_type: LogicalOperatorType::LogicalCreateView,
                info: Box::new(info.clone()),
            })
        }
        CreateInfoKind::Sequence(_) => {
            bind_create_schema(scope, info)?;
            LogicalOperator::new(LogicalKind::Create {
                operator_type: LogicalOperatorType::LogicalCreateSequence,
                info: Box::new(info.clone()),
            })
        }
        CreateInfoKind::Index(_) => super::create_index::bind_create_index(scope, info)?,
        CreateInfoKind::Table(_) => {
            let (bound, query) = bind_create_table_info(scope, info.clone())?;
            let mut plan = LogicalOperator::new(LogicalKind::CreateTable(Box::new(bound)));
            if let Some(query) = query {
                return_type = StatementReturnType::ChangedRows;
                plan.add_child(query);
            }
            plan
        }
        CreateInfoKind::Type(_) => bind_create_type(scope, info)?,
        CreateInfoKind::Macro(_) | CreateInfoKind::TableMacro(_) => {
            super::create_macro::bind_create_function_info(scope, info)?;
            LogicalOperator::new(LogicalKind::Create {
                operator_type: LogicalOperatorType::LogicalCreateMacro,
                info: Box::new(info.clone()),
            })
        }
        CreateInfoKind::Secret(secret) => {
            let input = bind_create_secret(scope, secret, info.on_conflict)?;
            scope.with_properties(|properties| properties.return_type = StatementReturnType::QueryResult);
            return Ok(BoundStatement::new(
                LogicalOperator::new(LogicalKind::CreateSecret(Box::new(input))),
                vec!["Success".to_owned()],
                vec![LogicalType::BOOLEAN],
            ));
        }
    };
    scope.with_properties(|properties| properties.return_type = return_type);
    Ok(count_result(plan))
}

/// Evaluates one expression of `CREATE SECRET` (DuckDB binds them with a
/// `ConstantBinder` and executes them).
fn evaluate_secret_expression(
    scope: &mut BindScope<'_>,
    expression: &rustdb_ast::ParsedExpression,
) -> Result<Value> {
    let mut copy = expression.clone();
    if crate::expression_binder::parsed_has_parameter(&mut copy) {
        return Err(Error::invalid_input(
            "Create Secret expressions can not have parameters!",
        ));
    }
    let bound = {
        let (inner, outer) = scope.split();
        let mut binder = crate::expression_binder::ConstantBinder::new(inner, outer, "Secret Parameter");
        binder.bind(&mut copy, false)?
    };
    scope.context.evaluator.evaluate_scalar(&bound)
}

/// The `SECRET_ENTRY` case of DuckDB's `Binder::Bind(CreateStatement &)`: the type,
/// provider, scope and options are evaluated; the secret manager checks them.
fn bind_create_secret(
    scope: &mut BindScope<'_>,
    info: &rustdb_ast::CreateSecretInfo,
    on_conflict: rustdb_ast::OnCreateConflict,
) -> Result<crate::operator::CreateSecretInput> {
    let mut lowered = |expression: Option<&rustdb_ast::ParsedExpression>| -> Result<String> {
        match expression {
            Some(expression) => Ok(evaluate_secret_expression(scope, expression)?
                .to_varchar()?
                .to_lowercase()),
            None => Ok(String::new()),
        }
    };
    let provider = lowered(info.provider.as_deref())?;
    let secret_type = lowered(info.secret_type.as_deref())?;
    let mut scope_strings = Vec::new();
    if let Some(expression) = &info.scope {
        let value = evaluate_secret_expression(scope, expression)?;
        let ty = value.logical_type().clone();
        if ty == LogicalType::VARCHAR {
            scope_strings.push(value.to_varchar()?);
        } else if ty.id() == LogicalTypeId::List && ty.child_type() == Some(&LogicalType::VARCHAR) {
            for item in value.children().unwrap_or_default() {
                scope_strings.push(item.to_varchar()?);
            }
        } else if ty.id() == LogicalTypeId::Struct {
            for child in value.children().unwrap_or_default() {
                if *child.logical_type() != LogicalType::VARCHAR {
                    return Err(Error::invalid_input(
                        "Invalid input to scope parameter of create secret: only struct of VARCHARs is allowed",
                    ));
                }
                scope_strings.push(child.to_varchar()?);
            }
        } else {
            return Err(Error::invalid_input(
                "Create Secret scope must be of type VARCHAR or LIST(VARCHAR)",
            ));
        }
    }
    let mut options = Vec::with_capacity(info.options.len());
    for (name, expression) in info.options.iter() {
        options.push((name.to_owned(), evaluate_secret_expression(scope, expression)?));
    }
    Ok(crate::operator::CreateSecretInput {
        secret_type,
        provider,
        storage: info.storage_type.clone(),
        name: info.name.clone(),
        scope: scope_strings,
        options,
        on_conflict,
        persist_type: info.persist_type,
    })
}

/// `CREATE TYPE` (the `TYPE_ENTRY` case of DuckDB's `Binder::Bind(CreateStatement &)`):
/// binds the aliased type, or plans the query of `AS ENUM (SELECT ...)` whose distinct
/// values become the enum's members.
fn bind_create_type(scope: &mut BindScope<'_>, info: &mut CreateInfo) -> Result<LogicalOperator> {
    bind_create_schema(scope, info)?;
    let CreateInfoKind::Type(create_type) = &mut info.kind else {
        return Err(Error::internal("expected a type"));
    };
    let query = if let Some(mut query) = create_type.query.take() {
        let mut child = scope.binder.child()?;
        let bound = {
            let mut child_scope = BindScope {
                binder: &mut child,
                outer: Some(scope as &mut dyn Scope),
            };
            bind_node(&mut child_scope, &mut query.node)?
        };
        let [sql_type] = bound.types.as_slice() else {
            return Err(Error::binder("The query must return a single column"));
        };
        let mut plan = bound.plan;
        if sql_type.id() != LogicalTypeId::Varchar {
            // Cast the values to VARCHAR in a projection.
            let binding = plan
                .column_bindings()
                .first()
                .copied()
                .ok_or_else(|| Error::internal("query without columns"))?;
            let reference =
                crate::expression::Expression::column_ref(String::new(), sql_type.clone(), binding, 0);
            let cast = crate::function_binder::add_cast_to_type(
                &scope.context,
                reference,
                &LogicalType::VARCHAR,
                false,
            )?;
            plan = LogicalOperator::projection(scope.binder.generate_table_index(), vec![cast], plan);
        }
        Some(plan)
    } else {
        create_type.logical_type = bind_logical_type(&scope.context, &create_type.logical_type)?;
        None
    };
    let mut plan = LogicalOperator::new(LogicalKind::Create {
        operator_type: LogicalOperatorType::LogicalCreateType,
        info: Box::new(info.clone()),
    });
    if let Some(query) = query {
        plan.add_child(query);
    }
    Ok(plan)
}

/// The catalog a schema is created in (`Binder::BindCatalog`).
fn bind_catalog(scope: &BindScope<'_>, catalog: &str) -> String {
    match scope.context.catalog(catalog) {
        Some(found) if !catalog.is_empty() => found.name().to_owned(),
        _ => scope.context.default_catalog().to_owned(),
    }
}

/// Resolves the catalog and schema an entry is created in, which must exist
/// (`Binder::BindCreateSchema`).
pub fn bind_create_schema(scope: &BindScope<'_>, info: &mut CreateInfo) -> Result<()> {
    let (mut catalog, mut schema) = bind_schema_or_catalog(scope, &info.catalog, &info.schema)?;
    // `Binder::SearchSchema`: temporary entries go to the connection's `temp` catalog
    // (without one, to the default database).
    let has_temp = scope.context.catalog(rustdb_catalog::TEMP_CATALOG).is_some();
    if catalog.is_empty() && info.temporary && has_temp {
        rustdb_catalog::TEMP_CATALOG.clone_into(&mut catalog);
    }
    if !has_temp && catalog.eq_ignore_ascii_case(rustdb_catalog::TEMP_CATALOG) {
        catalog.clear();
    }
    match (catalog.is_empty(), schema.is_empty()) {
        (true, true) => {
            scope.context.default_catalog().clone_into(&mut catalog);
            scope.context.default_schema().clone_into(&mut schema);
        }
        (false, true) => schema = scope.context.default_schema_for(&catalog),
        (true, false) => catalog = scope.context.default_catalog_for(&schema),
        (false, false) => {}
    }
    if has_temp {
        let is_temp = catalog.eq_ignore_ascii_case(rustdb_catalog::TEMP_CATALOG);
        if !info.temporary && is_temp {
            return Err(Error::parser(
                "Only TEMPORARY table names can use the \"temp\" catalog",
            ));
        }
        if info.temporary && !is_temp {
            return Err(Error::parser(
                "TEMPORARY table names can *only* use the \"temp\" catalog",
            ));
        }
    }
    let Some(found) = scope.context.catalog(&catalog) else {
        // `Catalog::GetSchema` (`BindSchema`).
        return Err(Error::catalog(format!("Catalog with name {catalog} does not exist!")));
    };
    if found.name().eq_ignore_ascii_case("system") {
        return Err(Error::binder("Cannot create entry in system catalog"));
    }
    let Some(schema_entry) = found.schema(&scope.context.transaction, &schema) else {
        return Err(Error::catalog(format!(
            "Schema with name {schema} does not exist!"
        )));
    };
    found.name().clone_into(&mut info.catalog);
    schema_entry.name().clone_into(&mut info.schema);
    Ok(())
}

/// `Binder::BindCreateViewInfo`: binds the view's query for its names and types.
fn bind_create_view_info(scope: &mut BindScope<'_>, info: &mut CreateInfo) -> Result<()> {
    let CreateInfoKind::View(view) = &mut info.kind else {
        return Err(Error::internal("expected a view"));
    };
    let mut query = view.query.clone();
    let mut view_binder = super::super::Binder::new(Arc::new(
        scope.context.with_view_search_path(&info.catalog, &info.schema),
    ))?;
    view_binder.can_contain_nulls = true;
    let record = scope.context.config.enable_view_dependencies;
    let previous = record.then(|| scope.context.start_recording_dependencies());
    let bound = {
        let mut view_scope = BindScope::root(&mut view_binder);
        bind_node(&mut view_scope, &mut query.node)
    };
    if let Some(previous) = previous {
        let recorded = scope.context.stop_recording_dependencies(previous);
        add_recorded_dependencies(&mut info.dependencies, recorded, &info.catalog, info.temporary);
    }
    let bound = bound?;
    if view.aliases.len() > bound.names.len() {
        return Err(Error::binder("More VIEW aliases than columns in query result"));
    }
    view.types = bound.types;
    view.names = bound.names;
    Ok(())
}

/// Adds the dependencies the catalog lookups in `recorded` give an entry created in
/// `catalog` (DuckDB's catalog lookup callbacks): entries of other catalogs and internal
/// types are no dependencies. Temporary entries belong to DuckDB's `temp` catalog, so a
/// temporary entry only depends on temporary entries and vice versa.
pub(crate) fn add_recorded_dependencies(
    dependencies: &mut LogicalDependencyList,
    recorded: Vec<crate::context::EntryLookup>,
    catalog: &str,
    temporary: bool,
) {
    for lookup in recorded {
        if !lookup.catalog.eq_ignore_ascii_case(catalog)
            || lookup.entry.temporary != temporary
            || (lookup.entry.internal && matches!(lookup.entry.kind, EntryKind::Type(_)))
        {
            continue;
        }
        let dependency = LogicalDependency {
            entry: CatalogEntryInfo {
                catalog_type: lookup.entry.dependency_type(),
                schema: lookup.schema,
                name: lookup.entry.name().to_owned(),
            },
            catalog: lookup.catalog,
        };
        if !dependencies.set.contains(&dependency) {
            dependencies.set.push(dependency);
        }
    }
}

/// A column definition with `name` and `ty`.
fn column(name: String, ty: LogicalType) -> ColumnDefinition {
    ColumnDefinition {
        name,
        logical_type: ty,
        expression: None,
        category: TableColumnType::Standard,
        compression_type: CompressionType::CompressionAuto,
        comment: Value::null(LogicalType::SQLNULL),
        tags: rustdb_common::OrderedMap::default(),
    }
}

/// Adds `definition`, renaming it like DuckDB's `ColumnList` with duplicates allowed
/// (`a`, `a_1`, ...).
fn push_deduplicated(columns: &mut Vec<ColumnDefinition>, mut definition: ColumnDefinition) {
    let base = definition.name.clone();
    let mut index = 1;
    while columns
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case(&definition.name))
    {
        definition.name = format!("{base}_{index}");
        index += 1;
    }
    columns.push(definition);
}

/// `Binder::BindCreateTableInfo`: binds the column types, constraints and defaults (or the
/// query of `CREATE TABLE AS`). Returns the bound info and the query's plan.
pub fn bind_create_table_info(
    scope: &mut BindScope<'_>,
    mut info: CreateInfo,
) -> Result<(BoundCreateTableInfo, Option<LogicalOperator>)> {
    bind_create_schema(scope, &mut info)?;
    let catalog = info.catalog.clone();
    let schema = info.schema.clone();
    let CreateInfoKind::Table(table) = &mut info.kind else {
        return Err(Error::internal("expected a table"));
    };
    let mut query_plan = None;
    if let Some(mut query) = table.query.take() {
        let mut child = scope.binder.child()?;
        let bound = {
            let mut child_scope = BindScope {
                binder: &mut child,
                outer: Some(scope as &mut dyn Scope),
            };
            bind_node(&mut child_scope, &mut query.node)?
        };
        let mut target: Vec<String> = table.columns.columns.iter().map(|c| c.name.clone()).collect();
        if target.len() > bound.types.len() {
            return Err(Error::binder(
                "Target table has more colum names than query result.",
            ));
        }
        for name in bound.names.iter().skip(target.len()) {
            target.push(name.clone());
        }
        let mut columns = Vec::with_capacity(target.len());
        for (name, ty) in target.into_iter().zip(&bound.types) {
            push_deduplicated(&mut columns, column(name, bind_logical_type(&scope.context, ty)?));
        }
        table.columns.columns = columns;
        query_plan = Some(bound.plan);
    } else {
        let previous = scope.context.start_recording_dependencies();
        let result = bind_table_definition(scope, &catalog, &schema, table);
        let recorded = scope.context.stop_recording_dependencies(previous);
        add_recorded_dependencies(&mut info.dependencies, recorded, &catalog, info.temporary);
        result?;
    }
    if !table
        .columns
        .columns
        .iter()
        .any(|c| c.category != TableColumnType::Generated)
    {
        return Err(Error::binder(
            "Creating a table without physical (non-generated) columns is not supported",
        ));
    }
    Ok((
        BoundCreateTableInfo {
            catalog,
            schema,
            info,
        },
        query_plan,
    ))
}

/// Binds the column types, generated columns, constraints and defaults of a `CREATE
/// TABLE` without a query.
fn bind_table_definition(
    scope: &mut BindScope<'_>,
    catalog: &str,
    schema: &str,
    table: &mut rustdb_ast::CreateTableInfo,
) -> Result<()> {
    for column in &mut table.columns.columns {
        if column.logical_type.id() != rustdb_types::LogicalTypeId::Any {
            column.logical_type =
                bind_logical_type_in_schema(&scope.context, &column.logical_type, catalog, schema)?;
        }
    }
    bind_generated_columns(scope, &table.table, &mut table.columns.columns)?;
    bind_create_table_constraints(
        scope,
        &table.table,
        schema,
        &mut table.columns.columns,
        &mut table.constraints,
    )?;
    if any_constraint_references_generated_column(&table.columns.columns, &table.constraints) {
        return Err(Error::binder(
            "Constraints on generated columns are not supported yet",
        ));
    }
    // `BindNewConstraints` binds the CHECK expressions first.
    let check_columns = generated_check_columns(&table.columns.columns);
    for constraint in &table.constraints {
        if let Constraint::Check(check) = constraint {
            let mut expression = (*check.expression).clone();
            let mut binder = scope.binder.child()?;
            crate::expression_binder::CheckBinder::new(&mut binder, None, &table.table, &check_columns)
                .bind(&mut expression, true)?;
        }
    }
    bind_new_constraints(&table.table, &table.columns.columns, &mut table.constraints)?;
    // Validate the default values (`BindDefaultValues`); names resolve in the table's
    // catalog and schema.
    let path = (catalog.to_owned(), schema.to_owned());
    for column in &table.columns.columns {
        if column.category != TableColumnType::Generated {
            bind_default_value_in(
                scope,
                column.expression.as_deref(),
                &column.logical_type,
                Some(&path),
            )?;
        }
    }
    Ok(())
}

/// The names of the columns `expression` references (`ColumnDefinition::
/// GetListOfDependencies`); lambdas are rejected.
fn referenced_columns(expression: &rustdb_ast::ParsedExpression, out: &mut Vec<String>) -> Result<()> {
    struct Collect<'a>(&'a mut Vec<String>, bool);
    impl rustdb_ast::visit::VisitorMut for Collect<'_> {
        fn visit_expression(&mut self, expression: &mut rustdb_ast::ParsedExpression) {
            if let rustdb_ast::ParsedExpressionKind::Lambda(_) = &expression.kind {
                self.1 = true;
            }
            if let rustdb_ast::ParsedExpressionKind::ColumnRef(column) = &expression.kind
                && let Some(name) = column.column_names.last()
                && !self.0.iter().any(|seen| seen.eq_ignore_ascii_case(name))
            {
                self.0.push(name.clone());
            }
            rustdb_ast::visit::walk_expression(self, expression);
        }
    }
    let mut copy = expression.clone();
    let mut collect = Collect(out, false);
    rustdb_ast::visit::VisitorMut::visit_expression(&mut collect, &mut copy);
    if collect.1 {
        return Err(Error::not_implemented(
            "Lambda functions are currently not supported in generated columns.",
        ));
    }
    Ok(())
}

/// Depth-first visit of the generated columns for their bind order; a column seen again
/// while its dependencies are visited is a cycle.
fn visit_generated(
    column: usize,
    columns: &[ColumnDefinition],
    dependencies: &[Vec<usize>],
    state: &mut [u8],
    order: &mut Vec<usize>,
) -> Result<()> {
    match state[column] {
        2 => return Ok(()),
        1 => {
            return Err(Error::invalid_input(
                "Circular dependency encountered when resolving generated column expressions",
            ));
        }
        _ => {}
    }
    state[column] = 1;
    for &dependency in &dependencies[column] {
        if columns[dependency].category == TableColumnType::Generated {
            visit_generated(dependency, columns, dependencies, state, order)?;
        }
    }
    state[column] = 2;
    order.push(column);
    Ok(())
}

/// `Binder::BindGeneratedColumns`: checks the columns generated expressions reference,
/// rejects circular dependencies, and binds the expressions in dependency order; a
/// generated column without a type takes its expression's type (wrapped in a cast).
fn bind_generated_columns(
    scope: &mut BindScope<'_>,
    table: &str,
    columns: &mut [ColumnDefinition],
) -> Result<()> {
    let count = columns.len();
    let mut dependencies: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (index, column) in columns.iter().enumerate() {
        if column.category != TableColumnType::Generated {
            continue;
        }
        let Some(expression) = &column.expression else {
            continue;
        };
        let mut names = Vec::new();
        referenced_columns(expression, &mut names)?;
        for name in names {
            let Some(dependency) = columns.iter().position(|c| c.name.eq_ignore_ascii_case(&name)) else {
                return Err(Error::binder(format!(
                    "Column \"{name}\" referenced by generated column does not exist"
                )));
            };
            dependencies[index].push(dependency);
        }
    }
    // Order the generated columns so that every column comes after the generated
    // columns it references (`ColumnDependencyManager::GetBindOrder`).
    let mut state = vec![0u8; count];
    let mut order = Vec::with_capacity(count);
    for index in 0..count {
        if columns[index].category == TableColumnType::Generated {
            visit_generated(index, columns, &dependencies, &mut state, &mut order)?;
        }
    }
    for index in order {
        let check_columns = generated_check_columns(columns);
        let Some(mut expression) = columns[index].expression.as_deref().cloned() else {
            continue;
        };
        let bound = {
            let mut binder = scope.binder.child()?;
            let mut generated_binder =
                crate::expression_binder::CheckBinder::generated(&mut binder, None, table, &check_columns);
            generated_binder.bind(&mut expression, true)?
        };
        let column = &mut columns[index];
        if column.logical_type.id() == rustdb_types::LogicalTypeId::Any {
            // `ColumnDefinition::ChangeGeneratedExpressionType`.
            column.logical_type = bound.return_type.clone();
            if let Some(expression) = column.expression.take() {
                column.expression = Some(Box::new(rustdb_ast::ParsedExpression::new(
                    rustdb_ast::ExpressionType::OperatorCast,
                    rustdb_ast::ParsedExpressionKind::Cast(rustdb_ast::CastExpression {
                        child: expression,
                        cast_type: bound.return_type.clone(),
                        try_cast: false,
                    }),
                )));
            }
        }
    }
    Ok(())
}

/// The columns of a table being created, as generated expressions see them.
fn generated_check_columns(columns: &[ColumnDefinition]) -> Vec<CheckColumn> {
    let mut physical = 0;
    columns
        .iter()
        .map(|column| {
            let generated = column.category == TableColumnType::Generated;
            let index = if generated {
                None
            } else {
                physical += 1;
                Some(physical - 1)
            };
            CheckColumn {
                name: column.name.clone(),
                logical_type: column.logical_type.clone(),
                physical: index,
                generated: if generated {
                    column.expression.as_deref().cloned()
                } else {
                    None
                },
            }
        })
        .collect()
}

/// `AnyConstraintReferencesGeneratedColumn`.
fn any_constraint_references_generated_column(
    columns: &[ColumnDefinition],
    constraints: &[Constraint],
) -> bool {
    let generated = |name: &str| {
        columns
            .iter()
            .any(|c| c.category == TableColumnType::Generated && c.name.eq_ignore_ascii_case(name))
    };
    let generated_index = |index: u64| {
        usize::try_from(index)
            .ok()
            .and_then(|index| columns.get(index))
            .is_some_and(|c| c.category == TableColumnType::Generated)
    };
    if !columns.iter().any(|c| c.category == TableColumnType::Generated) {
        return false;
    }
    constraints.iter().any(|constraint| match constraint {
        Constraint::Check(check) => {
            let mut names = Vec::new();
            // Lambdas in CHECK constraints are rejected by the CHECK binder instead.
            let _ = referenced_columns(&check.expression, &mut names);
            names.iter().any(|name| generated(name))
        }
        Constraint::NotNull(not_null) => generated_index(not_null.index.0),
        Constraint::Unique(unique) => {
            if unique.index.0 == u64::MAX {
                unique.columns.iter().any(|name| generated(name))
            } else {
                generated_index(unique.index.0)
            }
        }
        Constraint::ForeignKey(fk) => fk.fk_columns.iter().any(|name| generated(name)),
    })
}

/// The physical index of each column of a column list (`None` for generated columns).
fn physical_index(columns: &[ColumnDefinition], logical: usize) -> Option<usize> {
    if columns.get(logical)?.category == TableColumnType::Generated {
        return None;
    }
    Some(
        columns[..logical]
            .iter()
            .filter(|c| c.category != TableColumnType::Generated)
            .count(),
    )
}

/// `Binder::BindNewConstraints`: a PRIMARY KEY makes its columns NOT NULL.
fn bind_new_constraints(
    table: &str,
    columns: &[ColumnDefinition],
    constraints: &mut Vec<Constraint>,
) -> Result<()> {
    let mut primary_keys: Option<Vec<usize>> = None;
    let mut not_null = Vec::new();
    for constraint in constraints.iter() {
        match constraint {
            Constraint::NotNull(constraint) => not_null.push(constraint.index.0 as usize),
            Constraint::Unique(unique) if unique.is_primary_key => {
                if primary_keys.is_some() {
                    return Err(Error::parser(format!(
                        "table \"{table}\" has more than one primary key"
                    )));
                }
                let keys = if unique.index.0 == u64::MAX {
                    let mut keys = Vec::new();
                    for name in &unique.columns {
                        let Some(index) = columns.iter().position(|c| c.name.eq_ignore_ascii_case(name))
                        else {
                            return Err(Error::catalog(format!(
                                "table \"{table}\" does not have a column named \"{name}\""
                            )));
                        };
                        if columns[index].category == TableColumnType::Generated {
                            return Err(Error::binder(format!(
                                "cannot create a PRIMARY KEY on a generated column: {}",
                                columns[index].name
                            )));
                        }
                        if keys.contains(&index) {
                            return Err(Error::parser(format!(
                                "column \"{name}\" appears twice in primary key constraint"
                            )));
                        }
                        keys.push(index);
                    }
                    keys
                } else {
                    vec![unique.index.0 as usize]
                };
                primary_keys = Some(keys);
            }
            Constraint::Unique(unique) if unique.index.0 == u64::MAX => {
                let mut seen = Vec::new();
                for name in &unique.columns {
                    let Some(index) = columns.iter().position(|c| c.name.eq_ignore_ascii_case(name)) else {
                        return Err(Error::catalog(format!(
                            "table \"{table}\" does not have a column named \"{name}\""
                        )));
                    };
                    if seen.contains(&index) {
                        return Err(Error::parser(format!(
                            "column \"{name}\" appears twice in primary key constraint"
                        )));
                    }
                    seen.push(index);
                }
            }
            _ => {}
        }
    }
    if let Some(keys) = primary_keys {
        for logical in keys {
            if !not_null.contains(&logical) {
                constraints.push(Constraint::NotNull(NotNullConstraint {
                    index: LogicalIndex(logical as u64),
                }));
            }
        }
    }
    Ok(())
}

/// The key column names of a UNIQUE constraint.
fn unique_names(columns: &[ColumnDefinition], unique: &rustdb_ast::UniqueConstraint) -> Vec<String> {
    if unique.index.0 == u64::MAX {
        unique.columns.clone()
    } else {
        columns
            .get(unique.index.0 as usize)
            .map(|c| vec![c.name.clone()])
            .unwrap_or_default()
    }
}

/// `FindMatchingPrimaryKeyColumns`.
fn find_matching_primary_key_columns(
    columns: &[ColumnDefinition],
    constraints: &[Constraint],
    fk: &mut ForeignKeyConstraint,
) -> Result<()> {
    let find_primary_key = fk.pk_columns.is_empty();
    let mut found_constraint = false;
    for constraint in constraints {
        let Constraint::Unique(unique) = constraint else {
            continue;
        };
        if find_primary_key && !unique.is_primary_key {
            continue;
        }
        found_constraint = true;
        let pk_names = unique_names(columns, unique);
        if find_primary_key {
            if pk_names.len() != fk.fk_columns.len() {
                return Err(Error::binder(format!(
                    "Failed to create foreign key: number of referencing ({}) and referenced columns ({}) differ",
                    fk.fk_columns.join(","),
                    pk_names.join(",")
                )));
            }
            fk.pk_columns = pk_names;
            return Ok(());
        }
        if pk_names.len() != fk.fk_columns.len() {
            continue;
        }
        if fk
            .pk_columns
            .iter()
            .zip(&pk_names)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return Ok(());
        }
    }
    if !found_constraint {
        let search = if find_primary_key {
            "primary key"
        } else {
            "primary key or unique constraint"
        };
        return Err(Error::binder(format!(
            "Failed to create foreign key: there is no {search} for referenced table \"{}\"",
            fk.table
        )));
    }
    for name in &fk.pk_columns {
        if !columns.iter().any(|c| c.name.eq_ignore_ascii_case(name)) {
            return Err(Error::binder(format!(
                "Failed to create foreign key: referenced table \"{}\" does not have a column named \"{name}\"",
                fk.table
            )));
        }
    }
    Err(Error::binder(format!(
        "Failed to create foreign key: referenced table \"{}\" does not have a primary key or unique constraint on the columns {}",
        fk.table,
        fk.pk_columns.join(",")
    )))
}

/// `FindForeignKeyIndexes`.
fn find_foreign_key_indexes(columns: &[ColumnDefinition], names: &[String]) -> Result<Vec<PhysicalIndex>> {
    let mut indexes = Vec::with_capacity(names.len());
    for name in names {
        let Some(logical) = columns.iter().position(|c| c.name.eq_ignore_ascii_case(name)) else {
            return Err(Error::binder(format!(
                "column \"{name}\" named in key does not exist"
            )));
        };
        let Some(physical) = physical_index(columns, logical) else {
            return Err(Error::binder(format!(
                "Failed to create foreign key: referenced column \"{}\" is a generated column",
                columns[logical].name
            )));
        };
        indexes.push(PhysicalIndex(physical as u64));
    }
    Ok(indexes)
}

fn physical_column(columns: &[ColumnDefinition], physical: usize) -> Option<&ColumnDefinition> {
    columns
        .iter()
        .filter(|c| c.category != TableColumnType::Generated)
        .nth(physical)
}

/// `CheckForeignKeyTypes`.
fn check_foreign_key_types(
    pk_columns: &[ColumnDefinition],
    fk_columns: &[ColumnDefinition],
    fk: &ForeignKeyConstraint,
) -> Result<()> {
    for (pk, fk_key) in fk.pk_keys.iter().zip(&fk.fk_keys) {
        let (Some(pk_col), Some(fk_col)) = (
            physical_column(pk_columns, pk.0 as usize),
            physical_column(fk_columns, fk_key.0 as usize),
        ) else {
            continue;
        };
        if pk_col.logical_type != fk_col.logical_type {
            return Err(Error::binder(format!(
                "Failed to create foreign key: incompatible types between column \"{}\" (\"{}\") and column \"{}\" (\"{}\")",
                pk_col.name, pk_col.logical_type, fk_col.name, fk_col.logical_type
            )));
        }
    }
    Ok(())
}

/// The column definitions of an existing table entry.
fn entry_column_definitions(table: &TableEntry) -> Vec<ColumnDefinition> {
    table
        .columns
        .iter()
        .map(|c| {
            let mut definition = column(c.name.clone(), c.logical_type.clone());
            if c.generated.is_some() {
                definition.category = TableColumnType::Generated;
            }
            definition
        })
        .collect()
}

/// `BindCreateTableConstraints`: resolves the referenced columns of foreign keys.
fn bind_create_table_constraints(
    scope: &BindScope<'_>,
    table_name: &str,
    schema: &str,
    columns: &mut [ColumnDefinition],
    constraints: &mut [Constraint],
) -> Result<()> {
    let own_constraints = constraints.to_vec();
    for constraint in constraints.iter_mut() {
        let Constraint::ForeignKey(fk) = constraint else {
            continue;
        };
        if fk.fk_type != ForeignKeyType::FkTypeForeignKeyTable {
            continue;
        }
        if !fk.pk_keys.is_empty() && !fk.fk_keys.is_empty() {
            return Ok(());
        }
        fk.fk_keys = find_foreign_key_indexes(columns, &fk.fk_columns)?;
        if fk.table.eq_ignore_ascii_case(table_name) {
            fk.fk_type = ForeignKeyType::FkTypeSelfReferenceTable;
            find_matching_primary_key_columns(columns, &own_constraints, fk)?;
            fk.pk_keys = find_foreign_key_indexes(columns, &fk.pk_columns)?;
            check_foreign_key_types(columns, columns, fk)?;
            continue;
        }
        let found = scope
            .context
            .get_entry("", &fk.schema, CatalogType::Table, &fk.table, false)?
            .ok_or_else(|| Error::internal("foreign key table lookup"))?;
        let EntryKind::Table(pk_table) = &found.entry.kind else {
            return Err(Error::binder("cannot reference a VIEW with a FOREIGN KEY"));
        };
        fk.schema.clone_from(&found.schema);
        if !found.schema.eq_ignore_ascii_case(schema) {
            return Err(Error::binder(
                "Creating foreign keys across different schemas or catalogs is not supported",
            ));
        }
        let pk_columns = entry_column_definitions(pk_table);
        find_matching_primary_key_columns(&pk_columns, &pk_table.constraints, fk)?;
        fk.pk_keys = find_foreign_key_indexes(&pk_columns, &fk.pk_columns)?;
        check_foreign_key_types(&pk_columns, columns, fk)?;
        if let Some(storage) = &pk_table.storage {
            let keys: Vec<usize> = fk.pk_keys.iter().map(|k| k.0 as usize).collect();
            if storage
                .indexes()
                .find_foreign_key_index(
                    &keys,
                    rustdb_storage::index::table_index_list::ForeignKeyType::PrimaryKeyTable,
                )
                .is_none()
            {
                return Err(Error::binder(format!(
                    "Failed to create foreign key on {}({}): no UNIQUE or PRIMARY KEY constraint present on these columns",
                    found.entry.name(),
                    fk.pk_columns.join(",")
                )));
            }
        }
    }
    Ok(())
}

/// `DROP ...` (DuckDB's `Binder::Bind(DropStatement &)`).
pub fn bind_drop(scope: &mut BindScope<'_>, drop: &mut DropInfo) -> Result<BoundStatement> {
    scope.with_properties(|properties| properties.return_type = StatementReturnType::Nothing);
    if drop.catalog_type == AstCatalogType::SecretEntry {
        // `DROP SECRET` is executed by the secret manager (`PhysicalDrop`).
        return Ok(BoundStatement::new(
            LogicalOperator::new(LogicalKind::Drop(Box::new(drop.clone()))),
            vec!["Success".to_owned()],
            vec![LogicalType::BOOLEAN],
        ));
    }
    let ty = match drop.catalog_type {
        AstCatalogType::SchemaEntry => None,
        AstCatalogType::TableEntry => Some(CatalogType::Table),
        AstCatalogType::ViewEntry => Some(CatalogType::View),
        AstCatalogType::SequenceEntry => Some(CatalogType::Sequence),
        AstCatalogType::MacroEntry => Some(CatalogType::Macro),
        AstCatalogType::TableMacroEntry => Some(CatalogType::TableMacro),
        AstCatalogType::IndexEntry => Some(CatalogType::Index),
        AstCatalogType::TypeEntry => Some(CatalogType::Type),
        other => {
            return Err(Error::binder(format!(
                "Unknown catalog type for drop statement: '{}'",
                other.name()
            )));
        }
    };
    if let Some(ty) = ty {
        let (catalog, schema) = bind_schema_or_catalog(scope, &drop.catalog, &drop.schema)?;
        let if_exists = drop.if_not_found == OnEntryNotFound::ReturnNull;
        if ty == CatalogType::Type
            && (schema.is_empty() || schema.eq_ignore_ascii_case(rustdb_catalog::DEFAULT_SCHEMA))
            && rustdb_types::builtin_type_id(&drop.name).is_some()
        {
            return Err(Error::catalog(format!(
                "Cannot drop internal catalog entry \"{}\"!",
                drop.name
            )));
        }
        let mut found = scope.context.get_entry(&catalog, &schema, ty, &drop.name, true)?;
        if found.is_none() && ty == CatalogType::Macro {
            // `DROP MACRO` also drops table macros.
            found = scope
                .context
                .get_entry(&catalog, &schema, CatalogType::TableMacro, &drop.name, true)?;
            if found.is_some() {
                drop.catalog_type = AstCatalogType::TableMacroEntry;
            }
        }
        if found.is_none() && !if_exists {
            found = scope
                .context
                .get_entry(&catalog, &schema, ty, &drop.name, false)?;
        }
        if let Some(found) = found {
            if found.entry.internal {
                return Err(Error::catalog(format!(
                    "Cannot drop internal catalog entry \"{}\"!",
                    found.entry.name()
                )));
            }
            drop.catalog = found.catalog;
            drop.schema = found.schema;
        } else {
            drop.schema = if schema.is_empty() {
                scope.context.default_schema_for(if catalog.is_empty() {
                    scope.context.default_catalog()
                } else {
                    &catalog
                })
            } else {
                schema
            };
            drop.catalog = catalog;
        }
    } else if drop.catalog.is_empty() {
        scope.context.default_catalog().clone_into(&mut drop.catalog);
    }
    let plan = LogicalOperator::new(LogicalKind::Drop(Box::new(drop.clone())));
    Ok(BoundStatement::new(
        plan,
        vec!["Success".to_owned()],
        vec![LogicalType::BOOLEAN],
    ))
}

/// Whether the logical type is fully bound.
#[allow(dead_code, reason = "kept for parity with DuckDB's debug check")]
fn is_bound(ty: &LogicalType) -> bool {
    ty.id() != LogicalTypeId::Unbound
}

/// The table entry of a found catalog entry, for DML on it.
#[allow(dead_code, reason = "shared helper")]
pub fn table_entry(entry: &rustdb_catalog::CatalogEntry) -> Result<&TableEntry> {
    table_of(entry)
}
