//! Path handling of the local file system (DuckDB's `FileSystem::ExpandPath` and
//! `FileSystem::GetHomeDirectory`) and the file opener of the running statement.
//!
//! DuckDB hands the `ClientContext` to the file system as the `FileOpener`, which reads
//! settings such as `home_directory` and logs file operations to the connection's
//! loggers. Our readers do not see the connection, so the connection makes its function
//! state the opener of the thread running its statement ([`activate_opener`]).

use std::cell::RefCell;
use std::sync::Arc;

use crate::client::FunctionClient;

thread_local! {
    /// The function state of the connection whose statement this thread runs.
    static OPENER: RefCell<Option<Arc<FunctionClient>>> = const { RefCell::new(None) };
}

/// Restores the thread's previous opener when dropped.
#[derive(Debug)]
pub struct OpenerGuard {
    #[allow(clippy::option_option, reason = "the outer `None`: nothing was replaced")]
    previous: Option<Option<Arc<FunctionClient>>>,
}

impl Drop for OpenerGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            OPENER.with(|opener| *opener.borrow_mut() = previous);
        }
    }
}

/// Makes `client` the file opener of this thread until the guard is dropped.
pub fn activate_opener(client: Arc<FunctionClient>) -> OpenerGuard {
    OpenerGuard {
        previous: Some(OPENER.with(|opener| opener.borrow_mut().replace(client))),
    }
}

/// The file opener of the statement this thread runs, if any.
pub fn opener() -> Option<Arc<FunctionClient>> {
    OPENER.with(|opener| opener.borrow().clone())
}

/// `FileSystem::GetHomeDirectory`: the `home_directory` setting if set, else `$HOME`.
pub fn home_directory() -> String {
    let setting = opener()
        .and_then(|client| client.settings())
        .and_then(|settings| settings.current_setting("home_directory").ok())
        .filter(|value| !value.is_null())
        .and_then(|value| value.to_varchar().ok())
        .filter(|home| !home.is_empty());
    setting.unwrap_or_else(|| std::env::var("HOME").unwrap_or_default())
}

/// `GetFileUrlOffset`: the length of a `file:/`, `file:///` or `file://localhost/`
/// prefix, keeping the path's leading slash.
fn file_url_offset(path: &str) -> usize {
    let Some(rest) = path.strip_prefix("file:/") else {
        return 0;
    };
    if !rest.starts_with('/') {
        // `file:/some/path`.
        return "file:".len();
    }
    if rest[1..].starts_with('/') {
        // `file:///some/path`: an empty host.
        return "file://".len();
    }
    if rest[1..].starts_with("localhost/") {
        // `file://localhost/some/path`.
        return "file://localhost".len();
    }
    0
}

/// `FileSystem::ExpandPath`: a leading `~` is the home directory and `file:` URLs are
/// local paths.
pub fn expand_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix('~') {
        return format!("{}{rest}", home_directory());
    }
    path[file_url_offset(path)..].to_owned()
}

/// `FileSystem::CanonicalizePath`: `.` and empty components dropped, `..` resolved
/// lexically (at the root it is ignored); remote paths are kept as they are.
pub fn canonicalize_path(path: &str) -> String {
    if path.contains("://") {
        return path.to_owned();
    }
    let mut stack: Vec<&str> = Vec::new();
    for element in path.split('/') {
        match element {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            element => stack.push(element),
        }
    }
    let mut result = if path.starts_with('/') {
        "/".to_owned()
    } else {
        String::new()
    };
    result.push_str(&stack.join("/"));
    result
}

/// `LocalFileSystem::CanonicalizePath`: the absolute path with symbolic links, `.` and
/// `..` resolved as far as the path exists (`realpath`), and lexically beyond that.
pub fn canonicalize_local_path(input: &str) -> String {
    // `MakePathAbsolute`.
    let mut path = expand_path(input);
    if !path.starts_with('/') {
        let working_directory = std::env::current_dir()
            .map(|directory| directory.to_string_lossy().into_owned())
            .unwrap_or_default();
        path = format!("{working_directory}/{path}");
    }
    let mut current = path.clone();
    let mut remainder = String::new();
    let mut dot_dot_count = 0;
    while !current.is_empty() {
        if dot_dot_count == 0
            && let Ok(resolved) = std::fs::canonicalize(&current)
        {
            let resolved = resolved.to_string_lossy().into_owned();
            if remainder.is_empty() {
                return resolved;
            }
            if resolved.ends_with('/') {
                return resolved + &remainder;
            }
            return format!("{resolved}/{remainder}");
        }
        // Up one directory.
        let Some(sep) = current.rfind('/') else {
            break;
        };
        let component = &current[sep + 1..];
        if component == ".." {
            dot_dot_count += 1;
        } else if !component.is_empty() && component != "." {
            if dot_dot_count > 0 {
                dot_dot_count -= 1;
            } else if remainder.is_empty() {
                component.clone_into(&mut remainder);
            } else {
                remainder = format!("{component}/{remainder}");
            }
        }
        current.truncate(sep);
    }
    canonicalize_path(&path)
}

