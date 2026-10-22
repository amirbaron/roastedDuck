//! `COPY ... TO` / `COPY ... FROM` (DuckDB's `bind_copy.cpp`).

use rustdb_ast::{
    AggregateHandling, CommonTableExpressionMap, CopyInfo, ExpressionType, InsertColumnOrder,
    InsertStatement, ParsedExpression, ParsedExpressionKind, QueryNode, QueryNodeKind, SelectNode,
    StarExpression, TableRef, TableRefKind,
};
use rustdb_catalog::{CatalogType, EntryKind};
use rustdb_common::string_util::candidates_message;
use rustdb_common::{CaseInsensitiveMap, Error, ErrorKind, Result};
use rustdb_function::cast::{ValueCast, implicit_cast_cost};
use rustdb_function::copy::{CopyFromBindInput, CopyFunctionBindInput, CopyToSelectInput, CopyToType};
use rustdb_function::{CopyFunction, CopyOption, CopyOptionMode};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use super::modify::bind_insert;
use crate::binder::BindScope;
use crate::binder::BoundStatement;
use crate::binder::query_node::bind_node;
use crate::binder::tableref::deduplicate_columns;
use crate::expression_binder::{ExpressionBinder, TableFunctionBinder, parameter_not_resolved};
use crate::operator::{
    ColumnIndex, CopyFunctionReturnType, CopyOverwriteMode, FilenamePattern, LogicalCopyToFile, LogicalGet,
    LogicalKind, LogicalOperator,
};
use crate::prepared::StatementReturnType;

/// `GetBooleanArg`.
fn boolean_arg(scope: &BindScope<'_>, values: &[Value]) -> Result<bool> {
    let Some(value) = values.first() else {
        return Ok(true);
    };
    let value = value.try_cast_as(&scope.context.casts, &LogicalType::BOOLEAN, false)?;
    Ok(value.as_bool().unwrap_or(false))
}

/// `BindCopyOption`: evaluates one option to its values.
fn bind_copy_option(
    scope: &mut BindScope<'_>,
    name: &str,
    expr: Option<&ParsedExpression>,
) -> Result<Vec<Value>> {
    let Some(expr) = expr else {
        return Ok(Vec::new());
    };
    if expr.expression_type == ExpressionType::Star
        && let ParsedExpressionKind::Star(star) = &expr.kind
        && star.relation_name.is_empty()
        && star.exclude_list.is_empty()
        && star.replace_list.is_empty()
        && star.rename_list.is_empty()
        && star.expr.is_none()
        && !star.columns
    {
        return Ok(vec![Value::varchar("*")]);
    }
    let is_partition_by = name.eq_ignore_ascii_case("partition_by");
    let mut expr = expr.clone();
    let bound = {
        let (inner, outer) = scope.split();
        let mut binder = TableFunctionBinder::new(inner, outer, "Copy", "Copy options");
        if is_partition_by {
            binder.accept_sql_value_functions = false;
        }
        binder.bind(&mut expr, true)?
    };
    if bound.has_parameter() {
        return Err(parameter_not_resolved());
    }
    let value = scope.context.evaluator.evaluate_scalar(&bound)?;
    if value.is_null() {
        return Err(Error::binder(format!(
            "NULL is not supported as a valid option for COPY option \"{name}\""
        )));
    }
    let ty = value.logical_type();
    if ty.id() == LogicalTypeId::Struct
        && ty
            .struct_children()
            .is_some_and(|children| children.iter().all(|(name, _)| name.is_empty()))
    {
        return Ok(value.children().unwrap_or_default().to_vec());
    }
    Ok(vec![value])
}

/// `ExtractFormat`: the format implied by the file extension.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the path is lowercased"
)]
fn extract_format(file_path: &str) -> String {
    let mut format = file_path.to_lowercase();
    if format.ends_with(".gz") {
        format.truncate(format.len() - 3);
    } else if format.ends_with(".zst") {
        format.truncate(format.len() - 4);
    }
    match format.rfind('.') {
        Some(dot) if dot + 1 < format.len() => format[dot + 1..].to_owned(),
        _ => String::new(),
    }
}

