//! Helpers shared by the statements that work on a table: finding the table entry,
//! binding its constraints and default values, and `RETURNING` (DuckDB's
//! `Binder::BindConstraints`, `BindDefaultValues` and `BindReturning`).

use std::sync::Arc;

use rustdb_ast::{Constraint, ParsedExpression, ParsedExpressionKind};
use rustdb_catalog::{CatalogEntry, CatalogType, EntryKind, TableEntry};
use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, Value};

use super::super::expression::expand_star_expression;
use super::super::{BindScope, Binder, Binding, BindingAlias, BindingKind, BoundStatement};
use crate::expression::Expression;
use crate::expression_binder::{CheckBinder, CheckColumn, ConstantBinder, ExpressionBinder, ReturningBinder};
use crate::operator::{
    BoundConstraint, BoundForeignKeyInfo, COLUMN_IDENTIFIER_ROW_ID, ColumnIndex, LogicalOperator,
    SharedColumnIds,
};
use crate::prepared::StatementReturnType;

/// The table entry of a catalog entry.
pub fn table_of(entry: &CatalogEntry) -> Result<&TableEntry> {
    match &entry.kind {
        EntryKind::Table(table) => Ok(table),
        _ => Err(Error::internal("expected a table entry")),
    }
}

/// A table looked up for INSERT/UPDATE/DELETE, with the schema it was found in.
pub struct FoundTable {
    pub schema: String,
    pub entry: Arc<CatalogEntry>,
}

/// `Catalog::GetEntry<TableCatalogEntry>`: the table called `name`; a view is an error.
pub fn find_table(scope: &BindScope<'_>, catalog: &str, schema: &str, name: &str) -> Result<FoundTable> {
    let (catalog, schema) = bind_schema_or_catalog(scope, catalog, schema);
    let found = scope
        .context
        .get_entry(&catalog, &schema, CatalogType::Table, name, false)?
        .ok_or_else(|| Error::internal("table lookup"))?;
    if !matches!(found.entry.kind, EntryKind::Table(_)) {
        return Err(Error::catalog(format!(
            "Existing object {} is of type View, trying to replace with type Table",
            found.entry.name()
        )));
    }
    Ok(FoundTable {
        schema: found.schema,
        entry: found.entry,
    })
}

/// `Binder::BindSchemaOrCatalog`: a schema name that names a catalog becomes the catalog.
pub fn bind_schema_or_catalog(scope: &BindScope<'_>, catalog: &str, schema: &str) -> (String, String) {
    if !schema.is_empty() && catalog.is_empty() && scope.context.catalog(schema).is_some() {
        return (schema.to_owned(), String::new());
    }
    (catalog.to_owned(), schema.to_owned())
}

/// "Table "t" does not have a column with name "x"" (`TableCatalogEntry::GetColumnIndex`).
pub fn missing_column_error(table: &TableEntry, table_name: &str, name: &str) -> Error {
    let names: Vec<String> = table.columns.iter().map(|column| column.name.clone()).collect();
    let candidates = rustdb_common::string_util::candidates_error_message(&names, name, "Did you mean");
    Error::binder(format!(
        "Table \"{table_name}\" does not have a column with name \"{name}\"\n{candidates}"
    ))
}

/// The columns of `table` as seen by CHECK constraints.
pub fn check_columns(table: &TableEntry) -> Vec<CheckColumn> {
    let mut physical = 0;
    table
        .columns
        .iter()
        .map(|column| {
            let index = if column.generated.is_none() {
                physical += 1;
                Some(physical - 1)
            } else {
                None
            };
            CheckColumn {
                name: column.name.clone(),
                logical_type: column.logical_type.clone(),
                physical: index,
                generated: column.generated.clone(),
            }
        })
        .collect()
}