/// `DBConfig::SanitizeAllowedPath`: the path as the file system serving it canonicalizes
/// it; remote paths are kept as they are.
fn sanitize_allowed_path(path: &str) -> String {
    if path.contains("://") && file_url_offset(path) == 0 {
        return canonicalize_path(path);
    }
    canonicalize_local_path(path)
}

/// The VARCHAR entries of the list setting `name`.
fn list_setting(settings: &dyn crate::client::SettingsSource, name: &str) -> Vec<String> {
    let Ok(value) = settings.current_setting(name) else {
        return Vec::new();
    };
    value
        .children()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry.to_varchar().ok())
        .collect()
}

/// `DBConfig::CanAccessFile`: with `enable_external_access` off, only `allowed_paths`
/// and files under `allowed_directories` can be accessed.
fn can_access(settings: &dyn crate::client::SettingsSource, path: &str, directory: bool) -> bool {
    let enabled = settings
        .current_setting("enable_external_access")
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    if enabled {
        return true;
    }
    let mut path = sanitize_allowed_path(path);
    if list_setting(settings, "allowed_paths")
        .iter()
        .any(|allowed| sanitize_allowed_path(allowed) == path)
    {
        return true;
    }
    let directories = list_setting(settings, "allowed_directories");
    if directories.is_empty() {
        return false;
    }
    if directory && !path.ends_with('/') {
        path.push('/');
    }
    directories.iter().any(|allowed| {
        let mut allowed = sanitize_allowed_path(allowed);
        if !allowed.ends_with('/') {
            allowed.push('/');
        }
        path.starts_with(&allowed)
    })
}

/// Whether the statement's configuration allows external access (`enable_external_access`).
pub fn external_access_enabled() -> bool {
    opener()
        .and_then(|client| client.settings())
        .and_then(|settings| settings.current_setting("enable_external_access").ok())
        .and_then(|value| value.as_bool())
        .unwrap_or(true)
}

fn verify_can_access(path: &str, directory: bool) -> rustdb_common::Result<()> {
    let Some(client) = opener() else {
        return Ok(());
    };
    if let Some(settings) = client.settings()
        && !can_access(settings.as_ref(), path, directory)
    {
        return Err(rustdb_common::Error::new(
            rustdb_common::ErrorKind::Permission,
            format!(
                "Cannot access {} \"{path}\" - file system operations are disabled by configuration",
                if directory { "directory" } else { "file" }
            ),
        ));
    }
    // `VirtualFileSystem::FindFileSystem`: the file system of the path may be disabled.
    let name = file_system_name(path);
    let disabled = client
        .object_cache()
        .and_then(|cache| cache.get::<DisabledFileSystems>(DISABLED_FILE_SYSTEMS_KEY))
        .is_some_and(|disabled| disabled.contains(name));
    if disabled {
        return Err(rustdb_common::Error::new(
            rustdb_common::ErrorKind::Permission,
            format!("File system {name} has been disabled by configuration"),
        ));
    }
    verify_file_system_loaded(&client, path)
}

/// Path prefixes of the file systems extensions register, with the extension
/// (`EXTENSION_FILE_PREFIXES`).
const EXTENSION_FILE_PREFIXES: [(&str, &str); 12] = [
    ("http://", "httpfs"),
    ("https://", "httpfs"),
    ("s3://", "httpfs"),
    ("s3a://", "httpfs"),
    ("s3n://", "httpfs"),
    ("gcs://", "httpfs"),
    ("gs://", "httpfs"),
    ("r2://", "httpfs"),
    ("azure://", "azure"),
    ("az://", "azure"),
    ("abfss://", "azure"),
    ("hf://", "httpfs"),
];

/// `VirtualFileSystem::FindFileSystem`: a path of a file system an extension registers
/// needs that extension, and rustdb loads none of them.
fn verify_file_system_loaded(client: &crate::FunctionClient, path: &str) -> rustdb_common::Result<()> {
    let Some((_, extension)) = EXTENSION_FILE_PREFIXES
        .iter()
        .find(|(prefix, _)| path.starts_with(prefix))
    else {
        return Ok(());
    };
    let message = crate::extension_entries::add_client_install_hint(
        client,
        &format!("File {path} requires the extension {extension} to be loaded"),
        extension,
    );
    Err(rustdb_common::Error::new(
        rustdb_common::ErrorKind::MissingExtension,
        message,
    ))
}

