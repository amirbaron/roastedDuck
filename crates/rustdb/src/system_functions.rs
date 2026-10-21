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
    FunctionData, FunctionState, PragmaFunction, PragmaFunctionSet, ScalarFunction, ScalarFunctionSet,
    Stability, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
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

/// A `comment` column value: NULL without a comment.
fn comment_text(comment: &Value) -> Value {
    if comment.is_null() {
        return null_text();
    }
    comment.to_varchar().map_or_else(|_| null_text(), text)
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

/// A `MAP(VARCHAR, VARCHAR)` of `entries`.
fn text_map(entries: &[(String, String)]) -> Value {
    Value::map(
        LogicalType::VARCHAR,
        LogicalType::VARCHAR,
        entries
            .iter()
            .map(|(key, value)| (text(key.clone()), text(value.clone())))
            .collect(),
    )
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
                text(&*catalog.name()),
                oid(catalog.oid()),
                path,
                null_text(),
                info.map_or_else(empty_map, |info| text_map(&info.tags)),
                Value::boolean(internal),
                text("duckdb"),
                Value::boolean(info.is_some_and(|info| info.read_only)),
                Value::boolean(false),
                null_text(),
                info.map_or_else(empty_map, |info| text_map(&info.options)),
            ],
        ));
    }
    // `DatabaseManager::GetDatabases`: the attached databases, then `system` and `temp`.
    let rank = |name: &str| match name {
        "system" => 1,
        "temp" => 2,
        _ => 0,
    };
    rows.sort_by(|a, b| (rank(&a.0), &a.0).cmp(&(rank(&b.0), &b.0)));
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}

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
            &catalog.name(),
            catalog.oid(),
            schema.name(),
            schema.oid,
            schema.internal,
        );
    }
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
                comment: column.comment.clone(),
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
                text(&*catalog.name()),
                oid(catalog.oid()),
                text(schema.name()),
                oid(schema.oid),
                text(entry.name()),
                oid(entry.oid),
                comment_text(&entry.comment),
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
                    column_comments: view.column_comments.clone(),
                })),
            );
            rows.push(vec![
                text(&*catalog.name()),
                oid(catalog.oid()),
                text(schema.name()),
                oid(schema.oid),
                text(entry.name()),
                oid(entry.oid),
                comment_text(&entry.comment),
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
                text(&*catalog.name()),
                oid(catalog.oid()),
                text(schema.name()),
                oid(schema.oid),
                text(entry.name()),
                oid(entry.oid),
                text(index.info.table.clone()),
                table_oid,
                comment_text(&entry.comment),
                empty_map(),
                Value::boolean(matches!(
                    constraint,
                    rustdb_ast::IndexConstraintType::Unique | rustdb_ast::IndexConstraintType::Primary
                )),
                Value::boolean(constraint == rustdb_ast::IndexConstraintType::Primary),
                // `GetIndexExpressions(index).ToString()`: the VARCHAR list's text.
                text(
                    Value::list(
                        LogicalType::VARCHAR,
                        expressions.iter().map(Value::varchar).collect(),
                    )
                    .to_varchar()?,
                ),
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
                text(&*catalog.name()),
                oid(catalog.oid()),
                text(schema.name()),
                oid(schema.oid),
                text(entry.name()),
                oid(entry.oid),
                comment_text(&entry.comment),
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
    comment: &Value,
) -> Vec<Value> {
    let [precision, radix, scale] = numeric_properties(ty);
    vec![
        text(&*catalog.name()),
        oid(catalog.oid()),
        text(schema.name()),
        oid(schema.oid),
        text(entry.name()),
        oid(entry.oid),
        text(name),
        Value::integer(i32::try_from(index + 1).unwrap_or(i32::MAX)),
        comment_text(comment),
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
                            &column.comment,
                        ));
                    }
                }
                EntryKind::View(view) => {
                    // `ViewColumnHelper`: the view's column names, deduplicated.
                    let mut names: Vec<String> = (0..view.types.len())
                        .map(|index| {
                            view.aliases
                                .get(index)
                                .or_else(|| view.names.get(index))
                                .cloned()
                                .unwrap_or_default()
                        })
                        .collect();
                    rustdb_planner::deduplicate_columns(&mut names);
                    for (index, (ty, name)) in view.types.iter().zip(&names).enumerate() {
                        let comment = view.column_comments.get(name).cloned().unwrap_or_default();
                        rows.push(column_row(
                            catalog, &schema, &entry, index, name, ty, None, true, &comment,
                        ));
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
                column_comments: rustdb_common::OrderedMap::default(),
            },
        )?;
    }
    Ok(())
}

/// One overload of a default macro (`DefaultFunctionGenerator::CreateInternalMacroInfo`
/// and its table counterpart): the parameters as column references, the defaults and
/// the parsed body.
fn default_macro_function(
    default: &crate::default_functions::DefaultMacro,
    table: bool,
) -> Result<rustdb_ast::MacroFunction> {
    let kind = if table {
        let mut statements = rustdb_parser::parse(default.body)?;
        let Some(rustdb_ast::SqlStatementKind::Select(select)) = statements.pop().map(|s| s.kind) else {
            return Err(Error::internal("default table macro is not a SELECT"));
        };
        rustdb_ast::MacroFunctionKind::Table(rustdb_ast::TableMacroFunction {
            query_node: select.node,
        })
    } else {
        let mut expressions =
            rustdb_parser::parse_expression_list(default.body, &rustdb_parser::ParserOptions::default())?;
        let (Some(expression), true) = (expressions.pop(), expressions.is_empty()) else {
            return Err(Error::internal("Expected a single expression"));
        };
        rustdb_ast::MacroFunctionKind::Scalar(rustdb_ast::ScalarMacroFunction {
            expression: Box::new(expression),
        })
    };
    let mut parameters: Vec<rustdb_ast::ParsedExpression> = default
        .parameters
        .iter()
        .map(|name| rustdb_ast::ParsedExpression::column_ref([*name]))
        .collect();
    let mut default_parameters = CaseInsensitiveMap::default();
    for (name, value) in default.named_parameters {
        let mut expressions =
            rustdb_parser::parse_expression_list(value, &rustdb_parser::ParserOptions::default())?;
        let (Some(expression), true) = (expressions.pop(), expressions.is_empty()) else {
            return Err(Error::internal("Expected a single expression"));
        };
        parameters.push(rustdb_ast::ParsedExpression::column_ref([*name]));
        default_parameters.insert(*name, expression);
    }
    let types = vec![LogicalType::UNKNOWN; parameters.len()];
    Ok(rustdb_ast::MacroFunction {
        parameters,
        default_parameters,
        types,
        kind,
    })
}

/// Registers the default scalar and table macros in the system catalog (DuckDB's
/// `DefaultFunctionGenerator` and `DefaultTableFunctionGenerator`, created eagerly).
/// Consecutive entries with the same schema and name are the overloads of one macro.
pub(crate) fn register_default_macros(system: &Catalog) -> Result<()> {
    use crate::default_functions::{DEFAULT_MACROS, DEFAULT_TABLE_MACROS};
    for (defaults, table) in [(DEFAULT_MACROS, false), (DEFAULT_TABLE_MACROS, true)] {
        let mut index = 0;
        while index < defaults.len() {
            let first = &defaults[index];
            let mut macros = Vec::new();
            while index < defaults.len()
                && defaults[index].schema == first.schema
                && defaults[index].name == first.name
            {
                macros.push(default_macro_function(&defaults[index], table)?);
                index += 1;
            }
            let entry = rustdb_catalog::MacroEntry { macros };
            let kind = if table {
                EntryKind::TableMacro(entry)
            } else {
                EntryKind::Macro(entry)
            };
            let mut entry = CatalogEntry::new(first.name, kind);
            entry.internal = true;
            system.create_entry_committed(first.schema, entry)?;
        }
    }
    Ok(())
}

