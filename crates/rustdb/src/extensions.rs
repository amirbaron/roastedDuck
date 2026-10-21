//! `LOAD` / `INSTALL` (DuckDB's `ExtensionHelper::LoadExternalExtension` and
//! `InstallExtension`). The extensions DuckDB links statically are built in; loading
//! them does nothing. Extension files cannot be loaded.

use rustdb_ast::{LoadInfo, LoadType};
use rustdb_common::string_util::{candidates_message, top_n_jaro_winkler};
use rustdb_common::{Error, ErrorKind, Result};

/// The extensions DuckDB knows, with their descriptions (`internal_extensions`).
const DEFAULT_EXTENSIONS: [(&str, &str); 30] = [
    ("core_functions", "Core function library"),
    (
        "icu",
        "Adds support for time zones and collations using the ICU library",
    ),
    ("excel", "Adds support for Excel-like format strings"),
    ("parquet", "Adds support for reading and writing parquet files"),
    ("tpch", "Adds TPC-H data generation and query support"),
    ("tpcds", "Adds TPC-DS data generation and query support"),
    (
        "httpfs",
        "Adds support for reading and writing files over a HTTP(S) connection",
    ),
    ("json", "Adds support for JSON operations"),
    ("autocomplete", "Adds support for autocomplete in the shell"),
    ("motherduck", "Enables motherduck integration with the system"),
    ("mysql_scanner", "Adds support for connecting to a MySQL database"),
    (
        "odbc_scanner",
        "Adds support for connecting to remote databases over ODBC",
    ),
    (
        "sqlite_scanner",
        "Adds support for reading and writing SQLite database files",
    ),
    (
        "postgres_scanner",
        "Adds support for connecting to a Postgres database",
    ),
    ("inet", "Adds support for IP-related data types and functions"),
    (
        "spatial",
        "Geospatial extension that adds support for working with spatial data and functions",
    ),
    ("aws", "Provides features that depend on the AWS SDK"),
    (
        "azure",
        "Adds a filesystem abstraction for Azure blob storage to DuckDB",
    ),
    ("encodings", "All unicode encodings to UTF-8"),
    ("iceberg", "Adds support for Apache Iceberg"),
    (
        "vss",
        "Adds indexing support to accelerate Vector Similarity Search",
    ),
    ("delta", "Adds support for Delta Lake"),
    ("fts", "Adds support for Full-Text Search Indexes"),
    ("ui", "Adds local UI for DuckDB"),
    ("ducklake", "Adds support for DuckLake, SQL as a Lakehouse Format"),
    ("quack", "The DuckDB 'Quack' Client/Server Protocol"),
    (
        "vortex",
        "Adds support for reading and writing files using the Vortex file format",
    ),
    ("lance", "Adds support for querying Lance datasets"),
    ("avro", "Adds support for reading Avro files"),
    ("unity_catalog", "Adds support for connecting to Unity Catalog"),
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
    let mut candidates: Vec<String> = DEFAULT_EXTENSIONS
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
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

/// The version directory of extension paths and URLs (`GetVersionDirectoryName`; the
/// reference build is `v0.0.1`).
const VERSION_DIRECTORY: &str = "v0.0.1";

/// The extension repositories known by name (`ExtensionRepository::TryGetRepositoryUrl`).
const REPOSITORIES: [(&str, &str); 5] = [
    ("core", "http://extensions.duckdb.org"),
    ("core_nightly", "http://nightly-extensions.duckdb.org"),
    ("community", "http://community-extensions.duckdb.org"),
    ("local_build_debug", "./build/debug/repository"),
    ("local_build_release", "./build/release/repository"),
];

/// Path prefixes of files other file systems serve (`EXTENSION_FILE_PREFIXES`).
const REMOTE_PREFIXES: [&str; 12] = [
    "http://", "https://", "s3://", "s3a://", "s3n://", "gcs://", "gs://", "r2://", "azure://", "az://",
    "abfss://", "hf://",
];

/// `FileSystem::IsRemoteFile`.
fn is_remote_file(path: &str) -> bool {
    REMOTE_PREFIXES.iter().any(|prefix| path.starts_with(prefix))
}

/// The settings that locate extensions and their repository.
#[derive(Debug, Clone)]
pub(crate) struct ExtensionConfig {
    /// The directory extensions are installed in, version and platform included
    /// (`ExtensionHelper::GetExtensionDirectoryPath`, first entry).
    directory: String,
    /// `custom_extension_repository`.
    custom_repository: Option<String>,
}

impl ExtensionConfig {
    /// Reads `extension_directory`, `extension_directories` and
    /// `custom_extension_repository` through `setting`; the directory defaults to
    /// `~/.duckdb/extensions`.
    pub(crate) fn from_settings(setting: impl Fn(&str) -> Option<rustdb_types::Value>) -> Self {
        let text = |name: &str| {
            setting(name)
                .filter(|value| !value.is_null())
                .and_then(|value| value.to_varchar().ok())
                .filter(|text| !text.is_empty())
        };
        let configured = setting("extension_directories").and_then(|value| {
            value.children()?.iter().find_map(|directory| {
                directory
                    .to_varchar()
                    .ok()
                    .filter(|directory| !directory.is_empty())
            })
        });
        let base = text("extension_directory")
            .or(configured)
            .unwrap_or_else(|| "~/.duckdb/extensions".to_owned());
        let base = match base.strip_prefix('~') {
            Some(rest) => format!("{}{rest}", std::env::var("HOME").unwrap_or_default()),
            None => base,
        };
        Self {
            directory: format!("{base}/{VERSION_DIRECTORY}/{}", platform()),
            custom_repository: text("custom_extension_repository"),
        }
    }

    /// The installed file of extension `name`.
    fn extension_path(&self, name: &str) -> String {
        format!("{}/{name}.duckdb_extension", self.directory)
    }

    /// `ExtensionHelper::ExtensionDirectory`: the directory, created if missing through
    /// the client's file system.
    fn create_directory(&self) -> Result<&str> {
        rustdb_function::file_system::verify_can_access_directory(&self.directory)?;
        std::fs::create_dir_all(&self.directory).map_err(|error| {
            Error::new(
                ErrorKind::Io,
                format!("Failed to create directory \"{}\": {error}", self.directory),
            )
        })?;
        Ok(&self.directory)
    }
}

/// An extension repository (`ExtensionRepository`).
struct Repository {
    path: String,
}

/// `ExtensionInstallDocumentationLink`.
fn documentation_link(name: &str) -> String {
    format!(
        "https://duckdb.org/docs/stable/extensions/troubleshooting?version={VERSION_DIRECTORY}&platform={}&extension={name}",
        platform()
    )
}

/// `Binder::Bind(LoadStatement &)`: a repository given by name must be known.
fn check_repository_alias(info: &LoadInfo) -> Result<()> {
    if info.repo_is_alias
        && !info.repository.is_empty()
        && !REPOSITORIES.iter().any(|(name, _)| *name == info.repository)
    {
        let repository = &info.repository;
        return Err(Error::binder(format!(
            "'{repository}' is not a known repository name. Are you trying to query from a repository by path? Use \
             single quotes: `FROM '{repository}'`"
        )));
    }
    Ok(())
}

/// `INSTALL` / `FORCE INSTALL` (`PhysicalLoad::GetDataInternal` and
/// `ExtensionHelper::InstallExtensionInternal`).
fn install_extension(info: &LoadInfo, config: &ExtensionConfig) -> Result<()> {
    let force = info.load_type == LoadType::ForceInstall;
    let mut repository = if info.repository.is_empty() {
        None
    } else if info.repo_is_alias {
        REPOSITORIES
            .iter()
            .find(|(name, _)| *name == info.repository)
            .map(|(_, path)| Repository {
                path: (*path).to_owned(),
            })
    } else {
        Some(Repository {
            path: info.repository.clone(),
        })
    };
    let directory = config.create_directory()?;
    let extension = &info.filename;
    let base_name = extension
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(extension)
        .split('.')
        .next()
        .unwrap_or_default();
    let name = apply_extension_alias(base_name);
    let local_path = format!("{directory}/{name}.duckdb_extension");
    if std::path::Path::new(&local_path).exists() && !force {
        // Installed already: only a different origin is an error.
        return check_origin(&local_path, &name, extension, repository.as_ref());
    }
    if is_full_path(extension) && repository.is_some() {
        return Err(Error::invalid_input(
            "Cannot pass both a repository and a full path url",
        ));
    }
    if !is_full_path(extension) && repository.is_none() {
        repository = Some(Repository {
            path: config
                .custom_repository
                .clone()
                .unwrap_or_else(|| REPOSITORIES[0].1.to_owned()),
        });
    }
    if is_full_path(extension) {
        if extension.starts_with("http://") {
            return install_from_http_url(extension, &name);
        }
        return direct_install(extension, extension);
    }
    let Some(repository) = repository else {
        return Err(Error::internal("INSTALL without a repository"));
    };
    let versioned = if info.version.is_empty() {
        format!("/{VERSION_DIRECTORY}/{}/{name}.duckdb_extension.gz", platform())
    } else {
        format!(
            "/{name}/{}/{VERSION_DIRECTORY}/{}/{name}.duckdb_extension.gz",
            info.version,
            platform()
        )
    };
    let url = format!("{}{versioned}", repository.path);
    if repository.path.starts_with("http://") || repository.path.starts_with("https://") {
        install_from_http_url(&url, &name)
    } else {
        direct_install(&url, &name)
    }
}

/// `ThrowErrorOnMismatchingExtensionOrigin` for an installed extension.
fn check_origin(
    local_path: &str,
    name: &str,
    extension: &str,
    repository: Option<&Repository>,
) -> Result<()> {
    let info_path = format!("{local_path}.info");
    if !std::path::Path::new(&info_path).exists() {
        return Ok(());
    }
    let installed = read_install_info(&info_path, name)?;
    if installed.mode != InstallMode::Repository {
        return Ok(());
    }
    let mismatch = |kind: &str, origin: &str| {
        Err(Error::invalid_input(format!(
            "Installing extension '{name}' failed. The extension is already installed but the origin is different.\n\
             Currently installed extension is from repository '{}', while the extension to be installed is from \
             {kind} '{origin}'.\nTo solve this rerun this command with `FORCE INSTALL`",
            installed.repository_url
        )))
    };
    if let Some(repository) = repository
        && installed.repository_url != repository.path
    {
        return mismatch("repository", &repository.path);
    }
    if is_full_path(extension) {
        return mismatch("custom_path", extension);
    }
    Ok(())
}

/// `DirectInstallExtension`: installs the extension file at `path`.
fn direct_install(path: &str, name: &str) -> Result<()> {
    let mut file = path.to_owned();
    let mut exists = is_remote_file(&file) || std::path::Path::new(&file).exists();
    if !exists && let Some(stripped) = file.strip_suffix(".gz") {
        file = stripped.to_owned();
        exists = std::path::Path::new(&file).exists();
    }
    if !exists {
        return Err(Error::new(
            ErrorKind::Io,
            format!(
                "Failed to install local extension \"{name}\", no access to the file at PATH \"{file}\"\n"
            ),
        ));
    }
    Err(Error::not_implemented(format!(
        "installing the extension file \"{file}\""
    )))
}

/// `InstallFromHttpUrl`: downloads the extension at `url` over plain HTTP (without
/// following redirects).
fn install_from_http_url(url: &str, name: &str) -> Result<()> {
    let status = http_get_status(url);
    if matches!(status, Ok(200..300)) {
        return Err(Error::not_implemented(format!(
            "installing the extension downloaded from \"{url}\""
        )));
    }
    let (mut message, _) = create_suggestions(name);
    message.push_str("\nFor more info, visit ");
    message.push_str(&documentation_link(name));
    Err(match status {
        // The request itself failed.
        Err(request_error) => Error::new(
            ErrorKind::Io,
            format!(
                "Failed to download extension \"{name}\" at URL \"{url}\"\n{message} (ERROR {request_error})"
            ),
        ),
        // The server responded with an error status.
        Ok(status) => Error::new(
            ErrorKind::Http,
            format!("Failed to download extension \"{name}\" at URL \"{url}\" (HTTP {status})\n{message}"),
        ),
    })
}

/// The status of a plain HTTP/1.1 `GET` of `url` (DuckDB's built-in `HTTPUtil`
/// request), or the request error.
fn http_get_status(url: &str) -> std::result::Result<u16, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    const CONNECTION_ERROR: &str = "Could not establish connection";
    let Some(rest) = url.strip_prefix("http://") else {
        return Err(CONNECTION_ERROR.to_owned());
    };
    let (authority, path) = rest.find('/').map_or((rest, "/"), |slash| rest.split_at(slash));
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().map_err(|_| CONNECTION_ERROR.to_owned())?),
        None => (authority, 80),
    };
    let timeout = std::time::Duration::from_secs(30);
    let stream = (host, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| {
            addresses.find_map(|address| TcpStream::connect_timeout(&address, timeout).ok())
        })
        .ok_or_else(|| CONNECTION_ERROR.to_owned())?;
    let io_error = |error: std::io::Error| error.to_string();
    stream.set_read_timeout(Some(timeout)).map_err(io_error)?;
    let mut writer = &stream;
    write!(
        writer,
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: duckdb/{VERSION_DIRECTORY}({})\r\nConnection: close\r\n\r\n",
        platform()
    )
    .map_err(io_error)?;
    let mut status_line = String::new();
    BufReader::new(&stream)
        .read_line(&mut status_line)
        .map_err(io_error)?;
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| "Failed to read connection".to_owned())
}