/// `BindCopyOptions`: evaluates the file name and the options.
pub(super) fn bind_copy_options(scope: &mut BindScope<'_>, info: &mut CopyInfo) -> Result<()> {
    if let Some(expr) = info.file_path_expression.take() {
        let inputs = bind_copy_option(scope, "filename", Some(&expr))?;
        if inputs.len() != 1 || inputs[0].logical_type().id() != LogicalTypeId::Varchar {
            return Err(Error::internal(
                "Unsupported parameter type for filename: expected e.g. TARGET 'file.parquet'",
            ));
        }
        if !info.file_path.is_empty() {
            return Err(Error::internal(
                "Both a file path and a file path expression were provided for COPY - only one of the two can be provided",
            ));
        }
        info.file_path = inputs[0].to_varchar()?;
    }
    let parsed = std::mem::take(&mut info.parsed_options);
    for (name, expr) in parsed.iter() {
        let inputs = bind_copy_option(scope, name, expr.as_ref())?;
        if name.eq_ignore_ascii_case("format") {
            if inputs.len() != 1 || inputs[0].logical_type().id() != LogicalTypeId::Varchar {
                return Err(Error::parser(
                    "Unsupported parameter type for FORMAT: expected e.g. FORMAT 'csv', 'parquet'",
                ));
            }
            info.format = inputs[0].to_varchar()?.to_lowercase();
            info.is_format_auto_detected = false;
            continue;
        }
        info.options.insert(name.to_owned(), inputs);
    }
    if info.is_format_auto_detected && info.format.is_empty() {
        info.format = extract_format(&info.file_path);
    }
    Ok(())
}

/// `IsFormatExtensionKnown`.
fn format_extension_error(format: &str) -> Option<Error> {
    const POSTFIXES: [(&str, &str); 9] = [
        ("parquet", "parquet"),
        ("json", "json"),
        ("jsonl", "json"),
        ("ndjson", "json"),
        ("shp", "spatial"),
        ("gpkg", "spatial"),
        ("fgb", "spatial"),
        ("xlsx", "excel"),
        ("avro", "avro"),
    ];
    POSTFIXES.iter().find(|(name, _)| *name == format).map(|(name, extension)| {
        Error::catalog(format!(
            "Copy Function with name \"{name}\" is not in the catalog, but it exists in the {extension} extension."
        ))
    })
}

/// Looks up a COPY format in the system catalog.
pub(super) fn copy_function(
    scope: &BindScope<'_>,
    format: &str,
    if_exists: bool,
) -> Result<Option<CopyFunction>> {
    let found = scope
        .context
        .get_entry("system", "main", CatalogType::CopyFunction, format, if_exists)?;
    Ok(found.and_then(|found| match &found.entry.kind {
        EntryKind::CopyFunction(function) => Some((**function).clone()),
        _ => None,
    }))
}

/// `GetFullCopyOptionsList`: the format's options with the generic write options.
fn full_copy_options_list(function: &CopyFunction) -> CaseInsensitiveMap<CopyOption> {
    let mut options = CaseInsensitiveMap::default();
    for (name, option) in &function.options {
        options.insert(name.clone(), option.clone());
    }
    let write = |ty: LogicalType| CopyOption::new(ty, CopyOptionMode::WriteOnly);
    for (name, ty) in [
        ("use_tmp_file", LogicalType::BOOLEAN),
        ("overwrite_or_ignore", LogicalType::BOOLEAN),
        ("overwrite", LogicalType::BOOLEAN),
        ("append", LogicalType::BOOLEAN),
        ("filename_pattern", LogicalType::VARCHAR),
        ("file_extension", LogicalType::VARCHAR),
        ("per_thread_output", LogicalType::BOOLEAN),
        ("file_size_bytes", LogicalType::ANY),
        ("partition_by", LogicalType::ANY),
        ("return_files", LogicalType::BOOLEAN),
        ("preserve_order", LogicalType::BOOLEAN),
        ("return_stats", LogicalType::BOOLEAN),
        ("write_partition_columns", LogicalType::BOOLEAN),
        ("write_empty_file", LogicalType::BOOLEAN),
        ("hive_file_pattern", LogicalType::BOOLEAN),
    ] {
        options.insert(name.to_owned(), write(ty));
    }
    options
}

