//! The logging interface functions write through (DuckDB's `Logger` and `LogLevel`); the
//! log manager and its storages belong to the database.

use std::any::Any;
use std::fmt;

use roastedduck_common::{ByteString, Error, ErrorKind, Result};
use roastedduck_types::LogicalType;

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
    let message = roastedduck_common::string_util::candidates_message(
        &roastedduck_common::string_util::top_n_jaro_winkler(&candidates, value),
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
    fn write_log_bytes(&self, scope: LogScope, log_type: &str, level: LogLevel, message: &[u8])
    -> Result<()>;
    /// UTF-8 input forwarded to the native byte logger.
    fn write_log(&self, scope: LogScope, log_type: &str, level: LogLevel, message: &str) -> Result<()> {
        self.write_log_bytes(scope, log_type, level, message.as_bytes())
    }
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
    parameters: Vec<(ByteString, ByteString)>,
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
        parameters: impl FnOnce() -> Vec<(ByteString, ByteString)>,
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
    /// log is propagated.
    pub fn log(&self, class: &str, event: &str, info: &[(&str, String)]) -> Result<()> {
        let message = operator_log_message(&self.operator_type, &self.parameters, class, event, info)?;
        self.logger
            .write_log_bytes(LogScope::Connection, Self::NAME, Self::LEVEL, &message)
    }
    /// Native info values, validated at the same VARCHAR construction as C++ logging.
    pub fn log_bytes(&self, class: &str, event: &str, info: &[(&str, ByteString)]) -> Result<()> {
        let message = operator_log_message_bytes(&self.operator_type, &self.parameters, class, event, info)?;
        self.logger
            .write_log_bytes(LogScope::Connection, Self::NAME, Self::LEVEL, &message)
    }
}

/// `PhysicalOperatorLogType::ConstructLogMessage`: the STRUCT of the operator's type and
/// parameters, the class and event logged and their info, as text.
pub fn operator_log_message(
    operator_type: &str,
    parameters: &[(ByteString, ByteString)],
    class: &str,
    event: &str,
    info: &[(&str, String)],
) -> Result<ByteString> {
    let native: Vec<_> = info
        .iter()
        .map(|(key, value)| (*key, ByteString::from(value)))
        .collect();
    operator_log_message_bytes(operator_type, parameters, class, event, &native)
}

pub fn operator_log_message_bytes(
    operator_type: &str,
    parameters: &[(ByteString, ByteString)],
    class: &str,
    event: &str,
    info: &[(&str, ByteString)],
) -> Result<ByteString> {
    use roastedduck_types::Value;
    // `StringPairIterableToMap`.
    let map = |entries: Vec<(Value, Value)>| Value::map(LogicalType::VARCHAR, LogicalType::VARCHAR, entries);
    let message = Value::struct_value(vec![
        ("operator_type".into(), Value::varchar(operator_type)),
        (
            "parameters".into(),
            map(parameters
                .iter()
                .map(|(key, value)| Ok((Value::try_varchar(key)?, Value::try_varchar(value)?)))
                .collect::<Result<Vec<_>>>()?),
        ),
        ("class".into(), Value::varchar(class)),
        ("event".into(), Value::varchar(event)),
        (
            "info".into(),
            map(info
                .iter()
                .map(|(key, value)| Ok((Value::varchar(*key), Value::try_varchar(value)?)))
                .collect::<Result<_>>()?),
        ),
    ]);
    message.to_varchar_bytes()
}

/// The `FileSystem` log entries of a local file (DuckDB's `FileSystemLogType` written by
/// the `DUCKDB_LOG_FILE_SYSTEM_*` macros of a file handle).
#[derive(Debug)]
pub struct FileSystemLogger {
    logger: std::sync::Arc<dyn ClientLogger>,
    path: ByteString,
}

impl FileSystemLogger {
    /// The logger of a file opened now, if the thread runs a statement; logs `OPEN`.
    pub fn open(path: impl AsRef<[u8]>) -> Option<Self> {
        Self::open_for(crate::file_system::opener().as_deref(), path)
    }

    /// The logger of a file opened now for the connection `client` (which may run the
    /// statement on another thread); logs `OPEN`.
    pub fn open_for(client: Option<&crate::FunctionClient>, path: impl AsRef<[u8]>) -> Option<Self> {
        let logger = Self {
            logger: client?.logger()?,
            path: path.as_ref().into(),
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
            Some((bytes, position)) => roastedduck_common::byte_format!(
                "{{\"fs\":\"LocalFileSystem\",\"path\":\"{}\",\"op\":\"{op}\",\"bytes\":\"{bytes}\",\"pos\":\"{position}\"}}",
                self.path
            ),
            None => roastedduck_common::byte_format!(
                "{{\"fs\":\"LocalFileSystem\",\"path\":\"{}\",\"op\":\"{op}\"}}",
                self.path
            ),
        };
        let _ = self
            .logger
            .write_log_bytes(LogScope::Connection, "FileSystem", LogLevel::Trace, &message);
    }
}
