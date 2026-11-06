//! `COPY ... TO` (DuckDB's `PhysicalCopyToFile`): writes its input through a copy
//! function into one file, a file per partition (`PARTITION_BY`), per size
//! (`FILE_SIZE_BYTES`) or per thread, and returns the number of rows (and files).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use super::batch_memory_manager::BatchMemoryManager;
use rustdb_buffer::{BufferManager, MemoryTag};
use rustdb_common::{ByteString, Error, ErrorKind, Result};
use rustdb_function::copy::{CopyFunctionFileStatistics, CopyGlobalState};
use rustdb_function::file_system::native_path;
use rustdb_function::{CopyFunction, FunctionData};
use rustdb_planner::operator::{CopyFunctionReturnType, CopyOverwriteMode, FilenamePattern};
use rustdb_row::{ColumnDataAppendState, ColumnDataCollection};
use rustdb_types::{DataChunk, LogicalType, SelectionVector, Value, Vector};

use crate::operator::{
    ErasedSink, ErasedSource, OperatorPartitionInfo, PartitionInfo, PhysicalOperator, PhysicalRef, SinkState,
};
use crate::{ExecutionContext, Sink, SinkFinalize, SinkResult, Source, SourceResult};

fn io_error(message: impl Into<ByteString>) -> Error {
    Error::new_bytes(ErrorKind::Io, message.into())
}

/// The settings of a `COPY ... TO` (the fields of DuckDB's `PhysicalCopyToFile`).
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct CopyToFileOptions {
    pub file_path: ByteString,
    pub use_tmp_file: bool,
    pub overwrite_mode: CopyOverwriteMode,
    pub filename_pattern: FilenamePattern,
    pub file_extension: String,
    pub per_thread_output: bool,
    pub file_size_bytes: Option<u64>,
    pub rotate: bool,
    pub partition_output: bool,
    pub partition_columns: Vec<usize>,
    pub write_partition_columns: bool,
    pub write_empty_file: bool,
    pub hive_file_pattern: bool,
    pub return_type: CopyFunctionReturnType,
    pub names: Vec<rustdb_common::ByteString>,
    pub expected_types: Vec<LogicalType>,
}

/// Writes its input to files (DuckDB's `PhysicalCopyToFile`).
#[derive(Debug)]
pub struct PhysicalCopyToFile {
    types: Vec<LogicalType>,
    children: [PhysicalRef; 1],
    function: CopyFunction,
    bind_data: Arc<dyn FunctionData>,
    options: CopyToFileOptions,
    /// Whether threads prepare batches written in batch order (DuckDB's
    /// `PhysicalBatchCopyToFile`).
    batch: bool,
    /// Whether threads write row groups of their own rows, in any order
    /// (`PARALLEL_COPY_TO_FILE`).
    parallel: bool,
}

impl PhysicalCopyToFile {
    pub fn new(
        child: PhysicalRef,
        function: CopyFunction,
        bind_data: Arc<dyn FunctionData>,
        options: CopyToFileOptions,
    ) -> Self {
        Self {
            types: options.return_type.types(),
            children: [child],
            function,
            bind_data,
            options,
            batch: false,
            parallel: false,
        }
    }

    /// Each thread writes row groups of its own rows (`PARALLEL_COPY_TO_FILE`).
    #[must_use]
    pub fn with_parallel_writes(mut self) -> Self {
        self.parallel = true;
        self
    }

    /// Writes batches prepared by several threads, in batch order
    /// (`BATCH_COPY_TO_FILE`).
    #[must_use]
    pub fn with_batches(mut self) -> Self {
        self.batch = true;
        self
    }

    /// Opens a file; `logger` (the operator's, if the query logs it) goes to the format's
    /// `initialize_operator`.
    fn initialize_global(
        &self,
        path: &[u8],
        logger: Option<&rustdb_function::logging::OperatorLogger>,
    ) -> Result<CopyGlobalState> {
        let initialize = self
            .function
            .copy_to_initialize_global
            .as_ref()
            .ok_or_else(|| Error::internal("copy function without a writer"))?;
        let state = initialize(self.bind_data.as_ref(), path)?;
        if let (Some(initialize_operator), Some(logger)) =
            (&self.function.copy_to_initialize_operator, logger)
        {
            initialize_operator(&state, logger);
        }
        Ok(state)
    }

    fn sink_into(&self, state: &CopyGlobalState, chunk: &DataChunk) -> Result<()> {
        let sink = self
            .function
            .copy_to_sink
            .as_ref()
            .ok_or_else(|| Error::internal("copy function without a sink"))?;
        sink(self.bind_data.as_ref(), state, chunk)
    }

    fn finalize_file(&self, state: &CopyGlobalState) -> Result<()> {
        match &self.function.copy_to_finalize {
            Some(finalize) => finalize(self.bind_data.as_ref(), state),
            None => Ok(()),
        }
    }

    /// `GetTrimmedPath`.
    fn trimmed_path(&self) -> ByteString {
        let bytes = self.options.file_path.as_bytes();
        let end = bytes.iter().rposition(|byte| *byte != b'/').map_or(0, |i| i + 1);
        bytes[..end].into()
    }

    /// `CreateFileState`: a new numbered file in the output directory.
    fn create_file_state(&self, inner: &mut CopyInner) -> Result<OpenFile> {
        let offset = inner.last_file_offset;
        inner.last_file_offset += 1;
        let output_path = self.options.filename_pattern.create_filename(
            &self.options.file_path,
            &self.options.file_extension,
            offset,
        );
        inner.created_files.push(output_path.clone());
        let file = (self.options.return_type != CopyFunctionReturnType::ChangedRows)
            .then(|| inner.add_file(output_path.clone(), Value::default()));
        Ok(OpenFile {
            state: Arc::new(self.initialize_global(&output_path, inner.logger.as_ref())?),
            file,
        })
    }

    /// `CopyToFunctionGlobalState::Initialize`: opens the single output file.
    fn initialize(&self, inner: &mut CopyInner) -> Result<()> {
        if inner.initialized {
            return Ok(());
        }
        inner.created_files.push(self.options.file_path.clone());
        let state = Arc::new(self.initialize_global(&self.options.file_path, inner.logger.as_ref())?);
        let file = inner.add_file(self.options.file_path.clone(), Value::default());
        inner.global_state = Some(OpenFile {
            state,
            file: Some(file),
        });
        inner.initialized = true;
        Ok(())
    }

