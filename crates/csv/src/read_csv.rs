//! The `read_csv` / `read_csv_auto` table functions (DuckDB's `read_csv.cpp`,
//! `csv_multi_file_info.cpp`, `global_csv_state.cpp` and the CSV parts of
//! `MultiFileFunction`).

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_function::cast::CastFunctionSet;
use rustdb_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
    TableScanState,
};
use rustdb_parser::{ParserOptions, parse_column_definition};
use rustdb_planner::{ClientContext, bind_logical_type};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value, Vector};

use crate::buffer::CsvBufferManager;
use crate::error::CsvError;
use crate::file_scan::{CsvFileScan, ValidatorLine};
use crate::multi_file::{
    FileExpandResult, FileList, MultiFileBindData, MultiFileOptions, bind_options, combine_union_types,
    create_file_list, hive_parse, multi_file_parameters, parse_paths,
};
use crate::options::{CsvReaderOptions, MultiFileOptionsView, SNIFF_SIZE};
use crate::scanner::boundary::CsvIterator;
use crate::scanner::string_value::{CsvBufferUsage, StringValueScanner};
use crate::schema::CsvSchema;
use crate::sniffer::CsvSniffer;

/// The binder's client context of a table function call.
pub fn client_context<'a>(input: &TableFunctionBindInput<'a>) -> Result<&'a ClientContext> {
    input
        .binder
        .and_then(|binder| binder.downcast_ref::<ClientContext>())
        .ok_or_else(|| Error::internal("read_csv bound without a client context"))
}

/// `TransformStringToLogicalType`: a type name as written in SQL.
pub fn transform_string_to_logical_type(context: &ClientContext, text: &str) -> Result<LogicalType> {
    if text.eq_ignore_ascii_case("null") {
        return Ok(LogicalType::SQLNULL);
    }
    let column = parse_column_definition(&format!("dummy {text}"), &ParserOptions::default())?;
    bind_logical_type(context, &column.logical_type)
}

/// The cached schema of one file of a `union_by_name` read (DuckDB's `CSVUnionData`).
#[derive(Clone, Debug)]
pub struct CsvUnionData {
    pub file: String,
    pub names: Vec<String>,
    pub types: Vec<LogicalType>,
    pub options: CsvReaderOptions,
}

/// The bind data of `read_csv` (DuckDB's `MultiFileBindData` with `ReadCSVData`).
#[derive(Debug)]
pub struct ReadCsvData {
    pub files: FileList,
    pub options: CsvReaderOptions,
    pub file_options: MultiFileOptions,
    /// The output columns, including the generated filename and hive columns.
    pub names: Vec<String>,
    pub types: Vec<LogicalType>,
    pub reader_bind: MultiFileBindData,
    pub csv_schema: CsvSchema,
    pub union_readers: Vec<CsvUnionData>,
    /// The buffers of the first file, read while sniffing.
    buffer_manager: Mutex<Option<Arc<CsvBufferManager>>>,
    pub casts: Arc<CastFunctionSet>,
    /// The number of threads (`threads` setting) deciding single-threaded reads.
    pub threads: u64,
    /// The connection reading, whose temporary catalog keeps the rejects tables.
    pub client: Option<Arc<rustdb_function::FunctionClient>>,
}

impl FunctionData for ReadCsvData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl ReadCsvData {
    fn view(&self) -> MultiFileOptionsView {
        options_view(&self.file_options)
    }

    /// A copy reading `types` (sharing the first file's buffers).
    fn with_types(&self, types: Vec<LogicalType>) -> Self {
        Self {
            files: self.files.clone(),
            options: self.options.clone(),
            file_options: self.file_options.clone(),
            names: self.names.clone(),
            types,
            reader_bind: self.reader_bind.clone(),
            csv_schema: self.csv_schema.clone(),
            union_readers: self.union_readers.clone(),
            buffer_manager: Mutex::new(
                self.buffer_manager
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            ),
            casts: Arc::clone(&self.casts),
            threads: self.threads,
            client: self.client.clone(),
        }
    }
}

