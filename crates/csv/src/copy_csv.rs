//! The `csv` COPY format (DuckDB's `copy_csv.cpp` and `csv_writer.cpp`): the CSV writer
//! of `COPY ... TO` and the `read_csv` reader of `COPY ... FROM`.

use std::any::Any;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::{Arc, Mutex, PoisonError};

use rustdb_common::{CaseInsensitiveMap, Error, ErrorKind, Result};
use rustdb_function::cast::{CastFunctionSet, CastParameters};
use rustdb_function::copy::{
    CopyFromBindInput, CopyFunctionBindInput, CopyFunctionExecutionMode, CopyGlobalState, CopyLocalState,
    PreparedBatch,
};
use rustdb_function::{CopyFunction, CopyOption, CopyOptionMode, FunctionData};
use rustdb_functions::strtime::StrpTimeFormat;
use rustdb_planner::ClientContext;
use rustdb_types::{DataChunk, Date, LogicalType, LogicalTypeId, StringT, Timestamp, Value, Vector};

use crate::multi_file::{FileList, MultiFileOptions, create_file_list};
use crate::options::{CsvReaderOptions, FileCompressionType};
use crate::read_csv::{bind_csv_files, read_csv_functions};

/// `ConvertVectorToValue`: the values of an option as one LIST value.
fn convert_vector_to_value(values: &[Value]) -> Value {
    match values.first() {
        None => Value::list(LogicalType::BOOLEAN, Vec::new()),
        Some(first) => Value::list(first.logical_type().clone(), values.to_vec()),
    }
}

fn client_context(binder: Option<&dyn Any>) -> Result<&ClientContext> {
    binder
        .and_then(|binder| binder.downcast_ref::<ClientContext>())
        .ok_or_else(|| Error::internal("csv copy bound without a client context"))
}

/// How the writer quotes and separates lines (DuckDB's `CSVWriterOptions`).
#[derive(Clone, Debug)]
struct CsvWriterOptions {
    newline: String,
    requires_quotes: [bool; 256],
}

impl CsvWriterOptions {
    fn new(delimiter: &str, quote: u8, write_newline: &str) -> Self {
        let mut requires_quotes = [false; 256];
        requires_quotes[usize::from(b'\n')] = true;
        requires_quotes[usize::from(b'\r')] = true;
        requires_quotes[usize::from(b'#')] = true;
        requires_quotes[usize::from(delimiter.as_bytes().first().copied().unwrap_or(0))] = true;
        requires_quotes[usize::from(quote)] = true;
        let newline = if write_newline.is_empty() {
            "\n".to_owned()
        } else {
            write_newline.replace("\\r", "\r").replace("\\n", "\n")
        };
        Self {
            newline,
            requires_quotes,
        }
    }
}

/// How one column is turned into text.
#[derive(Clone, Debug)]
enum ColumnFormat {
    /// `CAST(column AS VARCHAR)`.
    Cast,
    /// `strftime(column, format)` of a DATE.
    Date(StrpTimeFormat),
    /// `strftime(column, format)` of a TIMESTAMP (WITH TIME ZONE).
    Timestamp(StrpTimeFormat),
}

/// The bind data of a CSV write (DuckDB's `WriteCSVData`).
#[derive(Debug)]
pub struct WriteCsvData {
    options: CsvReaderOptions,
    writer_options: CsvWriterOptions,
    formats: Vec<ColumnFormat>,
    casts: Arc<CastFunctionSet>,
}

impl FunctionData for WriteCsvData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Parses a `strftime` format as its binder does.
fn write_format(format: &Value) -> Result<StrpTimeFormat> {
    let text = format.to_varchar()?;
    StrpTimeFormat::parse(&text)
        .map_err(|error| Error::invalid_input(format!("Failed to parse format specifier {text}: {error}")))
}

