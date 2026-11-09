//! S3 positional reads and AWS Signature Version 4, following pinned httpfs 4bc690d.
//! Credentials are selected per open; this module never caches object contents.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Read as _;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use ring::{digest, hmac};
use roastedduck_common::{ByteString, Error, ErrorKind, Result};
use roastedduck_function::ExtensionOption;
use roastedduck_function::registered_files::{FileHandle, FileSystem, GlobResult};
use roastedduck_types::{LogicalType, Timestamp, Value};

use crate::{HttpFile, Parameters, io_error};

mod delete;
mod write;

pub const SECRET_RESOLVER_KEY: &str = "roastedduck.s3_secret_resolver";

/// The engine owns secret lookup and persistence; the transport only receives the
/// selected map. Debug output deliberately excludes credential values.
pub trait ResolveS3Secret: std::fmt::Debug + Send + Sync {
    fn resolve(&self, path: &str) -> Result<Option<BTreeMap<String, Value>>>;
    fn refresh(&self, _path: &str) -> Result<bool> {
        Ok(false)
    }
    /// Selected secret identities keep batches separate even if credentials match.
    fn batch_identity(&self, _path: &str) -> Result<Vec<Vec<u8>>> {
        Ok(Vec::new())
    }
}

#[derive(Debug)]
pub struct SecretResolver(pub Arc<dyn ResolveS3Secret>);

pub fn options() -> Vec<ExtensionOption> {
    let mut result = Vec::new();
    for (name, description) in [
        ("s3_region", "S3 Region"),
        ("s3_access_key_id", "S3 Access Key ID"),
        ("s3_secret_access_key", "S3 Access Key"),
        ("s3_session_token", "S3 Session Token"),
        ("s3_endpoint", "S3 Endpoint"),
        ("s3_kms_key_id", "S3 KMS Key ID"),
    ] {
        result.push(ExtensionOption {
            name: name.into(),
            description: description.into(),
            input_type: LogicalType::VARCHAR,
            default_value: Value::null(LogicalType::VARCHAR),
            set_function: None,
            global_by_default: false,
        });
    }
    for (name, description, default_value) in [
        ("s3_url_style", "S3 URL style", Value::varchar("vhost")),
        ("s3_use_ssl", "S3 use SSL", Value::boolean(true)),
        (
            "s3_url_compatibility_mode",
            "Disable Globs and Query Parameters on S3 URLs",
            Value::boolean(false),
        ),
        (
            "s3_requester_pays",
            "S3 use requester pays mode",
            Value::boolean(false),
        ),
        (
            "merge_http_secret_into_s3_request",
            "Merges http secret params into S3 requests",
            Value::boolean(true),
        ),
        (
            "enable_global_s3_configuration",
            "Automatically fetch AWS credentials from environment variables.",
            Value::boolean(true),
        ),
        (
            "s3_allow_recursive_globbing",
            "Whether globs on S3-like storage are optimized with recursive strategy (alterative is listing)",
            Value::boolean(true),
        ),
        (
            "s3_uploader_max_filesize",
            "S3 Uploader max filesize (between 50GB and 5TB)",
            Value::varchar("800GB"),
        ),
        (
            "s3_uploader_max_parts_per_file",
            "S3 Uploader max parts per file (between 1 and 10000)",
            Value::ubigint(10000),
        ),
        (
            "s3_uploader_thread_limit",
            "S3 Uploader global thread limit",
            Value::ubigint(50),
        ),
        (
            "s3_version_id_pinning",
            "Pin S3 reads to a specific object version for consistency",
            Value::boolean(false),
        ),
        (
            "httpfs_enable_credential_refresh",
            "Refresh S3 authentication material once after an authentication failure",
            Value::boolean(true),
        ),
    ] {
        result.push(ExtensionOption {
            name: name.into(),
            description: description.into(),
            input_type: default_value.logical_type().clone(),
            default_value,
            set_function: None,
            global_by_default: false,
        });
    }
    result
}

#[derive(Clone, Default)]
// Independent request options, rather than mutually exclusive states.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct RequestSigning {
    region: String,
    key_id: String,
    secret: String,
    token: String,
    bearer: String,
    host: String,
    path: String,
    query: String,
    requester_pays: bool,
    kms_key_id: String,
    content_type: String,
    content_md5: String,
    aws_endpoint: bool,
    vhost_bucket: String,
    pub(crate) bucket_request: bool,
    pub(crate) pin_version: bool,
    pub(crate) allow_region_redirect: bool,
    state: Arc<Mutex<RequestState>>,
    pub(crate) original_path: String,
}