impl ReadCsvData {
    /// A copy that reads only the files at positions `keep` (`PruneReaders`).
    fn with_files(&self, keep: &[usize]) -> Self {
        let mut result = self.with_types(self.types.clone());
        let files: Vec<String> = keep
            .iter()
            .filter_map(|&index| self.files.files.get(index).cloned())
            .collect();
        if result.union_readers.len() == self.files.files.len() {
            result.union_readers = keep
                .iter()
                .filter_map(|&index| self.union_readers.get(index).cloned())
                .collect();
        } else {
            result.union_readers.retain(|reader| files.contains(&reader.file));
        }
        result.files = FileList { files };
        result
    }
}

/// The file filter pushdown of `read_csv` (`MultiFileComplexFilterPushdown`).
fn read_csv_file_filter_pushdown() -> rustdb_function::FileFilterPushdown {
    rustdb_function::FileFilterPushdown {
        files: Arc::new(|bind_data| {
            let data = bind_data.as_any().downcast_ref::<ReadCsvData>()?;
            (data.file_options.hive_partitioning || data.file_options.filename)
                .then(|| data.files.files.clone())
        }),
        known_value: Arc::new(|bind_data, path, name, ty| {
            let Some(data) = bind_data.as_any().downcast_ref::<ReadCsvData>() else {
                return Ok(None);
            };
            crate::multi_file::known_column_value(&data.file_options, &data.casts, path, name, ty)
        }),
        with_files: Arc::new(|bind_data, keep| {
            let data = bind_data
                .as_any()
                .downcast_ref::<ReadCsvData>()
                .ok_or_else(|| Error::internal("read_csv file pruning without bind data"))?;
            Ok(Arc::new(data.with_files(keep)) as Arc<dyn FunctionData>)
        }),
    }
}

/// `MultiFileFunction::PushdownType`: reads columns as other types.
fn read_csv_type_pushdown(
    bind_data: &dyn FunctionData,
    new_types: &[(usize, LogicalType)],
) -> Result<Arc<dyn FunctionData>> {
    let data = bind_data
        .as_any()
        .downcast_ref::<ReadCsvData>()
        .ok_or_else(|| Error::internal("read_csv type pushdown without bind data"))?;
    let mut types = data.types.clone();
    for (column, ty) in new_types {
        if let Some(slot) = types.get_mut(*column) {
            slot.clone_from(ty);
        }
    }
    Ok(Arc::new(data.with_types(types)))
}

fn options_view(file_options: &MultiFileOptions) -> MultiFileOptionsView {
    MultiFileOptionsView {
        union_by_name: file_options.union_by_name,
        hive_partitioning: file_options.hive_partitioning,
        filename: file_options.filename,
    }
}