/// Checks the options against the format's list and casts them to the option types.
fn check_copy_options(scope: &BindScope<'_>, function: &CopyFunction, info: &mut CopyInfo) -> Result<()> {
    let copy_mode = if info.is_from {
        CopyOptionMode::ReadOnly
    } else {
        CopyOptionMode::WriteOnly
    };
    let copy_options = full_copy_options_list(function);
    let mut options = std::mem::take(&mut info.options);
    for (provided_option, values) in options.iter_mut() {
        let Some(copy_option) = copy_options.get(provided_option) else {
            let candidates: Vec<String> = copy_options.iter().map(|(name, _)| name.to_owned()).collect();
            let candidate_str = candidates_message(
                &rustdb_common::string_util::top_n_jaro_winkler(&candidates, provided_option),
                "Candidate options",
            );
            return Err(Error::not_implemented(format!(
                "Unrecognized option \"{provided_option}\" for {}\n{candidate_str}",
                info.format
            )));
        };
        if copy_option.mode != CopyOptionMode::ReadWrite && copy_option.mode != copy_mode {
            let (doing, only) = if info.is_from {
                ("reading", "writing")
            } else {
                ("writing", "reading")
            };
            return Err(Error::invalid_input(format!(
                "Option \"{provided_option}\" is not supported for {doing} - only for {only}"
            )));
        }
        if copy_option.ty.id() == LogicalTypeId::Any {
            continue;
        }
        if values.is_empty() {
            if copy_option.ty.id() == LogicalTypeId::Boolean {
                continue;
            }
            return Err(Error::invalid_input(format!(
                "Copy option \"{provided_option}\" requires an argument of type {}",
                copy_option.ty
            )));
        }
        if values.len() > 1 {
            return Err(Error::invalid_input(format!(
                "Copy option \"{provided_option}\" did not expect a list as argument"
            )));
        }
        let original = &values[0];
        if *original.logical_type() == copy_option.ty {
            continue;
        }
        let original_type = original.logical_type().clone();
        let mut can_cast = implicit_cast_cost(&original_type, &copy_option.ty) >= 0;
        if !can_cast {
            if copy_option.ty.is_numeric() {
                can_cast = original_type.is_numeric();
            } else if copy_option.ty.id() == LogicalTypeId::Boolean {
                can_cast = original_type.is_integral();
            }
            if original_type.id() == LogicalTypeId::Varchar {
                can_cast = true;
            }
        }
        let cast = if can_cast {
            original
                .try_cast_as(&scope.context.casts, &copy_option.ty, false)
                .ok()
        } else {
            None
        };
        let Some(new_value) = cast else {
            return Err(Error::invalid_input(format!(
                "Copy option \"{provided_option}\" expected an argument of type {} - the argument \"{}\" of type {original_type} could not be cast as this type",
                copy_option.ty,
                original.to_varchar().unwrap_or_default()
            )));
        };
        values[0] = new_value;
    }
    info.options = options;
    Ok(())
}

/// `ParseColumnsOrdered`: the positions of the named columns, in the given order.
fn parse_columns_ordered(values: &[Value], names: &[String], option: &str) -> Result<Vec<usize>> {
    if values.len() == 1 && values[0].as_str() == Some("*") {
        return Ok((0..names.len()).collect());
    }
    if values.is_empty() {
        return Err(Error::binder(format!(
            "\"{option}\" expects a column list or * as parameter"
        )));
    }
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        let text = value.to_varchar()?;
        let Some(index) = names.iter().position(|name| name.eq_ignore_ascii_case(&text)) else {
            return Err(Error::binder(format!(
                "\"{option}\" expected to find {text}, but it was not found in the table"
            )));
        };
        result.push(index);
    }
    Ok(result)
}