/// The `JSON` type and the macros of the json extension (`JSON_MACROS`), internal like
/// DuckDB's (`JsonExtension::Load`).
pub(crate) fn register_json_macros(system: &Catalog) -> Result<()> {
    let json_type = rustdb_catalog::TypeEntry {
        user_type: rustdb_json::json_type(),
    };
    let mut entry = CatalogEntry::new(rustdb_json::JSON_TYPE_NAME, EntryKind::Type(json_type));
    entry.internal = true;
    system.create_entry_committed("main", entry)?;
    for json_macro in rustdb_json::JSON_MACROS {
        let default = crate::default_functions::DefaultMacro {
            schema: "main",
            name: json_macro.name,
            parameters: json_macro.parameters,
            named_parameters: &[],
            body: json_macro.body,
        };
        let entry = rustdb_catalog::MacroEntry {
            macros: vec![default_macro_function(&default, false)?],
        };
        let mut entry = CatalogEntry::new(json_macro.name, EntryKind::Macro(entry));
        entry.internal = true;
        system.create_entry_committed("main", entry)?;
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
        rows_function("duckdb_memory", MEMORY_COLUMNS, memory_rows),
        rows_function("duckdb_constraints", CONSTRAINT_COLUMNS, constraint_rows),
        rows_function("duckdb_functions", FUNCTION_COLUMNS, function_rows),
        rows_function("duckdb_keywords", KEYWORD_COLUMNS, keyword_rows),
        rows_function("duckdb_settings", SETTING_COLUMNS, setting_rows),
        rows_function("duckdb_optimizers", OPTIMIZER_COLUMNS, optimizer_rows),
        rows_function(
            "duckdb_approx_database_count",
            DATABASE_COUNT_COLUMNS,
            database_count_rows,
        ),
        rows_function(
            "duckdb_connection_count",
            CONNECTION_COUNT_COLUMNS,
            connection_count_rows,
        ),
        rows_function("pragma_collations", COLLATION_COLUMNS, collation_rows),
        rows_function("duckdb_variables", VARIABLE_COLUMNS, variable_rows),
        rows_function(
            "duckdb_prepared_statements",
            PREPARED_STATEMENT_COLUMNS,
            prepared_statement_rows,
        ),
        rows_function(
            "duckdb_temporary_files",
            TEMPORARY_FILE_COLUMNS,
            temporary_file_rows,
        ),
        rows_function("duckdb_dependencies", DEPENDENCY_COLUMNS, dependency_rows),
        rows_function("duckdb_types", TYPE_COLUMNS, type_rows),
        secrets_function(),
        which_secret_function(),
        rows_function("duckdb_secret_types", SECRET_TYPE_COLUMNS, secret_type_rows),
        rows_function(
            "duckdb_coordinate_systems",
            COORDINATE_SYSTEM_COLUMNS,
            coordinate_system_rows,
        ),
        table_sample_function(),
        seq_scan_function(),
    ]
}

const SECRET_TYPE_COLUMNS: Columns = &[
    ("type", varchar),
    ("default_provider", varchar),
    ("extension", varchar),
];

/// `duckdb_secret_types()` (`DuckDBSecretTypesFunction`): the secret types of the secret
/// manager (`AllSecretTypes`).
fn secret_type_rows(_: &ClientContext) -> Result<Vec<Vec<Value>>> {
    Ok(crate::secrets::SECRET_TYPES
        .iter()
        .map(|secret_type| {
            vec![
                text(secret_type.name),
                text(secret_type.default_provider),
                text(secret_type.extension),
            ]
        })
        .collect())
}

const COORDINATE_SYSTEM_COLUMNS: Columns = &[
    ("database_name", varchar),
    ("database_oid", bigint),
    ("schema_name", varchar),
    ("schema_oid", bigint),
    ("crs_oid", bigint),
    ("crs_name", varchar),
    ("auth_name", varchar),
    ("auth_code", varchar),
    ("projjson", varchar),
    ("wkt2_2019", varchar),
];

/// The object ids of the default coordinate systems of a database, given when they are
/// first listed (DuckDB's `DefaultCoordinateSystemGenerator` creates their entries then).
struct CoordinateSystemOids(Vec<u64>);

/// `duckdb_coordinate_systems()` (`DuckDBCoordinateSystemsFunction`): the coordinate
/// systems of `system.main`, by name (the catalog set's order).
fn coordinate_system_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    use rustdb_types::geometry::DEFAULT_COORDINATE_SYSTEMS;
    const KEY: &str = "duckdb_coordinate_systems_oids";
    let new_oids = || {
        Arc::new(CoordinateSystemOids(
            DEFAULT_COORDINATE_SYSTEMS
                .iter()
                .map(|_| rustdb_catalog::next_oid())
                .collect(),
        ))
    };
    let oids = if let Some(cache) = context.function_client.object_cache() {
        cache.get::<CoordinateSystemOids>(KEY).unwrap_or_else(|| {
            let oids = new_oids();
            cache.put(KEY.to_owned(), Arc::clone(&oids));
            oids
        })
    } else {
        new_oids()
    };
    let Some((system, main)) = schemas(context).into_iter().find(|(catalog, schema)| {
        catalog.name().eq_ignore_ascii_case("system") && schema.name() == rustdb_catalog::DEFAULT_SCHEMA
    }) else {
        return Ok(Vec::new());
    };
    let mut systems: Vec<_> = DEFAULT_COORDINATE_SYSTEMS.iter().zip(&oids.0).collect();
    systems.sort_by_key(|(crs, _)| crs.name);
    Ok(systems
        .into_iter()
        .map(|(crs, crs_oid)| {
            vec![
                text(&*system.name()),
                oid(system.oid()),
                text(main.name()),
                oid(main.oid),
                oid(*crs_oid),
                text(crs.name),
                text(crs.auth_name),
                text(crs.auth_code),
                text(crs.projjson),
                text(crs.wkt2_2019),
            ]
        })
        .collect())
}

/// `duckdb_table_sample(table)` (`DuckDBTableSampleFunction`): the reservoir sample of a
/// table, with the table's columns. DuckDB 1.5's tables keep no sample
/// (`RowGroupCollection::GetSample` returns none), so it has no rows.
fn table_sample_function() -> TableFunctionSet {
    let function = TableFunction::new("duckdb_table_sample", vec![LogicalType::VARCHAR])
        .with_bind(|input| {
            let context = client_context(input)?;
            // `GetValue<string>`: a NULL argument reads as the name "NULL".
            let name = match input.inputs.first() {
                Some(value) if !value.is_null() => value.to_varchar()?,
                _ => "NULL".to_owned(),
            };
            let (catalog, schema, table) = crate::sequence::parse_qualified_name(&name)?;
            let (catalog, schema) = context.bind_schema_or_catalog(&catalog, &schema)?;
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
                return Err(Error::catalog(format!("{table} is not an table")));
            };
            let types: Vec<LogicalType> = entry
                .columns
                .iter()
                .map(|column| column.logical_type.clone())
                .collect();
            Ok(TableFunctionBindOutput {
                return_types: types.clone(),
                names: entry.columns.iter().map(|column| column.name.clone()).collect(),
                bind_data: Some(Arc::new(Rows {
                    types,
                    rows: Vec::new(),
                })),
            })
        })
        .with_scan(|bind_data, _, state, chunk| scan_rows(bind_data, state, chunk));
    let mut set = TableFunctionSet::new("duckdb_table_sample");
    set.add(function);
    set
}