/// `WriteCSVBind`.
fn write_csv_bind(
    input: &mut CopyFunctionBindInput<'_>,
    names: &[String],
    types: &[LogicalType],
) -> Result<Arc<dyn FunctionData>> {
    let context = client_context(input.binder)?;
    let mut options = CsvReaderOptions {
        name_list: names.to_vec(),
        ..CsvReaderOptions::default()
    };
    if options.dialect_options.state_machine_options.escape.value() == 0 {
        let quote = options.dialect_options.state_machine_options.quote.clone();
        options.dialect_options.state_machine_options.escape = crate::options::CsvOption::new(quote.value());
    }
    for (name, values) in input.options.iter() {
        options.set_write_option(&name.to_lowercase(), &convert_vector_to_value(values))?;
    }
    if options.force_quote.is_empty() {
        options.force_quote = vec![false; names.len()];
    }
    options.finalize()?;
    // `IsFileCompressed` compares the extension as written.
    let extension = std::path::Path::new(&input.file_extension).extension();
    match options.compression {
        FileCompressionType::Gzip if extension.is_none_or(|extension| extension != "gz") => {
            input.file_extension += ".gz";
        }
        FileCompressionType::Zstd if extension.is_none_or(|extension| extension != "zst") => {
            input.file_extension += ".zst";
        }
        _ => {}
    }
    let date_format = options
        .write_date_format
        .get(&LogicalTypeId::Date)
        .filter(|value| !value.is_null())
        .map(write_format)
        .transpose()?;
    let timestamp_format = options
        .write_date_format
        .get(&LogicalTypeId::Timestamp)
        .filter(|value| !value.is_null())
        .map(write_format)
        .transpose()?;
    let formats = types
        .iter()
        .map(|ty| match ty.id() {
            LogicalTypeId::Date if date_format.is_some() => {
                ColumnFormat::Date(date_format.clone().unwrap_or_default())
            }
            LogicalTypeId::Timestamp | LogicalTypeId::TimestampTz if timestamp_format.is_some() => {
                ColumnFormat::Timestamp(timestamp_format.clone().unwrap_or_default())
            }
            _ => ColumnFormat::Cast,
        })
        .collect();
    let sm = &options.dialect_options.state_machine_options;
    let writer_options = CsvWriterOptions::new(sm.delimiter.get(), sm.quote.value(), &options.write_newline);
    Ok(Arc::new(WriteCsvData {
        options,
        writer_options,
        formats,
        casts: Arc::clone(&context.casts),
    }))
}

/// A created file whose writes are logged to the connection's `FileSystem` log
/// (the `DUCKDB_LOG_FILE_SYSTEM_*` calls of a local file handle).
pub(crate) struct LoggedFile {
    file: File,
    logger: Option<rustdb_function::logging::FileSystemLogger>,
    position: u64,
}

impl Write for LoggedFile {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let written = self.file.write(data)?;
        if let Some(logger) = &self.logger {
            logger.write(written as u64, self.position);
        }
        self.position += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Drop for LoggedFile {
    fn drop(&mut self) {
        if let Some(logger) = &self.logger {
            logger.close();
        }
    }
}

/// Where the writer's bytes go: a plain or compressed file.
pub(crate) enum CsvSink {
    Plain(BufWriter<LoggedFile>),
    Gzip(flate2::write::GzEncoder<BufWriter<LoggedFile>>),
    /// Zstandard output is compressed when the file is closed.
    Zstd(Vec<u8>, BufWriter<LoggedFile>),
    Closed,
}

impl CsvSink {
    /// Creates `path` (replacing an existing file), compressed as `compression` says
    /// (`FileSystem::OpenFile` with `FILE_FLAGS_FILE_CREATE_NEW` and the compression).
    pub(crate) fn create(path: &str, compression: FileCompressionType) -> Result<Self> {
        let file = File::create(path).map_err(|error| crate::buffer::open_error(path, &error))?;
        let file = LoggedFile {
            file,
            logger: rustdb_function::logging::FileSystemLogger::open(path),
            position: 0,
        };
        let file = BufWriter::with_capacity(1 << 15, file);
        Ok(match compression.resolve(path) {
            FileCompressionType::Gzip => Self::Gzip(flate2::write::GzEncoder::new(file, flate2::Compression::default())),
            FileCompressionType::Zstd => Self::Zstd(Vec::new(), file),
            _ => Self::Plain(file),
        })
    }

    pub(crate) fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        match self {
            Self::Plain(file) => file.write_all(data),
            Self::Gzip(encoder) => encoder.write_all(data),
            Self::Zstd(buffer, _) => {
                buffer.extend_from_slice(data);
                Ok(())
            }
            Self::Closed => Ok(()),
        }
    }

    pub(crate) fn close(&mut self) -> std::io::Result<()> {
        match std::mem::replace(self, Self::Closed) {
            // Dropping the file after the flush logs its `CLOSE`.
            Self::Plain(mut file) => file.flush(),
            Self::Gzip(encoder) => encoder.finish()?.flush(),
            Self::Zstd(buffer, mut file) => {
                ruzstd::encoding::compress(
                    buffer.as_slice(),
                    &mut file,
                    ruzstd::encoding::CompressionLevel::Fastest,
                );
                file.flush()
            }
            Self::Closed => Ok(()),
        }
    }
}

