use std::borrow::Cow;
use std::fmt;

/// Error categories, numbered exactly like DuckDB's `ExceptionType`.
///
/// The discriminants are part of the C API (`duckdb_error_type`) and must not change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ErrorKind {
    Invalid = 0,
    OutOfRange = 1,
    Conversion = 2,
    UnknownType = 3,
    Decimal = 4,
    MismatchType = 5,
    DivideByZero = 6,
    ObjectSize = 7,
    InvalidType = 8,
    Serialization = 9,
    Transaction = 10,
    NotImplemented = 11,
    Expression = 12,
    Catalog = 13,
    Parser = 14,
    Planner = 15,
    Scheduler = 16,
    Executor = 17,
    Constraint = 18,
    Index = 19,
    Stat = 20,
    Connection = 21,
    Syntax = 22,
    Settings = 23,
    Binder = 24,
    Network = 25,
    Optimizer = 26,
    NullPointer = 27,
    Io = 28,
    Interrupt = 29,
    Fatal = 30,
    Internal = 31,
    InvalidInput = 32,
    OutOfMemory = 33,
    Permission = 34,
    ParameterNotResolved = 35,
    ParameterNotAllowed = 36,
    Dependency = 37,
    Http = 38,
    MissingExtension = 39,
    Autoload = 40,
    Sequence = 41,
    InvalidConfiguration = 42,
}

impl ErrorKind {
    /// The name DuckDB prints in front of `Error:` (`Exception::ExceptionTypeToString`).
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Invalid => "Invalid",
            Self::OutOfRange => "Out of Range",
            Self::Conversion => "Conversion",
            Self::UnknownType => "Unknown Type",
            Self::Decimal => "Decimal",
            Self::MismatchType => "Mismatch Type",
            Self::DivideByZero => "Divide by Zero",
            Self::ObjectSize => "Object Size",
            Self::InvalidType => "Invalid type",
            Self::Serialization => "Serialization",
            Self::Transaction => "TransactionContext",
            Self::NotImplemented => "Not implemented",
            Self::Expression => "Expression",
            Self::Catalog => "Catalog",
            Self::Parser => "Parser",
            Self::Planner => "Planner",
            Self::Scheduler => "Scheduler",
            Self::Executor => "Executor",
            Self::Constraint => "Constraint",
            Self::Index => "Index",
            Self::Stat => "Stat",
            Self::Connection => "Connection",
            Self::Syntax => "Syntax",
            Self::Settings => "Settings",
            Self::Binder => "Binder",
            Self::Network => "Network",
            Self::Optimizer => "Optimizer",
            Self::NullPointer => "NullPointer",
            Self::Io => "IO",
            Self::Interrupt => "INTERRUPT",
            Self::Fatal => "FATAL",
            Self::Internal => "INTERNAL",
            Self::InvalidInput => "Invalid Input",
            Self::OutOfMemory => "Out of Memory",
            Self::Permission => "Permission",
            Self::ParameterNotResolved => "Parameter Not Resolved",
            Self::ParameterNotAllowed => "Parameter Not Allowed",
            Self::Dependency => "Dependency",
            Self::Http => "HTTP",
            Self::MissingExtension => "Missing Extension",
            Self::Autoload => "Extension Autoloading",
            Self::Sequence => "Sequence",
            Self::InvalidConfiguration => "Invalid Configuration",
        }
    }
}

/// An engine error: a category plus DuckDB's raw message text.
///
/// `Display` renders DuckDB's final message (`ErrorData::ConstructFinalMessage`), which is
/// what clients and the sqllogictest corpus compare against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: Cow<'static, str>,
    /// Byte offset in the query the error refers to (DuckDB's `position` extra info);
    /// rendered as `LINE n:` context once the query text is attached.
    position: Option<u64>,
    /// DuckDB's `error_subtype` extra info (`"SYNTAX_ERROR"` for parse failures).
    subtype: Option<&'static str>,
    /// Whether the message is the error as a JSON object (`errors_as_json`), which
    /// clients see without the `"<Kind> Error: "` prefix.
    json: bool,
}

/// Engine-wide result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