/// `seq_scan` (`TableScanFunction`): the table scan as the catalog lists it. Table scans
/// are planned from the table; called directly it fails as it does in DuckDB, which has
/// no bind function for it.
fn seq_scan_function() -> TableFunctionSet {
    let function = TableFunction::new("seq_scan", Vec::new()).with_bind(|_| {
        Err(Error::invalid_input(
            "Cannot call function \"seq_scan\" directly - it has no bind function",
        ))
    });
    let mut set = TableFunctionSet::new("seq_scan");
    set.add(function);
    set
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

/// The value of a `SET VARIABLE` variable, read when `getvariable` is bound.
#[derive(Debug)]
struct VariableValue(Value);

impl FunctionData for VariableValue {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The type a `make_type` call names.
#[derive(Debug)]
struct MadeType(LogicalType);

impl FunctionData for MadeType {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `make_type(name, [parameters...])` (DuckDB's `MakeTypeFun`): the constant TYPE of the
/// type `name` with the (named) parameters, bound like a type in SQL text.
fn make_type_function() -> ScalarFunctionSet {
    use rustdb_types::{TypeInfo, TypeParameter, TypeParameterValue, UnboundTypeInfo};
    let mut function = ScalarFunction::new(
        "make_type",
        vec![LogicalType::VARCHAR],
        LogicalType::simple(LogicalTypeId::Type),
        |_: &DataChunk, _: &mut FunctionState, _: &mut Vector| {
            Err(Error::invalid_input(
                "make_type function can only be used in constant expressions",
            ))
        },
    )
    .with_bind(|_, input| {
        // `BindMakeTypeFunctionExpression`.
        let mut arguments = Vec::with_capacity(input.constants.len());
        for (index, constant) in input.constants.iter().enumerate() {
            let Some(value) = constant else {
                return Err(Error::binder(
                    "make_type function arguments must be constant expressions",
                ));
            };
            arguments.push((
                input.aliases.get(index).cloned().unwrap_or_default(),
                value.clone(),
            ));
        }
        let Some((_, name)) = arguments.first() else {
            return Err(Error::binder("make_type function requires at least one argument"));
        };
        if name.logical_type().id() != LogicalTypeId::Varchar {
            return Err(Error::binder(
                "make_type function first argument must be the type name as VARCHAR",
            ));
        }
        let (catalog, schema, name) = crate::sequence::parse_qualified_name(&name.to_varchar()?)?;
        let parameters = arguments[1..]
            .iter()
            .map(|(alias, value)| TypeParameter {
                name: alias.clone(),
                value: TypeParameterValue::Constant(value.clone()),
                query_location: None,
            })
            .collect();
        let unbound = LogicalType::with_info(
            LogicalTypeId::Unbound,
            TypeInfo::Unbound(UnboundTypeInfo {
                catalog,
                schema,
                name,
                parameters,
                query_location: None,
            }),
        );
        let context = input
            .binder
            .and_then(|binder| binder.downcast_ref::<ClientContext>())
            .ok_or_else(|| Error::internal("make_type bound without a client context"))?;
        let bound = rustdb_planner::bind_logical_type(context, &unbound)?;
        Ok(Some(Arc::new(MadeType(bound)) as Arc<dyn FunctionData>))
    })
    .with_bind_expression(|data, _| {
        data.and_then(|data| data.as_any().downcast_ref::<MadeType>())
            .map(|made| Value::type_value(made.0.clone()))
    });
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = rustdb_function::NullHandling::Special;
    let mut set = ScalarFunctionSet::new("make_type");
    set.add(function);
    set
}

/// `getvariable(name)` (DuckDB's `GetVariableFun`): replaced by the variable's value,
/// a constant of the value's type, when bound.
fn getvariable_function() -> ScalarFunctionSet {
    let function = ScalarFunction::new(
        "getvariable",
        vec![LogicalType::VARCHAR],
        LogicalType::ANY,
        |_: &DataChunk, _: &mut FunctionState, _: &mut Vector| {
            Err(Error::internal("getvariable is replaced by its value when bound"))
        },
    )
    .with_bind(|function, input| {
        if input.argument_types[0].id() == LogicalTypeId::Unknown {
            return Err(Error::new(
                rustdb_common::ErrorKind::ParameterNotResolved,
                "Parameter types could not be resolved",
            ));
        }
        let Some(name) = &input.constants[0] else {
            return Err(Error::not_implemented("getvariable requires a constant input"));
        };
        let context = input
            .binder
            .and_then(|binder| binder.downcast_ref::<ClientContext>())
            .ok_or_else(|| Error::internal("getvariable bound without a client context"))?;
        let value = if name.is_null() {
            Value::null(LogicalType::SQLNULL)
        } else {
            context
                .user_variables
                .get(&name.to_varchar()?)
                .cloned()
                .unwrap_or_else(|| Value::null(LogicalType::SQLNULL))
        };
        function.return_type = value.logical_type().clone();
        Ok(Some(Arc::new(VariableValue(value)) as Arc<dyn FunctionData>))
    })
    .with_bind_expression(|data, _| {
        data.and_then(|data| data.as_any().downcast_ref::<VariableValue>())
            .map(|variable| variable.0.clone())
    });
    let mut set = ScalarFunctionSet::new("getvariable");
    set.add(function);
    set
}

/// `current_database()`, `current_schema()`, `in_search_path()` and `getvariable()`.
pub(crate) fn context_scalar_functions() -> Vec<ScalarFunctionSet> {
    vec![
        context_function("current_database", rustdb_planner::ClientContext::default_catalog),
        context_function("current_schema", |context| context.default_schema().to_owned()),
        in_search_path_function(),
        getvariable_function(),
        make_type_function(),
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
    let (catalog, schema) = context.bind_schema_or_catalog(&catalog, &schema)?;
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

const SHOW_COLUMNS: Columns = &[
    ("column_name", varchar),
    ("column_type", varchar),
    ("null", varchar),
    ("key", varchar),
    ("default", varchar),
    ("extra", varchar),
];

/// `pragma_show(name)` (`PragmaTableInfoFunction` with `PragmaShowHelper`): the columns of
/// a table or view in the form of DESCRIBE.
fn show_rows(context: &ClientContext, name: &str) -> Result<Vec<Vec<Value>>> {
    let (catalog, schema, table) = crate::sequence::parse_qualified_name(name)?;
    let (catalog, schema) = context.bind_schema_or_catalog(&catalog, &schema)?;
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
                // `ColumnConstraintInfo`.
                let (mut pk, mut unique) = (false, false);
                for constraint in &entry.constraints {
                    let rustdb_ast::Constraint::Unique(constraint) = constraint else {
                        continue;
                    };
                    let covers = if constraint.index.0 == u64::MAX {
                        constraint
                            .columns
                            .iter()
                            .any(|c| c.eq_ignore_ascii_case(&column.name))
                    } else {
                        constraint.index.0 == index as u64
                    };
                    if covers {
                        if constraint.is_primary_key {
                            pk = true;
                        } else {
                            unique = true;
                        }
                    }
                }
                let default = column
                    .generated
                    .as_ref()
                    .or(column.default_value.as_ref())
                    .map_or_else(null_text, |expression| text(expression.to_string()));
                let key = if pk {
                    text("PRI")
                } else if unique {
                    text("UNI")
                } else {
                    null_text()
                };
                rows.push(vec![
                    text(column.name.clone()),
                    text(column.logical_type.to_string()),
                    text(if entry.not_null_columns.contains(&index) {
                        "NO"
                    } else {
                        "YES"
                    }),
                    key,
                    default,
                    null_text(),
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
                    text(name),
                    text(ty.to_string()),
                    text("YES"),
                    null_text(),
                    null_text(),
                    null_text(),
                ]);
            }
        }
        _ => {}
    }
    Ok(rows)
}

const VERSION_COLUMNS: Columns = &[
    ("library_version", varchar),
    ("source_id", varchar),
    ("codename", varchar),
];

/// `pragma_version()` (`PragmaVersionFunction`): the library version, source id and
/// release codename (`DuckDB::ReleaseCodename`).
fn version_rows(_: &ClientContext) -> Result<Vec<Vec<Value>>> {
    use crate::Database;
    Ok(vec![vec![
        text(Database::library_version()),
        text(Database::source_id()),
        text(Database::release_codename()),
    ]])
}

const EXTENSIONS_COLUMNS: Columns = &[
    ("extension_name", varchar),
    ("loaded", boolean),
    ("installed", boolean),
    ("install_path", varchar),
    ("description", varchar),
    ("aliases", varchar_list),
    ("extension_version", varchar),
    ("install_mode", varchar),
    ("installed_from", varchar),
];

/// `duckdb_extensions()`.
fn extensions_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let cache = context.function_client.object_cache();
    let settings = context.function_client.settings();
    let config = crate::extensions::ExtensionConfig::from_settings(|name| {
        settings.as_ref()?.current_setting(name).ok()
    });
    let client_extensions = cache
        .as_deref()
        .and_then(|cache| {
            cache.get::<crate::database::ClientExtensions>(crate::database::CLIENT_EXTENSIONS_KEY)
        })
        .map(|extensions| {
            extensions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        })
        .unwrap_or_default();
    crate::extensions::extension_rows(
        &config,
        |name| {
            cache
                .as_deref()
                .is_none_or(|cache| crate::database::static_extension_loaded(cache, name))
        },
        &client_extensions,
    )
}

const PLATFORM_COLUMNS: Columns = &[("platform", varchar)];

/// `pragma_platform()`.
fn platform_rows(_: &ClientContext) -> Result<Vec<Vec<Value>>> {
    Ok(vec![vec![text(crate::extensions::platform())]])
}

const USER_AGENT_COLUMNS: Columns = &[("user_agent", varchar)];

/// `pragma_user_agent()` (`DBConfig::UserAgent`): `duckdb/<version>(<platform>)` followed
/// by the `duckdb_api` and `custom_user_agent` settings.
fn user_agent_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let mut user_agent = format!(
        "duckdb/{}({})",
        rustdb_storage::storage_info::LIBRARY_VERSION,
        crate::extensions::platform()
    );
    let setting = |name: &str| {
        context
            .function_client
            .settings()
            .and_then(|settings| settings.current_setting(name).ok())
            .filter(|value| !value.is_null())
            .and_then(|value| value.to_varchar().ok())
            .unwrap_or_default()
    };
    for name in ["duckdb_api", "custom_user_agent"] {
        let value = setting(name);
        if !value.is_empty() {
            user_agent.push(' ');
            user_agent.push_str(&value);
        }
    }
    Ok(vec![vec![text(user_agent)]])
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
            text(&*catalog.name()),
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

const MEMORY_COLUMNS: Columns = &[
    ("tag", varchar),
    ("memory_usage_bytes", bigint),
    ("temporary_storage_bytes", bigint),
];

/// `duckdb_memory()` (`DuckDBMemoryFunction`): memory and spilled bytes per memory tag,
/// clamped to 0 beyond BIGINT (`ClampReportedMemory`).
fn memory_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let Some(io) = context.catalogs.iter().find_map(|catalog| catalog.io().ok()) else {
        return Ok(Vec::new());
    };
    let clamp = |bytes: u64| Value::bigint(i64::try_from(bytes).unwrap_or(0));
    Ok(io
        .buffer_manager()
        .memory_usage_info()
        .into_iter()
        .map(|info| vec![text(info.tag.name()), clamp(info.size), clamp(info.evicted_data)])
        .collect())
}

/// `duckdb_temporary_files()` (`DuckDBTemporaryFilesFunction`): the files of the
/// temporary directory and their sizes.
fn temporary_file_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let Some(io) = context.catalogs.iter().find_map(|catalog| catalog.io().ok()) else {
        return Ok(Vec::new());
    };
    Ok(io
        .buffer_manager()
        .temporary_files()
        .into_iter()
        .map(|file| {
            vec![
                text(&file.path),
                Value::bigint(i64::try_from(file.size).unwrap_or(i64::MAX)),
            ]
        })
        .collect())
}

