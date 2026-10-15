//! Catalog table functions (DuckDB's `function/table/system`): `duckdb_databases()`,
//! `duckdb_schemas()`, `duckdb_tables()`, `duckdb_views()`, `duckdb_indexes()`,
//! `duckdb_sequences()` and `duckdb_columns()`, plus `current_database()` and
//! `current_schema()`. Their rows are collected from the binder's client context when the
//! call is bound.

#![allow(
    clippy::unnecessary_wraps,
    reason = "the row builders share one fallible signature"
)]

use std::any::Any;
use std::sync::Arc;

use rustdb_ast::{
    ColumnDefinition, ColumnList, CompressionType, CreateInfo, CreateInfoKind, CreateTableInfo,
    CreateViewInfo, LogicalDependencyList, OnCreateConflict, TableColumnType,
};
use rustdb_catalog::{Catalog, CatalogEntry, EntryKind, SchemaEntry, TableEntry};
use rustdb_common::{CaseInsensitiveMap, Error, OrderedMap, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::{
    FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet, Stability, TableFunction,
    TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
};
use rustdb_planner::ClientContext;
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value, Vector};

/// The output columns of a catalog function: names and types.
type Columns = &'static [(&'static str, fn() -> LogicalType)];

/// The rows of a call, computed at bind time.
#[derive(Debug)]
struct Rows {
    types: Vec<LogicalType>,
    rows: Vec<Vec<Value>>,
}

impl FunctionData for Rows {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The binder's client context of a table function call.
fn client_context<'a>(input: &TableFunctionBindInput<'a>) -> Result<&'a ClientContext> {
    input
        .binder
        .and_then(|binder| binder.downcast_ref::<ClientContext>())
        .ok_or_else(|| Error::internal("catalog function bound without a client context"))
}

/// A table function without arguments whose `columns` are filled by `rows`.
fn rows_function(
    name: &'static str,
    columns: Columns,
    rows: fn(&ClientContext) -> Result<Vec<Vec<Value>>>,
) -> TableFunctionSet {
    let mut set = TableFunctionSet::new(name);
    set.add(
        TableFunction::new(name, Vec::new())
            .with_bind(move |input| {
                let context = client_context(input)?;
                let types: Vec<LogicalType> = columns.iter().map(|(_, ty)| ty()).collect();
                let rows = rows(context)?;
                Ok(TableFunctionBindOutput {
                    return_types: types.clone(),
                    names: columns.iter().map(|(name, _)| (*name).to_owned()).collect(),
                    bind_data: Some(Arc::new(Rows { types, rows })),
                })
            })
            .with_scan(|bind_data, _, state, chunk| {
                let data = bind_data
                    .and_then(|data| data.as_any().downcast_ref::<Rows>())
                    .ok_or_else(|| Error::internal("catalog function without rows"))?;
                let start = usize::try_from(state.position)
                    .unwrap_or(usize::MAX)
                    .min(data.rows.len());
                let end = (start + STANDARD_VECTOR_SIZE).min(data.rows.len());
                let mut columns: Vec<Vector> = data
                    .types
                    .iter()
                    .map(|ty| Vector::flat(ty.clone(), (end - start).max(1)))
                    .collect();
                for (row_index, row) in data.rows[start..end].iter().enumerate() {
                    for (column, value) in columns.iter_mut().zip(row) {
                        column.set_value(row_index, value)?;
                    }
                }
                *chunk = DataChunk::new(columns, end - start);
                state.position = end as u64;
                Ok(end < data.rows.len())
            }),
    );
    set
}

fn varchar() -> LogicalType {
    LogicalType::VARCHAR
}
fn bigint() -> LogicalType {
    LogicalType::BIGINT
}
fn integer() -> LogicalType {
    LogicalType::INTEGER
}
fn boolean() -> LogicalType {
    LogicalType::BOOLEAN
}
fn string_map() -> LogicalType {
    LogicalType::map(LogicalType::VARCHAR, LogicalType::VARCHAR)
}

fn text(value: impl Into<String>) -> Value {
    Value::varchar(value.into())
}

fn null_text() -> Value {
    Value::null(LogicalType::VARCHAR)
}

fn oid(value: u64) -> Value {
    Value::bigint(i64::try_from(value).unwrap_or(i64::MAX))
}

fn count(value: usize) -> Value {
    Value::bigint(i64::try_from(value).unwrap_or(i64::MAX))
}

fn empty_map() -> Value {
    Value::map(LogicalType::VARCHAR, LogicalType::VARCHAR, Vec::new())
}

/// The visible catalogs, sorted by name, without hidden ones.
fn visible_catalogs(context: &ClientContext) -> Vec<&Arc<Catalog>> {
    let mut catalogs: Vec<&Arc<Catalog>> = context
        .catalogs
        .iter()
        .filter(|catalog| !catalog.attach_info().is_some_and(|info| info.hidden))
        .collect();
    catalogs.sort_by_key(|catalog| catalog.name().to_lowercase());
    catalogs
}