/// The name of the file system that handles `path` (`FileSystem::GetName` of the one
/// `FindFileSystem` picks): the local file system unless the path has a URL scheme.
fn file_system_name(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        "HTTPFileSystem"
    } else if ["s3://", "s3a://", "s3n://", "gcs://", "gs://", "r2://"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
    {
        "S3FileSystem"
    } else {
        "LocalFileSystem"
    }
}

/// The object cache key under which the database marks the ZSTD file system as
/// registered (`FileSystemRegistry::compressed_fs`, which the parquet extension extends).
pub const ZSTD_FILE_SYSTEM_KEY: &str = "rustdb.zstd_file_system";

/// `VirtualFileSystem::OpenFile` with a compression: ZSTD files need the ZSTD file
/// system, which the parquet extension registers. Without a statement (worker threads)
/// the check was made when the reader was bound.
pub fn verify_zstd_file_system() -> rustdb_common::Result<()> {
    let Some(cache) = opener().and_then(|client| client.object_cache()) else {
        return Ok(());
    };
    if cache.get::<()>(ZSTD_FILE_SYSTEM_KEY).is_some() {
        return Ok(());
    }
    Err(rustdb_common::Error::not_implemented(
        "Attempting to open a compressed file, but the compression type is not supported.\nConsider explicitly \
         \"INSTALL parquet; LOAD parquet;\" to support this compression scheme",
    ))
}

/// The object cache key of the database's [`DisabledFileSystems`].
pub const DISABLED_FILE_SYSTEMS_KEY: &str = "rustdb.disabled_filesystems";

/// The file systems the `disabled_filesystems` setting disabled
/// (`FileSystemRegistry::disabled_file_systems`); once disabled they stay disabled.
#[derive(Debug, Default)]
pub struct DisabledFileSystems(std::sync::Mutex<std::collections::BTreeSet<String>>);

impl DisabledFileSystems {
    fn contains(&self, name: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(name)
    }

    /// `FileSystemRegistry::SetDisabledFileSystems`.
    pub fn set(&self, names: &[&str]) -> rustdb_common::Result<()> {
        let mut new = std::collections::BTreeSet::new();
        for &name in names {
            if name.is_empty() {
                continue;
            }
            if !new.insert(name.to_owned()) {
                return Err(rustdb_common::Error::invalid_input(format!(
                    "Duplicate disabled file system \"{name}\""
                )));
            }
        }
        let mut current = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(previous) = current.iter().find(|previous| !new.contains(*previous)) {
            return Err(rustdb_common::Error::invalid_input(format!(
                "File system \"{previous}\" has been disabled previously, it cannot be re-enabled"
            )));
        }
        *current = new;
        Ok(())
    }
}

/// `OpenerFileSystem::VerifyCanAccessExtension`: files ending with `.duckdb_extension`
/// are only written by `INSTALL`.
pub fn verify_can_write(path: &str) -> rustdb_common::Result<()> {
    if path.ends_with(".duckdb_extension") {
        return Err(rustdb_common::Error::new(
            rustdb_common::ErrorKind::Permission,
            format!(
                "File '{path}' cannot be opened for writing since files ending with '.duckdb_extension' are \
                 reserved for DuckDB extensions, and these can only be installed through the INSTALL command"
            ),
        ));
    }
    match opener() {
        Some(client) => verify_file_system_loaded(&client, path),
        None => Ok(()),
    }
}

/// `OpenerFileSystem::VerifyCanAccessFile`: a Permission Error if the configuration
/// forbids accessing the file `path`.
pub fn verify_can_access_file(path: &str) -> rustdb_common::Result<()> {
    verify_can_access(path, false)
}

/// `OpenerFileSystem::VerifyCanAccessDirectory`.
pub fn verify_can_access_directory(path: &str) -> rustdb_common::Result<()> {
    verify_can_access(path, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_are_local_paths() {
        assert_eq!(expand_path("file:/a/b.csv"), "/a/b.csv");
        assert_eq!(expand_path("file:///a/b.csv"), "/a/b.csv");
        assert_eq!(expand_path("file://localhost/a/b.csv"), "/a/b.csv");
        assert_eq!(expand_path("file://host/a/b.csv"), "file://host/a/b.csv");
        assert_eq!(expand_path("data/b.csv"), "data/b.csv");
    }
}
