//! `DESCRIBE query`, `SHOW ...` (DuckDB's `bind_showref.cpp`): describing a query binds
//! it and materialises one row per output column; SHOW of catalog objects binds the
//! query DuckDB generates for it.

use std::sync::Arc;

use rustdb_ast::{QueryNode, ShowRef, ShowType, SqlStatementKind, SubqueryRef};
use rustdb_catalog::EntryKind;
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{DataChunk, LogicalType, Value};

use super::bind_subquery_ref;
use crate::ColumnBinding;
use crate::binder::query_node::bind_node;
use crate::binder::{BindScope, BoundStatement};
use crate::operator::{LogicalKind, LogicalOperator};

/// The catalog column a query's output column comes from, if it can be traced through
/// projections and pass-through operators (DuckDB's `FindBaseTableColumn`).
fn find_base_table_column(
    op: &LogicalOperator,
    binding: ColumnBinding,
) -> Option<(Arc<rustdb_catalog::CatalogEntry>, usize)> {
    match &op.kind {
        // Scans do not carry their catalog table yet, so table columns are described
        // like computed ones.
        LogicalKind::Get(_) => None,
        LogicalKind::Projection { table_index } => {
            if binding.table_index != *table_index {
                return None;
            }
            let expression = op.expressions.get(usize::try_from(binding.column_index).ok()?)?;
            let column = expression.as_column_ref()?;
            find_base_table_column(op.children.first()?, column.binding)
        }
        LogicalKind::Limit { .. }
        | LogicalKind::Order { .. }
        | LogicalKind::TopN { .. }
        | LogicalKind::Sample { .. }
        | LogicalKind::Distinct { .. }
        | LogicalKind::Filter { .. }
        | LogicalKind::ComparisonJoin(_)
        | LogicalKind::AnyJoin { .. }
        | LogicalKind::CrossProduct => op
            .children
            .iter()
            .find_map(|child| find_base_table_column(child, binding)),
        _ => None,
    }
}

/// `DESCRIBE query` (DuckDB's `Binder::BindShowQuery`).
fn bind_show_query(scope: &mut BindScope<'_>, query: &mut QueryNode) -> Result<BoundStatement> {
    let mut child_binder = scope.binder.child()?;
    let mut plan = {
        let mut child_scope = BindScope {
            binder: &mut child_binder,
            outer: Some(&mut *scope),
        };
        bind_node(&mut child_scope, query)?
    };
    crate::operator::sync_column_ids(&mut plan.plan);
    let names = ["column_name", "column_type", "null", "key", "default", "extra"]
        .map(str::to_owned)
        .to_vec();
    let types = vec![LogicalType::VARCHAR; 6];
    let bindings = plan.plan.column_bindings();
    let mut chunks = Vec::new();
    let mut rows: Vec<[Value; 6]> = Vec::new();
    for (column_index, (alias, ty)) in plan.names.iter().zip(&plan.types).enumerate() {
        let traced = bindings
            .get(column_index)
            .and_then(|binding| find_base_table_column(&plan.plan, *binding));
        let row = match traced {
            Some((entry, column)) => {
                let EntryKind::Table(table) = &entry.kind else {
                    return Err(Error::internal("traced column is not a table column"));
                };
                let definition = table
                    .columns
                    .get(column)
                    .ok_or_else(|| Error::internal("traced column out of range"))?;
                [
                    Value::varchar(alias.clone()),
                    Value::varchar(definition.logical_type.to_string()),
                    Value::varchar(if table.not_null_columns.contains(&column) {
                        "NO"
                    } else {
                        "YES"
                    }),
                    Value::null(LogicalType::VARCHAR),
                    definition.default_value.as_ref().map_or_else(
                        || Value::null(LogicalType::VARCHAR),
                        |default| Value::varchar(default.to_string()),
                    ),
                    Value::null(LogicalType::VARCHAR),
                ]
            }
            None => [
                Value::varchar(alias.clone()),
                Value::varchar(ty.to_string()),
                Value::varchar("YES"),
                Value::null(LogicalType::VARCHAR),
                Value::null(LogicalType::VARCHAR),
                Value::null(LogicalType::VARCHAR),
            ],
        };
        rows.push(row);
    }
    for batch in rows.chunks(STANDARD_VECTOR_SIZE) {
        let mut chunk = DataChunk::with_types(&types, batch.len());
        for (row_index, row) in batch.iter().enumerate() {
            for (column, value) in row.iter().enumerate() {
                chunk.column_mut(column).set_value(row_index, value)?;
            }
        }
        chunk.set_len(batch.len());
        chunks.push(chunk);
    }
    let table_index = scope.binder.generate_table_index();
    scope.binder.bind_context.add_generic_binding(
        table_index,
        "__show_select",
        names.clone(),
        types.clone(),
    )?;
    let plan = LogicalOperator::new(LogicalKind::ChunkGet {
        table_index,
        chunk_types: types.clone(),
        collection: chunks.into(),
    });
    Ok(BoundStatement::new(plan, names, types))
}

