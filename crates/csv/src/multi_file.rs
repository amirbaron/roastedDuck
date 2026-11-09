//! Reading several files as one table (DuckDB's `MultiFileReader`, `MultiFileOptions`,
//! `GlobMultiFileList`, the local file system's glob and `HivePartitioning`). Shared by
//! the readers of all file formats.

use roastedduck_function::file_system::native_path;
use std::collections::{BTreeMap, BinaryHeap, HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use roastedduck_common::{ByteString, Error, ErrorKind, Result};
use roastedduck_function::TablePartitionInfo;
use roastedduck_function::cast::{CastFunctionSet, ValueCast};
use roastedduck_types::{LogicalType, LogicalTypeId, Value};

/// The options every multi-file reader accepts (DuckDB's `MultiFileOptions`).
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct MultiFileOptions {
    pub filename: bool,
    pub filename_column: roastedduck_common::ByteString,
    pub hive_partitioning: bool,
    pub auto_detect_hive_partitioning: bool,
    pub union_by_name: bool,
    pub hive_types_autocast: bool,
    /// The partition types, given or detected (DuckDB's `case_insensitive_map_t`), in the
    /// order they were added.
    pub hive_types_schema: Vec<(roastedduck_common::ByteString, LogicalType)>,
}

impl Default for MultiFileOptions {
    fn default() -> Self {
        Self {
            filename: false,
            filename_column: "filename".into(),
            hive_partitioning: false,
            auto_detect_hive_partitioning: true,
            union_by_name: false,
            hive_types_autocast: true,
            hive_types_schema: Vec::new(),
        }
    }
}

impl MultiFileOptions {
    /// The type given or detected for the partition `name` (case-insensitively).
    pub fn hive_type(&self, name: impl AsRef<[u8]>) -> Option<&LogicalType> {
        self.hive_types_schema
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name.as_ref()))
            .map(|(_, ty)| ty)
    }

    /// `hive_types_schema` as DuckDB walks it: a `case_insensitive_map_t`, in libc++'s
    /// iteration order.
    pub fn hive_types_in_map_order(&self) -> Vec<(&[u8], &LogicalType)> {
        let mut map = roastedduck_common::libcxx::UnorderedOrder::default();
        for (name, _) in &self.hive_types_schema {
            map.insert(
                roastedduck_common::libcxx::ci_hash(name),
                name.as_bytes(),
                |a, b| a.eq_ignore_ascii_case(b),
            );
        }
        map.order()
            .into_iter()
            .filter_map(|node| {
                let name = map.keys()[node];
                self.hive_type(name).map(|ty| (name, ty))
            })
            .collect()
    }
}

/// The virtual `filename` column (`MultiFileReader::COLUMN_IDENTIFIER_FILENAME`).
pub const COLUMN_IDENTIFIER_FILENAME: u64 = 1 << 63;
/// The virtual `file_row_number` column (`COLUMN_IDENTIFIER_FILE_ROW_NUMBER`).
pub const COLUMN_IDENTIFIER_FILE_ROW_NUMBER: u64 = (1 << 63) + 1;
/// The virtual `file_index` column (`COLUMN_IDENTIFIER_FILE_INDEX`).
pub const COLUMN_IDENTIFIER_FILE_INDEX: u64 = (1 << 63) + 2;

/// `MultiFileGetVirtualColumns`: `filename` (unless the `filename` option names a real
/// column) and `file_index`.
pub fn multi_file_virtual_columns(
    filename_option: bool,
) -> Vec<(u64, roastedduck_common::ByteString, LogicalType)> {
    let mut result = Vec::with_capacity(2);
    if !filename_option {
        result.push((
            COLUMN_IDENTIFIER_FILENAME,
            "filename".into(),
            LogicalType::VARCHAR,
        ));
    }
    result.push((
        COLUMN_IDENTIFIER_FILE_INDEX,
        "file_index".into(),
        LogicalType::UBIGINT,
    ));
    result
}

/// How many files a file list expanded to (DuckDB's `FileExpandResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileExpandResult {
    NoFiles,
    SingleFile,
    MultipleFiles,
}

/// The named parameters of every multi-file reader (`MultiFileReader::AddParameters`).
/// A reader (`MultiFileFunction`) adds them first, which decides where they come in the
/// map of its named parameters.
pub fn multi_file_parameters() -> Vec<(String, LogicalType)> {
    vec![
        ("filename".to_owned(), LogicalType::ANY),
        ("hive_partitioning".to_owned(), LogicalType::BOOLEAN),
        ("union_by_name".to_owned(), LogicalType::BOOLEAN),
        ("hive_types".to_owned(), LogicalType::ANY),
        ("hive_types_autocast".to_owned(), LogicalType::BOOLEAN),
    ]
}

