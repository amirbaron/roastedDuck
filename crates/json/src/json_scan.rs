//! The scan of the JSON table functions (DuckDB's `json_scan.cpp` and the scan half of
//! `json_multi_file_info.cpp`): the bind data, and a parallel scan that hands out files —
//! or byte ranges of large newline-delimited files — to threads, converting each batch of
//! values to the projected columns.

use std::any::Any;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{Error, Result};
use rustdb_csv::multi_file::{FileList, MultiFileBindData, MultiFileOptions, hive_parse};
use rustdb_function::cast::CastFunctionSet;
use rustdb_function::{
    FunctionData, ParallelScanGlobal, ParallelScanLocal, ParallelTableScan, TableScanState,
};
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use crate::json_enums::{JsonFormat, JsonRecordType, JsonScanType};
use crate::json_executors::{ColumnBuilder, StrBuilder};
use crate::json_functions::json_transform::{DateFormatMap, TransformOptions, transform, transform_object};
use crate::json_reader::{ByteRange, JsonReader, ScanBatch};
use crate::json_reader_options::JsonReaderOptions;
use crate::yyjson::Doc;

pub use rustdb_csv::multi_file::{COLUMN_IDENTIFIER_FILE_INDEX, COLUMN_IDENTIFIER_FILENAME};
/// The empty column of multi-file readers (`COLUMN_IDENTIFIER_EMPTY`).
const COLUMN_IDENTIFIER_EMPTY: u64 = u64::MAX - 1;

/// Newline-delimited files larger than this are split into ranges of this size for
/// parallel scans.
const RANGE_SIZE: u64 = 16 << 20;

/// How mismatches are treated when converting (the flags of `JSONTransformOptions` set in
/// `BindReader`).
#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "DuckDB's transform options")]
pub(crate) struct TransformFlags {
    pub(crate) strict_cast: bool,
    pub(crate) error_duplicate_key: bool,
    pub(crate) error_missing_key: bool,
    pub(crate) error_unknown_key: bool,
}

/// The bind data of the JSON readers (`MultiFileBindData` with `JSONScanData`).
#[derive(Debug)]
pub(crate) struct JsonScanData {
    pub(crate) files: FileList,
    pub(crate) file_options: MultiFileOptions,
    pub(crate) reader_bind: MultiFileBindData,
    pub(crate) options: JsonReaderOptions,
    /// The output columns, including the filename and hive partition columns.
    pub(crate) names: Vec<String>,
    pub(crate) types: Vec<LogicalType>,
    /// The JSON keys of the first `key_names.len()` columns.
    pub(crate) key_names: Vec<String>,
    pub(crate) date_format_map: Arc<DateFormatMap>,
    pub(crate) transform: TransformFlags,
    pub(crate) casts: Arc<CastFunctionSet>,
    pub(crate) estimated_cardinality_per_file: Option<u64>,
    /// The number of threads of the database.
    pub(crate) threads: u64,
}

impl FunctionData for JsonScanData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl JsonScanData {
    /// `JSONMultiFileInfo::GetCardinality`.
    pub(crate) fn cardinality(&self) -> u64 {
        self.estimated_cardinality_per_file.unwrap_or(42) * self.files.files.len() as u64
    }

    /// The type of scanned column `id`, virtual columns included.
    fn column_type(&self, id: u64) -> LogicalType {
        match id {
            COLUMN_IDENTIFIER_FILENAME => LogicalType::VARCHAR,
            COLUMN_IDENTIFIER_FILE_INDEX => LogicalType::UBIGINT,
            _ => usize::try_from(id)
                .ok()
                .and_then(|index| self.types.get(index).cloned())
                .unwrap_or(LogicalType::BOOLEAN),
        }
    }

    /// A copy scanning only the files at positions `keep` (`PruneReaders`).
    pub(crate) fn with_files(&self, keep: &[usize]) -> Self {
        Self {
            files: FileList {
                files: keep
                    .iter()
                    .filter_map(|&index| self.files.files.get(index).cloned())
                    .collect(),
            },
            file_options: self.file_options.clone(),
            reader_bind: self.reader_bind.clone(),
            options: self.options.clone(),
            names: self.names.clone(),
            types: self.types.clone(),
            key_names: self.key_names.clone(),
            date_format_map: Arc::clone(&self.date_format_map),
            transform: self.transform,
            casts: Arc::clone(&self.casts),
            estimated_cardinality_per_file: self.estimated_cardinality_per_file,
            threads: self.threads,
        }
    }
}

