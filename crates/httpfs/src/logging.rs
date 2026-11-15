//! HTTP request logs use the attaching connection's logger, including retries.
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use roastedduck_common::Result;
use roastedduck_function::logging::{ClientLogger, LogLevel, LogScope};
use roastedduck_function::registered_files::FileHandle;
use roastedduck_types::{LogicalType, Timestamp, Value};
use std::io::Read;

#[derive(Debug)]
struct FileLog(roastedduck_function::logging::FileSystemLogger);
impl Drop for FileLog {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[derive(Debug)]
struct LoggedFile {
    inner: Arc<dyn FileHandle>,
    log: Arc<FileLog>,
}

pub(super) fn logged_file(
    inner: Arc<dyn FileHandle>,
    name: &'static str,
    path: &[u8],
) -> Arc<dyn FileHandle> {
    let Some(log) = roastedduck_function::logging::FileSystemLogger::open_for_system(
        roastedduck_function::file_system::opener().as_deref(),
        path,
        name,
    ) else {
        return inner;
    };
    Arc::new(LoggedFile {
        inner,
        log: Arc::new(FileLog(log)),
    })
}

impl FileHandle for LoggedFile {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize> {
        let count = self.inner.read_at(offset, buffer)?;
        self.log.0.read(count as u64, offset);
        Ok(count)
    }
    fn read_stream(&self) -> Result<Option<Box<dyn Read + Send>>> {
        Ok(self.inner.read_stream()?.map(|reader| {
            Box::new(LoggedStream {
                reader,
                log: Arc::clone(&self.log),
                position: 0,
            }) as Box<dyn Read + Send>
        }))
    }
    fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<usize> {
        let count = self.inner.write_at(offset, buffer)?;
        self.log.0.write(count as u64, offset);
        Ok(count)
    }
    fn size(&self) -> Result<u64> {
        self.inner.size()
    }
    fn modified(&self) -> Result<i64> {
        self.inner.modified()
    }
    fn version_tag(&self) -> Result<String> {
        self.inner.version_tag()
    }
    fn flush(&self) -> Result<()> {
        self.inner.flush()
    }
    fn close(&self) -> Result<()> {
        self.inner.close()
    }
    fn sync(&self) -> Result<()> {
        self.inner.sync()
    }
    fn truncate(&self, size: u64) -> Result<()> {
        self.inner.truncate(size)
    }
}

struct LoggedStream {
    reader: Box<dyn Read + Send>,
    log: Arc<FileLog>,
    position: u64,
}
impl Read for LoggedStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.reader.read(buffer)?;
        self.log.0.read(count as u64, self.position);
        self.position += count as u64;
        Ok(count)
    }
}

#[derive(Clone, Debug)]
pub(super) struct HttpLogger {
    client: Arc<dyn ClientLogger>,
    http_enabled: bool,
    azure_redaction: Option<(Vec<String>, Vec<String>)>,
}

impl HttpLogger {
    pub(super) fn fallback(&self, url: &str) -> roastedduck_common::Result<()> {
        if self
            .client
            .should_log(LogScope::Connection, "", LogLevel::Warning)
        {
            self.client.write_log(LogScope::Connection, "", LogLevel::Warning,
                &format!("Falling back to full file download for file '{url}': the server does not support HTTP range requests. Performance and memory usage are potentially degraded."))?;
        }
        Ok(())
    }

    pub(super) fn current() -> Option<Self> {
        let client = roastedduck_function::file_system::opener()?;
        Some(Self {
            http_enabled: client
                .settings()
                .and_then(|settings| settings.current_setting("enable_http_logging").ok())
                .and_then(|value| value.as_bool())
                .unwrap_or(true),
            client: client.logger()?,
            azure_redaction: None,
        })
    }

    pub(super) fn azure(query_params: Vec<String>, headers: Vec<String>) -> Option<Self> {
        let mut logger = Self::current()?;
        logger.azure_redaction = Some((query_params, headers));
        Some(logger)
    }

    fn header(&self, name: &str, value: &str) -> (Value, Value) {
        if let Some((_, redact)) = &self.azure_redaction {
            let name = name.to_ascii_lowercase();
            let value = if redact.contains(&name) { "REDACTED" } else { value };
            (Value::varchar(name), Value::varchar(value))
        } else {
            (Value::varchar(name), Value::varchar(value))
        }
    }