/// What `EXPLAIN ANALYZE` shows about a finished multi-file scan
/// (`MultiFileDynamicToString`): the number of files opened and the first file names
/// (`display`: the patterns given, or the files left after pruning).
pub fn dynamic_to_string(files_loaded: usize, display: &[ByteString]) -> Vec<(ByteString, ByteString)> {
    const FILE_NAME_LIST_LIMIT: usize = 5;
    let mut result = vec![("Total Files Read".into(), files_loaded.to_string().into())];
    if !display.is_empty() {
        let mut names: Vec<&[u8]> = display
            .iter()
            .take(FILE_NAME_LIST_LIMIT)
            .map(ByteString::as_bytes)
            .collect();
        if files_loaded > FILE_NAME_LIST_LIMIT {
            names.push(b"...");
        }
        result.push(("Filename(s)".into(), ByteString::join(&names, b", ")));
    }
    result
}

fn io_error(message: impl Into<ByteString>) -> Error {
    Error::new_bytes(ErrorKind::Io, message.into())
}

fn null_argument(key: &str) -> Error {
    Error::invalid_input(roastedduck_common::byte_format!(
        "Cannot use NULL as argument for \"{key}\""
    ))
}

impl MultiFileOptions {
    /// `MultiFileReader::ParseOption`: whether the option is a multi-file option.
    pub fn parse_option(
        &mut self,
        key: impl AsRef<[u8]>,
        value: &Value,
        parse_type: &dyn Fn(&str) -> Result<LogicalType>,
    ) -> Result<bool> {
        // Multi-file options have a closed ASCII vocabulary.
        let Ok(key) = std::str::from_utf8(key.as_ref()) else {
            return Ok(false);
        };
        match key.to_ascii_lowercase().as_str() {
            "filename" => {
                if value.is_null() {
                    return Err(null_argument(key));
                }
                if value.logical_type().id() == LogicalTypeId::Varchar {
                    self.filename = true;
                    self.filename_column = value.to_varchar_bytes()?;
                } else if let Ok(boolean) = value.try_cast(&LogicalType::BOOLEAN) {
                    self.filename = boolean.as_bool().unwrap_or(false);
                }
            }
            "hive_partitioning" => {
                if value.is_null() {
                    return Err(null_argument(key));
                }
                self.hive_partitioning = value.as_bool().unwrap_or(false);
                self.auto_detect_hive_partitioning = false;
            }
            "union_by_name" => {
                if value.is_null() {
                    return Err(null_argument(key));
                }
                self.union_by_name = value.as_bool().unwrap_or(false);
            }
            "hive_types_autocast" | "hive_type_autocast" => {
                if value.is_null() {
                    return Err(null_argument(key));
                }
                self.hive_types_autocast = value.as_bool().unwrap_or(false);
            }
            "hive_types" | "hive_type" => {
                if value.is_null() {
                    return Err(null_argument(key));
                }
                let ty = value.logical_type();
                if ty.id() != LogicalTypeId::Struct {
                    return Err(Error::invalid_input(roastedduck_common::byte_format!(
                        "'hive_types' only accepts a STRUCT('name':VARCHAR, ...), but '{ty}' was provided"
                    )));
                }
                let names: Vec<roastedduck_common::ByteString> = ty
                    .struct_children()
                    .map(|children| children.iter().map(|(name, _)| name.clone()).collect())
                    .unwrap_or_default();
                for (name, child) in names.iter().zip(value.children().unwrap_or_default()) {
                    if child.logical_type().id() != LogicalTypeId::Varchar {
                        return Err(Error::invalid_input(roastedduck_common::byte_format!(
                            "hive_types: '{name}' must be a VARCHAR, instead: '{}' was provided",
                            child.logical_type()
                        )));
                    }
                    let parsed = parse_type(child.as_str().unwrap_or_default())?;
                    // `hive_types_schema[name] = type`.
                    match self
                        .hive_types_schema
                        .iter_mut()
                        .find(|(key, _)| key.eq_ignore_ascii_case(name.as_ref()))
                    {
                        Some((_, ty)) => *ty = parsed,
                        None => self.hive_types_schema.push((name.clone(), parsed)),
                    }
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// `AnySet`.
    pub fn any_set(&self) -> bool {
        self.filename || self.hive_partitioning || self.union_by_name
    }

    /// `GetHiveLogicalType`.
    pub fn hive_logical_type(&self, column: &[u8]) -> LogicalType {
        self.hive_type(column).cloned().unwrap_or(LogicalType::VARCHAR)
    }

    /// `AutoDetectHivePartitioning`.
    pub fn auto_detect_hive_partitioning(&mut self, files: &FileList, casts: &CastFunctionSet) -> Result<()> {
        if files.expand_result() == FileExpandResult::NoFiles {
            return Ok(());
        }
        let explicitly_disabled = !self.auto_detect_hive_partitioning && !self.hive_partitioning;
        let types_enabled = !self.hive_types_schema.is_empty();
        if explicitly_disabled && types_enabled {
            return Err(Error::invalid_input(
                "cannot disable hive_partitioning when hive_types is enabled",
            ));
        }
        if types_enabled && self.auto_detect_hive_partitioning && !self.hive_partitioning {
            self.hive_partitioning = true;
            self.auto_detect_hive_partitioning = false;
        }
        if self.auto_detect_hive_partitioning {
            self.hive_partitioning = auto_detect_hive_partitioning_internal(files);
        }
        if self.hive_partitioning && self.hive_types_autocast {
            self.auto_detect_hive_types(files, casts)?;
        }
        Ok(())
    }

    /// `AutoDetectHiveTypesInternal`.
    fn auto_detect_hive_types(&mut self, files: &FileList, casts: &CastFunctionSet) -> Result<()> {
        let candidates = [LogicalType::DATE, LogicalType::TIMESTAMP, LogicalType::BIGINT];
        // An `unordered_map<string, LogicalType>`: its types join the schema in libc++'s
        // iteration order.
        let mut detected = roastedduck_common::libcxx::UnorderedOrder::default();
        let mut detected_types: Vec<LogicalType> = Vec::new();
        for file in &files.files {
            let partitions = hive_parse(file);
            if partitions.is_empty() {
                return Ok(());
            }
            for (name, value) in partitions {
                if self.hive_type(&name).is_some() || hive_is_null(&value) {
                    continue;
                }
                let text = Value::try_varchar(&value)?;
                let detected_type = candidates
                    .iter()
                    .find(|candidate| text.try_cast_as(casts, candidate, true).is_ok())
                    .cloned()
                    .unwrap_or(LogicalType::VARCHAR);
                let hash = roastedduck_common::libcxx::string_hash(name.as_bytes());
                let (inserted, index) = detected.insert(hash, name, |a, b| a == b);
                if inserted {
                    detected_types.push(detected_type);
                } else if detected_types[index] != detected_type {
                    detected_types[index] = LogicalType::VARCHAR;
                }
            }
        }
        for node in detected.order() {
            let name = &detected.keys()[node];
            if self.hive_type(name).is_none() {
                self.hive_types_schema
                    .push((name.clone(), detected_types[node].clone()));
            }
        }
        Ok(())
    }

    /// `GetHivePartitionValue`.
    pub fn hive_partition_value(&self, value: &[u8], key: &[u8], casts: &CastFunctionSet) -> Result<Value> {
        hive_value(key, value, &self.hive_logical_type(key), casts)
    }
}

/// The generated columns a multi-file reader adds (DuckDB's `MultiFileReaderBindData`).
#[derive(Clone, Debug, Default)]
pub struct MultiFileBindData {
    pub filename_idx: Option<usize>,
    /// (partition key, column index).
    pub hive_partitioning_indexes: Vec<(ByteString, usize)>,
}

impl MultiFileBindData {
    /// How the files partition the columns `partition_ids` (DuckDB's
    /// `MultiFileReader::GetPartitionInfo`): hive partitioning gives each file a single
    /// value of its partition columns; other columns are not partitioned.
    pub fn partition_info(&self, partition_ids: &[u64]) -> TablePartitionInfo {
        let partitioned = partition_ids.iter().all(|&column| {
            self.hive_partitioning_indexes
                .iter()
                .any(|(_, index)| *index as u64 == column)
        });
        if partitioned {
            TablePartitionInfo::SingleValuePartitions
        } else {
            TablePartitionInfo::NotPartitioned
        }
    }
}

/// `MultiFileReader::BindOptions`: adds the filename and hive partition columns.
pub fn bind_options(
    options: &MultiFileOptions,
    files: &FileList,
    return_types: &mut Vec<LogicalType>,
    names: &mut Vec<roastedduck_common::ByteString>,
) -> Result<MultiFileBindData> {
    let mut bind_data = MultiFileBindData::default();
    if options.filename {
        if names.contains(&options.filename_column) {
            return Err(Error::binder(roastedduck_common::byte_format!(
                "Option filename adds column \"{}\", but a column with this name is also in the file. Try setting a different name: filename='<filename column name>'",
                options.filename_column
            )));
        }
        bind_data.filename_idx = Some(names.len());
        return_types.push(LogicalType::VARCHAR);
        names.push(options.filename_column.clone());
    }
    if options.hive_partitioning {
        let first = files.first_file().unwrap_or_default();
        let partitions = hive_parse(first);
        for file in &files.files {
            let file_partitions = hive_parse(file);
            for key in partitions.keys() {
                if !file_partitions.contains_key(key) {
                    let error = roastedduck_common::byte_format!(
                        "Hive partition mismatch between file \"{first}\" and \"{file}\": key \"{key}\" not found"
                    );
                    if options.auto_detect_hive_partitioning {
                        return Err(Error::internal(roastedduck_common::byte_format!(
                            "{error}(hive partitioning was autodetected)"
                        )));
                    }
                    return Err(Error::binder(error));
                }
            }
            if partitions.len() != file_partitions.len() {
                let error = roastedduck_common::byte_format!(
                    "Hive partition mismatch between file \"{first}\" and \"{file}\""
                );
                if options.auto_detect_hive_partitioning {
                    return Err(Error::internal(roastedduck_common::byte_format!(
                        "{error}(hive partitioning was autodetected)"
                    )));
                }
                return Err(Error::binder(error));
            }
        }
        for (name, _) in options.hive_types_in_map_order() {
            if !partitions.keys().any(|key| key.as_bytes() == name) {
                return Err(Error::invalid_input(roastedduck_common::byte_format!(
                    "Unknown hive_type: \"{name}\" does not appear to be a partition"
                )));
            }
        }
        for key in partitions.keys() {
            let index = if let Some(index) = names.iter().position(|name| name.eq_ignore_ascii_case(key)) {
                return_types[index] = options.hive_logical_type(key);
                index
            } else {
                return_types.push(options.hive_logical_type(key));
                names.push(key.clone());
                names.len() - 1
            };
            bind_data.hive_partitioning_indexes.push((key.clone(), index));
        }
    }
    Ok(bind_data)
}

/// The files of a multi-file read (DuckDB's `GlobMultiFileList`, fully expanded).
#[derive(Clone, Debug, Default)]
pub struct FileList {
    pub files: Vec<ByteString>,
}

impl FileList {
    /// `GetExpandResult`.
    pub fn expand_result(&self) -> FileExpandResult {
        match self.files.len() {
            0 => FileExpandResult::NoFiles,
            1 => FileExpandResult::SingleFile,
            _ => FileExpandResult::MultipleFiles,
        }
    }

    pub fn first_file(&self) -> Option<&[u8]> {
        self.files.first().map(ByteString::as_bytes)
    }
}

/// `MultiFileReader::ParsePaths`.
pub fn parse_paths(function_name: &str, input: &Value) -> Result<Vec<ByteString>> {
    if input.is_null() {
        return Err(Error::parser(roastedduck_common::byte_format!(
            "{function_name} cannot take NULL list as parameter"
        )));
    }
    match input.logical_type().id() {
        LogicalTypeId::Varchar => Ok(vec![ByteString::from(input.as_bytes().unwrap_or_default())]),
        LogicalTypeId::List => {
            let mut paths = Vec::new();
            for value in input.children().unwrap_or_default() {
                if value.is_null() {
                    return Err(Error::parser(roastedduck_common::byte_format!(
                        "{function_name} reader cannot take NULL input as parameter"
                    )));
                }
                if value.logical_type().id() != LogicalTypeId::Varchar {
                    return Err(Error::parser(roastedduck_common::byte_format!(
                        "{function_name} reader can only take a list of strings as a parameter"
                    )));
                }
                paths.push(ByteString::from(value.as_bytes().unwrap_or_default()));
            }
            Ok(paths)
        }
        _ => Err(Error::internal(
            "Unsupported type for MultiFileReader::ParsePaths called with: '%s'",
        )),
    }
}

/// `MultiFileReader::CreateFileList`: expands the paths and globs (falling back to
/// `<path>/**/*.<extension>` for a path without glob that matches no file).
pub fn create_file_list(function_name: &str, paths: &[ByteString], extension: &str) -> Result<FileList> {
    let mut files = Vec::new();
    for path in paths {
        let mut expanded = glob_file_list(path, Some(extension))?;
        expanded.sort();
        files.extend(expanded);
    }
    let list = FileList { files };
    if list.expand_result() == FileExpandResult::NoFiles {
        return Err(io_error(roastedduck_common::byte_format!(
            "{function_name} needs at least one file to read"
        )));
    }
    Ok(list)
}

/// The bind data of `glob`: the matching files.
#[derive(Debug)]
struct GlobData {
    files: Mutex<GlobFiles>,
}

#[derive(Debug)]
struct GlobFiles {
    expanded: Vec<ByteString>,
    pending: VecDeque<ByteString>,
    current: Option<Box<dyn roastedduck_function::registered_files::GlobResult>>,
    finished: bool,
}

impl GlobFiles {
    /// `GetExpandResult` ensures that up to the first two paths are available
    /// during binding. Later pages stay deferred until scan execution.
    fn new(paths: Vec<ByteString>) -> Result<Self> {
        let mut files = Self {
            expanded: Vec::new(),
            pending: paths.into(),
            current: None,
            finished: false,
        };
        while files.expanded.len() < 2 && files.expand_next()? {}
        Ok(files)
    }

    fn expand_next(&mut self) -> Result<bool> {
        loop {
            if let Some(source) = &mut self.current {
                if let Some(mut page) = source.expand_next()? {
                    page.sort();
                    self.expanded.extend(page);
                    return Ok(true);
                }
                self.current = None;
            }
            let Some(path) = self.pending.pop_front() else {
                self.finished = true;
                return Ok(false);
            };
            roastedduck_function::file_system::verify_can_access_file(&path)?;
            if let Some(fs) = roastedduck_function::registered_files::lookup(&path)
                && let Some(source) = fs.glob_lazy(&path)?
            {
                self.current = Some(source);
                continue;
            }
            let mut files = local_glob(&path)?;
            files.sort();
            self.expanded.extend(files);
            return Ok(true);
        }
    }
}

impl roastedduck_function::FunctionData for GlobData {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The `glob(pattern)` table function (DuckDB's `GlobTableFunction`): the files matching
/// the patterns, none being no error.
pub fn glob_functions() -> Vec<roastedduck_function::TableFunctionSet> {
    use roastedduck_function::{TableFunction, TableFunctionBindOutput, TableFunctionSet};
    let function = |argument: LogicalType| {
        TableFunction::new("glob", vec![argument])
            .with_bind(|input| {
                let first = input
                    .inputs
                    .first()
                    .ok_or_else(|| Error::internal("glob without a pattern"))?;
                let files = Mutex::new(GlobFiles::new(parse_paths("glob", first)?)?);
                Ok(TableFunctionBindOutput {
                    return_types: vec![LogicalType::VARCHAR],
                    names: vec!["file".into()],
                    bind_data: Some(std::sync::Arc::new(GlobData { files })),
                })
            })
            .with_scan(|bind_data, _, state, chunk| {
                let data = bind_data
                    .and_then(|data| data.as_any().downcast_ref::<GlobData>())
                    .ok_or_else(|| Error::internal("glob scan without bind data"))?;
                let mut files = data.files.lock().unwrap_or_else(PoisonError::into_inner);
                let position = usize::try_from(state.position).unwrap_or(usize::MAX);
                while position >= files.expanded.len() && files.expand_next()? {}
                let start = position.min(files.expanded.len());
                // FETCH_IF_AVAILABLE: once a page supplied rows, finish this
                // chunk using available paths without fetching another page.
                let end = (start + roastedduck_common::STANDARD_VECTOR_SIZE).min(files.expanded.len());
                let mut column = roastedduck_types::Vector::flat(LogicalType::VARCHAR, (end - start).max(1));
                for (row, file) in files.expanded[start..end].iter().enumerate() {
                    column.set_value(row, &Value::try_varchar(file)?)?;
                }
                *chunk = roastedduck_types::DataChunk::new(vec![column], end - start);
                state.position = end as u64;
                Ok(end < files.expanded.len() || !files.finished)
            })
    };
    let mut set = TableFunctionSet::new("glob");
    set.add(function(LogicalType::VARCHAR));
    set.add(function(LogicalType::list(LogicalType::VARCHAR)));
    vec![set]
}

/// `FileSystem::HasGlob`.
pub fn has_glob(path: &[u8]) -> bool {
    path.iter()
        .copied()
        .any(|byte| matches!(byte, b'*' | b'?' | b'['))
}

/// `FileSystem::GlobFileList`: the files matching a pattern; an error when none match.
pub fn glob_file_list(pattern: &[u8], fallback_extension: Option<&str>) -> Result<Vec<ByteString>> {
    let result = local_glob(pattern)?;
    if result.is_empty() {
        if let Some(extension) = fallback_extension
            && !has_glob(pattern)
        {
            let new_pattern = join_path(
                &join_path(pattern, b"**"),
                &roastedduck_common::byte_format!("*.{extension}"),
            );
            let fallback = local_glob(&new_pattern)?;
            if !fallback.is_empty() {
                return Ok(fallback);
            }
        }
        return Err(io_error(roastedduck_common::byte_format!(
            "No files found that match the pattern \"{pattern}\""
        )));
    }
    Ok(result)
}

/// `FileSystem::JoinPath`.
fn join_path(a: &[u8], b: &[u8]) -> ByteString {
    if a.ends_with(b"/") {
        roastedduck_common::byte_format!("{a}{b}")
    } else {
        roastedduck_common::byte_format!("{a}/{b}")
    }
}

/// Whether a path names an existing file or pipe (`FetchFileWithoutGlob`).
fn file_exists(path: &[u8]) -> bool {
    if let Some(fs) = roastedduck_function::registered_files::lookup(path) {
        return fs.is_file(path).unwrap_or(false);
    }
    if roastedduck_function::object_store::lookup(path).is_some() {
        return true;
    }
    let Ok(path) = native_path(path.split(|byte| *byte == 0).next().unwrap_or_default()) else {
        return false;
    };
    path.is_file() || (path.exists() && !path.is_dir())
}

/// A directory still to expand, processed in path order (`ExpandDirectory`).
#[derive(Debug, PartialEq, Eq)]
struct ExpandDirectory {
    path: ByteString,
    split_index: usize,
    is_empty: bool,
}

impl Ord for ExpandDirectory {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // A max-heap of the reversed order pops the smallest path first.
        other
            .path
            .cmp(&self.path)
            .then(other.split_index.cmp(&self.split_index))
    }
}

impl PartialOrd for ExpandDirectory {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The entries of a directory: (name, is directory).
fn list_files(path: &[u8]) -> Vec<(ByteString, bool)> {
    let Ok(local) = native_path(path) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(local) else {
        return Vec::new();
    };
    entries
        .filter_map(std::result::Result::ok)
        .map(|entry| {
            let is_dir = entry.path().is_dir();
            (ByteString::from(entry.file_name().as_encoded_bytes()), is_dir)
        })
        .collect()
}

/// The directories of the `file_search_path` setting, relative paths are looked up in.
fn file_search_paths() -> Vec<ByteString> {
    roastedduck_function::file_system::opener()
        .and_then(|client| client.settings())
        .and_then(|settings| settings.current_setting("file_search_path").ok())
        .filter(|value| !value.is_null())
        .and_then(|value| value.to_varchar_bytes().ok())
        .map(|paths| {
            paths
                .split(|byte| *byte == b',')
                .filter(|path| !path.is_empty())
                .map(ByteString::from)
                .collect()
        })
        .unwrap_or_default()
}

/// `LocalFileSystem::FetchFileWithoutGlob`: the file itself, or else (for relative paths)
/// the files of that name in the `file_search_path` directories.
fn fetch_file_without_glob(path: &[u8], absolute_path: bool) -> Vec<ByteString> {
    if file_exists(path) {
        return vec![path.into()];
    }
    if absolute_path {
        return Vec::new();
    }
    file_search_paths()
        .iter()
        .map(|search_path| join_path(search_path, path))
        .filter(|joined| file_exists(joined))
        .collect()
}

/// `LocalGlobResult`: expands a pattern against the local file system.
pub fn local_glob(pattern: &[u8]) -> Result<Vec<ByteString>> {
    // `OpenerFileSystem::Glob` checks the pattern against the access configuration.
    roastedduck_function::file_system::verify_can_access_file(pattern)?;
    if let Some(fs) = roastedduck_function::registered_files::lookup(pattern) {
        return fs.glob(pattern);
    }
    // `LocalGlobResult`: `~` and `file:` URLs expand first (`FileSystem::ExpandPath`).
    let path = roastedduck_function::file_system::expand_path_bytes(pattern);
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let mut splits: Vec<ByteString> = Vec::new();
    let bytes = path.as_bytes();
    let mut last_pos = 0;
    for i in 0..bytes.len() {
        if bytes[i] == b'\\' || bytes[i] == b'/' {
            if i == last_pos {
                last_pos = i + 1;
                continue;
            }
            if splits.is_empty() {
                splits.push(path[..i].into());
            } else {
                splits.push(path[last_pos..i].into());
            }
            last_pos = i + 1;
        }
    }
    splits.push(path[last_pos..].into());
    let absolute_path = path.starts_with(b"/") || splits[0].contains(&b':');
    if !has_glob(&path) {
        return Ok(fetch_file_without_glob(&path, absolute_path));
    }
    if splits.iter().filter(|split| split.as_bytes() == b"**").count() > 1 {
        return Err(io_error("Cannot use multiple '**' in one path"));
    }
    let mut expand_directories = BinaryHeap::new();
    if absolute_path {
        if splits.len() > 1 {
            expand_directories.push(ExpandDirectory {
                path: if splits[0].is_empty() {
                    ByteString::from(b"/")
                } else {
                    splits[0].clone()
                },
                split_index: 1,
                is_empty: false,
            });
        }
    } else {
        // The `file_search_path` directories are where relative globs start.
        for search_path in file_search_paths() {
            expand_directories.push(ExpandDirectory {
                path: search_path,
                split_index: 0,
                is_empty: false,
            });
        }
        if expand_directories.is_empty() {
            expand_directories.push(ExpandDirectory {
                path: ByteString::from(b"."),
                split_index: 0,
                is_empty: true,
            });
        }
    }
    let mut expanded_files: Vec<ByteString> = Vec::new();
    while let Some(next_dir) = expand_directories.pop() {
        let split_index = next_dir.split_index;
        let current_path = next_dir.path;
        let next_component = &splits[split_index];
        let is_last_component = split_index + 1 == splits.len();
        if !has_glob(next_component) {
            if next_dir.is_empty {
                if is_last_component {
                    return Err(Error::internal(
                        "No glob in only component - but entire split has globs?",
                    ));
                }
                expand_directories.push(ExpandDirectory {
                    path: next_component.clone(),
                    split_index: split_index + 1,
                    is_empty: false,
                });
            } else if is_last_component {
                let filename = join_path(&current_path, next_component);
                if native_path(&filename)?.exists() {
                    expanded_files.push(filename);
                }
            } else {
                expand_directories.push(ExpandDirectory {
                    path: join_path(&current_path, next_component),
                    split_index: split_index + 1,
                    is_empty: false,
                });
            }
        } else if next_component.as_bytes() == b"**" {
            if !is_last_component {
                expand_directories.push(ExpandDirectory {
                    path: current_path.clone(),
                    split_index: split_index + 1,
                    is_empty: false,
                });
            }
            for (name, is_dir) in list_files(&current_path) {
                let full = join_path(&current_path, &name);
                if std::fs::symlink_metadata(native_path(&full)?)
                    .is_ok_and(|meta| meta.file_type().is_symlink())
                {
                    continue;
                }
                if is_dir {
                    expand_directories.push(ExpandDirectory {
                        path: full,
                        split_index,
                        is_empty: false,
                    });
                } else if is_last_component {
                    expanded_files.push(full);
                }
            }
        } else {
            for (name, is_dir) in list_files(&current_path) {
                if is_dir == is_last_component {
                    continue;
                }
                if glob(name.as_bytes(), next_component.as_bytes()) {
                    let full = if next_dir.is_empty || current_path == "." {
                        if current_path == "." {
                            name
                        } else {
                            join_path(&current_path, &name)
                        }
                    } else {
                        join_path(&current_path, &name)
                    };
                    if is_last_component {
                        expanded_files.push(full);
                    } else {
                        expand_directories.push(ExpandDirectory {
                            path: full,
                            split_index: split_index + 1,
                            is_empty: false,
                        });
                    }
                }
            }
        }
    }
    if expanded_files.is_empty() {
        // Last ditch effort: the path as a literal file name.
        expanded_files = fetch_file_without_glob(&path, absolute_path);
    }
    Ok(expanded_files)
}

pub use roastedduck_common::glob::glob;

/// `HivePartitioning::Parse`: the `key=value` directories of a path.
pub fn hive_parse(filename: &[u8]) -> BTreeMap<ByteString, ByteString> {
    let bytes = filename;
    let mut partition_start = 0;
    let mut equality_sign = 0;
    let mut candidate_partition = true;
    let mut result = BTreeMap::new();
    for (c, &byte) in bytes.iter().enumerate() {
        if byte == b'?' || byte == b'\n' {
            candidate_partition = false;
        }
        if byte == b'\\' || byte == b'/' {
            if candidate_partition && equality_sign > partition_start {
                let key = ByteString::from(&filename[partition_start..equality_sign]);
                let value = ByteString::from(&filename[equality_sign + 1..c]);
                result.entry(key).or_insert(value);
            }
            partition_start = c + 1;
            candidate_partition = true;
        } else if byte == b'=' {
            if equality_sign > partition_start {
                candidate_partition = false;
            }
            equality_sign = c;
        }
    }
    result
}

/// `HivePartitioning::IsNull`.
pub fn hive_is_null(value: &[u8]) -> bool {
    value.eq_ignore_ascii_case(b"NULL") || value == b"__HIVE_DEFAULT_PARTITION__"
}

/// Native `StringUtil::URLDecode`, preserving decoded bytes including invalid UTF-8.
pub fn url_decode(bytes: &[u8]) -> Result<ByteString> {
    let mut result = ByteString::new();
    let mut i = 0;
    let digit = |b| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(a), Some(b)) = (digit(bytes[i + 1]), digit(bytes[i + 2]))
        {
            result.push(a * 16 + b);
            i += 3;
            continue;
        }
        result.push(bytes[i]);
        i += 1;
    }
    if std::str::from_utf8(&result).is_err() {
        return Err(Error::invalid_input(roastedduck_common::byte_format!(
            "Failed to decode string \"{}\" using URL decoding - decoded value is invalid UTF8",
            bytes
        )));
    }
    Ok(result)
}

/// `HivePartitioning::GetValue`: validates when C++ constructs its VARCHAR Value.
pub fn hive_value(key: &[u8], value: &[u8], ty: &LogicalType, casts: &CastFunctionSet) -> Result<Value> {
    if hive_is_null(value) {
        return Ok(Value::null(ty.clone()));
    }
    if ty.id() == LogicalTypeId::Varchar {
        return Value::try_varchar(&url_decode(value)?);
    }
    if value.is_empty() {
        return Ok(Value::null(ty.clone()));
    }
    let decoded = url_decode(value)?;
    let text = Value::try_varchar(&decoded)?;
    text.try_cast_as(casts, ty, false).map_err(|_| {
        Error::invalid_input(roastedduck_common::byte_format!(
            "Unable to cast '{}' (from hive partition column '{}') to: '{ty}'",
            decoded,
            key.to_ascii_uppercase()
        ))
    })
}

/// The value of the column named `name` (of type `ty`) in the file at `path`, if its path
/// determines it: the `filename` column or a hive partition (`GetKnownColumnValues` +
/// `HivePartitioning::GetValue`).
pub fn known_column_value(
    options: &MultiFileOptions,
    casts: &CastFunctionSet,
    path: &[u8],
    name: &[u8],
    ty: &LogicalType,
) -> Result<Option<Value>> {
    if options.filename && name == b"filename" {
        return Ok(Some(Value::try_varchar(path)?));
    }
    if options.hive_partitioning
        && let Some((key, value)) = hive_parse(path).iter().find(|(key, _)| key.as_bytes() == name)
    {
        return hive_value(key, value, ty, casts).map(Some);
    }
    Ok(None)
}

/// `AutoDetectHivePartitioningInternal`.
fn auto_detect_hive_partitioning_internal(files: &FileList) -> bool {
    let Some(first) = files.first_file() else {
        return false;
    };
    let partitions = hive_parse(first);
    if partitions.is_empty() {
        return false;
    }
    files.files.iter().all(|file| {
        let new_partitions = hive_parse(file);
        new_partitions.len() == partitions.len()
            && new_partitions.keys().all(|key| partitions.contains_key(key))
    })
}

/// `UnionByName::CombineUnionTypes`.
pub fn combine_union_types(
    col_names: &[roastedduck_common::ByteString],
    sql_types: &[LogicalType],
    union_col_types: &mut Vec<LogicalType>,
    union_col_names: &mut Vec<roastedduck_common::ByteString>,
    union_names_map: &mut HashMap<roastedduck_common::ByteString, usize, impl std::hash::BuildHasher>,
) {
    for (name, ty) in col_names.iter().zip(sql_types) {
        let key = name.to_ascii_lowercase();
        if let Some(&index) = union_names_map.get(&key) {
            union_col_types[index] = LogicalType::force_max_logical_type(&union_col_types[index], ty);
        } else {
            union_names_map.insert(key, union_col_names.len());
            union_col_names.push(name.clone());
            union_col_types.push(ty.clone());
        }
    }
}