/// Every `(catalog, schema entry, schema)` visible to the transaction.
fn schemas(context: &ClientContext) -> Vec<(&Arc<Catalog>, Arc<CatalogEntry>)> {
    let mut result = Vec::new();
    for catalog in visible_catalogs(context) {
        for schema in catalog.schemas().scan(&context.transaction) {
            result.push((catalog, schema));
        }
    }
    result
}

fn schema_contents(schema: &CatalogEntry) -> Option<&SchemaEntry> {
    match &schema.kind {
        EntryKind::Schema(contents) => Some(contents),
        _ => None,
    }
}

const DATABASE_COLUMNS: Columns = &[
    ("database_name", varchar),
    ("database_oid", bigint),
    ("path", varchar),
    ("comment", varchar),
    ("tags", string_map),
    ("internal", boolean),
    ("type", varchar),
    ("readonly", boolean),
    ("encrypted", boolean),
    ("cipher", varchar),
    ("options", string_map),
];

/// `duckdb_databases()`.
fn database_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let mut rows: Vec<(String, Vec<Value>)> = Vec::new();
    for catalog in visible_catalogs(context) {
        let info = catalog.attach_info();
        let internal = info.is_none();
        let path = info
            .filter(|info| !info.path.is_empty())
            .map_or_else(null_text, |info| text(info.path.clone()));
        rows.push((
            catalog.name().to_lowercase(),
            vec![
                text(catalog.name()),
                oid(catalog.oid()),
                path,
                null_text(),
                empty_map(),
                Value::boolean(internal),
                text("duckdb"),
                Value::boolean(info.is_some_and(|info| info.read_only)),
                Value::boolean(false),
                null_text(),
                empty_map(),
            ],
        ));
    }
    // The temporary database exists in every DuckDB instance.
    rows.push((
        "temp".to_owned(),
        vec![
            text("temp"),
            oid(TEMP_OID),
            null_text(),
            null_text(),
            empty_map(),
            Value::boolean(true),
            text("duckdb"),
            Value::boolean(false),
            Value::boolean(false),
            null_text(),
            empty_map(),
        ],
    ));
    // `DatabaseManager::GetDatabases`: the attached databases, then `system` and `temp`.
    let rank = |name: &str| match name {
        "system" => 1,
        "temp" => 2,
        _ => 0,
    };
    rows.sort_by(|a, b| (rank(&a.0), &a.0).cmp(&(rank(&b.0), &b.0)));
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}

/// The object id shown for the temporary database.
const TEMP_OID: u64 = 1;

const SCHEMA_COLUMNS: Columns = &[
    ("oid", bigint),
    ("database_name", varchar),
    ("database_oid", bigint),
    ("schema_name", varchar),
    ("comment", varchar),
    ("tags", string_map),
    ("internal", boolean),
    ("sql", varchar),
];

/// `duckdb_schemas()`.
fn schema_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let mut rows: Vec<(String, String, Vec<Value>)> = Vec::new();
    let mut push = |catalog: &str, catalog_oid: u64, schema: &str, schema_oid: u64, internal: bool| {
        rows.push((
            catalog.to_lowercase(),
            schema.to_lowercase(),
            vec![
                oid(schema_oid),
                text(catalog),
                oid(catalog_oid),
                text(schema),
                null_text(),
                empty_map(),
                Value::boolean(internal),
                null_text(),
            ],
        ));
    };
    for (catalog, schema) in schemas(context) {
        push(
            catalog.name(),
            catalog.oid(),
            schema.name(),
            schema.oid,
            schema.internal,
        );
    }
    push("temp", TEMP_OID, "main", TEMP_OID + 1, true);
    rows.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    Ok(rows.into_iter().map(|(_, _, row)| row).collect())
}

/// The `CreateInfo` of a table entry (`TableCatalogEntry::GetInfo`).
fn table_create_info(catalog: &str, schema: &str, entry: &CatalogEntry, table: &TableEntry) -> CreateInfo {
    let columns = table
        .columns
        .iter()
        .map(|column| {
            let (expression, category) = match (&column.generated, &column.default_value) {
                (Some(generated), _) => (Some(Box::new(generated.clone())), TableColumnType::Generated),
                (None, default) => (default.clone().map(Box::new), TableColumnType::Standard),
            };
            ColumnDefinition {
                name: column.name.clone(),
                logical_type: column.logical_type.clone(),
                expression,
                category,
                compression_type: CompressionType::CompressionAuto,
                comment: Value::default(),
                tags: OrderedMap::default(),
            }
        })
        .collect();
    base_info(
        catalog,
        schema,
        entry.temporary,
        CreateInfoKind::Table(Box::new(CreateTableInfo {
            table: entry.name().to_owned(),
            columns: ColumnList { columns },
            constraints: table.constraints.clone(),
            query: None,
            partition_keys: Vec::new(),
            sort_keys: Vec::new(),
            options: CaseInsensitiveMap::default(),
        })),
    )
}

