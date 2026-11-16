//! `read_text` and `read_blob` (DuckDB's `function/table/read_file.cpp` and
//! `direct_file_reader.cpp`): one row per file with its name, content, size and
//! modification time, plus any hive partition columns.

use std::any::Any;
use std::io::Read;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use roastedduck_common::{Error, ErrorKind, Result, STANDARD_VECTOR_SIZE};
use roastedduck_csv::multi_file::{
    FileList, MultiFileBindData, MultiFileOptions, bind_options, hive_parse, local_glob, parse_paths,
};
use roastedduck_function::caching_file_system::CachingFileHandle;
use roastedduck_function::cast::CastFunctionSet;
use roastedduck_function::external_file_cache::CacheValidationMode;
use roastedduck_function::registered_files::{FileHandle, FileSystem};
use roastedduck_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
    TableScanState,
};
use roastedduck_types::{DataChunk, LogicalType, Timestamp, Value, Vector};

/// `ReadFileBindData::FILE_NAME_COLUMN` and the following file columns.
const FILE_NAME_COLUMN: usize = 0;
const FILE_CONTENT_COLUMN: usize = 1;
const FILE_SIZE_COLUMN: usize = 2;
const FILE_LAST_MODIFIED_COLUMN: usize = 3;

/// The bind data of a `read_text` / `read_blob` call (`ReadFileBindData` with the
/// multi-file bind data).
#[derive(Debug)]
struct ReadFileData {
    name: &'static str,
    content_type: LogicalType,
    files: Vec<roastedduck_common::ByteString>,
    types: Vec<LogicalType>,
    file_options: MultiFileOptions,
    reader_bind: MultiFileBindData,
    casts: Arc<CastFunctionSet>,
}

impl FunctionData for ReadFileData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `MultiFileFunction::MultiFileBind` with `DirectMultiFileInfo`: the files of the
/// paths (globs matching nothing are allowed, `FileGlobOptions::ALLOW_EMPTY`) and the
/// file columns, then the auto-detected hive partition columns.
fn read_file_bind(
    name: &'static str,
    content_type: &LogicalType,
    input: &TableFunctionBindInput<'_>,
) -> Result<TableFunctionBindOutput> {
    let context = roastedduck_csv::read_csv::client_context(input)?;
    let first = input
        .inputs
        .first()
        .ok_or_else(|| Error::internal("read_file without a file argument"))?;
    let mut files = Vec::new();
    for path in parse_paths(name, first)? {
        let mut expanded = local_glob(&path)?;
        expanded.sort();
        files.extend(expanded);
    }
    let file_list = FileList { files };
    let mut file_options = MultiFileOptions::default();
    file_options.auto_detect_hive_partitioning(&file_list, &context.casts)?;
    let mut types = vec![
        LogicalType::VARCHAR,
        content_type.clone(),
        LogicalType::BIGINT,
        LogicalType::TIMESTAMP_TZ,
    ];
    let mut names: Vec<roastedduck_common::ByteString> = ["filename", "content", "size", "last_modified"]
        .into_iter()
        .map(Into::into)
        .collect();
    let reader_bind = bind_options(&file_options, &file_list, &mut types, &mut names)?;
    Ok(TableFunctionBindOutput {
        return_types: types.clone(),
        names,
        bind_data: Some(Arc::new(ReadFileData {
            name,
            content_type: content_type.clone(),
            files: file_list.files,
            types,
            file_options,
            reader_bind,
            casts: Arc::clone(&context.casts),
        })),
    })
}

fn io_error(message: roastedduck_common::ByteString) -> Error {
    Error::new_bytes(ErrorKind::Io, message)
}

/// `AssertMaxFileSize`.
fn assert_max_file_size(path: &[u8], size: u64) -> Result<()> {
    let max = u64::from(u32::MAX);
    if size > max {
        return Err(Error::invalid_input(roastedduck_common::byte_format!(
            "File '{path}' size ({}) exceeds maximum allowed file ({})",
            roastedduck_buffer::format_bytes(size),
            roastedduck_buffer::format_bytes(max)
        )));
    }
    Ok(())
}

/// A registered file a row reads its columns from. `DirectFileReader::Scan` opens the
/// file once, before its column loop.
enum OpenedFile {
    /// A remote file: direct I/O through the external file cache
    /// (`FILE_FLAGS_DIRECT_IO` with `CachingMode::CACHE_REMOTE_ONLY`).
    Remote(CachingFileHandle),
    Registered(Arc<dyn FileHandle>),
}

