//! Binding the JSON readers (DuckDB's `json_multi_file_info.cpp` `BindReader` with the
//! schema detection of `read_json.cpp` and the formats of `json_scan.cpp`).

use std::collections::HashMap;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_csv::multi_file::{
    FileList, MultiFileBindData, MultiFileOptions, bind_options, create_file_list, parse_paths,
};
use rustdb_function::cast::CastFunctionSet;
use rustdb_function::{TableFunctionBindInput, TableFunctionBindOutput};
use rustdb_functions::strtime::StrpTimeFormat;
use rustdb_planner::ClientContext;
use rustdb_types::{LogicalType, LogicalTypeId};

use crate::json_enums::{JsonFormat, JsonRecordType, JsonScanType};
use crate::json_functions::json_structure::{
    CandidateRefiner, InferenceOptions, StructureNode, extract_structure, merge_nodes, structure_to_type,
};
use crate::json_functions::json_transform::DateFormatMap;
use crate::json_reader::{JsonReader, ScanBatch};
use crate::json_reader_options::JsonReaderOptions;
use crate::json_scan::{JsonScanData, TransformFlags};
use crate::json_type;
use crate::yyjson::Doc;

/// What a JSON table function reads (`JSONScanInfo`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct JsonScanInfo {
    pub(crate) scan_type: JsonScanType,
    pub(crate) format: JsonFormat,
    pub(crate) record_type: JsonRecordType,
    pub(crate) auto_detect: bool,
}

/// The binder's client context of a table function call.
pub(crate) fn client_context<'a>(input: &TableFunctionBindInput<'a>) -> Result<&'a ClientContext> {
    input
        .binder
        .and_then(|binder| binder.downcast_ref::<ClientContext>())
        .ok_or_else(|| Error::internal("read_json bound without a client context"))
}

/// The number of threads of the database (`TaskScheduler::NumberOfThreads`).
pub(crate) fn thread_count(context: &ClientContext) -> u64 {
    context
        .function_client
        .settings()
        .and_then(|settings| settings.current_setting("threads").ok())
        .and_then(|value| value.as_i128())
        .and_then(|threads| u64::try_from(threads).ok())
        .filter(|&threads| threads > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |threads| threads.get() as u64))
}

/// `JSONMultiFileInfo::InitializeOptions`.
fn initialize_options(info: Option<JsonScanInfo>) -> JsonReaderOptions {
    let mut options = JsonReaderOptions::default();
    if let Some(info) = info {
        options.scan_type = info.scan_type;
        options.format = info.format;
        options.record_type = info.record_type;
        options.auto_detect = info.auto_detect;
        if info.scan_type == JsonScanType::ReadJsonObjects {
            options.sql_type_list.push(json_type());
            options.name_list.push("json".into());
        }
    } else {
        // COPY ... FROM
        options.scan_type = JsonScanType::ReadJson;
        options.record_type = JsonRecordType::Records;
        options.format = JsonFormat::AutoDetect;
        options.auto_detect = false;
    }
    options
}

/// The detection templates of dates and timestamps (`InitializeFormats`).
const DATE_TEMPLATES: [&str; 6] = [
    "%m-%d-%Y", "%m-%d-%y", "%d-%m-%Y", "%d-%m-%y", "%Y-%m-%d", "%y-%m-%d",
];
const TIMESTAMP_TEMPLATES: [&str; 12] = [
    "%Y-%m-%d %H:%M:%S.%f",
    "%m-%d-%Y %I:%M:%S %p",
    "%m-%d-%y %I:%M:%S %p",
    "%d-%m-%Y %H:%M:%S",
    "%d-%m-%y %H:%M:%S",
    "%Y-%m-%d %H:%M:%S",
    "%y-%m-%d %H:%M:%S",
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%dT%H:%M:%SZ",
    "%Y-%m-%dT%H:%M:%S.%fZ",
    "%Y-%m-%dT%H:%M:%S%z",
    "%Y-%m-%dT%H:%M:%S.%f%z",
];

fn parse_format(format: &str) -> Result<StrpTimeFormat> {
    StrpTimeFormat::parse(format).map_err(Error::invalid_input)
}