fn base_info(catalog: &str, schema: &str, temporary: bool, kind: CreateInfoKind) -> CreateInfo {
    CreateInfo {
        catalog: catalog.to_owned(),
        schema: schema.to_owned(),
        temporary,
        internal: false,
        on_conflict: OnCreateConflict::ErrorOnConflict,
        sql: String::new(),
        comment: Value::default(),
        tags: CaseInsensitiveMap::default(),
        dependencies: LogicalDependencyList::default(),
        kind,
    }
}

const TABLE_COLUMNS: Columns = &[
    ("database_name", varchar),
    ("database_oid", bigint),
    ("schema_name", varchar),
    ("schema_oid", bigint),
    ("table_name", varchar),
    ("table_oid", bigint),
    ("comment", varchar),
    ("tags", string_map),
    ("internal", boolean),
    ("temporary", boolean),
    ("has_primary_key", boolean),
    ("estimated_size", bigint),
    ("column_count", bigint),
    ("index_count", bigint),
    ("check_constraint_count", bigint),
    ("sql", varchar),
];

/// `duckdb_tables()`.
fn table_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let mut rows = Vec::new();
    for (catalog, schema) in schemas(context) {
        let Some(contents) = schema_contents(&schema) else {
            continue;
        };
        for entry in contents.tables.scan(&context.transaction) {
            let EntryKind::Table(table) = &entry.kind else {
                continue;
            };
            let has_primary_key = table
                .constraints
                .iter()
                .any(|constraint| matches!(constraint, rustdb_ast::Constraint::Unique(unique) if unique.is_primary_key));
            let (estimated_size, index_count) = table.storage.as_ref().map_or((0, 0), |storage| {
                (storage.total_rows(), storage.info().indexes().len())
            });
            let checks = table
                .constraints
                .iter()
                .filter(|constraint| matches!(constraint, rustdb_ast::Constraint::Check(_)))
                .count();
            let info = table_create_info("", schema.name(), &entry, table);
            rows.push(vec![
                text(catalog.name()),
                oid(catalog.oid()),
                text(schema.name()),
                oid(schema.oid),
                text(entry.name()),
                oid(entry.oid),
                null_text(),
                empty_map(),
                Value::boolean(entry.internal),
                Value::boolean(entry.temporary),
                Value::boolean(has_primary_key),
                Value::bigint(i64::try_from(estimated_size).unwrap_or(i64::MAX)),
                count(table.columns.len()),
                count(index_count),
                count(checks),
                info.to_sql().map_or_else(null_text, text),
            ]);
        }
    }
    Ok(rows)
}

const VIEW_COLUMNS: Columns = &[
    ("database_name", varchar),
    ("database_oid", bigint),
    ("schema_name", varchar),
    ("schema_oid", bigint),
    ("view_name", varchar),
    ("view_oid", bigint),
    ("comment", varchar),
    ("tags", string_map),
    ("internal", boolean),
    ("temporary", boolean),
    ("column_count", bigint),
    ("sql", varchar),
    ("is_bound", boolean),
];

/// `duckdb_views()`.
fn view_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let mut rows = Vec::new();
    for (catalog, schema) in schemas(context) {
        let Some(contents) = schema_contents(&schema) else {
            continue;
        };
        for entry in contents.tables.scan(&context.transaction) {
            let EntryKind::View(view) = &entry.kind else {
                continue;
            };
            let info = base_info(
                "",
                schema.name(),
                entry.temporary,
                CreateInfoKind::View(Box::new(CreateViewInfo {
                    view_name: entry.name().to_owned(),
                    aliases: view.aliases.clone(),
                    types: view.types.clone(),
                    query: Box::new(view.query.clone()),
                    names: view.names.clone(),
                    column_comments: OrderedMap::default(),
                })),
            );
            rows.push(vec![
                text(catalog.name()),
                oid(catalog.oid()),
                text(schema.name()),
                oid(schema.oid),
                text(entry.name()),
                oid(entry.oid),
                null_text(),
                empty_map(),
                Value::boolean(entry.internal),
                Value::boolean(entry.temporary),
                count(view.types.len()),
                info.to_sql().map_or_else(null_text, text),
                Value::boolean(true),
            ]);
        }
    }
    Ok(rows)
}

const INDEX_COLUMNS: Columns = &[
    ("database_name", varchar),
    ("database_oid", bigint),
    ("schema_name", varchar),
    ("schema_oid", bigint),
    ("index_name", varchar),
    ("index_oid", bigint),
    ("table_name", varchar),
    ("table_oid", bigint),
    ("comment", varchar),
    ("tags", string_map),
    ("is_unique", boolean),
    ("is_primary", boolean),
    ("expressions", varchar),
    ("sql", varchar),
];

