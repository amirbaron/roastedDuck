//! HTTP(S) filesystem transport. Registration is explicit; no query data is cached.
use std::io::{self, Read};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use rustdb_common::{ByteString, Error, ErrorKind, Result};
use rustdb_function::ExtensionOption;
use rustdb_function::registered_files::{FileHandle, FileSystem};
use rustdb_types::{LogicalType, Value};
use ureq::unversioned::transport::{Connector as _, TcpConnector};

mod body;
pub mod huggingface;
pub use body::BodyReader;
mod logging;
mod openssl_tls;
mod proxy;
mod redirect;
pub mod s3;

fn io_error(error: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Io, error.to_string())
}

fn unquote_etag(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

/// Database-provided HTTP secret selection, shared with the core secret manager.
pub trait ResolveHttpSecret: std::fmt::Debug + Send + Sync {
    fn resolve(&self, url: &str) -> Result<Option<HttpSecret>>;
}

pub const HTTP_SECRET_RESOLVER_KEY: &str = "rustdb.http_secret_resolver";

#[derive(Debug)]
pub struct HttpSecretResolver(pub Arc<dyn ResolveHttpSecret>);

#[derive(Clone, Default)]
pub struct HttpSecret {
    pub headers: Vec<(String, String)>,
    pub bearer_token: Option<String>,
    pub verify_ssl: Option<bool>,
    pub proxy: Option<String>,
    pub proxy_user: Option<String>,
    pub proxy_password: Option<String>,
}

impl std::fmt::Debug for HttpSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSecret")
            .field("header_count", &self.headers.len())
            .finish_non_exhaustive()
    }
}

/// HTTP settings from the pinned httpfs source. S3/HuggingFace registration is
/// separate: this provider handles only HTTP and HTTPS paths.
pub fn options() -> Vec<ExtensionOption> {
    let entries = [
        (
            "http_timeout",
            "HTTP timeout read/write/connection/retry (in seconds)",
            LogicalType::UBIGINT,
            Value::ubigint(30),
        ),
        (
            "http_retries",
            "HTTP retries on I/O error",
            LogicalType::UBIGINT,
            Value::ubigint(3),
        ),
        (
            "http_retry_wait_ms",
            "Time between retries",
            LogicalType::UBIGINT,
            Value::ubigint(100),
        ),
        (
            "http_retry_backoff",
            "Backoff factor for exponentially increasing retry wait time",
            LogicalType::FLOAT,
            Value::float(4.0),
        ),
        (
            "http_keep_alive",
            "Keep alive connections. Setting this to false can help when running into connection failures",
            LogicalType::BOOLEAN,
            Value::boolean(true),
        ),
        (
            "allow_asterisks_in_http_paths",
            "Allow '*' character in URLs users can query",
            LogicalType::BOOLEAN,
            Value::boolean(false),
        ),
        (
            "enable_curl_server_cert_verification",
            "Enable server side certificate verification for CURL backend.",
            LogicalType::BOOLEAN,
            Value::boolean(true),
        ),
        (
            "enable_server_cert_verification",
            "Enable server side certificate verification.",
            LogicalType::BOOLEAN,
            Value::boolean(false),
        ),
        (
            "ca_cert_file",
            "Path to a custom certificate file for self-signed certificates.",
            LogicalType::VARCHAR,
            Value::varchar(""),
        ),
        (
            "httpfs_client_implementation",
            "Select which is the HTTPUtil implementation to be used",
            LogicalType::VARCHAR,
            Value::varchar("default"),
        ),
        (
            "auto_fallback_to_full_download",
            "Allows automatically falling back to full file downloads when possible.",
            LogicalType::BOOLEAN,
            Value::boolean(true),
        ),
        (
            "unsafe_disable_etag_checks",
            "Disable checks on ETag consistency",
            LogicalType::BOOLEAN,
            Value::boolean(false),
        ),
        (
            "hf_max_per_page",
            "Debug option to limit number of items returned in list requests",
            LogicalType::UBIGINT,
            Value::ubigint(0),
        ),
    ];
    entries.into_iter().map(|(name, description, input_type, default_value)| {
        let set_function: Option<rustdb_function::ExtensionOptionSet> = match name {
            "httpfs_client_implementation" => Some(Arc::new(|value| {
                if !matches!(value.to_varchar()?.as_str(), "default" | "curl" | "httplib") {
                    return Err(Error::invalid_input("Unsupported option for httpfs_client_implementation, only `curl`, `httplib` and `default` are currently supported"));
                }
                Ok(())
            })),
            "ca_cert_file" => Some(Arc::new(|value| {
                if value.is_null() { return Err(Error::invalid_input("NULL it's not a valid option for ca_cert_file")); }
                let path = value.to_varchar_bytes()?;
                if !path.is_empty() { *value = Value::try_varchar(&rustdb_function::file_system::canonicalize_local_path_bytes(&path))?; }
                Ok(())
            })),
            _ => None,
        };
        ExtensionOption { name: name.into(), description: description.into(), input_type, default_value, set_function, global_by_default: false }
    }).collect()
}

