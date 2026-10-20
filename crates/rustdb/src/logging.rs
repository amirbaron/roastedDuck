//! Logging (DuckDB's `src/logging`): the database's `LogManager` with its configuration,
//! registered log types and storage, and the loggers of connections.
//!
//! Loggers capture the configuration when they are created (at the start and end of
//! every query, like DuckDB's `ThreadSafeLogger`); the global (database) logger follows
//! the configuration as it changes (`MutableLogger`). Entries are buffered and only
//! become visible to `duckdb_logs()` once flushed: when the buffer fills, or at the next
//! query boundary.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use rustdb_common::{Error, ErrorKind, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::logging::{ClientLogger, LogLevel, LogScope, LogTypeInfo, enum_error};
use rustdb_types::{LogicalType, Value};

/// `LogConfig::IN_MEMORY_STORAGE_NAME` and friends.
pub(crate) const MEMORY_STORAGE: &str = "memory";
pub(crate) const STDOUT_STORAGE: &str = "stdout";
pub(crate) const FILE_STORAGE: &str = "file";

/// Which log types are written besides the level check (DuckDB's `LogMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LogMode {
    LevelOnly,
    DisableSelected,
    EnableSelected,
}

impl LogMode {
    const ALL: [Self; 3] = [Self::LevelOnly, Self::DisableSelected, Self::EnableSelected];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::LevelOnly => "LEVEL_ONLY",
            Self::DisableSelected => "DISABLE_SELECTED",
            Self::EnableSelected => "ENABLE_SELECTED",
        }
    }

    pub(crate) fn parse(text: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.name().eq_ignore_ascii_case(text))
            .ok_or_else(|| enum_error(text, "LogMode", &Self::ALL.map(Self::name)))
    }
}

/// The logging configuration (DuckDB's `LogConfig`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogConfig {
    pub enabled: bool,
    pub mode: LogMode,
    pub level: LogLevel,
    pub storage: String,
    pub enabled_log_types: Vec<String>,
    pub disabled_log_types: Vec<String>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: LogMode::LevelOnly,
            level: LogLevel::Info,
            storage: MEMORY_STORAGE.to_owned(),
            enabled_log_types: Vec::new(),
            disabled_log_types: Vec::new(),
        }
    }
}

impl LogConfig {
    /// `ThreadSafeLogger::ShouldLog` (the caller checks `enabled`).
    fn should_log(&self, log_type: &str, level: LogLevel) -> bool {
        if self.level > level {
            return false;
        }
        match self.mode {
            LogMode::EnableSelected => self.enabled_log_types.iter().any(|enabled| enabled == log_type),
            LogMode::DisableSelected => !self
                .disabled_log_types
                .iter()
                .any(|disabled| disabled == log_type),
            LogMode::LevelOnly => true,
        }
    }
}

/// The scope of a logging context (DuckDB's `LogContextScope`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContextScope {
    Database,
    Connection,
}

impl ContextScope {
    const fn name(self) -> &'static str {
        match self {
            Self::Database => "DATABASE",
            Self::Connection => "CONNECTION",
        }
    }
}

/// Where log entries come from (DuckDB's `LoggingContext`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct LoggingContext {
    pub scope: ContextScope,
    pub connection_id: Option<u64>,
    pub transaction_id: Option<u64>,
    pub query_id: Option<u64>,
}

impl LoggingContext {
    pub(crate) fn new(scope: ContextScope) -> Self {
        Self {
            scope,
            connection_id: None,
            transaction_id: None,
            query_id: None,
        }
    }
}

/// A context with its id (DuckDB's `RegisteredLoggingContext`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct RegisteredContext {
    pub id: u64,
    pub context: LoggingContext,
}

impl RegisteredContext {
    /// The row of `duckdb_log_contexts()`.
    pub(crate) fn row(&self) -> Vec<Value> {
        let id = |value: Option<u64>| value.map_or_else(|| Value::null(LogicalType::UBIGINT), Value::ubigint);
        vec![
            Value::ubigint(self.id),
            Value::varchar(self.context.scope.name()),
            id(self.context.connection_id),
            id(self.context.transaction_id),
            id(self.context.query_id),
            Value::null(LogicalType::UBIGINT),
        ]
    }
}