/// Runs `LOAD` / `INSTALL`.
/// Returns the name of the statically linked extension `LOAD` refers to, which the
/// caller loads.
pub(crate) fn load_extension(info: &LoadInfo, config: &ExtensionConfig) -> Result<Option<String>> {
    check_repository_alias(info)?;
    if info.load_type != LoadType::Load {
        install_extension(info, config)?;
        return Ok(None);
    }
    let extension = &info.filename;
    let filename = if is_full_path(extension) {
        extension.clone()
    } else {
        let name = apply_extension_alias(extension);
        if LINKED_EXTENSIONS.contains(&name.as_str()) {
            return Ok(Some(name));
        }
        config.extension_path(&name)
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

/// How an extension was installed (DuckDB's `ExtensionInstallMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InstallMode {
    Unknown,
    Repository,
    CustomPath,
    StaticallyLinked,
    NotInstalled,
}

impl InstallMode {
    const fn name(self) -> &'static str {
        match self {
            Self::Unknown => "UNKNOWN",
            Self::Repository => "REPOSITORY",
            Self::CustomPath => "CUSTOM_PATH",
            Self::StaticallyLinked => "STATICALLY_LINKED",
            Self::NotInstalled => "NOT_INSTALLED",
        }
    }

    const fn from_number(number: u8) -> Option<Self> {
        Some(match number {
            0 => Self::Unknown,
            1 => Self::Repository,
            2 => Self::CustomPath,
            3 => Self::StaticallyLinked,
            4 => Self::NotInstalled,
            _ => return None,
        })
    }
}