#[derive(Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent C++ HTTP configuration flags"
)]
struct Parameters {
    timeout: u64,
    retries: u64,
    wait_ms: u64,
    backoff: f64,
    verify: bool,
    httplib: bool,
    ca: String,
    keep_alive: bool,
    redirects: u32,
    fallback: bool,
    check_etag: bool,
    proxy: String,
    proxy_user: String,
    proxy_password: String,
    headers: Vec<(String, String)>,
}

impl std::fmt::Debug for Parameters {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Parameters")
            .field("timeout", &self.timeout)
            .field("verify", &self.verify)
            .finish_non_exhaustive()
    }
}

impl Parameters {
    fn same_refresh_material(&self, other: &Self) -> bool {
        self.verify == other.verify
            && self.proxy == other.proxy
            && self.proxy_user == other.proxy_user
            && self.proxy_password == other.proxy_password
            && self.headers == other.headers
    }
    fn current(url: &str) -> Result<Self> {
        Self::current_with_http_secret(url, true, true)
    }

    fn current_with_http_secret(url: &str, enable_http_secret: bool, enable_bearer: bool) -> Result<Self> {
        let source = rustdb_function::file_system::opener().and_then(|client| client.settings());
        let text = |name: &str, fallback: &str| {
            source
                .as_ref()
                .and_then(|s| s.current_setting(name).ok())
                .and_then(|v| v.to_varchar().ok())
                .unwrap_or_else(|| fallback.to_owned())
        };
        let number = |name: &str, fallback: u64| text(name, "").parse().unwrap_or(fallback);
        let boolean = |name: &str, fallback: bool| {
            text(name, if fallback { "true" } else { "false" }).eq_ignore_ascii_case("true")
        };
        let httplib = text("httpfs_client_implementation", "default") == "httplib";
        let mut parameters = Self {
            timeout: number("http_timeout", 30),
            retries: number("http_retries", 3),
            wait_ms: number("http_retry_wait_ms", 100),
            backoff: text("http_retry_backoff", "4").parse().unwrap_or(4.0),
            verify: boolean(
                if httplib {
                    "enable_server_cert_verification"
                } else {
                    "enable_curl_server_cert_verification"
                },
                !httplib,
            ),
            httplib,
            ca: text("ca_cert_file", ""),
            keep_alive: boolean("http_keep_alive", true),
            redirects: 10,
            fallback: boolean("auto_fallback_to_full_download", true),
            check_etag: !boolean("unsafe_disable_etag_checks", false),
            proxy: text("http_proxy", ""),
            proxy_user: text("http_proxy_username", ""),
            proxy_password: text("http_proxy_password", ""),
            headers: Vec::new(),
        };
        if enable_http_secret
            && let Some(resolver) = rustdb_function::file_system::opener()
                .and_then(|c| c.object_cache())
                .and_then(|cache| cache.get::<HttpSecretResolver>(HTTP_SECRET_RESOLVER_KEY))
            && let Some(secret) = resolver.0.resolve(url)?
        {
            if enable_bearer && let Some(token) = secret.bearer_token {
                parameters
                    .headers
                    .push(("Authorization".into(), format!("Bearer {token}")));
            }
            for (name, value) in secret.headers {
                parameters
                    .headers
                    .retain(|(key, _)| !key.eq_ignore_ascii_case(&name));
                parameters.headers.push((name, value));
            }
            if let Some(verify) = secret.verify_ssl {
                parameters.verify = verify;
            }
            if let Some(proxy) = secret.proxy.filter(|v| !v.is_empty()) {
                parameters.proxy = proxy;
            }
            if let Some(user) = secret.proxy_user {
                parameters.proxy_user = user;
            }
            if let Some(password) = secret.proxy_password {
                parameters.proxy_password = password;
            }
        }
        Ok(parameters)
    }