/// The writer of one file (DuckDB's `CSVWriter` with `GlobalWriteCSVData`).
struct CsvWriter {
    sink: CsvSink,
    path: String,
    written_anything: bool,
    bytes_written: u64,
}

fn write_error(path: &str, error: &std::io::Error) -> Error {
    Error::new(ErrorKind::Io, format!("Could not write file \"{path}\": {error}"))
}

impl CsvWriter {
    fn write_raw(&mut self, data: &[u8]) -> Result<()> {
        self.bytes_written += data.len() as u64;
        self.sink
            .write(data)
            .map_err(|error| write_error(&self.path, &error))
    }

    /// `FlushInternal`: appends a block of rows, separated from earlier ones.
    fn flush_block(&mut self, block: &[u8], newline: &str) -> Result<()> {
        if self.written_anything {
            self.write_raw(newline.as_bytes())?;
        }
        self.written_anything = true;
        self.write_raw(block)
    }
}

/// The open file of a CSV write.
struct GlobalWriteCsvData {
    writer: Mutex<CsvWriter>,
}

/// `WriteQuotedString`.
fn write_quoted_string(out: &mut Vec<u8>, value: &[u8], force_quote: bool, data: &WriteCsvData) {
    let options = &data.options;
    let quote = options.dialect_options.state_machine_options.quote.value();
    let escape = options.dialect_options.state_machine_options.escape.value();
    let null_str = options.null_str.first().map_or(&b""[..], |null| null.as_bytes());
    let force_quote = force_quote
        || value == null_str
        || value
            .iter()
            .any(|&byte| data.writer_options.requires_quotes[usize::from(byte)]);
    if !force_quote || quote == 0 {
        out.extend_from_slice(value);
        return;
    }
    out.push(quote);
    if !value.iter().any(|&byte| byte == quote || byte == escape) {
        out.extend_from_slice(value);
        out.push(quote);
        return;
    }
    // `AddEscapes` for the escape and then for the quote.
    let add_escapes = |to_be_escaped: u8, value: &[u8]| -> Vec<u8> {
        if escape == 0 {
            return value.to_vec();
        }
        let mut result = Vec::with_capacity(value.len() + 4);
        for &byte in value {
            if byte == to_be_escaped {
                result.push(escape);
            }
            result.push(byte);
        }
        result
    };
    let mut escaped = add_escapes(escape, value);
    if escape != quote {
        escaped = add_escapes(quote, &escaped);
    }
    out.extend_from_slice(&escaped);
    out.push(quote);
}

/// `WriteHeader`.
fn header_bytes(data: &WriteCsvData) -> Vec<u8> {
    let mut out = Vec::new();
    let delimiter = data
        .options
        .dialect_options
        .state_machine_options
        .delimiter
        .get()
        .as_bytes()
        .first()
        .copied()
        .unwrap_or(0);
    for (i, name) in data.options.name_list.iter().enumerate() {
        if i != 0 && delimiter != 0 {
            out.push(delimiter);
        }
        write_quoted_string(
            &mut out,
            name.as_bytes(),
            data.options.force_quote.get(i).copied().unwrap_or(false),
            data,
        );
    }
    out
}

/// `WriteCSVInitializeGlobal`: opens the file and writes the prefix and header.
fn write_csv_initialize_global(bind_data: &dyn FunctionData, path: &str) -> Result<CopyGlobalState> {
    let data = bind_data
        .as_any()
        .downcast_ref::<WriteCsvData>()
        .ok_or_else(|| Error::internal("csv write without bind data"))?;
    let sink = CsvSink::create(path, data.options.compression)?;
    let mut writer = CsvWriter {
        sink,
        path: path.to_owned(),
        written_anything: false,
        bytes_written: 0,
    };
    if !data.options.prefix.is_empty() {
        writer.write_raw(data.options.prefix.as_bytes())?;
    }
    let header = &data.options.dialect_options.header;
    if !header.is_set_by_user() || header.value() {
        writer.flush_block(&header_bytes(data), &data.writer_options.newline)?;
    }
    Ok(Box::new(GlobalWriteCsvData {
        writer: Mutex::new(writer),
    }))
}

