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