/// `JSONScanData::InitializeFormats`.
fn initialize_formats(options: &JsonReaderOptions) -> Result<DateFormatMap> {
    let mut map = DateFormatMap::default();
    if !options.date_format.is_empty() {
        map.date.push(parse_format(&options.date_format)?);
    }
    if !options.timestamp_format.is_empty() {
        map.timestamp.push(parse_format(&options.timestamp_format)?);
    }
    if options.auto_detect {
        if map.date.is_empty() {
            for template in DATE_TEMPLATES {
                map.date.push(parse_format(template)?);
            }
        }
        if map.timestamp.is_empty() {
            for template in TIMESTAMP_TEMPLATES {
                map.timestamp.push(parse_format(template)?);
            }
        }
    }
    Ok(map)
}

/// `RemoveDuplicateStructKeys`.
fn remove_duplicate_struct_keys(ty: &LogicalType, ignore_errors: bool) -> Result<LogicalType> {
    Ok(match ty.id() {
        LogicalTypeId::Struct => {
            let mut seen = std::collections::HashSet::new();
            let mut children = Vec::new();
            for (name, child) in ty.struct_children().unwrap_or_default() {
                if !seen.insert(name.to_ascii_lowercase()) {
                    if ignore_errors {
                        continue;
                    }
                    return Err(Error::not_implemented(rustdb_common::byte_format!(
                        "Duplicate name \"{name}\" in struct auto-detected in JSON, try ignore_errors=true"
                    )));
                }
                children.push((name.clone(), remove_duplicate_struct_keys(child, ignore_errors)?));
            }
            LogicalType::struct_type(children)
        }
        LogicalTypeId::Map => match ty.map_key_value() {
            Some((key, value)) => LogicalType::map(
                remove_duplicate_struct_keys(key, ignore_errors)?,
                remove_duplicate_struct_keys(value, ignore_errors)?,
            ),
            None => ty.clone(),
        },
        LogicalTypeId::List => LogicalType::list(remove_duplicate_struct_keys(
            ty.child_type().unwrap_or(&LogicalType::SQLNULL),
            ignore_errors,
        )?),
        _ => ty.clone(),
    })
}

/// What schema sampling learned about the files (`AutoDetectState`).
#[derive(Debug, Default)]
struct SampleStats {
    files_scanned: u64,
    tuples_scanned: u64,
    bytes_scanned: u64,
    total_file_size: u64,
}

/// `JSONSchemaTask::ExecuteInternal`: samples one file into `node`.
fn sample_file(
    path: &[u8],
    options: &JsonReaderOptions,
    refiner: &CandidateRefiner<'_>,
    node: &mut StructureNode,
    stats: &mut SampleStats,
) -> Result<()> {
    let mut reader = JsonReader::new(path, options, None);
    reader.initialize()?;
    let file_size = reader.file_size();
    let mut batch = ScanBatch::default();
    let mut remaining = options.sample_size;
    let mut tuples = 0;
    let mut read_size: u64 = 0;
    while remaining != 0 {
        let offset_before = reader.buffer_offset() as u64;
        let read_count = reader.scan(&mut batch)?;
        if read_count == 0 {
            break;
        }
        // Like DuckDB, the progress of the buffer offset (which restarts per buffer).
        read_size = read_size.wrapping_add((reader.buffer_offset() as u64).wrapping_sub(offset_before));
        tuples += read_count as u64;
        let next = (read_count as u64).min(remaining) as usize;
        let doc = Doc::new(&batch.vals, reader.buffer());
        for &val in &batch.values[..next] {
            if let Some(val) = val {
                extract_structure(&doc, val, node, true)?;
            }
        }
        remaining -= next as u64;
        if !node.contains_varchar() {
            continue;
        }
        node.initialize_candidate_types(options.max_depth, options.convert_strings_to_integers, 0);
        node.refine_candidate_types(&doc, &batch.values[..next], refiner)?;
    }
    stats.bytes_scanned = stats.bytes_scanned.wrapping_add(read_size);
    stats.total_file_size += file_size;
    stats.tuples_scanned += tuples;
    stats.files_scanned += 1;
    Ok(())
}

