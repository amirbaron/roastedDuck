//! The `read_parquet` / `parquet_scan` table function (DuckDB's `ParquetScanFunction`,
//! `ParquetMultiFileInfo` and the parts of `MultiFileFunction` / `MultiFileColumnMapper`
//! it relies on): binding the files' schema, mapping each file's columns onto it and
//! scanning row groups.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_csv::multi_file::{
    FileExpandResult, FileList, MultiFileBindData, MultiFileOptions, bind_options, combine_union_types,
    create_file_list, hive_parse, multi_file_parameters, parse_paths,
};
use rustdb_function::cast::{CastFunctionSet, CastParameters};
use rustdb_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet, TableScanState,
};
use rustdb_planner::ClientContext;
use rustdb_storage::TableFilterSet;
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use crate::reader::{ColumnDefinition, ParquetReader, ReaderScanState, ScanColumn};
use crate::schema::ParquetOptions;

/// The binder's client context of a table function call.
pub(crate) fn client_context<'a>(input: &TableFunctionBindInput<'a>) -> Result<&'a ClientContext> {
    input
        .binder
        .and_then(|binder| binder.downcast_ref::<ClientContext>())
        .ok_or_else(|| Error::internal("parquet function bound without a client context"))
}

/// The current value of a boolean setting (`ClientContext::TryGetCurrentSetting`).
pub(crate) fn setting_bool(context: &ClientContext, name: &str) -> bool {
    context
        .function_client
        .settings()
        .and_then(|settings| settings.current_setting(name).ok())
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// The bind data of `read_parquet` (`MultiFileBindData` with `ParquetReadBindData`).
#[derive(Debug)]
pub struct ParquetBindData {
    pub files: FileList,
    pub names: Vec<String>,
    pub types: Vec<LogicalType>,
    pub file_options: MultiFileOptions,
    reader_bind: MultiFileBindData,
    pub options: ParquetOptions,
    /// The index of the `file_row_number` column when the option is set.
    file_row_number_idx: Option<usize>,
    /// The reader of the first file, opened while binding (`initial_reader`).
    initial_reader: Mutex<Option<Arc<ParquetReader>>>,
    casts: Arc<CastFunctionSet>,
    /// The filters pushed into the scan, keyed by position in the scanned column ids.
    filters: Option<Arc<TableFilterSet>>,
    /// The number of rows of the first file (`initial_file_cardinality`).
    pub initial_file_cardinality: u64,
}

impl FunctionData for ParquetBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl ParquetBindData {
    /// A copy that applies `filters` (`MultiFileFunction` with pushed-down table filters).
    fn with_filters(&self, filters: TableFilterSet) -> Self {
        Self {
            files: self.files.clone(),
            names: self.names.clone(),
            types: self.types.clone(),
            file_options: self.file_options.clone(),
            reader_bind: self.reader_bind.clone(),
            options: self.options.clone(),
            file_row_number_idx: self.file_row_number_idx,
            initial_reader: Mutex::new(
                self.initial_reader
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            ),
            casts: Arc::clone(&self.casts),
            filters: Some(Arc::new(filters)),
            initial_file_cardinality: self.initial_file_cardinality,
        }
    }

    /// The reader of file `index`, reusing the one opened while binding.
    fn open_reader(&self, index: usize) -> Result<Arc<ParquetReader>> {
        let path = &self.files.files[index];
        if index == 0
            && let Some(reader) = self
                .initial_reader
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            && reader.file_name() == path
        {
            return Ok(Arc::clone(reader));
        }
        Ok(Arc::new(ParquetReader::open(path, self.options.clone())?))
    }
}

/// `ParquetMultiFileInfo::ParseOption` and `MultiFileReader::ParseOption`.
fn parse_options(
    input: &TableFunctionBindInput<'_>,
    file_options: &mut MultiFileOptions,
    options: &mut ParquetOptions,
    context: &ClientContext,
) -> Result<()> {
    let parse_type = |text: &str| rustdb_csv::read_csv::transform_string_to_logical_type(context, text);
    for (key, value) in input.named_parameters {
        if file_options.parse_option(key, value, &parse_type)? {
            continue;
        }
        let lower = key.to_lowercase();
        if value.is_null() {
            return Err(Error::binder(format!("Cannot use NULL as argument to {key}")));
        }
        match lower.as_str() {
            "binary_as_string" => options.binary_as_string = value.as_bool().unwrap_or(false),
            "file_row_number" => options.file_row_number = value.as_bool().unwrap_or(false),
            "can_have_nan" => options.can_have_nan = value.as_bool().unwrap_or(false),
            "explicit_cardinality" => {
                options.explicit_cardinality = value.as_u64().unwrap_or(0);
            }
            "compression" | "debug_use_openssl" => {}
            "schema" => {
                return Err(Error::not_implemented("the read_parquet schema parameter"));
            }
            "encryption_config" => {
                return Err(Error::not_implemented("Parquet encryption"));
            }
            _ => {}
        }
    }
    Ok(())
}

/// `MultiFileFunction::MultiFileBind` with `ParquetMultiFileInfo`.
fn read_parquet_bind(function_name: &str, input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let context = client_context(input)?;
    let first = input
        .inputs
        .first()
        .ok_or_else(|| Error::internal("read_parquet without a file argument"))?;
    let paths = parse_paths(function_name, first)?;
    let files = create_file_list(function_name, &paths, "parquet")?;
    let mut file_options = MultiFileOptions::default();
    let mut options = ParquetOptions {
        binary_as_string: setting_bool(context, "binary_as_string"),
        ..ParquetOptions::default()
    };
    parse_options(input, &mut file_options, &mut options, context)?;
    let data = bind_files(&context.casts, files, file_options, options)?;
    Ok(TableFunctionBindOutput {
        return_types: data.types.clone(),
        names: data.names.clone(),
        bind_data: Some(Arc::new(data)),
    })
}

/// `MultiFileBindInternal` / `BindReader` for a list of Parquet files.
pub fn bind_files(
    casts: &Arc<CastFunctionSet>,
    files: FileList,
    mut file_options: MultiFileOptions,
    options: ParquetOptions,
) -> Result<ParquetBindData> {
    file_options.auto_detect_hive_partitioning(&files, casts)?;
    let mut names = Vec::new();
    let mut types = Vec::new();
    let mut initial_reader = None;
    let mut initial_file_cardinality = 0;
    // The file_row_number column is added by the bind, not by the file schemas.
    let mut file_options_reader = options.clone();
    file_options_reader.file_row_number = false;
    if file_options.union_by_name {
        let mut union_names_map = HashMap::new();
        for (index, path) in files.files.iter().enumerate() {
            let reader = ParquetReader::open(path, file_options_reader.clone())?;
            let file_names: Vec<String> = reader.columns.iter().map(|column| column.name.clone()).collect();
            let file_types: Vec<LogicalType> = reader
                .columns
                .iter()
                .map(|column| column.logical_type.clone())
                .collect();
            combine_union_types(&file_names, &file_types, &mut types, &mut names, &mut union_names_map);
            if index == 0 {
                initial_file_cardinality = reader.num_rows();
                initial_reader = Some(Arc::new(reader));
            }
        }
    } else {
        let first = files
            .first_file()
            .ok_or_else(|| Error::internal("read_parquet without files"))?;
        let reader = ParquetReader::open(first, file_options_reader.clone())?;
        for column in &reader.columns {
            names.push(column.name.clone());
            types.push(column.logical_type.clone());
        }
        initial_file_cardinality = reader.num_rows();
        initial_reader = Some(Arc::new(reader));
    }
    let mut file_row_number_idx = None;
    if options.file_row_number {
        if names.iter().any(|name| name.eq_ignore_ascii_case("file_row_number")) {
            return Err(Error::binder(
                "Using file_row_number option on file with column named file_row_number is not supported",
            ));
        }
        file_row_number_idx = Some(names.len());
        names.push("file_row_number".to_owned());
        types.push(LogicalType::BIGINT);
    }
    let reader_bind = bind_options(&file_options, &files, &mut types, &mut names)?;
    Ok(ParquetBindData {
        files,
        names,
        types,
        file_options,
        reader_bind,
        options: file_options_reader,
        file_row_number_idx,
        initial_reader: Mutex::new(initial_reader),
        casts: Arc::clone(casts),
        filters: None,
        initial_file_cardinality,
    })
}

/// Where an output column of a file's scan comes from (the result of
/// `MultiFileColumnMapper::CreateMapping`).
#[derive(Debug)]
enum ColumnSource {
    /// Output `index` of the reader scan, cast to the global type if needed.
    Reader {
        index: usize,
        cast: Option<rustdb_function::cast::BoundCastInfo>,
        target: LogicalType,
    },
    /// The same value for every row (filename, hive partitions, missing columns).
    Constant(Value),
}

/// The scan of one file.
#[derive(Debug)]
struct FileScan {
    state: ReaderScanState,
    sources: Vec<ColumnSource>,
    /// Whether no column is read from the file (only its row count matters).
    rows_only: Option<RowsOnly>,
}

/// Emitting the row count of a file without reading any column.
#[derive(Debug)]
struct RowsOnly {
    remaining: u64,
}

/// The state of a `read_parquet` scan.
#[derive(Debug, Default)]
struct ScanState {
    file_index: usize,
    current: Option<FileScan>,
    scan_output: Vec<Vector>,
}

/// `MultiFileColumnMapper::ThrowColumnNotFoundError`.
fn column_not_found(data: &ParquetBindData, reader: &ParquetReader, name: &str) -> Error {
    let candidates: Vec<&str> = reader.columns.iter().map(|column| column.name.as_str()).collect();
    Error::invalid_input(format!(
        "Failed to read file \"{}\": schema mismatch in glob: column \"{name}\" was read from the original file \"{}\", but could not be found in file \"{}\".\nCandidate names: {}\nIf you are trying to read files with different schemas, try setting union_by_name=True",
        reader.file_name(),
        data.files.first_file().unwrap_or_default(),
        reader.file_name(),
        candidates.join(", ")
    ))
}

/// Opens file `file_index` and maps its columns onto the requested global columns
/// (`CreateReader` + `CreateMapping` + `PrepareReader` + `InitializeScan`).
fn open_file(data: &ParquetBindData, file_index: usize, column_ids: &[u64]) -> Result<FileScan> {
    let reader = data.open_reader(file_index)?;
    let path = data.files.files[file_index].clone();
    let hive_partitions = if data.reader_bind.hive_partitioning_indexes.is_empty() {
        std::collections::BTreeMap::default()
    } else {
        hive_parse(&path)
    };
    let mut scan_columns: Vec<ScanColumn> = Vec::new();
    let mut sources = Vec::with_capacity(column_ids.len());
    let mut row_number_column = None;
    for &global_id in column_ids {
        let Ok(global) = usize::try_from(global_id) else {
            sources.push(ColumnSource::Constant(Value::null(LogicalType::BOOLEAN)));
            continue;
        };
        if global >= data.names.len() {
            sources.push(ColumnSource::Constant(Value::null(LogicalType::BOOLEAN)));
            continue;
        }
        if data.reader_bind.filename_idx == Some(global) {
            sources.push(ColumnSource::Constant(Value::varchar(&path)));
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
        if data.file_row_number_idx == Some(global) {
            let index = if let Some(index) = row_number_column {
                index
            } else {
                scan_columns.push(ScanColumn::file_row_number());
                row_number_column = Some(scan_columns.len() - 1);
                scan_columns.len() - 1
            };
            sources.push(ColumnSource::Reader {
                index,
                cast: None,
                target: LogicalType::BIGINT,
            });
            continue;
        }
        let name = &data.names[global];
        let global_type = &data.types[global];
        let Some(local) = reader
            .columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(name))
        else {
            if data.file_options.union_by_name {
                sources.push(ColumnSource::Constant(Value::null(global_type.clone())));
                continue;
            }
            return Err(column_not_found(data, &reader, name));
        };
        let local_type = reader.columns[local].logical_type.clone();
        let cast = if &local_type == global_type {
            None
        } else {
            Some(data.casts.get_cast_function(&local_type, global_type)?)
        };
        scan_columns.push(ScanColumn::column(local));
        sources.push(ColumnSource::Reader {
            index: scan_columns.len() - 1,
            cast,
            target: global_type.clone(),
        });
    }
    // Filters on constant columns decide for the whole file; filters on columns read
    // as stored prune row groups by their statistics.
    let mut skip_file = false;
    let mut prune_filters = Vec::new();
    if let Some(filters) = &data.filters {
        for (&position, filter) in &filters.filters {
            match sources.get(position) {
                Some(ColumnSource::Constant(value)) => {
                    if !crate::filter::constant_passes(filter, value)? {
                        skip_file = true;
                    }
                }
                Some(ColumnSource::Reader { index, cast: None, .. }) => {
                    if let Some(file_column) = scan_columns.get(*index).and_then(|column| column.file_index) {
                        prune_filters.push((file_column, filter));
                    }
                }
                _ => {}
            }
        }
    }
    let mut row_groups = Vec::new();
    if !skip_file {
        for group in 0..reader.num_row_groups() {
            if !crate::filter::prune_row_group(&reader, group, &prune_filters, data.options.can_have_nan, &data.casts)? {
                row_groups.push(group);
            }
        }
    }
    let rows_only = scan_columns.is_empty().then(|| RowsOnly {
        remaining: if skip_file { 0 } else { reader.num_rows() },
    });
    let state = reader.initialize_scan(row_groups, &scan_columns)?;
    Ok(FileScan {
        state,
        sources,
        rows_only,
    })
}

/// The `read_parquet` scan: the next chunk of the requested columns.
fn read_parquet_scan(
    bind_data: Option<&dyn FunctionData>,
    column_ids: &[u64],
    scan_state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<ParquetBindData>())
        .ok_or_else(|| Error::internal("read_parquet scan without bind data"))?;
    if scan_state.local.is_none() {
        scan_state.local = Some(Box::new(ScanState::default()));
    }
    let state = scan_state
        .local
        .as_mut()
        .and_then(|local| local.downcast_mut::<ScanState>())
        .ok_or_else(|| Error::internal("read_parquet scan state"))?;
    loop {
        let Some(file) = state.current.as_mut() else {
            if state.file_index >= data.files.files.len() {
                *chunk = DataChunk::new(
                    data.types
                        .iter()
                        .map(|ty| Vector::constant_null(ty.clone()))
                        .collect(),
                    0,
                );
                return Ok(false);
            }
            state.current = Some(open_file(data, state.file_index, column_ids)?);
            continue;
        };
        let count = if let Some(rows) = &mut file.rows_only {
            let count = rows.remaining.min(STANDARD_VECTOR_SIZE as u64) as usize;
            rows.remaining -= count as u64;
            count
        } else {
            file.state.scan(&mut state.scan_output)?
        };
        if count == 0 {
            state.current = None;
            state.file_index += 1;
            continue;
        }
        let mut columns: Vec<Option<Vector>> = vec![None; data.types.len()];
        for (position, &global_id) in column_ids.iter().enumerate() {
            let Ok(global) = usize::try_from(global_id) else {
                continue;
            };
            if global >= columns.len() || columns[global].is_some() {
                continue;
            }
            columns[global] = Some(match &file.sources[position] {
                ColumnSource::Reader { index, cast, target } => {
                    let vector = state.scan_output[*index].clone();
                    match cast {
                        None => vector,
                        Some(cast) => {
                            let mut result = Vector::flat(target.clone(), count);
                            cast.execute(&vector, &mut result, count, &mut CastParameters::new())?;
                            result
                        }
                    }
                }
                ColumnSource::Constant(value) => Vector::constant(value)?,
            });
        }
        let mut output: Vec<Vector> = columns
            .into_iter()
            .zip(&data.types)
            .map(|(column, ty)| column.unwrap_or_else(|| Vector::constant_null(ty.clone())))
            .collect();
        let mut count = count;
        if let Some(filters) = &data.filters {
            match apply_filters(filters, column_ids, &mut output, count)? {
                Some(0) => continue,
                Some(passed) => count = passed,
                None => {}
            }
        }
        *chunk = DataChunk::new(output, count);
        return Ok(true);
    }
}

