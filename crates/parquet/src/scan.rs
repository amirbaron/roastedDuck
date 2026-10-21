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
    FunctionData, ParallelScanGlobal, ParallelScanLocal, ParallelTableScan, TableFunction,
    TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet, TableScanState,
};
use rustdb_planner::ClientContext;
use rustdb_storage::{TableFilter, TableFilterSet};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Value, Vector};

use crate::reader::{ColumnDefinition, ParquetReader, ReaderScanState, ScanColumn};
use crate::schema::{ParquetColumnDefinition, ParquetOptions};

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

pub use rustdb_csv::multi_file::{
    COLUMN_IDENTIFIER_FILE_INDEX, COLUMN_IDENTIFIER_FILE_ROW_NUMBER, COLUMN_IDENTIFIER_FILENAME,
};

/// `MultiFileScanStats`: the statistics of a column from the metadata already read
/// while binding: of the single file, or merged over all files with `union_by_name`.
fn read_parquet_statistics(bind_data: Option<&dyn FunctionData>, column: u64) -> Option<Box<dyn Any + Send>> {
    let data = bind_data?.as_any().downcast_ref::<ParquetBindData>()?;
    let reader = data
        .initial_reader
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()?;
    let index = usize::try_from(column)
        .ok()
        .filter(|&index| index < data.names.len())?;
    let name = &data.names[index];
    let column_statistics = |reader: &ParquetReader| {
        if data.file_row_number_idx == Some(index) {
            reader.row_number_statistics()
        } else {
            reader.read_statistics(name, &data.casts)
        }
    };
    let stats = if data.files.expand_result() == FileExpandResult::MultipleFiles {
        if !data.file_options.union_by_name {
            return None;
        }
        let mut merged = column_statistics(&reader)?;
        for union_reader in data.union_readers.iter().skip(1) {
            let stats = column_statistics(union_reader)?;
            if merged.logical_type() != stats.logical_type() {
                return None;
            }
            merged.merge(&stats);
        }
        merged
    } else {
        column_statistics(&reader)?
    };
    Some(Box::new(stats))
}

/// A row group of the file of a single-file scan (`ParquetPartitionRowGroup`).
#[derive(Debug)]
struct ParquetPartitionRowGroup {
    reader: Arc<ParquetReader>,
    row_group: usize,
    /// The `file_row_number` column of the scan, if requested.
    file_row_number_idx: Option<usize>,
    casts: Arc<CastFunctionSet>,
}

impl rustdb_function::PartitionRowGroup for ParquetPartitionRowGroup {
    fn column_statistics(&self, column: u64) -> Option<Box<dyn Any + Send>> {
        let column = usize::try_from(column).ok()?;
        if self.file_row_number_idx == Some(column) {
            return self
                .reader
                .row_number_statistics_of(self.row_group)
                .map(|stats| Box::new(stats) as Box<dyn Any + Send>);
        }
        let schema = self.reader.root_schema.children.get(column)?;
        let group = self.reader.context.metadata.row_groups.get(self.row_group)?;
        crate::statistics::transform_column_statistics(
            schema,
            &group.columns,
            self.reader.options.can_have_nan,
            &self.casts,
        )
        .ok()
        .flatten()
        .map(|stats| Box::new(stats) as Box<dyn Any + Send>)
    }

    fn min_max_is_exact(&self, column: u64) -> bool {
        let Ok(column) = usize::try_from(column) else {
            return false;
        };
        if self.file_row_number_idx == Some(column) {
            return true;
        }
        if column >= self.reader.root_schema.children.len() {
            return false;
        }
        // Like DuckDB, the column chunk at the column's (top-level) index.
        self.reader
            .context
            .metadata
            .row_groups
            .get(self.row_group)
            .and_then(|group| group.columns.get(column))
            .and_then(|chunk| chunk.meta_data.as_ref())
            .and_then(|meta| meta.statistics.as_ref())
            .is_some_and(|stats| {
                stats.is_min_value_exact == Some(true) && stats.is_max_value_exact == Some(true)
            })
    }
}

