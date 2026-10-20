//! `EXPORT DATABASE` (DuckDB's `bind_export.cpp`): a COPY of every table into a
//! directory, plus the `schema.sql` / `load.sql` the operator writes.

use std::collections::HashSet;
use std::sync::Arc;

use rustdb_ast::{
    AggregateHandling, CommonTableExpressionMap, Constraint, CopyInfo, ExportStatement, ParsedExpression,
    QueryNode, QueryNodeKind, SelectNode, TableRef, TableRefKind,
};
use rustdb_catalog::EntryKind;
use rustdb_common::{CaseInsensitiveMap, Error, ErrorKind, Result};
use rustdb_function::CopyOptionMode;
use rustdb_types::{LogicalType, Value};

use super::copy::{bind_copy_as, bind_copy_options, copy_function};
use crate::binder::{BindScope, BoundStatement, Scope};
use crate::generated::enums::LogicalOperatorType;
use crate::operator::{ExportedTable, LogicalExport, LogicalKind, LogicalOperator};
use crate::prepared::StatementReturnType;

/// `SanitizeExportIdentifier`: lower case letters, everything else an underscore.
fn sanitize_export_identifier(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'a'..='z' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '_',
        })
        .collect()
}

/// `CreateFileName`.
fn create_file_name(id_suffix: &str, schema: &str, table: &str, extension: &str) -> String {
    let name = sanitize_export_identifier(table);
    if schema == "main" {
        return format!("{name}{id_suffix}.{extension}");
    }
    format!(
        "{}_{name}{id_suffix}.{extension}",
        sanitize_export_identifier(schema)
    )
}

/// The `SELECT <physical columns> FROM table` of an exported table.
fn export_select(catalog: &str, schema: &str, table: &str, columns: &[String]) -> QueryNode {
    QueryNode {
        modifiers: Vec::new(),
        cte_map: CommonTableExpressionMap::default(),
        kind: QueryNodeKind::Select(Box::new(SelectNode {
            select_list: columns
                .iter()
                .map(|name| ParsedExpression::column_ref([name.clone()]))
                .collect(),
            from_table: Box::new(TableRef {
                alias: String::new(),
                sample: None,
                query_location: None,
                kind: TableRefKind::BaseTable(Box::new(rustdb_ast::BaseTableRef {
                    catalog_name: catalog.to_owned(),
                    schema_name: schema.to_owned(),
                    table_name: table.to_owned(),
                    column_name_alias: Vec::new(),
                    at_clause: None,
                })),
            }),
            where_clause: None,
            group_expressions: Vec::new(),
            group_sets: Vec::new(),
            aggregate_handling: AggregateHandling::StandardHandling,
            having: None,
            sample: None,
            qualify: None,
        })),
    }
}

