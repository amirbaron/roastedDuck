//! The logging interface functions write through (DuckDB's `Logger` and `LogLevel`); the
//! log manager and its storages belong to the database.

use std::any::Any;
use std::fmt;

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_types::LogicalType;

/// The severity of a log entry (DuckDB's `LogLevel`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    Trace = 10,
    Debug = 20,
    Info = 30,
    Warning = 40,
    Error = 50,
    Fatal = 60,
}

impl LogLevel {
    const ALL: [Self; 6] = [
        Self::Trace,
        Self::Debug,
        Self::Info,
        Self::Warning,
        Self::Error,
        Self::Fatal,
    ];

    /// `EnumUtil::ToString(LogLevel)`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
            Self::Fatal => "FATAL",
        }
    }

    /// `EnumUtil::FromString<LogLevel>`: case-insensitive, with DuckDB's error.
    pub fn parse(text: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|level| level.name().eq_ignore_ascii_case(text))
            .ok_or_else(|| enum_error(text, "LogLevel", &Self::ALL.map(Self::name)))
    }
}

/// `StringUtil::StringToEnum`'s error for an unknown enum value.
pub fn enum_error(value: &str, name: &str, candidates: &[&str]) -> Error {
    let candidates: Vec<String> = candidates.iter().map(|&c| c.to_owned()).collect();
    let message = rustdb_common::string_util::candidates_message(
        &rustdb_common::string_util::top_n_jaro_winkler(&candidates, value),
        "Candidates",
    );
    Error::new(
        ErrorKind::NotImplemented,
        format!("Enum value: unrecognized value \"{value}\" for enum \"{name}\"\n{message}"),
    )
}

/// Which logger a message goes to (`write_log`'s `scope`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogScope {
    /// The database's global logger.
    Database,
    /// The connection's logger of the current query.
    Connection,
}

/// A registered log type (DuckDB's `LogType`): its name, level and, for structured
/// types, the type its messages parse to.
#[derive(Clone, Debug)]
pub struct LogTypeInfo {
    pub name: String,
    pub level: LogLevel,
    pub structured_type: Option<LogicalType>,
}

/// The loggers of a connection (DuckDB's `Logger::Get(ClientContext)` and
/// `Logger::Get(DatabaseInstance)`).
pub trait ClientLogger: Send + Sync + fmt::Debug {
    /// `Logger::ShouldLog`.
    fn should_log(&self, scope: LogScope, log_type: &str, level: LogLevel) -> bool;
    /// `Logger::WriteLog`.
    fn write_log(&self, scope: LogScope, log_type: &str, level: LogLevel, message: &str) -> Result<()>;
    /// `LogManager::LookupLogType`.
    fn lookup_log_type(&self, name: &str) -> Option<LogTypeInfo>;
    /// The database's log manager, for its own functions.
    fn as_any(&self) -> &dyn Any;
}

/// The `PhysicalOperator` log entries of an operator (DuckDB's `PhysicalOperatorLogType`
/// written by `DUCKDB_LOG(context, PhysicalOperatorLogType, op, ...)`): the operator's
/// type and its `ParamsToString()`, captured when the operator starts and only if the
/// query logs the type, so operators that do not log pay nothing.
#[derive(Clone)]
pub struct OperatorLogger {
    logger: std::sync::Arc<dyn ClientLogger>,
    operator_type: String,
    parameters: Vec<(String, String)>,
}

impl fmt::Debug for OperatorLogger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperatorLogger")
            .field("operator_type", &self.operator_type)
            .finish_non_exhaustive()
    }
}

impl OperatorLogger {
    /// `PhysicalOperatorLogType::NAME`.
    pub const NAME: &'static str = "PhysicalOperator";
    /// `PhysicalOperatorLogType::LEVEL`.
    pub const LEVEL: LogLevel = LogLevel::Debug;

    /// The logger of an operator of type `operator_type` (`EnumUtil::ToString` of its
    /// `PhysicalOperatorType`), if `logger` writes `PhysicalOperator` entries (the
    /// `ShouldLog` of `DUCKDB_LOG`); `parameters` (its `ParamsToString()`) is only
    /// computed then.
    pub fn new(
        logger: Option<std::sync::Arc<dyn ClientLogger>>,
        operator_type: &str,
        parameters: impl FnOnce() -> Vec<(String, String)>,
    ) -> Option<Self> {
        let logger = logger?;
        if !logger.should_log(LogScope::Connection, Self::NAME, Self::LEVEL) {
            return None;
        }
        Some(Self {
            logger,
            operator_type: operator_type.to_owned(),
            parameters: parameters(),
        })
    }

