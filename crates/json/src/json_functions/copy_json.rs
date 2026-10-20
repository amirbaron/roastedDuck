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
fn parameter_error(option: &str) -> Error {
    Error::binder(format!(
        "COPY (FORMAT JSON) parameter {option} expects a single argument."
    ))
}

/// A SQL string literal.
fn quote_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// A SQL identifier.
fn quote_identifier(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

/// `CopyToJSONPlan`.
fn copy_to_json_plan(input: &CopyPlanInput<'_>) -> Result<CopyPlanRewrite> {
    use rustdb_function::cast::ValueCast;
    let mut date_format = String::new();
    let mut timestamp_format = String::new();
    let mut csv_options: CaseInsensitiveMap<Vec<Value>> = CaseInsensitiveMap::default();
    csv_options.insert("file_extension".to_owned(), vec![Value::varchar("json")]);
    for (key, values) in input.options.iter() {
        let option = key.to_lowercase();
        match option.as_str() {
            "dateformat" | "date_format" => {
                let [value] = values.as_slice() else {
                    return Err(parameter_error(&option));
                };
                date_format = value.to_varchar()?;
            }
            "timestampformat" | "timestamp_format" => {
                let [value] = values.as_slice() else {
                    return Err(parameter_error(&option));
                };
                timestamp_format = value.to_varchar()?;
            }
            "array" => {
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
            "file_extension" => {
                let extension = values
                    .last()
                    .map(Value::to_varchar)
                    .transpose()?
                    .unwrap_or_default();
                csv_options.insert("file_extension".to_owned(), vec![Value::varchar(extension)]);
            }
            _ if SUPPORTED_BASE_OPTIONS.contains(&option.as_str()) => {
                csv_options.insert(key.to_owned(), values.clone());
            }
            _ => {
                return Err(Error::binder(format!(
                    "Unknown option for COPY ... TO ... (FORMAT JSON): \"{option}\"."
                )));
            }
        }
    }
    // `to_json(struct_pack(name := #i, ...))`, formatting dates and timestamps if asked.
    let mut fields = Vec::with_capacity(input.types.len());
    for (index, (ty, name)) in input.types.iter().zip(input.names).enumerate() {
        let column = format!("#{}", index + 1);
        let column = if !date_format.is_empty() && ty.id() == LogicalTypeId::Date {
            format!("strftime({column}, {})", quote_literal(&date_format))
        } else if !timestamp_format.is_empty()
            && matches!(ty.id(), LogicalTypeId::Timestamp | LogicalTypeId::TimestampTz)
        {
            format!("strftime({column}, {})", quote_literal(&timestamp_format))
        } else {
            column
        };
        fields.push(format!("{} := {column}", quote_identifier(name)));
    }
    let select_list = format!("to_json(struct_pack({}))", fields.join(", "));
    csv_options.insert("quote".to_owned(), vec![Value::varchar("")]);
    csv_options.insert("escape".to_owned(), vec![Value::varchar("")]);
    csv_options.insert("delimiter".to_owned(), vec![Value::varchar("\n")]);
    csv_options.insert("header".to_owned(), vec![Value::integer(0)]);
    Ok(CopyPlanRewrite {
        format: "csv".to_owned(),
        options: csv_options,
        select_list,
    })
}

/// `MultiFileBindCopy` for `COPY ... FROM ... (FORMAT json)`.
fn copy_from_json_bind(
    input: &CopyFromBindInput<'_>,
    names: &[String],
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