fn bigint_list() -> LogicalType {
    LogicalType::list(LogicalType::BIGINT)
}

fn varchar_list() -> LogicalType {
    LogicalType::list(LogicalType::VARCHAR)
}

const SETTING_COLUMNS: Columns = &[
    ("name", varchar),
    ("value", varchar),
    ("description", varchar),
    ("input_type", varchar),
    ("scope", varchar),
    ("aliases", varchar_list),
];

/// `duckdb_settings()` (`DuckDBSettingsFunction`): every option with its current value.
fn setting_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let settings = context
        .function_client
        .settings()
        .map(|settings| settings.settings())
        .unwrap_or_default();
    Ok(settings
        .into_iter()
        .map(|setting| {
            vec![
                text(setting.name),
                text(setting.value),
                text(setting.description),
                text(setting.input_type),
                text(if setting.global { "GLOBAL" } else { "LOCAL" }),
                Value::list(
                    LogicalType::VARCHAR,
                    setting.aliases.into_iter().map(text).collect(),
                ),
            ]
        })
        .collect())
}

const KEYWORD_COLUMNS: Columns = &[("keyword_name", varchar), ("keyword_category", varchar)];

/// `duckdb_keywords()` (`DuckDBKeywordsFunction`): the grammar's keywords.
fn keyword_rows(_: &ClientContext) -> Result<Vec<Vec<Value>>> {
    use rustdb_parser::KeywordCategory;
    Ok(rustdb_parser::keyword_list()
        .iter()
        .map(|keyword| {
            let category = match keyword.category {
                KeywordCategory::Reserved => "reserved",
                KeywordCategory::Unreserved => "unreserved",
                KeywordCategory::TypeFunc => "type_function",
                KeywordCategory::ColName => "column_name",
            };
            vec![text(keyword.name), text(category)]
        })
        .collect())
}

const OPTIMIZER_COLUMNS: Columns = &[("name", varchar)];

/// `duckdb_optimizers()`: the optimizer names (`ListAllOptimizers`).
fn optimizer_rows(_: &ClientContext) -> Result<Vec<Vec<Value>>> {
    const OPTIMIZERS: &[&str] = &[
        "expression_rewriter",
        "filter_pullup",
        "filter_pushdown",
        "empty_result_pullup",
        "cte_filter_pusher",
        "regex_range",
        "in_clause",
        "join_order",
        "deliminator",
        "unnest_rewriter",
        "unused_columns",
        "statistics_propagation",
        "common_subexpressions",
        "common_aggregate",
        "column_lifetime",
        "limit_pushdown",
        "row_group_pruner",
        "top_n",
        "top_n_window_elimination",
        "build_side_probe_side",
        "compressed_materialization",
        "duplicate_groups",
        "reorder_filter",
        "sampling_pushdown",
        "join_filter_pushdown",
        "extension",
        "materialized_cte",
        "sum_rewriter",
        "late_materialization",
        "cte_inlining",
        "common_subplan",
        "join_elimination",
        "window_self_join",
    ];
    Ok(OPTIMIZERS.iter().map(|name| vec![text(*name)]).collect())
}

fn ubigint() -> LogicalType {
    LogicalType::UBIGINT
}

const DATABASE_COUNT_COLUMNS: Columns = &[("approx_count", ubigint)];

const CONNECTION_COUNT_COLUMNS: Columns = &[("count", ubigint)];

const COLLATION_COLUMNS: Columns = &[("collname", varchar)];

const VARIABLE_COLUMNS: Columns = &[("name", varchar), ("value", varchar), ("type", varchar)];

/// `duckdb_variables()`: the `SET VARIABLE` values of the connection.
fn variable_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    context
        .user_variables
        .iter()
        .map(|(name, value)| {
            Ok(vec![
                text(name),
                text(value.to_varchar()?),
                text(value.logical_type().to_string()),
            ])
        })
        .collect()
}

const PREPARED_STATEMENT_COLUMNS: Columns = &[
    ("name", varchar),
    ("statement", varchar),
    ("parameter_types", varchar_list),
    ("result_types", varchar_list),
];

/// `duckdb_prepared_statements()`: the prepared statements of the connection with their
/// parameter types (UNKNOWN until executed) and result types (when planned).
fn prepared_statement_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    use rustdb_planner::StatementReturnType;
    let names = |types: Vec<String>| {
        Value::list(
            LogicalType::VARCHAR,
            types.into_iter().map(Value::varchar).collect(),
        )
    };
    let null_list = || Value::null(varchar_list());
    Ok(context
        .prepared_statements
        .iter()
        .map(|(name, prepared)| {
            let statement = &prepared.unbound_statement;
            let sql = match &statement.kind {
                rustdb_ast::SqlStatementKind::Select(select) => select.to_string(),
                rustdb_ast::SqlStatementKind::Insert(insert) => insert.to_string(),
                _ => statement.query.clone(),
            };
            let parameters = if statement.named_param_map.is_empty() {
                null_list()
            } else {
                names(vec!["UNKNOWN".to_owned(); prepared.properties.parameter_count])
            };
            let results = match prepared.properties.return_type {
                StatementReturnType::QueryResult if prepared.properties.bound_all_parameters => {
                    names(prepared.types.iter().map(ToString::to_string).collect())
                }
                StatementReturnType::ChangedRows => names(vec!["BIGINT".to_owned()]),
                _ => null_list(),
            };
            vec![text(name), text(sql), parameters, results]
        })
        .collect())
}

const TEMPORARY_FILE_COLUMNS: Columns = &[("path", varchar), ("size", bigint)];

const DEPENDENCY_COLUMNS: Columns = &[
    ("classid", bigint),
    ("objid", bigint),
    ("objsubid", integer),
    ("refclassid", bigint),
    ("refobjid", bigint),
    ("refobjsubid", integer),
    ("deptype", varchar),
];

/// `duckdb_dependencies()`: for every entry of the default catalog that depends on
/// another, the depended-on entry (`objid`) and the dependent (`refobjid`)
/// (`DependencyManager::Scan`).
fn dependency_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let mut entries = Vec::new();
    if let Some(catalog) = context.catalogs.first() {
        for schema in catalog.schemas().scan(&context.transaction) {
            let Some(contents) = schema_contents(&schema) else {
                continue;
            };
            for set in [
                &contents.tables,
                &contents.functions,
                &contents.table_functions,
                &contents.sequences,
                &contents.types,
                &contents.indexes,
            ] {
                for entry in set.scan(&context.transaction) {
                    entries.push((schema.name().to_owned(), entry));
                }
            }
        }
    }
    let row = |object: u64, dependent: u64, kind: &str| {
        vec![
            Value::bigint(0),
            Value::bigint(object as i64),
            Value::integer(0),
            Value::bigint(0),
            Value::bigint(dependent as i64),
            Value::integer(0),
            text(kind),
        ]
    };
    let mut rows = Vec::new();
    for (_, dependent) in &entries {
        for dependency in &dependent.dependencies {
            if let Some((_, object)) = entries
                .iter()
                .find(|(schema, entry)| entry.is_described_by(schema, &dependency.entry))
            {
                rows.push(row(object.oid, dependent.oid, "n"));
            }
        }
        if let Some(owner) = &dependent.owned_by
            && let Some((_, object)) = entries
                .iter()
                .find(|(schema, entry)| entry.is_described_by(schema, owner))
        {
            rows.push(row(object.oid, dependent.oid, "a"));
        }
    }
    Ok(rows)
}

/// `pragma_collations()`: the builtin collations (`PragmaCollations`; the ICU collations
/// come with the ICU extension).
fn collation_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let mut names: Vec<String> = ["nocase", "noaccent", "nfc"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    if let Some(system) = context.catalog("system")
        && let Some(schema) = system.schema(&context.transaction, rustdb_catalog::DEFAULT_SCHEMA)
        && let Some(schema) = schema.as_schema()
    {
        names.extend(
            schema
                .collations
                .scan(&context.transaction)
                .iter()
                .map(|entry| entry.name().to_owned()),
        );
    }
    names.sort();
    Ok(names.into_iter().map(|name| vec![Value::varchar(name)]).collect())
}

