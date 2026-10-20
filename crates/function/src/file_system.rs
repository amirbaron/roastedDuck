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
    let mut path = canonicalize_path(path);
    if list_setting(settings, "allowed_paths")
        .iter()
        .any(|allowed| canonicalize_path(allowed) == path)
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
        let mut allowed = canonicalize_path(allowed);
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
    let Some(settings) = opener().and_then(|client| client.settings()) else {
        return Ok(());
    };
    if can_access(settings.as_ref(), path, directory) {
        return Ok(());
    }
    Err(rustdb_common::Error::new(
        rustdb_common::ErrorKind::Permission,
        format!(
            "Cannot access {} \"{path}\" - file system operations are disabled by configuration",
            if directory { "directory" } else { "file" }
        ),
    ))
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