/// One log entry.
#[derive(Clone, Debug)]
pub(crate) struct LogEntry {
    pub context_id: u64,
    /// Microseconds since the epoch.
    pub timestamp: i64,
    pub log_type: String,
    pub level: LogLevel,
    pub message: String,
}

impl LogEntry {
    /// The row of `duckdb_logs()`.
    pub(crate) fn row(&self) -> Vec<Value> {
        vec![
            Value::ubigint(self.context_id),
            Value::timestamp_tz(rustdb_types::Timestamp(self.timestamp)),
            Value::varchar(&self.log_type),
            Value::varchar(self.level.name()),
            Value::varchar(&self.message),
        ]
    }
}

/// Which storage implementation backs the log (DuckDB's `LogStorage` subclasses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StorageKind {
    /// `InMemoryLogStorage`: entries and contexts kept in memory, normalized.
    Memory,
    /// `StdOutLogStorage`: denormalized entries printed as they are flushed.
    Stdout,
    /// `FileLogStorage`: CSV files, normalized into a directory or denormalized into one
    /// `.csv` file.
    File,
}

/// A log table (DuckDB's `LoggingTargetTable`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LogTable {
    AllLogs,
    LogEntries,
    LogContexts,
}

impl LogTable {
    /// `LogStorage::GetColumnNames`.
    fn column_names(self) -> &'static [&'static str] {
        match self {
            Self::AllLogs => &[
                "context_id",
                "scope",
                "connection_id",
                "transaction_id",
                "query_id",
                "thread_id",
                "timestamp",
                "type",
                "log_level",
                "message",
            ],
            Self::LogEntries => &["context_id", "timestamp", "type", "log_level", "message"],
            Self::LogContexts => &[
                "context_id",
                "scope",
                "connection_id",
                "transaction_id",
                "query_id",
                "thread_id",
            ],
        }
    }

    /// The `columns` of the `read_csv_auto` reading a log file (`FileLogStorage::BindReplace`).
    fn csv_columns(self) -> &'static str {
        match self {
            Self::LogEntries => {
                "'context_id': 'UBIGINT', 'timestamp': 'TIMESTAMP WITH TIME ZONE', 'type': 'VARCHAR', 'log_level': \
                 'VARCHAR' , 'message': 'VARCHAR'"
            }
            Self::LogContexts => {
                "'context_id': 'UBIGINT', 'scope': 'VARCHAR', 'connection_id': 'UBIGINT', 'transaction_id': \
                 'UBIGINT', 'query_id': 'UBIGINT', 'thread_id': 'UBIGINT'"
            }
            Self::AllLogs => {
                "'context_id': 'UBIGINT', 'scope': 'VARCHAR', 'connection_id': 'UBIGINT', 'transaction_id': \
                 'UBIGINT', 'query_id': 'UBIGINT', 'thread_id': 'UBIGINT', 'timestamp': 'TIMESTAMP WITH TIME \
                 ZONE', 'type': 'VARCHAR', 'log_level': 'VARCHAR' , 'message': 'VARCHAR'"
            }
        }
    }
}

/// A CSV field as DuckDB's `CSVWriter` writes it: NULL is empty, the empty string and
/// values with a comma, quote, `#` or line break are quoted. The log storages keep the
/// writer options of the default dialect when the delimiter changes, so the comma
/// decides quoting whatever the delimiter (`CSVLogStorage::SetWriterConfigs`).
fn csv_field(value: &Value, out: &mut String) {
    if value.is_null() {
        return;
    }
    let text = value.to_varchar().unwrap_or_default();
    if text.is_empty() || text.contains([',', '"', '#', '\n', '\r']) {
        out.push('"');
        out.push_str(&text.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(&text);
    }
}

/// Whether a log path names a single CSV file (`StringUtil::EndsWith(path, ".csv")`).
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "DuckDB compares the extension case-sensitively"
)]
fn ends_with_csv(path: &str) -> bool {
    path.ends_with(".csv")
}

/// The files of a `FileLogStorage`.
#[derive(Debug, Default)]
struct LogFiles {
    base_path: String,
    /// The path of each table (`SetPaths`), empty if the table is not written.
    paths: Vec<(LogTable, String)>,
}

impl LogFiles {
    fn path(&self, table: LogTable) -> Option<&str> {
        self.paths
            .iter()
            .find(|(candidate, path)| *candidate == table && !path.is_empty())
            .map(|(_, path)| path.as_str())
    }

