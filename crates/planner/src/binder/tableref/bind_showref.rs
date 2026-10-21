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
        LogicalKind::Get(get) => {
            if get.table_index != binding.table_index {
                return None;
            }
            let table = get.table.as_ref()?;
            let EntryKind::Table(entry) = &table.kind else {
                return None;
            };
            let column = get.column_ids.get(usize::try_from(binding.column_index).ok()?)?;
            // Virtual columns (like `rowid`) have no column definition.
            let index = usize::try_from(column.index).ok()?;
            (index < entry.columns.len()).then(|| (Arc::clone(table), index))
        }
        _ => None,
    }
}

/// `PragmaTableInfo::GetColumnInfo` in the form of `pragma_show`: name, type, nullability,
/// key (`PRI` / `UNI`), default and extra of a table column.
fn table_column_info(table: &rustdb_catalog::TableEntry, column: usize) -> Result<[Value; 6]> {
    let definition = table
        .columns
        .get(column)
        .ok_or_else(|| Error::internal("traced column out of range"))?;
    // `ColumnConstraintInfo`: the UNIQUE / PRIMARY KEY constraints on the column.
    let (mut pk, mut unique) = (false, false);
    for constraint in &table.constraints {
        let rustdb_ast::Constraint::Unique(constraint) = constraint else {
            continue;
        };
        let covers = if constraint.index.0 == u64::MAX {
            constraint
                .columns
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&definition.name))
        } else {
            constraint.index.0 == column as u64
        };
        if covers {
            if constraint.is_primary_key {
                pk = true;
            } else {
                unique = true;
            }
        }
    }
    let null = Value::null(LogicalType::VARCHAR);
    let key = if pk {
        Value::varchar("PRI")
    } else if unique {
        Value::varchar("UNI")
    } else {
        null.clone()
    };
    // `DefaultValue`: the generated expression or the default.
    let default = definition
        .generated
        .as_ref()
        .or(definition.default_value.as_ref())
        .map_or_else(
            || null.clone(),
            |expression| Value::varchar(expression.to_string()),
        );
    Ok([
        Value::varchar(definition.name.clone()),
        Value::varchar(definition.logical_type.to_string()),
        Value::varchar(if table.not_null_columns.contains(&column) {
            "NO"
        } else {
            "YES"
        }),
        key,
        default,
        null,
    ])
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
                let mut row = table_column_info(table, column)?;
                // The alias replaces the base column's name.
                row[0] = Value::varchar(alias.clone());
                row
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
                    // The catalog of the default search path entry, which is empty
                    // without a `USE` or `SET search_path`.
                    catalog = scope
                        .context
                        .search_path
                        .first()
                        .map(|(catalog, _)| catalog.clone())
                        .unwrap_or_default();
                }
                let lookup = if catalog.is_empty() {
                    scope.context.default_catalog()
                } else {
                    catalog.as_str()
                };
                let exists = scope
                    .context
                    .catalog(lookup)
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
        // `PragmaShowTablesExpanded`.
        "__show_tables_expanded" => "\n\tSELECT\n\t\tt.database_name AS database,\n\t\tt.schema_name AS schema,\n\t\tt.table_name AS \
                                     name,\n\t\tLIST(c.column_name order by c.column_index) AS column_names,\n\t\tLIST(c.data_type \
                                     order by c.column_index) AS column_types,\n\t\tFIRST(t.temporary) AS temporary,\n\tFROM \
                                     duckdb_tables t\n\tJOIN duckdb_columns c\n\tUSING (table_oid)\n\tGROUP BY database, schema, \
                                     name\n\n\tUNION ALL\n\n\tSELECT\n\t\tv.database_name AS database,\n\t\tv.schema_name AS \
                                     schema,\n\t\tv.view_name AS name,\n\t\tLIST(c.column_name order by c.column_index) AS \
                                     column_names,\n\t\tLIST(c.data_type order by c.column_index) AS column_types,\n\t\t\
                                     FIRST(v.temporary) AS temporary,\n\tFROM duckdb_views v\n\tJOIN duckdb_columns c\n\tON \
                                     (v.view_oid=c.table_oid)\n\tGROUP BY database, schema, name\n\n\tORDER BY database, \
                                     schema, name\n\t"
            .to_owned(),
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