#[derive(Default)]
struct RequestState {
    region: Option<String>,
    version: String,
    // The replacement owns a separate empty request state, avoiding a cycle.
    refreshed: Option<Arc<HttpFile>>,
}

#[derive(Clone, Debug)]
pub(crate) struct RequestInfo {
    pub region: String,
    pub redirected: bool,
}

impl std::fmt::Debug for RequestSigning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3RequestSigning").finish_non_exhaustive()
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        result.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
    }
    result
}

fn sha256(bytes: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, bytes).as_ref())
}

fn sign(key: &[u8], value: &str) -> hmac::Tag {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), value.as_bytes())
}

/// S3 escapes bytes rather than Unicode code points and retains slashes in keys.
fn encode(value: &str, slash: bool) -> String {
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) || byte == b'/' && !slash {
            result.push(char::from(byte));
        } else {
            result.push('%');
            result.push(char::from(b"0123456789ABCDEF"[usize::from(byte >> 4)]));
            result.push(char::from(b"0123456789ABCDEF"[usize::from(byte & 15)]));
        }
    }
    result
}

fn decode(value: &str) -> Result<String> {
    let mut bytes = Vec::new();
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        match byte {
            b'%' => {
                let a = input.next().and_then(|b| char::from(b).to_digit(16));
                let b = input.next().and_then(|b| char::from(b).to_digit(16));
                let (Some(a), Some(b)) = (a, b) else {
                    return Err(io_error("Invalid URL escape"));
                };
                bytes.push((a * 16 + b) as u8);
            }
            b'+' => bytes.push(b' '),
            _ => bytes.push(byte),
        }
    }
    String::from_utf8(bytes).map_err(io_error)
}

impl RequestSigning {
    pub(crate) fn effective(&self, method: &str) -> Self {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut signing = state
            .refreshed
            .as_ref()
            .and_then(|file| file.signing.as_ref())
            .unwrap_or(self)
            .clone();
        signing.state = Arc::clone(&self.state);
        signing.query.clone_from(&self.query);
        signing.content_type.clone_from(&self.content_type);
        signing.content_md5.clone_from(&self.content_md5);
        signing.pin_version = self.pin_version;
        signing.allow_region_redirect = self.allow_region_redirect;
        Self::apply_state(&mut signing, method, &state);
        signing
    }

    fn apply_state(signing: &mut Self, method: &str, state: &RequestState) {
        if let Some(region) = &state.region {
            signing.region.clone_from(region);
            if signing.aws_endpoint {
                signing.host = if signing.vhost_bucket.is_empty() {
                    format!("s3.{region}.amazonaws.com")
                } else {
                    format!("{}.s3.{region}.amazonaws.com", signing.vhost_bucket)
                };
            }
        }
        if signing.pin_version && method == "GET" && !state.version.is_empty() {
            signing.query = format!("versionId={}", encode(&state.version, true));
        }
    }