/// `JSONScan::AutoDetect`: samples the files and derives the columns.
#[allow(
    clippy::too_many_arguments,
    reason = "the parts of the bind data AutoDetect fills"
)]
fn auto_detect(
    casts: &CastFunctionSet,
    files: &FileList,
    file_options: &MultiFileOptions,
    options: &mut JsonReaderOptions,
    date_format_map: &DateFormatMap,
    threads: u64,
    return_types: &mut Vec<LogicalType>,
    names: &mut Vec<rustdb_common::ByteString>,
) -> Result<Option<u64>> {
    let file_count = if file_options.union_by_name {
        files.files.len()
    } else {
        files
            .files
            .len()
            .min(usize::try_from(options.maximum_sample_files).unwrap_or(usize::MAX))
    };
    let refiner = CandidateRefiner {
        casts,
        date_formats: date_format_map,
    };
    let threads = usize::try_from(threads.max(1)).unwrap_or(1);
    let files_per_task = file_count.div_ceil(threads).max(1);
    let mut node = StructureNode::default();
    let mut stats = SampleStats::default();
    for task in files.files[..file_count].chunks(files_per_task) {
        let mut task_node = StructureNode::default();
        for path in task {
            sample_file(path, options, &refiner, &mut task_node, &mut stats)?;
        }
        merge_nodes(&mut node, &task_node);
    }
    let mut estimated_cardinality_per_file = None;
    if stats.files_scanned > 0 && stats.tuples_scanned > 0 {
        let average_tuple_size = (stats.bytes_scanned / stats.tuples_scanned).max(1);
        estimated_cardinality_per_file = Some(stats.total_file_size / average_tuple_size);
    }
    let inference = InferenceOptions {
        max_depth: options.max_depth,
        field_appearance_threshold: options.field_appearance_threshold,
        map_inference_threshold: options.map_inference_threshold,
    };
    let ty = structure_to_type(&node, inference, 0, &json_type());
    if options.record_type == JsonRecordType::AutoDetect {
        options.record_type = if ty.id() == LogicalTypeId::Struct {
            JsonRecordType::Records
        } else {
            JsonRecordType::Values
        };
    }
    if !names.is_empty() {
        // COPY: the names and types are the table's.
        return Ok(estimated_cardinality_per_file);
    }
    if options.record_type == JsonRecordType::Records {
        let Some(children) = ty.struct_children().filter(|_| ty.id() == LogicalTypeId::Struct) else {
            return Err(Error::binder(
                "json_read expected records, but got non-record JSON instead.\n Try setting records='auto' or records='false'.",
            ));
        };
        for (name, child) in children {
            return_types.push(remove_duplicate_struct_keys(child, options.ignore_errors)?);
            names.push(name.clone());
        }
    } else {
        return_types.push(remove_duplicate_struct_keys(&ty, options.ignore_errors)?);
        names.push("json".into());
    }
    Ok(estimated_cardinality_per_file)
}

/// `MultiFileBindInternal` with `JSONMultiFileInfo::BindReader`: binds a read of `files`;
/// for `COPY FROM`, `expected` holds the names and types of the target table.
pub(crate) fn bind_json_files(
    context: &ClientContext,
    files: FileList,
    mut file_options: MultiFileOptions,
    mut options: JsonReaderOptions,
    expected: Option<(&[rustdb_common::ByteString], &[LogicalType])>,
) -> Result<JsonScanData> {
    let casts = Arc::clone(&context.casts);
    let threads = thread_count(context);
    file_options.auto_detect_hive_partitioning(&files, &casts)?;
    let mut names = options.name_list.clone();
    let mut types = options.sql_type_list.clone();
    if options.record_type == JsonRecordType::AutoDetect && types.len() > 1 {
        options.record_type = JsonRecordType::Records;
    }
    if !types.is_empty() {
        options.auto_detect = false;
    }
    if !options.auto_detect {
        if types.is_empty() {
            return Err(Error::binder(
                "When auto_detect=false, read_json requires columns to be specified through the \"columns\" parameter.",
            ));
        }
        if options.record_type == JsonRecordType::Values && types.len() != 1 {
            return Err(Error::binder(
                "read_json requires a single column to be specified through the \"columns\" parameter when \"records\" is set to 'false'.",
            ));
        }
    }
    let date_format_map = initialize_formats(&options)?;
    // Pipes are read once, here: sampling and scanning read the same data.
    let mut pipe_contents = HashMap::new();
    for path in &files.files {
        if crate::json_reader::is_pipe(path) && !pipe_contents.contains_key(path) {
            pipe_contents.insert(path.clone(), Arc::from(crate::json_reader::read_pipe(path)?));
        }
    }
    if !pipe_contents.is_empty() {
        options.pipe_contents = crate::json_reader_options::PipeContents(Arc::new(pipe_contents));
    }
    let mut estimated_cardinality_per_file = None;
    if options.auto_detect || options.record_type == JsonRecordType::AutoDetect {
        estimated_cardinality_per_file = auto_detect(
            &casts,
            &files,
            &file_options,
            &mut options,
            &date_format_map,
            threads,
            &mut types,
            &mut names,
        )?;
    }
    let key_names = names.clone();
    let reader_bind: MultiFileBindData = bind_options(&file_options, &files, &mut types, &mut names)?;
    let transform = TransformFlags {
        strict_cast: !options.ignore_errors,
        error_duplicate_key: !options.ignore_errors,
        error_missing_key: false,
        error_unknown_key: options.auto_detect && !options.ignore_errors,
    };
    if options.auto_detect {
        // Rename "id" / "Id" style duplicates (CSV's header detection scheme).
        let mut collisions: HashMap<rustdb_common::ByteString, u64> = HashMap::new();
        for name in &mut names {
            while let Some(count) = collisions.get_mut(&name.to_ascii_lowercase()) {
                *count += 1;
                *name = rustdb_common::byte_format!("{name}_{count}");
            }
            collisions.insert(name.to_ascii_lowercase(), 0);
        }
    }
    if let Some((expected_names, expected_types)) = expected {
        if expected_types.len() != types.len() {
            return Err(Error::conversion(rustdb_common::byte_format!(
                "Failed to read file(s) \"{}\" - column count mismatch: expected {} columns but found {}",
                files.first_file().unwrap_or_default(),
                expected_types.len(),
                types.len()
            )));
        }
        types = expected_types.to_vec();
        let _ = expected_names;
    }
    Ok(JsonScanData {
        files,
        file_options,
        reader_bind,
        options,
        names,
        types,
        key_names,
        date_format_map: Arc::new(date_format_map),
        transform,
        casts,
        estimated_cardinality_per_file,
        threads,
    })
}