/// `CSVSchemaDiscovery::SchemaDiscovery`: sniffs the first file(s) for the schema.
fn schema_discovery(
    casts: &Arc<CastFunctionSet>,
    options: &mut CsvReaderOptions,
    file_options: &MultiFileOptionsView,
    return_types: &mut Vec<LogicalType>,
    names: &mut Vec<String>,
    files: &FileList,
) -> Result<(CsvSchema, Arc<CsvBufferManager>)> {
    let mut schemas = Vec::new();
    let option_og = options.clone();
    let required_number_of_lines = SNIFF_SIZE.wrapping_mul(options.sample_size_chunks);
    let mut total_number_of_rows: u64 = 0;
    let mut current_file = 0;
    let first = files.files[0].clone();
    options.file_path.clone_from(&first);
    let buffer_manager = Arc::new(CsvBufferManager::new(options, &first)?);
    let mut only_header_or_empty_files = 0;
    {
        let mut sniffer = CsvSniffer::new(
            options,
            file_options,
            Arc::clone(&buffer_manager),
            Arc::clone(casts),
            true,
        );
        let result = sniffer.sniff_csv(false)?;
        let lines_sniffed = sniffer.lines_sniffed();
        let empty_or_only_header = sniffer.empty_or_only_header();
        drop(sniffer);
        let rows_read = lines_sniffed.wrapping_sub(
            options.dialect_options.skip_rows.value() + u64::from(options.dialect_options.header.value()),
        );
        let empty = buffer_manager
            .get_buffer(0)?
            .is_none_or(|buffer| buffer.actual_size() == 0);
        schemas.push(CsvSchema::new(
            &result.names,
            &result.return_types,
            &first,
            rows_read,
            empty,
        ));
        total_number_of_rows += lines_sniffed;
        current_file += 1;
        if empty_or_only_header {
            only_header_or_empty_files += 1;
        }
    }
    let max_files_to_sniff = if options.files_to_sniff == -1 {
        usize::MAX
    } else {
        usize::try_from(options.files_to_sniff).unwrap_or(0)
    };
    let files_to_sniff = files.files.len().min(max_files_to_sniff);
    while total_number_of_rows < required_number_of_lines && current_file < files_to_sniff {
        let mut option_copy = option_og.clone();
        option_copy.file_path.clone_from(&files.files[current_file]);
        let file_buffer_manager = Arc::new(CsvBufferManager::new(&option_copy, &files.files[current_file])?);
        let path = option_copy.file_path.clone();
        let mut sniffer = CsvSniffer::new(
            &mut option_copy,
            file_options,
            Arc::clone(&file_buffer_manager),
            Arc::clone(casts),
            true,
        );
        let result = sniffer.sniff_csv(false)?;
        let lines_sniffed = sniffer.lines_sniffed();
        let empty_or_only_header = sniffer.empty_or_only_header();
        drop(sniffer);
        let rows_read = lines_sniffed.wrapping_sub(
            option_copy.dialect_options.skip_rows.value()
                + u64::from(option_copy.dialect_options.header.value()),
        );
        if file_buffer_manager
            .get_buffer(0)?
            .is_none_or(|buffer| buffer.actual_size() == 0)
        {
            schemas.push(CsvSchema::new(&[], &[], "", 0, true));
        } else {
            schemas.push(CsvSchema::new(
                &result.names,
                &result.return_types,
                &path,
                rows_read,
                false,
            ));
        }
        total_number_of_rows += lines_sniffed;
        if empty_or_only_header {
            only_header_or_empty_files += 1;
        }
        current_file += 1;
    }
    let mut best_schema = CsvSchema::default();
    for schema in &schemas {
        if best_schema.is_empty() || best_schema.rows_read() == 0 {
            best_schema = schema.clone();
        } else if schema.rows_read() != 0 {
            best_schema.merge_schemas(schema, options.null_padding);
        }
    }
    best_schema.replace_null_with_varchar();
    if names.is_empty() {
        *names = best_schema.names();
        *return_types = best_schema.types();
    }
    if only_header_or_empty_files == current_file && !options.columns_set {
        for i in 0..return_types.len() {
            if options.sql_types_per_column.is_empty() {
                if i < options.sql_type_list.len() {
                    continue;
                }
            } else if options.sql_types_per_column.contains_key(&names[i]) {
                continue;
            }
            return_types[i] = LogicalType::VARCHAR;
        }
    }
    Ok((best_schema, buffer_manager))
}

