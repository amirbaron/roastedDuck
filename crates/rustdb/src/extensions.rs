//! `LOAD` / `INSTALL` (DuckDB's `ExtensionHelper::LoadExternalExtension` and
//! `InstallExtension`). The extensions DuckDB links statically are built in; loading
//! them does nothing. Extension files cannot be loaded.

use rustdb_ast::{LoadInfo, LoadType};
use rustdb_common::string_util::{candidates_message, top_n_jaro_winkler};
use rustdb_common::{Error, ErrorKind, Result};

/// The extensions DuckDB knows (`internal_extensions`).
const DEFAULT_EXTENSIONS: [&str; 30] = [
    "core_functions",
    "icu",
    "excel",
    "parquet",
    "tpch",
    "tpcds",
    "httpfs",
    "json",
    "autocomplete",
    "motherduck",
    "mysql_scanner",
    "odbc_scanner",
    "sqlite_scanner",
    "postgres_scanner",
    "inet",
    "spatial",
    "aws",
    "azure",
    "encodings",
    "iceberg",
    "vss",
    "delta",
    "fts",
    "ui",
    "ducklake",
    "quack",
    "vortex",
    "lance",
    "avro",
    "unity_catalog",
];

/// The extensions the reference build links (loading them is a no-op).
const LINKED_EXTENSIONS: [&str; 7] = [
    "core_functions",
    "icu",
    "parquet",
    "tpch",
    "tpcds",
    "json",
    "autocomplete",
];

/// `internal_aliases`.
const ALIASES: [(&str, &str); 10] = [
    ("http", "httpfs"),
    ("https", "httpfs"),
    ("md", "motherduck"),
    ("mysql", "mysql_scanner"),
    ("odbc", "odbc_scanner"),
    ("s3", "httpfs"),
    ("postgres", "postgres_scanner"),
    ("sqlite", "sqlite_scanner"),
    ("sqlite3", "sqlite_scanner"),
    ("uc_catalog", "unity_catalog"),
];

/// `ApplyExtensionAlias`.
fn apply_extension_alias(name: &str) -> String {
    let lower = name.to_lowercase();
    ALIASES
        .iter()
        .find(|(alias, _)| *alias == lower)
        .map_or(lower, |(_, extension)| (*extension).to_owned())
}

/// `IsFullPath`.
fn is_full_path(extension: &str) -> bool {
    extension.contains(['.', '/', '\\'])
}

/// `CreateSuggestions`: the candidates message, and whether the name is an extension.
fn create_suggestions(extension: &str) -> (String, bool) {
    let lower = extension.to_lowercase();
    let mut candidates: Vec<String> = DEFAULT_EXTENSIONS.iter().map(|name| (*name).to_owned()).collect();
    candidates.extend(ALIASES.iter().map(|(alias, _)| (*alias).to_owned()));
    let closest = top_n_jaro_winkler(&candidates, &lower);
    if closest.contains(&lower) {
        return (
            format!("Extension \"{extension}\" is an existing extension.\n"),
            true,
        );
    }
    (candidates_message(&closest, "Candidate extensions"), false)
}

/// `DuckDBPlatform()`: the platform name of this build.
pub(crate) fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "osx_arm64"
        } else {
            "osx_amd64"
        }
    } else if cfg!(target_arch = "aarch64") {
        "linux_arm64"
    } else {
        "linux_amd64"
    }
}

/// The file `LOAD name` reads (`~/.duckdb/extensions/<version>/<platform>/`; the reference build is `v0.0.1`).
fn extension_path(name: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let platform = platform();
    format!("{home}/.duckdb/extensions/v0.0.1/{platform}/{name}.duckdb_extension")
}

/// Runs `LOAD` / `INSTALL`.
/// Returns the name of the statically linked extension `LOAD` refers to, which the
/// caller loads.
pub(crate) fn load_extension(info: &LoadInfo) -> Result<Option<String>> {
    if info.load_type != LoadType::Load {
        // Installing writes to the extension directory through the client's opener file
        // system, which the access configuration may forbid.
        let path = extension_path(&apply_extension_alias(&info.filename));
        if let Some((directory, _)) = path.rsplit_once('/') {
            rustdb_function::file_system::verify_can_access_directory(directory)?;
        }
        return Err(Error::not_implemented("INSTALL of extensions"));
    }
    let extension = &info.filename;
    let filename = if is_full_path(extension) {
        extension.clone()
    } else {
        let name = apply_extension_alias(extension);
        if LINKED_EXTENSIONS.contains(&name.as_str()) {
            return Ok(Some(name));
        }
        extension_path(&name)
    };
    // `TryInitialLoad`: external extensions need external access.
    if !rustdb_function::file_system::external_access_enabled() {
        return Err(Error::new(
            ErrorKind::Permission,
            "Loading external extensions is disabled through configuration",
        ));
    }
    if !filename.ends_with(".duckdb_extension") {
        return Err(Error::new(
            ErrorKind::Permission,
            format!(
                "DuckDB extensions are files ending with '.duckdb_extension', loading different files is not possible, error while loading from '{filename}', consider 'INSTALL <path>; LOAD <name>;'"
            ),
        ));
    }
    if !std::path::Path::new(&filename).exists() {
        let (mut message, exact_match) = create_suggestions(extension);
        if exact_match {
            message += "\nInstall it first using \"INSTALL ";
            message += extension;
            message += "\".";
        }
        return Err(Error::new(
            ErrorKind::Io,
            format!("Extension \"{filename}\" not found.\n{message}"),
        ));
    }
    Err(Error::not_implemented(format!(
        "loading the extension file \"{filename}\""
    )))
}