    pub(crate) fn refreshed_transport(&self) -> Option<Arc<HttpFile>> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .refreshed
            .clone()
    }

    fn same_material(&self, other: &Self) -> bool {
        self.region == other.region
            && self.key_id == other.key_id
            && self.secret == other.secret
            && self.token == other.token
            && self.bearer == other.bearer
            && self.host == other.host
            && self.path == other.path
            && self.requester_pays == other.requester_pays
            && self.kms_key_id == other.kms_key_id
            && self.aws_endpoint == other.aws_endpoint
            && self.vhost_bucket == other.vhost_bucket
    }

    pub(crate) fn try_refresh(
        &self,
        method: &str,
        attempted: &Self,
        parameters: &Parameters,
        url: &str,
    ) -> Result<bool> {
        // Reloading without an opener would replace connection-scoped secrets
        // and endpoints with defaults. C++ likewise declines refresh without one.
        let Some(client) = roastedduck_function::file_system::opener() else {
            return Ok(false);
        };
        let prepare = || -> Result<HttpFile> {
            let mut request = S3FileSystem::prepare(self.original_path.as_bytes(), false)?;
            if self.bucket_request {
                request
                    .signing
                    .path
                    .truncate(request.signing.path.len() - request.key.len());
                request.signing.bucket_request = true;
            }
            HttpFile::new_request(request.url, request.parameters, Some(request.signing))
        };
        let changed = |candidate: &HttpFile| {
            let Some(signing) = &candidate.signing else {
                return false;
            };
            let mut signing = signing.clone();
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            Self::apply_state(&mut signing, method, &state);
            !signing.same_material(attempted)
                || !candidate.parameters.same_refresh_material(parameters)
                || candidate.url.split_once("://").map(|(scheme, _)| scheme)
                    != url.split_once("://").map(|(scheme, _)| scheme)
        };
        let mut candidate = prepare()?;
        if !changed(&candidate) {
            let resolver = client
                .object_cache()
                .and_then(|cache| cache.get::<SecretResolver>(SECRET_RESOLVER_KEY));
            let Some(resolver) = resolver else {
                return Ok(false);
            };
            if !resolver.0.refresh(&self.original_path)? {
                return Ok(false);
            }
            candidate = prepare()?;
            if !changed(&candidate) {
                return Ok(false);
            }
        }
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .refreshed = Some(Arc::new(candidate));
        Ok(true)
    }
    pub(crate) fn request_region(&self) -> &str {
        &self.region
    }
    pub(crate) fn redirect(&self, region: &str) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).region = Some(region.into());
    }
    pub(crate) fn record_version(&self, headers: &ureq::http::HeaderMap) {
        if !self.pin_version {
            return;
        }
        if let Some(version) = headers.get("x-amz-version-id").and_then(|v| v.to_str().ok()) {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.version.is_empty() {
                version.clone_into(&mut state.version);
            }
        }
    }
    pub(crate) fn request_url(&self, template: &str, method: &str) -> String {
        let protocol = template
            .split_once("://")
            .map_or("https", |(protocol, _)| protocol);
        let query = if self.pin_version && method == "GET" && self.query.starts_with("versionId=") {
            self.query.as_str()
        } else {
            template.split_once('?').map_or("", |(_, query)| query)
        };
        let mut url = format!("{protocol}://{}{}", self.host, encode(&self.path, false));
        if !query.is_empty() {
            url.push('?');
            url.push_str(query);
        }
        url
    }
    pub(crate) fn headers(&self, method: &str, body: &[u8]) -> Result<Vec<(String, String)>> {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io_error)?
            .as_secs();
        let timestamp =
            Timestamp::try_from_epoch_seconds(i64::try_from(seconds).map_err(io_error)?)?.to_string();
        let date = timestamp[..10].replace('-', "");
        let time = timestamp[11..19].replace(':', "");
        Ok(self.headers_at(method, &date, &format!("{date}T{time}Z"), body))
    }

    fn headers_at(&self, method: &str, date: &str, datetime: &str, body: &[u8]) -> Vec<(String, String)> {
        let mut headers = BTreeMap::new();
        headers.insert("host", self.host.clone());
        if !self.content_md5.is_empty() {
            headers.insert("content-md5", self.content_md5.clone());
        }
        if !self.content_type.is_empty() {
            headers.insert("content-type", self.content_type.clone());
        }
        if !self.bearer.is_empty() {
            headers.insert("Authorization", format!("Bearer {}", self.bearer));
        } else if !self.key_id.is_empty() || !self.secret.is_empty() {
            let payload = sha256(body);
            headers.insert("x-amz-content-sha256", payload.clone());
            headers.insert("x-amz-date", datetime.into());
            if self.requester_pays {
                headers.insert("x-amz-request-payer", "requester".into());
            }
            if !self.token.is_empty() {
                headers.insert("x-amz-security-token", self.token.clone());
            }
            if !self.kms_key_id.is_empty()
                && matches!(method, "POST" | "PUT")
                && !self.query.contains("uploadId")
            {
                headers.insert("x-amz-server-side-encryption", "aws:kms".into());
                headers.insert(
                    "x-amz-server-side-encryption-aws-kms-key-id",
                    self.kms_key_id.clone(),
                );
            }
            let names = headers.keys().copied().collect::<Vec<_>>().join(";");
            let mut canonical_headers = String::new();
            for (name, value) in &headers {
                canonical_headers.push_str(name);
                canonical_headers.push(':');
                canonical_headers.push_str(value);
                canonical_headers.push('\n');
            }
            let canonical_request = format!(
                "{method}\n{}\n{}\n{canonical_headers}\n{names}\n{payload}",
                encode(&self.path, false),
                self.query
            );
            let scope = format!("{date}/{}/s3/aws4_request", self.region);
            let to_sign = format!(
                "AWS4-HMAC-SHA256\n{datetime}\n{scope}\n{}",
                sha256(canonical_request.as_bytes())
            );
            let date_key = sign(format!("AWS4{}", self.secret).as_bytes(), date);
            let region_key = sign(date_key.as_ref(), &self.region);
            let service_key = sign(region_key.as_ref(), "s3");
            let signing_key = sign(service_key.as_ref(), "aws4_request");
            let signature = sign(signing_key.as_ref(), &to_sign);
            headers.insert(
                "Authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={names}, Signature={}",
                    self.key_id,
                    hex(signature.as_ref())
                ),
            );
        }
        headers.into_iter().map(|(k, v)| (k.into(), v)).collect()
    }
}