    fn agent(&self) -> Result<ureq::Agent> {
        self.agent_with_resolver(ureq::unversioned::resolver::DefaultResolver::default())
    }

    fn agent_with_resolver(
        &self,
        resolver: impl ureq::unversioned::resolver::Resolver,
    ) -> Result<ureq::Agent> {
        if !self.ca.is_empty() {
            rustdb_function::file_system::verify_can_access_file(&self.ca)?;
        }
        let mut config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .proxy(None)
            .max_redirects(self.redirects)
            .redirect_auth_headers(ureq::config::RedirectAuthHeaders::SameHost)
            .timeout_connect(Some(Duration::from_secs(self.timeout)))
            .timeout_per_read(Some(Duration::from_secs(self.timeout)))
            .timeout_per_write(Some(Duration::from_secs(self.timeout)));
        if !self.keep_alive {
            config = config.max_idle_connections(0);
        }
        if !self.proxy.is_empty() {
            let proxy = ureq::Proxy::new(&self.proxy).map_err(io_error)?;
            let mut builder = ureq::Proxy::builder(ureq::ProxyProtocol::Http)
                .host(proxy.host())
                .port(proxy.port());
            if !self.proxy_user.is_empty() {
                builder = builder.username(&self.proxy_user).password(&self.proxy_password);
            }
            config = config.proxy(Some(builder.build().map_err(io_error)?));
        }
        let connector =
            ().chain(proxy::ProxyConnector)
                .chain(TcpConnector::default())
                .chain(openssl_tls::TlsConnector::new(
                    self.ca.clone(),
                    self.verify,
                    !self.httplib || self.ca.is_empty(),
                ));
        Ok(ureq::Agent::with_parts(config.build(), connector, resolver))
    }
}

#[derive(Clone, Debug)]
struct HttpFile {
    url: String,
    display_url: String,
    size: u64,
    modified: i64,
    etag: String,
    parameters: Parameters,
    agent: ureq::Agent,
    signing: Option<s3::RequestSigning>,
    logger: Option<logging::HttpLogger>,
    client: Option<std::sync::Weak<rustdb_function::FunctionClient>>,
}