/// The `.info` file written next to an installed extension (DuckDB's
/// `ExtensionInstallInfo`).
#[derive(Debug)]
struct InstallInfo {
    mode: InstallMode,
    full_path: String,
    repository_url: String,
    version: String,
}

impl rustdb_serde::DeserializeObject for InstallInfo {
    fn deserialize_fields(deserializer: &mut dyn rustdb_serde::Deserializer) -> Result<Self> {
        let mode: u8 = deserializer.read_property_with_default(100, "mode")?;
        let full_path: String = deserializer.read_property_with_default(101, "full_path")?;
        let repository_url: String = deserializer.read_property_with_default(102, "repository_url")?;
        let version: String = deserializer.read_property_with_default(103, "version")?;
        let _etag: String = deserializer.read_property_with_default(104, "etag")?;
        Ok(Self {
            mode: InstallMode::from_number(mode).unwrap_or(InstallMode::Unknown),
            full_path,
            repository_url,
            version,
        })
    }
}

/// `ExtensionInstallInfo::TryReadInfoFile`.
fn read_install_info(path: &str, name: &str) -> Result<InstallInfo> {
    let missing = || InstallInfo {
        mode: InstallMode::Unknown,
        full_path: String::new(),
        repository_url: String::new(),
        version: String::new(),
    };
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(missing());
    };
    let hint = format!("Try reinstalling the extension using 'FORCE INSTALL {name};'");
    if bytes.is_empty() {
        return Err(Error::new(
            ErrorKind::Io,
            format!(
                "Failed to read info file for '{name}' extension: '{path}'.\nThe file appears to be empty!\n{hint}"
            ),
        ));
    }
    rustdb_serde::binary::BinaryDeserializer::deserialize::<InstallInfo>(&bytes).map_err(|error| {
        Error::new(
            ErrorKind::Io,
            format!(
                "Failed to read info file for '{name}' extension: '{path}'.\nA serialization error occurred: '{}'\n{hint}",
                error.raw_message()
            ),
        )
    })
}