/// `ParquetGetPartitionStats`: the row groups of a single file whose metadata was read
/// while binding.
fn read_parquet_partition_stats(
    bind_data: Option<&dyn FunctionData>,
) -> Vec<rustdb_function::PartitionStatistics> {
    let Some(data) = bind_data.and_then(|data| data.as_any().downcast_ref::<ParquetBindData>()) else {
        return Vec::new();
    };
    if data.files.expand_result() != FileExpandResult::SingleFile
        || data
            .initial_reader
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none()
    {
        // `TryLoadCaches`: the row counts of every file from the metadata cache.
        let Some(cache) = &data.options.metadata_cache else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for path in &data.files.files {
            let Some(entry) = crate::reader::CachedMetadata::lookup(cache, path) else {
                return Vec::new();
            };
            result.extend(entry.metadata.row_groups.iter().map(|group| {
                rustdb_function::PartitionStatistics {
                    count: group.num_rows.max(0) as u64,
                    exact: true,
                    row_group: None,
                }
            }));
        }
        return result;
    }
    let Some(reader) = data
        .initial_reader
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
    else {
        return Vec::new();
    };
    let row_groups = reader.context.metadata.row_groups.len();
    (0..row_groups)
        .map(|row_group| rustdb_function::PartitionStatistics {
            count: reader.context.metadata.row_groups[row_group].num_rows.max(0) as u64,
            exact: true,
            row_group: Some(Arc::new(ParquetPartitionRowGroup {
                reader: Arc::clone(&reader),
                row_group,
                file_row_number_idx: data.file_row_number_idx,
                casts: Arc::clone(&data.casts),
            })),
        })
        .collect()
}

/// `MultiFileGetVirtualColumns`: `filename` (unless the `filename` option names a real
/// column), `file_index` and Parquet's `file_row_number`.
fn read_parquet_virtual_columns(bind_data: Option<&dyn FunctionData>) -> Vec<(u64, String, LogicalType)> {
    let filename_option = bind_data
        .and_then(|data| data.as_any().downcast_ref::<ParquetBindData>())
        .is_some_and(|data| data.reader_bind.filename_idx.is_some());
    let mut result = rustdb_csv::multi_file::multi_file_virtual_columns(filename_option);
    result.push((
        COLUMN_IDENTIFIER_FILE_ROW_NUMBER,
        "file_row_number".to_owned(),
        LogicalType::BIGINT,
    ));
    result
}

/// The bind data of `read_parquet` (`MultiFileBindData` with `ParquetReadBindData`).
#[derive(Debug)]
pub struct ParquetBindData {
    pub files: FileList,
    /// The file list `EXPLAIN ANALYZE` shows: the patterns given, until filters prune
    /// the expanded files (`GetDisplayFileList`).
    display_paths: Option<Vec<String>>,
    pub names: Vec<String>,
    pub types: Vec<LogicalType>,
    pub file_options: MultiFileOptions,
    reader_bind: MultiFileBindData,
    pub options: ParquetOptions,
    /// The index of the `file_row_number` column when the option is set.
    file_row_number_idx: Option<usize>,
    /// The reader of the first file, opened while binding (`initial_reader`).
    initial_reader: Mutex<Option<Arc<ParquetReader>>>,
    /// With `union_by_name`, the readers of all files, opened while binding
    /// (`union_readers`).
    union_readers: Vec<Arc<ParquetReader>>,
    casts: Arc<CastFunctionSet>,
    /// The columns of the table a `COPY ... FROM` loads (`table_columns`).
    pub(crate) table_columns: Vec<String>,
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
    /// A copy that scans only the files at positions `keep` (`PruneReaders`).
    fn with_files(&self, keep: &[usize]) -> Self {
        let files: Vec<String> = keep
            .iter()
            .filter_map(|&index| self.files.files.get(index).cloned())
            .collect();
        let mut result = self.with_filters_opt(self.filters.clone());
        result
            .union_readers
            .retain(|reader| files.iter().any(|file| file == reader.file_name()));
        let initial = result
            .initial_reader
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        if initial
            .as_ref()
            .is_some_and(|reader| !files.iter().any(|file| file == reader.file_name()))
        {
            *initial = None;
        }
        result.files = FileList { files };
        result.display_paths = None;
        result
    }

    /// A copy that applies `filters` (`MultiFileFunction` with pushed-down table filters).
    fn with_filters(&self, filters: TableFilterSet) -> Self {
        self.with_filters_opt(Some(Arc::new(filters)))
    }

    /// A copy with the pushed-down table filters `filters`.
    fn with_filters_opt(&self, filters: Option<Arc<TableFilterSet>>) -> Self {
        Self {
            files: self.files.clone(),
            display_paths: self.display_paths.clone(),
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
            union_readers: self.union_readers.clone(),
            table_columns: self.table_columns.clone(),
            casts: Arc::clone(&self.casts),
            filters,
            initial_file_cardinality: self.initial_file_cardinality,
        }
    }

    /// The type of scanned column `id`, virtual columns included.
    fn column_type(&self, id: u64) -> LogicalType {
        match id {
            COLUMN_IDENTIFIER_FILENAME => LogicalType::VARCHAR,
            COLUMN_IDENTIFIER_FILE_INDEX => LogicalType::UBIGINT,
            COLUMN_IDENTIFIER_FILE_ROW_NUMBER => LogicalType::BIGINT,
            _ => usize::try_from(id)
                .ok()
                .and_then(|index| self.types.get(index).cloned())
                .unwrap_or(LogicalType::BOOLEAN),
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
            "schema" => {
                options.schema = parse_schema_parameter(value, context)?;
                file_options.auto_detect_hive_partitioning = false;
            }
            "encryption_config" => {
                let cache = context.function_client.object_cache();
                options.encryption = Some(crate::crypto::parse_encryption_config(value, cache.as_deref())?);
            }
            // `compression` and `debug_use_openssl` are accepted and ignored.
            _ => {}
        }
    }
    Ok(())
}

