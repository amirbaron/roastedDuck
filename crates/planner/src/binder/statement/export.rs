//! `EXPORT DATABASE` (DuckDB's `bind_export.cpp`): a COPY of every table into a
//! directory, plus the `schema.sql` / `load.sql` the operator writes.

use std::collections::HashSet;
use std::sync::Arc;

use roastedduck_ast::{
    AggregateHandling, CommonTableExpressionMap, Constraint, CopyInfo, ExportStatement, ParsedExpression,
    QueryNode, QueryNodeKind, SelectNode, TableRef, TableRefKind,
};
use roastedduck_catalog::EntryKind;
use roastedduck_common::{ByteString, CaseInsensitiveMap, Error, ErrorKind, Result};
use roastedduck_function::CopyOptionMode;
use roastedduck_types::{LogicalType, Value};

use super::copy::{bind_copy_as, bind_copy_options, copy_function};
use crate::binder::{BindScope, BoundStatement, Scope};
use crate::generated::enums::LogicalOperatorType;
use crate::operator::{ExportedTable, LogicalExport, LogicalKind, LogicalOperator};
use crate::prepared::StatementReturnType;

/// `SanitizeExportIdentifier`: lower case letters, everything else an underscore.
fn sanitize_export_identifier(text: &[u8]) -> String {
    text.iter()
        .map(|&c| match c {
            b'a'..=b'z' => char::from(c),
            b'A'..=b'Z' => char::from(c.to_ascii_lowercase()),
            _ => '_',
        })
        .collect()
}

/// `CreateFileName`.
fn create_file_name(id_suffix: &str, schema: &[u8], table: &[u8], extension: &str) -> ByteString {
    let name = sanitize_export_identifier(table);
    if schema == b"main" {
        return roastedduck_common::byte_format!("{name}{id_suffix}.{extension}");
    }
    roastedduck_common::byte_format!(
        "{}_{name}{id_suffix}.{extension}",
        sanitize_export_identifier(schema)
    )
}