/// `ExtensionRepository::GetRepository`: the name of a known repository URL
/// (`TryConvertUrlToKnownRepository`).
fn repository_name(url: &str) -> String {
    match url {
        "http://extensions.duckdb.org" => "core".to_owned(),
        "http://nightly-extensions.duckdb.org" => "core_nightly".to_owned(),
        "http://community-extensions.duckdb.org" => "community".to_owned(),
        "./build/release/repository" => "local_build_release".to_owned(),
        "./build/debug/repository" => "local_build_debug".to_owned(),
        other => other.to_owned(),
    }
}

/// The rows of `duckdb_extensions()` (`DuckDBExtensionsInit`): the extensions DuckDB
/// knows, those installed in the extension directory and the loaded ones, by name.
/// `is_loaded` tells whether a statically linked extension is loaded; `client_extensions`
/// are the names and descriptions of the extensions the client linked and loaded.
pub(crate) fn extension_rows(
    config: &ExtensionConfig,
    is_loaded: impl Fn(&str) -> bool,
    client_extensions: &[(String, String)],
) -> Result<Vec<Vec<rustdb_types::Value>>> {
    use rustdb_types::{LogicalType, Value};
    struct Information {
        loaded: bool,
        installed: bool,
        file_path: String,
        install_mode: InstallMode,
        installed_from: String,
        description: String,
        aliases: Vec<Value>,
        extension_version: String,
    }
    let version = rustdb_storage::storage_info::LIBRARY_VERSION;
    let mut extensions: std::collections::BTreeMap<String, Information> = std::collections::BTreeMap::new();
    for (name, description) in DEFAULT_EXTENSIONS {
        let linked = LINKED_EXTENSIONS.contains(&name);
        let loaded = linked && is_loaded(name);
        extensions.insert(
            name.to_owned(),
            Information {
                loaded,
                installed: linked,
                file_path: if linked {
                    "(BUILT-IN)".to_owned()
                } else {
                    String::new()
                },
                install_mode: if linked {
                    InstallMode::StaticallyLinked
                } else {
                    InstallMode::NotInstalled
                },
                installed_from: String::new(),
                description: description.to_owned(),
                aliases: ALIASES
                    .iter()
                    .filter(|(_, extension)| *extension == name)
                    .map(|(alias, _)| Value::varchar(*alias))
                    .collect(),
                // A loaded built-in extension reports the library version.
                extension_version: if loaded { version.to_owned() } else { String::new() },
            },
        );
    }
    for (name, description) in client_extensions {
        extensions.insert(
            name.clone(),
            Information {
                loaded: true,
                installed: true,
                file_path: "(BUILT-IN)".to_owned(),
                install_mode: InstallMode::StaticallyLinked,
                installed_from: String::new(),
                description: description.clone(),
                aliases: Vec::new(),
                extension_version: version.to_owned(),
            },
        );
    }
    let directory = config.directory.as_str();
    // `fs.ListFiles` through the client's file system.
    rustdb_function::file_system::verify_can_access_directory(directory)?;
    if let Ok(entries) = std::fs::read_dir(directory) {
        let mut files: Vec<String> = entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|file| file.ends_with(".duckdb_extension"))
            .collect();
        files.sort();
        for file in files {
            let name = file.trim_end_matches(".duckdb_extension").to_owned();
            let info = read_install_info(&format!("{directory}/{file}.info"), &name)?;
            let installed_from = if info.mode == InstallMode::Repository {
                repository_name(&info.repository_url)
            } else {
                info.full_path.clone()
            };
            let file_path = format!("{directory}/{file}");
            match extensions.get_mut(&name) {
                Some(entry) => {
                    if entry.install_mode != InstallMode::StaticallyLinked {
                        entry.file_path = file_path;
                        entry.install_mode = info.mode;
                        entry.installed_from = installed_from;
                        entry.extension_version = info.version;
                    }
                    entry.installed = true;
                }
                None => {
                    extensions.insert(
                        name,
                        Information {
                            loaded: false,
                            installed: true,
                            file_path,
                            install_mode: info.mode,
                            installed_from,
                            description: String::new(),
                            aliases: Vec::new(),
                            extension_version: info.version,
                        },
                    );
                }
            }
        }
    }
    Ok(extensions
        .into_iter()
        .map(|(name, info)| {
            vec![
                Value::varchar(name),
                Value::boolean(info.loaded),
                Value::boolean(info.installed),
                Value::varchar(info.file_path),
                Value::varchar(info.description),
                Value::list(LogicalType::VARCHAR, info.aliases),
                Value::varchar(info.extension_version),
                Value::varchar(info.install_mode.name()),
                Value::varchar(info.installed_from),
            ]
        })
        .collect())
}
