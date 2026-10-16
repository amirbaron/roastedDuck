//! `CREATE INDEX` (DuckDB's `INDEX_ENTRY` case of `Binder::Bind(CreateStatement &)` and
//! `IndexBinder::BindCreateIndex`).

use rustdb_ast::{BaseTableRef, CreateInfo, CreateInfoKind, TableRef, TableRefKind};
use rustdb_catalog::TableEntry;
use rustdb_common::{Error, Result};
use rustdb_types::LogicalType;

use super::super::BindScope;
use super::super::tableref::bind_table_ref;
use super::table::table_of;
use crate::expression::{Expression, ExpressionKind};
use crate::expression_binder::{ExpressionBinder, IndexBinder};
use crate::operator::{
    COLUMN_IDENTIFIER_ROW_ID, ColumnIndex, LogicalCreateIndex, LogicalKind, LogicalOperator,
};

/// Binds `CREATE INDEX`: the indexed table is bound as a base table scan, the key
/// expressions against it, and their column references become references to the
/// table's physical columns.
pub fn bind_create_index(scope: &mut BindScope<'_>, info: &mut CreateInfo) -> Result<LogicalOperator> {
    let CreateInfoKind::Index(index) = &info.kind else {
        return Err(Error::internal("expected CREATE INDEX"));
    };
    let mut table_ref = TableRef {
        alias: String::new(),
        sample: None,
        query_location: None,
        kind: TableRefKind::BaseTable(Box::new(BaseTableRef {
            schema_name: info.schema.clone(),
            table_name: index.table.clone(),
            column_name_alias: Vec::new(),
            catalog_name: info.catalog.clone(),
            at_clause: None,
        })),
    };
    let mut parsed_expressions = index.parsed_expressions.clone();
    let plan = bind_table_ref(scope, &mut table_ref)?.plan;
    let LogicalKind::Get(get) = &plan.kind else {
        return Err(Error::binder("can only create an index on a base table"));
    };
    let Some(entry) = get.table.clone() else {
        return Err(Error::binder("can only create an index on a base table"));
    };
    let table = table_of(&entry)?;
    if entry.temporary {
        info.temporary = true;
    }
    let mut expressions = Vec::with_capacity(parsed_expressions.len());
    for expression in &mut parsed_expressions {
        let (inner, outer) = scope.split();
        let mut binder = IndexBinder::new(inner, outer);
        expressions.push(binder.bind(expression, true)?);
    }
    // `InitCreateIndexInfo`: the scanned columns are the indexed ones.
    let column_ids: Vec<ColumnIndex> = match &get.shared_column_ids {
        Some(shared) => shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
        None => get.column_ids.clone(),
    };
    let mut physical = Vec::with_capacity(column_ids.len());
    let mut scan_types = Vec::with_capacity(column_ids.len() + 1);
    for column in &column_ids {
        if column.index == COLUMN_IDENTIFIER_ROW_ID {
            return Err(Error::binder("cannot create an index on the rowid"));
        }
        let logical =
            usize::try_from(column.index).map_err(|_| Error::internal("column index out of range"))?;
        physical.push(
            table
                .logical_to_physical(logical)
                .ok_or_else(|| Error::not_implemented("indexes on generated columns"))?,
        );
        scan_types.push(
            table
                .columns
                .get(logical)
                .map(|c| c.logical_type.clone())
                .ok_or_else(|| Error::internal("column index out of range"))?,
        );
    }
    scan_types.push(LogicalType::BIGINT);
    if physical.is_empty() {
        return Err(Error::binder(
            "CREATE INDEX does not refer to any columns in the base table!",
        ));
    }
    for expression in &mut expressions {
        to_physical_references(expression, get.table_index, &column_ids, table)?;
    }
    let schema = table.storage.as_ref().map_or_else(
        || rustdb_catalog::DEFAULT_SCHEMA.to_owned(),
        |storage| storage.info().schema_name(),
    );
    let names = table.columns.iter().map(|column| column.name.clone()).collect();
    // The index lives in the catalog of its table (`old.items` may name a catalog).
    let table_catalog = match &table_ref.kind {
        TableRefKind::BaseTable(base) => scope
            .context
            .get_entry(
                &base.catalog_name,
                &base.schema_name,
                rustdb_catalog::CatalogType::Table,
                &base.table_name,
                true,
            )?
            .map(|lookup| lookup.catalog),
        _ => None,
    };
    let mut info = info.clone();
    info.schema = schema;
    if let Some(catalog) = table_catalog {
        info.catalog = catalog;
    } else if info.catalog.is_empty() {
        scope.context.default_catalog().clone_into(&mut info.catalog);
    }
    if let CreateInfoKind::Index(index) = &mut info.kind {
        index.column_ids = physical.iter().map(|&p| p as u64).collect();
        index.scan_types = scan_types;
        index.names = names;
    }
    Ok(LogicalOperator::new(LogicalKind::CreateIndex(Box::new(
        LogicalCreateIndex {
            info,
            table: entry,
            expressions,
        },
    ))))
}

/// Replaces references to the scan's columns by references to the table's physical
/// columns.
fn to_physical_references(
    expression: &mut Expression,
    table_index: u64,
    column_ids: &[ColumnIndex],
    table: &TableEntry,
) -> Result<()> {
    if let ExpressionKind::ColumnRef(column) = &expression.kind {
        if column.binding.table_index != table_index {
            return Err(Error::internal("index expression references another table"));
        }
        let physical = usize::try_from(column.binding.column_index)
            .ok()
            .and_then(|position| column_ids.get(position))
            .and_then(|id| usize::try_from(id.index).ok())
            .and_then(|logical| table.logical_to_physical(logical))
            .ok_or_else(|| Error::internal("index expression column out of range"))?;
        *expression = Expression::reference(
            expression.alias.clone(),
            expression.return_type.clone(),
            physical as u64,
        );
        return Ok(());
    }
    let mut result = Ok(());
    expression.for_each_child_mut(|child| {
        if result.is_ok() {
            result = to_physical_references(child, table_index, column_ids, table);
        }
    });
    result
}
