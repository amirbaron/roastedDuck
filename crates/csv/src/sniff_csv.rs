//! The `sniff_csv` table function (DuckDB's `sniff_csv.cpp`): the dialect, columns and a
//! `read_csv` call for a file.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{ByteString, Error, Result, byte_write};
use rustdb_function::cast::CastFunctionSet;
use rustdb_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
    TableScanState,
};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value, Vector};

use crate::buffer::CsvBufferManager;
use crate::multi_file::{MultiFileOptions, glob_file_list};
use crate::options::{CsvReaderOptions, FormatType};
use crate::read_csv::{client_context, read_csv_parameters, transform_string_to_logical_type};
use crate::sniffer::CsvSniffer;

/// The bind data of `sniff_csv` (DuckDB's `CSVSniffFunctionData`).
#[derive(Debug)]
struct CsvSniffFunctionData {
    path: rustdb_common::ByteString,
    options: CsvReaderOptions,
    file_options: MultiFileOptions,
    force_match: bool,
    casts: Arc<CastFunctionSet>,
}

impl FunctionData for CsvSniffFunctionData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The output columns of `sniff_csv`.
fn sniff_columns() -> Vec<(&'static str, LogicalType)> {
    let column = LogicalType::struct_type(vec![
        ("name".into(), LogicalType::VARCHAR),
        ("type".into(), LogicalType::VARCHAR),
    ]);
    vec![
        ("Delimiter", LogicalType::VARCHAR),
        ("Quote", LogicalType::VARCHAR),
        ("Escape", LogicalType::VARCHAR),
        ("NewLineDelimiter", LogicalType::VARCHAR),
        ("Comment", LogicalType::VARCHAR),
        ("SkipRows", LogicalType::UINTEGER),
        ("HasHeader", LogicalType::BOOLEAN),
        ("Columns", LogicalType::list(column)),
        ("DateFormat", LogicalType::VARCHAR),
        ("TimestampFormat", LogicalType::VARCHAR),
        ("UserArguments", LogicalType::VARCHAR),
        ("Prompt", LogicalType::VARCHAR),
    ]
}

/// `CSVSniffBind`.
fn sniff_csv_bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let context = client_context(input)?;
    let path = input
        .inputs
        .first()
        .ok_or_else(|| Error::internal("sniff_csv without a path"))?;
    if path.is_null() {
        return Err(Error::binder(
            "sniff_csv cannot take NULL as a file path parameter",
        ));
    }
    let mut force_match = true;
    let mut parameters = Vec::new();
    for (key, value) in input.named_parameters {
        match key.to_ascii_lowercase().as_bytes() {
            b"auto_detect" => {
                if value.is_null() {
                    return Err(Error::binder(rustdb_common::byte_format!(
                        "\"{key}\" expects a non-null boolean value (e.g. TRUE or 1)"
                    )));
                }
                if !value.as_bool().unwrap_or(true) {
                    return Err(Error::invalid_input(
                        "sniff_csv function does not accept auto_detect variable set to false",
                    ));
                }
            }
            b"force_match" => force_match = value.as_bool().unwrap_or(true),
            _ => parameters.push((key, value)),
        }
    }
    let parse_type = |text: &str| transform_string_to_logical_type(context, text);
    let mut file_options = MultiFileOptions::default();
    let mut options = CsvReaderOptions::default();
    for (key, value) in parameters {
        if file_options.parse_option(key, value, &parse_type)? {
            continue;
        }
        options.parse_option(key, value, &parse_type)?;
    }
    options.verify(&crate::options::MultiFileOptionsView {
        union_by_name: file_options.union_by_name,
        hive_partitioning: file_options.hive_partitioning,
        filename: file_options.filename,
    })?;
    let columns = sniff_columns();
    Ok(TableFunctionBindOutput {
        return_types: columns.iter().map(|(_, ty)| ty.clone()).collect(),
        names: columns.iter().map(|(name, _)| (*name).into()).collect(),
        bind_data: Some(Arc::new(CsvSniffFunctionData {
            path: path.to_varchar_bytes()?,
            options,
            file_options,
            force_match,
            casts: Arc::clone(&context.casts),
        })),
    })
}

