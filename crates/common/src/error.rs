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
/// Native clients use `to_bytes` for DuckDB's final message
/// (`ErrorData::ConstructFinalMessage`). `Display` is its UTF-8 presentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: Cow<'static, [u8]>,
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
        key: Cow<'static, str>,
        value: Option<Vec<u8>>,
    },
    /// The exception is caught into an `ErrorData` (`ErrorData(const std::exception &)`).
    RoundTrip,
}

impl ExtraInfo {
    fn get(&self, key: &str) -> Option<&[u8]> {
        self.steps.iter().find_map(|step| match step {
            ExtraInfoStep::Entry {
                key: entry,
                value: Some(value),
            } if entry.as_ref() == key => Some(value.as_slice()),
            _ => None,
        })
    }

    /// Sets `key`, in its place if it has one.
    fn set(&mut self, key: impl Into<Cow<'static, str>>, value: Option<Vec<u8>>) {
        let key = key.into();
        for step in &mut self.steps {
            if let ExtraInfoStep::Entry {
                key: entry,
                value: slot,
            } = step
                && entry.as_ref() == key.as_ref()
            {
                *slot = value;
                return;
            }
        }
        self.steps.push(ExtraInfoStep::Entry { key, value });
    }

    /// The entries in the iteration order of DuckDB's map after the history, and one more
    /// round trip: the client catching the error.
    fn iteration_order(&self) -> Vec<(&str, &[u8])> {
        let mut order: Vec<(&str, &[u8])> = Vec::new();
        let mut table = crate::platform_order::UnorderedOrder::default();
        for step in &self.steps {
            match step {
                ExtraInfoStep::Entry {
                    key,
                    value: Some(value),
                } => {
                    let (inserted, index) = table.insert(
                        crate::platform_order::string_hash(key.as_bytes()),
                        key.as_ref(),
                        |a, b| a == b,
                    );
                    if inserted {
                        order.push((key.as_ref(), value));
                    } else {
                        order[index].1 = value;
                    }
                }
                ExtraInfoStep::Entry { value: None, .. } => {}
                ExtraInfoStep::RoundTrip => {
                    let entries: Vec<(&str, &[u8])> =
                        table.order().into_iter().map(|index| order[index]).collect();
                    (table, order) = round_trip(&entries);
                }
            }
        }
        // The final client catch rebuilds the map once more. Keep this separate
        // from the borrowed steps so owned keys retain the error's lifetime.
        let entries: Vec<(&str, &[u8])> = table.order().into_iter().map(|index| order[index]).collect();
        let (table, order) = round_trip(&entries);
        table.order().into_iter().map(|index| order[index]).collect()
    }
}