/// Where an output column comes from.
#[derive(Clone, Debug)]
enum ColumnSource {
    /// The converted JSON column at this position of the projected keys.
    Key(usize),
    Filename,
    FileIndex,
    /// A hive partition key.
    Hive(String),
    /// A column with no data (the empty column).
    Null(LogicalType),
}

/// The projection of a scan (the projection pushdown of `InitializeGlobalState`).
#[derive(Debug)]
struct Projection {
    sources: Vec<ColumnSource>,
    /// The keys converted, and their types.
    names: Vec<String>,
    types: Vec<LogicalType>,
    error_unknown_key: bool,
}

impl Projection {
    fn new(data: &JsonScanData, column_ids: &[u64]) -> Self {
        let mut sources = Vec::with_capacity(column_ids.len());
        let mut names = Vec::new();
        let mut types = Vec::new();
        for &id in column_ids {
            let source = match id {
                COLUMN_IDENTIFIER_FILENAME => ColumnSource::Filename,
                COLUMN_IDENTIFIER_FILE_INDEX => ColumnSource::FileIndex,
                COLUMN_IDENTIFIER_EMPTY => ColumnSource::Null(LogicalType::BOOLEAN),
                _ => {
                    let index = usize::try_from(id).unwrap_or(usize::MAX);
                    if data.reader_bind.filename_idx == Some(index) {
                        ColumnSource::Filename
                    } else if let Some((key, _)) = data
                        .reader_bind
                        .hive_partitioning_indexes
                        .iter()
                        .find(|(_, column)| *column == index)
                    {
                        ColumnSource::Hive(key.clone())
                    } else if let Some(name) = data.key_names.get(index) {
                        names.push(name.clone());
                        types.push(data.column_type(id));
                        ColumnSource::Key(names.len() - 1)
                    } else {
                        ColumnSource::Null(data.column_type(id))
                    }
                }
            };
            sources.push(source);
        }
        let error_unknown_key = data.transform.error_unknown_key
            && names.len() >= data.key_names.len()
            && !data.file_options.union_by_name;
        Self {
            sources,
            names,
            types,
            error_unknown_key,
        }
    }
}

/// One unit of work: a file, or a byte range of one.
#[derive(Clone, Copy, Debug)]
struct WorkUnit {
    file_index: usize,
    range: Option<ByteRange>,
    batch_index: u64,
    /// The detected format of the file (for ranges).
    format: JsonFormat,
}

/// The shared state of a scan: the next files and ranges to read.
#[derive(Debug)]
struct ScanGlobal {
    projection: Projection,
    queue: Mutex<WorkQueue>,
    max_threads: usize,
}

#[derive(Debug, Default)]
struct WorkQueue {
    next_file: usize,
    pending: VecDeque<WorkUnit>,
    next_batch: u64,
    failed: bool,
}

impl WorkQueue {
    /// The next unit; splits large newline-delimited files into ranges when the scan is
    /// parallel (`TryInitializeScan`).
    fn next(&mut self, data: &JsonScanData) -> Result<Option<WorkUnit>> {
        if let Some(unit) = self.pending.pop_front() {
            return Ok(Some(unit));
        }
        if self.next_file >= data.files.files.len() {
            return Ok(None);
        }
        let file_index = self.next_file;
        self.next_file += 1;
        let parallel = (data.files.files.len() as u64) < data.threads;
        let mut format = data.options.format;
        if parallel {
            let path = &data.files.files[file_index];
            let size = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
            let compressed = !matches!(
                data.options.compression.resolve(path),
                rustdb_csv::options::FileCompressionType::Uncompressed
            );
            if size > RANGE_SIZE && !compressed {
                if format == JsonFormat::AutoDetect {
                    let mut probe = JsonReader::new(path, &data.options, None);
                    probe.initialize()?;
                    format = probe.format;
                }
                if format == JsonFormat::NewlineDelimited {
                    let mut start = 0;
                    while start < size {
                        let end = (start + RANGE_SIZE).min(size);
                        self.pending.push_back(WorkUnit {
                            file_index,
                            range: Some(ByteRange { start, end }),
                            batch_index: self.next_batch,
                            format,
                        });
                        self.next_batch += 1;
                        start = end;
                    }
                    return Ok(self.pending.pop_front());
                }
            }
        }
        let unit = WorkUnit {
            file_index,
            range: None,
            batch_index: self.next_batch,
            format,
        };
        self.next_batch += 1;
        Ok(Some(unit))
    }
}

/// The state of one thread.
#[derive(Debug, Default)]
struct ScanLocal {
    reader: Option<(JsonReader, WorkUnit)>,
    batch: ScanBatch,
    batch_index: u64,
}

