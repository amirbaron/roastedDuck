//! Reading several files as one table (DuckDB's `MultiFileReader`, `MultiFileOptions`,
//! `GlobMultiFileList`, the local file system's glob and `HivePartitioning`). Shared by
//! the readers of all file formats.

use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::path::Path;

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::cast::{CastFunctionSet, ValueCast};
use rustdb_types::{LogicalType, LogicalTypeId, Value};

/// The options every multi-file reader accepts (DuckDB's `MultiFileOptions`).
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct MultiFileOptions {
    pub filename: bool,
    pub filename_column: String,
    pub hive_partitioning: bool,
    pub auto_detect_hive_partitioning: bool,
    pub union_by_name: bool,
    pub hive_types_autocast: bool,
    pub hive_types_schema: BTreeMap<String, LogicalType>,
}

impl Default for MultiFileOptions {
    fn default() -> Self {
        Self {
            filename: false,
            filename_column: "filename".to_owned(),
            hive_partitioning: false,
            auto_detect_hive_partitioning: true,
            union_by_name: false,
            hive_types_autocast: true,
            hive_types_schema: BTreeMap::new(),
        }
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
pub fn multi_file_virtual_columns(filename_option: bool) -> Vec<(u64, String, LogicalType)> {
    let mut result = Vec::with_capacity(2);
    if !filename_option {
        result.push((
            COLUMN_IDENTIFIER_FILENAME,
            "filename".to_owned(),
            LogicalType::VARCHAR,
        ));
    }
    result.push((
        COLUMN_IDENTIFIER_FILE_INDEX,
        "file_index".to_owned(),
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
pub fn dynamic_to_string(files_loaded: usize, display: &[String]) -> Vec<(String, String)> {
    const FILE_NAME_LIST_LIMIT: usize = 5;
    let mut result = vec![("Total Files Read".to_owned(), files_loaded.to_string())];
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

/// DuckDB's `IOException`.
fn io_error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Io, message.into())
}

fn null_argument(key: &str) -> Error {
    Error::invalid_input(format!("Cannot use NULL as argument for \"{key}\""))
}

impl MultiFileOptions {
    /// `MultiFileReader::ParseOption`: whether the option is a multi-file option.
    pub fn parse_option(
        &mut self,
        key: &str,
        value: &Value,
        parse_type: &dyn Fn(&str) -> Result<LogicalType>,
    ) -> Result<bool> {
        match key.to_lowercase().as_str() {
            "filename" => {
                if value.is_null() {
                    return Err(null_argument(key));
                }
                if value.logical_type().id() == LogicalTypeId::Varchar {
                    self.filename = true;
                    value
                        .as_str()
                        .unwrap_or_default()
                        .clone_into(&mut self.filename_column);
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
                    return Err(Error::invalid_input(format!(
                        "'hive_types' only accepts a STRUCT('name':VARCHAR, ...), but '{ty}' was provided"
                    )));
                }
                let names: Vec<String> = ty
                    .struct_children()
                    .map(|children| children.iter().map(|(name, _)| name.clone()).collect())
                    .unwrap_or_default();
                for (name, child) in names.iter().zip(value.children().unwrap_or_default()) {
                    if child.logical_type().id() != LogicalTypeId::Varchar {
                        return Err(Error::invalid_input(format!(
                            "hive_types: '{name}' must be a VARCHAR, instead: '{}' was provided",
                            child.logical_type()
                        )));
                    }
                    let parsed = parse_type(child.as_str().unwrap_or_default())?;
                    self.hive_types_schema.insert(name.clone(), parsed);
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
    pub fn hive_logical_type(&self, column: &str) -> LogicalType {
        self.hive_types_schema
            .get(column)
            .cloned()
            .unwrap_or(LogicalType::VARCHAR)
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
            self.auto_detect_hive_types(files, casts);
        }
        Ok(())
    }

    /// `AutoDetectHiveTypesInternal`.
    fn auto_detect_hive_types(&mut self, files: &FileList, casts: &CastFunctionSet) {
        let candidates = [LogicalType::DATE, LogicalType::TIMESTAMP, LogicalType::BIGINT];
        let mut detected: HashMap<String, LogicalType> = HashMap::new();
        for file in &files.files {
            let partitions = hive_parse(file);
            if partitions.is_empty() {
                return;
            }
            for (name, value) in partitions {
                if self.hive_types_schema.contains_key(&name) || hive_is_null(&value) {
                    continue;
                }
                let text = Value::varchar(&value);
                let detected_type = candidates
                    .iter()
                    .find(|candidate| text.try_cast_as(casts, candidate, true).is_ok())
                    .cloned()
                    .unwrap_or(LogicalType::VARCHAR);
                match detected.get_mut(&name) {
                    None => {
                        detected.insert(name, detected_type);
                    }
                    Some(existing) => {
                        if *existing != detected_type {
                            *existing = LogicalType::VARCHAR;
                        }
                    }
                }
            }
        }
        for (name, ty) in detected {
            self.hive_types_schema.entry(name).or_insert(ty);
        }
    }

    /// `GetHivePartitionValue`.
    pub fn hive_partition_value(&self, value: &str, key: &str, casts: &CastFunctionSet) -> Result<Value> {
        hive_value(key, value, &self.hive_logical_type(key), casts)
    }
}

/// The generated columns a multi-file reader adds (DuckDB's `MultiFileReaderBindData`).
#[derive(Clone, Debug, Default)]
pub struct MultiFileBindData {
    pub filename_idx: Option<usize>,
    /// (partition key, column index).
    pub hive_partitioning_indexes: Vec<(String, usize)>,
}

/// `MultiFileReader::BindOptions`: adds the filename and hive partition columns.
pub fn bind_options(
    options: &MultiFileOptions,
    files: &FileList,
    return_types: &mut Vec<LogicalType>,
    names: &mut Vec<String>,
) -> Result<MultiFileBindData> {
    let mut bind_data = MultiFileBindData::default();
    if options.filename {
        if names.contains(&options.filename_column) {
            return Err(Error::binder(format!(
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
                    let error = format!(
                        "Hive partition mismatch between file \"{first}\" and \"{file}\": key \"{key}\" not found"
                    );
                    if options.auto_detect_hive_partitioning {
                        return Err(Error::internal(format!(
                            "{error}(hive partitioning was autodetected)"
                        )));
                    }
                    return Err(Error::binder(error));
                }
            }
            if partitions.len() != file_partitions.len() {
                let error = format!("Hive partition mismatch between file \"{first}\" and \"{file}\"");
                if options.auto_detect_hive_partitioning {
                    return Err(Error::internal(format!(
                        "{error}(hive partitioning was autodetected)"
                    )));
                }
                return Err(Error::binder(error));
            }
        }
        for name in options.hive_types_schema.keys() {
            if !partitions.contains_key(name) {
                return Err(Error::invalid_input(format!(
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
    pub files: Vec<String>,
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

    pub fn first_file(&self) -> Option<&str> {
        self.files.first().map(String::as_str)
    }
}

/// `MultiFileReader::ParsePaths`.
pub fn parse_paths(function_name: &str, input: &Value) -> Result<Vec<String>> {
    if input.is_null() {
        return Err(Error::parser(format!(
            "{function_name} cannot take NULL list as parameter"
        )));
    }
    match input.logical_type().id() {
        LogicalTypeId::Varchar => Ok(vec![input.as_str().unwrap_or_default().to_owned()]),
        LogicalTypeId::List => {
            let mut paths = Vec::new();
            for value in input.children().unwrap_or_default() {
                if value.is_null() {
                    return Err(Error::parser(format!(
                        "{function_name} reader cannot take NULL input as parameter"
                    )));
                }
                if value.logical_type().id() != LogicalTypeId::Varchar {
                    return Err(Error::parser(format!(
                        "{function_name} reader can only take a list of strings as a parameter"
                    )));
                }
                paths.push(value.as_str().unwrap_or_default().to_owned());
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
pub fn create_file_list(function_name: &str, paths: &[String], extension: &str) -> Result<FileList> {
    let mut files = Vec::new();
    for path in paths {
        let mut expanded = glob_file_list(path, Some(extension))?;
        expanded.sort();
        files.extend(expanded);
    }
    let list = FileList { files };
    if list.expand_result() == FileExpandResult::NoFiles {
        return Err(io_error(format!(
            "{function_name} needs at least one file to read"
        )));
    }
    Ok(list)
}

/// The bind data of `glob`: the matching files.
#[derive(Debug)]
struct GlobData {
    files: Vec<String>,
}

impl rustdb_function::FunctionData for GlobData {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The `glob(pattern)` table function (DuckDB's `GlobTableFunction`): the files matching
/// the patterns, none being no error.
pub fn glob_functions() -> Vec<rustdb_function::TableFunctionSet> {
    use rustdb_function::{TableFunction, TableFunctionBindOutput, TableFunctionSet};
    let function = |argument: LogicalType| {
        TableFunction::new("glob", vec![argument])
            .with_bind(|input| {
                let first = input
                    .inputs
                    .first()
                    .ok_or_else(|| Error::internal("glob without a pattern"))?;
                let mut files = Vec::new();
                for path in parse_paths("glob", first)? {
                    let mut expanded = local_glob(&path)?;
                    expanded.sort();
                    files.extend(expanded);
                }
                Ok(TableFunctionBindOutput {
                    return_types: vec![LogicalType::VARCHAR],
                    names: vec!["file".to_owned()],
                    bind_data: Some(std::sync::Arc::new(GlobData { files })),
                })
            })
            .with_scan(|bind_data, _, state, chunk| {
                let data = bind_data
                    .and_then(|data| data.as_any().downcast_ref::<GlobData>())
                    .ok_or_else(|| Error::internal("glob scan without bind data"))?;
                let start = usize::try_from(state.position)
                    .unwrap_or(usize::MAX)
                    .min(data.files.len());
                let end = (start + rustdb_common::STANDARD_VECTOR_SIZE).min(data.files.len());
                let mut column = rustdb_types::Vector::flat(LogicalType::VARCHAR, (end - start).max(1));
                for (row, file) in data.files[start..end].iter().enumerate() {
                    column.set_value(row, &Value::varchar(file))?;
                }
                *chunk = rustdb_types::DataChunk::new(vec![column], end - start);
                state.position = end as u64;
                Ok(end < data.files.len())
            })
    };
    let mut set = TableFunctionSet::new("glob");
    set.add(function(LogicalType::VARCHAR));
    set.add(function(LogicalType::list(LogicalType::VARCHAR)));
    vec![set]
}

/// `FileSystem::HasGlob`.
pub fn has_glob(path: &str) -> bool {
    path.bytes().any(|byte| matches!(byte, b'*' | b'?' | b'['))
}

/// `FileSystem::GlobFileList`: the files matching a pattern; an error when none match.
pub fn glob_file_list(pattern: &str, fallback_extension: Option<&str>) -> Result<Vec<String>> {
    let result = local_glob(pattern)?;
    if result.is_empty() {
        if let Some(extension) = fallback_extension
            && !has_glob(pattern)
        {
            let new_pattern = join_path(&join_path(pattern, "**"), &format!("*.{extension}"));
            let fallback = local_glob(&new_pattern)?;
            if !fallback.is_empty() {
                return Ok(fallback);
            }
        }
        return Err(io_error(format!(
            "No files found that match the pattern \"{pattern}\""
        )));
    }
    Ok(result)
}

/// `FileSystem::JoinPath`.
fn join_path(a: &str, b: &str) -> String {
    if a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

/// Whether a path names an existing file or pipe (`FetchFileWithoutGlob`).
fn file_exists(path: &str) -> bool {
    let path = Path::new(path);
    path.is_file() || (path.exists() && !path.is_dir())
}

/// A directory still to expand, processed in path order (`ExpandDirectory`).
#[derive(Debug, PartialEq, Eq)]
struct ExpandDirectory {
    path: String,
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
fn list_files(path: &str) -> Vec<(String, bool)> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    entries
        .filter_map(std::result::Result::ok)
        .map(|entry| {
            let is_dir = entry.path().is_dir();
            (entry.file_name().to_string_lossy().into_owned(), is_dir)
        })
        .collect()
}

/// The directories of the `file_search_path` setting, relative paths are looked up in.
fn file_search_paths() -> Vec<String> {
    rustdb_function::file_system::opener()
        .and_then(|client| client.settings())
        .and_then(|settings| settings.current_setting("file_search_path").ok())
        .filter(|value| !value.is_null())
        .and_then(|value| value.to_varchar().ok())
        .map(|paths| {
            paths
                .split(',')
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// `LocalFileSystem::FetchFileWithoutGlob`: the file itself, or else (for relative paths)
/// the files of that name in the `file_search_path` directories.
fn fetch_file_without_glob(path: &str, absolute_path: bool) -> Vec<String> {
    if file_exists(path) {
        return vec![path.to_owned()];
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
pub fn local_glob(pattern: &str) -> Result<Vec<String>> {
    // `OpenerFileSystem::Glob` checks the pattern against the access configuration.
    rustdb_function::file_system::verify_can_access_file(pattern)?;
    // `LocalGlobResult`: `~` and `file:` URLs expand first (`FileSystem::ExpandPath`).
    let path = rustdb_function::file_system::expand_path(pattern);
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let mut splits: Vec<String> = Vec::new();
    let bytes = path.as_bytes();
    let mut last_pos = 0;
    for i in 0..bytes.len() {
        if bytes[i] == b'\\' || bytes[i] == b'/' {
            if i == last_pos {
                last_pos = i + 1;
                continue;
            }
            if splits.is_empty() {
                splits.push(path[..i].to_owned());
            } else {
                splits.push(path[last_pos..i].to_owned());
            }
            last_pos = i + 1;
        }
    }
    splits.push(path[last_pos..].to_owned());
    let absolute_path = path.starts_with('/') || splits[0].contains(':');
    if !has_glob(&path) {
        return Ok(fetch_file_without_glob(&path, absolute_path));
    }
    if splits.iter().filter(|split| *split == "**").count() > 1 {
        return Err(io_error("Cannot use multiple '**' in one path"));
    }
    let mut expand_directories = BinaryHeap::new();
    if absolute_path {
        if splits.len() > 1 {
            expand_directories.push(ExpandDirectory {
                path: if splits[0].is_empty() {
                    "/".to_owned()
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
                path: ".".to_owned(),
                split_index: 0,
                is_empty: true,
            });
        }
    }
    let mut expanded_files: Vec<String> = Vec::new();
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
                if Path::new(&filename).exists() {
                    expanded_files.push(filename);
                }
            } else {
                expand_directories.push(ExpandDirectory {
                    path: join_path(&current_path, next_component),
                    split_index: split_index + 1,
                    is_empty: false,
                });
            }
        } else if next_component == "**" {
            if !is_last_component {
                expand_directories.push(ExpandDirectory {
                    path: current_path.clone(),
                    split_index: split_index + 1,
                    is_empty: false,
                });
            }
            for (name, is_dir) in list_files(&current_path) {
                let full = join_path(&current_path, &name);
                if std::fs::symlink_metadata(&full).is_ok_and(|meta| meta.file_type().is_symlink()) {
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

/// DuckDB's `Glob` (with `?` matching any character).
pub fn glob(string: &[u8], pattern: &[u8]) -> bool {
    let (mut sidx, mut pidx) = (0, 0);
    while sidx < string.len() && pidx < pattern.len() {
        let s = string[sidx];
        let p = pattern[pidx];
        match p {
            b'*' => {
                pidx += 1;
                while pidx < pattern.len() && pattern[pidx] == b'*' {
                    pidx += 1;
                }
                if pidx == pattern.len() {
                    return true;
                }
                return (sidx..string.len()).any(|start| glob(&string[start..], &pattern[pidx..]));
            }
            b'?' => {}
            b'[' => {
                pidx += 1;
                if pidx == pattern.len() {
                    return false;
                }
                let invert = pattern[pidx] == b'!';
                if invert {
                    pidx += 1;
                }
                let mut found_match = invert;
                let start_pos = pidx;
                let mut found_closing_bracket = false;
                while pidx < pattern.len() {
                    let p = pattern[pidx];
                    if p == b']' && pidx > start_pos {
                        found_closing_bracket = true;
                        pidx += 1;
                        break;
                    }
                    if pidx + 1 == pattern.len() {
                        break;
                    }
                    let matches = if pattern[pidx + 1] == b'-' {
                        if pidx + 2 == pattern.len() {
                            break;
                        }
                        let next_char = pattern[pidx + 2];
                        pidx += 3;
                        s >= p && s <= next_char
                    } else {
                        pidx += 1;
                        p == s
                    };
                    if found_match == invert && matches {
                        found_match = !invert;
                    }
                }
                if !found_closing_bracket || !found_match {
                    return false;
                }
                sidx += 1;
                continue;
            }
            b'\\' => {
                pidx += 1;
                if pidx == pattern.len() || s != pattern[pidx] {
                    return false;
                }
            }
            _ => {
                if s != p {
                    return false;
                }
            }
        }
        sidx += 1;
        pidx += 1;
    }
    while pidx < pattern.len() && pattern[pidx] == b'*' {
        pidx += 1;
    }
    pidx == pattern.len() && sidx == string.len()
}

/// `HivePartitioning::Parse`: the `key=value` directories of a path.
pub fn hive_parse(filename: &str) -> BTreeMap<String, String> {
    let bytes = filename.as_bytes();
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
                let key = filename[partition_start..equality_sign].to_owned();
                let value = filename[equality_sign + 1..c].to_owned();
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
pub fn hive_is_null(value: &str) -> bool {
    value.eq_ignore_ascii_case("NULL") || value == "__HIVE_DEFAULT_PARTITION__"
}

/// `StringUtil::URLDecode`.
pub fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                result.push(byte);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).into_owned()
}

/// `HivePartitioning::GetValue`.
pub fn hive_value(key: &str, value: &str, ty: &LogicalType, casts: &CastFunctionSet) -> Result<Value> {
    if hive_is_null(value) {
        return Ok(Value::null(ty.clone()));
    }
    if ty.id() == LogicalTypeId::Varchar {
        return Ok(Value::varchar(url_decode(value)));
    }
    if value.is_empty() {
        return Ok(Value::null(ty.clone()));
    }
    let text = Value::varchar(url_decode(value));
    text.try_cast_as(casts, ty, false).map_err(|_| {
        Error::invalid_input(format!(
            "Unable to cast '{}' (from hive partition column '{}') to: '{ty}'",
            url_decode(value),
            key.to_uppercase()
        ))
    })
}

/// The value of the column named `name` (of type `ty`) in the file at `path`, if its path
/// determines it: the `filename` column or a hive partition (`GetKnownColumnValues` +
/// `HivePartitioning::GetValue`).
pub fn known_column_value(
    options: &MultiFileOptions,
    casts: &CastFunctionSet,
    path: &str,
    name: &str,
    ty: &LogicalType,
) -> Result<Option<Value>> {
    if options.filename && name == "filename" {
        return Ok(Some(Value::varchar(path)));
    }
    if options.hive_partitioning
        && let Some(value) = hive_parse(path).get(name)
    {
        return hive_value(name, value, ty, casts).map(Some);
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
    col_names: &[String],
    sql_types: &[LogicalType],
    union_col_types: &mut Vec<LogicalType>,
    union_col_names: &mut Vec<String>,
    union_names_map: &mut HashMap<String, usize, impl std::hash::BuildHasher>,
) {
    for (name, ty) in col_names.iter().zip(sql_types) {
        let key = name.to_lowercase();
        if let Some(&index) = union_names_map.get(&key) {
            union_col_types[index] = LogicalType::force_max_logical_type(&union_col_types[index], ty);
        } else {
            union_names_map.insert(key, union_col_names.len());
            union_col_names.push(name.clone());
            union_col_types.push(ty.clone());
        }
    }
}
