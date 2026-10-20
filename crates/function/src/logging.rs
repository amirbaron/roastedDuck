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

thread_local! {
    /// The loggers of the connection whose statement this thread runs.
    static ACTIVE_LOGGER: std::cell::RefCell<Option<std::sync::Arc<dyn ClientLogger>>> =
        const { std::cell::RefCell::new(None) };
}

/// Restores the thread's previous loggers when dropped.
#[derive(Debug)]
pub struct ActiveLoggerGuard {
    previous: Option<Option<std::sync::Arc<dyn ClientLogger>>>,
}

impl Drop for ActiveLoggerGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            ACTIVE_LOGGER.with(|active| *active.borrow_mut() = previous);
        }
    }
}

/// Makes `logger` the loggers file handles opened on this thread log to, until the guard
/// is dropped (DuckDB passes the `ClientContext` as the file opener).
pub fn activate_logger(logger: std::sync::Arc<dyn ClientLogger>) -> ActiveLoggerGuard {
    ActiveLoggerGuard {
        previous: Some(ACTIVE_LOGGER.with(|active| active.borrow_mut().replace(logger))),
    }
}

/// The loggers of the connection whose statement this thread runs.
pub fn active_logger() -> Option<std::sync::Arc<dyn ClientLogger>> {
    ACTIVE_LOGGER.with(|active| active.borrow().clone())
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
        let logger = Self {
            logger: active_logger()?,
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
