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
    /// DuckDB's extra info: `error_subtype`, the query `position` (rendered as `LINE n:`
    /// context once the query text is attached), the `name` and `type` of a missing
    /// entry, `candidates`, ...
    extra_info: ExtraInfo,
    /// Whether the message is the error as a JSON object (`errors_as_json`), which
    /// clients see without the `"<Kind> Error: "` prefix.
    json: bool,
}

/// DuckDB's exception extra info, an `unordered_map<string, string>` whose iteration
/// order the JSON of `errors_as_json` follows. That order is libc++'s: it depends on the
/// order the keys were set in and on how often the map was rebuilt, as DuckDB rebuilds it
/// each time an exception is caught into an `ErrorData` (which parses the exception's
/// JSON into a fresh map). So this keeps that history: the keys in the order DuckDB sets
/// them, and the round trips in between.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtraInfo {
    steps: Vec<ExtraInfoStep>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ExtraInfoStep {
    /// The key is set (`extra_info[key] = value`); a key whose value is not known yet
    /// holds its place for a value DuckDB has at that point (`None` until then).
    Entry {
        key: &'static str,
        value: Option<String>,
    },
    /// The exception is caught into an `ErrorData` (`ErrorData(const std::exception &)`).
    RoundTrip,
}

impl ExtraInfo {
    fn get(&self, key: &str) -> Option<&str> {
        self.steps.iter().find_map(|step| match step {
            ExtraInfoStep::Entry {
                key: entry,
                value: Some(value),
            } if *entry == key => Some(value.as_str()),
            _ => None,
        })
    }

    /// Sets `key`, in its place if it has one.
    fn set(&mut self, key: &'static str, value: Option<String>) {
        for step in &mut self.steps {
            if let ExtraInfoStep::Entry {
                key: entry,
                value: slot,
            } = step
                && *entry == key
            {
                *slot = value;
                return;
            }
        }
        self.steps.push(ExtraInfoStep::Entry { key, value });
    }