    /// `FileLogStorage::SetPaths`.
    fn set_paths(&mut self, base_path: &str, normalized: bool) {
        let join = |name: &str| {
            if base_path.ends_with('/') {
                format!("{base_path}{name}")
            } else {
                format!("{base_path}/{name}")
            }
        };
        base_path.clone_into(&mut self.base_path);
        self.paths = if normalized {
            vec![
                (LogTable::LogContexts, join("duckdb_log_contexts.csv")),
                (LogTable::LogEntries, join("duckdb_log_entries.csv")),
            ]
        } else if ends_with_csv(base_path) {
            vec![(LogTable::AllLogs, base_path.to_owned())]
        } else {
            vec![(LogTable::LogEntries, join("duckdb_log_entries.csv"))]
        };
    }

    /// Appends rows to the table's file, creating it (with a header) if needed
    /// (`InitializeFile` and the CSV writer's flush).
    fn append(&self, table: LogTable, rows: &[Vec<Value>], delimiter: &str) -> Result<()> {
        use std::io::Write as _;
        let path = self.path(table).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "Failed to initialize file log storage table, path wasn't set",
            )
        })?;
        if let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| Error::new(ErrorKind::Io, error.to_string()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| Error::new(ErrorKind::Io, format!("Cannot open file \"{path}\": {error}")))?;
        let empty = file.metadata().map_or(true, |metadata| metadata.len() == 0);
        let mut out = String::new();
        if empty {
            out.push_str(&table.column_names().join(delimiter));
            out.push('\n');
        }
        for row in rows {
            for (index, value) in row.iter().enumerate() {
                if index > 0 {
                    out.push_str(delimiter);
                }
                csv_field(value, &mut out);
            }
            out.push('\n');
        }
        file.write_all(out.as_bytes())
            .map_err(|error| Error::new(ErrorKind::Io, error.to_string()))
    }

    /// `FileLogStorage::Truncate`: the written files are emptied.
    fn truncate(&self) -> Result<()> {
        for (_, path) in &self.paths {
            if !path.is_empty() && std::path::Path::new(path).exists() {
                std::fs::write(path, "").map_err(|error| Error::new(ErrorKind::Io, error.to_string()))?;
            }
        }
        Ok(())
    }
}

/// A `BufferingLogStorage`: entries collect in a buffer that is flushed into the storage
/// when it fills or at query boundaries.
#[derive(Debug)]
struct BufferingStorage {
    kind: StorageKind,
    /// Whether contexts are kept apart from the entries (`normalize_contexts`).
    normalize: bool,
    buffer_limit: usize,
    /// `only_flush_on_full_buffer`: a test option that disables the query-boundary flushes.
    only_flush_on_full_buffer: bool,
    buffer_entries: Vec<(LogEntry, RegisteredContext)>,
    buffer_contexts: Vec<RegisteredContext>,
    registered: HashSet<u64>,
    flush_contexts_on_next_entry_flush: bool,
    entries: Vec<LogEntry>,
    contexts: Vec<RegisteredContext>,
    files: LogFiles,
    /// The CSV delimiter of the stdout and file storages (`delim`).
    delimiter: String,
}

impl BufferingStorage {
    fn new(kind: StorageKind) -> Self {
        Self {
            kind,
            normalize: kind != StorageKind::Stdout,
            buffer_limit: if kind == StorageKind::Stdout {
                1
            } else {
                STANDARD_VECTOR_SIZE
            },
            only_flush_on_full_buffer: false,
            buffer_entries: Vec::new(),
            buffer_contexts: Vec::new(),
            registered: HashSet::new(),
            flush_contexts_on_next_entry_flush: false,
            entries: Vec::new(),
            contexts: Vec::new(),
            files: LogFiles::default(),
            delimiter: if kind == StorageKind::Stdout { "\t" } else { "," }.to_owned(),
        }
    }