    /// Finalizes a file and collects its statistics for `RETURN_STATS`
    /// (`copy_to_get_written_statistics`).
    fn finish_file(&self, inner: &mut CopyInner, file: &OpenFile) -> Result<()> {
        self.finalize_file(&file.state)?;
        if self.options.return_type == CopyFunctionReturnType::WrittenFileStatistics
            && let (Some(index), Some(statistics)) =
                (file.file, &self.function.copy_to_get_written_statistics)
        {
            let stats = statistics(self.bind_data.as_ref(), &file.state)?;
            if let Some(entry) = inner.written_files.get_mut(index) {
                entry.stats = Some(stats);
            }
        }
        Ok(())
    }

    /// The writer of a partition (`GetPartitionWriteInfo`), creating its directories.
    fn partition_state<'a>(
        &self,
        inner: &'a mut CopyInner,
        key: &PartitionKey,
    ) -> Result<&'a CopyGlobalState> {
        if !inner.partitions.contains_key(key) {
            let mut path = self.trimmed_path();
            create_dir(&mut inner.created_directories, &path)?;
            if self.options.hive_file_pattern {
                for (column, value) in self.options.partition_columns.iter().zip(&key.values) {
                    let name = &self.options.names[*column];
                    let value = if value.is_null() {
                        "__HIVE_DEFAULT_PARTITION__".to_owned()
                    } else {
                        url_encode(value.to_varchar_bytes()?)
                    };
                    path = rustdb_common::byte_format!("{path}/{}={value}", url_encode(name));
                    create_dir(&mut inner.created_directories, &path)?;
                }
            }
            let offset = if self.options.hive_file_pattern {
                0
            } else {
                let offset = inner.global_offset;
                inner.global_offset += 1;
                offset
            };
            let mut full_path =
                self.options
                    .filename_pattern
                    .create_filename(&path, &self.options.file_extension, offset);
            if self.options.overwrite_mode == CopyOverwriteMode::Append {
                while native_path(&full_path)?.exists() {
                    if !self.options.filename_pattern.has_uuid() {
                        return Err(Error::internal(
                            "CopyOverwriteMode::COPY_APPEND without {uuid} - and file exists",
                        ));
                    }
                    full_path = self.options.filename_pattern.create_filename(
                        &path,
                        &self.options.file_extension,
                        offset,
                    );
                }
            }
            inner.created_files.push(full_path.clone());
            let file = if self.options.return_type == CopyFunctionReturnType::ChangedRows {
                None
            } else {
                // The partition's values as text (`partition_keys`).
                let mut entries = Vec::with_capacity(key.values.len());
                for (column, value) in self.options.partition_columns.iter().zip(&key.values) {
                    let text = if value.is_null() {
                        Value::null(LogicalType::VARCHAR)
                    } else {
                        Value::varchar(value.to_varchar()?)
                    };
                    entries.push((Value::try_varchar(&self.options.names[*column])?, text));
                }
                let keys = Value::map(LogicalType::VARCHAR, LogicalType::VARCHAR, entries);
                Some(inner.add_file(full_path.clone(), keys))
            };
            let state = self.initialize_global(&full_path, inner.logger.as_ref())?;
            inner.partition_order.push(key.clone());
            inner.partitions.insert(
                key.clone(),
                OpenFile {
                    state: Arc::new(state),
                    file,
                },
            );
        }
        inner
            .partitions
            .get(key)
            .map(|file| &*file.state)
            .ok_or_else(|| Error::internal("partition writer disappeared"))
    }

    /// Writes a chunk split by its partition values.
    fn sink_partitioned(&self, inner: &mut CopyInner, chunk: &DataChunk) -> Result<()> {
        let mut rows_per_key: Vec<(PartitionKey, Vec<u32>)> = Vec::new();
        let mut key_index: HashMap<PartitionKey, usize> = HashMap::new();
        for row in 0..chunk.len() {
            let values: Vec<Value> = self
                .options
                .partition_columns
                .iter()
                .map(|column| chunk.column(*column).get_value(row))
                .collect();
            let key = PartitionKey::new(values)?;
            let index = *key_index.entry(key.clone()).or_insert_with(|| {
                rows_per_key.push((key, Vec::new()));
                rows_per_key.len() - 1
            });
            rows_per_key[index].1.push(row as u32);
        }
        for (key, rows) in rows_per_key {
            let count = rows.len();
            let sel = SelectionVector::new(rows);
            let columns: Vec<Vector> = chunk
                .columns()
                .iter()
                .enumerate()
                .filter(|(i, _)| {
                    self.options.write_partition_columns || !self.options.partition_columns.contains(i)
                })
                .map(|(_, column)| column.slice(&sel))
                .collect();
            let part = DataChunk::new(columns, count);
            let state = self.partition_state(inner, &key)?;
            self.sink_into(state, &part)?;
        }
        Ok(())
    }

    /// Prepares the output directory (`GetGlobalSinkState` for directory outputs).
    fn prepare_directory(&self) -> Result<()> {
        if let Some(fs) = rustdb_function::registered_files::lookup(&self.options.file_path) {
            if !fs.is_remote() && fs.is_file(&self.options.file_path)? {
                if self.options.overwrite_mode == CopyOverwriteMode::Overwrite {
                    fs.remove(&self.options.file_path)?;
                } else {
                    return Err(io_error("Cannot write to an existing file as a directory"));
                }
            }
            if fs.is_dir(&self.options.file_path)? {
                check_directory(&self.options.file_path, self.options.overwrite_mode)?;
            } else {
                fs.mkdir(&self.options.file_path)?;
            }
            return Ok(());
        }
        let path = native_path(&self.options.file_path)?;
        if path.is_file() {
            if self.options.overwrite_mode == CopyOverwriteMode::Overwrite {
                std::fs::remove_file(&path).map_err(|error| {
                    io_error(rustdb_common::byte_format!(
                        "Could not remove file \"{}\": {error}",
                        self.options.file_path
                    ))
                })?;
            } else {
                return Err(io_error(rustdb_common::byte_format!(
                    "Cannot write to \"{}\" - it exists and is a file, not a directory! Enable OVERWRITE option to overwrite the file",
                    self.options.file_path
                )));
            }
        }
        if path.is_dir() {
            check_directory(&self.options.file_path, self.options.overwrite_mode)
        } else {
            std::fs::create_dir_all(&path).map_err(|error| {
                io_error(rustdb_common::byte_format!(
                    "Failed to create directory \"{}\": {error}",
                    self.options.file_path
                ))
            })
        }
    }

    /// The name of a written file as the copy returns it.
    fn returned_path(&self, path: &[u8]) -> ByteString {
        if self.options.use_tmp_file {
            Self::non_tmp_file(path)
        } else {
            path.into()
        }
    }

    /// `GetData` with `RETURN_STATS` (`ReturnStatistics`): a row per written file, from
    /// file `offset` on.
    fn return_statistics(
        &self,
        inner: &CopyInner,
        chunk: &mut DataChunk,
        offset: &AtomicUsize,
    ) -> Result<SourceResult> {
        let start = offset.load(std::sync::atomic::Ordering::Relaxed);
        let end = (start + rustdb_common::STANDARD_VECTOR_SIZE).min(inner.written_files.len());
        let stats_map_type = LogicalType::map(LogicalType::VARCHAR, LogicalType::VARCHAR);
        let mut columns: Vec<Vector> = self
            .types
            .iter()
            .map(|ty| Vector::flat(ty.clone(), (end - start).max(1)))
            .collect();
        for (row, file) in inner.written_files[start..end].iter().enumerate() {
            let default = CopyFunctionFileStatistics::default();
            let stats = file.stats.as_ref().unwrap_or(&default);
            // `CreateColumnStatistics`: columns and statistics in name order.
            let mut column_stats: Vec<&(rustdb_common::ByteString, Vec<(String, Value)>)> =
                stats.column_statistics.iter().collect();
            column_stats.sort_by(|a, b| a.0.cmp(&b.0));
            let mut entries = Vec::with_capacity(column_stats.len());
            for (column, values) in column_stats {
                let mut values: Vec<&(String, Value)> = values.iter().collect();
                values.sort_by(|a, b| a.0.cmp(&b.0));
                let mut inner_entries = Vec::with_capacity(values.len());
                for (name, value) in values {
                    let text = if value.is_null() {
                        Value::null(LogicalType::VARCHAR)
                    } else {
                        Value::varchar(value.to_varchar()?)
                    };
                    inner_entries.push((Value::varchar(name), text));
                }
                entries.push((
                    Value::try_varchar(column)?,
                    Value::map(LogicalType::VARCHAR, LogicalType::VARCHAR, inner_entries),
                ));
            }
            let footer = if stats.footer_size_bytes.is_null() {
                Value::null(LogicalType::UBIGINT)
            } else {
                stats.footer_size_bytes.clone()
            };
            let partition_keys = if file.partition_keys.is_null() {
                Value::null(LogicalType::map(LogicalType::VARCHAR, LogicalType::VARCHAR))
            } else {
                file.partition_keys.clone()
            };
            let values = [
                Value::try_varchar(&self.returned_path(&file.path))?,
                Value::ubigint(stats.row_count),
                Value::ubigint(stats.file_size_bytes),
                footer,
                Value::map(LogicalType::VARCHAR, stats_map_type.clone(), entries),
                partition_keys,
            ];
            for (column, value) in columns.iter_mut().zip(&values) {
                column.set_value(row, value)?;
            }
        }
        chunk.set_columns(columns, end - start);
        offset.store(end, std::sync::atomic::Ordering::Relaxed);
        Ok(if end < inner.written_files.len() {
            SourceResult::HaveMoreOutput
        } else {
            SourceResult::Finished
        })
    }

    /// `GetNonTmpFile`: the final name of a file written as `tmp_<name>`.
    fn non_tmp_file(path: &[u8]) -> ByteString {
        let (directory, base) = match path.iter().rposition(|byte| *byte == b'/') {
            Some(slash) => (&path[..=slash], &path[slash + 1..]),
            None => (b"".as_slice(), path),
        };
        let base = base.strip_prefix(b"tmp_").unwrap_or(base);
        rustdb_common::byte_format!("{directory}{base}")
    }
}