/// The bind data of a scan.
fn scan_data(bind_data: Option<&dyn FunctionData>) -> Result<&JsonScanData> {
    bind_data
        .and_then(|data| data.as_any().downcast_ref::<JsonScanData>())
        .ok_or_else(|| Error::internal("JSON scan without bind data"))
}

/// The number of threads that can usefully scan.
fn max_threads(data: &JsonScanData) -> usize {
    let files = data.files.files.len();
    if files > 1 {
        return files;
    }
    let size = data
        .files
        .first_file()
        .and_then(|path| std::fs::metadata(path).ok())
        .map_or(0, |metadata| metadata.len());
    usize::try_from(size / RANGE_SIZE + 1).unwrap_or(1)
}

fn init_global(
    bind_data: Option<&dyn FunctionData>,
    column_ids: &[u64],
    _: &[rustdb_types::ColumnIndex],
    _: Option<&rustdb_function::logging::OperatorLogger>,
) -> Result<ParallelScanGlobal> {
    let data = scan_data(bind_data)?;
    Ok(Arc::new(ScanGlobal {
        projection: Projection::new(data, column_ids),
        queue: Mutex::new(WorkQueue::default()),
        max_threads: max_threads(data),
    }))
}

fn global_state(global: &ParallelScanGlobal) -> Result<&ScanGlobal> {
    global
        .downcast_ref::<ScanGlobal>()
        .ok_or_else(|| Error::internal("JSON scan global state"))
}

/// The hint appended to conversion errors (`ReadJSONFunction`).
fn transform_hint(auto_detect: bool) -> &'static str {
    if auto_detect {
        "\nTry increasing 'sample_size', reducing 'maximum_depth', specifying 'columns', 'format' or 'records' manually, setting 'ignore_errors' to true, or setting 'union_by_name' to true when reading multiple files with a different structure."
    } else {
        "\nTry setting 'auto_detect' to true, specifying 'format' or 'records' manually, or setting 'ignore_errors' to true."
    }
}

/// Converts the values of a batch into the projected key columns (`ReadJSONFunction` /
/// `ReadJSONObjectsFunction`).
fn convert_batch(
    data: &JsonScanData,
    projection: &Projection,
    reader: &JsonReader,
    batch: &ScanBatch,
) -> Result<Vec<Vector>> {
    if projection.names.is_empty() {
        return Ok(Vec::new());
    }
    if data.options.scan_type == JsonScanType::ReadJsonObjects {
        let buffer = reader.buffer();
        let mut builder = StrBuilder::default();
        for (value, &(start, len)) in batch.values.iter().zip(&batch.units) {
            if value.is_some() {
                builder.push(&buffer[start..start + len]);
            } else {
                builder.push_null();
            }
        }
        let ty = projection.types.first().cloned().unwrap_or_else(crate::json_type);
        return Ok(vec![builder.finish(&ty)?]);
    }
    let doc = Doc::new(&batch.vals, reader.buffer());
    let mut options = TransformOptions::new(
        data.transform.strict_cast,
        data.transform.error_duplicate_key,
        data.transform.error_missing_key,
        data.transform.error_unknown_key && projection.error_unknown_key,
        Arc::clone(&data.casts),
    );
    options.delay_error = true;
    options.date_format_map = Some(Arc::clone(&data.date_format_map));
    let (columns, success) = if data.options.record_type == JsonRecordType::Records {
        let error_unknown_key = options.error_unknown_key;
        transform_object(
            &doc,
            &batch.values,
            &projection.names,
            &projection.types,
            &mut options,
            error_unknown_key,
        )?
    } else {
        let ty = projection.types.first().cloned().unwrap_or_else(crate::json_type);
        let (column, success) = transform(&doc, &batch.values, &ty, &mut options)?;
        (vec![column], success)
    };
    if !success {
        let mut message = options.error_message;
        message.extend_from_slice(transform_hint(data.options.auto_detect).as_bytes());
        return Err(reader.transform_error(batch, options.object_index, &message)?);
    }
    Ok(columns)
}

/// The output chunk of a batch: one vector per scanned column.
fn assemble(
    data: &JsonScanData,
    projection: &Projection,
    file_index: usize,
    columns: Vec<Vector>,
    count: usize,
) -> Result<DataChunk> {
    let mut columns: Vec<Option<Vector>> = columns.into_iter().map(Some).collect();
    let path = &data.files.files[file_index];
    let mut output = Vec::with_capacity(projection.sources.len());
    for source in &projection.sources {
        output.push(match source {
            ColumnSource::Key(index) => match columns.get_mut(*index).and_then(Option::take) {
                Some(column) => column,
                None => Vector::constant_null(projection.types[*index].clone()),
            },
            ColumnSource::Filename => Vector::constant(&Value::varchar(path))?,
            ColumnSource::FileIndex => Vector::constant(&Value::ubigint(file_index as u64))?,
            ColumnSource::Hive(key) => {
                let value = hive_parse(path).get(key).cloned().unwrap_or_default();
                let constant = data.file_options.hive_partition_value(&value, key, &data.casts)?;
                Vector::constant(&constant)?
            }
            ColumnSource::Null(ty) => Vector::constant_null(ty.clone()),
        });
    }
    Ok(DataChunk::new(output, count))
}