    /// The entries in the iteration order of DuckDB's map after the history, and one more
    /// round trip: the client catching the error.
    fn iteration_order(&self) -> Vec<(&'static str, &str)> {
        let mut order: Vec<(&'static str, &str)> = Vec::new();
        let mut table = crate::libcxx::UnorderedOrder::default();
        for step in self
            .steps
            .iter()
            .chain(std::iter::once(&ExtraInfoStep::RoundTrip))
        {
            match step {
                ExtraInfoStep::Entry {
                    key,
                    value: Some(value),
                } => {
                    let (inserted, index) =
                        table.insert(crate::libcxx::string_hash(key.as_bytes()), *key, |a, b| a == b);
                    if inserted {
                        order.push((key, value));
                    } else {
                        order[index].1 = value;
                    }
                }
                ExtraInfoStep::Entry { value: None, .. } => {}
                ExtraInfoStep::RoundTrip => {
                    let entries: Vec<(&'static str, &str)> =
                        table.order().into_iter().map(|index| order[index]).collect();
                    (table, order) = round_trip(&entries);
                }
            }
        }
        table.order().into_iter().map(|index| order[index]).collect()
    }
}

/// Rebuilds a map of `entries` (in iteration order) as `ErrorData` does from the
/// exception's JSON: the JSON object, with `exception_type` and `exception_message`
/// first, is parsed into a map (`ComplexJSON`), that one is flattened into another, whose
/// entries but the type and message make the new extra info.
fn round_trip<'a>(
    entries: &[(&'static str, &'a str)],
) -> (
    crate::libcxx::UnorderedOrder<&'static str>,
    Vec<(&'static str, &'a str)>,
) {
    const TYPE_AND_MESSAGE: [(&str, &str); 2] = [("exception_type", ""), ("exception_message", "")];
    let rebuild = |entries: &[(&'static str, &'a str)]| {
        let mut table = crate::libcxx::UnorderedOrder::default();
        for (key, _) in entries {
            table.insert(crate::libcxx::string_hash(key.as_bytes()), *key, |a, b| a == b);
        }
        let ordered: Vec<(&'static str, &'a str)> =
            table.order().into_iter().map(|index| entries[index]).collect();
        ordered
    };
    let json: Vec<(&'static str, &'a str)> = TYPE_AND_MESSAGE
        .iter()
        .copied()
        .chain(entries.iter().copied())
        .collect();
    let flattened = rebuild(&rebuild(&json));
    let mut table = crate::libcxx::UnorderedOrder::default();
    let mut order = Vec::with_capacity(entries.len());
    for entry in flattened {
        if TYPE_AND_MESSAGE.iter().any(|(key, _)| *key == entry.0) {
            continue;
        }
        table.insert(crate::libcxx::string_hash(entry.0.as_bytes()), entry.0, |a, b| {
            a == b
        });
        order.push(entry);
    }
    (table, order)
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
            extra_info: ExtraInfo::default(),
            json: false,
        }
    }

    /// Tags the error with DuckDB's `error_subtype` extra info (such as `"SYNTAX_ERROR"`).
    #[must_use]
    pub fn with_subtype(self, subtype: &'static str) -> Self {
        self.with_extra_info("error_subtype", subtype)
    }

    /// The `error_subtype` extra info, if any.
    pub fn subtype(&self) -> Option<&str> {
        self.extra_info.get("error_subtype")
    }

    /// Sets extra info `key` (`extra_info[key] = value`).
    #[must_use]
    pub fn with_extra_info(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.extra_info.set(key, Some(value.into()));
        self
    }

    /// Holds the place of extra info `key` that DuckDB sets at this point, when the value
    /// is attached later (the `position` of a catalog lookup).
    #[must_use]
    pub fn with_extra_info_slot(mut self, key: &'static str) -> Self {
        self.extra_info.set(key, None);
        self
    }

    /// Marks where DuckDB catches the exception into an `ErrorData` and throws it again,
    /// which rebuilds the extra info in a new order. A held place keeps its place: the
    /// value DuckDB had there is attached later.
    #[must_use]
    pub fn with_extra_info_round_trip(mut self) -> Self {
        self.extra_info.steps.push(ExtraInfoStep::RoundTrip);
        self
    }

    /// The error caught into an `ErrorData` to be thrown again with more context, as a
    /// binder's `catch` that adds the location of a reference (`AddQueryLocation`): DuckDB's
    /// exception had no value for a held place, so a value attached from now on comes
    /// after the round trip.
    #[must_use]
    pub fn caught(mut self) -> Self {
        self.extra_info
            .steps
            .retain(|step| !matches!(step, ExtraInfoStep::Entry { value: None, .. }));
        self.extra_info.steps.push(ExtraInfoStep::RoundTrip);
        self
    }

    /// Attaches a query position, unless one is already set.
    #[must_use]
    pub fn with_position(self, position: Option<u64>) -> Self {
        match position {
            Some(position) if self.position().is_none() => {
                self.with_extra_info("position", position.to_string())
            }
            _ => self,
        }
    }

    /// Replaces the query position with `position`, if any (DuckDB's
    /// `ErrorData::AddQueryLocation`, which overwrites the `position` extra info).
    #[must_use]
    pub fn with_position_override(self, position: Option<u64>) -> Self {
        match position {
            Some(position) => self.with_extra_info("position", position.to_string()),
            None => self,
        }
    }

    pub fn position(&self) -> Option<u64> {
        self.extra_info
            .get("position")
            .and_then(|position| position.parse().ok())
    }

    /// Sets the query position (used by the parser for syntax errors).
    #[must_use]
    pub fn with_query_location(self, location: usize) -> Self {
        self.with_extra_info("position", location.to_string())
    }

    /// The query position the error refers to, if known.
    pub fn query_location(&self) -> Option<usize> {
        self.position()
            .and_then(|position| usize::try_from(position).ok())
    }

    /// Renders the position as `LINE n: ...` context within `query`
    /// (DuckDB's `ErrorData::AddErrorLocation`, which erases the `position`).
    #[must_use]
    pub fn with_query_context(mut self, query: &str) -> Self {
        if let Some(position) = self.position() {
            self.message = crate::query_context::format(query, &self.message, position).into();
            self.extra_info.set("position", None);
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
        for (key, value) in self.extra_info.iteration_order() {
            field(key, value);
        }
        json.push('}');
        self.message = json.into();
        self.extra_info.set("position", None);
        self.json = true;
        self
    }

    /// The error as DuckDB throws a processed `ErrorData` again (`ErrorData::Throw`): a
    /// JSON message (`errors_as_json`) becomes the message of an error of its type, which
    /// prints with the type's prefix.
    #[must_use]
    pub fn rethrown(mut self) -> Self {
        self.json = false;
        self
    }

    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The extra info (`error_subtype`, `position`, ...) in the iteration order of the
    /// map of an `ErrorData` that caught this error (`ErrorData::ExtraInfo`).
    pub fn extra_info(&self) -> Vec<(&'static str, &str)> {
        self.extra_info.iteration_order()
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

    fn json_keys(error: Error) -> Vec<String> {
        let json = error.into_json();
        let text = json.raw_message();
        let mut keys = Vec::new();
        for part in text.split("\",\"") {
            if let Some((key, _)) = part.trim_start_matches("{\"").split_once("\":\"") {
                keys.push(key.to_owned());
            }
        }
        keys.retain(|key| key != "exception_type" && key != "exception_message");
        keys
    }

    #[test]
    fn extra_info_json_order_matches_duckdb() {
        // The key orders of DuckDB 1.5.6's `errors_as_json` objects.
        let column = |candidates: bool| {
            let error = Error::binder("m").with_subtype("COLUMN_NOT_FOUND");
            if candidates {
                error
                    .with_extra_info("position", "7")
                    .with_extra_info("name", "x")
                    .with_extra_info("candidates", "y")
                    .with_extra_info_round_trip()
            } else {
                error
                    .with_extra_info("name", "x")
                    .with_extra_info_round_trip()
                    .with_position(Some(7))
            }
        };
        assert_eq!(
            json_keys(column(true)),
            ["candidates", "position", "error_subtype", "name"]
        );
        assert_eq!(json_keys(column(false)), ["error_subtype", "position", "name"]);
        let missing = |position: Option<u64>| {
            Error::catalog("m")
                .with_subtype("MISSING_ENTRY")
                .with_extra_info_slot("position")
                .with_extra_info("name", "t")
                .with_extra_info("type", "Table")
                .with_extra_info("candidates", "s")
                .with_extra_info_round_trip()
                .with_position(position)
        };
        assert_eq!(
            json_keys(missing(Some(14))),
            ["candidates", "position", "type", "error_subtype", "name"]
        );
        assert_eq!(
            json_keys(missing(None)),
            ["candidates", "type", "error_subtype", "name"]
        );
        let function = Error::binder("m")
            .with_subtype("NO_MATCHING_FUNCTION")
            .with_extra_info("name", "f")
            .with_extra_info("catalog", "system")
            .with_extra_info("schema", "main")
            .with_extra_info("call", "f()")
            .with_extra_info("candidates", "f(INTEGER)")
            .with_extra_info_round_trip()
            .with_position(Some(7));
        assert_eq!(
            json_keys(function),
            [
                "catalog",
                "candidates",
                "call",
                "error_subtype",
                "position",
                "schema",
                "name"
            ]
        );
        let syntax = Error::parser("m")
            .with_subtype("SYNTAX_ERROR")
            .with_query_location(14);
        assert_eq!(json_keys(syntax), ["error_subtype", "position"]);
    }

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