/// A partition's values, compared by their text (NULLs equal).
#[derive(Clone, Debug)]
struct PartitionKey {
    text: Vec<Option<ByteString>>,
    values: Vec<Value>,
}

impl PartitionKey {
    fn new(values: Vec<Value>) -> Result<Self> {
        let text = values
            .iter()
            .map(|value| {
                if value.is_null() {
                    Ok(None)
                } else {
                    value.to_varchar_bytes().map(Some)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { text, values })
    }
}

impl PartialEq for PartitionKey {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl Eq for PartitionKey {}

impl std::hash::Hash for PartitionKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.text.hash(state);
    }
}

/// `StringUtil::URLEncode` as `HivePartitioning::Escape` uses it.
fn url_encode(input: impl AsRef<[u8]>) -> String {
    let mut result = String::new();
    for &byte in input.as_ref() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            result.push(char::from(byte));
        } else {
            let _ = write!(result, "%{byte:02X}");
        }
    }
    result
}

fn create_dir(created: &mut Vec<ByteString>, path: &[u8]) -> Result<()> {
    if let Some(fs) = rustdb_function::registered_files::lookup(path) {
        if !created.iter().any(|p| p.as_bytes() == path) {
            if !fs.is_dir(path)? {
                fs.mkdir(path)?;
            }
            created.push(path.into());
        }
        return Ok(());
    }
    if created.iter().any(|existing| existing.as_bytes() == path) {
        return Ok(());
    }
    if !native_path(path)?.is_dir() {
        std::fs::create_dir_all(native_path(path)?).map_err(|error| {
            io_error(rustdb_common::byte_format!(
                "Failed to create directory \"{path}\": {error}"
            ))
        })?;
    }
    created.push(path.into());
    Ok(())
}

