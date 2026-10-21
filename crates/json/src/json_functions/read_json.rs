//! `read_json`, `read_ndjson`, `read_json_auto`, `read_ndjson_auto` and the
//! `read_json_objects` family (DuckDB's `read_json.cpp` and `read_json_objects.cpp`).

use std::sync::Arc;

use rustdb_common::Error;
use rustdb_csv::multi_file::{known_column_value, multi_file_parameters};
use rustdb_function::{FileFilterPushdown, FunctionData, TableFunction, TableFunctionSet};
use rustdb_types::LogicalType;

use crate::json_enums::{JsonFormat, JsonRecordType, JsonScanType};
use crate::json_multi_file_info::{JsonScanInfo, json_bind};
use crate::json_scan::{JsonScanData, json_parallel_scan, json_scan, json_virtual_columns};

/// `JSONScan::TableFunctionDefaults`: the parameters of every JSON reader.
fn default_parameters() -> Vec<(String, LogicalType)> {
    // The multi-file parameters come first (`MultiFileFunction`).
    let mut parameters = multi_file_parameters();
    parameters.extend(
        [
            ("maximum_object_size", LogicalType::UINTEGER),
            ("ignore_errors", LogicalType::BOOLEAN),
            ("format", LogicalType::VARCHAR),
            ("compression", LogicalType::VARCHAR),
        ]
        .into_iter()
        .map(|(name, ty)| (name.to_owned(), ty)),
    );
    parameters
}

/// The parameters of `read_json` (`GetReadJSONTableFunction` + `CreateJSONFunctionInfo`).
fn read_json_parameters() -> Vec<(String, LogicalType)> {
    let mut parameters = default_parameters();
    parameters.extend(
        [
            ("columns", LogicalType::ANY),
            ("auto_detect", LogicalType::BOOLEAN),
            ("sample_size", LogicalType::BIGINT),
            ("dateformat", LogicalType::VARCHAR),
            ("date_format", LogicalType::VARCHAR),
            ("timestampformat", LogicalType::VARCHAR),
            ("timestamp_format", LogicalType::VARCHAR),
            ("records", LogicalType::VARCHAR),
            ("maximum_sample_files", LogicalType::BIGINT),
            ("maximum_depth", LogicalType::BIGINT),
            ("field_appearance_threshold", LogicalType::DOUBLE),
            ("convert_strings_to_integers", LogicalType::BOOLEAN),
            ("map_inference_threshold", LogicalType::BIGINT),
        ]
        .into_iter()
        .map(|(name, ty)| (name.to_owned(), ty)),
    );
    parameters
}

/// The file filter pushdown of the JSON readers (`MultiFileComplexFilterPushdown`).
fn json_file_filter_pushdown() -> FileFilterPushdown {
    FileFilterPushdown {
        files: Arc::new(|bind_data| {
            let data = bind_data.as_any().downcast_ref::<JsonScanData>()?;
            (data.file_options.hive_partitioning || data.file_options.filename)
                .then(|| data.files.files.clone())
        }),
        known_value: Arc::new(|bind_data, path, name, ty| {
            let Some(data) = bind_data.as_any().downcast_ref::<JsonScanData>() else {
                return Ok(None);
            };
            known_column_value(&data.file_options, &data.casts, path, name, ty)
        }),
        with_files: Arc::new(|bind_data, keep| {
            let data = bind_data
                .as_any()
                .downcast_ref::<JsonScanData>()
                .ok_or_else(|| Error::internal("read_json file pruning without bind data"))?;
            Ok(Arc::new(data.with_files(keep)) as Arc<dyn FunctionData>)
        }),
    }
}

/// One overload of a JSON reader (`MultiFileFunction<JSONMultiFileInfo>`).
pub(crate) fn json_reader_function(
    name: &'static str,
    info: JsonScanInfo,
    argument: LogicalType,
) -> TableFunction {
    let mut function = TableFunction::new(name, vec![argument])
        .with_bind(move |input| json_bind(name, info, input))
        .with_scan(json_scan)
        .with_cardinality(|bind_data| {
            let data = bind_data?.as_any().downcast_ref::<JsonScanData>()?;
            Some(data.cardinality())
        });
    function.named_parameters = if info.scan_type == JsonScanType::ReadJsonObjects {
        default_parameters()
    } else {
        read_json_parameters()
    };
    function.projection_pushdown = true;
    function.projected_output = true;
    function.empty_column = true;
    function.parallel_scan = Some(json_parallel_scan());
    function.get_virtual_columns = Some(Arc::new(json_virtual_columns));
    function.file_filter_pushdown = Some(json_file_filter_pushdown());
    function
}

/// `MultiFileReader::CreateFunctionSet`: the VARCHAR and VARCHAR[] overloads.
fn function_set(name: &'static str, info: JsonScanInfo) -> TableFunctionSet {
    let mut set = TableFunctionSet::new(name);
    set.add(json_reader_function(name, info, LogicalType::VARCHAR));
    set.add(json_reader_function(
        name,
        info,
        LogicalType::list(LogicalType::VARCHAR),
    ));
    set
}

/// The JSON reader table functions.
pub(crate) fn read_json_functions() -> Vec<TableFunctionSet> {
    let objects = |format| JsonScanInfo {
        scan_type: JsonScanType::ReadJsonObjects,
        format,
        record_type: JsonRecordType::Records,
        auto_detect: false,
    };
    let columns = |format| JsonScanInfo {
        scan_type: JsonScanType::ReadJson,
        format,
        record_type: JsonRecordType::AutoDetect,
        auto_detect: true,
    };
    vec![
        function_set("read_json_objects", objects(JsonFormat::AutoDetect)),
        function_set("read_ndjson_objects", objects(JsonFormat::NewlineDelimited)),
        function_set("read_json_objects_auto", objects(JsonFormat::AutoDetect)),
        function_set("read_json", columns(JsonFormat::AutoDetect)),
        function_set("read_ndjson", columns(JsonFormat::NewlineDelimited)),
        function_set("read_json_auto", columns(JsonFormat::AutoDetect)),
        function_set("read_ndjson_auto", columns(JsonFormat::NewlineDelimited)),
    ]
}