/// `duckdb_indexes()`.
fn index_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let mut rows = Vec::new();
    for (catalog, schema) in schemas(context) {
        let Some(contents) = schema_contents(&schema) else {
            continue;
        };
        for entry in contents.indexes.scan(&context.transaction) {
            let EntryKind::Index(index) = &entry.kind else {
                continue;
            };
            let table_oid = contents
                .tables
                .get(&context.transaction, &index.info.table)
                .map_or_else(|| Value::null(LogicalType::BIGINT), |table| oid(table.oid));
            let constraint = index.info.constraint_type;
            let expressions = rustdb_ast::index_expressions_to_sql(&index.info);
            let info = base_info(
                "",
                &index.schema,
                entry.temporary,
                CreateInfoKind::Index(Box::new(index.info.clone())),
            );
            rows.push(vec![
                text(catalog.name()),
                oid(catalog.oid()),
                text(schema.name()),
                oid(schema.oid),
                text(entry.name()),
                oid(entry.oid),
                text(index.info.table.clone()),
                table_oid,
                null_text(),
                empty_map(),
                Value::boolean(matches!(
                    constraint,
                    rustdb_ast::IndexConstraintType::Unique | rustdb_ast::IndexConstraintType::Primary
                )),
                Value::boolean(constraint == rustdb_ast::IndexConstraintType::Primary),
                text(format!("[{}]", expressions.join(", "))),
                info.to_sql().map_or_else(null_text, text),
            ]);
        }
    }
    Ok(rows)
}

const SEQUENCE_COLUMNS: Columns = &[
    ("database_name", varchar),
    ("database_oid", bigint),
    ("schema_name", varchar),
    ("schema_oid", bigint),
    ("sequence_name", varchar),
    ("sequence_oid", bigint),
    ("comment", varchar),
    ("tags", string_map),
    ("temporary", boolean),
    ("start_value", bigint),
    ("min_value", bigint),
    ("max_value", bigint),
    ("increment_by", bigint),
    ("cycle", boolean),
    ("last_value", bigint),
    ("sql", varchar),
];

/// `duckdb_sequences()`.
fn sequence_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let mut rows = Vec::new();
    for (catalog, schema) in schemas(context) {
        let Some(contents) = schema_contents(&schema) else {
            continue;
        };
        for entry in contents.sequences.scan(&context.transaction) {
            let EntryKind::Sequence(sequence) = &entry.kind else {
                continue;
            };
            let data = sequence.data();
            let sql = rustdb_ast::sequence_to_sql(&rustdb_ast::CreateSequenceInfo {
                name: entry.name().to_owned(),
                usage_count: data.usage_count,
                increment: data.increment,
                min_value: data.min_value,
                max_value: data.max_value,
                start_value: data.counter,
                cycle: data.cycle,
            });
            rows.push(vec![
                text(catalog.name()),
                oid(catalog.oid()),
                text(schema.name()),
                oid(schema.oid),
                text(entry.name()),
                oid(entry.oid),
                null_text(),
                empty_map(),
                Value::boolean(entry.temporary),
                Value::bigint(data.start_value),
                Value::bigint(data.min_value),
                Value::bigint(data.max_value),
                Value::bigint(data.increment),
                Value::boolean(data.cycle),
                if data.usage_count == 0 {
                    Value::null(LogicalType::BIGINT)
                } else {
                    Value::bigint(data.last_value)
                },
                text(sql),
            ]);
        }
    }
    Ok(rows)
}

const COLUMN_COLUMNS: Columns = &[
    ("database_name", varchar),
    ("database_oid", bigint),
    ("schema_name", varchar),
    ("schema_oid", bigint),
    ("table_name", varchar),
    ("table_oid", bigint),
    ("column_name", varchar),
    ("column_index", integer),
    ("comment", varchar),
    ("internal", boolean),
    ("column_default", varchar),
    ("is_nullable", boolean),
    ("data_type", varchar),
    ("data_type_id", bigint),
    ("character_maximum_length", integer),
    ("numeric_precision", integer),
    ("numeric_precision_radix", integer),
    ("numeric_scale", integer),
];