/// `FormatOptions`: an option as written in a SQL string literal.
fn format_option(option: &str) -> String {
    match option {
        "'" => "''".to_owned(),
        "\0" => String::new(),
        _ => option.to_owned(),
    }
}

fn format_byte(option: u8) -> String {
    match option {
        b'\'' => "''".to_owned(),
        0 => String::new(),
        _ => char::from(option).to_string(),
    }
}

/// `CSVSniffFunction`: sniffs the file and produces the one result row.
#[allow(clippy::too_many_lines)]
fn sniff_csv_row(data: &CsvSniffFunctionData) -> Result<Vec<Value>> {
    let mut files = glob_file_list(&data.path, None)?;
    files.sort();
    if files.len() > 1 {
        return Err(Error::not_implemented(
            "sniff_csv does not operate on more than one file yet",
        ));
    }
    let file = files[0].clone();
    let mut options = data.options.clone();
    options.file_path.clone_from(&file);
    let buffer_manager = Arc::new(CsvBufferManager::new(&options, &file)?);
    let view = crate::options::MultiFileOptionsView::default();
    let _ = &data.file_options;
    let mut sniffer = CsvSniffer::new(&mut options, &view, buffer_manager, Arc::clone(&data.casts), true);
    let mut result = sniffer.sniff_csv(data.force_match)?;
    let empty_or_only_header = sniffer.empty_or_only_header();
    drop(sniffer);
    if empty_or_only_header {
        for i in 0..result.return_types.len() {
            if options.sql_types_per_column.is_empty() {
                if i < options.sql_type_list.len() {
                    continue;
                }
            } else if options.sql_types_per_column.contains_key(&result.names[i]) {
                continue;
            }
            result.return_types[i] = LogicalType::VARCHAR;
        }
    }
    for ty in &mut result.return_types {
        if ty.id() == LogicalTypeId::SqlNull {
            *ty = LogicalType::VARCHAR;
        }
    }
    let separator = ", ";
    let sm = &options.dialect_options.state_machine_options;
    let new_line = options.newline();
    let has_header = options.dialect_options.header.value();
    let mut row = vec![
        Value::varchar(sm.delimiter.format_value()),
        Value::varchar(sm.quote.format_value()),
        Value::varchar(sm.escape.format_value()),
        Value::varchar(new_line),
        Value::varchar(sm.comment.format_value()),
        Value::uinteger(u32::try_from(options.dialect_options.skip_rows.value()).unwrap_or(u32::MAX)),
        Value::boolean(has_header),
    ];
    let mut columns = ByteString::from("{");
    let mut values = Vec::new();
    for (i, (name, ty)) in result.names.iter().zip(&result.return_types).enumerate() {
        values.push(Value::struct_value(vec![
            ("name".into(), Value::try_varchar(name)?),
            ("type".into(), Value::varchar(ty.to_string())),
        ]));
        let _ = byte_write!(columns, "'{name}': '{ty}'");
        if i + 1 != result.return_types.len() {
            columns.push_bytes(separator);
        }
    }
    columns.push_bytes("}");
    row.push(Value::list(
        LogicalType::struct_type(vec![
            ("name".into(), LogicalType::VARCHAR),
            ("type".into(), LogicalType::VARCHAR),
        ]),
        values,
    ));
    let has_date = result
        .return_types
        .iter()
        .any(|ty| ty.id() == LogicalTypeId::Date);
    let date_format = options.dialect_options.date_format(FormatType::Date);
    if date_format.get().is_empty() {
        row.push(if has_date {
            Value::varchar("%Y-%m-%d")
        } else {
            Value::null(LogicalType::VARCHAR)
        });
    } else {
        row.push(Value::varchar(date_format.get().format_specifier()));
    }
    let timestamp_format = options.dialect_options.date_format(FormatType::Timestamp);
    row.push(if timestamp_format.get().is_empty() {
        Value::null(LogicalType::VARCHAR)
    } else {
        Value::varchar(timestamp_format.get().format_specifier())
    });
    row.push(if data.options.user_defined_parameters.is_empty() {
        Value::null(LogicalType::VARCHAR)
    } else {
        Value::try_varchar(&data.options.user_defined_parameters_string())?
    });
    let mut csv_read =
        rustdb_common::byte_format!("FROM read_csv('{file}'{separator}auto_detect=false{separator}");
    if !sm.delimiter.is_set_by_user() {
        let _ = byte_write!(
            csv_read,
            "delim='{}'{separator}",
            format_option(sm.delimiter.get())
        );
    }
    if !sm.quote.is_set_by_user() {
        let _ = byte_write!(csv_read, "quote='{}'{separator}", format_byte(sm.quote.value()));
    }
    if !sm.escape.is_set_by_user() {
        let _ = byte_write!(csv_read, "escape='{}'{separator}", format_byte(sm.escape.value()));
    }
    if !sm.new_line.is_set_by_user() && new_line != "mix" {
        let _ = byte_write!(csv_read, "new_line='{new_line}'{separator}");
    }
    if !options.dialect_options.skip_rows.is_set_by_user() {
        let _ = byte_write!(
            csv_read,
            "skip={}{separator}",
            options.dialect_options.skip_rows.value()
        );
    }
    if !sm.comment.is_set_by_user() {
        let _ = byte_write!(
            csv_read,
            "comment='{}'{separator}",
            format_byte(sm.comment.value())
        );
    }
    if !options.dialect_options.header.is_set_by_user() {
        let _ = byte_write!(
            csv_read,
            "header={}{separator}",
            if has_header { "true" } else { "false" }
        );
    }
    let _ = byte_write!(csv_read, "columns={columns}");
    if !date_format.is_set_by_user() {
        if !date_format.get().format_specifier().is_empty() {
            let _ = byte_write!(
                csv_read,
                "{separator}dateformat='{}'",
                date_format.get().format_specifier()
            );
        } else if has_date {
            let _ = byte_write!(csv_read, "{separator}dateformat='%Y-%m-%d'");
        }
    }
    if !timestamp_format.is_set_by_user() && !timestamp_format.get().format_specifier().is_empty() {
        let _ = byte_write!(
            csv_read,
            "{separator}timestampformat='{}'",
            timestamp_format.get().format_specifier()
        );
    }
    if !data.options.user_defined_parameters.is_empty() {
        csv_read.push_bytes(separator);
        csv_read.push_bytes(data.options.user_defined_parameters_string());
    }
    csv_read.push_bytes(");");
    row.push(Value::try_varchar(&csv_read)?);
    Ok(row)
}