    /// `LogStorage::GetStorageName`.
    const fn name(&self) -> &'static str {
        match self.kind {
            StorageKind::Memory => "InMemoryLogStorage",
            StorageKind::Stdout => "StdOutLogStorage",
            StorageKind::File => "FileLogStorage",
        }
    }

    /// `BufferingLogStorage::WriteLogEntry`.
    fn write_entry(&mut self, entry: LogEntry, context: RegisteredContext) -> Result<()> {
        if self.registered.insert(context.id) && self.normalize {
            // `WriteLoggingContext`.
            if self.buffer_contexts.len() + 1 > self.buffer_limit.max(1) {
                self.flush_contexts()?;
            }
            self.buffer_contexts.push(context);
            self.flush_contexts_on_next_entry_flush = true;
        }
        self.buffer_entries.push((entry, context));
        if self.buffer_entries.len() >= self.buffer_limit {
            self.flush_entries()?;
            if self.normalize && self.flush_contexts_on_next_entry_flush {
                self.flush_contexts()?;
                self.flush_contexts_on_next_entry_flush = false;
            }
        }
        Ok(())
    }

    fn flush_entries(&mut self) -> Result<()> {
        let entries = std::mem::take(&mut self.buffer_entries);
        if entries.is_empty() {
            return Ok(());
        }
        let denormalized = |(entry, context): &(LogEntry, RegisteredContext)| {
            let mut row = context.row();
            row.extend(entry.row().into_iter().skip(1));
            row
        };
        match self.kind {
            StorageKind::Memory => self.entries.extend(entries.into_iter().map(|(entry, _)| entry)),
            StorageKind::Stdout => {
                let mut out = String::new();
                for row in entries.iter().map(denormalized) {
                    for (index, value) in row.iter().enumerate() {
                        if index > 0 {
                            out.push_str(&self.delimiter);
                        }
                        csv_field(value, &mut out);
                    }
                    out.push('\n');
                }
                print!("{out}");
            }
            StorageKind::File if self.normalize => {
                let rows: Vec<Vec<Value>> = entries.iter().map(|(entry, _)| entry.row()).collect();
                self.files.append(LogTable::LogEntries, &rows, &self.delimiter)?;
            }
            StorageKind::File => {
                let rows: Vec<Vec<Value>> = entries.iter().map(denormalized).collect();
                let table = if self.files.path(LogTable::AllLogs).is_some() {
                    LogTable::AllLogs
                } else {
                    LogTable::LogEntries
                };
                self.files.append(table, &rows, &self.delimiter)?;
            }
        }
        Ok(())
    }

    fn flush_contexts(&mut self) -> Result<()> {
        let contexts = std::mem::take(&mut self.buffer_contexts);
        match self.kind {
            StorageKind::Memory => self.contexts.extend(contexts),
            StorageKind::File if !contexts.is_empty() => {
                let rows: Vec<Vec<Value>> = contexts.iter().map(RegisteredContext::row).collect();
                self.files.append(LogTable::LogContexts, &rows, &self.delimiter)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// `FlushAll`: skipped with `only_flush_on_full_buffer`.
    fn flush_all(&mut self) -> Result<()> {
        if self.only_flush_on_full_buffer {
            return Ok(());
        }
        self.flush_entries()?;
        if self.normalize {
            self.flush_contexts()?;
        }
        Ok(())
    }

    /// `ResetAllBuffers` (also clears the in-memory log).
    fn reset(&mut self) {
        self.buffer_entries.clear();
        self.buffer_contexts.clear();
        self.entries.clear();
        self.contexts.clear();
    }

    /// `Truncate`.
    fn truncate(&mut self) -> Result<()> {
        self.reset();
        if self.kind == StorageKind::File {
            self.files.truncate()?;
        }
        Ok(())
    }

    /// `FileLogStorage::UpdateConfigInternal` and `BufferingLogStorage::UpdateConfig`.
    fn update_config(&mut self, config: &[(String, Value)]) -> Result<()> {
        let mut rest = Vec::new();
        if self.kind == StorageKind::File {
            let mut new_path = String::new();
            let mut normalize = self.normalize;
            let mut normalize_explicit = false;
            for (key, value) in config {
                match key.to_lowercase().as_str() {
                    "path" => {
                        new_path = value.to_varchar()?;
                        if !normalize_explicit && ends_with_csv(&new_path) {
                            normalize = false;
                        }
                    }
                    "normalize" => {
                        normalize_explicit = true;
                        normalize = value.as_bool().unwrap_or(true);
                    }
                    _ => rest.push((key.clone(), value.clone())),
                }
            }
            if ends_with_csv(&new_path) && normalize {
                return Err(Error::new(
                    ErrorKind::InvalidConfiguration,
                    format!(
                        "Can not set path to '{new_path}' while normalize is true. Normalize will make DuckDB write \
                         multiple log files to more efficiently store log entries. Please specify a directory path \
                         instead of a csv file path, or set normalize to false."
                    ),
                ));
            }
            if !self.files.base_path.is_empty() {
                self.flush_all()?;
            }
            if normalize != self.normalize {
                self.reset();
            }
            self.normalize = normalize;
            if !new_path.is_empty() && new_path != self.files.base_path {
                self.files.set_paths(&new_path, normalize);
            }
        } else {
            rest = config.to_vec();
        }
        for (key, value) in &rest {
            match key.to_lowercase().as_str() {
                "buffer_size" => {
                    let size = value.default_cast_as_ubigint()?;
                    self.buffer_limit = usize::try_from(size).unwrap_or(usize::MAX);
                    self.reset();
                }
                "only_flush_on_full_buffer" => {
                    self.only_flush_on_full_buffer = value.as_bool().unwrap_or(false);
                }
                // `CSVLogStorage::UpdateConfigInternal`.
                "delim" if self.kind != StorageKind::Memory => {
                    self.flush_all()?;
                    self.delimiter = value.to_varchar()?;
                }
                _ => {
                    return Err(Error::invalid_input(format!(
                        "Unrecognized log storage config option for storage: '{}': '{key}'",
                        self.name()
                    )));
                }
            }
        }
        Ok(())
    }

    /// `LogStorage::BindReplace` of `table`: for file storage the query reading it, after a
    /// flush; `None` reads the in-memory log.
    fn bind_replace(&mut self, table: LogTable) -> Result<Option<String>> {
        if self.kind != StorageKind::File {
            return Ok(None);
        }
        let enabled = if self.normalize {
            matches!(table, LogTable::LogEntries | LogTable::LogContexts)
        } else {
            table == LogTable::AllLogs
        };
        if !enabled {
            return Ok(None);
        }
        match table {
            LogTable::LogContexts => self.flush_contexts()?,
            _ => self.flush_entries()?,
        }
        let Some(path) = self.files.path(table) else {
            return Ok(None);
        };
        let select = if table == LogTable::AllLogs {
            "SELECT context_id, scope, connection_id, transaction_id, query_id, thread_id, timestamp, type, \
             log_level, message "
        } else {
            "SELECT *"
        };
        Ok(Some(format!(
            "{select} FROM read_csv_auto({}, columns={{{}}}, delim={})",
            rustdb_types::identifier::sql_identifier(path),
            table.csv_columns(),
            rustdb_types::identifier::write_quoted(&self.delimiter, '\'')
        )))
    }
}

/// `Value::GetValue<uint64_t>` for the storage options.
trait UbigintValue {
    fn default_cast_as_ubigint(&self) -> Result<u64>;
}

impl UbigintValue for Value {
    fn default_cast_as_ubigint(&self) -> Result<u64> {
        use rustdb_function::cast::ValueCast;
        let value = self.default_cast_as(&LogicalType::UBIGINT)?;
        value
            .as_u64()
            .ok_or_else(|| Error::invalid_input("Failed to cast value: buffer_size"))
    }
}

/// The default log types (`LogManager::RegisterDefaultLogTypes`).
fn default_log_types() -> Vec<LogTypeInfo> {
    let simple = |name: &str, level| LogTypeInfo {
        name: name.to_owned(),
        level,
        structured_type: None,
    };
    let structured = |name: &str, level, fields: Vec<(&str, LogicalType)>| LogTypeInfo {
        name: name.to_owned(),
        level,
        structured_type: Some(LogicalType::struct_type(
            fields
                .into_iter()
                .map(|(field, ty)| (field.to_owned(), ty))
                .collect(),
        )),
    };
    let headers = || LogicalType::map(LogicalType::VARCHAR, LogicalType::VARCHAR);
    vec![
        simple("", LogLevel::Info),
        structured(
            "FileSystem",
            LogLevel::Trace,
            vec![
                ("fs", LogicalType::VARCHAR),
                ("path", LogicalType::VARCHAR),
                ("op", LogicalType::VARCHAR),
                ("bytes", LogicalType::BIGINT),
                ("pos", LogicalType::BIGINT),
            ],
        ),
        structured(
            "HTTP",
            LogLevel::Debug,
            vec![
                (
                    "request",
                    LogicalType::struct_type(vec![
                        ("type".to_owned(), LogicalType::VARCHAR),
                        ("url".to_owned(), LogicalType::VARCHAR),
                        ("start_time".to_owned(), LogicalType::TIMESTAMP_TZ),
                        ("duration_ms".to_owned(), LogicalType::BIGINT),
                        ("request_body_length".to_owned(), LogicalType::UBIGINT),
                        ("headers".to_owned(), headers()),
                    ]),
                ),
                (
                    "response",
                    LogicalType::struct_type(vec![
                        ("status".to_owned(), LogicalType::VARCHAR),
                        ("reason".to_owned(), LogicalType::VARCHAR),
                        ("headers".to_owned(), headers()),
                    ]),
                ),
            ],
        ),
        simple("QueryLog", LogLevel::Info),
        structured(
            "PhysicalOperator",
            LogLevel::Debug,
            vec![
                ("operator_type", LogicalType::VARCHAR),
                ("parameters", headers()),
                ("class", LogicalType::VARCHAR),
                ("event", LogicalType::VARCHAR),
                ("info", headers()),
            ],
        ),
        structured(
            "Metrics",
            LogLevel::Info,
            vec![("metric", LogicalType::VARCHAR), ("value", LogicalType::VARCHAR)],
        ),
    ]
}

#[derive(Debug)]
struct ManagerState {
    config: LogConfig,
    next_context_id: u64,
    storage: BufferingStorage,
    log_types: Vec<LogTypeInfo>,
    /// The context of the database's global logger.
    global: RegisteredContext,
}

impl ManagerState {
    fn register(&mut self, context: LoggingContext) -> RegisteredContext {
        let registered = RegisteredContext {
            id: self.next_context_id,
            context,
        };
        self.next_context_id += 1;
        registered
    }

    fn lookup(&self, name: &str) -> Option<&LogTypeInfo> {
        self.log_types
            .iter()
            .find(|log_type| log_type.name.eq_ignore_ascii_case(name))
    }

    /// `SetLogStorageInternal`.
    fn set_storage(&mut self, name: &str) -> Result<()> {
        let lower = name.to_lowercase();
        if self.config.storage == lower {
            return Ok(());
        }
        let kind = match lower.as_str() {
            MEMORY_STORAGE => StorageKind::Memory,
            STDOUT_STORAGE => StorageKind::Stdout,
            FILE_STORAGE => StorageKind::File,
            _ => {
                return Err(Error::invalid_input(format!(
                    "Log storage '{name}' is not yet registered"
                )));
            }
        };
        self.storage.flush_all()?;
        self.storage = BufferingStorage::new(kind);
        self.config.storage = lower;
        Ok(())
    }
}

/// Whether `name` is one of the logging options, which configure the log manager.
pub(crate) fn is_logging_setting(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "enable_logging"
            | "logging_level"
            | "logging_mode"
            | "logging_storage"
            | "enabled_log_types"
            | "disabled_log_types"
    )
}

/// `StringUtil::Split(value, ",")`: empty parts dropped.
fn split_types(value: &Value) -> Result<Vec<String>> {
    let text = value.to_varchar()?;
    let mut types: Vec<String> = Vec::new();
    for part in text.split(',').filter(|part| !part.is_empty()) {
        if !types.iter().any(|existing| existing == part) {
            types.push(part.to_owned());
        }
    }
    Ok(types)
}

/// `SET`/`RESET` of a logging option (the `SetGlobal`/`ResetGlobal` of DuckDB's
/// `EnableLogging`, `LoggingLevel`, `LoggingMode`, `LoggingStorage`, `EnabledLogTypes`
/// and `DisabledLogTypes`).
pub(crate) fn apply_setting(manager: &LogManager, name: &str, value: Option<&Value>) -> Result<()> {
    use rustdb_function::cast::ValueCast;
    match (name.to_lowercase().as_str(), value) {
        ("enable_logging", Some(value)) => {
            let enabled = value
                .default_cast_as(&LogicalType::BOOLEAN)?
                .as_bool()
                .unwrap_or(false);
            manager.set_enabled(enabled);
        }
        ("enable_logging", None) => manager.set_enabled(false),
        ("logging_level", Some(value)) => manager.set_level(LogLevel::parse(&value.to_varchar()?)?),
        ("logging_level", None) => manager.set_level(LogLevel::Info),
        ("logging_mode", Some(value)) => manager.set_mode(LogMode::parse(&value.to_varchar()?)?),
        ("logging_mode", None) => manager.set_mode(LogMode::LevelOnly),
        ("logging_storage", Some(value)) => manager.set_storage(&value.to_varchar()?)?,
        ("logging_storage", None) => manager.set_storage(MEMORY_STORAGE)?,
        ("enabled_log_types", Some(value)) => manager.set_enabled_log_types(split_types(value)?),
        ("enabled_log_types", None) => manager.set_enabled_log_types(Vec::new()),
        ("disabled_log_types", Some(value)) => manager.set_disabled_log_types(split_types(value)?),
        ("disabled_log_types", None) => manager.set_disabled_log_types(Vec::new()),
        _ => {}
    }
    Ok(())
}

/// The current value of a logging option (their `GetSetting`); `enable_logging` reads as
/// an INTEGER, as DuckDB's `bool` converts to `Value(int32_t)`.
pub(crate) fn setting_value(config: &LogConfig, name: &str) -> Option<Value> {
    Some(match name {
        "enable_logging" => Value::integer(i32::from(config.enabled)),
        "logging_level" => Value::varchar(config.level.name()),
        "logging_mode" => Value::varchar(config.mode.name()),
        "logging_storage" => Value::varchar(&config.storage),
        "enabled_log_types" => Value::varchar(config.enabled_log_types.join(",")),
        "disabled_log_types" => Value::varchar(config.disabled_log_types.join(",")),
        _ => return None,
    })
}

/// The database's log manager (DuckDB's `LogManager`).
#[derive(Debug)]
pub(crate) struct LogManager {
    state: Mutex<ManagerState>,
}

fn now_micros() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
        i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
    })
}