impl OpenedFile {
    fn open(filesystem: &dyn FileSystem, path: &[u8]) -> Result<Self> {
        if filesystem.is_remote() {
            let client = roastedduck_function::file_system::opener();
            return CachingFileHandle::open_direct(
                client.as_ref().and_then(|client| client.external_file_cache()),
                path,
                CacheValidationMode::of_client(client.as_deref()),
            )
            .map(Self::Remote);
        }
        roastedduck_function::file_system::verify_can_access_file(path)?;
        filesystem.open(path, false).map(Self::Registered)
    }

    fn size(&self) -> Result<u64> {
        match self {
            Self::Remote(handle) => handle.file_size(),
            Self::Registered(handle) => handle.size(),
        }
    }

    fn modified(&self) -> Result<i64> {
        match self {
            Self::Remote(handle) => handle.last_modified(),
            Self::Registered(handle) => handle.modified(),
        }
    }

    /// The `size` bytes of the file.
    fn content(&self, path: &[u8], size: u64) -> Result<Vec<u8>> {
        let capacity = usize::try_from(size).map_err(|_| Error::internal("file larger than memory"))?;
        let mut bytes = Vec::with_capacity(capacity);
        match self {
            Self::Remote(handle) => {
                // Read in batches of 128mb, each a range of the cache.
                const MAX_READ_SIZE: u64 = 128 << 20;
                while (bytes.len() as u64) < size {
                    let position = bytes.len() as u64;
                    let read = handle.read((size - position).min(MAX_READ_SIZE), position)?;
                    bytes.extend_from_slice(read.bytes());
                }
            }
            Self::Registered(handle) => {
                roastedduck_function::registered_files::Cursor::new(Arc::clone(handle))
                    .take(size)
                    .read_to_end(&mut bytes)
                    .map_err(|error| {
                        roastedduck_storage::file_system::callback_error(&error).unwrap_or_else(|| {
                            io_error(roastedduck_common::byte_format!(
                                "Could not read file \"{path}\": {error}"
                            ))
                        })
                    })?;
                if bytes.len() as u64 != size {
                    return Err(io_error(roastedduck_common::byte_format!(
                        "Could not read file \"{path}\": short read"
                    )));
                }
            }
        }
        Ok(bytes)
    }
}

/// The value of file column `column` of `path` (`DirectFileReader::Scan`); `opened`
/// keeps the file open for the row's other columns.
fn file_column(
    data: &ReadFileData,
    path: &[u8],
    column: usize,
    opened: &mut Option<OpenedFile>,
) -> Result<Value> {
    if column == FILE_NAME_COLUMN {
        return Value::try_varchar(path);
    }
    if let Some(filesystem) = roastedduck_function::registered_files::lookup(path) {
        let file = match opened {
            Some(file) => file,
            None => opened.insert(OpenedFile::open(&*filesystem, path)?),
        };
        return match column {
            FILE_CONTENT_COLUMN => {
                let size = file.size()?;
                assert_max_file_size(path, size)?;
                file_content(data, path, &file.content(path, size)?)
            }
            FILE_SIZE_COLUMN => Ok(Value::bigint(i64::try_from(file.size()?).unwrap_or(i64::MAX))),
            FILE_LAST_MODIFIED_COLUMN => Ok(Value::timestamp_tz(Timestamp(file.modified()?))),
            _ => Ok(Value::null(
                data.types.get(column).cloned().unwrap_or(LogicalType::SQLNULL),
            )),
        };
    }
    let metadata =
        std::fs::metadata(roastedduck_function::file_system::native_path(path)?).map_err(|error| {
            io_error(roastedduck_common::byte_format!(
                "Cannot open file \"{path}\": {}",
                roastedduck_common::os_error_text(&error)
            ))
        })?;
    Ok(match column {
        FILE_CONTENT_COLUMN => {
            assert_max_file_size(path, metadata.len())?;
            let bytes =
                std::fs::read(roastedduck_function::file_system::native_path(path)?).map_err(|error| {
                    io_error(roastedduck_common::byte_format!(
                        "Could not read file \"{path}\": {error}"
                    ))
                })?;
            assert_max_file_size(path, bytes.len() as u64)?;
            file_content(data, path, &bytes)?
        }
        FILE_SIZE_COLUMN => Value::bigint(i64::try_from(metadata.len()).unwrap_or(i64::MAX)),
        FILE_LAST_MODIFIED_COLUMN => match metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        {
            Some(seconds) => Value::timestamp_tz(Timestamp(seconds.saturating_mul(1_000_000))),
            None => Value::null(LogicalType::TIMESTAMP_TZ),
        },
        _ => Value::null(data.types.get(column).cloned().unwrap_or(LogicalType::SQLNULL)),
    })
}