fn status_error(method: &str, url: &str, status: u16) -> Error {
    let reason = ureq::http::StatusCode::from_u16(status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or_default();
    Error::new(
        ErrorKind::Http,
        format!("HTTP {method} error on '{url}' (HTTP {status} {reason})"),
    )
}

/// Preserve the wire response fields that the normalized header map cannot expose.
pub(crate) fn response_error<B>(
    error: Error,
    response: &ureq::http::Response<B>,
    body: &[u8],
    httplib: bool,
) -> Error {
    let status = response.status().as_u16();
    if let Some(metadata) = response
        .extensions()
        .get::<ureq_proto::OriginalResponseMetadata>()
    {
        error.with_http_response(
            status,
            body,
            response_header_order::input_order(&metadata.headers, httplib)
                .into_iter()
                .map(|index| {
                    let (name, value) = &metadata.headers[index];
                    (name.as_str(), value.as_slice())
                }),
            if httplib {
                metadata.reason.as_str()
            } else {
                response.status().canonical_reason().unwrap_or_default()
            },
        )
    } else {
        error.with_http_response(
            status,
            body,
            response
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
            response.status().canonical_reason().unwrap_or_default(),
        )
    }
}

pub(crate) struct ResponseMetadata {
    reason: String,
    headers: Vec<(String, Vec<u8>)>,
}

/// Prepare the installer's header inputs using its C++ transport's container history.
pub fn extension_headers(mut headers: Vec<(String, Vec<u8>)>, httplib: bool) -> Vec<(String, Vec<u8>)> {
    response_header_order::input_order(&headers, httplib)
        .into_iter()
        .map(|index| std::mem::take(&mut headers[index]))
        .collect()
}

pub(crate) fn take_response_metadata<B>(
    response: &mut ureq::http::Response<B>,
    httplib: bool,
) -> ResponseMetadata {
    let metadata = response
        .extensions_mut()
        .remove::<ureq_proto::OriginalResponseMetadata>()
        .unwrap_or_else(|| ureq_proto::OriginalResponseMetadata {
            reason: response
                .status()
                .canonical_reason()
                .unwrap_or_default()
                .to_owned(),
            headers: response
                .headers()
                .iter()
                .map(|(name, value)| (name.to_string(), value.as_bytes().to_vec()))
                .collect(),
        });
    let reason = if httplib {
        metadata.reason
    } else {
        response
            .status()
            .canonical_reason()
            .unwrap_or_default()
            .to_owned()
    };
    ResponseMetadata {
        reason,
        headers: extension_headers(metadata.headers, httplib),
    }
}

fn request_error(error: &ureq::Error, method: &str, url: &str, httplib: bool) -> Error {
    if let ureq::Error::Other(error) = error
        && let Some(certificate) = error.downcast_ref::<openssl_tls::CertificateError>()
    {
        let reason = if !httplib {
            "SSL peer certificate or SSH remote key was not OK"
        } else if certificate.hostname {
            "SSL server hostname verification failed"
        } else {
            "SSL server verification failed"
        };
        return io_error(format!("{reason} error for HTTP {method} to '{url}'"));
    }
    io_error(error)
}

/// The installer's streaming GET result. Installer retries and publication stay
/// in the core; HTTPFS supplies its configured TLS/proxy transport when loaded.
pub struct ExtensionResponse {
    pub status: u16,
    pub etag: ByteString,
    pub reason: ByteString,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: BodyReader<ureq::BodyReader<'static>>,
}

impl std::fmt::Debug for ExtensionResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionResponse")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// Match `HTTPUtil`'s install flow: no redirects or connection reuse, preserving
/// conditional GET and the calling session's HTTPFS options.
pub fn extension_get(url: &str, etag: Option<&[u8]>) -> std::result::Result<ExtensionResponse, String> {
    let mut parameters = Parameters::current(url).map_err(|error| error.raw_message().into_owned())?;
    parameters.redirects = 0;
    parameters.keep_alive = false;
    let agent = parameters
        .agent_with_resolver(proxy::InstallerResolver)
        .map_err(|error| error.raw_message().into_owned())?;
    let mut request = agent.get(url);
    for (name, value) in &parameters.headers {
        request = request.header(name.as_str(), value.as_str());
    }
    if let Some(etag) = etag {
        let etag = std::str::from_utf8(etag).map_err(|_| "Invalid extension ETag".to_owned())?;
        request = request.header("If-None-Match", etag);
    }
    let client = rustdb_function::file_system::opener();
    if let Some(client) = &client {
        client.record_http_request("GET", 0);
    }
    let mut response = request.call().map_err(|error| {
        if matches!(&error, ureq::Error::Other(error) if error.is::<proxy::ProxyNameResolutionError>()) {
            if parameters.httplib {
                "Failed to connect".to_owned()
            } else {
                "Could not resolve proxy name".to_owned()
            }
        } else {
            request_error(&error, "GET", url, parameters.httplib)
                .raw_message()
                .into_owned()
        }
    })?;
    let status = response.status().as_u16();
    let etag = response
        .headers()
        .get("etag")
        .map_or_else(ByteString::new, |value| value.as_bytes().into());
    let metadata = take_response_metadata(&mut response, parameters.httplib);
    Ok(ExtensionResponse {
        status,
        etag,
        reason: metadata.reason.into(),
        headers: metadata.headers,
        body: body::Body::new(response.into_body(), client.as_ref()).into_reader(),
    })
}

impl HttpFile {
    fn request(&self, method: &str, range: Option<(u64, u64)>) -> Result<ureq::http::Response<body::Body>> {
        self.request_payload(method, range, &[], false)
    }

    fn request_payload(
        &self,
        method: &str,
        range: Option<(u64, u64)>,
        payload: &[u8],
        return_final_status: bool,
    ) -> Result<ureq::http::Response<body::Body>> {
        let mut last = io_error("Could not establish connection");
        let mut redirected = false;
        let mut location_url = None;
        let mut location_headers = None;
        let mut location_count = 0;
        let mut request_method = method;
        let mut request_payload = payload;
        let mut refreshed_auth = false;
        'region: loop {
            let refreshed = self
                .signing
                .as_ref()
                .and_then(s3::RequestSigning::refreshed_transport);
            let parameters = refreshed
                .as_ref()
                .map_or(&self.parameters, |file| &file.parameters);
            let agent = refreshed.as_ref().map_or(&self.agent, |file| &file.agent);
            let mut template = refreshed
                .as_ref()
                .map_or_else(|| self.url.clone(), |file| file.url.clone());
            if refreshed.is_some()
                && let Some((_, query)) = self.url.split_once('?')
            {
                template.push('?');
                template.push_str(query);
            }
            for attempt in 0..=self.parameters.retries {
                if attempt >= 2 {
                    let delay =
                        self.parameters.wait_ms as f64 * self.parameters.backoff.powf((attempt - 2) as f64);
                    if delay.is_finite() && delay > 0.0 {
                        std::thread::sleep(Duration::from_millis(delay as u64));
                    }
                }
                let signing = self.signing.as_ref().map(|s| s.effective(method));
                let initial_url = signing
                    .as_ref()
                    .map_or_else(|| template.clone(), |s| s.request_url(&template, method));
                let url = location_url.as_ref().unwrap_or(&initial_url);
                let mut headers = Vec::new();
                if let Some(signing) = &signing {
                    headers.extend(signing.headers(method, payload)?);
                }
                for (name, value) in &parameters.headers {
                    headers.retain(|(key, _)| !key.eq_ignore_ascii_case(name));
                    headers.push((name.clone(), value.clone()));
                }
                if let Some(retained) = &location_headers {
                    headers.clone_from(retained);
                }
                let start_time = std::time::SystemTime::now();
                let start = std::time::Instant::now();
                let client = rustdb_function::file_system::opener()
                    .or_else(|| self.client.as_ref().and_then(std::sync::Weak::upgrade));
                if let Some(client) = &client {
                    client.record_http_request(request_method, request_payload.len() as u64);
                }
                let result = if request_method == "HEAD" {
                    let mut request = agent.head(url);
                    for (name, value) in &headers {
                        request = request.header(name.as_str(), value.as_str());
                    }
                    request.call()
                } else if request_method == "POST" || request_method == "PUT" {
                    let mut request = if request_method == "POST" {
                        agent.post(url)
                    } else {
                        agent.put(url)
                    };
                    for (name, value) in &headers {
                        request = request.header(name.as_str(), value.as_str());
                    }
                    request.send(request_payload)
                } else if request_method == "DELETE" {
                    let mut request = agent.delete(url);
                    for (name, value) in &headers {
                        request = request.header(name.as_str(), value.as_str());
                    }
                    request.call()
                } else {
                    let mut request = agent.get(url);
                    for (name, value) in &headers {
                        request = request.header(name.as_str(), value.as_str());
                    }
                    if let Some((start, end)) = range {
                        request = request.header("Range", format!("bytes={start}-{end}"));
                    }
                    request.call()
                };
                if let Some(logger) = &self.logger {
                    let mut logged_headers = headers.clone();
                    if request_method == "GET"
                        && let Some((start, end)) = range
                    {
                        logged_headers.push(("Range".to_owned(), format!("bytes={start}-{end}")));
                    }
                    logger.record(
                        request_method,
                        url,
                        &logged_headers,
                        request_payload.len(),
                        start_time,
                        start,
                        &result,
                    );
                }
                let result = result.map(|response| {
                    response.map(|body| {
                        body::Body::new(
                            body,
                            client
                                .as_ref()
                                .filter(|_| matches!(request_method, "GET" | "POST")),
                        )
                    })
                });
                match result {
                    Ok(mut response) => {
                        let status = response.status().as_u16();
                        let mut auth_failure = matches!(status, 401 | 403);
                        if signing.is_some() && status == 400 {
                            let mut bytes = Vec::new();
                            response
                                .body_mut()
                                .as_reader()
                                .read_to_end(&mut bytes)
                                .map_err(io_error)?;
                            auth_failure = s3::optional_tag(&String::from_utf8_lossy(&bytes), "Code")
                                == Some("ExpiredToken");
                            let (parts, _) = response.into_parts();
                            response = ureq::http::Response::from_parts(
                                parts,
                                body::Body::new(ureq::Body::builder().data(bytes), None),
                            );
                        }
                        if auth_failure
                            && !refreshed_auth
                            && location_url.is_none()
                            && let Some(attempted) = &signing
                            && let Some(source) = &self.signing
                            && rustdb_function::file_system::opener()
                                .and_then(|client| client.settings())
                                .and_then(|settings| {
                                    settings.current_setting("httpfs_enable_credential_refresh").ok()
                                })
                                .and_then(|value| value.as_bool())
                                .unwrap_or(true)
                            && source.try_refresh(method, attempted, parameters, url)?
                        {
                            refreshed_auth = true;
                            continue 'region;
                        }
                        if signing.is_some()
                            && matches!(status, 301 | 302 | 303 | 307 | 308)
                            && let Some(location) = response.headers().get("location")
                            && let Ok(location) = location.to_str()
                            && !location.is_empty()
                        {
                            let limit = if self.parameters.httplib { 20 } else { 30 };
                            if location_count == limit {
                                let reason = if self.parameters.httplib {
                                    "Maximum redirect count exceeded"
                                } else {
                                    "Number of redirects hit maximum amount"
                                };
                                return Err(io_error(format!(
                                    "{reason} error for HTTP {request_method} to '{url}'"
                                )));
                            }
                            let next = redirect::resolve(url, location)?;
                            if redirect::changes_to_get(status, request_method, self.parameters.httplib) {
                                request_method = "GET";
                                request_payload = &[];
                                if self.parameters.httplib {
                                    headers.clear();
                                } else {
                                    headers.retain(|(name, _)| !name.eq_ignore_ascii_case("content-length"));
                                }
                            }
                            if !redirect::same_origin(url, &next) {
                                // Original S3/HTTP-secret material is scoped to
                                // its origin, including nonstandard token headers.
                                headers.clear();
                            }
                            location_url = Some(next);
                            location_headers = Some(headers);
                            location_count += 1;
                            continue 'region;
                        }
                        if let Some(signing) = &signing {
                            if signing.allow_region_redirect
                                && !redirected
                                && matches!(status, 301 | 400)
                                && let Some(region) = response
                                    .headers()
                                    .get("x-amz-bucket-region")
                                    .and_then(|v| v.to_str().ok())
                                && !region.is_empty()
                                && region != signing.request_region()
                            {
                                signing.redirect(region);
                                redirected = true;
                                location_url = None;
                                location_headers = None;
                                location_count = 0;
                                request_method = method;
                                request_payload = payload;
                                continue 'region;
                            }
                            response.extensions_mut().insert(s3::RequestInfo {
                                region: signing.request_region().into(),
                                redirected,
                            });
                        }
                        if !matches!(status, 408 | 418 | 429 | 500 | 503 | 504)
                            || return_final_status && attempt == self.parameters.retries
                        {
                            return Ok(response);
                        }
                        if matches!(request_method, "GET" | "POST") {
                            io::copy(&mut response.body_mut().as_reader(), &mut io::sink())
                                .map_err(io_error)?;
                        }
                        last = response_error(
                            status_error(method, &self.display_url, status),
                            &response,
                            b"",
                            self.parameters.httplib,
                        );
                    }
                    Err(error) => last = request_error(&error, request_method, url, self.parameters.httplib),
                }
            }
            return Err(last);
        }
    }

    fn open(path: &[u8]) -> Result<Self> {
        let url = std::str::from_utf8(path).map_err(io_error)?.to_owned();
        let parameters = Parameters::current(&url)?;
        Self::open_with(url, parameters, None)
    }

    fn open_with(url: String, parameters: Parameters, signing: Option<s3::RequestSigning>) -> Result<Self> {
        let mut file = Self::new_request(url, parameters, signing)?;
        file.initialize_metadata()?;
        Ok(file)
    }

    fn open_with_display(url: String, display_url: String, parameters: Parameters) -> Result<Self> {
        let mut file = Self::new_request(url, parameters, None)?;
        file.display_url = display_url;
        file.initialize_metadata()?;
        Ok(file)
    }

    fn new_request(url: String, parameters: Parameters, signing: Option<s3::RequestSigning>) -> Result<Self> {
        let agent = parameters.agent()?;
        Ok(Self {
            display_url: signing
                .as_ref()
                .map_or_else(|| url.clone(), |s| s.original_path.clone()),
            url,
            size: 0,
            modified: 0,
            etag: String::new(),
            parameters,
            agent,
            signing,
            logger: logging::HttpLogger::current(),
            client: rustdb_function::file_system::opener()
                .as_ref()
                .map(Arc::downgrade),
        })
    }

    fn initialize_metadata(&mut self) -> Result<()> {
        let mut response = self.request("HEAD", None)?;
        let mut method = "HEAD";
        let mut length = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if !response.status().is_success() || length.is_none() {
            method = "GET";
            response = self.request("GET", Some((0, 1)))?;
            length = response
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit_once('/')?.1.parse::<u64>().ok());
            if length.is_none() && response.status().as_u16() == 200 {
                length = response
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok());
                if length.is_none() {
                    length = Some(
                        io::copy(&mut response.body_mut().as_reader(), &mut io::sink()).map_err(io_error)?,
                    );
                }
            }
        }
        if !response.status().is_success() {
            return Err(response_error(
                status_error(method, &self.display_url, response.status().as_u16()),
                &response,
                b"",
                self.parameters.httplib,
            ));
        }
        self.size = length.ok_or_else(|| io_error("HTTP server did not provide a file size"))?;
        if let Some(signing) = &self.signing {
            // Pin from initial metadata (HEAD or its GET fallback), as C++ does.
            // Later data-range responses must not establish or change the pin.
            signing.record_version(response.headers());
        }
        response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .clone_into(&mut self.etag);
        self.modified = response
            .headers()
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| httpdate::parse_http_date(v).ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |time| time.as_secs().saturating_mul(1_000_000) as i64);
        Ok(())
    }
    fn range_response(&self, offset: u64, end: u64) -> Result<ureq::http::Response<body::Body>> {
        let mut response = self.request("GET", Some((offset, end)))?;
        let status = response.status().as_u16();
        if status != 200 && status != 206 {
            io::copy(&mut response.body_mut().as_reader(), &mut io::sink()).map_err(io_error)?;
            return Err(response_error(
                status_error("GET", &self.display_url, status),
                &response,
                b"",
                self.parameters.httplib,
            ));
        }
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if self.parameters.check_etag
            && !self.etag.is_empty()
            && !etag.is_empty()
            && unquote_etag(&self.etag) != unquote_etag(etag)
        {
            return Err(response_error(
                Error::new(
                    ErrorKind::Http,
                    format!(
                        "ETag on reading file \"{}\" was initially {} and now it returned {}, this likely means the remote file has changed.\nFor parquet or similar single table sources, consider retrying the query, for persistent FileHandles such as databases consider `DETACH` and re-`ATTACH` \nYou can disable checking etags via `SET unsafe_disable_etag_checks = true;`",
                        self.display_url, self.etag, etag
                    ),
                ),
                &response,
                b"",
                self.parameters.httplib,
            ));
        }
        if status == 206 {
            let header = response
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| io_error("Range response is missing Content-Range"))?;
            let expected = format!("bytes {offset}-{end}/{}", self.size);
            if header != expected {
                return Err(io_error("HTTP server returned an unexpected Content-Range"));
            }
        } else if !self.parameters.fallback {
            return Err(io_error("HTTP server does not support range requests"));
        }
        Ok(response)
    }
}