/// The text of every value of a column (the cast expressions of `WriteCSVData`).
fn column_text(data: &WriteCsvData, column: usize, vector: &Vector, count: usize) -> Result<Vector> {
    match &data.formats[column] {
        ColumnFormat::Cast => {
            if vector.logical_type().id() == LogicalTypeId::Varchar {
                return Ok(vector.flattened(count));
            }
            let cast = data
                .casts
                .get_cast_function(vector.logical_type(), &LogicalType::VARCHAR)?;
            let mut result = Vector::flat(LogicalType::VARCHAR, count.max(1));
            cast.execute(vector, &mut result, count, &mut CastParameters::new())?;
            Ok(result)
        }
        ColumnFormat::Date(format) => {
            let flat = vector.flattened(count);
            let mut result = Vector::flat(LogicalType::VARCHAR, count.max(1));
            let mut buffer = Vec::new();
            for row in 0..count {
                if flat.is_null(row) {
                    result.validity_mut().set_invalid(row, count);
                    continue;
                }
                buffer.clear();
                format.format_date(flat.values::<Date>()[row], &mut buffer);
                let text = result.add_string(&buffer);
                result.values_mut::<StringT>()[row] = text;
            }
            Ok(result)
        }
        ColumnFormat::Timestamp(format) => {
            let flat = vector.flattened(count);
            let mut result = Vector::flat(LogicalType::VARCHAR, count.max(1));
            let mut buffer = Vec::new();
            for row in 0..count {
                if flat.is_null(row) {
                    result.validity_mut().set_invalid(row, count);
                    continue;
                }
                buffer.clear();
                format.format_timestamp(flat.values::<Timestamp>()[row], &mut buffer);
                let text = result.add_string(&buffer);
                result.values_mut::<StringT>()[row] = text;
            }
            Ok(result)
        }
    }
}

/// The bind data of a CSV write.
fn write_data(bind_data: &dyn FunctionData) -> Result<&WriteCsvData> {
    bind_data
        .as_any()
        .downcast_ref::<WriteCsvData>()
        .ok_or_else(|| Error::internal("csv write without bind data"))
}

/// The open file of a CSV write.
fn write_global(state: &CopyGlobalState) -> Result<&GlobalWriteCsvData> {
    state
        .downcast_ref::<GlobalWriteCsvData>()
        .ok_or_else(|| Error::internal("csv write without its file"))
}

/// `WriteCSVSink`: writes the rows of a chunk. The rows are encoded before the file is
/// locked, so concurrent sinks (parallel writes) only serialize the write itself.
fn write_csv_sink(bind_data: &dyn FunctionData, state: &CopyGlobalState, chunk: &DataChunk) -> Result<()> {
    let data = write_data(bind_data)?;
    let global = write_global(state)?;
    let Some(block) = encode_chunk(data, chunk)? else {
        return Ok(());
    };
    let mut writer = global.writer.lock().unwrap_or_else(PoisonError::into_inner);
    writer.flush_block(&block, &data.writer_options.newline)
}

/// The rows of a chunk as CSV lines (without a final newline); `None` for no rows.
fn encode_chunk(data: &WriteCsvData, chunk: &DataChunk) -> Result<Option<Vec<u8>>> {
    let count = chunk.len();
    if count == 0 {
        return Ok(None);
    }
    let texts = chunk
        .columns()
        .iter()
        .enumerate()
        .map(|(column, vector)| column_text(data, column, vector, count))
        .collect::<Result<Vec<_>>>()?;
    let options = &data.options;
    let delimiter = options
        .dialect_options
        .state_machine_options
        .delimiter
        .get()
        .as_bytes()
        .first()
        .copied()
        .unwrap_or(0);
    let null_str = options.null_str.first().map_or("", String::as_str);
    let newline = data.writer_options.newline.as_bytes();
    let mut out = Vec::with_capacity(count * 16);
    for row in 0..count {
        if row > 0 {
            out.extend_from_slice(newline);
        }
        for (column, text) in texts.iter().enumerate() {
            if column != 0 && delimiter != 0 {
                out.push(delimiter);
            }
            if text.is_null(row) {
                out.extend_from_slice(null_str.as_bytes());
                continue;
            }
            write_quoted_string(
                &mut out,
                text.string_bytes(row),
                options.force_quote.get(column).copied().unwrap_or(false),
                data,
            );
        }
    }
    Ok(Some(out))
}