fn file_content(data: &ReadFileData, path: &[u8], bytes: &[u8]) -> Result<Value> {
    if data.content_type == LogicalType::VARCHAR {
        let text = std::str::from_utf8(bytes).map_err(|_| {
            Error::invalid_input(roastedduck_common::byte_format!(
                "{}: could not read content of file '{path}' as valid UTF-8 encoded text. You may want to \
                 use read_blob instead.",
                data.name
            ))
        })?;
        Ok(Value::varchar(text))
    } else {
        Ok(Value::blob(bytes))
    }
}

/// The value of column `column` for `path`: a file column or a hive partition value.
fn column_value(
    data: &ReadFileData,
    path: &[u8],
    column: usize,
    opened: &mut Option<OpenedFile>,
) -> Result<Value> {
    if let Some((key, _)) = data
        .reader_bind
        .hive_partitioning_indexes
        .iter()
        .find(|(_, index)| *index == column)
    {
        let partitions = hive_parse(path);
        let value = partitions
            .get(key)
            .map_or(b"".as_slice(), roastedduck_common::ByteString::as_bytes);
        return data.file_options.hive_partition_value(value, key, &data.casts);
    }
    file_column(data, path, column, opened)
}

/// `DirectFileReader::Scan`: only the requested columns are read (projection pushdown).
fn read_file_scan(
    bind_data: Option<&dyn FunctionData>,
    column_ids: &[u64],
    state: &mut TableScanState,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<ReadFileData>())
        .ok_or_else(|| Error::internal("read_file scan without bind data"))?;
    let start = usize::try_from(state.position)
        .unwrap_or(usize::MAX)
        .min(data.files.len());
    let end = (start + STANDARD_VECTOR_SIZE).min(data.files.len());
    // DirectFileReader verifies remote existence even for a filename-only scan.
    // Content/metadata projections still open normally and propagate open errors.
    let requires_open = column_ids.iter().any(|column| matches!(*column, 1..=3));
    let mut paths = Vec::new();
    for path in &data.files[start..end] {
        if !requires_open
            && let Some(filesystem) = roastedduck_function::registered_files::lookup(path)
            && filesystem.is_remote()
        {
            roastedduck_function::file_system::verify_can_access_file(path)?;
            if !filesystem.is_file(path).unwrap_or(false) {
                continue;
            }
        }
        paths.push(path);
    }
    let rows = paths.len();
    let mut columns: Vec<Vector> = data
        .types
        .iter()
        .map(|ty| Vector::constant_null(ty.clone()))
        .collect();
    let mut filled = vec![false; columns.len()];
    let mut opened: Vec<Option<OpenedFile>> = paths.iter().map(|_| None).collect();
    for &column_id in column_ids {
        let Ok(column) = usize::try_from(column_id) else {
            continue;
        };
        if column >= columns.len() || filled[column] {
            continue;
        }
        let mut vector = Vector::flat(data.types[column].clone(), rows.max(1));
        for (row, path) in paths.iter().enumerate() {
            let value = column_value(data, path, column, &mut opened[row])?;
            if value.is_null() {
                vector.set_null(row);
            } else {
                vector.set_value(row, &value)?;
            }
        }
        columns[column] = vector;
        filled[column] = true;
    }
    *chunk = DataChunk::new(columns, rows);
    state.position = end as u64;
    Ok(end < data.files.len())
}

/// One overload of `read_text` / `read_blob` (`MultiFileReader::CreateFunctionSet` adds
/// the VARCHAR and LIST(VARCHAR) overloads; the multi-file options are erased).
fn read_file_function(
    name: &'static str,
    content_type: &LogicalType,
    argument: LogicalType,
) -> TableFunction {
    let content_type = content_type.clone();
    let mut function = TableFunction::new(name, vec![argument])
        .with_bind(move |input| read_file_bind(name, &content_type, input))
        .with_scan(read_file_scan);
    function.projection_pushdown = true;
    function
}

/// `read_text` and `read_blob`.
pub fn read_file_functions() -> Vec<TableFunctionSet> {
    [
        ("read_text", LogicalType::VARCHAR),
        ("read_blob", LogicalType::BLOB),
    ]
    .into_iter()
    .map(|(name, content_type)| {
        let mut set = TableFunctionSet::new(name);
        set.add(read_file_function(name, &content_type, LogicalType::VARCHAR));
        set.add(read_file_function(
            name,
            &content_type,
            LogicalType::list(LogicalType::VARCHAR),
        ));
        set
    })
    .collect()
}