    /// `DUCKDB_LOG(context, PhysicalOperatorLogType, op, class, event, info)`; a failure to
    /// log is ignored.
    pub fn log(&self, class: &str, event: &str, info: &[(&str, String)]) {
        let message = operator_log_message(&self.operator_type, &self.parameters, class, event, info);
        let _ = self
            .logger
            .write_log(LogScope::Connection, Self::NAME, Self::LEVEL, &message);
    }
}

/// `PhysicalOperatorLogType::ConstructLogMessage`: the STRUCT of the operator's type and
/// parameters, the class and event logged and their info, as text.
pub fn operator_log_message(
    operator_type: &str,
    parameters: &[(String, String)],
    class: &str,
    event: &str,
    info: &[(&str, String)],
) -> String {
    use rustdb_types::Value;
    // `StringPairIterableToMap`.
    let map = |entries: Vec<(Value, Value)>| Value::map(LogicalType::VARCHAR, LogicalType::VARCHAR, entries);
    let message = Value::struct_value(vec![
        ("operator_type".to_owned(), Value::varchar(operator_type)),
        (
            "parameters".to_owned(),
            map(parameters
                .iter()
                .map(|(key, value)| (Value::varchar(key), Value::varchar(value)))
                .collect()),
        ),
        ("class".to_owned(), Value::varchar(class)),
        ("event".to_owned(), Value::varchar(event)),
        (
            "info".to_owned(),
            map(info
                .iter()
                .map(|(key, value)| (Value::varchar(*key), Value::varchar(value)))
                .collect()),
        ),
    ]);
    message.to_varchar().unwrap_or_default()
}

/// The `FileSystem` log entries of a local file (DuckDB's `FileSystemLogType` written by
/// the `DUCKDB_LOG_FILE_SYSTEM_*` macros of a file handle).
#[derive(Debug)]
pub struct FileSystemLogger {
    logger: std::sync::Arc<dyn ClientLogger>,
    path: String,
}

impl FileSystemLogger {
    /// The logger of a file opened now, if the thread runs a statement; logs `OPEN`.
    pub fn open(path: &str) -> Option<Self> {
        Self::open_for(crate::file_system::opener().as_deref(), path)
    }

    /// The logger of a file opened now for the connection `client` (which may run the
    /// statement on another thread); logs `OPEN`.
    pub fn open_for(client: Option<&crate::FunctionClient>, path: &str) -> Option<Self> {
        let logger = Self {
            logger: client?.logger()?,
            path: path.to_owned(),
        };
        logger.log("OPEN", None);
        Some(logger)
    }

    /// `DUCKDB_LOG_FILE_SYSTEM_READ`: `bytes` read at `position`.
    pub fn read(&self, bytes: u64, position: u64) {
        self.log("READ", Some((bytes, position)));
    }

    /// `DUCKDB_LOG_FILE_SYSTEM_WRITE`: `bytes` written at `position`.
    pub fn write(&self, bytes: u64, position: u64) {
        self.log("WRITE", Some((bytes, position)));
    }

    /// `DUCKDB_LOG_FILE_SYSTEM_CLOSE`.
    pub fn close(&self) {
        self.log("CLOSE", None);
    }

    /// `FileSystemLogType::ConstructLogMessage`.
    fn log(&self, op: &str, bytes: Option<(u64, u64)>) {
        if !self
            .logger
            .should_log(LogScope::Connection, "FileSystem", LogLevel::Trace)
        {
            return;
        }
        let message = match bytes {
            Some((bytes, position)) => format!(
                "{{\"fs\":\"LocalFileSystem\",\"path\":\"{}\",\"op\":\"{op}\",\"bytes\":\"{bytes}\",\"pos\":\"{position}\"}}",
                self.path
            ),
            None => format!(
                "{{\"fs\":\"LocalFileSystem\",\"path\":\"{}\",\"op\":\"{op}\"}}",
                self.path
            ),
        };
        let _ = self
            .logger
            .write_log(LogScope::Connection, "FileSystem", LogLevel::Trace, &message);
    }
}
