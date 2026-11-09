//! `HuggingFace` HTTP paths and tree traversal from pinned HTTPFS 4bc690d.
use crate::{HttpFile, Parameters, io_error};
use roastedduck_common::{ByteString, Error, ErrorKind, Result};
use roastedduck_function::registered_files::{FileHandle, FileSystem};
use std::fmt::Write as _;
use std::io::Read as _;
use std::sync::Arc;

const ENDPOINT: &str = "https://huggingface.co";
pub const SECRET_RESOLVER_KEY: &str = "roastedduck.huggingface_secret_resolver";

pub trait ResolveSecret: std::fmt::Debug + Send + Sync {
    fn token(&self, path: &str) -> Result<Option<String>>;
}
#[derive(Debug)]
pub struct SecretResolver(pub Arc<dyn ResolveSecret>);

#[derive(Debug)]
struct Parsed {
    repo_type: String,
    repository: String,
    revision: String,
    path: String,
}

impl Parsed {
    fn parse(url: &str) -> Result<Self> {
        let parse_error = || {
            io_error(format!(
                "Failed to parse '{url}'. Please format url like: 'hf://datasets/my-username/my-dataset/path/to/file.parquet'"
            ))
        };
        let rest = url
            .strip_prefix("hf://")
            .ok_or_else(|| Error::internal("Not an hf url"))?;
        let (repo_type, rest) = rest.split_once('/').ok_or_else(parse_error)?;
        if !matches!(repo_type, "datasets" | "spaces") {
            return Err(io_error(format!(
                "Failed to parse: '{url}'. Currently DuckDB only supports querying datasets or spaces, so the url should start with 'hf://datasets' or 'hf://spaces'"
            )));
        }
        let owner_end = rest.find('/').ok_or_else(parse_error)?;
        let path_at = rest[owner_end + 1..]
            .find('/')
            .map(|i| i + owner_end + 1)
            .ok_or_else(parse_error)?;
        let repository_revision = &rest[..path_at];
        let (repository, revision) =
            repository_revision[owner_end + 1..]
                .find('@')
                .map_or((repository_revision, "main"), |at| {
                    let at = at + owner_end + 1;
                    (&repository_revision[..at], &repository_revision[at + 1..])
                });
        Ok(Self {
            repo_type: repo_type.into(),
            repository: repository.into(),
            revision: revision.into(),
            path: rest[path_at..].into(),
        })
    }
    fn file_url(&self) -> String {
        format!(
            "{ENDPOINT}/{}/{}/resolve/{}{}",
            self.repo_type, self.repository, self.revision, self.path
        )
    }
    fn tree_url(&self, path: &str, limit: u64) -> String {
        let mut url = format!(
            "{ENDPOINT}/api/{}/{}/tree/{}{path}",
            self.repo_type, self.repository, self.revision
        );
        if limit > 0 {
            let _ = write!(url, "?limit={limit}");
        }
        url
    }
    fn uri(&self, path: &str) -> String {
        if self.revision == "main" {
            format!("hf://{}/{}{path}", self.repo_type, self.repository)
        } else {
            format!(
                "hf://{}/{}@{}{path}",
                self.repo_type, self.repository, self.revision
            )
        }
    }
}

fn parameters(path: &str) -> Result<Parameters> {
    let token = roastedduck_function::file_system::opener()
        .and_then(|c| c.object_cache())
        .and_then(|c| c.get::<SecretResolver>(SECRET_RESOLVER_KEY))
        .map_or(Ok(None), |r| r.0.token(path))?;
    let mut params = Parameters::current_with_http_secret(path, true, token.is_none())?;
    if let Some(token) = token.filter(|token| !token.is_empty())
        && !params
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
    {
        params
            .headers
            .push(("Authorization".into(), format!("Bearer {token}")));
    }
    Ok(params)
}

fn next_link(header: &str) -> Result<String> {
    for entry in header.split(',') {
        let parts: Vec<_> = entry.split(';').collect();
        if parts.len() != 2 {
            return Err(io_error(format!(
                "Unexpected link header for huggingface pagination: {header}"
            )));
        }
        if parts[1].trim() == "rel=\"next\"" {
            let url = parts[0].trim();
            if !url.starts_with('<') || !url.ends_with('>') {
                return Err(io_error(format!(
                    "Unexpected link header for huggingface pagination: {header}"
                )));
            }
            return Ok(url[1..url.len() - 1].into());
        }
    }
    Err(io_error(
        "Failed to parse Link header for paginated response, pagination support",
    ))
}