impl FileHandle for HttpFile {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize> {
        if offset >= self.size || buffer.is_empty() {
            return Ok(0);
        }
        let count = buffer
            .len()
            .min(usize::try_from(self.size - offset).unwrap_or(usize::MAX));
        let end = offset + count as u64 - 1;
        let mut response = self.range_response(offset, end)?;
        let status = response.status().as_u16();
        let mut body = response.body_mut().as_reader();
        if status == 200 && offset != 0 {
            let skipped = io::copy(&mut (&mut body).take(offset), &mut io::sink()).map_err(io_error)?;
            if skipped != offset {
                return Err(io_error("HTTP server returned a short file"));
            }
        }
        body.read_exact(&mut buffer[..count]).map_err(io_error)?;
        Ok(count)
    }
    fn read_stream(&self) -> Result<Option<Box<dyn Read + Send>>> {
        Ok(Some(Box::new(HttpReadStream {
            file: self.clone(),
            position: 0,
            end: 0,
            window: 1 << 20,
            body: None,
        })))
    }
    fn write_at(&self, _: u64, _: &[u8]) -> Result<usize> {
        Err(Error::not_implemented("Writing to HTTP files not supported"))
    }
    fn size(&self) -> Result<u64> {
        Ok(self.size)
    }
    fn modified(&self) -> Result<i64> {
        Ok(self.modified)
    }
    fn version_tag(&self) -> Result<String> {
        Ok(self.etag.clone())
    }
    fn flush(&self) -> Result<()> {
        Ok(())
    }
}