/// Binds the constraints of `table` (DuckDB's `Binder::BindConstraints`).
pub fn bind_constraints(
    scope: &mut BindScope<'_>,
    table_name: &str,
    table: &TableEntry,
) -> Result<Vec<BoundConstraint>> {
    let columns = check_columns(table);
    let mut bound = Vec::with_capacity(table.constraints.len());
    for constraint in &table.constraints {
        bound.push(bind_constraint(scope, constraint, table_name, table, &columns)?);
    }
    // The generated columns are verified against appended rows (`VerifyAppendConstraints`).
    for column in &table.columns {
        let Some(generated) = &column.generated else {
            continue;
        };
        let mut expression = generated.clone();
        let mut binder = scope.binder.child()?;
        let mut generated_binder =
            crate::expression_binder::CheckBinder::generated(&mut binder, None, table_name, &columns)
                .with_target_type(column.logical_type.clone());
        let bound_expression = generated_binder.bind(&mut expression, true)?;
        bound.push(BoundConstraint::Generated {
            expression: bound_expression,
            text: format!("{} {} AS ({generated})", column.name, column.logical_type),
        });
    }
    Ok(bound)
}

/// `Binder::BindConstraint`.
pub fn bind_constraint(
    scope: &mut BindScope<'_>,
    constraint: &Constraint,
    table_name: &str,
    table: &TableEntry,
    columns: &[CheckColumn],
) -> Result<BoundConstraint> {
    Ok(match constraint {
        Constraint::Check(check) => {
            let mut expression = (*check.expression).clone();
            let mut binder = scope.binder.child()?;
            let mut check_binder = CheckBinder::new(&mut binder, None, table_name, columns);
            let bound = check_binder.bind(&mut expression, true)?;
            BoundConstraint::Check {
                expression: bound,
                bound_columns: check_binder.bound_columns.clone(),
                text: format!("CHECK({})", check.expression),
            }
        }
        Constraint::NotNull(not_null) => {
            let physical = table
                .logical_to_physical(not_null.index.0 as usize)
                .ok_or_else(|| Error::internal("NOT NULL constraint on a generated column"))?;
            BoundConstraint::NotNull(physical)
        }
        Constraint::Unique(unique) => {
            let keys = unique_keys(table_name, table, unique)?;
            BoundConstraint::Unique {
                keys,
                is_primary_key: unique.is_primary_key,
            }
        }
        Constraint::ForeignKey(fk) => BoundConstraint::ForeignKey(BoundForeignKeyInfo {
            fk_type: fk.fk_type,
            schema: fk.schema.clone(),
            table: fk.table.clone(),
            pk_keys: fk.pk_keys.iter().map(|k| k.0 as usize).collect(),
            fk_keys: fk.fk_keys.iter().map(|k| k.0 as usize).collect(),
        }),
    })
}

/// The physical key columns of a UNIQUE / PRIMARY KEY constraint
/// (`Binder::BindUniqueConstraint`).
pub fn unique_keys(
    table_name: &str,
    table: &TableEntry,
    unique: &rustdb_ast::UniqueConstraint,
) -> Result<Vec<usize>> {
    if unique.index.0 != u64::MAX {
        let physical = table
            .logical_to_physical(unique.index.0 as usize)
            .ok_or_else(|| Error::internal("UNIQUE constraint on a generated column"))?;
        return Ok(vec![physical]);
    }
    let mut keys = Vec::with_capacity(unique.columns.len());
    for name in &unique.columns {
        let Some((logical, column)) = table.column(name) else {
            return Err(Error::catalog(format!(
                "table \"{table_name}\" does not have a column named \"{name}\""
            )));
        };
        if column.generated.is_some() {
            return Err(Error::binder(format!(
                "cannot create a PRIMARY KEY on a generated column: {}",
                column.name
            )));
        }
        let physical = table
            .logical_to_physical(logical)
            .ok_or_else(|| Error::internal("generated column in a key"))?;
        if keys.contains(&physical) {
            return Err(Error::parser(format!(
                "column \"{name}\" appears twice in primary key constraint"
            )));
        }
        keys.push(physical);
    }
    Ok(keys)
}

/// Binds the default value of every stored column (DuckDB's `BindDefaultValues`); columns
/// without a default get a typed NULL.
pub fn bind_default_values(scope: &mut BindScope<'_>, table: &TableEntry) -> Result<Vec<Expression>> {
    let mut defaults = Vec::new();
    for (_, _, column) in table.physical_columns() {
        defaults.push(bind_default_value(
            scope,
            column.default_value.as_ref(),
            &column.logical_type,
        )?);
    }
    Ok(defaults)
}