/// `VerifyParquetSchemaParameter` and `ParquetColumnDefinition::FromSchemaValue`: the
/// columns of the `schema` parameter, a MAP from field id (or name) to
/// `{name, type, default_value}`.
fn parse_schema_parameter(value: &Value, context: &ClientContext) -> Result<Vec<ParquetColumnDefinition>> {
    use rustdb_function::cast::ValueCast as _;
    let map_type = value.logical_type();
    if map_type.id() != LogicalTypeId::Map {
        return Err(Error::invalid_input(format!(
            "'schema' expects a value of type MAP, not {}",
            crate::copy::type_id_name(map_type)
        )));
    }
    let (key_type, value_type) = map_type
        .map_key_value()
        .ok_or_else(|| Error::internal("MAP type without key/value"))?;
    if value_type.id() != LogicalTypeId::Struct {
        return Err(Error::invalid_input(
            "'schema' expects a STRUCT as the value type of the map",
        ));
    }
    let children = value_type.struct_children().unwrap_or_default();
    if children.len() < 3 {
        return Err(Error::invalid_input(
            "'schema' expects the STRUCT to have 3 children, 'name', 'type' and 'default_value",
        ));
    }
    if !children[0].0.eq_ignore_ascii_case("name") {
        return Err(Error::invalid_input(
            "'schema' expects the first field of the struct to be called 'name'",
        ));
    }
    if children[0].1.id() != LogicalTypeId::Varchar {
        return Err(Error::invalid_input(format!(
            "'schema' expects the 'name' field to be of type VARCHAR, not {}",
            crate::copy::type_id_name(&children[0].1)
        )));
    }
    if !children[1].0.eq_ignore_ascii_case("type") {
        return Err(Error::invalid_input(
            "'schema' expects the second field of the struct to be called 'type'",
        ));
    }
    if children[1].1.id() != LogicalTypeId::Varchar {
        return Err(Error::invalid_input(format!(
            "'schema' expects the 'type' field to be of type VARCHAR, not {}",
            crate::copy::type_id_name(&children[1].1)
        )));
    }
    if !children[2].0.eq_ignore_ascii_case("default_value") {
        return Err(Error::invalid_input(
            "'schema' expects the third field of the struct to be called 'default_value'",
        ));
    }
    if !matches!(key_type.id(), LogicalTypeId::Integer | LogicalTypeId::Varchar) {
        return Err(Error::invalid_input(format!(
            "'schema' expects the value type of the map to be either INTEGER or VARCHAR, not {}",
            crate::copy::type_id_name(key_type)
        )));
    }
    let entries = value.children().unwrap_or_default();
    if entries.is_empty() {
        return Err(Error::binder("Parquet schema cannot be empty"));
    }
    let mut columns = Vec::with_capacity(entries.len());
    for entry in entries {
        let parts = entry.children().unwrap_or_default();
        let (Some(identifier), Some(definition)) = (parts.first(), parts.get(1)) else {
            return Err(Error::internal("malformed MAP entry in the Parquet schema"));
        };
        let fields = definition.children().unwrap_or_default();
        let text = |index: usize| {
            fields
                .get(index)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_default()
        };
        let name = text(0);
        let logical_type = rustdb_csv::read_csv::transform_string_to_logical_type(context, &text(1))?;
        let default = fields.get(2).cloned().unwrap_or_default();
        let default_value = default
            .try_cast_as(&context.casts, &logical_type, false)
            .map_err(|_| {
                Error::binder(format!(
                    "Unable to cast Parquet schema default_value \"{}\" to {logical_type}",
                    default.to_varchar().unwrap_or_else(|_| "NULL".to_owned())
                ))
            })?;
        columns.push(ParquetColumnDefinition {
            identifier: identifier.clone(),
            name,
            logical_type,
            default_value,
        });
    }
    Ok(columns)
}