/// `MultiFileBindInternal` with `CSVMultiFileInfo`: binds a read of `files`; for
/// `COPY FROM`, `expected` holds the names and types of the target table.
pub fn bind_csv_files(
    casts: &Arc<CastFunctionSet>,
    files: FileList,
    mut file_options: MultiFileOptions,
    mut options: CsvReaderOptions,
    expected: Option<(&[String], &[LogicalType])>,
) -> Result<ReadCsvData> {
    // InitializeBindData
    if files.expand_result() == FileExpandResult::MultipleFiles {
        options.multi_file_reader = true;
    }
    options.verify(&options_view(&file_options))?;
    file_options.auto_detect_hive_partitioning(&files, casts)?;
    let view = options_view(&file_options);
    let mut names = Vec::new();
    let mut types = Vec::new();
    let mut csv_schema = CsvSchema::default();
    let mut union_readers = Vec::new();
    let mut first_buffer_manager = None;
    let reader_bind;
    if file_options.union_by_name {
        let mut union_names_map = HashMap::new();
        for (file_idx, file) in files.files.iter().enumerate() {
            let scan =
                CsvFileScan::sniffed(Arc::clone(casts), file, file_idx as u64, options.clone(), &view)?;
            combine_union_types(
                &scan.names,
                &scan.types,
                &mut types,
                &mut names,
                &mut union_names_map,
            );
            let mut reader_options = scan.options.clone();
            reader_options.auto_detect = false;
            union_readers.push(CsvUnionData {
                file: file.clone(),
                names: scan.names.clone(),
                types: scan.types.clone(),
                options: reader_options,
            });
        }
        reader_bind = bind_options(&file_options, &files, &mut types, &mut names)?;
        if !options.sql_types_per_column.is_empty() {
            let names_in_types: Vec<String> = options
                .sql_types_per_column
                .iter()
                .map(|(name, _)| name.to_owned())
                .collect();
            let error = CsvError::column_types_error(names_in_types, &names);
            if !error.error_message.is_empty() {
                return Err(Error::binder(error.error_message));
            }
            for (i, name) in names.iter().enumerate() {
                if let Some(&index) = options.sql_types_per_column.get(name) {
                    types[i] = options.sql_type_list[index].clone();
                }
            }
        }
    } else {
        if options.auto_detect {
            let (schema, buffer_manager) =
                schema_discovery(casts, &mut options, &view, &mut types, &mut names, &files)?;
            csv_schema = schema;
            first_buffer_manager = Some(buffer_manager);
        } else {
            if !options.columns_set {
                return Err(Error::binder(
                    "read_csv requires columns to be specified through the 'columns' option. Use read_csv_auto or set read_csv(..., AUTO_DETECT=TRUE) to automatically guess columns.",
                ));
            }
            names.clone_from(&options.name_list);
            types.clone_from(&options.sql_type_list);
        }
        options.dialect_options.num_cols = names.len() as u64;
        reader_bind = bind_options(&file_options, &files, &mut types, &mut names)?;
    }
    // FinalizeBindData
    if !options.force_not_null_names.is_empty() {
        for force_name in &options.force_not_null_names {
            if !names.contains(force_name) {
                return Err(Error::binder(format!(
                    "\"force_not_null\" expected to find {force_name}, but it was not found in the table"
                )));
            }
        }
        options.force_not_null = names
            .iter()
            .map(|name| options.force_not_null_names.contains(name))
            .collect();
    }
    for ty in &mut types {
        if ty.id() == LogicalTypeId::SqlNull {
            *ty = LogicalType::VARCHAR;
        }
    }
    options.finalize()?;
    if let Some((expected_names, expected_types)) = expected {
        if expected_types.len() != types.len() {
            return Err(Error::conversion(format!(
                "Failed to read file(s) \"{}\" - column count mismatch: expected {} columns but found {}",
                files.first_file().unwrap_or_default(),
                expected_types.len(),
                types.len()
            )));
        }
        types = expected_types.to_vec();
        let _ = expected_names;
    }
    Ok(ReadCsvData {
        files,
        options,
        file_options,
        names,
        types,
        reader_bind,
        csv_schema,
        union_readers,
        buffer_manager: Mutex::new(first_buffer_manager),
        casts: Arc::clone(casts),
        threads: std::thread::available_parallelism().map_or(1, |threads| threads.get() as u64),
        client: rustdb_function::file_system::opener(),
    })
}

/// `MultiFileFunction::MultiFileBind` for `read_csv`.
fn read_csv_bind(function_name: &str, input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let context = client_context(input)?;
    let parse_type = |text: &str| transform_string_to_logical_type(context, text);
    let first = input
        .inputs
        .first()
        .ok_or_else(|| Error::internal("read_csv without a file argument"))?;
    let paths = parse_paths(function_name, first)?;
    let files = create_file_list(function_name, &paths, "csv")?;
    let mut file_options = MultiFileOptions::default();
    let mut options = CsvReaderOptions::default();
    for (key, value) in input.named_parameters {
        if file_options.parse_option(key, value, &parse_type)? {
            continue;
        }
        options.parse_option(key, value, &parse_type)?;
    }
    let data = bind_csv_files(&context.casts, files, file_options, options, None)?;
    Ok(TableFunctionBindOutput {
        return_types: data.types.clone(),
        names: data.names.clone(),
        bind_data: Some(Arc::new(data)),
    })
}