/// `Binder::Bind(ExportStatement &)`.
#[allow(clippy::too_many_lines)]
pub fn bind_export(scope: &mut BindScope<'_>, stmt: &mut ExportStatement) -> Result<BoundStatement> {
    let info = &mut stmt.info;
    bind_copy_options(scope, info)?;
    let function = copy_function(scope, &info.format.clone(), false)?
        .ok_or_else(|| Error::internal("copy function lookup returned nothing"))?;
    if function.copy_to_bind.is_none() && function.plan.is_none() {
        return Err(Error::not_implemented(format!(
            "COPY TO is not supported for FORMAT \"{}\"",
            info.format
        )));
    }
    let catalog_name = if stmt.database.is_empty() {
        scope.context.default_catalog().to_owned()
    } else {
        stmt.database.clone()
    };
    let catalog = scope
        .context
        .catalog(&catalog_name)
        .cloned()
        .ok_or_else(|| Error::binder(format!("Catalog \"{catalog_name}\" does not exist!")))?;
    let transaction = Arc::clone(&scope.context.transaction);
    let mut tables = Vec::new();
    for schema in catalog.schemas().scan(&transaction) {
        let Some(schema_entry) = schema.as_schema() else {
            continue;
        };
        for entry in schema_entry.tables.scan(&transaction) {
            if matches!(entry.kind, EntryKind::Table(_)) {
                tables.push((schema.name().to_owned(), entry));
            }
        }
    }
    let tables = rustdb_catalog::checkpoint::reorder_table_entries(tables);

    let mut exported = Vec::new();
    let mut taken_names: HashSet<String> = HashSet::new();
    let mut export_nodes = Vec::new();
    for (schema, entry) in &tables {
        let EntryKind::Table(table) = &entry.kind else {
            continue;
        };
        let mut id = 0;
        let file_path = loop {
            let suffix = if id == 0 { String::new() } else { format!("_{id}") };
            let name = create_file_name(&suffix, schema, entry.name(), &function.extension);
            let separator = if info.file_path.ends_with('/') { "" } else { "/" };
            let full_path = format!("{}{separator}{name}", info.file_path);
            if taken_names.insert(full_path.clone()) {
                break full_path;
            }
            id += 1;
        };
        let not_null_columns: Vec<String> = table
            .constraints
            .iter()
            .filter_map(|constraint| match constraint {
                Constraint::NotNull(not_null) => table
                    .columns
                    .get(not_null.index.0 as usize)
                    .map(|column| column.name.clone()),
                _ => None,
            })
            .collect();
        let columns: Vec<String> = table
            .physical_columns()
            .map(|(_, _, column)| column.name.clone())
            .collect();
        exported.push(ExportedTable {
            schema_name: schema.clone(),
            table_name: entry.name().to_owned(),
            file_path: file_path.clone(),
            not_null_columns,
        });
        let mut copy_info = CopyInfo {
            catalog: catalog_name.clone(),
            schema: schema.clone(),
            table: entry.name().to_owned(),
            select_list: Vec::new(),
            is_from: false,
            format: info.format.clone(),
            file_path,
            options: info.options.clone(),
            select_statement: Some(Box::new(export_select(
                &catalog_name,
                schema,
                entry.name(),
                &columns,
            ))),
            is_format_auto_detected: false,
            parsed_options: CaseInsensitiveMap::default(),
            file_path_expression: None,
        };
        let mut child = scope.binder.child()?;
        let bound = {
            let mut child_scope = BindScope {
                binder: &mut child,
                outer: Some(scope as &mut dyn Scope),
            };
            bind_copy_as(
                &mut child_scope,
                &mut copy_info,
                rustdb_function::copy::CopyToType::ExportDatabase,
            )?
        };
        export_nodes.push(bound.plan);
    }
    // Union the copies pairwise (the executor's UNION has two inputs).
    let mut child_operator: Option<LogicalOperator> = None;
    for node in export_nodes {
        child_operator = Some(match child_operator {
            None => node,
            Some(left) => LogicalOperator::new(LogicalKind::SetOperation {
                operator_type: LogicalOperatorType::LogicalUnion,
                table_index: scope.generate_table_index(),
                column_count: 1,
                setop_all: true,
                allow_out_of_order: false,
            })
            .with_children(vec![left, node]),
        });
    }
    // `FileSystem::CreateDirectory` through the client's opener file system.
    rustdb_function::file_system::verify_can_access_directory(&info.file_path)?;
    let directory = std::path::Path::new(&info.file_path);
    if !directory.is_dir() {
        std::fs::create_dir_all(directory).map_err(|error| {
            Error::new(
                ErrorKind::Io,
                format!("Failed to create directory \"{}\": {error}", info.file_path),
            )
        })?;
    }
    info.catalog.clone_from(&catalog_name);
    if info.format == "csv" {
        if info.options.get("header").is_none() {
            info.options.insert("header".to_owned(), vec![Value::integer(1)]);
        }
        if info.options.get("delimiter").is_none()
            && info.options.get("sep").is_none()
            && info.options.get("delim").is_none()
        {
            info.options
                .insert("delimiter".to_owned(), vec![Value::varchar(",")]);
        }
        if info.options.get("quote").is_none() {
            info.options
                .insert("quote".to_owned(), vec![Value::varchar("\"")]);
        }
        info.options.remove("force_quote");
    }
    // Only options for reading go into the COPY statements of `load.sql`.
    let readable: Vec<String> = function
        .options
        .iter()
        .filter(|(_, option)| option.mode != CopyOptionMode::WriteOnly)
        .map(|(name, _)| name.clone())
        .collect();
    let erased: Vec<String> = info
        .options
        .iter()
        .filter(|(name, _)| !readable.iter().any(|option| option.eq_ignore_ascii_case(name)))
        .map(|(name, _)| name.to_owned())
        .collect();
    for name in erased {
        info.options.remove(&name);
    }
    let export = LogicalExport {
        function,
        file_path: info.file_path.clone(),
        format: info.format.clone(),
        options: info
            .options
            .iter()
            .map(|(name, values)| (name.to_owned(), values.clone()))
            .collect(),
        catalog: catalog_name,
        tables: exported,
    };
    let mut plan = LogicalOperator::new(LogicalKind::Export(Box::new(export)));
    if let Some(child) = child_operator {
        plan = plan.with_children(vec![child]);
    }
    scope.with_properties(|properties| properties.return_type = StatementReturnType::Nothing);
    Ok(BoundStatement::new(
        plan,
        vec!["Success".to_owned()],
        vec![LogicalType::BOOLEAN],
    ))
}