    fn url(&self, url: &str) -> String {
        let Some((redact, _)) = &self.azure_redaction else {
            return url.into();
        };
        if redact.is_empty() {
            return url.into();
        }
        let Some((base, query)) = url.split_once('?') else {
            return url.into();
        };
        let query = query
            .split('&')
            .filter(|s| !s.is_empty())
            .map(|field| {
                if let Some((name, _)) = field.split_once('=')
                    && redact.iter().any(|s| s == name)
                {
                    format!("{name}=REDACTED")
                } else {
                    field.into()
                }
            })
            .collect::<Vec<String>>()
            .join("&");
        format!("{base}?{query}")
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn record(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body_length: usize,
        start: SystemTime,
        elapsed: Instant,
        response: &std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) {
        if self.azure_redaction.is_none() && !self.http_enabled {
            return;
        }
        if !self
            .client
            .should_log(LogScope::Connection, "HTTP", LogLevel::Debug)
        {
            return;
        }
        let map = |entries: Vec<(Value, Value)>| {
            // HTTPHeaders::Insert uses a case-insensitive map and keeps the
            // first value. HTTP responses may repeat a header (e.g. Vary).
            let mut names = std::collections::HashSet::new();
            let entries = entries
                .into_iter()
                .filter(|(name, _)| names.insert(name.to_varchar().unwrap_or_default().to_ascii_lowercase()))
                .collect();
            Value::map(LogicalType::VARCHAR, LogicalType::VARCHAR, entries)
        };
        let mut request_fields = vec![
            ("type".into(), Value::varchar(method)),
            ("url".into(), Value::varchar(self.url(url))),
            (
                "start_time".into(),
                Value::timestamp_tz(Timestamp(
                    start
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros()
                        .min(i64::MAX as u128) as i64,
                )),
            ),
            (
                "duration_ms".into(),
                Value::bigint(elapsed.elapsed().as_millis().min(i64::MAX as u128) as i64),
            ),
            (
                "request_body_length".into(),
                if body_length == 0 {
                    Value::null(LogicalType::UBIGINT)
                } else {
                    Value::ubigint(body_length as u64)
                },
            ),
            (
                "headers".into(),
                map(headers
                    .iter()
                    .map(|(name, value)| self.header(name, value))
                    .collect()),
            ),
        ];
        if self.azure_redaction.is_some() {
            request_fields.retain(|(key, _)| key != "request_body_length");
        }
        let request = Value::struct_value(request_fields);
        let (status, reason, headers) = match response {
            Ok(response) => (
                status_name(response.status()),
                response
                    .status()
                    .canonical_reason()
                    .unwrap_or_default()
                    .to_owned(),
                response
                    .headers()
                    .iter()
                    .map(|(name, value)| {
                        self.header(
                            super::wire_headers::name(response, name.as_str()),
                            &String::from_utf8_lossy(value.as_bytes()),
                        )
                    })
                    .collect(),
            ),
            Err(error) => ("INVALID".to_owned(), error.to_string(), Vec::new()),
        };
        let response = if response.is_err() && self.azure_redaction.is_some() {
            Value::null(LogicalType::SQLNULL)
        } else {
            Value::struct_value(vec![
                ("status".into(), Value::varchar(status)),
                ("reason".into(), Value::varchar(reason)),
                ("headers".into(), map(headers)),
            ])
        };
        if let Ok(message) =
            Value::struct_value(vec![("request".into(), request), ("response".into(), response)])
                .to_varchar_bytes()
        {
            let _ = self
                .client
                .write_log_bytes(LogScope::Connection, "HTTP", LogLevel::Debug, &message);
        }
    }
}

fn status_name(status: ureq::http::StatusCode) -> String {
    // DuckDB 1.5.6 HTTPStatusCode public enum names.
    match status.as_u16() {
        100 => "Continue_100",
        101 => "SwitchingProtocol_101",
        102 => "Processing_102",
        103 => "EarlyHints_103",
        200 => "OK_200",
        201 => "Created_201",
        202 => "Accepted_202",
        203 => "NonAuthoritativeInformation_203",
        204 => "NoContent_204",
        205 => "ResetContent_205",
        206 => "PartialContent_206",
        207 => "MultiStatus_207",
        208 => "AlreadyReported_208",
        226 => "IMUsed_226",
        300 => "MultipleChoices_300",
        301 => "MovedPermanently_301",
        302 => "Found_302",
        303 => "SeeOther_303",
        304 => "NotModified_304",
        305 => "UseProxy_305",
        306 => "unused_306",
        307 => "TemporaryRedirect_307",
        308 => "PermanentRedirect_308",
        400 => "BadRequest_400",
        401 => "Unauthorized_401",
        402 => "PaymentRequired_402",
        403 => "Forbidden_403",
        404 => "NotFound_404",
        405 => "MethodNotAllowed_405",
        406 => "NotAcceptable_406",
        407 => "ProxyAuthenticationRequired_407",
        408 => "RequestTimeout_408",
        409 => "Conflict_409",
        410 => "Gone_410",
        411 => "LengthRequired_411",
        412 => "PreconditionFailed_412",
        413 => "PayloadTooLarge_413",
        414 => "UriTooLong_414",
        415 => "UnsupportedMediaType_415",
        416 => "RangeNotSatisfiable_416",
        417 => "ExpectationFailed_417",
        418 => "ImATeapot_418",
        421 => "MisdirectedRequest_421",
        422 => "UnprocessableContent_422",
        423 => "Locked_423",
        424 => "FailedDependency_424",
        425 => "TooEarly_425",
        426 => "UpgradeRequired_426",
        428 => "PreconditionRequired_428",
        429 => "TooManyRequests_429",
        431 => "RequestHeaderFieldsTooLarge_431",
        451 => "UnavailableForLegalReasons_451",
        500 => "InternalServerError_500",
        501 => "NotImplemented_501",
        502 => "BadGateway_502",
        503 => "ServiceUnavailable_503",
        504 => "GatewayTimeout_504",
        505 => "HttpVersionNotSupported_505",
        506 => "VariantAlsoNegotiates_506",
        507 => "InsufficientStorage_507",
        508 => "LoopDetected_508",
        510 => "NotExtended_510",
        511 => "NetworkAuthenticationRequired_511",
        _ => "INVALID",
    }
    .to_owned()
}