/// `MultiFileFunction::MultiFileBind` for the JSON readers.
pub(crate) fn json_bind(
    function_name: &str,
    info: JsonScanInfo,
    input: &TableFunctionBindInput<'_>,
) -> Result<TableFunctionBindOutput> {
    let context = client_context(input)?;
    let first = input
        .inputs
        .first()
        .ok_or_else(|| Error::internal("read_json without a file argument"))?;
    let paths = parse_paths(function_name, first)?;
    let files = create_file_list(function_name, &paths, "json")?;
    let mut file_options = MultiFileOptions::default();
    let mut options = initialize_options(Some(info));
    let parse_type =
        |text: &str| crate::json_functions::json_transform::string_to_logical_type(Some(context), text);
    for (key, value) in input.named_parameters {
        if file_options.parse_option(key, value, &parse_type)? {
            continue;
        }
        if !options.parse_option(Some(context), key, value)? {
            return Err(Error::not_implemented(rustdb_common::byte_format!(
                "Unsupported option for {function_name}: {key}"
            )));
        }
    }
    let data = bind_json_files(context, files, file_options, options, None)?;
    Ok(TableFunctionBindOutput {
        return_types: data.types.clone(),
        names: data.names.clone(),
        bind_data: Some(Arc::new(data)),
    })
}

/// `MultiFileBindCopy`: the reader of `COPY ... FROM ... (FORMAT json)`.
pub(crate) fn json_copy_from_bind(
    context: &ClientContext,
    file_path: &[u8],
    copy_options: &rustdb_common::CaseInsensitiveMap<Vec<rustdb_types::Value>, rustdb_common::ByteString>,
    expected_names: &[rustdb_common::ByteString],
    expected_types: &[LogicalType],
) -> Result<JsonScanData> {
    let files = create_file_list("COPY", &[file_path.into()], "json")?;
    let mut options = initialize_options(None);
    let file_options = MultiFileOptions {
        auto_detect_hive_partitioning: false,
        ..MultiFileOptions::default()
    };
    for (key, values) in copy_options.iter() {
        if options.parse_copy_option(key, values)? {
            continue;
        }
        return Err(Error::not_implemented(rustdb_common::byte_format!(
            "Unsupported option for COPY FROM: {key}"
        )));
    }
    // `FinalizeCopyBind`.
    options.name_list = expected_names.to_vec();
    options.sql_type_list = expected_types.to_vec();
    if options.auto_detect && options.format != JsonFormat::Array {
        options.format = JsonFormat::AutoDetect;
    }
    bind_json_files(
        context,
        files,
        file_options,
        options,
        Some((expected_names, expected_types)),
    )
}