/// The `SELECT <physical columns> FROM table` of an exported table.
fn export_select(
    catalog: &[u8],
    schema: &[u8],
    table: &[u8],
    columns: &[roastedduck_common::ByteString],
) -> QueryNode {
    QueryNode {
        modifiers: Vec::new(),
        cte_map: CommonTableExpressionMap::default(),
        kind: QueryNodeKind::Select(Box::new(SelectNode {
            select_list: columns
                .iter()
                .map(|name| ParsedExpression::column_ref([name.clone()]))
                .collect(),
            from_table: Box::new(TableRef {
                external_dependency: roastedduck_ast::ExternalDependency::default(),
                alias: roastedduck_common::ByteString::new(),
                sample: None,
                query_location: None,
                kind: TableRefKind::BaseTable(Box::new(roastedduck_ast::BaseTableRef {
                    catalog_name: catalog.into(),
                    schema_name: schema.into(),
                    table_name: table.into(),
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
        return Err(Error::not_implemented(roastedduck_common::byte_format!(
            "COPY TO is not supported for FORMAT \"{}\"",
            info.format
        )));
    }
    let catalog_name = if stmt.database.is_empty() {
        scope.context.default_catalog()
    } else {
        stmt.database.clone()
    };
    // Keep the requested identity in COPY/bound export metadata. An empty
    // catalog means the default catalog; resolving it must not rewrite the plan.
    let requested_catalog = stmt.database.clone();
    let catalog = scope.context.catalog(&catalog_name).cloned().ok_or_else(|| {
        Error::binder(roastedduck_common::byte_format!(
            "Catalog \"{catalog_name}\" does not exist!"
        ))
    })?;
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
    let tables = roastedduck_catalog::checkpoint::reorder_table_entries(tables);

    let mut exported = Vec::new();
    let mut taken_names: HashSet<ByteString> = HashSet::new();
    let mut export_nodes = Vec::new();
    for (schema, entry) in &tables {
        let EntryKind::Table(table) = &entry.kind else {
            continue;
        };
        let mut id = 0;
        let file_path = loop {
            let suffix = if id == 0 { String::new() } else { format!("_{id}") };
            let name = create_file_name(&suffix, schema, entry.name(), &function.extension);
            let separator = if info.file_path.ends_with(b"/") { "" } else { "/" };
            let full_path = roastedduck_common::byte_format!("{}{separator}{name}", info.file_path);
            if taken_names.insert(full_path.clone()) {
                break full_path;
            }
            id += 1;
        };
        let not_null_columns: Vec<roastedduck_common::ByteString> = table
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
        let columns: Vec<roastedduck_common::ByteString> = table
            .physical_columns()
            .map(|(_, _, column)| column.name.clone())
            .collect();
        exported.push(ExportedTable {
            database_name: requested_catalog.clone(),
            schema_name: schema.clone(),
            table_name: entry.name().to_owned(),
            file_path: file_path.clone(),
            not_null_columns,
        });
        let mut copy_info = CopyInfo {
            catalog: requested_catalog.clone(),
            schema: schema.clone(),
            table: entry.name().to_owned(),
            select_list: Vec::new(),
            is_from: false,
            format: info.format.clone(),
            file_path,
            options: info.options.clone(),
            select_statement: Some(Box::new(export_select(
                &requested_catalog,
                schema,
                entry.name(),
                &columns,
            ))),
            is_format_auto_detected: true,
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
                roastedduck_function::copy::CopyToType::ExportDatabase,
            )?
        };
        export_nodes.push(bound.plan);
    }
    // Binder::UnionOperators keeps all COPY children in one logical UNION.
    // Physical planning lowers that logical shape to the executor's binary unions.
    let child_operator = match export_nodes.len() {
        0 => None,
        1 => export_nodes.pop(),
        _ => Some(
            LogicalOperator::new(LogicalKind::SetOperation {
                operator_type: LogicalOperatorType::LogicalUnion,
                table_index: scope.generate_table_index(),
                column_count: 1,
                setop_all: true,
                allow_out_of_order: false,
            })
            .with_children(export_nodes),
        ),
    };
    // `FileSystem::CreateDirectory` through the client's opener file system.
    roastedduck_function::file_system::verify_can_access_directory(&info.file_path)?;
    if let Some(fs) = roastedduck_function::registered_files::lookup(&info.file_path) {
        if !fs.is_dir(&info.file_path)? {
            fs.mkdir(&info.file_path)?;
        }
    } else {
        let directory = roastedduck_function::file_system::native_path(&info.file_path)?;
        if !directory.is_dir() {
            std::fs::create_dir_all(&directory).map_err(|error| {
                Error::new_bytes(
                    ErrorKind::Io,
                    roastedduck_common::byte_format!(
                        "Failed to create directory \"{}\": {error}",
                        info.file_path
                    ),
                )
            })?;
        }
    }
    info.catalog.clone_from(&requested_catalog);
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
    }
    // BindCopyOptions inserts evaluated values into C++'s case-insensitive
    // unordered map, then EXPORT inserts CSV defaults. Retain that real map's
    // iteration order, including its bucket growth before write-only erasure.
    let mut option_order = roastedduck_common::libcxx::UnorderedOrder::default();
    for (name, _) in info.options.iter() {
        option_order.insert(roastedduck_common::libcxx::ci_hash(name), name.clone(), |a, b| {
            a.eq_ignore_ascii_case(b)
        });
    }
    if info.format == "csv" {
        info.options.remove("force_quote");
    }
    // Only options for reading go into the COPY statements of `load.sql`.
    let readable: Vec<String> = function
        .options
        .iter()
        .filter(|(_, option)| option.mode != CopyOptionMode::WriteOnly)
        .map(|(name, _)| name.clone())
        .collect();
    let erased: Vec<ByteString> = info
        .options
        .iter()
        .filter(|(name, _)| {
            !readable
                .iter()
                .any(|option| option.as_bytes().eq_ignore_ascii_case(name))
        })
        .map(|(name, _)| name.to_owned())
        .collect();
    for name in erased {
        info.options.remove(&name);
    }
    let mut ordered_options = roastedduck_common::CaseInsensitiveMap::default();
    for index in option_order.order() {
        let name = &option_order.keys()[index];
        if let Some(values) = info.options.remove(name) {
            ordered_options.push(name.clone(), values);
        }
    }
    info.options = ordered_options;
    let export = LogicalExport {
        copy_info: info.as_ref().clone(),
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
        vec!["Success".into()],
        vec![LogicalType::BOOLEAN],
    ))
}