fn sniff_csv_scan(
    bind_data: Option<&dyn FunctionData>,
    _: &[u64],
    state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<CsvSniffFunctionData>())
        .ok_or_else(|| Error::internal("sniff_csv scan without bind data"))?;
    let types: Vec<LogicalType> = sniff_columns().into_iter().map(|(_, ty)| ty).collect();
    if state.position > 0 {
        *chunk = DataChunk::new(types.into_iter().map(Vector::constant_null).collect(), 0);
        return Ok(false);
    }
    let row = sniff_csv_row(data)?;
    let mut columns = Vec::with_capacity(types.len());
    for (ty, value) in types.into_iter().zip(&row) {
        let mut vector = Vector::flat(ty, 1);
        vector.set_value(0, value)?;
        columns.push(vector);
    }
    *chunk = DataChunk::new(columns, 1);
    state.position = 1;
    Ok(false)
}

/// `sniff_csv`.
pub fn sniff_csv_functions() -> Vec<TableFunctionSet> {
    let mut function = TableFunction::new("sniff_csv", vec![LogicalType::VARCHAR])
        .with_bind(sniff_csv_bind)
        .with_scan(sniff_csv_scan);
    function.named_parameters = read_csv_parameters();
    function
        .named_parameters
        .push(("force_match".to_owned(), LogicalType::BOOLEAN));
    let mut set = TableFunctionSet::new("sniff_csv");
    set.add(function);
    vec![set]
}