fn setting(name: &str) -> Option<Value> {
    roastedduck_function::file_system::opener()?
        .settings()?
        .current_setting(name)
        .ok()
        .filter(|v| !v.is_null())
}

fn secret(path: &str) -> Result<Option<BTreeMap<String, Value>>> {
    roastedduck_function::file_system::opener()
        .and_then(|c| c.object_cache())
        .and_then(|c| c.get::<SecretResolver>(SECRET_RESOLVER_KEY))
        .map_or_else(|| Ok(None), |r| r.0.resolve(path))
}

/// One instance is registered in the database-local registry beside HTTP(S).
#[derive(Debug)]
pub struct S3FileSystem {
    manager: roastedduck_buffer::BufferManager,
}

struct S3Request {
    url: String,
    parameters: Parameters,
    signing: RequestSigning,
    key: String,
    bucket_url: String,
    query_suffix: String,
    compatibility: bool,
}

#[derive(Default)]
struct ListingPage {
    keys: Vec<String>,
    prefixes: Vec<String>,
    next: String,
}

/// The pinned implementation parses these exact S3 XML tags. Keys are requested
/// with encoding-type=url, so their contents are percent-encoded UTF-8.
fn tag<'a>(xml: &'a str, name: &str, offset: usize) -> Result<Option<(&'a str, usize)>> {
    let start_tag = format!("<{name}>");
    let Some(start) = xml[offset..]
        .find(&start_tag)
        .map(|p| offset + p + start_tag.len())
    else {
        return Ok(None);
    };
    let end_tag = format!("</{name}>");
    let end = xml[start..].find(&end_tag).map(|p| start + p).ok_or_else(|| {
        Error::internal(format!(
            "Failed to parse S3 result: found open tag for {name} but did not find matching close tag"
        ))
    })?;
    Ok(Some((&xml[start..end], end + end_tag.len())))
}

impl ListingPage {
    fn parse(xml: &str) -> Result<Self> {
        let mut result = Self::default();
        let mut offset = 0;
        while let Some((contents, next)) = tag(xml, "Contents", offset)? {
            offset = next;
            let (key, _) = tag(contents, "Key", 0)?
                .ok_or_else(|| Error::internal(format!("Key not found in S3 response: {contents}")))?;
            let key = decode(key)?;
            if !key.ends_with('/') {
                result.keys.push(key);
            }
        }
        offset = 0;
        while let Some((contents, next)) = tag(xml, "CommonPrefixes", offset)? {
            offset = next;
            let (prefix, _) = tag(contents, "Prefix", 0)?
                .ok_or_else(|| Error::internal("Parsing error while parsing s3 listobject result"))?;
            result.prefixes.push(prefix.into());
        }
        result.next =
            tag(xml, "NextContinuationToken", 0)?.map_or_else(String::new, |(token, _)| token.into());
        Ok(result)
    }
}

/// `Match` from s3fs.cpp: ordinary patterns match one path component; a component
/// equal to ** spans any number of components. Partial matches prune traversal.
pub(super) fn path_matches(key: &[&str], pattern: &[&str], completed: bool) -> bool {
    if key.is_empty() && !completed {
        return true;
    }
    let (mut k, mut p) = (0, 0);
    while k < key.len() && p < pattern.len() {
        if pattern[p] == "**" {
            if p + 1 == pattern.len() {
                return true;
            }
            return (k..key.len()).any(|start| path_matches(&key[start..], &pattern[p + 1..], completed))
                || !completed;
        }
        if !roastedduck_common::glob::glob(key[k].as_bytes(), pattern[p].as_bytes()) {
            return false;
        }
        k += 1;
        p += 1;
    }
    if p != pattern.len() && !completed {
        return true;
    }
    k == key.len() && p == pattern.len()
}

// Error bodies may be truncated XML. Missing closing tags must not become an
// InternalException or invalidate the database (TryFindTagContents in C++).
pub(crate) fn optional_tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let opening = format!("<{name}>");
    let start = xml.find(&opening)? + opening.len();
    let end = xml[start..].find(&format!("</{name}>"))? + start;
    Some(&xml[start..end])
}