/// `numeric_precision`, `numeric_precision_radix` and `numeric_scale` of a type.
fn numeric_properties(ty: &LogicalType) -> [Value; 3] {
    let binary = |bits: i32| [Value::integer(bits), Value::integer(2), Value::integer(0)];
    match ty.id() {
        LogicalTypeId::Decimal => {
            let (width, scale) = ty.decimal_properties().unwrap_or((18, 3));
            [
                Value::integer(i32::from(width)),
                Value::integer(10),
                Value::integer(i32::from(scale)),
            ]
        }
        LogicalTypeId::HugeInt => binary(128),
        LogicalTypeId::BigInt => binary(64),
        LogicalTypeId::Integer => binary(32),
        LogicalTypeId::SmallInt => binary(16),
        LogicalTypeId::TinyInt => binary(8),
        LogicalTypeId::Float => binary(24),
        LogicalTypeId::Double => binary(53),
        _ => [
            Value::null(LogicalType::INTEGER),
            Value::null(LogicalType::INTEGER),
            Value::null(LogicalType::INTEGER),
        ],
    }
}

/// One `duckdb_columns()` row.
#[allow(clippy::too_many_arguments, reason = "one argument per output group")]
fn column_row(
    catalog: &Catalog,
    schema: &CatalogEntry,
    entry: &CatalogEntry,
    index: usize,
    name: &str,
    ty: &LogicalType,
    default: Option<String>,
    nullable: bool,
) -> Vec<Value> {
    let [precision, radix, scale] = numeric_properties(ty);
    vec![
        text(catalog.name()),
        oid(catalog.oid()),
        text(schema.name()),
        oid(schema.oid),
        text(entry.name()),
        oid(entry.oid),
        text(name),
        Value::integer(i32::try_from(index + 1).unwrap_or(i32::MAX)),
        null_text(),
        Value::boolean(entry.internal),
        default.map_or_else(null_text, text),
        Value::boolean(nullable),
        text(ty.to_string()),
        Value::bigint(i64::from(ty.id() as u8)),
        Value::null(LogicalType::INTEGER),
        precision,
        radix,
        scale,
    ]
}