/// Where a projected output column comes from.
#[derive(Debug)]
enum ColumnSource {
    /// The reader's output column at this index.
    Reader(usize),
    Constant(Value),
}

/// The scan of the current file (a `CSVFileScan` with its column mapping and the
/// boundary state of `CSVGlobalState`).
struct FileState {
    file_scan: Arc<CsvFileScan>,
    sources: Vec<ColumnSource>,
    reader_types: Vec<LogicalType>,
    boundary: CsvIterator,
    initialized: bool,
    buffer_in_use: Option<Arc<CsvBufferUsage>>,
    scanner: Option<StringValueScanner>,
}

/// The state of a `read_csv` scan (DuckDB's `CSVGlobalState` with the multi-file
/// global state, scanned by one thread).
struct CsvScanState {
    file_idx: usize,
    current: Option<FileState>,
    scanner_idx: u64,
    single_threaded: bool,
    /// The rejects tables with `store_rejects`.
    rejects: Option<Arc<crate::rejects::CsvRejectsTable>>,
    /// The rejects file index of each file read so far (`rejects_file_indexes`).
    rejects_file_indexes: Vec<u64>,
}

/// `MultiFileColumnMapper::ThrowColumnNotFoundError`.
fn column_not_found(data: &ReadCsvData, file: &CsvFileScan, name: &str) -> Error {
    Error::invalid_input(format!(
        "Failed to read file \"{}\": schema mismatch in glob: column \"{name}\" was read from the original file \"{}\", but could not be found in file \"{}\".\nCandidate names: {}\nIf you are trying to read files with different schemas, try setting union_by_name=True",
        file.file_path,
        data.files.first_file().unwrap_or_default(),
        file.file_path,
        file.names.join(", ")
    ))
}

/// `CreateReader` + `CreateMapping` + `PrepareReader` for one file.
fn open_file(data: &ReadCsvData, file_idx: usize, column_ids: &[u64]) -> Result<FileState> {
    let file = &data.files.files[file_idx];
    let view = data.view();
    let mut file_scan = if data.file_options.union_by_name {
        let union_data = &data.union_readers[file_idx];
        CsvFileScan::new(
            Arc::clone(&data.casts),
            &union_data.file,
            file_idx as u64,
            union_data.options.clone(),
            &view,
            &union_data.names,
            &union_data.types,
            &data.csv_schema,
            None,
            false,
        )?
    } else {
        let mut options = data.options.clone();
        if data.files.expand_result() == FileExpandResult::SingleFile {
            options.auto_detect = false;
        }
        let mut buffer_manager = None;
        if file_idx == 0 {
            buffer_manager = data
                .buffer_manager
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .filter(|manager| manager.file_path() == file);
        }
        let csv_count = usize::try_from(data.options.dialect_options.num_cols).unwrap_or(usize::MAX);
        CsvFileScan::new(
            Arc::clone(&data.casts),
            file,
            file_idx as u64,
            options,
            &view,
            &data.names[..csv_count.min(data.names.len())],
            &data.types[..csv_count.min(data.types.len())],
            &data.csv_schema,
            buffer_manager,
            false,
        )?
    };
    // CreateMapping: by name, the reader converting types itself (`UseCastMap`).
    let mut sources = Vec::with_capacity(column_ids.len());
    let mut reader_types = Vec::new();
    let hive_partitions = if data.reader_bind.hive_partitioning_indexes.is_empty() {
        std::collections::BTreeMap::default()
    } else {
        hive_parse(file)
    };
    for &global_id in column_ids {
        let global = global_id as usize;
        if global >= data.names.len() {
            sources.push(ColumnSource::Constant(Value::null(LogicalType::BOOLEAN)));
            continue;
        }
        if data.reader_bind.filename_idx == Some(global) {
            sources.push(ColumnSource::Constant(Value::varchar(file)));
            continue;
        }
        if let Some((key, _)) = data
            .reader_bind
            .hive_partitioning_indexes
            .iter()
            .find(|(_, index)| *index == global)
        {
            let value = hive_partitions.get(key).cloned().unwrap_or_default();
            let constant = data.file_options.hive_partition_value(&value, key, &data.casts)?;
            sources.push(ColumnSource::Constant(constant));
            continue;
        }
        let name = &data.names[global];
        let global_type = &data.types[global];
        let Some(local) = file_scan
            .names
            .iter()
            .position(|local| local.eq_ignore_ascii_case(name))
        else {
            if data.file_options.union_by_name {
                sources.push(ColumnSource::Constant(Value::null(global_type.clone())));
                continue;
            }
            return Err(column_not_found(data, &file_scan, name));
        };
        let local_type = file_scan
            .types
            .get(local)
            .cloned()
            .unwrap_or(LogicalType::VARCHAR);
        if *global_type != local_type {
            file_scan.cast_map.insert(local as u64, global_type.clone());
        }
        sources.push(ColumnSource::Reader(reader_types.len()));
        reader_types.push(global_type.clone());
        file_scan.column_ids.push(local as u64);
    }
    // PrepareReader
    file_scan.initialize_file_names_types();
    file_scan.set_start()?;
    Ok(FileState {
        file_scan: Arc::new(file_scan),
        sources,
        reader_types,
        boundary: CsvIterator::default(),
        initialized: false,
        buffer_in_use: None,
        scanner: None,
    })
}