/// `MultiFileFunction::MultiFileBind` with `ParquetMultiFileInfo`.
fn read_parquet_bind(
    function_name: &str,
    input: &TableFunctionBindInput<'_>,
) -> Result<TableFunctionBindOutput> {
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
        geoparquet_conversion: setting_bool(context, "enable_geoparquet_conversion"),
        // `ParquetReader::MetadataCacheEnabled`.
        metadata_cache: if setting_bool(context, "parquet_metadata_cache") {
            context.function_client.object_cache()
        } else {
            None
        },
        // `ShouldAndCanPrefetch`: local files only with `prefetch_all_parquet_files`.
        prefetch: setting_bool(context, "prefetch_all_parquet_files")
            && !setting_bool(context, "disable_parquet_prefetching"),
        ..ParquetOptions::default()
    };
    parse_options(input, &mut file_options, &mut options, context)?;
    let mut data = bind_files(&context.casts, files, file_options, &options)?;
    data.display_paths = Some(paths);
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
    options: &ParquetOptions,
) -> Result<ParquetBindData> {
    file_options.auto_detect_hive_partitioning(&files, casts)?;
    let mut names = Vec::new();
    let mut types = Vec::new();
    let mut initial_reader = None;
    let mut union_readers = Vec::new();
    let mut initial_file_cardinality = 0;
    // The file_row_number column is added by the bind, not by the file schemas.
    let mut file_options_reader = options.clone();
    file_options_reader.file_row_number = false;
    if !options.schema.is_empty() {
        // `BindSchema`: the columns come from the parameter, not from the files.
        if file_options.union_by_name || file_options.hive_partitioning {
            return Err(Error::binder(
                "Parquet schema cannot be combined with union_by_name=true or hive_partitioning=true",
            ));
        }
        for column in &options.schema {
            names.push(column.name.clone());
            types.push(column.logical_type.clone());
        }
    } else if file_options.union_by_name {
        let mut union_names_map = HashMap::new();
        for (index, path) in files.files.iter().enumerate() {
            let reader = Arc::new(ParquetReader::open(path, file_options_reader.clone())?);
            let file_names: Vec<String> = reader.columns.iter().map(|column| column.name.clone()).collect();
            let file_types: Vec<LogicalType> = reader
                .columns
                .iter()
                .map(|column| column.logical_type.clone())
                .collect();
            combine_union_types(
                &file_names,
                &file_types,
                &mut types,
                &mut names,
                &mut union_names_map,
            );
            if index == 0 {
                initial_file_cardinality = reader.num_rows();
                initial_reader = Some(Arc::clone(&reader));
            }
            union_readers.push(reader);
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
        if names
            .iter()
            .any(|name| name.eq_ignore_ascii_case("file_row_number"))
        {
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
        display_paths: None,
        names,
        types,
        file_options,
        reader_bind,
        options: file_options_reader,
        file_row_number_idx,
        initial_reader: Mutex::new(initial_reader),
        union_readers,
        table_columns: Vec::new(),
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
        cast: Option<Box<ColumnCast>>,
        target: LogicalType,
    },
    /// The same value for every row (filename, hive partitions, missing columns).
    Constant(Value),
}

/// The cast of a file column to the type the scan produces (an expression of the
/// `MultiFileColumnMapper`); casts that do not exist fail when they would run.
#[derive(Debug)]
struct ColumnCast {
    cast: Result<rustdb_function::cast::BoundCastInfo>,
    /// The column's name and type in the file.
    local_name: String,
    local_type: LogicalType,
}

impl ColumnCast {
    /// Casts a chunk of the column (`MultiFileReader::FinalizeChunk`), explaining a
    /// failure like `GetExtendedMultiFileError`.
    fn execute(
        &self,
        data: &ParquetBindData,
        file_name: &str,
        position: usize,
        vector: &Vector,
        target: &LogicalType,
        count: usize,
    ) -> Result<Vector> {
        let result = self.cast.as_ref().map_err(Clone::clone).and_then(|cast| {
            let mut result = Vector::flat(target.clone(), count);
            cast.execute(vector, &mut result, count, &mut CastParameters::new())?;
            Ok(result)
        });
        result.map_err(|error| {
            if error.kind() == rustdb_common::ErrorKind::Internal {
                return error;
            }
            let (local_name, local_type) = (&self.local_name, &self.local_type);
            let extended = if data.table_columns.is_empty() {
                format!(
                    "In file \"{file_name}\" the column \"{local_name}\" has type {local_type}, but we are trying to read it as type {target}.\nThis can happen when reading multiple Parquet files. The schema information is taken from the first Parquet file by default. Possible solutions:\n* Enable the union_by_name=True option to combine the schema of all Parquet files (https://duckdb.org/docs/stable/data/multiple_files/combining_schemas)\n* Use a COPY statement to automatically derive types from an existing table."
                )
            } else {
                let target_column = data
                    .table_columns
                    .get(position)
                    .map(|name| format!("\"{name}\" "))
                    .unwrap_or_default();
                format!(
                    "In file \"{file_name}\" the column \"{local_name}\" has type {local_type}, but we are trying to load it into column {target_column}with type {target}.\nThis means the Parquet schema does not match the schema of the table.\nPossible solutions:\n* Insert by name instead of by position using \"INSERT INTO tbl BY NAME SELECT * FROM read_parquet(...)\"\n* Manually specify which columns to insert using \"INSERT INTO tbl SELECT ... FROM read_parquet(...)\""
                )
            };
            Error::conversion(format!(
                "Error while reading file \"{file_name}\": failed to cast column \"{local_name}\" from type {local_type} to {target}: : {}\n\n{extended}",
                error.raw_message()
            ))
        })
    }
}

/// The scan of one file.
#[derive(Debug)]
struct FileScan {
    state: ReaderScanState,
    sources: Vec<ColumnSource>,
    /// Whether no column is read from the file (only its row count matters).
    rows_only: Option<RowsOnly>,
    /// The filters the reader does not apply, by output column.
    filters: Vec<(usize, TableFilter)>,
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

/// A file ready to be scanned: its reader, how its columns map onto the output and the
/// row groups left after pruning.
#[derive(Debug)]
struct PreparedFile {
    reader: Arc<ParquetReader>,
    scan_columns: Vec<ScanColumn>,
    sources: Vec<ColumnSource>,
    /// The filters the reader applies, by scan column.
    reader_filters: Vec<(usize, TableFilter)>,
    /// The filters the reader does not apply, by output column.
    chunk_filters: Vec<(usize, TableFilter)>,
    row_groups: Vec<usize>,
    /// The row count to emit when no column is read from the file.
    rows_only: Option<u64>,
}

/// Opens file `file_index` and maps its columns onto the requested global columns
/// (`CreateReader` + `CreateMapping` + `PrepareReader` + `InitializeScan`).
fn open_file(data: &ParquetBindData, file_index: usize, column_ids: &[u64]) -> Result<FileScan> {
    let file = prepare_file(data, file_index, column_ids)?;
    let state = file
        .reader
        .initialize_scan(file.row_groups, &file.scan_columns, file.reader_filters)?;
    Ok(FileScan {
        state,
        sources: file.sources,
        rows_only: file.rows_only.map(|remaining| RowsOnly { remaining }),
        filters: file.chunk_filters,
    })
}

/// Opens file `file_index`, maps its columns onto the requested global columns and
/// prunes its row groups (`CreateReader` + `CreateMapping` + `PrepareReader`).
fn prepare_file(data: &ParquetBindData, file_index: usize, column_ids: &[u64]) -> Result<PreparedFile> {
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
        let is_row_number = match global_id {
            COLUMN_IDENTIFIER_FILENAME => {
                sources.push(ColumnSource::Constant(Value::varchar(&path)));
                continue;
            }
            COLUMN_IDENTIFIER_FILE_INDEX => {
                sources.push(ColumnSource::Constant(Value::ubigint(file_index as u64)));
                continue;
            }
            COLUMN_IDENTIFIER_FILE_ROW_NUMBER => true,
            _ => usize::try_from(global_id).is_ok_and(|global| data.file_row_number_idx == Some(global)),
        };
        if is_row_number {
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
        let Some(global) = usize::try_from(global_id)
            .ok()
            .filter(|&global| global < data.names.len())
        else {
            sources.push(ColumnSource::Constant(Value::null(LogicalType::BOOLEAN)));
            continue;
        };
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
        let global_type = &data.types[global];
        if let Some(column) = data.options.schema.get(global) {
            // `MultiFileColumnMapper` with the `schema` parameter: by field id or name.
            let local = if column.identifier.logical_type().id() == LogicalTypeId::Integer {
                reader
                    .columns
                    .iter()
                    .position(|local| local.identifier.as_ref() == Some(&column.identifier))
            } else {
                let name = column.identifier.as_str().unwrap_or_default();
                reader
                    .columns
                    .iter()
                    .position(|local| local.name.eq_ignore_ascii_case(name))
            };
            let Some(local) = local else {
                sources.push(ColumnSource::Constant(column.default_value.clone()));
                continue;
            };
            let local_type = reader.columns[local].logical_type.clone();
            let cast = (&local_type != global_type).then(|| {
                Box::new(ColumnCast {
                    cast: data.casts.get_cast_function(&local_type, global_type),
                    local_name: reader.columns[local].name.clone(),
                    local_type: local_type.clone(),
                })
            });
            scan_columns.push(ScanColumn::column(local));
            sources.push(ColumnSource::Reader {
                index: scan_columns.len() - 1,
                cast,
                target: global_type.clone(),
            });
            continue;
        }
        let name = &data.names[global];
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
        let cast = (&local_type != global_type).then(|| {
            Box::new(ColumnCast {
                cast: data.casts.get_cast_function(&local_type, global_type),
                local_name: reader.columns[local].name.clone(),
                local_type: local_type.clone(),
            })
        });
        scan_columns.push(ScanColumn::column(local));
        sources.push(ColumnSource::Reader {
            index: scan_columns.len() - 1,
            cast,
            target: global_type.clone(),
        });
    }
    // Filters on constant columns decide for the whole file; filters on columns read
    // as stored prune row groups by their statistics.
    // Filters on columns read as stored are evaluated by the reader, the others (on cast
    // columns) on the chunks it returns.
    let mut skip_file = false;
    let mut prune_filters = Vec::new();
    let mut reader_filters = TableFilterSet::default();
    let mut chunk_filters = Vec::new();
    if let Some(filters) = &data.filters {
        for (&position, filter) in &filters.filters {
            match sources.get(position) {
                Some(ColumnSource::Constant(value)) => {
                    if !crate::filter::constant_passes(filter, value)? {
                        skip_file = true;
                    }
                }
                Some(ColumnSource::Reader {
                    index, cast: None, ..
                }) => {
                    if let Some(file_column) = scan_columns.get(*index).and_then(|column| column.file_index) {
                        prune_filters.push((file_column, filter));
                    }
                    reader_filters.push_filter(*index, filter.clone());
                }
                _ => chunk_filters.push((position, filter.clone())),
            }
        }
    }
    let mut row_groups = Vec::new();
    if !skip_file {
        for group in 0..reader.num_row_groups() {
            if !crate::filter::prune_row_group(
                &reader,
                group,
                &prune_filters,
                data.options.can_have_nan,
                &data.casts,
            )? {
                row_groups.push(group);
            }
        }
    }
    let rows_only = scan_columns
        .is_empty()
        .then(|| if skip_file { 0 } else { reader.num_rows() });
    Ok(PreparedFile {
        reader,
        scan_columns,
        sources,
        reader_filters: reader_filters.filters.into_iter().collect(),
        chunk_filters,
        row_groups,
        rows_only,
    })
}

/// The output chunk of `count` scanned rows of file `file_index` (`scan_output`, one
/// vector per scan column): one column per source, cast where needed, with the chunk
/// filters applied. `None` when no row passes them.
fn assemble_chunk(
    data: &ParquetBindData,
    file_index: usize,
    sources: &[ColumnSource],
    filters: &[(usize, TableFilter)],
    scan_output: &[Vector],
    count: usize,
) -> Result<Option<DataChunk>> {
    // One output column per scanned column id (`projected_output`).
    let mut output = Vec::with_capacity(sources.len());
    for (position, source) in sources.iter().enumerate() {
        output.push(match source {
            ColumnSource::Reader { index, cast, target } => {
                let vector = scan_output[*index].clone();
                match cast {
                    None => vector,
                    Some(cast) => cast.execute(
                        data,
                        &data.files.files[file_index],
                        position,
                        &vector,
                        target,
                        count,
                    )?,
                }
            }
            ColumnSource::Constant(value) => Vector::constant(value)?,
        });
    }
    let mut count = count;
    match apply_filters(filters, &mut output, count)? {
        Some(0) => return Ok(None),
        Some(passed) => count = passed,
        None => {}
    }
    Ok(Some(DataChunk::new(output, count)))
}

/// The shared state of a parallel `read_parquet` scan: a cursor handing out the files'
/// row groups (DuckDB's `MultiFileGlobalState` with `TryInitializeNextBatch`).
#[derive(Debug)]
struct ParallelGlobal {
    cursor: Mutex<Cursor>,
    max_threads: usize,
}

/// The position of a parallel scan.
#[derive(Debug, Default)]
struct Cursor {
    file_index: usize,
    file: Option<Arc<PreparedFile>>,
    /// The position in the current file's row groups of the next morsel.
    next_group: usize,
    /// The batch index of the next morsel.
    batch_index: u64,
    /// Whether a thread failed, so the others stop.
    failed: bool,
}

/// A unit of a parallel scan: a file (with its index) and one of its row groups, or
/// (`None`) all the rows of a file no column is read from; with its batch index.
type Morsel = (Arc<PreparedFile>, usize, Option<usize>, u64);

impl Cursor {
    /// The next morsel, opening files as needed; `None` once all are handed out.
    fn next(&mut self, data: &ParquetBindData, column_ids: &[u64]) -> Result<Option<Morsel>> {
        loop {
            if let Some(file) = &self.file {
                let group = match file.rows_only {
                    Some(rows) => (self.next_group == 0 && rows > 0).then_some(None),
                    None => file.row_groups.get(self.next_group).map(|&group| Some(group)),
                };
                if let Some(group) = group {
                    self.next_group += 1;
                    let batch = self.batch_index;
                    self.batch_index += 1;
                    return Ok(Some((Arc::clone(file), self.file_index, group, batch)));
                }
                self.file = None;
                self.file_index += 1;
                self.next_group = 0;
            }
            if self.file_index >= data.files.files.len() {
                return Ok(None);
            }
            self.file = Some(Arc::new(prepare_file(data, self.file_index, column_ids)?));
        }
    }
}

/// The state of one thread of a parallel `read_parquet` scan.
#[derive(Debug, Default)]
struct ParallelLocal {
    file: Option<Arc<PreparedFile>>,
    file_index: usize,
    /// The reader of the current row group; `None` for a rows-only morsel.
    state: Option<ReaderScanState>,
    /// The rows left of a rows-only morsel.
    rows_remaining: u64,
    scan_output: Vec<Vector>,
    batch_index: u64,
    /// Whether a morsel is being read.
    active: bool,
}

/// The bind data of a `read_parquet` scan.
fn parquet_bind_data(bind_data: Option<&dyn FunctionData>) -> Result<&ParquetBindData> {
    bind_data
        .and_then(|data| data.as_any().downcast_ref::<ParquetBindData>())
        .ok_or_else(|| Error::internal("read_parquet scan without bind data"))
}

/// `MultiFileFunction::InitGlobal` + `MaxThreads`: one thread per row group of a single
/// file, any number for several files.
fn read_parquet_init_global(
    bind_data: Option<&dyn FunctionData>,
    column_ids: &[u64],
) -> Result<ParallelScanGlobal> {
    let data = parquet_bind_data(bind_data)?;
    let mut cursor = Cursor::default();
    let max_threads = match data.files.files.len() {
        0 => 1,
        1 => {
            let file = prepare_file(data, 0, column_ids)?;
            let threads = file.row_groups.len().max(1);
            cursor.file = Some(Arc::new(file));
            threads
        }
        _ => usize::MAX,
    };
    Ok(Arc::new(ParallelGlobal {
        cursor: Mutex::new(cursor),
        max_threads,
    }))
}

/// The global state of a parallel `read_parquet` scan.
fn parallel_global(global: &ParallelScanGlobal) -> Result<&ParallelGlobal> {
    global
        .downcast_ref::<ParallelGlobal>()
        .ok_or_else(|| Error::internal("read_parquet parallel scan state"))
}

/// `MultiFileFunction::MultiFileScan` on one thread: the next chunk of this thread's
/// row group, taking the next row group when it is done.
fn read_parquet_parallel_scan(
    bind_data: Option<&dyn FunctionData>,
    column_ids: &[u64],
    global: &ParallelScanGlobal,
    local: &mut ParallelScanLocal,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = parquet_bind_data(bind_data)?;
    let global = parallel_global(global)?;
    let local = local
        .downcast_mut::<ParallelLocal>()
        .ok_or_else(|| Error::internal("read_parquet parallel scan local state"))?;
    loop {
        if local.active
            && let Some(file) = &local.file
        {
            let count = if let Some(state) = &mut local.state {
                state.scan(&mut local.scan_output)?
            } else {
                let count = local.rows_remaining.min(STANDARD_VECTOR_SIZE as u64) as usize;
                local.rows_remaining -= count as u64;
                count
            };
            if count > 0 {
                if let Some(output) = assemble_chunk(
                    data,
                    local.file_index,
                    &file.sources,
                    &file.chunk_filters,
                    &local.scan_output,
                    count,
                )? {
                    *chunk = output;
                    return Ok(true);
                }
                continue;
            }
            local.active = false;
        }
        let next = {
            let mut cursor = global.cursor.lock().unwrap_or_else(PoisonError::into_inner);
            if cursor.failed {
                return Ok(false);
            }
            match cursor.next(data, column_ids) {
                Ok(next) => next,
                Err(error) => {
                    cursor.failed = true;
                    return Err(error);
                }
            }
        };
        let Some((file, file_index, group, batch_index)) = next else {
            return Ok(false);
        };
        local.batch_index = batch_index;
        local.file_index = file_index;
        match group {
            None => {
                local.state = None;
                local.rows_remaining = file.rows_only.unwrap_or(0);
            }
            Some(group) => {
                let same_file = local
                    .file
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &file));
                match &mut local.state {
                    Some(state) if same_file => state.restart(vec![group]),
                    _ => {
                        local.state = Some(file.reader.initialize_scan(
                            vec![group],
                            &file.scan_columns,
                            file.reader_filters.clone(),
                        )?);
                    }
                }
            }
        }
        local.file = Some(file);
        local.active = true;
    }
}

/// The file filter pushdown of `read_parquet` (`MultiFileComplexFilterPushdown`).
pub(crate) fn read_parquet_file_filter_pushdown() -> rustdb_function::FileFilterPushdown {
    rustdb_function::FileFilterPushdown {
        files: Arc::new(|bind_data| {
            let data = bind_data.as_any().downcast_ref::<ParquetBindData>()?;
            (data.file_options.hive_partitioning || data.file_options.filename)
                .then(|| data.files.files.clone())
        }),
        known_value: Arc::new(|bind_data, path, name, ty| {
            let Some(data) = bind_data.as_any().downcast_ref::<ParquetBindData>() else {
                return Ok(None);
            };
            rustdb_csv::multi_file::known_column_value(&data.file_options, &data.casts, path, name, ty)
        }),
        with_files: Arc::new(|bind_data, keep| {
            let data = bind_data
                .as_any()
                .downcast_ref::<ParquetBindData>()
                .ok_or_else(|| Error::internal("read_parquet file pruning without bind data"))?;
            Ok(Arc::new(data.with_files(keep)) as Arc<dyn FunctionData>)
        }),
    }
}

/// `MultiFileDynamicToString`: the number of files opened and the first file names.
fn read_parquet_dynamic_to_string(
    bind_data: Option<&dyn FunctionData>,
    global: &ParallelScanGlobal,
) -> Vec<(String, String)> {
    const FILE_NAME_LIST_LIMIT: usize = 5;
    let (Ok(data), Ok(global)) = (parquet_bind_data(bind_data), parallel_global(global)) else {
        return Vec::new();
    };
    let files_loaded = {
        let cursor = global.cursor.lock().unwrap_or_else(PoisonError::into_inner);
        cursor.file_index + usize::from(cursor.file.is_some())
    };
    let mut result = vec![("Total Files Read".to_owned(), files_loaded.to_string())];
    let display = data.display_paths.as_ref().unwrap_or(&data.files.files);
    if !display.is_empty() {
        let mut names: Vec<&str> = display
            .iter()
            .take(FILE_NAME_LIST_LIMIT)
            .map(String::as_str)
            .collect();
        if files_loaded > FILE_NAME_LIST_LIMIT {
            names.push("...");
        }
        result.push(("Filename(s)".to_owned(), names.join(", ")));
    }
    result
}

/// The `read_parquet` scan by several threads (`MultiFileFunction` with
/// `ParquetScanMaxThreads` / `ParquetScanGetBatchIndex`).
fn read_parquet_parallel() -> ParallelTableScan {
    ParallelTableScan {
        init_global: Arc::new(read_parquet_init_global),
        init_local: Arc::new(|_| Ok(Box::new(ParallelLocal::default()) as ParallelScanLocal)),
        scan: Arc::new(read_parquet_parallel_scan),
        max_threads: Arc::new(|global| parallel_global(global).map_or(1, |global| global.max_threads)),
        batch_index: Arc::new(|_, local| {
            local
                .downcast_ref::<ParallelLocal>()
                .map_or(0, |local| local.batch_index)
        }),
        dynamic_to_string: Some(Arc::new(read_parquet_dynamic_to_string)),
    }
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
                    column_ids
                        .iter()
                        .map(|&id| Vector::constant_null(data.column_type(id)))
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
        if let Some(output) = assemble_chunk(
            data,
            state.file_index,
            &file.sources,
            &file.filters,
            &state.scan_output,
            count,
        )? {
            *chunk = output;
            return Ok(true);
        }
    }
}