/// Stream adaptive HTTP range windows without allocating a window-sized cache.
/// CSV's small input buffer consumes each response incrementally. Random-access
/// readers retain the existing exact-range `read_at` behavior.
struct HttpReadStream {
    file: HttpFile,
    position: u64,
    end: u64,
    window: u64,
    body: Option<BodyReader<ureq::BodyReader<'static>>>,
}

impl std::fmt::Debug for HttpReadStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpReadStream")
            .field("position", &self.position)
            .field("end", &self.end)
            .field("window", &self.window)
            .finish_non_exhaustive()
    }
}

impl Read for HttpReadStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.position >= self.file.size {
            return Ok(0);
        }
        if self.position == self.end {
            self.body = None;
            let end = self.position.saturating_add(self.window).min(self.file.size);
            let response = self
                .file
                .range_response(self.position, end - 1)
                .map_err(io::Error::other)?;
            let full_body = response.status().as_u16() == 200;
            let mut body = response.into_body().into_reader();
            if full_body && self.position != 0 {
                let skipped = io::copy(&mut (&mut body).take(self.position), &mut io::sink())?;
                if skipped != self.position {
                    return Err(io::Error::other("HTTP server returned a short file"));
                }
            }
            // A server ignoring Range already sent the whole file. Keep reading
            // that response rather than downloading and skipping its prefix again.
            self.end = if full_body { self.file.size } else { end };
            self.window = self.window.saturating_mul(2).min(32 << 20);
            self.body = Some(body);
        }
        let count = buffer
            .len()
            .min(usize::try_from(self.end - self.position).unwrap_or(usize::MAX));
        let body = self
            .body
            .as_mut()
            .ok_or_else(|| io::Error::other("Missing HTTP response body"))?;
        let read = body.read(&mut buffer[..count])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "failed to fill whole buffer",
            ));
        }
        self.position += read as u64;
        Ok(read)
    }
}