/// `SummarizeWrapUnnest`: `unnest(list_value(children...)) AS alias`.
fn summarize_unnest(
    children: Vec<rustdb_ast::ParsedExpression>,
    alias: &str,
) -> rustdb_ast::ParsedExpression {
    use crate::expression_binder::function_call;
    let list = function_call("list_value", children);
    function_call("unnest", vec![list]).with_alias(alias)
}

fn summarize_cast(child: rustdb_ast::ParsedExpression, ty: LogicalType) -> rustdb_ast::ParsedExpression {
    rustdb_ast::ParsedExpression::new(
        rustdb_ast::ExpressionType::OperatorCast,
        rustdb_ast::ParsedExpressionKind::Cast(rustdb_ast::CastExpression {
            child: Box::new(child),
            cast_type: ty,
            try_cast: false,
        }),
    )
}

/// `SummarizeCreateAggregate`: `CAST(aggregate(column[, modifier]) AS VARCHAR)`.
fn summarize_aggregate(
    aggregate: &str,
    column: &str,
    modifier: Option<Value>,
) -> rustdb_ast::ParsedExpression {
    let mut children = vec![rustdb_ast::ParsedExpression::column_ref([column])];
    if let Some(modifier) = modifier {
        children.push(rustdb_ast::ParsedExpression::constant(modifier));
    }
    summarize_cast(
        crate::expression_binder::function_call(aggregate, children),
        LogicalType::VARCHAR,
    )
}

fn summarize_count_star() -> rustdb_ast::ParsedExpression {
    crate::expression_binder::function_call("count_star", Vec::new())
}

/// `SummarizeCreateNullPercentage`: `CAST(CASE WHEN count_star() > 0 THEN (1 - count(x) /
/// count_star()) * 100 ELSE NULL END AS DECIMAL(9, 2))` on DOUBLEs.
fn summarize_null_percentage(column: &str) -> rustdb_ast::ParsedExpression {
    use crate::expression_binder::function_call;
    use rustdb_ast::{ExpressionType, ParsedExpression, ParsedExpressionKind};
    let count_star = summarize_cast(summarize_count_star(), LogicalType::DOUBLE);
    let count = summarize_cast(summarize_aggregate("count", column, None), LogicalType::DOUBLE);
    let ratio = function_call("/", vec![count, count_star]);
    let negated = function_call("-", vec![ParsedExpression::constant(Value::double(1.0)), ratio]);
    let percentage = function_call(
        "*",
        vec![negated, ParsedExpression::constant(Value::double(100.0))],
    );
    let comparison = ParsedExpression::new(
        ExpressionType::CompareGreaterthan,
        ParsedExpressionKind::Comparison(rustdb_ast::ComparisonExpression {
            left: Box::new(summarize_count_star()),
            right: Box::new(ParsedExpression::constant(Value::bigint(0))),
        }),
    );
    let case = ParsedExpression::new(
        ExpressionType::CaseExpr,
        ParsedExpressionKind::Case(rustdb_ast::CaseExpression {
            case_checks: vec![rustdb_ast::CaseCheck {
                when_expr: Box::new(comparison),
                then_expr: Box::new(percentage),
            }],
            else_expr: Box::new(ParsedExpression::constant(Value::null(LogicalType::SQLNULL))),
        }),
    );
    summarize_cast(case, LogicalType::decimal(9, 2))
}

/// `LogicalType::IsTemporal`.
fn is_temporal(ty: &LogicalType) -> bool {
    use rustdb_types::LogicalTypeId as L;
    matches!(
        ty.id(),
        L::Date
            | L::Time
            | L::TimeNs
            | L::Timestamp
            | L::TimeTz
            | L::TimestampTz
            | L::TimestampSec
            | L::TimestampMs
            | L::TimestampNs
    )
}