/// `CheckDirectory`: an existing output directory must be empty unless overwriting.
fn check_directory(path: &[u8], overwrite_mode: CopyOverwriteMode) -> Result<()> {
    if let Some(fs) = rustdb_function::registered_files::lookup(path) {
        if matches!(
            overwrite_mode,
            CopyOverwriteMode::OverwriteOrIgnore | CopyOverwriteMode::Append
        ) {
            return Ok(());
        }
        let files = fs.list_files_recursive(path)?;
        if files.is_empty() {
            return Ok(());
        }
        if overwrite_mode == CopyOverwriteMode::Overwrite {
            return fs.remove_files(&files);
        }
        return Err(io_error(rustdb_common::byte_format!(
            "Directory \"{path}\" is not empty! Enable OVERWRITE option to overwrite files"
        )));
    }
    if matches!(
        overwrite_mode,
        CopyOverwriteMode::OverwriteOrIgnore | CopyOverwriteMode::Append
    ) {
        return Ok(());
    }
    let mut files = Vec::new();
    let mut directories = vec![native_path(path)?];
    while let Some(directory) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let full = entry.path();
            if full.is_dir() {
                directories.push(full);
            } else {
                files.push(full);
            }
        }
    }
    if files.is_empty() {
        return Ok(());
    }
    if overwrite_mode == CopyOverwriteMode::Overwrite {
        for file in files {
            let _ = std::fs::remove_file(file);
        }
        return Ok(());
    }
    Err(io_error(rustdb_common::byte_format!(
        "Directory \"{path}\" is not empty! Enable OVERWRITE option to overwrite files"
    )))
}

/// An open output file: the function's state and the file's index in
/// `written_files`, if its name is returned.
pub struct OpenFile {
    state: Arc<CopyGlobalState>,
    file: Option<usize>,
}

impl std::fmt::Debug for OpenFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenFile")
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

/// A file a copy wrote (DuckDB's `CopyToFileInfo`).
#[derive(Debug)]
struct WrittenFile {
    path: ByteString,
    /// The partition's values, a MAP(VARCHAR, VARCHAR) (NULL without partitions).
    partition_keys: Value,
    stats: Option<CopyFunctionFileStatistics>,
}

/// The mutable state of a copy (DuckDB's `CopyToFunctionGlobalState`).
#[derive(Default)]
struct CopyInner {
    initialized: bool,
    finalized: bool,
    rows_copied: u64,
    last_file_offset: u64,
    global_offset: u64,
    global_state: Option<OpenFile>,
    created_files: Vec<ByteString>,
    written_files: Vec<WrittenFile>,
    created_directories: Vec<ByteString>,
    partitions: HashMap<PartitionKey, OpenFile>,
    partition_order: Vec<PartitionKey>,
    /// The operator's logger, if the query logs it; the files' writers log with it.
    logger: Option<rustdb_function::logging::OperatorLogger>,
}

impl CopyInner {
    /// `AddFile`: records a file whose name (and statistics) the copy returns.
    fn add_file(&mut self, path: ByteString, partition_keys: Value) -> usize {
        self.written_files.push(WrittenFile {
            path,
            partition_keys,
            stats: None,
        });
        self.written_files.len() - 1
    }
}

impl std::fmt::Debug for CopyInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopyInner")
            .field("rows_copied", &self.rows_copied)
            .field("created_files", &self.created_files)
            .finish_non_exhaustive()
    }
}

/// The global state of a copy; files created by a failed copy are removed.
#[derive(Debug, Default)]
pub struct CopyGlobal {
    inner: Mutex<CopyInner>,
    /// The batches of a batched copy.
    batch: BatchCopy,
}

/// Buffer-managed rows and their original allocation charge (`FixedRawBatchData`).
#[derive(Debug)]
struct RawBatch {
    collection: ColumnDataCollection,
    memory_usage: u64,
}

impl RawBatch {
    fn new(buffer_manager: BufferManager, types: Vec<LogicalType>) -> Result<Self> {
        let mut collection = ColumnDataCollection::new(buffer_manager, types, MemoryTag::ColumnData)?;
        collection.set_partition_index(0)?;
        Ok(Self {
            collection,
            memory_usage: 0,
        })
    }
    fn count(&self) -> usize {
        self.collection.count()
    }
}

impl rustdb_function::copy::CopyBatch for RawBatch {
    fn count(&self) -> usize {
        self.count()
    }
    fn scan(&self, column: Option<usize>, visit: &mut dyn FnMut(&DataChunk) -> Result<()>) -> Result<()> {
        let mut state = if let Some(index) = column {
            self.collection.initialize_scan_columns(&[index])?
        } else {
            self.collection.initialize_scan()
        };
        while let Some(chunk) = self.collection.scan(&mut state)? {
            visit(&chunk)?;
        }
        Ok(())
    }
}

/// Work of a batched copy (`BatchCopyTask`).
#[derive(Debug)]
enum BatchTask {
    /// `PrepareBatchTask`.
    Prepare { batch_index: u64, batch: RawBatch },
    /// `RepartitionedFlushTask`.
    Flush,
}

/// The batch state of a batched copy (`FixedBatchCopyGlobalState`).
#[derive(Default)]
struct BatchState {
    /// The rows per prepared batch (`desired_batch_size`; 0: any).
    batch_size: usize,
    /// Batches not yet repartitioned, by batch index.
    raw_batches: BTreeMap<u64, RawBatch>,
    tasks: VecDeque<BatchTask>,
    /// Prepared batches ready to be written, by their index in the output.
    prepared: BTreeMap<u64, (u64, rustdb_function::copy::PreparedBatch)>,
    scheduled_batch_index: u64,
    flushed_batch_index: u64,
    /// Whether a thread has finished sinking (`any_finished`).
    any_finished: bool,
    /// Whether the final flush after the parallel finalize stage is pending.
    final_flush_pending: bool,
}

impl std::fmt::Debug for BatchState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchState")
            .field("scheduled_batch_index", &self.scheduled_batch_index)
            .field("flushed_batch_index", &self.flushed_batch_index)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct BatchCopy {
    memory_manager: Option<BatchMemoryManager>,
    minimum_memory_per_thread: u64,
    buffer_manager: Option<BufferManager>,
    state: Mutex<BatchState>,
    /// Whether a thread is writing prepared batches (`any_flushing`).
    flushing: AtomicBool,
}

/// One thread's state of a copy: its own file with `PER_THREAD_OUTPUT`, or the rows of
/// its current batch (`FixedBatchCopyLocalState`).
#[derive(Debug, Default)]
pub struct CopyLocal {
    file: Option<OpenFile>,
    /// The function's state of this thread (parallel writes).
    function_local: Option<rustdb_function::copy::CopyLocalState>,
    batch_index: u64,
    min_batch_index: u64,
    batch: Option<RawBatch>,
    append_state: ColumnDataAppendState,
    /// Retained across a blocked sink (`PROCESSING_TASKS`).
    processing_tasks: bool,
    rows_copied: u64,
}