/// `duckdb_connection_count()`: the number of open connections
/// (`ConnectionManager::GetConnectionCount`).
fn connection_count_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let count = context.function_client.connection_count().unwrap_or(1);
    Ok(vec![vec![Value::ubigint(count)]])
}

/// `duckdb_approx_database_count()`: the number of attached database files
/// (`DatabasePathManager::ApproxDatabaseCount`; in-memory databases have no path).
fn database_count_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let count = context
        .catalogs
        .iter()
        .filter(|catalog| {
            catalog
                .attach_info()
                .is_some_and(|info| !info.path.is_empty() && info.path != ":memory:")
        })
        .count();
    Ok(vec![vec![Value::ubigint(count as u64)]])
}

const FUNCTION_COLUMNS: Columns = &[
    ("database_name", varchar),
    ("database_oid", varchar),
    ("schema_name", varchar),
    ("function_name", varchar),
    ("alias_of", varchar),
    ("function_type", varchar),
    ("description", varchar),
    ("comment", varchar),
    ("tags", string_map),
    ("return_type", varchar),
    ("parameters", varchar_list),
    ("parameter_types", varchar_list),
    ("varargs", varchar),
    ("macro_definition", varchar),
    ("has_side_effects", boolean),
    ("internal", boolean),
    ("function_oid", bigint),
    ("examples", varchar_list),
    ("stability", varchar),
    ("categories", varchar_list),
];

/// One overload as `duckdb_functions()` lists it (the `*Extractor` structs of DuckDB's
/// `duckdb_functions.cpp`).
struct FunctionRow {
    function_type: &'static str,
    return_type: Value,
    parameters: Vec<String>,
    /// `None` for untyped macro parameters (NULL).
    parameter_types: Vec<Option<String>>,
    varargs: Value,
    macro_definition: Value,
    has_side_effects: Value,
    stability: Value,
    /// The matching `FunctionDescription` (description, examples, categories).
    description: Option<rustdb_function::function_descriptions::FunctionDescription>,
}

/// `GetFunctionDescriptionIndex` and `GetParameterNames`: the description of a built-in
/// overload with `arguments`, and its parameter names (`col<i>` beyond the described ones).
fn describe_overload(
    info: Option<&rustdb_function::function_descriptions::FunctionInfo>,
    arguments: &[LogicalType],
) -> (
    Vec<String>,
    Option<rustdb_function::function_descriptions::FunctionDescription>,
) {
    let description = info.and_then(|info| {
        rustdb_function::function_descriptions::description_index(&info.descriptions, arguments)
            .map(|index| info.descriptions[index].clone())
    });
    let parameters = match &description {
        Some(description) if !description.parameter_names.is_empty() => (0..arguments.len())
            .map(|index| {
                description
                    .parameter_names
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("col{index}"))
            })
            .collect(),
        _ => positional_names(arguments.len()),
    };
    (parameters, description)
}

/// `FunctionStabilityToValue`.
fn stability_value(stability: rustdb_function::Stability) -> Value {
    text(match stability {
        rustdb_function::Stability::Volatile => "VOLATILE",
        rustdb_function::Stability::ConsistentWithinQuery => "CONSISTENT_WITHIN_QUERY",
        rustdb_function::Stability::Consistent => "CONSISTENT",
    })
}

/// `col0, col1, ...`: the parameter names of built-in functions.
fn positional_names(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("col{index}")).collect()
}

/// The rows of a macro's overloads (`MacroExtractor` / `TableMacroExtractor`).
fn macro_rows(entry: &rustdb_catalog::MacroEntry, table: bool) -> Vec<FunctionRow> {
    entry
        .macros
        .iter()
        .map(|function| {
            let parameters: Vec<String> = function
                .parameters
                .iter()
                .map(|parameter| match &parameter.kind {
                    rustdb_ast::ParsedExpressionKind::ColumnRef(column) => {
                        column.column_names.last().cloned().unwrap_or_default()
                    }
                    _ => parameter.to_string(),
                })
                .collect();
            let parameter_types = (0..parameters.len())
                .map(|index| match function.types.get(index) {
                    Some(ty) if ty.id() != LogicalTypeId::Unknown => Some(ty.to_string()),
                    _ => None,
                })
                .collect();
            let macro_definition = match &function.kind {
                rustdb_ast::MacroFunctionKind::Scalar(scalar) => text(scalar.expression.to_string()),
                rustdb_ast::MacroFunctionKind::Table(table) => text(table.query_node.to_string()),
            };
            FunctionRow {
                function_type: if table { "table_macro" } else { "macro" },
                return_type: null_text(),
                parameters,
                parameter_types,
                varargs: null_text(),
                macro_definition,
                has_side_effects: Value::null(LogicalType::BOOLEAN),
                stability: null_text(),
                description: None,
            }
        })
        .collect()
}

/// The rows of one catalog entry, one per overload; `info` describes a built-in function.
fn entry_function_rows(
    entry: &CatalogEntry,
    info: Option<&rustdb_function::function_descriptions::FunctionInfo>,
) -> Vec<FunctionRow> {
    let varargs = |varargs: Option<&LogicalType>| varargs.map_or_else(null_text, |ty| text(ty.to_string()));
    match &entry.kind {
        EntryKind::ScalarFunction(set) => set
            .functions
            .iter()
            .map(|function| {
                let (parameters, description) = describe_overload(info, &function.arguments);
                FunctionRow {
                    function_type: "scalar",
                    return_type: text(function.return_type.to_string()),
                    parameters,
                    parameter_types: function.arguments.iter().map(|ty| Some(ty.to_string())).collect(),
                    varargs: varargs(function.varargs.as_ref()),
                    macro_definition: null_text(),
                    has_side_effects: Value::boolean(
                        function.stability == rustdb_function::Stability::Volatile,
                    ),
                    stability: stability_value(function.stability),
                    description,
                }
            })
            .collect(),
        EntryKind::AggregateFunction(set) => set
            .functions
            .iter()
            .map(|function| {
                let (parameters, description) = describe_overload(info, &function.arguments);
                FunctionRow {
                    function_type: "aggregate",
                    return_type: text(function.return_type.to_string()),
                    parameters,
                    parameter_types: function.arguments.iter().map(|ty| Some(ty.to_string())).collect(),
                    varargs: varargs(function.varargs.as_ref()),
                    macro_definition: null_text(),
                    has_side_effects: Value::boolean(false),
                    stability: stability_value(rustdb_function::Stability::Consistent),
                    description,
                }
            })
            .collect(),
        EntryKind::TableFunction(set) => set
            .functions
            .iter()
            .map(|function| {
                let mut parameters = positional_names(function.arguments.len());
                let mut parameter_types: Vec<Option<String>> =
                    function.arguments.iter().map(|ty| Some(ty.to_string())).collect();
                for (name, ty) in rustdb_function::named_parameters_in_map_order(&function.named_parameters) {
                    parameters.push(name.clone());
                    parameter_types.push(Some(ty.to_string()));
                }
                FunctionRow {
                    function_type: "table",
                    return_type: null_text(),
                    parameters,
                    parameter_types,
                    varargs: varargs(function.varargs.as_ref()),
                    macro_definition: null_text(),
                    has_side_effects: Value::null(LogicalType::BOOLEAN),
                    stability: null_text(),
                    description: None,
                }
            })
            .collect(),
        EntryKind::PragmaFunction(set) => set
            .functions
            .iter()
            .map(|function| {
                // `PragmaFunctionExtractor`.
                let mut parameters = positional_names(function.arguments.len());
                let mut parameter_types: Vec<Option<String>> =
                    function.arguments.iter().map(|ty| Some(ty.to_string())).collect();
                for (name, ty) in rustdb_function::named_parameters_in_map_order(&function.named_parameters) {
                    parameters.push(name.clone());
                    parameter_types.push(Some(ty.to_string()));
                }
                FunctionRow {
                    function_type: "pragma",
                    return_type: null_text(),
                    parameters,
                    parameter_types,
                    varargs: varargs(function.varargs.as_ref()),
                    macro_definition: null_text(),
                    has_side_effects: Value::null(LogicalType::BOOLEAN),
                    stability: null_text(),
                    description: None,
                }
            })
            .collect(),
        EntryKind::Macro(macro_entry) => macro_rows(macro_entry, false),
        EntryKind::TableMacro(macro_entry) => macro_rows(macro_entry, true),
        _ => Vec::new(),
    }
}