/// `WriteCSVExecutionMode`.
fn write_csv_execution_mode(
    _: &dyn FunctionData,
    preserve_insertion_order: bool,
    supports_batch_index: bool,
) -> CopyFunctionExecutionMode {
    if !preserve_insertion_order {
        return CopyFunctionExecutionMode::ParallelCopyToFile;
    }
    if supports_batch_index {
        return CopyFunctionExecutionMode::BatchCopyToFile;
    }
    CopyFunctionExecutionMode::RegularCopyToFile
}

/// `WriteCSVPrepareBatch`: encodes the chunks of a batch (any thread).
fn write_csv_prepare_batch(
    bind_data: &dyn FunctionData,
    _: &CopyGlobalState,
    chunks: Vec<DataChunk>,
) -> Result<PreparedBatch> {
    let data = write_data(bind_data)?;
    let mut blocks = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if let Some(block) = encode_chunk(data, &chunk)? {
            blocks.push(block);
        }
    }
    Ok(Box::new(blocks))
}

/// `WriteCSVFlushBatch`: writes an encoded batch (in batch order).
fn write_csv_flush_batch(
    bind_data: &dyn FunctionData,
    state: &CopyGlobalState,
    batch: PreparedBatch,
) -> Result<()> {
    let data = write_data(bind_data)?;
    let global = write_global(state)?;
    let blocks = batch
        .downcast::<Vec<Vec<u8>>>()
        .map_err(|_| Error::internal("csv batch of another format"))?;
    let mut writer = global.writer.lock().unwrap_or_else(PoisonError::into_inner);
    for block in blocks.iter() {
        writer.flush_block(block, &data.writer_options.newline)?;
    }
    Ok(())
}

/// `WriteCSVFinalize`: writes the suffix (or the final newline) and closes the file.
fn write_csv_finalize(bind_data: &dyn FunctionData, state: &CopyGlobalState) -> Result<()> {
    let data = bind_data
        .as_any()
        .downcast_ref::<WriteCsvData>()
        .ok_or_else(|| Error::internal("csv write without bind data"))?;
    let global = state
        .downcast_ref::<GlobalWriteCsvData>()
        .ok_or_else(|| Error::internal("csv write without its file"))?;
    let mut writer = global.writer.lock().unwrap_or_else(PoisonError::into_inner);
    if !data.options.suffix.is_empty() {
        writer.write_raw(data.options.suffix.as_bytes())?;
    } else if writer.written_anything {
        writer.write_raw(data.writer_options.newline.as_bytes())?;
    }
    let path = writer.path.clone();
    writer.sink.close().map_err(|error| write_error(&path, &error))
}

/// The bytes written to a file so far (`WriteCSVRotateNextFile`).
fn write_csv_file_size(state: &CopyGlobalState) -> u64 {
    state.downcast_ref::<GlobalWriteCsvData>().map_or(0, |global| {
        global
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .bytes_written
    })
}

/// `MultiFileBindCopy` with `CSVMultiFileInfo`: binds `read_csv` for the target columns.
fn read_csv_copy_bind(
    input: &CopyFromBindInput<'_>,
    expected_names: &[String],
    expected_types: &[LogicalType],
) -> Result<Arc<dyn FunctionData>> {
    let context = client_context(input.binder)?;
    let files: FileList = create_file_list("COPY", std::slice::from_ref(&input.file_path.to_owned()), "csv")?;
    let mut options = CsvReaderOptions::default();
    for (name, values) in input.options.iter() {
        let loption = name.to_lowercase();
        options.set_read_option(&loption, &convert_vector_to_value(values), expected_names)?;
    }
    // FinalizeCopyBind
    options.name_list = expected_names.to_vec();
    options.sql_type_list = expected_types.to_vec();
    options.columns_set = true;
    let mut per_column = CaseInsensitiveMap::default();
    for (i, name) in expected_names.iter().enumerate() {
        per_column.insert(name.clone(), i);
    }
    options.sql_types_per_column = per_column;
    let file_options = MultiFileOptions {
        auto_detect_hive_partitioning: false,
        ..MultiFileOptions::default()
    };
    let data = bind_csv_files(
        &context.casts,
        files,
        file_options,
        options,
        Some((expected_names, expected_types)),
    )?;
    Ok(Arc::new(data))
}