/// `CorrectSizeForBatch`: within a vector of the desired size.
fn correct_size_for_batch(count: usize, desired: usize) -> bool {
    desired == 0 || count.abs_diff(desired) < rustdb_common::STANDARD_VECTOR_SIZE
}

impl PhysicalCopyToFile {
    /// The function state of the single output file, opening it on first use
    /// (`FixedBatchCopyGlobalState::Initialize`).
    fn batch_file_state(&self, global: &CopyGlobal) -> Result<Arc<CopyGlobalState>> {
        let mut inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
        self.initialize(&mut inner)?;
        inner
            .global_state
            .as_ref()
            .map(|file| Arc::clone(&file.state))
            .ok_or_else(|| Error::internal("batched copy without an open file"))
    }

    /// `RepartitionBatches`: merges and splits the complete raw batches (those below
    /// `min_index`) into batches of the desired size, as tasks.
    fn repartition_batches(
        state: &mut BatchState,
        min_index: u64,
        final_batch: bool,
        buffer_manager: &BufferManager,
        types: &[LogicalType],
    ) -> Result<()> {
        if state.raw_batches.is_empty() {
            return Ok(());
        }
        if !final_batch {
            if state.any_finished {
                return Ok(());
            }
            let candidate_rows: usize = state
                .raw_batches
                .range(..min_index)
                .map(|(_, batch)| batch.count())
                .sum();
            if candidate_rows < state.batch_size {
                return Ok(());
            }
        }
        let indexes: Vec<u64> = state
            .raw_batches
            .range(..min_index)
            .map(|(&index, _)| index)
            .collect();
        let Some(&max_batch_index) = indexes.last() else {
            return Ok(());
        };
        let batch_size = state.batch_size;
        let mut append: Option<RawBatch> = None;
        let mut append_state = ColumnDataAppendState::default();
        for index in indexes {
            let Some(current) = state.raw_batches.remove(&index) else {
                continue;
            };
            if append.is_none() {
                if correct_size_for_batch(current.count(), batch_size) {
                    let batch_index = state.scheduled_batch_index;
                    state.scheduled_batch_index += 1;
                    state.tasks.push_back(BatchTask::Prepare {
                        batch_index,
                        batch: current,
                    });
                    continue;
                }
                if current.count() < batch_size {
                    append = Some(current);
                    continue;
                }
                append = Some(RawBatch::new(buffer_manager.clone(), types.to_vec())?);
            }
            // Preserve the original charge through repartitioning, as C++ does;
            // a source's charge transfers to the first output it contributes to.
            if let Some(target) = append.as_mut() {
                target.memory_usage += current.memory_usage;
            }
            let mut scan = current.collection.initialize_scan();
            while let Some(chunk) = current.collection.scan(&mut scan)? {
                let target = append
                    .as_mut()
                    .ok_or_else(|| Error::internal("COPY repartition without target"))?;
                target.collection.append(&mut append_state, &chunk)?;
                if target.count() >= batch_size {
                    target.collection.finalize_append(&mut append_state);
                    let full = append
                        .replace(RawBatch::new(buffer_manager.clone(), types.to_vec())?)
                        .ok_or_else(|| Error::internal("COPY repartition lost target"))?;
                    let batch_index = state.scheduled_batch_index;
                    state.scheduled_batch_index += 1;
                    state.tasks.push_back(BatchTask::Prepare {
                        batch_index,
                        batch: full,
                    });
                }
            }
        }
        if let Some(rest) = append {
            rest.collection.finalize_append(&mut append_state);
            if rest.count() > 0 {
                if final_batch || correct_size_for_batch(rest.count(), batch_size) {
                    let batch_index = state.scheduled_batch_index;
                    state.scheduled_batch_index += 1;
                    state.tasks.push_back(BatchTask::Prepare {
                        batch_index,
                        batch: rest,
                    });
                } else {
                    state.raw_batches.insert(max_batch_index, rest);
                }
            }
        }
        Ok(())
    }

    /// `ExecuteTask`: runs one queued task; returns whether there was one.
    fn execute_batch_task(&self, global: &CopyGlobal) -> Result<bool> {
        let task = {
            let mut state = global.batch.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.tasks.pop_front()
        };
        let Some(task) = task else {
            return Ok(false);
        };
        match task {
            BatchTask::Prepare { batch_index, batch } => {
                let prepare = self
                    .function
                    .prepare_batch
                    .as_ref()
                    .ok_or_else(|| Error::internal("batched copy without prepare_batch"))?;
                let file_state = self.batch_file_state(global)?;
                let memory_usage = batch.memory_usage;
                let prepared = prepare(self.bind_data.as_ref(), &file_state, &batch)?;
                drop(batch);
                let mut state = global.batch.state.lock().unwrap_or_else(PoisonError::into_inner);
                if state
                    .prepared
                    .insert(batch_index, (memory_usage, prepared))
                    .is_some()
                {
                    return Err(Error::internal(rustdb_common::byte_format!(
                        "Duplicate batch index {batch_index} encountered in PhysicalFixedBatchCopy"
                    )));
                }
                if batch_index == state.flushed_batch_index {
                    state.tasks.push_back(BatchTask::Flush);
                }
            }
            BatchTask::Flush => self.flush_batch_data(global)?,
        }
        Ok(true)
    }

    /// `FlushBatchData`: writes the prepared batches that are next in order, on one
    /// thread at a time.
    fn flush_batch_data(&self, global: &CopyGlobal) -> Result<()> {
        if global.batch.flushing.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let result = (|| -> Result<()> {
            let flush = self
                .function
                .flush_batch
                .as_ref()
                .ok_or_else(|| Error::internal("batched copy without flush_batch"))?;
            loop {
                let batch = {
                    let mut state = global.batch.state.lock().unwrap_or_else(PoisonError::into_inner);
                    let next = state.flushed_batch_index;
                    match state.prepared.first_key_value() {
                        Some((&index, _)) if index == next => state.prepared.remove(&index),
                        _ => None,
                    }
                };
                let Some((memory_usage, batch)) = batch else {
                    return Ok(());
                };
                let file_state = self.batch_file_state(global)?;
                flush(self.bind_data.as_ref(), &file_state, batch)?;
                if let Some(manager) = &global.batch.memory_manager {
                    manager.reduce_unflushed_memory(memory_usage)?;
                    manager.unblock_tasks();
                }
                let mut state = global.batch.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.flushed_batch_index += 1;
            }
        })();
        global.batch.flushing.store(false, Ordering::Release);
        result
    }