impl Default for LogManager {
    /// `LogManager::Initialize`: the global logger gets the first context.
    fn default() -> Self {
        let mut state = ManagerState {
            config: LogConfig::default(),
            next_context_id: 0,
            storage: BufferingStorage::new(StorageKind::Memory),
            log_types: default_log_types(),
            global: RegisteredContext {
                id: 0,
                context: LoggingContext::new(ContextScope::Database),
            },
        };
        state.global = state.register(LoggingContext::new(ContextScope::Database));
        Self {
            state: Mutex::new(state),
        }
    }
}

impl LogManager {
    fn lock(&self) -> std::sync::MutexGuard<'_, ManagerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `LogManager::CreateLogger`: a context is registered whether or not logging is on.
    pub(crate) fn create_logger(&self, context: LoggingContext) -> Logger {
        let mut state = self.lock();
        let context = state.register(context);
        Logger {
            context,
            config: state.config.enabled.then(|| state.config.clone()),
        }
    }

    /// `LogManager::GetConfig`.
    pub(crate) fn config(&self) -> LogConfig {
        self.lock().config.clone()
    }

    /// `LogManager::WriteLogEntry`.
    fn write_entry(
        &self,
        context: RegisteredContext,
        log_type: &str,
        level: LogLevel,
        message: &str,
    ) -> Result<()> {
        let entry = LogEntry {
            context_id: context.id,
            timestamp: now_micros(),
            log_type: log_type.to_owned(),
            level,
            message: message.to_owned(),
        };
        self.lock().storage.write_entry(entry, context)
    }