/// `SUMMARIZE query` (DuckDB's `Binder::BindSummarize`): the query is bound once for its
/// columns, then summarized by one aggregate query whose lists are unnested into a row
/// per column.
fn bind_summarize(scope: &mut BindScope<'_>, show: &ShowRef) -> Result<BoundStatement> {
    use rustdb_ast::{ParsedExpression, TableRef, TableRefKind};
    let query = show
        .query
        .as_deref()
        .cloned()
        .ok_or_else(|| Error::internal("SUMMARIZE without a query"))?;
    let (names, types) = {
        let mut binder = scope.binder.child()?;
        let mut child_scope = BindScope {
            binder: &mut binder,
            outer: Some(&mut *scope),
        };
        let mut copy = query.clone();
        let bound = bind_node(&mut child_scope, &mut copy)?;
        (bound.names, bound.types)
    };
    let mut lists: [Vec<ParsedExpression>; 12] = Default::default();
    for (name, ty) in names.iter().zip(&types) {
        let null = || ParsedExpression::constant(Value::null(LogicalType::SQLNULL));
        let numeric_or_temporal = ty.is_numeric() || is_temporal(ty);
        lists[0].push(ParsedExpression::constant(Value::varchar(name)));
        lists[1].push(ParsedExpression::constant(Value::varchar(ty.to_string())));
        lists[2].push(summarize_aggregate("min", name, None));
        lists[3].push(summarize_aggregate("max", name, None));
        lists[4].push(summarize_cast(
            summarize_aggregate("approx_count_distinct", name, None),
            LogicalType::BIGINT,
        ));
        lists[5].push(if numeric_or_temporal {
            summarize_aggregate("avg", name, None)
        } else {
            null()
        });
        lists[6].push(if ty.is_numeric() {
            summarize_aggregate("stddev", name, None)
        } else {
            null()
        });
        for (list, fraction) in [(7, 0.25f32), (8, 0.5), (9, 0.75)] {
            lists[list].push(if numeric_or_temporal {
                summarize_aggregate("approx_quantile", name, Some(Value::float(fraction)))
            } else {
                null()
            });
        }
        lists[10].push(summarize_count_star());
        lists[11].push(summarize_null_percentage(name));
    }
    let aliases = [
        "column_name",
        "column_type",
        "min",
        "max",
        "approx_unique",
        "avg",
        "std",
        "q25",
        "q50",
        "q75",
        "count",
        "null_percentage",
    ];
    let select_list = lists
        .into_iter()
        .zip(aliases)
        .map(|(children, alias)| summarize_unnest(children, alias))
        .collect();
    let from_table = TableRef {
        alias: "summarize_tbl".to_owned(),
        sample: None,
        query_location: None,
        kind: TableRefKind::Subquery(SubqueryRef {
            subquery: Box::new(rustdb_ast::SelectStatement {
                node: Box::new(query),
                named_param_map: rustdb_common::CaseInsensitiveMap::default(),
            }),
            column_name_alias: names,
        }),
    };
    let node = QueryNode {
        modifiers: Vec::new(),
        cte_map: rustdb_ast::CommonTableExpressionMap::default(),
        kind: rustdb_ast::QueryNodeKind::Select(Box::new(rustdb_ast::SelectNode {
            select_list,
            from_table: Box::new(from_table),
            where_clause: None,
            group_expressions: Vec::new(),
            group_sets: Vec::new(),
            aggregate_handling: rustdb_ast::AggregateHandling::StandardHandling,
            having: None,
            sample: None,
            qualify: None,
        })),
    };
    let mut subquery = SubqueryRef {
        subquery: Box::new(rustdb_ast::SelectStatement {
            node: Box::new(node),
            named_param_map: rustdb_common::CaseInsensitiveMap::default(),
        }),
        column_name_alias: Vec::new(),
    };
    bind_subquery_ref(scope, &mut subquery, "")
}

/// DuckDB's `Binder::Bind(ShowRef &)`.
pub fn bind_show(scope: &mut BindScope<'_>, show: &mut ShowRef) -> Result<BoundStatement> {
    if show.show_type == ShowType::Summary {
        return bind_summarize(scope, show);
    }
    match &mut show.query {
        Some(query) => bind_show_query(scope, query),
        None => bind_show_table(scope, show),
    }
}