    /// `AddLocalBatch`: hands the thread's finished batch to the global state, then
    /// helps preparing and writing batches.
    fn add_local_batch(&self, global: &CopyGlobal, local: &mut CopyLocal) -> Result<()> {
        let Some(batch) = local.batch.take() else {
            return Ok(());
        };
        batch.collection.finalize_append(&mut local.append_state);
        if batch.count() == 0 {
            return Ok(());
        }
        {
            let mut state = global.batch.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.raw_batches.insert(local.batch_index, batch).is_some() {
                return Err(Error::internal(rustdb_common::byte_format!(
                    "Duplicate batch index {} encountered in PhysicalFixedBatchCopy",
                    local.batch_index
                )));
            }
            Self::repartition_batches(
                &mut state,
                local.min_batch_index,
                false,
                global
                    .batch
                    .buffer_manager
                    .as_ref()
                    .ok_or_else(|| Error::internal("COPY without buffer manager"))?,
                self.children[0].types(),
            )?;
        }
        let any_unblocked = global
            .batch
            .memory_manager
            .as_ref()
            .is_some_and(BatchMemoryManager::unblock_tasks);
        // A woken helper can prepare queued work while this source advances its
        // batch index. Preparing here would hold back the minimum source batch.
        if !any_unblocked {
            self.execute_batch_task(global)?;
            self.flush_batch_data(global)?;
        }
        Ok(())
    }

    /// `FinalFlush`: writes what is left and completes the file.
    fn batch_final_flush(&self, global: &CopyGlobal) -> Result<()> {
        self.flush_batch_data(global)?;
        {
            let state = global.batch.state.lock().unwrap_or_else(PoisonError::into_inner);
            if !state.tasks.is_empty() {
                return Err(Error::internal(
                    "Unexecuted tasks are remaining in PhysicalFixedBatchCopy::FinalFlush!?",
                ));
            }
            if state.scheduled_batch_index != state.flushed_batch_index {
                return Err(Error::internal(
                    "Not all batches were flushed to disk - incomplete file?",
                ));
            }
        }
        if let Some(manager) = &global.batch.memory_manager {
            manager.final_check()?;
        }
        let mut inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
        self.finish_single_file(&mut inner)?;
        inner.finalized = true;
        Ok(())
    }

    /// Completes the single output file, renaming it from its temporary name.
    fn finish_single_file(&self, inner: &mut CopyInner) -> Result<()> {
        let options = &self.options;
        if let Some(file) = inner.global_state.take() {
            self.finish_file(inner, &file)?;
            drop(file);
            if options.use_tmp_file {
                let target = Self::non_tmp_file(&options.file_path);
                if let Some(fs) = rustdb_function::registered_files::lookup(&options.file_path) {
                    fs.rename(&options.file_path, &target)?;
                } else {
                    std::fs::rename(native_path(&options.file_path)?, native_path(&target)?).map_err(
                        |error| {
                            io_error(rustdb_common::byte_format!(
                                "Could not rename \"{}\" to \"{target}\": {error}",
                                options.file_path
                            ))
                        },
                    )?;
                }
            }
        }
        Ok(())
    }
}