/// `CSVGlobalState::Next`: the scanner of the next boundary of the file.
fn next_scanner(state: &mut CsvScanState, file: &mut FileState) -> Result<Option<StringValueScanner>> {
    let file_scan = &file.file_scan;
    if file.initialized {
        if file.boundary.done
            || !file
                .boundary
                .next(&file_scan.buffer_manager, &file_scan.options)?
        {
            return Ok(None);
        }
    } else {
        file.boundary = file_scan.start_iterator;
        file.boundary
            .set_current_boundary_to_position(state.single_threaded, &file_scan.options);
        file.buffer_in_use = Some(Arc::new(CsvBufferUsage {
            buffer_manager: Arc::clone(&file_scan.buffer_manager),
            buffer_idx: file.boundary.buffer_idx(),
        }));
        file.initialized = true;
    }
    if file
        .buffer_in_use
        .as_ref()
        .is_none_or(|usage| usage.buffer_idx != file.boundary.buffer_idx())
    {
        file.buffer_in_use = Some(Arc::new(CsvBufferUsage {
            buffer_manager: Arc::clone(&file_scan.buffer_manager),
            buffer_idx: file.boundary.buffer_idx(),
        }));
    }
    let scanner_idx = state.scanner_idx;
    state.scanner_idx += 1;
    let mut scanner = StringValueScanner::new(
        scanner_idx,
        Arc::clone(&file_scan.buffer_manager),
        Arc::clone(&file_scan.state_machine),
        Arc::clone(&file_scan.error_handler),
        Some(Arc::clone(file_scan)),
        false,
        file.boundary,
        rustdb_common::STANDARD_VECTOR_SIZE as u64,
    )?;
    scanner.buffer_tracker.clone_from(&file.buffer_in_use);
    Ok(Some(scanner))
}

/// `CSVGlobalState::FinishScan`.
fn finish_scan(scanner: StringValueScanner) {
    if let Some(file_scan) = &scanner.csv_file_scan {
        let (start_pos, end_pos) = scanner.validation_line();
        file_scan
            .validator
            .insert(scanner.scanner_idx, ValidatorLine { start_pos, end_pos });
    }
    drop(scanner);
}