/// The window functions (`WindowExpression::WindowFunctions`) with their signatures
/// (`ExtractWindowFunctionData`).
fn window_function_rows() -> Vec<(&'static str, Vec<LogicalType>, LogicalType)> {
    let template = || {
        LogicalType::with_info(
            LogicalTypeId::Template,
            rustdb_types::TypeInfo::Template(rustdb_types::TemplateTypeInfo { name: "T".to_owned() }),
        )
    };
    vec![
        ("rank", Vec::new(), LogicalType::BIGINT),
        ("rank_dense", Vec::new(), LogicalType::BIGINT),
        ("dense_rank", Vec::new(), LogicalType::BIGINT),
        ("percent_rank", Vec::new(), LogicalType::DOUBLE),
        ("row_number", Vec::new(), LogicalType::BIGINT),
        ("first_value", vec![template()], template()),
        ("first", vec![template()], template()),
        ("last_value", vec![template()], template()),
        ("last", vec![template()], template()),
        ("nth_value", vec![template(), LogicalType::BIGINT], template()),
        ("cume_dist", Vec::new(), LogicalType::DOUBLE),
        (
            "lead",
            vec![template(), LogicalType::BIGINT, template()],
            template(),
        ),
        (
            "lag",
            vec![template(), LogicalType::BIGINT, template()],
            template(),
        ),
        ("ntile", vec![LogicalType::BIGINT], LogicalType::BIGINT),
        ("fill", vec![template()], template()),
    ]
}

/// `duckdb_functions()` (`DuckDBFunctionsFunction`): every function overload of every
/// schema, then the window functions. Descriptions, examples and categories are not
/// recorded by the function sets, so they are empty.
fn function_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    let list =
        |values: Vec<String>| Value::list(LogicalType::VARCHAR, values.into_iter().map(text).collect());
    let mut rows = Vec::new();
    let mut push = |catalog_name: &str,
                    catalog_oid: u64,
                    schema_name: &str,
                    name: &str,
                    alias_of: Option<&str>,
                    internal: bool,
                    function_oid: u64,
                    comment: &Value,
                    row: FunctionRow| {
        let description = row.description.unwrap_or_default();
        rows.push(vec![
            text(catalog_name),
            text(catalog_oid.to_string()),
            text(schema_name),
            text(name),
            alias_of
                .filter(|alias_of| *alias_of != name)
                .map_or_else(null_text, text),
            text(row.function_type),
            if description.description.is_empty() {
                null_text()
            } else {
                text(description.description)
            },
            comment_text(comment),
            empty_map(),
            row.return_type,
            list(row.parameters),
            Value::list(
                LogicalType::VARCHAR,
                row.parameter_types
                    .into_iter()
                    .map(|ty| ty.map_or_else(null_text, text))
                    .collect(),
            ),
            row.varargs,
            row.macro_definition,
            row.has_side_effects,
            Value::boolean(internal),
            oid(function_oid),
            list(description.examples),
            row.stability,
            list(description.categories),
        ]);
    };
    for (catalog, schema) in schemas(context) {
        let Some(contents) = schema_contents(&schema) else {
            continue;
        };
        let mut entries = contents.functions.scan(&context.transaction);
        entries.extend(contents.table_functions.scan(&context.transaction));
        entries.extend(contents.pragma_functions.scan(&context.transaction));
        // `std::sort` by DuckDB's `CatalogType` value; a stable sort keeps the scan order.
        entries.sort_by_key(|entry| match entry.catalog_type() {
            rustdb_catalog::CatalogType::TableFunction => 25,
            rustdb_catalog::CatalogType::ScalarFunction => 26,
            rustdb_catalog::CatalogType::AggregateFunction => 27,
            rustdb_catalog::CatalogType::PragmaFunction => 28,
            rustdb_catalog::CatalogType::Macro => 30,
            rustdb_catalog::CatalogType::TableMacro => 31,
            _ => 99,
        });
        for entry in entries {
            // Built-in scalar and aggregate functions carry their function list entry.
            let info = (entry.internal
                && matches!(
                    entry.kind,
                    EntryKind::ScalarFunction(_) | EntryKind::AggregateFunction(_)
                ))
            .then(|| rustdb_function::function_descriptions::function_info(entry.name()))
            .flatten();
            for row in entry_function_rows(&entry, info.as_ref()) {
                push(
                    &catalog.name(),
                    catalog.oid(),
                    schema.name(),
                    entry.name(),
                    info.as_ref().map(|info| info.alias_of.as_str()),
                    entry.internal,
                    entry.oid,
                    &entry.comment,
                    row,
                );
            }
        }
    }
    for (name, arguments, return_type) in window_function_rows() {
        push(
            "system",
            0,
            "main",
            name,
            None,
            true,
            0,
            &Value::default(),
            FunctionRow {
                function_type: "aggregate",
                return_type: text(return_type.to_string()),
                parameters: positional_names(arguments.len()),
                parameter_types: arguments.iter().map(|ty| Some(ty.to_string())).collect(),
                varargs: null_text(),
                macro_definition: null_text(),
                has_side_effects: Value::boolean(false),
                stability: stability_value(rustdb_function::Stability::Consistent),
                description: None,
            },
        );
    }
    Ok(rows)
}

const TYPE_COLUMNS: Columns = &[
    ("database_name", varchar),
    ("database_oid", bigint),
    ("schema_name", varchar),
    ("schema_oid", bigint),
    ("type_oid", bigint),
    ("type_name", varchar),
    ("type_size", bigint),
    ("logical_type", varchar),
    ("type_category", varchar),
    ("comment", varchar),
    ("tags", string_map),
    ("internal", boolean),
    ("labels", varchar_list),
];

/// The `type_category` of `duckdb_types()`.
fn type_category(id: LogicalTypeId) -> Option<&'static str> {
    use LogicalTypeId as L;
    Some(match id {
        L::TinyInt
        | L::SmallInt
        | L::Integer
        | L::BigInt
        | L::Decimal
        | L::Float
        | L::Double
        | L::UTinyInt
        | L::USmallInt
        | L::UInteger
        | L::UBigInt
        | L::HugeInt
        | L::UHugeInt => "NUMERIC",
        L::Date
        | L::Time
        | L::TimeNs
        | L::TimestampSec
        | L::TimestampMs
        | L::Timestamp
        | L::TimestampNs
        | L::Interval
        | L::TimeTz
        | L::TimestampTz => "DATETIME",
        L::Char | L::Varchar => "STRING",
        L::Boolean => "BOOLEAN",
        L::Struct | L::List | L::Array | L::Map | L::Union => "COMPOSITE",
        L::Variant => "VARIANT",
        _ => return None,
    })
}

/// `duckdb_types()`: the builtin types of every catalog's `main` schema (DuckDB's
/// `DefaultTypeGenerator` entries) and the user types, by name within each schema. A
/// builtin type's oid is its type id, listed once.
fn type_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    struct TypeRow {
        name: String,
        ty: LogicalType,
        internal: bool,
        oid: u64,
        comment: Value,
    }
    let mut rows = Vec::new();
    let mut seen_oids = std::collections::HashSet::new();
    for (catalog, schema) in schemas(context) {
        let Some(contents) = schema_contents(&schema) else {
            continue;
        };
        let mut types: Vec<TypeRow> = Vec::new();
        if schema.name().eq_ignore_ascii_case(rustdb_catalog::DEFAULT_SCHEMA) {
            for name in rustdb_types::builtin_type_names() {
                let Some(id) = rustdb_types::builtin_type_id(name) else {
                    continue;
                };
                types.push(TypeRow {
                    name: name.to_owned(),
                    ty: LogicalType::simple(id),
                    internal: true,
                    oid: u64::from(id as u8),
                    comment: Value::default(),
                });
            }
        }
        for entry in contents.types.scan(&context.transaction) {
            if let EntryKind::Type(ty) = &entry.kind {
                types.push(TypeRow {
                    name: entry.name().to_owned(),
                    ty: ty.user_type.clone(),
                    internal: entry.internal,
                    oid: entry.oid,
                    comment: entry.comment.clone(),
                });
            }
        }
        // The catalog orders entries by their upper-cased names.
        types.sort_by_cached_key(|row| row.name.to_uppercase());
        for row in types {
            let type_oid = if seen_oids.insert(row.oid) {
                oid(row.oid)
            } else {
                Value::null(LogicalType::BIGINT)
            };
            let physical = row.ty.physical_type();
            let size = if physical == rustdb_types::PhysicalType::Invalid {
                Value::null(LogicalType::BIGINT)
            } else {
                count(physical.size())
            };
            let labels = row.ty.enum_values().map_or_else(
                || Value::null(LogicalType::list(LogicalType::VARCHAR)),
                |values| Value::list(LogicalType::VARCHAR, values.iter().map(text).collect()),
            );
            rows.push(vec![
                text(&*catalog.name()),
                oid(catalog.oid()),
                text(schema.name()),
                oid(schema.oid),
                type_oid,
                text(row.name),
                size,
                text(row.ty.id().name()),
                type_category(row.ty.id()).map_or_else(null_text, text),
                comment_text(&row.comment),
                empty_map(),
                Value::boolean(row.internal),
                labels,
            ]);
        }
    }
    Ok(rows)
}