impl Drop for CopyGlobal {
    fn drop(&mut self) {
        let inner = self.inner.get_mut().unwrap_or_else(PoisonError::into_inner);
        if !inner.initialized && inner.created_files.is_empty() || inner.finalized {
            return;
        }
        // Release the writers before removing their files.
        inner.global_state = None;
        inner.partitions.clear();
        for file in &inner.created_files {
            if let Ok(path) = native_path(file) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

impl PhysicalOperator for PhysicalCopyToFile {
    fn name(&self) -> &'static str {
        if self.batch {
            "BATCH_COPY_TO_FILE"
        } else {
            "COPY_TO_FILE"
        }
    }
    fn types(&self) -> &[LogicalType] {
        &self.types
    }
    fn children(&self) -> &[PhysicalRef] {
        &self.children
    }
    fn as_sink(&self) -> Option<&dyn ErasedSink> {
        Some(self)
    }
    fn as_source(&self) -> Option<&dyn ErasedSource> {
        Some(self)
    }
    /// `ParallelSink`: each thread writes its own files with `PER_THREAD_OUTPUT`, or
    /// prepares its own batches.
    fn parallel_sink(&self) -> bool {
        self.options.per_thread_output || self.batch || self.parallel
    }
    fn required_partition_info(&self) -> OperatorPartitionInfo {
        OperatorPartitionInfo {
            batch_index: self.batch,
            partition_columns: Vec::new(),
        }
    }
}

impl PhysicalCopyToFile {
    /// `Sink` with `PER_THREAD_OUTPUT`: the thread's own file (rotated by size if asked),
    /// written without holding the copy's lock.
    fn sink_per_thread(
        &self,
        chunk: &DataChunk,
        global: &CopyGlobal,
        local: &mut Option<OpenFile>,
    ) -> Result<()> {
        {
            let mut inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
            inner.rows_copied += chunk.len() as u64;
            let rotate_now = match local.as_ref() {
                Some(file) if self.options.rotate => {
                    match (&self.function.rotate_next_file, &self.function.copy_to_file_size) {
                        (Some(rotate_next_file), _) => rotate_next_file(
                            &file.state,
                            self.bind_data.as_ref(),
                            self.options.file_size_bytes,
                        ),
                        (None, Some(file_size)) => self
                            .options
                            .file_size_bytes
                            .is_some_and(|limit| file_size(&file.state) > limit),
                        (None, None) => false,
                    }
                }
                _ => false,
            };
            if rotate_now && let Some(file) = local.take() {
                self.finish_file(&mut inner, &file)?;
            }
            if local.is_none() {
                *local = Some(self.create_file_state(&mut inner)?);
            }
        }
        let file = local
            .as_ref()
            .ok_or_else(|| Error::internal("copy without an open file"))?;
        self.sink_into(&file.state, chunk)
    }
}

impl Sink for PhysicalCopyToFile {
    type Global = CopyGlobal;
    type Local = CopyLocal;

    fn global_sink_state(&self, ctx: &ExecutionContext) -> Result<CopyGlobal> {
        let mut global = CopyGlobal::default();
        if self.batch {
            let minimum = self.children[0].types().len().max(1) as u64 * 4 * 1024 * 1024;
            global.batch.memory_manager = Some(BatchMemoryManager::new(
                ctx.env.buffer_manager.clone(),
                ctx.env.threads(),
                minimum,
            ));
            global.batch.minimum_memory_per_thread = minimum;
            global.batch.buffer_manager = Some(ctx.env.buffer_manager.clone());
        }
        {
            let mut inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
            inner.logger = ctx.operator_logger(self);
            if self.options.partition_output || self.options.per_thread_output || self.options.rotate {
                self.prepare_directory()?;
                if !self.options.per_thread_output && self.options.rotate && self.options.write_empty_file {
                    let state = self.create_file_state(&mut inner)?;
                    inner.global_state = Some(state);
                }
                inner.initialized = true;
            } else if self.options.write_empty_file {
                self.initialize(&mut inner)?;
            }
        }
        if self.batch || self.parallel {
            let batch_size = self
                .function
                .desired_batch_size
                .as_ref()
                .map_or(0, |size| size(self.bind_data.as_ref()) as usize);
            global
                .batch
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .batch_size = batch_size;
        }
        Ok(global)
    }

    fn max_threads(&self, global: &CopyGlobal, source_max: usize) -> usize {
        let Some(manager) = &global.batch.memory_manager else {
            return source_max;
        };
        let minimum = global.batch.minimum_memory_per_thread;
        manager.request_memory(source_max as u64 * minimum);
        source_max.min((manager.available_memory() / minimum) as usize + 1)
    }

    fn local_sink_state(&self, _: &ExecutionContext, _: &CopyGlobal) -> Result<CopyLocal> {
        Ok(CopyLocal::default())
    }

    fn sink(
        &self,
        ctx: &ExecutionContext,
        chunk: &DataChunk,
        global: &CopyGlobal,
        local: &mut CopyLocal,
    ) -> Result<SinkResult> {
        if self.parallel {
            let file_state = self.batch_file_state(global)?;
            local.rows_copied += chunk.len() as u64;
            if local.function_local.is_none() {
                let initialize = self
                    .function
                    .copy_to_initialize_local
                    .as_ref()
                    .ok_or_else(|| Error::internal("parallel copy without copy_to_initialize_local"))?;
                local.function_local = Some(initialize(self.bind_data.as_ref())?);
            }
            let sink = self
                .function
                .copy_to_sink_local
                .as_ref()
                .ok_or_else(|| Error::internal("parallel copy without a local sink"))?;
            if let Some(function_local) = local.function_local.as_mut() {
                sink(self.bind_data.as_ref(), &file_state, function_local, chunk)?;
            }
            return Ok(SinkResult::NeedMoreInput);
        }
        if self.batch {
            let manager = global
                .batch
                .memory_manager
                .as_ref()
                .ok_or_else(|| Error::internal("COPY without memory manager"))?;
            let index = ctx.partition_info.batch_index;
            loop {
                if local.processing_tasks {
                    while self.execute_batch_task(global)? {}
                    self.flush_batch_data(global)?;
                    if !manager.is_minimum_batch_index(index)
                        && manager.out_of_memory(index)
                        && let Some(blocked) = manager.block_if_out_of_memory(index, &ctx.interrupt_state)
                    {
                        return Ok(blocked);
                    }
                    local.processing_tasks = false;
                }
                if !manager.is_minimum_batch_index(index) {
                    manager.update_min_batch_index(ctx.partition_info.min_batch_index);
                    if manager.out_of_memory(index) {
                        local.processing_tasks = true;
                        continue;
                    }
                }
                break;
            }
            if !self.options.write_empty_file && local.rows_copied == 0 {
                self.batch_file_state(global)?;
            }
            local.rows_copied += chunk.len() as u64;
            if local.batch.is_none() {
                local.batch = Some(RawBatch::new(
                    ctx.env.buffer_manager.clone(),
                    self.children[0].types().to_vec(),
                )?);
            }
            let batch = local
                .batch
                .as_mut()
                .ok_or_else(|| Error::internal("COPY missing local batch"))?;
            batch.collection.append(&mut local.append_state, chunk)?;
            let new_memory = batch.collection.allocation_size() as u64;
            let increase = new_memory
                .checked_sub(batch.memory_usage)
                .ok_or_else(|| Error::internal("COPY local allocation decreased"))?;
            manager.increase_unflushed_memory(increase);
            batch.memory_usage = new_memory;
            return Ok(SinkResult::NeedMoreInput);
        }
        if self.options.per_thread_output && !self.options.partition_output {
            self.sink_per_thread(chunk, global, &mut local.file)?;
            return Ok(SinkResult::NeedMoreInput);
        }
        let mut inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let options = &self.options;
        if !options.write_empty_file && !options.rotate {
            self.initialize(&mut inner)?;
        }
        inner.rows_copied += chunk.len() as u64;
        if options.partition_output {
            self.sink_partitioned(&mut inner, chunk)?;
            return Ok(SinkResult::NeedMoreInput);
        }
        if options.per_thread_output || options.rotate {
            let rotate_now = match &inner.global_state {
                Some(file) if options.rotate => {
                    match (&self.function.rotate_next_file, &self.function.copy_to_file_size) {
                        (Some(rotate_next_file), _) => {
                            rotate_next_file(&file.state, self.bind_data.as_ref(), options.file_size_bytes)
                        }
                        (None, Some(file_size)) => options
                            .file_size_bytes
                            .is_some_and(|limit| file_size(&file.state) > limit),
                        (None, None) => false,
                    }
                }
                _ => false,
            };
            if rotate_now && let Some(file) = inner.global_state.take() {
                self.finish_file(&mut inner, &file)?;
            }
            if inner.global_state.is_none() {
                let state = self.create_file_state(&mut inner)?;
                inner.global_state = Some(state);
            }
        }
        let file = inner
            .global_state
            .as_ref()
            .ok_or_else(|| Error::internal("copy without an open file"))?;
        self.sink_into(&file.state, chunk)?;
        Ok(SinkResult::NeedMoreInput)
    }

    fn next_batch(
        &self,
        _: &ExecutionContext,
        global: &CopyGlobal,
        local: &mut CopyLocal,
        partition: &PartitionInfo,
    ) -> Result<()> {
        local.min_batch_index = partition.min_batch_index;
        self.add_local_batch(global, local)?;
        if let Some(manager) = &global.batch.memory_manager {
            manager.update_min_batch_index(partition.min_batch_index);
        }
        local.batch_index = partition.batch_index;
        Ok(())
    }

    fn combine(&self, ctx: &ExecutionContext, global: &CopyGlobal, mut local: CopyLocal) -> Result<()> {
        if self.parallel {
            global
                .inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .rows_copied += local.rows_copied;
            if let Some(function_local) = local.function_local.take() {
                let combine = self
                    .function
                    .copy_to_combine
                    .as_ref()
                    .ok_or_else(|| Error::internal("parallel copy without copy_to_combine"))?;
                let file_state = self.batch_file_state(global)?;
                combine(self.bind_data.as_ref(), &file_state, function_local)?;
            }
            return Ok(());
        }
        if self.batch {
            global
                .inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .rows_copied += local.rows_copied;
            self.add_local_batch(global, &mut local)?;
            global
                .batch
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .any_finished = true;
            if let Some(manager) = &global.batch.memory_manager {
                manager.update_min_batch_index(ctx.partition_info.min_batch_index);
            }
            while self.execute_batch_task(global)? {}
            self.flush_batch_data(global)?;
            if let Some(manager) = &global.batch.memory_manager {
                manager.unblock_tasks();
            }
            return Ok(());
        }
        if let Some(file) = local.file {
            let mut inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
            self.finish_file(&mut inner, &file)?;
        }
        Ok(())
    }

    fn finalize(&self, ctx: &ExecutionContext, global: &mut CopyGlobal) -> Result<SinkFinalize> {
        if self.batch {
            let pending = {
                let state = global
                    .batch
                    .state
                    .get_mut()
                    .unwrap_or_else(PoisonError::into_inner);
                Self::repartition_batches(
                    state,
                    u64::MAX,
                    true,
                    &ctx.env.buffer_manager,
                    self.children[0].types(),
                )?;
                state.tasks.len()
            };
            if pending <= 1 {
                while self.execute_batch_task(global)? {}
                self.batch_final_flush(global)?;
            } else {
                // `ProcessRemainingBatchesEvent`: the remaining batches are prepared by
                // every thread in a finalize stage.
                global
                    .batch
                    .state
                    .get_mut()
                    .unwrap_or_else(PoisonError::into_inner)
                    .final_flush_pending = true;
            }
            return Ok(SinkFinalize::Ready);
        }
        let inner = global.inner.get_mut().unwrap_or_else(PoisonError::into_inner);
        let options = &self.options;
        if options.partition_output {
            for key in std::mem::take(&mut inner.partition_order) {
                if let Some(file) = inner.partitions.remove(&key) {
                    self.finish_file(inner, &file)?;
                }
            }
        } else if options.per_thread_output && inner.rows_copied == 0 {
            let file = self.create_file_state(inner)?;
            self.finish_file(inner, &file)?;
        } else {
            self.finish_single_file(inner)?;
        }
        inner.finalized = true;
        Ok(SinkFinalize::Ready)
    }

    fn finalize_tasks(&self, ctx: &ExecutionContext, global: &CopyGlobal) -> Result<usize> {
        if !self.batch {
            return Ok(0);
        }
        let (pending, tasks) = {
            let mut state = global.batch.state.lock().unwrap_or_else(PoisonError::into_inner);
            let pending = std::mem::replace(&mut state.final_flush_pending, false);
            (pending, state.tasks.len())
        };
        if !pending {
            return Ok(0);
        }
        if tasks == 0 {
            self.batch_final_flush(global)?;
            return Ok(0);
        }
        // Run the stage; the final flush follows when it is done.
        global
            .batch
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .final_flush_pending = true;
        let _ = ctx;
        Ok(tasks.clamp(1, 64))
    }

    fn execute_finalize_task(&self, _: &ExecutionContext, global: &CopyGlobal, _: usize) -> Result<()> {
        while self.execute_batch_task(global)? {
            self.flush_batch_data(global)?;
        }
        Ok(())
    }
}

impl Source for PhysicalCopyToFile {
    type Global = (SinkState, AtomicUsize);
    type Local = ();

    fn global_source_state(&self, _: &ExecutionContext, sink: Option<&SinkState>) -> Result<Self::Global> {
        let sink = sink.ok_or_else(|| Error::internal("copy source requires its sink state"))?;
        Ok((Arc::clone(sink), AtomicUsize::new(0)))
    }

    fn local_source_state(&self, _: &ExecutionContext, _: &Self::Global) -> Result<()> {
        Ok(())
    }

    fn get_data(
        &self,
        _: &ExecutionContext,
        chunk: &mut DataChunk,
        (sink, done): &Self::Global,
        (): &mut (),
    ) -> Result<SourceResult> {
        let global = sink
            .downcast_ref::<CopyGlobal>()
            .ok_or_else(|| Error::internal("copy sink state has the wrong type"))?;
        if self.options.return_type == CopyFunctionReturnType::WrittenFileStatistics {
            let inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
            return self.return_statistics(&inner, chunk, done);
        }
        if done.swap(1, std::sync::atomic::Ordering::Relaxed) == 1 {
            chunk.set_columns(Vec::new(), 0);
            return Ok(SourceResult::Finished);
        }
        let inner = global.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let count = Value::bigint(i64::try_from(inner.rows_copied).unwrap_or(i64::MAX));
        let mut columns = vec![Vector::constant(&count)?];
        if self.options.return_type == CopyFunctionReturnType::ChangedRowsAndFileList {
            let files = inner
                .written_files
                .iter()
                .map(|file| Value::try_varchar(&self.returned_path(&file.path)))
                .collect::<Result<_>>()?;
            columns.push(Vector::constant(&Value::list(LogicalType::VARCHAR, files))?);
        }
        let mut flat = Vec::with_capacity(columns.len());
        for column in columns {
            flat.push(column.flattened(1));
        }
        chunk.set_columns(flat, 1);
        Ok(SourceResult::Finished)
    }
}