/// Applies the pushed-down filters to a chunk; returns the number of rows left if any
/// row was removed (the vectors are then sliced to them).
fn apply_filters(
    filters: &TableFilterSet,
    column_ids: &[u64],
    output: &mut [Vector],
    count: usize,
) -> Result<Option<usize>> {
    let mut sel: Vec<u32> = (0..count as u32).collect();
    for (&position, filter) in &filters.filters {
        let Some(vector) = column_ids
            .get(position)
            .and_then(|&id| usize::try_from(id).ok())
            .and_then(|id| output.get(id))
        else {
            continue;
        };
        filter.select(vector, count, &mut sel)?;
        if sel.is_empty() {
            return Ok(Some(0));
        }
    }
    if sel.len() == count {
        return Ok(None);
    }
    let passed = sel.len();
    let selection = rustdb_types::SelectionVector::new(sel);
    for vector in output.iter_mut() {
        *vector = vector.slice(&selection);
    }
    Ok(Some(passed))
}

/// `MultiFileFunction` with table filters: the bind data that applies them.
fn read_parquet_pushdown_filters(
    bind_data: &dyn FunctionData,
    _: &[u64],
    filters: &dyn Any,
) -> Result<Arc<dyn FunctionData>> {
    let data = bind_data
        .as_any()
        .downcast_ref::<ParquetBindData>()
        .ok_or_else(|| Error::internal("read_parquet filter pushdown without bind data"))?;
    let filters = filters
        .downcast_ref::<TableFilterSet>()
        .ok_or_else(|| Error::internal("read_parquet filter pushdown without a filter set"))?;
    Ok(Arc::new(data.with_filters(filters.clone())))
}