/// Rebuilds a map of `entries` (in iteration order) as `ErrorData` does from the
/// exception's JSON: the JSON object, with `exception_type` and `exception_message`
/// first, is parsed into a map (`ComplexJSON`), that one is flattened into another, whose
/// entries but the type and message make the new extra info.
fn round_trip<'a>(
    entries: &[(&'a str, &'a [u8])],
) -> (
    crate::platform_order::UnorderedOrder<&'a str>,
    Vec<(&'a str, &'a [u8])>,
) {
    const TYPE_AND_MESSAGE: [(&str, &[u8]); 2] = [("exception_type", b""), ("exception_message", b"")];
    let rebuild = |entries: &[(&'a str, &'a [u8])]| {
        let mut table = crate::platform_order::UnorderedOrder::default();
        for (key, _) in entries {
            table.insert(
                crate::platform_order::string_hash(key.as_bytes()),
                *key,
                |a, b| a == b,
            );
        }
        let ordered: Vec<(&'a str, &'a [u8])> =
            table.order().into_iter().map(|index| entries[index]).collect();
        ordered
    };
    let json: Vec<(&'a str, &'a [u8])> = TYPE_AND_MESSAGE
        .iter()
        .copied()
        .chain(entries.iter().copied())
        .collect();
    let flattened = rebuild(&rebuild(&json));
    let mut table = crate::platform_order::UnorderedOrder::default();
    let mut order = Vec::with_capacity(entries.len());
    for entry in flattened {
        if TYPE_AND_MESSAGE.iter().any(|(key, _)| *key == entry.0) {
            continue;
        }
        table.insert(
            crate::platform_order::string_hash(entry.0.as_bytes()),
            entry.0,
            |a, b| a == b,
        );
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
        let message = match message.into() {
            Cow::Borrowed(text) => Cow::Borrowed(text.as_bytes()),
            Cow::Owned(text) => Cow::Owned(text.into_bytes()),
        };
        Self::new_bytes(kind, message)
    }

    /// An exception message is a byte string in DuckDB, including invalid UTF-8.
    pub fn new_bytes(kind: ErrorKind, message: impl Into<Cow<'static, [u8]>>) -> Self {
        // ErrorData::SanitizeErrorMessage runs before either native or JSON formatting.
        let mut message = message.into();
        if message.contains(&0) {
            let mut sanitized = Vec::with_capacity(message.len());
            for &byte in message.iter() {
                if byte == 0 {
                    sanitized.extend_from_slice(b"\\0");
                } else {
                    sanitized.push(byte);
                }
            }
            message = sanitized.into();
        }
        Self {
            kind,
            message,
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
        self.extra_info
            .get("error_subtype")
            .and_then(|value| std::str::from_utf8(value).ok())
    }

    /// Sets extra info `key` (`extra_info[key] = value`).
    #[must_use]
    pub fn with_extra_info(
        mut self,
        key: impl Into<Cow<'static, str>>,
        value: impl Into<crate::ByteString>,
    ) -> Self {
        self.extra_info.set(key, Some(value.into().into_bytes()));
        self
    }

    /// Attach an HTTP response as DuckDB's `HTTPException::HTTPExtraInfo` does.
    /// Header names retain their original spelling; the response header map keeps
    /// the first value for each case-insensitive name.
    #[must_use]
    pub fn with_http_response<K: AsRef<str>, V: AsRef<[u8]>>(
        mut self,
        status: u16,
        body: impl Into<crate::ByteString>,
        headers: impl IntoIterator<Item = (K, V)>,
        reason: impl Into<crate::ByteString>,
    ) -> Self {
        self = self
            .with_extra_info("status_code", status.to_string())
            .with_extra_info("reason", reason)
            .with_extra_info("response_body", body);
        let entries: Vec<_> = headers
            .into_iter()
            .map(|(name, value)| (name.as_ref().to_owned(), value.as_ref().to_vec()))
            .collect();
        let mut order = crate::platform_order::UnorderedOrder::default();
        let mut selected = Vec::new();
        for (index, (name, _)) in entries.iter().enumerate() {
            let (inserted, _) = order.insert(crate::libcxx::ci_hash(name), name.as_str(), |a, b| {
                a.eq_ignore_ascii_case(b)
            });
            if inserted {
                selected.push(index);
            }
        }
        for index in order.order() {
            let (name, value) = &entries[selected[index]];
            self = self.with_extra_info(format!("header_{name}"), value.as_slice());
        }
        // Constructing HTTPException does not catch it. The binder or executor
        // records its own ErrorData boundary when this error reaches that caller.
        self
    }

    /// Holds the place of extra info `key` that DuckDB sets at this point, when the value
    /// is attached later (the `position` of a catalog lookup).
    #[must_use]
    pub fn with_extra_info_slot(mut self, key: impl Into<Cow<'static, str>>) -> Self {
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
            .and_then(|position| std::str::from_utf8(position).ok()?.parse().ok())
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
    pub fn with_query_context(self, query: &str) -> Self {
        self.with_query_context_bytes(query.as_bytes())
    }

    #[must_use]
    pub fn with_query_context_bytes(mut self, query: &[u8]) -> Self {
        if !query.is_empty()
            && let Some(position) = self.position()
        {
            let mut message = self.message.to_vec();
            message.extend_from_slice(crate::query_context::format_bytes(query, b"", position).as_bytes());
            self.message = message.into();
            self.extra_info.set("position", None);
        }
        // ErrorData::AddErrorLocation transfers an already resolved trace into
        // the raw message before ConstructFinalMessage appends the internal footer.
        if let Some(trace) = self
            .extra_info
            .get("stack_trace")
            .filter(|trace| !trace.is_empty())
        {
            let mut message = self.message.to_vec();
            message.extend_from_slice(b"\n\nStack Trace:\n");
            message.extend_from_slice(trace);
            self.message = message.into();
            self.extra_info.set("stack_trace", Some(Vec::new()));
        }
        self
    }

    /// The error as a JSON object of its type, message and extra info, which becomes
    /// its whole message (`ErrorData::ConvertErrorToJSON` under `errors_as_json`).
    #[must_use]
    pub fn into_json(mut self) -> Self {
        if self.json || self.message.starts_with(b"{") {
            return self;
        }
        let mut json = Vec::from(b"{".as_slice());
        let mut field = |key: &str, value: &[u8]| {
            if json.len() > 1 {
                json.push(b',');
            }
            json.extend(crate::string_util::json_quote(key).as_bytes());
            json.push(b':');
            json.extend(crate::string_util::json_quote_bytes(value));
        };
        field("exception_type", self.kind.display_name().as_bytes());
        field("exception_message", &self.message);
        for (key, value) in self.extra_info.iteration_order() {
            field(key, value);
        }
        json.push(b'}');
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
        // Throw serializes the processed message into an exception; the next
        // ErrorData catch sanitizes any NUL introduced by query context.
        if self.message.contains(&0) {
            let mut message = Vec::with_capacity(self.message.len());
            for &byte in self.message.iter() {
                if byte == 0 {
                    message.extend_from_slice(b"\\0");
                } else {
                    message.push(byte);
                }
            }
            self.message = message.into();
        }
        self.json = false;
        self
    }

    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The extra info (`error_subtype`, `position`, ...) in the iteration order of the
    /// map of an `ErrorData` that caught this error (`ErrorData::ExtraInfo`).
    pub fn extra_info(&self) -> Vec<(&str, &[u8])> {
        self.extra_info.iteration_order()
    }

    /// The UTF-8 presentation of the message without the `"<Kind> Error: "` prefix.
    /// Native interfaces must use `raw_message_bytes` to preserve invalid UTF-8.
    pub fn raw_message(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.message)
    }

    /// Message bytes without the exception type prefix, with NULs sanitized as in
    /// DuckDB's `ErrorData::SanitizeErrorMessage`.
    pub fn raw_message_bytes(&self) -> &[u8] {
        &self.message
    }

    /// Native client formatting, preserving non-UTF-8 exception bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        if self.json {
            return self.message.to_vec();
        }
        let mut result = Vec::new();
        if self.kind != ErrorKind::UnknownType {
            result.extend_from_slice(self.kind.display_name().as_bytes());
            result.push(b' ');
        }
        result.extend_from_slice(b"Error: ");
        for &byte in self.message.iter() {
            if byte == 0 {
                result.extend_from_slice(b"\\0");
            } else {
                result.push(byte);
            }
        }
        if self.kind == ErrorKind::Internal {
            result.extend_from_slice(INTERNAL_ERROR_SUFFIX.as_bytes());
            if let Some((_, trace)) = self
                .extra_info
                .iteration_order()
                .into_iter()
                .find(|(key, value)| *key == "stack_trace" && !value.is_empty())
            {
                result.extend_from_slice(b"\n\nStack Trace:\n");
                result.extend_from_slice(trace);
            }
        }
        result
    }

    pub fn parser(message: impl Into<crate::ByteString>) -> Self {
        Self::new_bytes(ErrorKind::Parser, message.into().into_bytes())
    }

    pub fn binder(message: impl Into<crate::ByteString>) -> Self {
        Self::new_bytes(ErrorKind::Binder, message.into().into_bytes())
    }

    pub fn catalog(message: impl Into<crate::ByteString>) -> Self {
        Self::new_bytes(ErrorKind::Catalog, message.into().into_bytes())
    }

    pub fn conversion(message: impl Into<crate::ByteString>) -> Self {
        Self::new_bytes(ErrorKind::Conversion, message.into().into_bytes())
    }

    pub fn out_of_range(message: impl Into<crate::ByteString>) -> Self {
        Self::new_bytes(ErrorKind::OutOfRange, message.into().into_bytes())
    }

    pub fn invalid_input(message: impl Into<crate::ByteString>) -> Self {
        Self::new_bytes(ErrorKind::InvalidInput, message.into().into_bytes())
    }

    pub fn internal(message: impl Into<crate::ByteString>) -> Self {
        Self::new_bytes(ErrorKind::Internal, message.into().into_bytes())
    }

    /// An internal invariant with its actual native origin, captured before the
    /// failing stack unwinds. Ordinary errors do not pay for trace capture.
    pub fn internal_with_stack_trace(message: impl Into<crate::ByteString>) -> Self {
        Self::internal(message).with_extra_info(
            "stack_trace",
            std::backtrace::Backtrace::force_capture().to_string(),
        )
    }

    /// DuckDB's `SerializationException`.
    pub fn serialization(message: impl Into<crate::ByteString>) -> Self {
        Self::new_bytes(ErrorKind::Serialization, message.into().into_bytes())
    }

    /// A feature that the reference engine supports but this port does not (yet).
    pub fn not_implemented(message: impl Into<crate::ByteString>) -> Self {
        Self::new_bytes(ErrorKind::NotImplemented, message.into().into_bytes())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.to_bytes()))
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
    fn owned_metadata_keys_preserve_existing_round_trip_order() {
        let borrowed = Error::new(ErrorKind::Http, "missing")
            .with_extra_info("header_x-MiXeD", "value")
            .with_extra_info_round_trip()
            .with_extra_info("status_code", "404")
            .into_json();
        let owned = Error::new(ErrorKind::Http, "missing")
            .with_extra_info(format!("header_{}", "x-MiXeD"), "value")
            .with_extra_info_round_trip()
            .with_extra_info("status_code".to_owned(), "404")
            .into_json();
        assert_eq!(owned.extra_info(), borrowed.extra_info());
        assert_eq!(owned.to_bytes(), borrowed.to_bytes());
    }

    #[test]
    fn http_metadata_preserves_wire_spelling_and_first_header_value() {
        let error = Error::new(ErrorKind::Http, "missing").with_http_response(
            404,
            b"body\0bytes",
            [
                ("x-MiXeD", b"first\xff".as_slice()),
                ("X-MIXED", b"second".as_slice()),
                ("Content-Length", b"10".as_slice()),
            ],
            "Not Found",
        );
        let info = error.extra_info();
        assert!(info.contains(&("status_code", b"404".as_slice())));
        assert!(info.contains(&("reason", b"Not Found".as_slice())));
        assert!(info.contains(&("response_body", b"body\0bytes".as_slice())));
        assert!(info.contains(&("header_x-MiXeD", b"first\xff".as_slice())));
        assert!(!info.iter().any(|(name, _)| *name == "header_X-MIXED"));
        assert_eq!(error.to_bytes(), b"HTTP Error: missing");
    }

    #[test]
    fn native_error_bytes_preserve_invalid_utf8_and_escape_nul() {
        let error = Error::new_bytes(ErrorKind::InvalidInput, b"caf\xe9\0\xff".to_vec());
        assert_eq!(error.raw_message_bytes(), b"caf\xe9\\0\xff");
        assert_eq!(error.to_bytes(), b"Invalid Input Error: caf\xe9\\0\xff");
        assert_eq!(error.raw_message(), "caf\u{fffd}\\0\u{fffd}");
        assert_eq!(
            Error::new_bytes(ErrorKind::UnknownType, vec![255]).to_bytes(),
            b"Error: \xff"
        );
        assert_eq!(
            Error::internal("boom").to_bytes(),
            Error::internal("boom").to_string().as_bytes()
        );
    }

    #[test]
    fn captured_internal_trace_preserves_raw_message_and_clone() {
        let error = Error::new_bytes(ErrorKind::Internal, b"caf\xe9\0".to_vec())
            .with_extra_info("stack_trace", b"actual frame\xff".to_vec());
        assert_eq!(error.raw_message_bytes(), b"caf\xe9\\0");
        let rendered = error.to_bytes();
        assert!(rendered.starts_with(b"INTERNAL Error: caf\xe9\\0"));
        assert!(rendered.ends_with(b"\n\nStack Trace:\nactual frame\xff"));
        assert_eq!(error.clone().to_bytes(), rendered);
        assert!(
            !Error::internal("plain")
                .to_bytes()
                .windows(b"Stack Trace:".len())
                .any(|window| window == b"Stack Trace:")
        );
    }

    #[test]
    fn originating_trace_survives_return_to_its_renderer() {
        #[inline(never)]
        fn traced_failure_origin() -> Error {
            Error::internal_with_stack_trace("origin")
        }
        let error = traced_failure_origin();
        let rendered = error.to_string();
        let (_, trace) = rendered
            .split_once("\n\nStack Trace:\n")
            .expect("native trace section");
        assert_ne!(trace.trim(), "");
        assert_eq!(error.raw_message_bytes(), b"origin");
        assert_eq!(error.clone().to_string(), rendered);
    }

    #[test]
    fn json_error_bytes_follow_yyjson_escaping() {
        let message = b"\xff\"\\\n\r\t\x08\x0c\0\x1f";
        let error = Error::new_bytes(ErrorKind::InvalidInput, message.to_vec())
            .with_extra_info("name", "caf\u{e9}")
            .into_json();
        assert_eq!(error.to_bytes(), b"{\"exception_type\":\"Invalid Input\",\"exception_message\":\"\xff\\\"\\\\\\n\\r\\t\\b\\f\\\\0\\u001F\",\"name\":\"caf\xc3\xa9\"}");
        assert_eq!(error.clone().into_json(), error);
        let rethrown = error.clone().rethrown().to_bytes();
        assert_eq!(&rethrown[b"Invalid Input Error: ".len()..], error.to_bytes());
    }

    #[test]
    fn query_context_keeps_original_error_bytes() {
        let error = Error::new_bytes(ErrorKind::Binder, b"caf\xe9".to_vec())
            .with_position(Some(7))
            .with_query_context("SELECT unknown");
        assert_eq!(
            error.raw_message_bytes(),
            b"caf\xe9\n\nLINE 1: SELECT unknown\n               ^"
        );
        assert_eq!(error.position(), None);
    }

    #[test]
    fn query_context_places_resolved_trace_before_internal_footer_once() {
        let error = Error::internal("origin")
            .with_extra_info("stack_trace", "0        real origin\n")
            .with_position(Some(7))
            .with_query_context("SELECT unknown")
            .with_query_context("SELECT unknown");
        let rendered = error.to_string();
        assert_eq!(rendered.matches("Stack Trace:").count(), 1);
        let context = rendered.find("LINE 1:").unwrap();
        let trace = rendered.find("Stack Trace:").unwrap();
        let footer = rendered.find("This error signals").unwrap();
        assert!(context < trace);
        assert!(trace < footer);
        assert_eq!(error.extra_info.get("stack_trace"), Some(b"".as_slice()));
    }

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
            if cfg!(target_os = "linux") {
                vec!["candidates", "name", "error_subtype", "position"]
            } else {
                vec!["candidates", "position", "error_subtype", "name"]
            }
        );
        assert_eq!(
            json_keys(column(false)),
            vec!["error_subtype", "position", "name"]
        );
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
            if cfg!(target_os = "linux") {
                vec!["candidates", "type", "name", "error_subtype", "position"]
            } else {
                vec!["candidates", "position", "type", "error_subtype", "name"]
            }
        );
        assert_eq!(
            json_keys(missing(None)),
            vec!["candidates", "type", "error_subtype", "name"]
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
            if cfg!(target_os = "linux") {
                vec![
                    "candidates",
                    "call",
                    "schema",
                    "error_subtype",
                    "catalog",
                    "name",
                    "position",
                ]
            } else {
                vec![
                    "catalog",
                    "candidates",
                    "call",
                    "error_subtype",
                    "position",
                    "schema",
                    "name",
                ]
            }
        );
        let syntax = Error::parser("m")
            .with_subtype("SYNTAX_ERROR")
            .with_query_location(14);
        assert_eq!(
            json_keys(syntax),
            if cfg!(target_os = "linux") {
                vec!["position", "error_subtype"]
            } else {
                vec!["error_subtype", "position"]
            }
        );
    }

    #[test]
    fn filled_location_slot_tracks_later_value_updates() {
        let error = Error::catalog("missing")
            .with_subtype("MISSING_ENTRY")
            .with_extra_info_slot("position")
            .with_extra_info("name", "t")
            .with_extra_info_round_trip()
            .with_position(Some(7))
            .with_position(Some(14));
        assert_eq!(error.position(), Some(7));
        let error = error.with_position_override(Some(14));
        assert_eq!(error.extra_info.get("position"), Some(b"14".as_slice()));
        let entries = error.extra_info.iteration_order();
        assert_eq!(entries.iter().filter(|(key, _)| *key == "position").count(), 1);
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