/// SQL text as a literal (DuckDB's `SQLString`).
fn sql_string(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// DuckDB's `PragmaShowTables(database, schema)`.
fn pragma_show_tables(database: &str, schema: &str) -> String {
    let mut conditions = Vec::new();
    if !database.is_empty() {
        conditions.push(format!("lower(database_name) = lower({})", sql_string(database)));
    }
    if !schema.is_empty() {
        conditions.push(format!("lower(schema_name) = lower({})", sql_string(schema)));
    }
    if conditions.is_empty() {
        conditions.push("in_search_path(database_name, schema_name)".to_owned());
    }
    let where_clause = format!("WHERE {}", conditions.join(" AND "));
    format!(
        "\n\twith \"tables\" as\n\t(\n\t\tSELECT table_name as \"name\"\n\t\tFROM duckdb_tables\n\t\t{where_clause}\n\t), \
         \"views\" as\n\t(\n\t\tSELECT view_name as \"name\"\n\t\tFROM duckdb_views\n\t\t{where_clause}\n\t), db_objects \
         as\n\t(\n\t\tSELECT \"name\" FROM \"tables\"\n\t\tUNION ALL\n\t\tSELECT \"name\" FROM \"views\"\n\t)\n\tSELECT \
         \"name\"\n\tFROM db_objects\n\tORDER BY \"name\";"
    )
}

/// `SHOW TABLES`, `SHOW <table>` and friends (DuckDB's `Binder::BindShowTable`).
fn bind_show_table(scope: &mut BindScope<'_>, show: &ShowRef) -> Result<BoundStatement> {
    let lname = show.table_name.to_lowercase();
    let sql = match lname.as_str() {
        "\"databases\"" => "SELECT database_name FROM duckdb_databases() WHERE NOT internal ORDER BY database_name;".to_owned(),
        "\"schemas\"" => "SELECT  schema.database_name,  schema.schema_name,  ((select current_schema() = \
                          schema.schema_name)   and (select current_database() = schema.database_name)) \"current\" \
                          FROM duckdb_schemas() schema JOIN duckdb_databases dbs USING (database_oid) WHERE \
                          dbs.internal = false ORDER BY all;"
            .to_owned(),
        "\"tables\"" => pragma_show_tables("", ""),
        _ if show.show_type == ShowType::ShowFrom => {
            let (mut catalog, mut schema) = (show.catalog_name.clone(), show.schema_name.clone());
            if catalog.is_empty() && !schema.is_empty() && scope.context.catalog(&schema).is_some() {
                catalog = std::mem::take(&mut schema);
            }
            if !schema.is_empty() {
                if catalog.is_empty() {
                    catalog = scope.context.default_catalog().to_owned();
                }
                let exists = scope
                    .context
                    .catalog(&catalog)
                    .and_then(|entry| entry.schema(&scope.context.transaction, &schema))
                    .is_some();
                if !exists {
                    return Err(Error::catalog(format!(
                        "SHOW TABLES FROM: No catalog + schema named \"{catalog}.{schema}\" found."
                    )));
                }
            }
            pragma_show_tables(&catalog, &schema)
        }
        "\"variables\"" => "SELECT * FROM duckdb_variables() ORDER BY name".to_owned(),
        "__show_tables_expanded" => {
            return Err(Error::not_implemented("SHOW ALL TABLES"));
        }
        _ => format!("SELECT * FROM pragma_show({});", sql_string(&show.table_name)),
    };
    let mut statements = rustdb_parser::parse(&sql)?;
    let Some(statement) = statements.pop() else {
        return Err(Error::internal("SHOW query parsed to nothing"));
    };
    let SqlStatementKind::Select(select) = statement.kind else {
        return Err(Error::internal("SHOW query is not a SELECT"));
    };
    let mut subquery = SubqueryRef {
        subquery: Box::new(select),
        column_name_alias: Vec::new(),
    };
    bind_subquery_ref(scope, &mut subquery, "")
}

/// DuckDB's `Binder::Bind(ShowRef &)`.
pub fn bind_show(scope: &mut BindScope<'_>, show: &mut ShowRef) -> Result<BoundStatement> {
    if show.show_type == ShowType::Summary {
        return Err(Error::not_implemented("SUMMARIZE"));
    }
    match &mut show.query {
        Some(query) => bind_show_query(scope, query),
        None => bind_show_table(scope, show),
    }
}