/// The secret manager of the database a call is bound in.
fn secret_manager(context: &ClientContext) -> Result<&crate::secrets::SecretManager> {
    crate::database::instance_state(context)
        .map(|state| state.secrets.as_ref())
        .map_err(|_| Error::internal("secret functions bound without a secret manager"))
}

const SECRET_COLUMNS: Columns = &[
    ("name", varchar),
    ("type", varchar),
    ("provider", varchar),
    ("persistent", boolean),
    ("storage", varchar),
    ("scope", varchar_list),
    ("secret_string", varchar),
];

/// `duckdb_secrets(redact := true)` (`DuckDBSecretsFunction`).
fn secrets_function() -> TableFunctionSet {
    let mut function = TableFunction::new("duckdb_secrets", Vec::new()).with_bind(|input| {
        let context = client_context(input)?;
        let mut redact = true;
        for (name, value) in input.named_parameters {
            if name.eq_ignore_ascii_case("redact") {
                if value.is_null() {
                    return Err(Error::invalid_input("Cannot use NULL as argument for redact"));
                }
                redact = value.as_bool().unwrap_or(true);
            }
        }
        let allow_unredacted = context.config.allow_unredacted_secrets;
        if !redact && !allow_unredacted {
            return Err(Error::invalid_input("Displaying unredacted secrets is disabled"));
        }
        let mut rows = Vec::new();
        for entry in secret_manager(context)?.all_secrets()? {
            let secret = &entry.secret;
            rows.push(vec![
                text(&secret.name),
                text(&secret.secret_type),
                text(&secret.provider),
                Value::boolean(entry.persistent),
                text(&entry.storage),
                Value::list(LogicalType::VARCHAR, secret.scope.iter().map(text).collect()),
                text(secret.to_display_string(redact)),
            ]);
        }
        let types: Vec<LogicalType> = SECRET_COLUMNS.iter().map(|(_, ty)| ty()).collect();
        Ok(TableFunctionBindOutput {
            return_types: types.clone(),
            names: SECRET_COLUMNS
                .iter()
                .map(|(name, _)| (*name).to_owned())
                .collect(),
            bind_data: Some(Arc::new(Rows { types, rows })),
        })
    });
    function.named_parameters = vec![("redact".to_owned(), LogicalType::BOOLEAN)];
    let function = function.with_scan(|bind_data, _, state, chunk| scan_rows(bind_data, state, chunk));
    let mut set = TableFunctionSet::new("duckdb_secrets");
    set.add(function);
    set
}

/// `which_secret(path, type)` (`WhichSecretFunction`): the secret a file would use.
fn which_secret_function() -> TableFunctionSet {
    const COLUMNS: Columns = &[("name", varchar), ("persistent", varchar), ("storage", varchar)];
    let function = TableFunction::new("which_secret", vec![LogicalType::VARCHAR, LogicalType::VARCHAR])
        .with_bind(|input| {
            let context = client_context(input)?;
            let argument = |index: usize| -> Result<String> {
                input
                    .inputs
                    .get(index)
                    .filter(|value| !value.is_null())
                    .map(Value::to_varchar)
                    .transpose()
                    .map(Option::unwrap_or_default)
            };
            let path = argument(0)?;
            let secret_type = argument(1)?;
            let mut rows = Vec::new();
            if let Some(entry) = secret_manager(context)?.lookup_secret(&path, &secret_type)? {
                rows.push(vec![
                    text(&entry.secret.name),
                    text(if entry.persistent {
                        "PERSISTENT"
                    } else {
                        "TEMPORARY"
                    }),
                    text(&entry.storage),
                ]);
            }
            let types: Vec<LogicalType> = COLUMNS.iter().map(|(_, ty)| ty()).collect();
            Ok(TableFunctionBindOutput {
                return_types: types.clone(),
                names: COLUMNS.iter().map(|(name, _)| (*name).to_owned()).collect(),
                bind_data: Some(Arc::new(Rows { types, rows })),
            })
        })
        .with_scan(|bind_data, _, state, chunk| scan_rows(bind_data, state, chunk));
    let mut set = TableFunctionSet::new("which_secret");
    set.add(function);
    set
}

const CONSTRAINT_COLUMNS: Columns = &[
    ("database_name", varchar),
    ("database_oid", bigint),
    ("schema_name", varchar),
    ("schema_oid", bigint),
    ("table_name", varchar),
    ("table_oid", bigint),
    ("constraint_index", bigint),
    ("constraint_type", varchar),
    ("constraint_text", varchar),
    ("expression", varchar),
    ("constraint_column_indexes", bigint_list),
    ("constraint_column_names", varchar_list),
    ("constraint_name", varchar),
    ("referenced_table", varchar),
    ("referenced_column_names", varchar_list),
];

/// The column names a CHECK expression references (`ExtractReferencedColumns`).
struct ColumnNames(Vec<String>);

impl rustdb_ast::visit::VisitorMut for ColumnNames {
    fn visit_expression(&mut self, expression: &mut rustdb_ast::ParsedExpression) {
        if let rustdb_ast::ParsedExpressionKind::ColumnRef(column) = &expression.kind {
            self.0.extend(column.column_names.last().cloned());
        }
        rustdb_ast::visit::walk_expression(self, expression);
    }
}

/// `duckdb_constraints()` (`DuckDBConstraintsFunction`): the constraints of every table,
/// tables ordered by name within each schema.
fn constraint_rows(context: &ClientContext) -> Result<Vec<Vec<Value>>> {
    use rustdb_ast::Constraint;
    let mut rows = Vec::new();
    let mut constraint_index = 0i64;
    let list = |ty: LogicalType, values: Vec<Value>| Value::list(ty, values);
    for (catalog, schema) in schemas(context) {
        let Some(contents) = schema_contents(&schema) else {
            continue;
        };
        let mut tables: Vec<_> = contents
            .tables
            .scan(&context.transaction)
            .into_iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Table(_)))
            .collect();
        tables.sort_by(|left, right| left.name().cmp(right.name()));
        for entry in tables {
            let EntryKind::Table(table) = &entry.kind else {
                continue;
            };
            let column_index = |name: &str| {
                table
                    .columns
                    .iter()
                    .position(|column| column.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| Error::internal(format!("Column with name {name} does not exist")))
            };
            for constraint in &table.constraints {
                // `GetExtraConstraintInfo`.
                let mut indexes: Vec<usize> = Vec::new();
                let mut names: Vec<String> = Vec::new();
                let mut referenced_table = String::new();
                let mut referenced_columns: Vec<String> = Vec::new();
                let (constraint_type, suffix) = match constraint {
                    Constraint::Check(check) => {
                        let mut visitor = ColumnNames(Vec::new());
                        let mut expression = (*check.expression).clone();
                        rustdb_ast::visit::VisitorMut::visit_expression(&mut visitor, &mut expression);
                        names = visitor.0;
                        ("CHECK", "check")
                    }
                    Constraint::NotNull(not_null) => {
                        indexes.push(usize::try_from(not_null.index.0).unwrap_or(usize::MAX));
                        ("NOT NULL", "not_null")
                    }
                    Constraint::Unique(unique) => {
                        if unique.index.0 == u64::MAX {
                            names.clone_from(&unique.columns);
                        } else {
                            indexes.push(usize::try_from(unique.index.0).unwrap_or(usize::MAX));
                        }
                        if unique.is_primary_key {
                            ("PRIMARY KEY", "pkey")
                        } else {
                            ("UNIQUE", "key")
                        }
                    }
                    Constraint::ForeignKey(fk) => {
                        if fk.fk_type == rustdb_ast::ForeignKeyType::FkTypePrimaryKeyTable {
                            // Covered by the PRIMARY KEY and UNIQUE entries.
                            continue;
                        }
                        referenced_columns.clone_from(&fk.pk_columns);
                        referenced_table.clone_from(&fk.table);
                        names.clone_from(&fk.fk_columns);
                        ("FOREIGN KEY", "fkey")
                    }
                };
                if indexes.is_empty() {
                    indexes = names
                        .iter()
                        .map(|name| column_index(name))
                        .collect::<Result<_>>()?;
                } else {
                    names = indexes
                        .iter()
                        .map(|&index| {
                            table
                                .columns
                                .get(index)
                                .map(|column| column.name.clone())
                                .unwrap_or_default()
                        })
                        .collect();
                }
                // `GetConstraintName`.
                let mut constraint_name = format!("{}_", entry.name());
                for column in names.iter().chain(&referenced_columns) {
                    constraint_name.push_str(&column.to_lowercase());
                    constraint_name.push('_');
                }
                constraint_name.push_str(suffix);
                let expression = match constraint {
                    Constraint::Check(check) => text(check.expression.to_string()),
                    _ => null_text(),
                };
                rows.push(vec![
                    text(&*catalog.name()),
                    oid(catalog.oid()),
                    text(schema.name()),
                    oid(schema.oid),
                    text(entry.name()),
                    oid(entry.oid),
                    Value::bigint(constraint_index),
                    text(constraint_type),
                    text(constraint.to_sql()),
                    expression,
                    list(LogicalType::BIGINT, indexes.into_iter().map(count).collect()),
                    list(LogicalType::VARCHAR, names.into_iter().map(text).collect()),
                    text(constraint_name),
                    if referenced_table.is_empty() {
                        null_text()
                    } else {
                        text(referenced_table)
                    },
                    list(
                        LogicalType::VARCHAR,
                        referenced_columns.into_iter().map(text).collect(),
                    ),
                ]);
                constraint_index += 1;
            }
        }
    }
    Ok(rows)
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
    let (catalog, schema) = context.bind_schema_or_catalog(&catalog, &schema)?;
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
        // `Catalog::GetEntry<TableCatalogEntry>`.
        return Err(Error::catalog(format!("{table} is not an table")));
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