fn error_details(body: &str) -> String {
    let mut extra = String::new();
    if let Some(error) = optional_tag(body, "Error")
        && let (Some(code), Some(message)) = (optional_tag(error, "Code"), optional_tag(error, "Message"))
    {
        let _ = write!(extra, "\n\n{code}: {message}");
        if code == "InvalidAccessKeyId"
            && let Some(key) = optional_tag(error, "AWSAccessKeyId")
        {
            let _ = write!(extra, "\nInvalid Access Key: \"{key}\"");
        }
    }
    extra
}

fn listing_error(signing: &RequestSigning, status: u16, body: &str, url: &str) -> Error {
    let mut extra = error_details(body);
    if status == 400 {
        extra.push_str("\n\nBad Request - this can be caused by the S3 region being set incorrectly.");
        if signing.region.is_empty() {
            extra.push_str("\n* No region is provided.");
        } else {
            let _ = write!(extra, "\n* Provided region is: \"{}\"", signing.region);
        }
    }
    if status == 403 {
        extra.push_str(
            "\n\nAuthentication Failure - this is usually caused by invalid or missing credentials.",
        );
        extra.push_str(if signing.key_id.is_empty() && signing.secret.is_empty() {
            "\n* No credentials are provided."
        } else {
            "\n* Credentials are provided, but they did not work."
        });
        extra.push_str("\n* See https://duckdb.org/docs/stable/extensions/httpfs/s3api.html");
    }
    let reason = ureq::http::StatusCode::from_u16(status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or_default();
    Error::new(
        ErrorKind::Http,
        format!(
            "HTTP GET error reading '{url}' in region '{}' (HTTP {status} {reason}){extra}",
            signing.region
        ),
    )
}

impl S3Request {
    fn list_page(&self, prefix: &str, continuation: &str, delimiter: bool) -> Result<ListingPage> {
        let mut signing = self.signing.clone();
        signing.path.truncate(signing.path.len() - self.key.len());
        signing.bucket_request = true;
        let mut query = BTreeMap::new();
        if !continuation.is_empty() {
            query.insert("continuation-token", encode(continuation, true));
        }
        if delimiter {
            query.insert("delimiter", "%2F".into());
        }
        query.insert("encoding-type", "url".into());
        query.insert("list-type", "2".into());
        query.insert("prefix", encode(prefix, true));
        signing.query = query
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let protocol = self
            .url
            .split_once("://")
            .map_or("https", |(protocol, _)| protocol);
        let url = format!(
            "{protocol}://{}{}?{}",
            signing.host,
            encode(&signing.path, false),
            signing.query
        );
        let request = HttpFile::new_request(url, self.parameters.clone(), Some(signing.clone()))?;
        let mut response = request.request("GET", None)?;
        let status = response.status();
        let info = response.extensions().get::<RequestInfo>().cloned();
        if let Some(info) = &info {
            signing.region.clone_from(&info.region);
        }
        if status.as_u16() == 301 {
            let suggested = response
                .headers()
                .get("x-amz-bucket-region")
                .and_then(|v| v.to_str().ok());
            let reason = if info.as_ref().is_none_or(|i| !i.redirected) && suggested.is_some() {
                if suggested == Some(signing.region.as_str()) {
                    format!(
                        "suggested region \"{}\" is the same as the region we used to make the request",
                        signing.region
                    )
                } else {
                    String::new()
                }
            } else {
                "HTTP response did not contain header_x-amz-bucket-region".into()
            };
            if !reason.is_empty() {
                let mut body = Vec::new();
                response
                    .body_mut()
                    .as_reader()
                    .read_to_end(&mut body)
                    .map_err(io_error)?;
                return Err(crate::response_error(
                    Error::new(
                        ErrorKind::Http,
                        format!(
                            "HTTP 301 response when running glob \"{}{prefix}\" but {reason}",
                            self.bucket_url
                        ),
                    ),
                    &response,
                    &body,
                    self.parameters.httplib,
                ));
            }
        }
        let mut xml = String::new();
        response
            .body_mut()
            .as_reader()
            .read_to_string(&mut xml)
            .map_err(io_error)?;
        if !status.is_success() {
            let source = format!("{}{prefix}", self.bucket_url);
            return Err(crate::response_error(
                listing_error(&signing, status.as_u16(), &xml, source.trim_end_matches('/')),
                &response,
                xml.as_bytes(),
                self.parameters.httplib,
            ));
        }
        ListingPage::parse(&xml)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GlobStrategy {
    Unknown,
    Listing,
    Hierarchical,
}

struct S3Glob {
    request: S3Request,
    shared: String,
    main_token: String,
    prefix_token: String,
    current_prefix: String,
    prefixes: Vec<String>,
    strategy: GlobStrategy,
    investigate: bool,
    finished: bool,
}

impl std::fmt::Debug for S3Glob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Glob").finish_non_exhaustive()
    }
}

impl S3Glob {
    fn new(request: S3Request, first_wildcard: usize) -> Self {
        let shared = request.key[..first_wildcard].into();
        let investigate = !request.key.contains("**")
            && setting("s3_allow_recursive_globbing")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
        Self {
            request,
            shared,
            main_token: String::new(),
            prefix_token: String::new(),
            current_prefix: String::new(),
            prefixes: Vec::new(),
            strategy: GlobStrategy::Unknown,
            investigate,
            finished: false,
        }
    }
}

impl GlobResult for S3Glob {
    fn expand_next(&mut self) -> Result<Option<Vec<ByteString>>> {
        if self.finished {
            return Ok(None);
        }
        let pattern: Vec<_> = self.request.key.split_terminator('/').collect();
        let page = if self.current_prefix.is_empty() {
            let mut page = self.request.list_page(
                &self.shared,
                &self.main_token,
                self.strategy == GlobStrategy::Hierarchical,
            )?;
            if self.investigate && self.strategy == GlobStrategy::Unknown && !page.next.is_empty() {
                let folders: BTreeSet<_> = page
                    .keys
                    .iter()
                    .map(|key| key.rsplit_once('/').map_or("", |(folder, _)| folder))
                    .collect();
                if folders.len().saturating_mul(100) < page.keys.len() {
                    page = self.request.list_page(&self.shared, &self.main_token, true)?;
                    self.strategy = GlobStrategy::Hierarchical;
                } else {
                    self.strategy = GlobStrategy::Listing;
                }
            }
            self.main_token.clone_from(&page.next);
            self.prefixes.clone_from(&page.prefixes);
            self.current_prefix = self.prefixes.pop().unwrap_or_default();
            page
        } else {
            let decoded = decode(&self.current_prefix)?;
            let components: Vec<_> = decoded.split_terminator('/').collect();
            let page = if path_matches(&components, &pattern, false) {
                self.request.list_page(&decoded, &self.prefix_token, true)?
            } else {
                ListingPage::default()
            };
            self.prefixes.extend(page.prefixes.iter().cloned());
            self.prefix_token.clone_from(&page.next);
            if self.prefix_token.is_empty() {
                self.current_prefix = self.prefixes.pop().unwrap_or_default();
            }
            page
        };
        let mut result = Vec::new();
        for key in page.keys {
            let components: Vec<_> = key.split_terminator('/').collect();
            if path_matches(&components, &pattern, true) {
                result.push(format!("{}{key}{}", self.request.bucket_url, self.request.query_suffix).into());
            }
        }
        self.finished = self.main_token.is_empty() && self.current_prefix.is_empty();
        Ok(Some(result))
    }
}

impl S3FileSystem {
    pub fn new(manager: roastedduck_buffer::BufferManager) -> Self {
        Self { manager }
    }
    fn prepare(path: &[u8], for_glob: bool) -> Result<S3Request> {
        let original = std::str::from_utf8(path).map_err(io_error)?;
        let selected = secret(original)?;
        let has_secret = selected.is_some();
        let secrets = selected.unwrap_or_default();
        let globals = setting("enable_global_s3_configuration")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let text = |key: &str, option: &str, fallback: &str| -> Result<String> {
            secrets
                .get(key)
                .cloned()
                .or_else(|| {
                    let settings = roastedduck_function::file_system::opener()?.settings()?;
                    let (value, scope) = settings.current_setting_scope(option)?;
                    (globals || scope != roastedduck_function::SettingScope::Global).then_some(value)
                })
                .filter(|v| !v.is_null())
                .map_or_else(|| Ok(fallback.into()), |v| v.to_varchar())
        };
        let compatibility =
            text("s3_url_compatibility_mode", "s3_url_compatibility_mode", "false")? == "true";
        let (location, query) = if compatibility {
            (original, "")
        } else {
            original.split_once('?').unwrap_or((original, ""))
        };
        let (scheme, rest) = location
            .split_once("://")
            .ok_or_else(|| io_error("URL needs to start with s3://, gcs:// or r2://"))?;
        let (bucket, key) = rest
            .split_once('/')
            .ok_or_else(|| io_error("URL needs to contain a '/' after the host"))?;
        if bucket.is_empty() {
            return Err(io_error("URL needs to contain a bucket name"));
        }
        let mut params = BTreeMap::new();
        // Glob returns literal paths without interpreting their query options.
        // Only expansion and actual file opens consume those options in C++.
        if !for_glob || !compatibility && key.bytes().any(|b| b"*[\\".contains(&b)) {
            for entry in query.split('&').filter(|v| !v.is_empty()) {
                let (name, value) = entry.split_once('=').unwrap_or((entry, ""));
                params.insert(decode(name)?, decode(value)?);
            }
        }
        let mut value = |key: &str, option: &str, fallback: &str| -> Result<String> {
            params
                .remove(option)
                .map_or_else(|| text(key, option, fallback), Ok)
        };
        let mut signing = RequestSigning {
            allow_region_redirect: true,
            original_path: original.into(),
            region: value("region", "s3_region", "")?,
            key_id: value("key_id", "s3_access_key_id", "")?,
            secret: value("secret", "s3_secret_access_key", "")?,
            token: value("session_token", "s3_session_token", "")?,
            bearer: secrets
                .get("bearer_token")
                .map(Value::to_varchar)
                .transpose()?
                .unwrap_or_default(),
            kms_key_id: text("kms_key_id", "s3_kms_key_id", "")?,
            ..Default::default()
        };
        let mut endpoint = value("endpoint", "s3_endpoint", "")?;
        let mut style = value("url_style", "s3_url_style", "vhost")?;
        let ssl = value("use_ssl", "s3_use_ssl", "true")?;
        let requester = value("requester_pays", "s3_requester_pays", "false")?;
        for (name, v) in [("s3_use_ssl", &ssl), ("s3_requester_pays", &requester)] {
            if !matches!(v.as_str(), "true" | "false") {
                return Err(io_error(format!(
                    "Incorrect setting found for {name}, allowed values are: 'true' or 'false'"
                )));
            }
        }
        signing.requester_pays = requester == "true";
        if !params.is_empty() {
            return Err(io_error(
                "Invalid query parameters found. Supported parameters are:\n's3_region', 's3_access_key_id', 's3_secret_access_key', 's3_session_token',\n's3_endpoint', 's3_url_style', 's3_use_ssl', 's3_requester_pays'",
            ));
        }
        if matches!(scheme, "gcs" | "gs") {
            if endpoint.is_empty() || !secrets.contains_key("endpoint") {
                endpoint = "storage.googleapis.com".into();
            }
            if style.is_empty() || !secrets.contains_key("url_style") {
                style = "path".into();
            }
        }
        if endpoint.is_empty() || endpoint.starts_with("s3.") && endpoint.ends_with(".amazonaws.com") {
            signing.aws_endpoint = true;
            if signing.region.is_empty() && !signing.key_id.is_empty() {
                signing.region = "us-east-1".into();
            }
            endpoint = if signing.region.is_empty() {
                "s3.amazonaws.com".into()
            } else {
                format!("s3.{}.amazonaws.com", signing.region)
            };
        }
        let (host, base) = endpoint
            .split_once('/')
            .map_or((endpoint.as_str(), String::new()), |(h, p)| (h, format!("/{p}")));
        signing.host = if matches!(style.as_str(), "vhost" | "virtual" | "") {
            signing.vhost_bucket = bucket.into();
            format!("{bucket}.{host}")
        } else {
            host.into()
        };
        signing.path = if style == "path" {
            format!("{base}/{bucket}/{key}")
        } else {
            format!("{base}/{key}")
        };
        let url = format!(
            "{}://{}{}",
            if ssl == "true" { "https" } else { "http" },
            signing.host,
            encode(&signing.path, false)
        );
        let merge_http = setting("merge_http_secret_into_s3_request")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let mut http = Parameters::current_with_http_secret(original, !has_secret && merge_http, false)?;
        http.fallback = false;
        // Redirects need a new signature. Never forward signed headers or the
        // session token through the generic HTTP redirect machinery.
        http.redirects = 0;
        if let Some(verify) = secrets.get("verify_ssl").and_then(Value::as_bool) {
            http.verify = verify;
        }
        for (key, target) in [
            ("http_proxy", &mut http.proxy),
            ("http_proxy_username", &mut http.proxy_user),
            ("http_proxy_password", &mut http.proxy_password),
        ] {
            if let Some(value) = secrets.get(key).filter(|v| !v.is_null()) {
                let text = value.to_varchar()?;
                if key != "http_proxy" || !text.is_empty() {
                    *target = text;
                }
            }
        }
        if let Some(headers) = secrets.get("extra_http_headers").and_then(Value::children) {
            for pair in headers {
                let Some([key, value]) = pair.children() else {
                    return Err(Error::internal("S3 HTTP header must be a key/value pair"));
                };
                http.headers.push((key.to_varchar()?, value.to_varchar()?));
            }
        }
        Ok(S3Request {
            url,
            parameters: http,
            signing,
            key: key.into(),
            bucket_url: format!("{scheme}://{bucket}/"),
            query_suffix: if query.is_empty() {
                String::new()
            } else {
                format!("?{query}")
            },
            compatibility,
        })
    }

    fn open_file(path: &[u8]) -> Result<HttpFile> {
        let mut request = Self::prepare(path, false)?;
        request.signing.pin_version = setting("s3_version_id_pinning")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        HttpFile::open_with(request.url, request.parameters, Some(request.signing))
    }
}

impl FileSystem for S3FileSystem {
    fn name(&self) -> &'static str {
        "S3FileSystem"
    }
    fn is_remote(&self) -> bool {
        true
    }
    fn handles(&self, path: &[u8]) -> bool {
        [
            b"s3://".as_slice(),
            b"s3a://",
            b"s3n://",
            b"r2://",
            b"gcs://",
            b"gs://",
        ]
        .iter()
        .any(|prefix| {
            path.get(..prefix.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
        })
    }
    fn open(&self, path: &[u8], write: bool) -> Result<Arc<dyn FileHandle>> {
        if write {
            return write::S3WriteFile::open(Self::prepare(path, false)?, self.manager.clone())
                .map(|file| Arc::new(file) as Arc<dyn FileHandle>);
        }
        Self::open_file(path).map(|file| Arc::new(file) as Arc<dyn FileHandle>)
    }
    fn glob(&self, path: &[u8]) -> Result<Vec<ByteString>> {
        if let Some(mut source) = self.glob_lazy(path)? {
            let mut result = Vec::new();
            while let Some(page) = source.expand_next()? {
                result.extend(page);
            }
            return Ok(result);
        }
        Ok(vec![path.into()])
    }
    fn glob_lazy(&self, path: &[u8]) -> Result<Option<Box<dyn GlobResult>>> {
        let request = Self::prepare(path, true)?;
        let Some(first) = request.key.bytes().position(|b| b"*[\\".contains(&b)) else {
            return Ok(None);
        };
        if request.compatibility {
            return Ok(None);
        }
        Ok(Some(Box::new(S3Glob::new(request, first))))
    }
    fn is_file(&self, path: &[u8]) -> Result<bool> {
        match Self::open_file(path) {
            Ok(_) => Ok(true),
            Err(error)
                if error.kind() == roastedduck_common::ErrorKind::Http
                    && error.raw_message().contains("HTTP 404") =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
    fn is_dir(&self, _: &[u8]) -> Result<bool> {
        Ok(true)
    }
    fn mkdir(&self, _: &[u8]) -> Result<()> {
        Err(Error::not_implemented("S3 directories are not supported"))
    }
    fn remove(&self, path: &[u8]) -> Result<()> {
        delete::remove(path)
    }
    fn remove_files(&self, paths: &[ByteString]) -> Result<()> {
        delete::remove_files(paths)
    }
    fn list_files_recursive(&self, path: &[u8]) -> Result<Vec<ByteString>> {
        let path = std::str::from_utf8(path).map_err(io_error)?.trim_end_matches('/');
        self.glob(format!("{path}/**").as_bytes())
    }
    fn rename(&self, _: &[u8], _: &[u8]) -> Result<()> {
        Err(Error::not_implemented("Moving S3 files is not supported"))
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

    #[test]
    fn refresh_without_opener_preserves_original_destination() {
        assert!(roastedduck_function::file_system::opener().is_none());
        let signing = RequestSigning {
            original_path: "s3://bucket/part-error.csv".into(),
            host: "127.0.0.1:12345".into(),
            path: "/bucket/part-error.csv".into(),
            ..Default::default()
        };
        let parameters = Parameters::current_with_http_secret(&signing.original_path, false, false).unwrap();
        assert!(
            !signing
                .try_refresh(
                    "PUT",
                    &signing,
                    &parameters,
                    "http://127.0.0.1:12345/bucket/part-error.csv",
                )
                .unwrap()
        );
        assert!(signing.refreshed_transport().is_none());
    }
}