const INTERNAL_ERROR_SUFFIX: &str = "\nThis error signals an assertion failure within DuckDB. This usually occurs due to \
unexpected conditions or errors in the program's logic.\nFor more information, see \
https://duckdb.org/docs/stable/dev/internal_errors";

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            kind,
            message: message.into(),
            position: None,
            subtype: None,
            json: false,
        }
    }

    /// Tags the error with DuckDB's `error_subtype` extra info (such as `"SYNTAX_ERROR"`).
    #[must_use]
    pub const fn with_subtype(mut self, subtype: &'static str) -> Self {
        self.subtype = Some(subtype);
        self
    }

    /// The `error_subtype` extra info, if any.
    pub const fn subtype(&self) -> Option<&'static str> {
        self.subtype
    }

    /// Attaches a query position, unless one is already set.
    #[must_use]
    pub fn with_position(mut self, position: Option<u64>) -> Self {
        if self.position.is_none() {
            self.position = position;
        }
        self
    }

    /// Replaces the query position with `position`, if any (DuckDB's
    /// `ErrorData::AddQueryLocation`, which overwrites the `position` extra info).
    #[must_use]
    pub fn with_position_override(mut self, position: Option<u64>) -> Self {
        if position.is_some() {
            self.position = position;
        }
        self
    }

    pub const fn position(&self) -> Option<u64> {
        self.position
    }

    /// Sets the query position (used by the parser for syntax errors).
    #[must_use]
    pub fn with_query_location(mut self, location: usize) -> Self {
        self.position = Some(location as u64);
        self
    }

    /// The query position the error refers to, if known.
    pub fn query_location(&self) -> Option<usize> {
        self.position.and_then(|position| usize::try_from(position).ok())
    }

    /// Renders the position as `LINE n: ...` context within `query`
    /// (DuckDB's `ErrorData::AddErrorLocation`).
    #[must_use]
    pub fn with_query_context(mut self, query: &str) -> Self {
        if let Some(position) = self.position.take() {
            self.message = crate::query_context::format(query, &self.message, position).into();
        }
        self
    }

    /// The error as a JSON object of its type, message and extra info, which becomes
    /// its whole message (`ErrorData::ConvertErrorToJSON` under `errors_as_json`).
    #[must_use]
    pub fn into_json(mut self) -> Self {
        if self.json || self.message.starts_with('{') {
            return self;
        }
        let mut json = String::from("{");
        let mut field = |key: &str, value: &str| {
            if json.len() > 1 {
                json.push(',');
            }
            json.push_str(&crate::string_util::json_quote(key));
            json.push(':');
            json.push_str(&crate::string_util::json_quote(value));
        };
        field("exception_type", self.kind.display_name());
        field("exception_message", &self.message);
        if let Some(subtype) = self.subtype {
            field("error_subtype", subtype);
        }
        if let Some(position) = self.position.take() {
            field("position", &position.to_string());
        }
        json.push('}');
        self.message = json.into();
        self.json = true;
        self
    }

    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The message without the `"<Kind> Error: "` prefix.
    pub fn raw_message(&self) -> &str {
        &self.message
    }

    pub fn parser(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(ErrorKind::Parser, message)
    }

    pub fn binder(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(ErrorKind::Binder, message)
    }

    pub fn catalog(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(ErrorKind::Catalog, message)
    }

    pub fn conversion(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(ErrorKind::Conversion, message)
    }

    pub fn out_of_range(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(ErrorKind::OutOfRange, message)
    }

    pub fn invalid_input(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(ErrorKind::InvalidInput, message)
    }

    pub fn internal(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    /// DuckDB's `SerializationException`.
    pub fn serialization(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(ErrorKind::Serialization, message)
    }

    /// A feature that the reference engine supports but this port does not (yet).
    pub fn not_implemented(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(ErrorKind::NotImplemented, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.json {
            return f.write_str(&self.message);
        }
        if self.kind != ErrorKind::UnknownType {
            write!(f, "{} ", self.kind.display_name())?;
        }
        write!(f, "Error: {}", self.message.replace('\0', "\\0"))?;
        if self.kind == ErrorKind::Internal {
            f.write_str(INTERNAL_ERROR_SUFFIX)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

/// The message of an operating system error as C's `strerror` gives it (Rust's
/// `io::Error` adds ` (os error N)`).
pub fn os_error_text(error: &std::io::Error) -> String {
    let text = error.to_string();
    match error.raw_os_error() {
        Some(code) => text
            .strip_suffix(&format!(" (os error {code})"))
            .map_or_else(|| text.clone(), str::to_owned),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_message_matches_duckdb() {
        let err = Error::parser("syntax error at or near \"=\"");
        assert_eq!(err.to_string(), "Parser Error: syntax error at or near \"=\"");
        let err = Error::new(ErrorKind::UnknownType, "x");
        assert_eq!(err.to_string(), "Error: x");
        assert!(
            Error::internal("boom")
                .to_string()
                .starts_with("INTERNAL Error: boom\nThis error")
        );
    }
}