/// `CSVGlobalState::FinishFile`.
fn finish_file(data: &ReadCsvData, state: &mut CsvScanState, file: &FileState) -> Result<()> {
    let ignore_or_store_errors = data.options.ignore_errors.value() || data.options.store_rejects.value();
    if !state.single_threaded && !ignore_or_store_errors {
        file.file_scan.validator.verify()?;
    }
    file.file_scan.error_handler.error_if_any()?;
    fill_rejects_table(data, state, &file.file_scan)
}

/// `CSVGlobalState::FillRejectsTable`.
fn fill_rejects_table(data: &ReadCsvData, state: &mut CsvScanState, file: &CsvFileScan) -> Result<()> {
    let (Some(rejects), Some(client)) = (&state.rejects, &data.client) else {
        return Ok(());
    };
    let scan_idx = client.query_id();
    // File indexes are assigned in file order, whatever order the files finish in.
    let file_idx = file.file_idx as usize;
    while state.rejects_file_indexes.len() <= file_idx {
        state.rejects_file_indexes.push(rejects.current_file_index(scan_idx));
    }
    rejects.fill(
        client,
        file,
        (scan_idx, state.rejects_file_indexes[file_idx]),
        &data.names,
        data.options.rejects_limit,
    )
}

/// The `read_csv` scan: the next chunk of every output column.
fn read_csv_scan(
    bind_data: Option<&dyn FunctionData>,
    column_ids: &[u64],
    scan_state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<ReadCsvData>())
        .ok_or_else(|| Error::internal("read_csv scan without bind data"))?;
    if scan_state.local.is_none() {
        let file_count = data.files.files.len() as u64;
        let many_csv_files = file_count > 1 && file_count > data.threads * 2;
        // `CSVMultiFileInfo::InitializeGlobalState`: the rejects tables are created.
        let rejects = match (&data.client, data.options.store_rejects.value()) {
            (Some(client), true) => {
                let rejects = crate::rejects::CsvRejectsTable::get_or_create(
                    client,
                    data.options.rejects_scan_name.get(),
                    data.options.rejects_table_name.get(),
                )?;
                rejects.initialize_table(client)?;
                Some(rejects)
            }
            _ => None,
        };
        scan_state.local = Some(Box::new(CsvScanState {
            file_idx: 0,
            current: None,
            scanner_idx: 0,
            single_threaded: many_csv_files || !data.options.parallel,
            rejects,
            rejects_file_indexes: Vec::new(),
        }));
    }
    let state = scan_state
        .local
        .as_mut()
        .and_then(|local| local.downcast_mut::<CsvScanState>())
        .ok_or_else(|| Error::internal("read_csv scan state"))?;
    loop {
        let mut file = if let Some(file) = state.current.take() {
            file
        } else {
            if state.file_idx >= data.files.files.len() {
                *chunk = DataChunk::new(
                    data.types
                        .iter()
                        .map(|ty| Vector::constant_null(ty.clone()))
                        .collect(),
                    0,
                );
                return Ok(false);
            }
            open_file(data, state.file_idx, column_ids)?
        };
        if file.scanner.is_none() {
            if let Some(scanner) = next_scanner(state, &mut file)? {
                file.scanner = Some(scanner);
            } else {
                file.buffer_in_use = None;
                finish_file(data, state, &file)?;
                state.file_idx += 1;
                continue;
            }
        }
        let Some(scanner) = file.scanner.as_mut() else {
            continue;
        };
        if scanner.finished_iterator() {
            if let Some(scanner) = file.scanner.take() {
                finish_scan(scanner);
            }
            state.current = Some(file);
            continue;
        }
        let result = scanner.flush(&file.reader_types)?;
        let Some(reader_chunk) = result.filter(|chunk| !chunk.is_empty()) else {
            if let Some(scanner) = file.scanner.take() {
                finish_scan(scanner);
            }
            state.current = Some(file);
            continue;
        };
        let rows = reader_chunk.len();
        let reader_columns = reader_chunk.into_columns();
        let mut columns: Vec<Option<Vector>> = vec![None; data.types.len()];
        for (position, &global_id) in column_ids.iter().enumerate() {
            let global = global_id as usize;
            if global >= columns.len() || columns[global].is_some() {
                continue;
            }
            columns[global] = Some(match &file.sources[position] {
                ColumnSource::Reader(index) => reader_columns[*index].clone(),
                ColumnSource::Constant(value) => Vector::constant(value)?,
            });
        }
        *chunk = DataChunk::new(
            columns
                .into_iter()
                .zip(&data.types)
                .map(|(column, ty)| column.unwrap_or_else(|| Vector::constant_null(ty.clone())))
                .collect(),
            rows,
        );
        state.current = Some(file);
        return Ok(true);
    }
}