/// The pinned parser recognizes minified type/path fields and ignores nested
/// metadata. Keep its byte-level acceptance and diagnostics for these responses.
fn parse_list(input: &[u8], files: &mut Vec<String>, directories: &mut Vec<String>) -> Result<()> {
    let mut index = 0;
    let mut nested = 0;
    while index < input.len() {
        while index < input.len() && input[index] != b'{' {
            index += 1;
        }
        if index == input.len() {
            break;
        }
        index += 1;
        let (mut found, mut kind, mut path) = (false, 0, Vec::new());
        while index < input.len() {
            if input[index] == b'}' {
                index += 1;
                if nested > 0 {
                    nested -= 1;
                    continue;
                }
                if !found || kind == 0 {
                    return Err(io_error("Failed to parse list result"));
                }
                let path = String::from_utf8(path).map_err(io_error)?;
                if kind == 1 {
                    files.push(format!("/{path}"));
                } else {
                    directories.push(format!("/{path}"));
                }
                break;
            } else if input[index] == b'{' {
                nested += 1;
                index += 1;
            } else if input[index..].starts_with(b"\"type\":\"directory\"") {
                kind = 2;
                index += 18;
            } else if input[index..].starts_with(b"\"type\":\"file\"") {
                kind = 1;
                index += 13;
            } else if input[index..].starts_with(b"\"path\":\"") {
                found = true;
                index += 8;
                while index < input.len() && input[index] != b'"' {
                    path.push(input[index]);
                    index += 1;
                }
                if index < input.len() {
                    index += 1;
                }
            } else {
                index += 1;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct HuggingFaceFileSystem;

impl FileSystem for HuggingFaceFileSystem {
    fn name(&self) -> &'static str {
        "HuggingFaceFileSystem"
    }
    fn is_remote(&self) -> bool {
        true
    }
    fn handles(&self, path: &[u8]) -> bool {
        path.starts_with(b"hf://")
    }
    fn open(&self, path: &[u8], write: bool) -> Result<Arc<dyn FileHandle>> {
        if write {
            return Err(Error::not_implemented("Writing to HTTP files not supported"));
        }
        let original = std::str::from_utf8(path).map_err(io_error)?;
        let parsed = Parsed::parse(original)?;
        HttpFile::open_with_display(parsed.file_url(), original.into(), parameters(original)?)
            .map(|file| Arc::new(file) as Arc<dyn FileHandle>)
    }
    fn glob(&self, path: &[u8]) -> Result<Vec<ByteString>> {
        let original = std::str::from_utf8(path).map_err(io_error)?;
        let parsed = Parsed::parse(original)?;
        let Some(first) = parsed.path.bytes().position(|c| b"*[\\".contains(&c)) else {
            return Ok(vec![path.into()]);
        };
        let shared = &parsed.path[..first];
        let shared = shared.rfind('/').map_or("", |at| &shared[..at]);
        let params = parameters(original)?;
        let limit = roastedduck_function::file_system::opener()
            .and_then(|c| c.settings())
            .and_then(|s| s.current_setting("hf_max_per_page").ok())
            .and_then(|v| v.to_varchar().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let (mut files, mut directories, mut next) = (Vec::new(), vec![shared.to_owned()], String::new());
        loop {
            if next.is_empty() {
                if let Some(directory) = directories.pop() {
                    next = parsed.tree_url(&directory, limit);
                } else {
                    break;
                }
            }
            let fragment = next.strip_prefix(ENDPOINT).unwrap_or(&next);
            let url = format!(
                "{ENDPOINT}{}{fragment}",
                if fragment.starts_with('/') { "" } else { "/" }
            );
            let request = HttpFile::new_request(url, params.clone(), None)?;
            let mut response = request.request("GET", None)?;
            let status = response.status().as_u16();
            if status >= 400 {
                return Err(crate::response_error(
                    Error::new(
                        ErrorKind::Http,
                        format!("HTTP GET error on '{next}' (HTTP {status})"),
                    ),
                    &response,
                    b"",
                    params.httplib,
                ));
            }
            if status != 200 {
                return Err(io_error(format!(
                    "{} error for HTTP GET to '{next}'",
                    response.status().canonical_reason().unwrap_or_default()
                )));
            }
            next = response
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .filter(|v| !v.is_empty())
                .map_or(Ok(String::new()), next_link)?;
            let mut body = Vec::new();
            response
                .body_mut()
                .as_reader()
                .read_to_end(&mut body)
                .map_err(io_error)?;
            parse_list(&body, &mut files, &mut directories)?;
        }
        let pattern: Vec<_> = parsed.path.split_terminator('/').collect();
        let mut result = Vec::new();
        for path in files {
            if crate::s3::path_matches(&path.split_terminator('/').collect::<Vec<_>>(), &pattern, true) {
                result.push(parsed.uri(&path).into());
            }
        }
        Ok(result)
    }
    fn is_file(&self, path: &[u8]) -> Result<bool> {
        match self.open(path, false) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == ErrorKind::Http && e.raw_message().contains("HTTP 404") => Ok(false),
            Err(e) => Err(e),
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