/// Register one instance per database with the existing filesystem registry.
#[derive(Debug, Default)]
pub struct HttpFileSystem;

impl FileSystem for HttpFileSystem {
    fn name(&self) -> &'static str {
        "HTTPFileSystem"
    }
    fn is_remote(&self) -> bool {
        true
    }
    fn handles(&self, path: &[u8]) -> bool {
        path.starts_with(b"http://") || path.starts_with(b"https://")
    }
    fn open(&self, path: &[u8], write: bool) -> Result<Arc<dyn FileHandle>> {
        if write {
            return Err(Error::not_implemented("Writing to HTTP files not supported"));
        }
        HttpFile::open(path).map(|file| Arc::new(file) as Arc<dyn FileHandle>)
    }
    fn glob(&self, path: &[u8]) -> Result<Vec<ByteString>> {
        let allow = rustdb_function::file_system::opener()
            .and_then(|c| c.settings())
            .and_then(|s| s.current_setting("allow_asterisks_in_http_paths").ok())
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if path.contains(&b'*') && !allow {
            return Err(io_error("Globs (wildcards) in HTTP paths are not supported"));
        }
        Ok(vec![path.into()])
    }
    fn is_file(&self, path: &[u8]) -> Result<bool> {
        match HttpFile::open(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == ErrorKind::Http && error.raw_message().contains("HTTP 404") => {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
    fn is_dir(&self, _: &[u8]) -> Result<bool> {
        Ok(false)
    }
    fn mkdir(&self, _: &[u8]) -> Result<()> {
        Err(Error::not_implemented("HTTP directories are not supported"))
    }
    fn remove(&self, _: &[u8]) -> Result<()> {
        Err(Error::not_implemented("Removing HTTP files is not supported"))
    }
    fn rename(&self, _: &[u8], _: &[u8]) -> Result<()> {
        Err(Error::not_implemented("Moving HTTP files is not supported"))
    }
}

#[cfg(test)]
mod http_metadata_tests;

mod response_header_order;