    /// `LogManager::Flush`.
    pub(crate) fn flush(&self) -> Result<()> {
        self.lock().storage.flush_all()
    }

    /// `LogManager::TruncateLogStorage`.
    pub(crate) fn truncate(&self) -> Result<()> {
        self.lock().storage.truncate()
    }

    /// The query reading `table` from the storage, if it is not in memory.
    pub(crate) fn bind_replace(&self, table: LogTable) -> Result<Option<String>> {
        self.lock().storage.bind_replace(table)
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.lock().config.enabled = enabled;
    }

    pub(crate) fn set_mode(&self, mode: LogMode) {
        self.lock().config.mode = mode;
    }

    pub(crate) fn set_level(&self, level: LogLevel) {
        self.lock().config.level = level;
    }

    pub(crate) fn set_enabled_log_types(&self, types: Vec<String>) {
        self.lock().config.enabled_log_types = types;
    }

    pub(crate) fn set_disabled_log_types(&self, types: Vec<String>) {
        self.lock().config.disabled_log_types = types;
    }

    /// `LogManager::SetLogStorage` (`SET logging_storage`).
    pub(crate) fn set_storage(&self, name: &str) -> Result<()> {
        let mut state = self.lock();
        if name.eq_ignore_ascii_case(FILE_STORAGE) && state.config.storage != FILE_STORAGE {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "Cannot select 'file' log storage via 'SET logging_storage' because it requires a path. Use CALL \
                 enable_logging(storage='file', storage_path='...') instead.",
            ));
        }
        state.set_storage(name)
    }

    /// `LogManager::SetConfig`: the storage switches first.
    pub(crate) fn set_config(&self, config: LogConfig) -> Result<()> {
        let mut state = self.lock();
        state.set_storage(&config.storage)?;
        let storage = state.config.storage.clone();
        state.config = LogConfig { storage, ..config };
        Ok(())
    }

    /// `LogManager::SetEnableStructuredLoggers`: only the named types, at the lowest of
    /// their levels.
    pub(crate) fn set_enable_structured_loggers(&self, types: &[String]) -> Result<()> {
        let mut state = self.lock();
        let mut enabled = Vec::new();
        let mut level = LogLevel::Fatal;
        for name in types {
            let found = state
                .lookup(name)
                .ok_or_else(|| Error::invalid_input(format!("Unknown log type: '{name}'")))?;
            if !enabled.contains(&found.name) {
                enabled.push(found.name.clone());
            }
            level = level.min(found.level);
        }
        state.config.enabled_log_types = enabled;
        state.config.level = level;
        state.config.mode = LogMode::EnableSelected;
        state.config.enabled = true;
        Ok(())
    }

    /// `LogManager::UpdateLogStorageConfig`.
    pub(crate) fn update_storage_config(&self, config: &[(String, Value)]) -> Result<()> {
        self.lock().storage.update_config(config)
    }

    /// `LogManager::LookupLogType`.
    pub(crate) fn lookup_log_type(&self, name: &str) -> Option<LogTypeInfo> {
        self.lock().lookup(name).cloned()
    }

    /// The flushed entries (`InMemoryLogStorage::Scan` of `LOG_ENTRIES`).
    pub(crate) fn entries(&self) -> Vec<LogEntry> {
        self.lock().storage.entries.clone()
    }

    /// The flushed contexts (`LOG_CONTEXTS`).
    pub(crate) fn contexts(&self) -> Vec<RegisteredContext> {
        self.lock().storage.contexts.clone()
    }

    /// The global logger's `ShouldLog` (it follows the live configuration).
    fn global_should_log(&self, log_type: &str, level: LogLevel) -> bool {
        let state = self.lock();
        state.config.enabled && state.config.should_log(log_type, level)
    }

    fn global_context(&self) -> RegisteredContext {
        self.lock().global
    }
}