const METADATA_INFO_COLUMNS: Columns = &[
    ("block_id", bigint),
    ("total_blocks", bigint),
    ("free_blocks", bigint),
    ("free_list", bigint_list),
];

/// The rows of `pragma_metadata_info` for the catalog called `name`
/// (`PragmaMetadataInfoFunction`).
fn metadata_info_rows(context: &ClientContext, name: &str) -> Result<Vec<Vec<Value>>> {
    let catalog = context
        .catalog(name)
        .ok_or_else(|| Error::binder(format!("Catalog \"{name}\" does not exist!")))?;
    let as_bigint = |value: u64| Value::bigint(i64::try_from(value).unwrap_or(i64::MAX));
    Ok(catalog
        .metadata_info()
        .into_iter()
        .map(|block| {
            vec![
                Value::bigint(block.block_id),
                as_bigint(block.total_blocks),
                as_bigint(block.free_list.len() as u64),
                Value::list(
                    LogicalType::BIGINT,
                    block.free_list.into_iter().map(as_bigint).collect(),
                ),
            ]
        })
        .collect())
}

/// `pragma_metadata_info([database])`: the metadata blocks of the default or the given
/// database (`PragmaMetadataInfoBind`).
fn metadata_info_functions() -> TableFunctionSet {
    let name = "pragma_metadata_info";
    let mut set = rows_function(name, METADATA_INFO_COLUMNS, |context| {
        metadata_info_rows(context, &context.default_catalog())
    });
    set.add(
        TableFunction::new(name, vec![LogicalType::VARCHAR])
            .with_bind(move |input| {
                let context = client_context(input)?;
                let database = match input.inputs.first() {
                    Some(value) if value.is_null() => {
                        return Err(Error::binder(
                            "Database argument for pragma_metadata_info cannot be NULL",
                        ));
                    }
                    Some(value) => value.to_varchar()?,
                    None => context.default_catalog(),
                };
                let types: Vec<LogicalType> = METADATA_INFO_COLUMNS.iter().map(|(_, ty)| ty()).collect();
                let rows = metadata_info_rows(context, &database)?;
                Ok(TableFunctionBindOutput {
                    return_types: types.clone(),
                    names: METADATA_INFO_COLUMNS
                        .iter()
                        .map(|(name, _)| (*name).to_owned())
                        .collect(),
                    bind_data: Some(Arc::new(Rows { types, rows })),
                })
            })
            .with_scan(|bind_data, _, state, chunk| scan_rows(bind_data, state, chunk)),
    );
    set
}

/// The `pragma_*` table functions.
pub(crate) fn pragma_table_functions() -> Vec<TableFunctionSet> {
    vec![
        rows_function_with_name("pragma_table_info", TABLE_INFO_COLUMNS, table_info_rows),
        rows_function_with_name("pragma_storage_info", STORAGE_INFO_COLUMNS, storage_info_rows),
        rows_function("pragma_database_size", DATABASE_SIZE_COLUMNS, database_size_rows),
        metadata_info_functions(),
        rows_function_with_name("pragma_show", SHOW_COLUMNS, show_rows),
        rows_function("pragma_version", VERSION_COLUMNS, version_rows),
        rows_function("pragma_platform", PLATFORM_COLUMNS, platform_rows),
        rows_function("pragma_user_agent", USER_AGENT_COLUMNS, user_agent_rows),
        rows_function("duckdb_extensions", EXTENSIONS_COLUMNS, extensions_rows),
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

/// The built-in pragmas (DuckDB's `PragmaFunctions::RegisterFunction` and
/// `PragmaQueries::RegisterFunction`); extensions add their own.
pub(crate) fn builtin_pragma_functions() -> Vec<PragmaFunctionSet> {
    let varchar = || LogicalType::VARCHAR;
    let statements = [
        "enable_profile",
        "enable_profiling",
        "disable_profile",
        "disable_profiling",
        "enable_verification",
        "disable_verification",
        "verify_external",
        "disable_verify_external",
        "verify_fetch_row",
        "disable_verify_fetch_row",
        "verify_serializer",
        "disable_verify_serializer",
        "verify_parallelism",
        "disable_verify_parallelism",
        "enable_object_cache",
        "disable_object_cache",
        "enable_optimizer",
        "disable_optimizer",
        "force_checkpoint",
        "enable_progress_bar",
        "disable_progress_bar",
        "enable_print_progress_bar",
        "disable_print_progress_bar",
        "enable_checkpoint_on_shutdown",
        "disable_checkpoint_on_shutdown",
        "show_tables",
        "show_tables_expanded",
        "show_databases",
        "database_list",
        "collations",
        "version",
        "extension_versions",
        "platform",
        "database_size",
        "functions",
        "all_profiling_output",
        "user_agent",
    ];
    let calls = [
        PragmaFunction::call("table_info", vec![varchar()]),
        PragmaFunction::call("storage_info", vec![varchar()]),
        PragmaFunction::call("metadata_info", Vec::new()),
        PragmaFunction::call("show", vec![varchar()]),
        PragmaFunction::call("import_database", vec![varchar()]),
        PragmaFunction::call("copy_database", vec![varchar(), varchar()]),
    ];
    statements
        .into_iter()
        .map(PragmaFunction::statement)
        .chain(calls)
        .map(PragmaFunctionSet::single)
        .collect()
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
        "show_tables_expanded" => rustdb_planner::SHOW_TABLES_EXPANDED.to_owned(),
        "show_tables" => "with \"tables\" as (SELECT table_name as \"name\" FROM duckdb_tables WHERE in_search_path(database_name, schema_name)), \"views\" as (SELECT view_name as \"name\" FROM duckdb_views WHERE in_search_path(database_name, schema_name)), db_objects as (SELECT \"name\" FROM \"tables\" UNION ALL SELECT \"name\" FROM \"views\") SELECT \"name\" FROM db_objects ORDER BY \"name\";".to_owned(),
        "show_databases" => "SELECT database_name FROM duckdb_databases() WHERE NOT internal ORDER BY database_name;".to_owned(),
        "database_list" => "SELECT * FROM pragma_database_list;".to_owned(),
        "database_size" => "SELECT * FROM pragma_database_size();".to_owned(),
        "metadata_info" => "SELECT * FROM pragma_metadata_info();".to_owned(),
        "collations" => "SELECT * FROM pragma_collations() ORDER BY 1".to_owned(),
        "show" => format!("SELECT * FROM pragma_show({});", quoted(&first()?)),
        "json_execute_serialized_sql" => rustdb_json::execute_serialized_sql_pragma(&first()?)?,
        "version" => "SELECT * FROM pragma_version();".to_owned(),
        "platform" => "SELECT * FROM pragma_platform();".to_owned(),
        "extension_versions" => "select extension_name, extension_version, install_mode, installed_from from \
                                 duckdb_extensions() where installed"
            .to_owned(),
        "user_agent" => "SELECT * FROM pragma_user_agent()".to_owned(),
        "functions" => "SELECT function_name AS name, upper(function_type) AS type, parameter_types AS parameters, \
                        varargs, return_type, has_side_effects AS side_effects FROM duckdb_functions() WHERE \
                        function_type IN ('scalar', 'aggregate') ORDER BY 1;"
            .to_owned(),
        "copy_database" => {
            let second = parameters
                .get(1)
                .map(Value::to_varchar)
                .transpose()?
                .ok_or_else(|| Error::invalid_input("PRAGMA copy_database requires two arguments"))?;
            let copy = format!(
                "COPY FROM DATABASE {} TO {}",
                rustdb_ast::keyword::write_optionally_quoted(&first()?),
                rustdb_ast::keyword::write_optionally_quoted(&second)
            );
            format!("{copy} (SCHEMA);\n{copy} (DATA);")
        }
        _ => return Ok(None),
    }))
}