/// `CSVListCopyOptions`.
fn csv_copy_options() -> Vec<(String, CopyOption)> {
    use CopyOptionMode::{ReadOnly, ReadWrite, WriteOnly};
    let option =
        |name: &str, ty: LogicalType, mode: CopyOptionMode| (name.to_owned(), CopyOption::new(ty, mode));
    vec![
        option("auto_detect", LogicalType::BOOLEAN, ReadOnly),
        option("sample_size", LogicalType::BIGINT, ReadOnly),
        option("skip", LogicalType::BIGINT, ReadOnly),
        option("max_line_size", LogicalType::BIGINT, ReadOnly),
        option("maximum_line_size", LogicalType::BIGINT, ReadOnly),
        option("ignore_errors", LogicalType::BOOLEAN, ReadOnly),
        option("buffer_size", LogicalType::BIGINT, ReadOnly),
        option("decimal_separator", LogicalType::VARCHAR, ReadOnly),
        option("null_padding", LogicalType::BOOLEAN, ReadOnly),
        option("parallel", LogicalType::BOOLEAN, ReadOnly),
        option("allow_quoted_nulls", LogicalType::BOOLEAN, ReadOnly),
        option("store_rejects", LogicalType::BOOLEAN, ReadOnly),
        option("force_not_null", LogicalType::ANY, ReadOnly),
        option("rejects_table", LogicalType::VARCHAR, ReadOnly),
        option("rejects_scan", LogicalType::VARCHAR, ReadOnly),
        option("rejects_limit", LogicalType::BIGINT, ReadOnly),
        option("encoding", LogicalType::VARCHAR, ReadOnly),
        option("thousands", LogicalType::VARCHAR, ReadOnly),
        option("force_quote", LogicalType::ANY, WriteOnly),
        option("prefix", LogicalType::VARCHAR, WriteOnly),
        option("suffix", LogicalType::VARCHAR, WriteOnly),
        option("new_line", LogicalType::VARCHAR, ReadWrite),
        option("date_format", LogicalType::VARCHAR, ReadWrite),
        option("dateformat", LogicalType::VARCHAR, ReadWrite),
        option("timestamp_format", LogicalType::VARCHAR, ReadWrite),
        option("timestampformat", LogicalType::VARCHAR, ReadWrite),
        option("quote", LogicalType::VARCHAR, ReadWrite),
        option("comment", LogicalType::VARCHAR, ReadWrite),
        option("delim", LogicalType::VARCHAR, ReadWrite),
        option("delimiter", LogicalType::VARCHAR, ReadWrite),
        option("sep", LogicalType::VARCHAR, ReadWrite),
        option("separator", LogicalType::VARCHAR, ReadWrite),
        option("escape", LogicalType::VARCHAR, ReadWrite),
        option("header", LogicalType::BOOLEAN, ReadWrite),
        option("nullstr", LogicalType::ANY, ReadWrite),
        option("null", LogicalType::ANY, ReadWrite),
        option("compression", LogicalType::VARCHAR, ReadWrite),
        option("strict_mode", LogicalType::BOOLEAN, ReadWrite),
    ]
}

/// The `csv` COPY format (`CSVCopyFunction::RegisterFunction`).
pub fn csv_copy_function() -> CopyFunction {
    let mut function = CopyFunction::new("csv");
    "csv".clone_into(&mut function.extension);
    function.options = csv_copy_options();
    function.copy_to_bind = Some(Arc::new(write_csv_bind));
    function.copy_to_initialize_global = Some(Arc::new(write_csv_initialize_global));
    function.copy_to_sink = Some(Arc::new(write_csv_sink));
    function.copy_to_finalize = Some(Arc::new(write_csv_finalize));
    function.execution_mode = Some(Arc::new(write_csv_execution_mode));
    function.prepare_batch = Some(Arc::new(write_csv_prepare_batch));
    function.flush_batch = Some(Arc::new(write_csv_flush_batch));
    // Parallel writes encode each chunk on its thread and append it under the file lock
    // (`WriteCSVInitializeLocal` / `WriteCSVSink` / `WriteCSVCombine`).
    function.copy_to_initialize_local = Some(Arc::new(|_| Ok(Box::new(()) as CopyLocalState)));
    function.copy_to_sink_local = Some(Arc::new(|bind_data, state, _, chunk| {
        write_csv_sink(bind_data, state, chunk)
    }));
    function.copy_to_combine = Some(Arc::new(|_, _, _| Ok(())));
    function.copy_to_file_size = Some(Arc::new(write_csv_file_size));
    function.copy_from_bind = Some(Arc::new(read_csv_copy_bind));
    function.copy_from_function = read_csv_functions()
        .into_iter()
        .next()
        .and_then(|set| set.functions.into_iter().next());
    function
}