/// `duckdb_columns()`: the columns of every table and view.
fn column_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let mut rows = Vec::new();
    for (catalog, schema) in schemas(context) {
        let Some(contents) = schema_contents(&schema) else {
            continue;
        };
        for entry in contents.tables.scan(&context.transaction) {
            match &entry.kind {
                EntryKind::Table(table) => {
                    for (index, column) in table.columns.iter().enumerate() {
                        let nullable = !table.not_null_columns.contains(&index);
                        let default = column
                            .generated
                            .as_ref()
                            .or(column.default_value.as_ref())
                            .map(ToString::to_string);
                        rows.push(column_row(
                            catalog,
                            &schema,
                            &entry,
                            index,
                            &column.name,
                            &column.logical_type,
                            default,
                            nullable,
                        ));
                    }
                }
                EntryKind::View(view) => {
                    for (index, ty) in view.types.iter().enumerate() {
                        let name = view
                            .aliases
                            .get(index)
                            .or_else(|| view.names.get(index))
                            .cloned()
                            .unwrap_or_default();
                        rows.push(column_row(catalog, &schema, &entry, index, &name, ty, None, true));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(rows)
}

/// Registers the default views in the system catalog.
pub(crate) fn register_default_views(system: &Catalog) -> Result<()> {
    for (schema, name, sql) in crate::default_views::DEFAULT_VIEWS {
        let mut statements = rustdb_parser::parse(sql)?;
        let Some(statement) = statements.pop() else {
            continue;
        };
        let rustdb_ast::SqlStatementKind::Select(select) = statement.kind else {
            return Err(Error::internal("default view is not a SELECT"));
        };
        system.register_view_in(
            schema,
            name,
            rustdb_catalog::ViewEntry {
                query: select,
                aliases: Vec::new(),
                types: Vec::new(),
                names: Vec::new(),
            },
        )?;
    }
    Ok(())
}

/// The catalog table functions.
pub(crate) fn catalog_table_functions() -> Vec<TableFunctionSet> {
    vec![
        rows_function("duckdb_databases", DATABASE_COLUMNS, database_rows),
        rows_function("duckdb_schemas", SCHEMA_COLUMNS, schema_rows),
        rows_function("duckdb_tables", TABLE_COLUMNS, table_rows),
        rows_function("duckdb_views", VIEW_COLUMNS, view_rows),
        rows_function("duckdb_indexes", INDEX_COLUMNS, index_rows),
        rows_function("duckdb_sequences", SEQUENCE_COLUMNS, sequence_rows),
        rows_function("duckdb_columns", COLUMN_COLUMNS, column_rows),
    ]
}

/// A constant VARCHAR computed from the client context when bound.
#[derive(Debug)]
struct ConstantText(String);

impl FunctionData for ConstantText {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A function without arguments returning `value(context)`.
fn context_function(name: &'static str, value: fn(&ClientContext) -> String) -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        name,
        Vec::new(),
        LogicalType::VARCHAR,
        |_: &DataChunk, state: &mut FunctionState, result: &mut Vector| {
            let text = state
                .bind_data
                .as_ref()
                .and_then(|data| data.as_any().downcast_ref::<ConstantText>())
                .map(|text| text.0.clone())
                .ok_or_else(|| Error::internal("context function without bind data"))?;
            *result = Vector::constant(&Value::varchar(text))?;
            Ok(())
        },
    )
    .with_bind(move |_, input| {
        let context = input
            .binder
            .and_then(|binder| binder.downcast_ref::<ClientContext>())
            .ok_or_else(|| Error::internal("context function bound without a client context"))?;
        Ok(Some(
            Arc::new(ConstantText(value(context))) as Arc<dyn FunctionData>
        ))
    });
    function.stability = Stability::ConsistentWithinQuery;
    let mut set = ScalarFunctionSet::new(name);
    set.add(function);
    set
}

/// `current_database()`, `current_schema()` and `in_search_path()`.
pub(crate) fn context_scalar_functions() -> Vec<ScalarFunctionSet> {
    vec![
        context_function("current_database", |context| context.default_catalog().to_owned()),
        context_function("current_schema", |context| context.default_schema().to_owned()),
        in_search_path_function(),
    ]
}

/// Emits pre-computed rows (the scan of every catalog function).
fn scan_rows(
    bind_data: Option<&dyn FunctionData>,
    state: &mut rustdb_function::TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<Rows>())
        .ok_or_else(|| Error::internal("catalog function without rows"))?;
    let start = usize::try_from(state.position)
        .unwrap_or(usize::MAX)
        .min(data.rows.len());
    let end = (start + STANDARD_VECTOR_SIZE).min(data.rows.len());
    let mut columns: Vec<Vector> = data
        .types
        .iter()
        .map(|ty| Vector::flat(ty.clone(), (end - start).max(1)))
        .collect();
    for (row_index, row) in data.rows[start..end].iter().enumerate() {
        for (column, value) in columns.iter_mut().zip(row) {
            column.set_value(row_index, value)?;
        }
    }
    *chunk = DataChunk::new(columns, end - start);
    state.position = end as u64;
    Ok(end < data.rows.len())
}

/// A table function of one VARCHAR argument whose `columns` are filled by `rows`.
fn rows_function_with_name(
    name: &'static str,
    columns: Columns,
    rows: fn(&ClientContext, &str) -> Result<Vec<Vec<Value>>>,
) -> TableFunctionSet {
    let mut set = TableFunctionSet::new(name);
    set.add(
        TableFunction::new(name, vec![LogicalType::VARCHAR])
            .with_bind(move |input| {
                let context = client_context(input)?;
                let argument = input
                    .inputs
                    .first()
                    .filter(|value| !value.is_null())
                    .map(Value::to_varchar)
                    .transpose()?
                    .unwrap_or_default();
                let types: Vec<LogicalType> = columns.iter().map(|(_, ty)| ty()).collect();
                let rows = rows(context, &argument)?;
                Ok(TableFunctionBindOutput {
                    return_types: types.clone(),
                    names: columns.iter().map(|(name, _)| (*name).to_owned()).collect(),
                    bind_data: Some(Arc::new(Rows { types, rows })),
                })
            })
            .with_scan(|bind_data, _, state, chunk| scan_rows(bind_data, state, chunk)),
    );
    set
}

const TABLE_INFO_COLUMNS: Columns = &[
    ("cid", integer),
    ("name", varchar),
    ("type", varchar),
    ("notnull", boolean),
    ("dflt_value", varchar),
    ("pk", boolean),
];

/// `pragma_table_info(name)` (`PragmaTableInfoFunction`): the columns of a table or view.
fn table_info_rows(context: &ClientContext, name: &str) -> Result<Vec<Vec<Value>>> {
    let (catalog, schema, table) = crate::sequence::parse_qualified_name(name)?;
    let found = context
        .get_entry(
            &catalog,
            &schema,
            rustdb_catalog::CatalogType::Table,
            &table,
            false,
        )?
        .ok_or_else(|| Error::catalog(format!("Table with name {table} does not exist!")))?;
    let mut rows = Vec::new();
    match &found.entry.kind {
        EntryKind::Table(entry) => {
            for (index, column) in entry.columns.iter().enumerate() {
                let pk = entry.constraints.iter().any(|constraint| match constraint {
                    rustdb_ast::Constraint::Unique(unique) if unique.is_primary_key => {
                        if unique.index.0 == u64::MAX {
                            unique
                                .columns
                                .iter()
                                .any(|c| c.eq_ignore_ascii_case(&column.name))
                        } else {
                            unique.index.0 == index as u64
                        }
                    }
                    _ => false,
                });
                let default = column
                    .generated
                    .as_ref()
                    .or(column.default_value.as_ref())
                    .map_or_else(null_text, |expression| text(expression.to_string()));
                rows.push(vec![
                    Value::integer(i32::try_from(index).unwrap_or(i32::MAX)),
                    text(column.name.clone()),
                    text(column.logical_type.to_string()),
                    Value::boolean(entry.not_null_columns.contains(&index)),
                    default,
                    Value::boolean(pk),
                ]);
            }
        }
        EntryKind::View(view) => {
            for (index, ty) in view.types.iter().enumerate() {
                let name = view
                    .aliases
                    .get(index)
                    .or_else(|| view.names.get(index))
                    .cloned()
                    .unwrap_or_default();
                rows.push(vec![
                    Value::integer(i32::try_from(index).unwrap_or(i32::MAX)),
                    text(name),
                    text(ty.to_string()),
                    Value::boolean(false),
                    null_text(),
                    Value::boolean(false),
                ]);
            }
        }
        _ => {}
    }
    Ok(rows)
}

const DATABASE_SIZE_COLUMNS: Columns = &[
    ("database_name", varchar),
    ("database_size", varchar),
    ("block_size", bigint),
    ("total_blocks", bigint),
    ("used_blocks", bigint),
    ("free_blocks", bigint),
    ("wal_size", varchar),
    ("memory_usage", varchar),
    ("memory_limit", varchar),
];

/// `pragma_database_size()` (`PragmaDatabaseSizeFunction`).
fn database_size_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let mut rows = Vec::new();
    for catalog in &context.catalogs {
        if catalog.attach_info().is_none() {
            continue;
        }
        let Ok(io) = catalog.io() else {
            continue;
        };
        let block_manager = io.block_manager();
        let (block_size, total, free) = if block_manager.in_memory() {
            (0, 0, 0)
        } else {
            (
                block_manager.block_alloc_size() as u64,
                block_manager.total_blocks(),
                block_manager.free_blocks(),
            )
        };
        let wal_size = catalog.wal().map_or(0, |wal| wal.size());
        let buffer_manager = io.buffer_manager();
        let limit = buffer_manager.max_memory();
        rows.push(vec![
            text(catalog.name()),
            text(rustdb_buffer::format_bytes(total * block_size)),
            Value::bigint(i64::try_from(block_size).unwrap_or(i64::MAX)),
            Value::bigint(i64::try_from(total).unwrap_or(i64::MAX)),
            Value::bigint(i64::try_from(total - free.min(total)).unwrap_or(i64::MAX)),
            Value::bigint(i64::try_from(free).unwrap_or(i64::MAX)),
            text(rustdb_buffer::format_bytes(wal_size)),
            text(rustdb_buffer::format_bytes(buffer_manager.used_memory())),
            if limit == u64::MAX {
                text("Unlimited")
            } else {
                text(rustdb_buffer::format_bytes(limit))
            },
        ]);
    }
    Ok(rows)
}

fn bigint_list() -> LogicalType {
    LogicalType::list(LogicalType::BIGINT)
}

const STORAGE_INFO_COLUMNS: Columns = &[
    ("row_group_id", bigint),
    ("column_name", varchar),
    ("column_id", bigint),
    ("column_path", varchar),
    ("segment_id", bigint),
    ("segment_type", varchar),
    ("start", bigint),
    ("count", bigint),
    ("compression", varchar),
    ("stats", varchar),
    ("has_updates", boolean),
    ("persistent", boolean),
    ("block_id", bigint),
    ("block_offset", bigint),
    ("segment_info", varchar),
    ("additional_block_ids", bigint_list),
];

/// `pragma_storage_info(name)` (`PragmaStorageInfoFunction`): every segment of a table.
fn storage_info_rows(context: &ClientContext, name: &str) -> Result<Vec<Vec<Value>>> {
    let (catalog, schema, table) = crate::sequence::parse_qualified_name(name)?;
    let found = context
        .get_entry(
            &catalog,
            &schema,
            rustdb_catalog::CatalogType::Table,
            &table,
            false,
        )?
        .ok_or_else(|| Error::catalog(format!("Table with name {table} does not exist!")))?;
    let EntryKind::Table(entry) = &found.entry.kind else {
        return Err(Error::catalog(format!(
            "Existing object {} is of type View, trying to replace with type Table",
            found.entry.name()
        )));
    };
    let Some(storage) = &entry.storage else {
        return Ok(Vec::new());
    };
    let big = |value: u64| Value::bigint(i64::try_from(value).unwrap_or(i64::MAX));
    let mut rows = Vec::new();
    for info in storage.row_groups().column_segment_info()? {
        let column_name = entry
            .physical_to_logical(usize::try_from(info.column_id).unwrap_or(usize::MAX))
            .and_then(|logical| entry.columns.get(logical))
            .map(|column| column.name.clone())
            .unwrap_or_default();
        let (block_id, block_offset, additional) = if info.persistent {
            (
                Value::bigint(info.block_id),
                big(info.block_offset),
                Value::list(
                    LogicalType::BIGINT,
                    info.additional_blocks
                        .iter()
                        .map(|&block| Value::bigint(block))
                        .collect(),
                ),
            )
        } else {
            (
                Value::null(LogicalType::BIGINT),
                Value::null(LogicalType::BIGINT),
                Value::null(bigint_list()),
            )
        };
        rows.push(vec![
            big(info.row_group_index),
            text(column_name),
            big(info.column_id),
            text(info.column_path),
            big(info.segment_idx),
            text(info.segment_type),
            big(info.segment_start),
            big(info.segment_count),
            text(info.compression_type),
            text(info.segment_stats),
            Value::boolean(info.has_updates),
            Value::boolean(info.persistent),
            block_id,
            block_offset,
            text(info.segment_info),
            additional,
        ]);
    }
    Ok(rows)
}

/// The `pragma_*` table functions.
pub(crate) fn pragma_table_functions() -> Vec<TableFunctionSet> {
    vec![
        rows_function_with_name("pragma_table_info", TABLE_INFO_COLUMNS, table_info_rows),
        rows_function_with_name("pragma_storage_info", STORAGE_INFO_COLUMNS, storage_info_rows),
        rows_function("pragma_database_size", DATABASE_SIZE_COLUMNS, database_size_rows),
    ]
}

/// The search path entries of the calling connection.
#[derive(Debug)]
struct SearchPath(Vec<(String, String)>);

impl FunctionData for SearchPath {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `in_search_path(database, schema)`: whether the pair is on the connection's search path.
fn in_search_path_function() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "in_search_path",
        vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
        LogicalType::BOOLEAN,
        |args: &DataChunk, state: &mut FunctionState, result: &mut Vector| {
            let path = state
                .bind_data
                .as_ref()
                .and_then(|data| data.as_any().downcast_ref::<SearchPath>())
                .ok_or_else(|| Error::internal("in_search_path without bind data"))?;
            let count = args.len();
            let databases = args.column(0).flattened(count);
            let schemas = args.column(1).flattened(count);
            let mut output = Vector::flat(LogicalType::BOOLEAN, count.max(1));
            for row in 0..count {
                let database = databases.get_value(row);
                let schema = schemas.get_value(row);
                let value =
                    if database.is_null() || schema.is_null() {
                        Value::null(LogicalType::BOOLEAN)
                    } else {
                        let database = database.to_varchar()?;
                        let schema = schema.to_varchar()?;
                        Value::boolean(path.0.iter().any(|(c, s)| {
                            c.eq_ignore_ascii_case(&database) && s.eq_ignore_ascii_case(&schema)
                        }))
                    };
                output.set_value(row, &value)?;
            }
            *result = output;
            Ok(())
        },
    )
    .with_bind(|_, input| {
        let context = input
            .binder
            .and_then(|binder| binder.downcast_ref::<ClientContext>())
            .ok_or_else(|| Error::internal("in_search_path bound without a client context"))?;
        Ok(Some(
            Arc::new(SearchPath(context.full_search_path())) as Arc<dyn FunctionData>
        ))
    });
    function.stability = Stability::ConsistentWithinQuery;
    let mut set = ScalarFunctionSet::new("in_search_path");
    set.add(function);
    set
}

/// The SQL a `PRAGMA` stands for (DuckDB's `PragmaQueries`), if it is one.
pub(crate) fn pragma_query(name: &str, parameters: &[Value]) -> Result<Option<String>> {
    let first = || -> Result<String> {
        parameters
            .first()
            .map(Value::to_varchar)
            .transpose()?
            .ok_or_else(|| Error::invalid_input(format!("PRAGMA {name} requires an argument")))
    };
    let quoted = |text: &str| format!("'{}'", text.replace('\'', "''"));
    Ok(Some(match name.to_ascii_lowercase().as_str() {
        "table_info" => format!("SELECT * FROM pragma_table_info({});", quoted(&first()?)),
        "storage_info" => format!("SELECT * FROM pragma_storage_info({});", quoted(&first()?)),
        "show_tables" => "with \"tables\" as (SELECT table_name as \"name\" FROM duckdb_tables WHERE in_search_path(database_name, schema_name)), \"views\" as (SELECT view_name as \"name\" FROM duckdb_views WHERE in_search_path(database_name, schema_name)), db_objects as (SELECT \"name\" FROM \"tables\" UNION ALL SELECT \"name\" FROM \"views\") SELECT \"name\" FROM db_objects ORDER BY \"name\";".to_owned(),
        "show_databases" => "SELECT database_name FROM duckdb_databases() WHERE NOT internal ORDER BY database_name;".to_owned(),
        "database_list" => "SELECT * FROM pragma_database_list;".to_owned(),
        "database_size" => "SELECT * FROM pragma_database_size();".to_owned(),
        _ => return Ok(None),
    }))
}