/// A memory size like `'1MB'` (DuckDB's `DBConfig::ParseMemoryLimit`).
fn parse_memory_limit(text: &str) -> Result<u64> {
    let text = text.trim().to_lowercase();
    let split = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| Error::parser(format!("Unable to parse memory limit \"{text}\"")))?;
    let multiplier: f64 = match unit.trim() {
        "" | "b" | "byte" | "bytes" => 1.0,
        "kb" | "kilobyte" | "kilobytes" => 1000.0,
        "mb" | "megabyte" | "megabytes" => 1e6,
        "gb" | "gigabyte" | "gigabytes" => 1e9,
        "tb" | "terabyte" | "terabytes" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => {
            return Err(Error::parser(format!(
                "Unknown unit for memory limit: {}",
                unit.trim()
            )));
        }
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok((number * multiplier) as u64)
}

/// `SELECT * FROM table` (or the listed columns) for `COPY table TO ...`.
fn table_select(info: &CopyInfo) -> QueryNode {
    let select_list = if info.select_list.is_empty() {
        vec![ParsedExpression::new(
            ExpressionType::Star,
            ParsedExpressionKind::Star(Box::new(StarExpression {
                relation_name: String::new(),
                exclude_list: Vec::new(),
                replace_list: CaseInsensitiveMap::default(),
                rename_list: rustdb_common::OrderedMap::default(),
                columns: false,
                expr: None,
            })),
        )]
    } else {
        info.select_list
            .iter()
            .map(|name| ParsedExpression::column_ref([name.clone()]))
            .collect()
    };
    QueryNode {
        modifiers: Vec::new(),
        cte_map: CommonTableExpressionMap::default(),
        kind: QueryNodeKind::Select(Box::new(SelectNode {
            select_list,
            from_table: Box::new(TableRef {
                alias: String::new(),
                sample: None,
                query_location: None,
                kind: TableRefKind::BaseTable(Box::new(rustdb_ast::BaseTableRef {
                    catalog_name: info.catalog.clone(),
                    schema_name: info.schema.clone(),
                    table_name: info.table.clone(),
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

/// `Binder::Bind(CopyStatement &)`.
pub fn bind_copy(scope: &mut BindScope<'_>, info: &mut CopyInfo) -> Result<BoundStatement> {
    bind_copy_as(scope, info, CopyToType::CopyToFile)
}

/// `Binder::Bind(CopyStatement &, CopyToType)`: a COPY, or a table of EXPORT DATABASE.
pub fn bind_copy_as(
    scope: &mut BindScope<'_>,
    info: &mut CopyInfo,
    copy_to_type: CopyToType,
) -> Result<BoundStatement> {
    bind_copy_options(scope, info)?;
    if !info.is_from && info.select_statement.is_none() {
        info.select_statement = Some(Box::new(table_select(info)));
    }
    let function =
        if let Some(function) = copy_function(scope, &info.format.clone(), info.is_format_auto_detected)? {
            function
        } else {
            if let Some(error) = format_extension_error(&info.format) {
                return Err(error);
            }
            copy_function(scope, "csv", false)?.ok_or_else(|| Error::internal("csv copy function missing"))?
        };
    // Formats without an option list (DuckDB's `copy_options == nullptr`) check their
    // options themselves.
    if !function.options.is_empty() {
        check_copy_options(scope, &function, info)?;
    }
    if !info.is_from
        && let Some(plan) = function.plan.clone()
    {
        return bind_copy_plan(scope, info, &plan, copy_to_type);
    }
    scope.with_properties(|properties| properties.return_type = StatementReturnType::ChangedRows);
    if info.is_from {
        bind_copy_from(scope, info, &function)
    } else {
        bind_copy_to(scope, info, &function, copy_to_type)
    }
}

/// `function.plan(binder, stmt)`: binds the COPY the format rewrites `COPY ... TO` into.
fn bind_copy_plan(
    scope: &mut BindScope<'_>,
    info: &CopyInfo,
    plan: &rustdb_function::copy::CopyPlan,
    copy_to_type: CopyToType,
) -> Result<BoundStatement> {
    let original = info
        .select_statement
        .clone()
        .ok_or_else(|| Error::internal("COPY TO without a query"))?;
    // Bind the query in a separate binder to learn its columns.
    let mut node = original.clone();
    let mut child = scope.binder.child()?;
    let bound = {
        let mut child_scope = BindScope {
            binder: &mut child,
            outer: Some(scope as &mut dyn crate::binder::Scope),
        };
        bind_node(&mut child_scope, &mut node)?
    };
    let rewrite = plan(&rustdb_function::copy::CopyPlanInput {
        names: &bound.names,
        types: &bound.types,
        options: &info.options,
    })?;
    let select_list =
        rustdb_parser::parse_expression_list(&rewrite.select_list, &rustdb_parser::ParserOptions::default())?;
    let subquery = TableRef {
        alias: String::new(),
        sample: None,
        query_location: None,
        kind: TableRefKind::Subquery(rustdb_ast::SubqueryRef {
            subquery: Box::new(rustdb_ast::SelectStatement {
                node: original,
                named_param_map: CaseInsensitiveMap::default(),
            }),
            column_name_alias: Vec::new(),
        }),
    };
    let mut rewritten = info.clone();
    rewritten.format = rewrite.format;
    rewritten.is_format_auto_detected = false;
    rewritten.options = rewrite.options;
    rewritten.select_statement = Some(Box::new(QueryNode {
        modifiers: Vec::new(),
        cte_map: CommonTableExpressionMap::default(),
        kind: QueryNodeKind::Select(Box::new(SelectNode {
            select_list,
            from_table: Box::new(subquery),
            where_clause: None,
            group_expressions: Vec::new(),
            group_sets: Vec::new(),
            aggregate_handling: AggregateHandling::StandardHandling,
            having: None,
            sample: None,
            qualify: None,
        })),
    }));
    bind_copy_as(scope, &mut rewritten, copy_to_type)
}

/// `Binder::BindCopyTo`.
#[allow(clippy::too_many_lines)]
fn bind_copy_to(
    scope: &mut BindScope<'_>,
    info: &mut CopyInfo,
    function: &CopyFunction,
    copy_to_type: CopyToType,
) -> Result<BoundStatement> {
    let mut node = info
        .select_statement
        .clone()
        .ok_or_else(|| Error::internal("COPY TO without a query"))?;
    let mut select_node = bind_node(scope, &mut node)?;
    let Some(copy_to_bind) = function.copy_to_bind.clone() else {
        return Err(Error::not_implemented(format!(
            "COPY TO is not supported for FORMAT \"{}\"",
            info.format
        )));
    };
    let mut use_tmp_file = true;
    let mut overwrite_mode = CopyOverwriteMode::ErrorOnConflict;
    let mut filename_pattern = FilenamePattern::default();
    let mut user_set_use_tmp_file = false;
    let mut per_thread_output = false;
    let mut file_size_bytes = None;
    let mut partition_cols = Vec::new();
    let mut seen_overwrite_mode = false;
    let mut seen_filepattern = false;
    let mut write_partition_columns = false;
    let mut write_empty_file = true;
    let mut hive_file_pattern = true;
    let mut preserve_order = None;
    let mut return_type = CopyFunctionReturnType::ChangedRows;
    let mut file_extension = function.extension.clone();
    let original_options = std::mem::take(&mut info.options);
    for (name, values) in original_options.iter() {
        match name.to_lowercase().as_str() {
            "use_tmp_file" => {
                use_tmp_file = boolean_arg(scope, values)?;
                user_set_use_tmp_file = true;
            }
            loption @ ("overwrite_or_ignore" | "overwrite" | "append") => {
                if seen_overwrite_mode {
                    return Err(Error::binder(
                        "Can only set one of OVERWRITE_OR_IGNORE, OVERWRITE or APPEND",
                    ));
                }
                seen_overwrite_mode = true;
                if boolean_arg(scope, values)? {
                    overwrite_mode = match loption {
                        "overwrite_or_ignore" => CopyOverwriteMode::OverwriteOrIgnore,
                        "overwrite" => CopyOverwriteMode::Overwrite,
                        _ => {
                            if !seen_filepattern {
                                filename_pattern.set_pattern("{uuid}");
                            }
                            CopyOverwriteMode::Append
                        }
                    };
                }
            }
            "filename_pattern" => {
                let Some(value) = values.first() else {
                    return Err(Error::new(ErrorKind::Io, "FILENAME_PATTERN cannot be empty"));
                };
                filename_pattern.set_pattern(&value.to_varchar()?);
                seen_filepattern = true;
            }
            "file_extension" => {
                let Some(value) = values.first() else {
                    return Err(Error::new(ErrorKind::Io, "FILE_EXTENSION cannot be empty"));
                };
                file_extension = value.to_varchar()?;
            }
            "per_thread_output" => per_thread_output = boolean_arg(scope, values)?,
            "file_size_bytes" => {
                let Some(value) = values.first() else {
                    return Err(Error::binder("FILE_SIZE_BYTES cannot be empty"));
                };
                if function.copy_to_file_size.is_none() {
                    return Err(Error::not_implemented(format!(
                        "FILE_SIZE_BYTES not implemented for FORMAT \"{}\"",
                        info.format
                    )));
                }
                file_size_bytes = Some(if value.logical_type().id() == LogicalTypeId::Varchar {
                    parse_memory_limit(&value.to_varchar()?)?
                } else {
                    let value = value.try_cast_as(&scope.context.casts, &LogicalType::UBIGINT, false)?;
                    value.as_u64().unwrap_or(0)
                });
            }
            "partition_by" => {
                partition_cols = parse_columns_ordered(values, &select_node.names, "partition_by")?;
            }
            "return_files" => {
                if boolean_arg(scope, values)? {
                    return_type = CopyFunctionReturnType::ChangedRowsAndFileList;
                }
            }
            "preserve_order" => preserve_order = Some(boolean_arg(scope, values)?),
            "return_stats" => {
                if boolean_arg(scope, values)? {
                    return_type = CopyFunctionReturnType::WrittenFileStatistics;
                }
            }
            "write_partition_columns" => write_partition_columns = boolean_arg(scope, values)?,
            "write_empty_file" => write_empty_file = boolean_arg(scope, values)?,
            "hive_file_pattern" => hive_file_pattern = boolean_arg(scope, values)?,
            _ => {
                info.options.insert(name.to_owned(), values.clone());
            }
        }
    }
    if overwrite_mode == CopyOverwriteMode::Append && !filename_pattern.has_uuid() {
        return Err(Error::binder(
            "APPEND mode requires a {uuid} label in filename_pattern",
        ));
    }
    if user_set_use_tmp_file && per_thread_output {
        return Err(Error::not_implemented(
            "Can't combine USE_TMP_FILE and PER_THREAD_OUTPUT for COPY",
        ));
    }
    if user_set_use_tmp_file && file_size_bytes.is_some() {
        return Err(Error::not_implemented(
            "Can't combine USE_TMP_FILE and FILE_SIZE_BYTES for COPY",
        ));
    }
    if user_set_use_tmp_file && !partition_cols.is_empty() {
        return Err(Error::not_implemented(
            "Can't combine USE_TMP_FILE and PARTITION_BY for COPY",
        ));
    }
    if per_thread_output && !partition_cols.is_empty() {
        return Err(Error::not_implemented(
            "Can't combine PER_THREAD_OUTPUT and PARTITION_BY for COPY",
        ));
    }
    if file_size_bytes.is_some() && !partition_cols.is_empty() {
        return Err(Error::not_implemented(
            "Can't combine FILE_SIZE_BYTES and PARTITION_BY for COPY",
        ));
    }
    if !write_partition_columns && partition_cols.len() == select_node.names.len() {
        return Err(Error::not_implemented(
            "No column to write as all columns are specified as partition columns. WRITE_PARTITION_COLUMNS option can be used to write partition columns.",
        ));
    }
    let is_file_and_exists = std::path::Path::new(&info.file_path).is_file();
    let is_stdout = info.file_path == "/dev/stdout";
    if !user_set_use_tmp_file {
        use_tmp_file = is_file_and_exists && !per_thread_output && partition_cols.is_empty() && !is_stdout;
    }
    // The format may cast columns it cannot write (`copy_to_select`): a projection on
    // top of the query.
    if let Some(copy_to_select) = &function.copy_to_select {
        let input = CopyToSelectInput {
            names: &select_node.names,
            types: &select_node.types,
            options: &info.options,
            copy_to_type,
            binder: Some(scope.context.as_ref() as &dyn std::any::Any),
        };
        if let Some(new_types) = copy_to_select(&input)? {
            let bindings = select_node.plan.column_bindings();
            let mut select_list = Vec::with_capacity(new_types.len());
            for (((name, source), target), binding) in select_node
                .names
                .iter()
                .zip(&select_node.types)
                .zip(&new_types)
                .zip(bindings)
            {
                let column =
                    crate::expression::Expression::column_ref(name.clone(), source.clone(), binding, 0);
                select_list.push(
                    crate::function_binder::add_cast_to_type(&scope.context, column, target, false)?
                        .with_alias(name.clone()),
                );
            }
            let plan = std::mem::replace(
                &mut select_node.plan,
                LogicalOperator::new(LogicalKind::DummyScan { table_index: 0 }),
            );
            select_node.plan = LogicalOperator::projection(scope.generate_table_index(), select_list, plan);
            select_node.types = new_types;
        }
    }
    let mut unique_column_names = select_node.names.clone();
    deduplicate_columns(&mut unique_column_names);
    let names_to_write =
        LogicalCopyToFile::without_partitions(&unique_column_names, &partition_cols, write_partition_columns);
    let types_to_write =
        LogicalCopyToFile::without_partitions(&select_node.types, &partition_cols, write_partition_columns);
    let mut bind_input = CopyFunctionBindInput {
        options: &info.options,
        file_path: &info.file_path,
        format: &info.format,
        file_extension,
        binder: Some(scope.context.as_ref() as &dyn std::any::Any),
    };
    let bind_data = copy_to_bind(&mut bind_input, &names_to_write, &types_to_write)?;
    let file_extension = bind_input.file_extension;
    let rotate = match &function.rotate_files {
        Some(rotate_files) => rotate_files(bind_data.as_ref(), file_size_bytes),
        None => file_size_bytes.is_some() && function.copy_to_file_size.is_some(),
    };
    if rotate {
        if user_set_use_tmp_file {
            return Err(Error::not_implemented(
                "Can't combine USE_TMP_FILE and file rotation (e.g., ROW_GROUPS_PER_FILE) for COPY",
            ));
        }
        if !partition_cols.is_empty() {
            return Err(Error::not_implemented(
                "Can't combine file rotation (e.g., ROW_GROUPS_PER_FILE) and PARTITION_BY for COPY",
            ));
        }
    }
    if !write_empty_file {
        if per_thread_output {
            return Err(Error::not_implemented(
                "Can't combine WRITE_EMPTY_FILE false with PER_THREAD_OUTPUT",
            ));
        }
        if !partition_cols.is_empty() {
            return Err(Error::not_implemented(
                "Can't combine WRITE_EMPTY_FILE false with PARTITION_BY",
            ));
        }
    }
    if return_type == CopyFunctionReturnType::WrittenFileStatistics
        && function.copy_to_get_written_statistics.is_none()
    {
        return Err(Error::not_implemented(format!(
            "RETURN_STATS is not supported for the \"{}\" copy format",
            info.format
        )));
    }
    let copy = LogicalCopyToFile {
        function: function.clone(),
        bind_data,
        file_path: info.file_path.clone(),
        use_tmp_file,
        overwrite_mode,
        filename_pattern,
        file_extension,
        per_thread_output,
        file_size_bytes,
        rotate,
        partition_output: !partition_cols.is_empty(),
        write_partition_columns,
        partition_columns: partition_cols,
        write_empty_file,
        hive_file_pattern,
        preserve_order,
        return_type,
        names: unique_column_names,
        expected_types: select_node.types.clone(),
        copy_info: Box::new(info.clone()),
    };
    scope.with_properties(|properties| {
        properties.return_type = if return_type == CopyFunctionReturnType::ChangedRows {
            StatementReturnType::ChangedRows
        } else {
            StatementReturnType::QueryResult
        };
    });
    let plan =
        LogicalOperator::new(LogicalKind::CopyToFile(Box::new(copy))).with_children(vec![select_node.plan]);
    Ok(BoundStatement::new(
        plan,
        return_type.names(),
        return_type.types(),
    ))
}

/// `Binder::BindCopyFrom`: an INSERT whose rows come from the format's reader.
fn bind_copy_from(
    scope: &mut BindScope<'_>,
    info: &CopyInfo,
    function: &CopyFunction,
) -> Result<BoundStatement> {
    if info.table.is_empty() {
        return Err(Error::parser("COPY FROM requires a table name to be specified"));
    }
    let (Some(copy_from_bind), Some(copy_from_function)) = (
        function.copy_from_bind.clone(),
        function.copy_from_function.clone(),
    ) else {
        return Err(Error::not_implemented(format!(
            "COPY FROM is not supported for FORMAT \"{}\"",
            info.format
        )));
    };
    let mut insert = InsertStatement {
        select_statement: None,
        columns: info.select_list.clone(),
        catalog: info.catalog.clone(),
        schema: info.schema.clone(),
        table: info.table.clone(),
        table_ref: None,
        returning_list: Vec::new(),
        on_conflict_info: None,
        cte_map: CommonTableExpressionMap::default(),
        default_values: false,
        column_order: InsertColumnOrder::InsertByPosition,
    };
    let mut bound = bind_insert(scope, &mut insert)?;
    let LogicalKind::Insert(bound_insert) = &bound.plan.kind else {
        return Err(Error::internal("COPY FROM did not bind to an INSERT"));
    };
    let EntryKind::Table(table) = &bound_insert.table.kind else {
        return Err(Error::internal("COPY FROM target is not a table"));
    };
    let expected_types = bound_insert.expected_types.clone();
    let mut expected_names = vec![String::new(); expected_types.len()];
    if bound_insert.column_index_map.is_empty() {
        expected_names = table
            .physical_columns()
            .map(|(_, _, column)| column.name.clone())
            .collect();
    } else {
        for ((physical, _, column), mapped) in table.physical_columns().zip(&bound_insert.column_index_map) {
            let _ = physical;
            if let Some(index) = mapped {
                expected_names[*index].clone_from(&column.name);
            }
        }
    }
    let input = CopyFromBindInput {
        options: &info.options,
        file_path: &info.file_path,
        format: &info.format,
        binder: Some(scope.context.as_ref() as &dyn std::any::Any),
    };
    let bind_data = copy_from_bind(&input, &expected_names, &expected_types)?;
    let get = LogicalGet {
        table_index: scope.generate_table_index(),
        function: copy_from_function,
        bind_data: Some(bind_data),
        returned_types: expected_types.clone(),
        names: expected_names,
        column_ids: (0..expected_types.len() as u64).map(ColumnIndex::new).collect(),
        projection_ids: Vec::new(),
        parameters: Vec::new(),
        named_parameters: Vec::new(),
        input_table_types: Vec::new(),
        input_table_names: Vec::new(),
        projected_input: Vec::new(),
        ordinality_index: None,
        shared_column_ids: None,
        table: None,
        table_schema: None,
        table_filters: rustdb_storage::TableFilterSet::default(),
        dynamic_filters: None,
        sample_options: None,
        file_filters: crate::operator::FileFilterInfo::default(),
        row_group_order: None,
        is_create_index: false,
    };
    bound
        .plan
        .add_child(LogicalOperator::new(LogicalKind::Get(Box::new(get))));
    scope.with_properties(|properties| properties.return_type = StatementReturnType::ChangedRows);
    Ok(BoundStatement::new(
        bound.plan,
        vec!["Count".to_owned()],
        vec![LogicalType::BIGINT],
    ))
}