/// Binds one `DEFAULT` expression with target type `ty`.
pub fn bind_default_value(
    scope: &mut BindScope<'_>,
    default: Option<&ParsedExpression>,
    ty: &LogicalType,
) -> Result<Expression> {
    let Some(default) = default else {
        return Ok(Expression::constant(Value::null(ty.clone())));
    };
    let mut copy = default.clone();
    if crate::expression_binder::parsed_has_parameter(&mut copy) {
        return Err(Error::binder("DEFAULT values cannot contain parameters"));
    }
    let mut binder = scope.binder.child()?;
    let mut constant_binder = ConstantBinder::new(&mut binder, None, "DEFAULT value");
    constant_binder.state().target_type = Some(ty.clone());
    constant_binder.bind(&mut copy, true)
}

/// Replaces `DEFAULT` in an expression by the column's default (DuckDB's
/// `TryReplaceDefaultExpression`).
pub fn expand_default(expr: &mut ParsedExpression, default: Option<&ParsedExpression>, ty: &LogicalType) {
    if !matches!(expr.kind, ParsedExpressionKind::Default) {
        return;
    }
    *expr = default
        .cloned()
        .unwrap_or_else(|| rustdb_ast::ParsedExpression::constant(Value::null(ty.clone())));
}

/// `RETURNING` (DuckDB's `Binder::BindReturning`): a projection over the modifying
/// operator, whose output columns are the table's stored columns.
pub fn bind_returning(
    scope: &mut BindScope<'_>,
    mut returning_list: Vec<ParsedExpression>,
    table_name: &str,
    table: &Arc<CatalogEntry>,
    alias: &str,
    table_index: u64,
    child: LogicalOperator,
) -> Result<BoundStatement> {
    let entry = table_of(table)?;
    let mut binder = Binder::new(Arc::clone(&scope.context))?;
    // Every stored column is output by the child in order: pre-register them so that the
    // binding positions are the physical column indexes.
    let shared: SharedColumnIds = SharedColumnIds::default();
    {
        let mut ids = shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for (_, logical, _) in entry.physical_columns() {
            ids.push(ColumnIndex::new(logical as u64));
        }
    }
    let names: Vec<String> = entry.columns.iter().map(|c| c.name.clone()).collect();
    let types: Vec<LogicalType> = entry.columns.iter().map(|c| c.logical_type.clone()).collect();
    let binding_alias = if alias.is_empty() {
        BindingAlias::new(table_name)
    } else {
        BindingAlias::new(alias)
    };
    binder.bind_context.add_binding(Binding::new(
        BindingKind::Table {
            bound_column_ids: shared,
            entry: Some(Arc::clone(table)),
            virtual_columns: vec![(COLUMN_IDENTIFIER_ROW_ID, "rowid".to_owned(), LogicalType::BIGINT)],
        },
        binding_alias,
        types,
        names,
        table_index,
    )?);
    let mut expanded = Vec::new();
    {
        let mut returning_scope = BindScope::root(&mut binder);
        for expression in std::mem::take(&mut returning_list) {
            expand_star_expression(&mut returning_scope, expression, &mut expanded)?;
        }
    }
    if expanded.is_empty() {
        return Err(Error::binder("RETURNING list is empty!"));
    }
    let mut result_names = Vec::new();
    let mut result_types = Vec::new();
    let mut projections = Vec::new();
    for mut expression in expanded {
        let mut returning_binder = ReturningBinder::new(&mut binder, None);
        let bound = returning_binder.bind(&mut expression, true)?;
        result_names.push(bound.name());
        result_types.push(bound.return_type.clone());
        projections.push(bound);
    }
    let projection = LogicalOperator::projection(scope.generate_table_index(), projections, child);
    scope.with_properties(|properties| properties.return_type = StatementReturnType::QueryResult);
    Ok(BoundStatement::new(projection, result_names, result_types))
}