/// Applies the pushed-down filters to a chunk; returns the number of rows left if any
/// row was removed (the vectors are then sliced to them).
fn apply_filters(
    filters: &[(usize, TableFilter)],
    output: &mut [Vector],
    count: usize,
) -> Result<Option<usize>> {
    if filters.is_empty() {
        return Ok(None);
    }
    let mut sel: Vec<u32> = (0..count as u32).collect();
    for &(position, ref filter) in filters {
        let Some(vector) = output.get(position) else {
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
    function.get_virtual_columns = Some(Arc::new(read_parquet_virtual_columns));
    function.statistics = Some(Arc::new(read_parquet_statistics));
    function.get_partition_stats = Some(Arc::new(read_parquet_partition_stats));
    function.projected_output = true;
    function.parallel_scan = Some(read_parquet_parallel());
    function.file_filter_pushdown = Some(read_parquet_file_filter_pushdown());
    // `ParquetGetRowIdColumns`.
    function.get_row_id_columns = Some(Arc::new(|_| {
        vec![COLUMN_IDENTIFIER_FILE_INDEX, COLUMN_IDENTIFIER_FILE_ROW_NUMBER]
    }));
    function
}

/// `read_parquet` and `parquet_scan` (`ParquetScanFunction::GetFunctionSet`).
pub fn read_parquet_functions() -> Vec<TableFunctionSet> {
    ["read_parquet", "parquet_scan"]
        .into_iter()
        .map(|name| {
            let mut set = TableFunctionSet::new(name);
            set.add(read_parquet_function(name, LogicalType::VARCHAR));
            set.add(read_parquet_function(
                name,
                LogicalType::list(LogicalType::VARCHAR),
            ));
            set
        })
        .collect()
}

#[allow(dead_code, reason = "used by the schema mapping of nested columns")]
fn find_column<'a>(columns: &'a [ColumnDefinition], name: &str) -> Option<&'a ColumnDefinition> {
    columns
        .iter()
        .find(|column| column.name.eq_ignore_ascii_case(name))
}