/// A logger with the configuration of when it was made (`ThreadSafeLogger`; `None` is a
/// `NopLogger`).
#[derive(Clone, Debug)]
pub(crate) struct Logger {
    context: RegisteredContext,
    config: Option<LogConfig>,
}

/// The loggers of a connection: its current per-query logger and the database's global
/// logger.
#[derive(Debug)]
pub(crate) struct ConnectionLogger {
    pub manager: Arc<LogManager>,
    current: Mutex<Logger>,
}

impl ConnectionLogger {
    pub(crate) fn new(manager: Arc<LogManager>, context: LoggingContext) -> Self {
        let logger = manager.create_logger(context);
        Self {
            manager,
            current: Mutex::new(logger),
        }
    }

    /// Replaces the connection's logger (at query boundaries).
    pub(crate) fn refresh(&self, context: LoggingContext) {
        let logger = self.manager.create_logger(context);
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = logger;
    }

    fn current(&self) -> Logger {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl ClientLogger for ConnectionLogger {
    fn should_log(&self, scope: LogScope, log_type: &str, level: LogLevel) -> bool {
        match scope {
            LogScope::Database => self.manager.global_should_log(log_type, level),
            LogScope::Connection => self
                .current()
                .config
                .is_some_and(|config| config.should_log(log_type, level)),
        }
    }

    fn write_log(&self, scope: LogScope, log_type: &str, level: LogLevel, message: &str) -> Result<()> {
        let context = match scope {
            LogScope::Database => self.manager.global_context(),
            LogScope::Connection => self.current().context,
        };
        self.manager.write_entry(context, log_type, level, message)
    }

    fn lookup_log_type(&self, name: &str) -> Option<LogTypeInfo> {
        self.manager.lookup_log_type(name)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
