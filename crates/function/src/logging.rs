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