/// The named parameters of `read_csv` (`ReadCSVAddNamedParameters`).
pub fn read_csv_parameters() -> Vec<(String, LogicalType)> {
    let varchar = LogicalType::VARCHAR;
    let any = LogicalType::ANY;
    let boolean = LogicalType::BOOLEAN;
    let bigint = LogicalType::BIGINT;
    let varchar_list = LogicalType::list(LogicalType::VARCHAR);
    let mut parameters: Vec<(String, LogicalType)> = [
        ("sep", varchar.clone()),
        ("delim", varchar.clone()),
        ("quote", varchar.clone()),
        ("new_line", varchar.clone()),
        ("escape", varchar.clone()),
        ("nullstr", any.clone()),
        ("columns", any.clone()),
        ("auto_type_candidates", any.clone()),
        ("header", boolean.clone()),
        ("auto_detect", boolean.clone()),
        ("sample_size", bigint.clone()),
        ("all_varchar", boolean.clone()),
        ("dateformat", varchar.clone()),
        ("timestampformat", varchar.clone()),
        ("normalize_names", boolean.clone()),
        ("compression", varchar.clone()),
        ("skip", bigint.clone()),
        ("max_line_size", varchar.clone()),
        ("maximum_line_size", varchar.clone()),
        ("ignore_errors", boolean.clone()),
        ("store_rejects", boolean.clone()),
        ("rejects_table", varchar.clone()),
        ("rejects_scan", varchar.clone()),
        ("rejects_limit", bigint.clone()),
        ("force_not_null", varchar_list.clone()),
        ("buffer_size", LogicalType::UBIGINT),
        ("decimal_separator", varchar.clone()),
        ("parallel", boolean.clone()),
        ("null_padding", boolean.clone()),
        ("allow_quoted_nulls", boolean.clone()),
        ("column_types", any.clone()),
        ("dtypes", any.clone()),
        ("types", any),
        ("names", varchar_list.clone()),
        ("column_names", varchar_list),
        ("comment", varchar.clone()),
        ("encoding", varchar.clone()),
        ("strict_mode", boolean),
        ("thousands", varchar),
        ("files_to_sniff", bigint),
    ]
    .into_iter()
    .map(|(name, ty)| (name.to_owned(), ty))
    .collect();
    parameters.extend(multi_file_parameters());
    parameters
}

/// One overload of `read_csv` / `read_csv_auto` (`MultiFileReader::CreateFunctionSet`
/// adds the VARCHAR and LIST(VARCHAR) overloads).
fn read_csv_function(name: &'static str, argument: LogicalType) -> TableFunction {
    let mut function = TableFunction::new(name, vec![argument])
        .with_bind(move |input| read_csv_bind(name, input))
        .with_scan(read_csv_scan);
    function.named_parameters = read_csv_parameters();
    function.projection_pushdown = true;
    function.empty_column = true;
    function.type_pushdown = Some(Arc::new(read_csv_type_pushdown));
    function.file_filter_pushdown = Some(read_csv_file_filter_pushdown());
    function
}

/// `read_csv` and `read_csv_auto`.
pub fn read_csv_functions() -> Vec<TableFunctionSet> {
    ["read_csv", "read_csv_auto"]
        .into_iter()
        .map(|name| {
            let mut set = TableFunctionSet::new(name);
            set.add(read_csv_function(name, LogicalType::VARCHAR));
            set.add(read_csv_function(name, LogicalType::list(LogicalType::VARCHAR)));
            set
        })
        .collect()
}
