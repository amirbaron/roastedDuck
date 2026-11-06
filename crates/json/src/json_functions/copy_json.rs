//! `COPY ... (FORMAT json)` (DuckDB's `copy_json.cpp`): writing is a rewrite to the CSV
//! writer of one column `to_json(struct_pack(...))` per row; reading is `read_json` with
//! the table's columns.

use std::sync::Arc;

use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_function::copy::{CopyFromBindInput, CopyPlanInput, CopyPlanRewrite};
use rustdb_function::{CopyFunction, FunctionData};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

use crate::json_enums::{JsonFormat, JsonRecordType, JsonScanType};
use crate::json_functions::read_json::json_reader_function;
use crate::json_multi_file_info::{JsonScanInfo, json_copy_from_bind};

/// The generic COPY options the rewrite passes on (`SUPPORTED_BASE_OPTIONS`).
const SUPPORTED_BASE_OPTIONS: [&str; 16] = [
    "compression",
    "encoding",
    "use_tmp_file",
    "overwrite_or_ignore",
    "overwrite",
    "append",
    "filename_pattern",
    "file_extension",
    "per_thread_output",
    "file_size_bytes",
    "return_files",
    "preserve_order",
    "return_stats",
    "write_partition_columns",
    "write_empty_file",
    "hive_file_pattern",
];

/// `ThrowJSONCopyParameterException`.
fn parameter_error(option: &[u8]) -> Error {
    Error::binder(rustdb_common::byte_format!(
        "COPY (FORMAT JSON) parameter {option} expects a single argument."
    ))
}

/// `CopyToJSONPlan`.
fn copy_to_json_plan(input: &CopyPlanInput<'_>) -> Result<CopyPlanRewrite> {
    use rustdb_function::cast::ValueCast;
    let mut date_format = String::new();
    let mut timestamp_format = String::new();
    let mut csv_options: CaseInsensitiveMap<Vec<Value>, rustdb_common::ByteString> =
        CaseInsensitiveMap::default();
    csv_options.insert("file_extension".to_owned(), vec![Value::varchar("json")]);
    for (key, values) in input.options.iter() {
        let option = key.to_ascii_lowercase();
        match option.as_bytes() {
            b"dateformat" | b"date_format" => {
                let [value] = values.as_slice() else {
                    return Err(parameter_error(&option));
                };
                date_format = value.to_varchar()?;
            }
            b"timestampformat" | b"timestamp_format" => {
                let [value] = values.as_slice() else {
                    return Err(parameter_error(&option));
                };
                timestamp_format = value.to_varchar()?;
            }
            b"array" => {
                if values.len() > 1 {
                    return Err(parameter_error(&option));
                }
                let array = match values.last() {
                    None => true,
                    Some(value) => value
                        .default_cast_as(&LogicalType::BOOLEAN)?
                        .as_bool()
                        .unwrap_or(false),
                };
                if array {
                    csv_options.insert("prefix".to_owned(), vec![Value::varchar("[\n\t")]);
                    csv_options.insert("suffix".to_owned(), vec![Value::varchar("\n]\n")]);
                    csv_options.insert("new_line".to_owned(), vec![Value::varchar(",\n\t")]);
                }
            }
            b"file_extension" => {
                let extension = values
                    .last()
                    .map(Value::to_varchar)
                    .transpose()?
                    .unwrap_or_default();
                csv_options.insert("file_extension".to_owned(), vec![Value::varchar(extension)]);
            }
            _ if SUPPORTED_BASE_OPTIONS
                .iter()
                .any(|key| key.as_bytes() == option.as_bytes()) =>
            {
                csv_options.insert(key.to_owned(), values.clone());
            }
            _ => {
                return Err(Error::binder(rustdb_common::byte_format!(
                    "Unknown option for COPY ... TO ... (FORMAT JSON): \"{option}\"."
                )));
            }
        }
    }
    // The same positional/strftime/struct_pack/to_json tree as C++'s generated SQL.
    let mut fields = Vec::with_capacity(input.types.len());
    for (index, (ty, name)) in input.types.iter().zip(input.names).enumerate() {
        let mut column = rustdb_parser::build::positional_reference((index + 1) as u64);
        let format = if !date_format.is_empty() && ty.id() == LogicalTypeId::Date {
            Some(&date_format)
        } else if !timestamp_format.is_empty()
            && matches!(ty.id(), LogicalTypeId::Timestamp | LogicalTypeId::TimestampTz)
        {
            Some(&timestamp_format)
        } else {
            None
        };
        if let Some(format) = format {
            column = rustdb_ast::ParsedExpression::function(
                "strftime",
                vec![
                    column,
                    rustdb_ast::ParsedExpression::constant(Value::varchar(format)),
                ],
            );
        }
        column.alias.clone_from(name);
        fields.push(column);
    }
    let select_list = vec![rustdb_ast::ParsedExpression::function(
        "to_json",
        vec![rustdb_ast::ParsedExpression::function("struct_pack", fields)],
    )];
    csv_options.insert("quote".to_owned(), vec![Value::varchar("")]);
    csv_options.insert("escape".to_owned(), vec![Value::varchar("")]);
    csv_options.insert("delimiter".to_owned(), vec![Value::varchar("\n")]);
    csv_options.insert("header".to_owned(), vec![Value::integer(0)]);
    Ok(CopyPlanRewrite {
        format: "csv".into(),
        options: csv_options,
        select_list,
    })
}

/// `MultiFileBindCopy` for `COPY ... FROM ... (FORMAT json)`.
fn copy_from_json_bind(
    input: &CopyFromBindInput<'_>,
    names: &[rustdb_common::ByteString],
    types: &[LogicalType],
) -> Result<Arc<dyn FunctionData>> {
    let context = input
        .binder
        .and_then(|binder| binder.downcast_ref::<rustdb_planner::ClientContext>())
        .ok_or_else(|| Error::internal("COPY FROM json without a client context"))?;
    Ok(Arc::new(json_copy_from_bind(
        context,
        input.file_path,
        input.options,
        names,
        types,
    )?))
}

/// The `json`, `ndjson` and `jsonl` copy functions (`JSONFunctions::GetJSONCopyFunction`).
pub(crate) fn json_copy_functions() -> Vec<CopyFunction> {
    let info = JsonScanInfo {
        scan_type: JsonScanType::ReadJson,
        format: JsonFormat::AutoDetect,
        record_type: JsonRecordType::Records,
        auto_detect: false,
    };
    ["json", "ndjson", "jsonl"]
        .into_iter()
        .map(|name| {
            let mut function = CopyFunction::new(name);
            name.clone_into(&mut function.extension);
            function.plan = Some(Arc::new(copy_to_json_plan));
            function.copy_from_bind = Some(Arc::new(copy_from_json_bind));
            function.copy_from_function = Some(json_reader_function("read_json", info, LogicalType::VARCHAR));
            function
        })
        .collect()
}