/// Scans the next chunk for one thread; `false` once there is no more work.
fn scan_next(
    data: &JsonScanData,
    global: &ScanGlobal,
    local: &mut ScanLocal,
    chunk: &mut DataChunk,
) -> Result<bool> {
    loop {
        if let Some((reader, unit)) = local.reader.as_mut() {
            let count = reader.scan(&mut local.batch)?;
            if count > 0 {
                let columns = convert_batch(data, &global.projection, reader, &local.batch)?;
                *chunk = assemble(data, &global.projection, unit.file_index, columns, count)?;
                local.batch_index = unit.batch_index;
                return Ok(true);
            }
            local.reader = None;
        }
        let next = {
            let mut queue = global.queue.lock().unwrap_or_else(PoisonError::into_inner);
            if queue.failed {
                return Ok(false);
            }
            match queue.next(data) {
                Ok(next) => next,
                Err(error) => {
                    queue.failed = true;
                    return Err(error);
                }
            }
        };
        let Some(unit) = next else {
            return Ok(false);
        };
        let mut options = data.options.clone();
        if unit.range.is_some() {
            options.format = unit.format;
        }
        let reader = JsonReader::new(&data.files.files[unit.file_index], &options, unit.range);
        local.reader = Some((reader, unit));
    }
}

fn parallel_scan(
    bind_data: Option<&dyn FunctionData>,
    _column_ids: &[u64],
    global: &ParallelScanGlobal,
    local: &mut ParallelScanLocal,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = scan_data(bind_data)?;
    let global = global_state(global)?;
    let local = local
        .downcast_mut::<ScanLocal>()
        .ok_or_else(|| Error::internal("JSON scan local state"))?;
    let result = scan_next(data, global, local, chunk);
    if result.is_err() {
        global.queue.lock().unwrap_or_else(PoisonError::into_inner).failed = true;
    }
    result
}

/// The parallel scan of the JSON readers.
pub(crate) fn json_parallel_scan() -> ParallelTableScan {
    ParallelTableScan {
        init_global: Arc::new(init_global),
        init_local: Arc::new(|_| Ok(Box::new(ScanLocal::default()) as ParallelScanLocal)),
        scan: Arc::new(parallel_scan),
        max_threads: Arc::new(|global| global_state(global).map_or(1, |global| global.max_threads)),
        batch_index: Arc::new(|_, local| {
            local
                .downcast_ref::<ScanLocal>()
                .map_or(0, |local| local.batch_index)
        }),
        partition_data: None,
        dynamic_to_string: None,
        row_group_metrics: None,
    }
}

/// The single-threaded scan of the JSON readers.
pub(crate) fn json_scan(
    bind_data: Option<&dyn FunctionData>,
    column_ids: &[u64],
    scan_state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = scan_data(bind_data)?;
    if scan_state.local.is_none() {
        let global = ScanGlobal {
            projection: Projection::new(data, column_ids),
            queue: Mutex::new(WorkQueue::default()),
            max_threads: 1,
        };
        scan_state.local = Some(Box::new((global, ScanLocal::default())));
    }
    let (global, local) = scan_state
        .local
        .as_mut()
        .and_then(|state| state.downcast_mut::<(ScanGlobal, ScanLocal)>())
        .ok_or_else(|| Error::internal("JSON scan state"))?;
    if scan_next(data, global, local, chunk)? {
        return Ok(true);
    }
    *chunk = DataChunk::new(
        column_ids
            .iter()
            .map(|&id| Vector::constant_null(data.column_type(id)))
            .collect(),
        0,
    );
    Ok(false)
}

/// `MultiFileGetVirtualColumns`.
pub(crate) fn json_virtual_columns(bind_data: Option<&dyn FunctionData>) -> Vec<(u64, String, LogicalType)> {
    let filename_option = bind_data
        .and_then(|data| data.as_any().downcast_ref::<JsonScanData>())
        .is_some_and(|data| data.reader_bind.filename_idx.is_some());
    rustdb_csv::multi_file::multi_file_virtual_columns(filename_option)
}