/// The named parameters of `read_parquet`.
fn read_parquet_parameters() -> Vec<(String, LogicalType)> {
    let mut parameters: Vec<(String, LogicalType)> = [
        ("binary_as_string", LogicalType::BOOLEAN),
        ("file_row_number", LogicalType::BOOLEAN),
        ("debug_use_openssl", LogicalType::BOOLEAN),
        ("compression", LogicalType::VARCHAR),
        ("explicit_cardinality", LogicalType::UBIGINT),
        ("schema", LogicalType::ANY),
        ("encryption_config", LogicalType::ANY),
        ("parquet_version", LogicalType::VARCHAR),
        ("can_have_nan", LogicalType::BOOLEAN),
    ]
    .into_iter()
    .map(|(name, ty)| (name.to_owned(), ty))
    .collect();
    parameters.extend(multi_file_parameters());
    parameters
}

/// One overload of `read_parquet` / `parquet_scan`.
fn read_parquet_function(name: &'static str, argument: LogicalType) -> TableFunction {
    let mut function = TableFunction::new(name, vec![argument])
        .with_bind(move |input| read_parquet_bind(name, input))
        .with_scan(read_parquet_scan)
        .with_cardinality(|bind_data| {
            let data = bind_data?.as_any().downcast_ref::<ParquetBindData>()?;
            if data.options.explicit_cardinality > 0 {
                return Some(data.options.explicit_cardinality);
            }
            match data.files.expand_result() {
                FileExpandResult::MultipleFiles => {
                    Some(data.initial_file_cardinality.max(1000) * data.files.files.len() as u64)
                }
                _ => Some(data.initial_file_cardinality),
            }
        });
    function.named_parameters = read_parquet_parameters();
    function.projection_pushdown = true;
    function.filter_pushdown = true;
    function.filter_prune = true;
    function.pushdown_filters = Some(Arc::new(read_parquet_pushdown_filters));
    function.empty_column = true;
    function
}

/// `read_parquet` and `parquet_scan` (`ParquetScanFunction::GetFunctionSet`).
pub fn read_parquet_functions() -> Vec<TableFunctionSet> {
    ["read_parquet", "parquet_scan"]
        .into_iter()
        .map(|name| {
            let mut set = TableFunctionSet::new(name);
            set.add(read_parquet_function(name, LogicalType::VARCHAR));
            set.add(read_parquet_function(name, LogicalType::list(LogicalType::VARCHAR)));
            set
        })
        .collect()
}

#[allow(dead_code, reason = "used by the schema mapping of nested columns")]
fn find_column<'a>(columns: &'a [ColumnDefinition], name: &str) -> Option<&'a ColumnDefinition> {
    columns.iter().find(|column| column.name.eq_ignore_ascii_case(name))
}
