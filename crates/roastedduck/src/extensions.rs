//! `LOAD` / `INSTALL` (DuckDB's `ExtensionHelper::LoadExternalExtension` and
//! `InstallExtension`). The extensions DuckDB links statically are built in; loading
//! them does nothing. External files use the embedding's C API adapter.

use std::io::{Read, Seek, SeekFrom, Write};

use roastedduck_ast::{LoadInfo, LoadType};
use roastedduck_common::string_util::{candidates_message_bytes, top_n_jaro_winkler};
use roastedduck_common::{ByteString, Error, ErrorKind, Result, byte_format};

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
const LINKED_EXTENSIONS: [&str; 9] = [
    "core_functions",
    "fts",
    "vss",
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
fn apply_extension_alias(name: &[u8]) -> ByteString {
    let lower = ByteString::from(name.to_ascii_lowercase());
    ALIASES
        .iter()
        .find(|(alias, _)| alias.as_bytes() == lower.as_bytes())
        .map_or(lower, |(_, extension)| (*extension).into())
}

/// `IsFullPath`.
fn is_full_path(extension: &[u8]) -> bool {
    extension.iter().any(|byte| b"./\\".contains(byte))
}

/// `CreateSuggestions`: the candidates message, and whether the name is an extension.
fn create_suggestions(extension: &[u8]) -> (ByteString, bool) {
    let lower = ByteString::from(extension.to_ascii_lowercase());
    let mut candidates: Vec<ByteString> = DEFAULT_EXTENSIONS
        .iter()
        .map(|(name, _)| (*name).into())
        .collect();
    candidates.extend(ALIASES.iter().map(|(alias, _)| (*alias).into()));
    let closest = top_n_jaro_winkler(&candidates, &lower);
    if closest.contains(&lower) {
        return (
            byte_format!("Extension \"{extension}\" is an existing extension.\n"),
            true,
        );
    }
    (candidates_message_bytes(&closest, b"Candidate extensions"), false)
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
/// release is `v1.5.6`).
const VERSION_DIRECTORY: &str = "v1.5.6";

/// The extension repositories known by name (`ExtensionRepository::TryGetRepositoryUrl`).
const REPOSITORIES: [(&str, &str); 5] = [
    ("core", "http://extensions.duckdb.org"),
    ("core_nightly", "http://nightly-extensions.duckdb.org"),
    ("community", "http://community-extensions.duckdb.org"),
    ("local_build_debug", "./build/debug/repository"),
    ("local_build_release", "./build/release/repository"),
];

#[allow(
    clippy::unnecessary_wraps,
    reason = "non-Unix native path conversion is fallible"
)]
pub(crate) fn native_path(bytes: &[u8]) -> Result<std::path::PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| Error::invalid_input("Invalid UTF-8 in filesystem path"))?;
        Ok(text.into())
    }
}
fn is_remote_file(path: &[u8]) -> bool {
    [
        "http://", "https://", "s3://", "s3a://", "s3n://", "gcs://", "gs://", "r2://", "azure://", "az://",
        "abfss://", "hf://",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix.as_bytes()))
}

/// The settings that locate extensions and their repository.
#[derive(Debug, Clone)]
pub(crate) struct ExtensionConfig {
    /// Ordered lookup/catalog directories, with version and platform included.
    /// INSTALL uses the first entry (`ExtensionHelper::GetExtensionDirectoryPath`).
    directories: Vec<String>,
    /// `custom_extension_repository`.
    custom_repository: Option<String>,
}

impl ExtensionConfig {
    /// Reads `extension_directory`, `extension_directories` and
    /// `custom_extension_repository` through `setting`; the directory defaults to
    /// `~/.duckdb/extensions`.
    pub(crate) fn from_settings(setting: impl Fn(&str) -> Option<roastedduck_types::Value>) -> Self {
        let text = |name: &str| {
            setting(name)
                .filter(|value| !value.is_null())
                .and_then(|value| value.to_varchar().ok())
                .filter(|text| !text.is_empty())
        };
        let mut bases: Vec<String> = text("extension_directory").into_iter().collect();
        if let Some(value) = setting("extension_directories")
            && let Some(configured) = value.children()
        {
            bases.extend(
                configured
                    .iter()
                    .filter_map(|directory| directory.to_varchar().ok()),
            );
        }
        if bases.is_empty() {
            bases.push("~/.duckdb/extensions".to_owned());
        }
        let directories = bases
            .into_iter()
            .map(|base| {
                let base = roastedduck_function::file_system::expand_path(&base);
                let separator = if base.is_empty() { "" } else { "/" };
                format!("{base}{separator}{VERSION_DIRECTORY}/{}", platform())
            })
            .collect();
        Self {
            directories,
            custom_repository: text("custom_extension_repository"),
        }
    }

    /// The first existing candidate, or the first path for a missing-file error.
    fn extension_path(&self, name: &[u8]) -> Result<ByteString> {
        for directory in &self.directories {
            let path = byte_format!("{directory}/{name}.duckdb_extension");
            if native_path(&path)?.is_file() {
                return Ok(path);
            }
        }
        Ok(byte_format!("{}/{name}.duckdb_extension", self.directories[0]))
    }

    /// `ExtensionHelper::ExtensionDirectory`: the directory, created if missing through
    /// the client's file system.
    fn create_directory(&self) -> Result<&str> {
        let directory = &self.directories[0];
        roastedduck_function::file_system::verify_can_access_directory(directory)?;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o755);
        }
        builder.create(directory).map_err(|error| {
            Error::new(
                ErrorKind::Io,
                format!("Failed to create directory \"{directory}\": {error}"),
            )
        })?;
        Ok(directory)
    }
}

/// An extension repository (`ExtensionRepository`).
struct Repository {
    path: ByteString,
}

/// `ExtensionInstallDocumentationLink`.
fn documentation_link(name: &[u8]) -> ByteString {
    byte_format!(
        "https://duckdb.org/docs/stable/extensions/troubleshooting?version={VERSION_DIRECTORY}&platform={}&extension={name}",
        platform()
    )
}

/// `Binder::Bind(LoadStatement &)`: a repository given by name must be known.
fn check_repository_alias(info: &LoadInfo) -> Result<()> {
    if info.repo_is_alias
        && !info.repository.is_empty()
        && !REPOSITORIES
            .iter()
            .any(|(name, _)| name.as_bytes() == info.repository.as_bytes())
    {
        let repository = &info.repository;
        return Err(Error::binder(byte_format!(
            "'{repository}' is not a known repository name. Are you trying to query from a repository by path? Use \
             single quotes: `FROM '{repository}'`"
        )));
    }
    Ok(())
}

/// `INSTALL` / `FORCE INSTALL` (`PhysicalLoad::GetDataInternal` and
/// `ExtensionHelper::InstallExtensionInternal`).
fn install_extension(info: &LoadInfo, config: &ExtensionConfig, database: &crate::Database) -> Result<()> {
    install_extension_options(info, config, database, false)
}

fn install_extension_options(
    info: &LoadInfo,
    config: &ExtensionConfig,
    database: &crate::Database,
    use_etags: bool,
) -> Result<()> {
    let force = info.load_type == LoadType::ForceInstall;
    let mut repository = if info.repository.is_empty() {
        None
    } else if info.repo_is_alias {
        REPOSITORIES
            .iter()
            .find(|(name, _)| name.as_bytes() == info.repository.as_bytes())
            .map(|(_, path)| Repository { path: (*path).into() })
    } else {
        Some(Repository {
            path: info.repository.clone(),
        })
    };
    let directory = config.create_directory()?;
    let extension = &info.filename;
    let base_name = extension
        .rsplit(|byte| matches!(byte, b'/' | b'\\'))
        .next()
        .unwrap_or(extension)
        .split(|byte| *byte == b'.')
        .next()
        .unwrap_or_default();
    let name = apply_extension_alias(base_name);
    let local_path = byte_format!("{directory}/{name}.duckdb_extension");
    if native_path(&local_path)?.exists() && !force {
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
                .unwrap_or_else(|| REPOSITORIES[0].1.to_owned())
                .into(),
        });
    }
    if is_full_path(extension) {
        if extension.starts_with(b"http://") {
            return install_from_http_url(extension, &name, &local_path, None, database, force, use_etags);
        }
        return direct_install(extension, extension, &local_path, None, database);
    }
    let Some(repository) = repository else {
        return Err(Error::internal("INSTALL without a repository"));
    };
    let versioned = if info.version.is_empty() {
        byte_format!("/{VERSION_DIRECTORY}/{}/{name}.duckdb_extension.gz", platform())
    } else {
        byte_format!(
            "/{name}/{}/{VERSION_DIRECTORY}/{}/{name}.duckdb_extension.gz",
            info.version,
            platform()
        )
    };
    let mut url = byte_format!("{}{versioned}", repository.path);
    // InstallFromRepository promotes HTTP once the HTTPFS HTTPUtil is loaded.
    // The recorded repository origin itself remains unchanged.
    if database.external_extensions().contains(b"httpfs")
        && let Some(rest) = url.strip_prefix(b"http://")
    {
        url = byte_format!("https://{rest}");
    }
    if repository.path.starts_with(b"http://") || repository.path.starts_with(b"https://") {
        install_from_http_url(
            &url,
            &name,
            &local_path,
            Some(&repository.path),
            database,
            force,
            use_etags,
        )
    } else {
        direct_install(&url, &name, &local_path, Some(&repository.path), database)
    }
}

/// `ThrowErrorOnMismatchingExtensionOrigin` for an installed extension.
fn check_origin(
    local_path: &[u8],
    name: &[u8],
    extension: &[u8],
    repository: Option<&Repository>,
) -> Result<()> {
    let info_path = byte_format!("{local_path}.info");
    if !native_path(&info_path)?.exists() {
        return Ok(());
    }
    let installed = read_install_info(&info_path, name)?;
    if installed.mode != InstallMode::Repository {
        return Ok(());
    }
    let mismatch = |kind: &str, origin: &[u8]| {
        Err(Error::invalid_input(byte_format!(
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
fn direct_install(
    path: &[u8],
    name: &[u8],
    destination: &[u8],
    repository: Option<&[u8]>,
    database: &crate::Database,
) -> Result<()> {
    // Local INSTALL explicitly uses C++'s local filesystem. Recognized remote
    // schemes go through the registered filesystem selected for the statement.
    let exists_at = |path: &[u8]| -> Result<bool> {
        let backend = roastedduck_storage::file_system::DatabaseFileSystem::for_path(path);
        if is_remote_file(path) && backend.is_registered() {
            backend.exists(path)
        } else {
            Ok(is_remote_file(path) || native_path(path)?.exists())
        }
    };
    let source_io_error = |error: std::io::Error| {
        roastedduck_storage::file_system::callback_error(&error)
            .unwrap_or_else(|| Error::new(ErrorKind::Io, error.to_string()))
    };
    let mut file = ByteString::from(path);
    let mut exists = exists_at(&file)?;
    if !exists && let Some(stripped) = file.strip_suffix(b".gz") {
        file = stripped.into();
        exists = exists_at(&file)?;
    }
    if !exists && !is_remote_file(&file) {
        return Err(Error::new_bytes(
            ErrorKind::Io,
            byte_format!(
                "Failed to install local extension \"{name}\", no access to the file at PATH \"{file}\"\n"
            ),
        ));
    }
    if !exists && file.starts_with(b"https://") {
        return Err(Error::new_bytes(
            ErrorKind::Io,
            byte_format!("Failed to install remote extension \"{name}\" from url \"{file}\""),
        ));
    }
    let metadata = InstallInfo {
        mode: if repository.is_some() {
            InstallMode::Repository
        } else {
            InstallMode::CustomPath
        },
        full_path: file.clone(),
        repository_url: repository.unwrap_or_default().into(),
        version: ByteString::new(),
        etag: ByteString::new(),
    };
    write_installed_extension(destination, name, metadata, database, |output| {
        let backend = roastedduck_storage::file_system::DatabaseFileSystem::for_path(&file);
        let mut input = if is_remote_file(&file) && backend.is_registered() {
            backend
                .open(
                    &file,
                    roastedduck_function::registered_files::OpenFlags {
                        read: true,
                        write: false,
                        create: false,
                        truncate: false,
                    },
                )
                .map_err(source_io_error)?
        } else {
            roastedduck_storage::file_system::StorageFile::Local(
                std::fs::File::open(native_path(&file)?).map_err(source_io_error)?,
            )
        };
        let mut magic = [0; 10];
        let count = input.read(&mut magic).map_err(source_io_error)?;
        input.seek(SeekFrom::Start(0)).map_err(source_io_error)?;
        if count == 10 && magic[..3] == [0x1f, 0x8b, 8] {
            crate::extension_http::decompress(&mut input, output)
        } else {
            std::io::copy(&mut input, output).map_err(source_io_error)?;
            Ok(())
        }
    })
}

/// Stage and validate all bytes before publishing metadata and then the native file.
fn write_installed_extension(
    destination: &[u8],
    name: &[u8],
    mut metadata: InstallInfo,
    database: &crate::Database,
    write: impl FnOnce(&mut std::fs::File) -> Result<()>,
) -> Result<()> {
    let mut random = [0; 16];
    roastedduck_crypto::generate_random_data(&mut random)?;
    let mut token = String::with_capacity(32);
    for byte in random {
        token.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        token.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
    }
    let temporary = byte_format!("{destination}.tmp-{token}.duckdb_extension");
    let temporary_info = byte_format!("{temporary}.info");
    let info_destination = byte_format!("{destination}.info");
    let perform = || -> Result<()> {
        let create = |path: &[u8]| -> Result<std::fs::File> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).read(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o666);
            }
            options
                .open(native_path(path)?)
                .map_err(|error| Error::new(ErrorKind::Io, error.to_string()))
        };
        let mut output = create(&temporary)?;
        write(&mut output)?;
        output
            .sync_all()
            .map_err(|error| Error::new(ErrorKind::Io, error.to_string()))?;
        drop(output);
        metadata.version = crate::external_extensions::validate_install(database, &temporary, name)?;
        let bytes = roastedduck_serde::binary::BinarySerializer::serialize(
            &metadata,
            roastedduck_serde::SerializationOptions::default(),
        )?;
        let mut info = create(&temporary_info)?;
        info.write_all(&bytes)
            .map_err(|error| Error::new(ErrorKind::Io, error.to_string()))?;
        info.sync_all()
            .map_err(|error| Error::new(ErrorKind::Io, error.to_string()))?;
        drop(info);
        std::fs::rename(native_path(&temporary_info)?, native_path(&info_destination)?)
            .map_err(|error| Error::new(ErrorKind::Io, error.to_string()))?;
        std::fs::rename(native_path(&temporary)?, native_path(destination)?)
            .map_err(|error| Error::new(ErrorKind::Io, error.to_string()))?;
        Ok(())
    };
    let result = perform();
    if let Ok(path) = native_path(&temporary) {
        let _ = std::fs::remove_file(path);
    }
    if let Ok(path) = native_path(&temporary_info) {
        let _ = std::fs::remove_file(path);
    }
    result
}

/// `InstallFromHttpUrl`, using C++'s retry statuses and nonredirecting transport.
fn install_from_http_url(
    url: &[u8],
    name: &[u8],
    destination: &[u8],
    repository: Option<&[u8]>,
    database: &crate::Database,
    force: bool,
    use_etags: bool,
) -> Result<()> {
    use std::sync::atomic::Ordering;
    let info_path = byte_format!("{destination}.info");
    let installed = if native_path(&info_path)?.is_file() {
        match read_install_info(&info_path, name) {
            Ok(info) => Some(info),
            Err(error) if !force => return Err(error),
            Err(_) => None,
        }
    } else {
        None
    };
    let mut last = Err("Could not establish connection".to_owned());
    for attempt in 0..=3 {
        if attempt >= 2 {
            std::thread::sleep(std::time::Duration::from_millis(if attempt == 2 {
                100
            } else {
                400
            }));
        }
        let etag = installed
            .as_ref()
            .filter(|info| use_etags && !info.etag.is_empty())
            .map(|info| info.etag.as_bytes());
        let mut response = match crate::extension_http::get(url, database, etag) {
            Ok(response) => response,
            Err(error) => {
                last = Err(error);
                continue;
            }
        };
        let status = response.status;
        if status == 304 && installed.is_some() {
            return Ok(());
        }
        if (200..300).contains(&status) || status == 304 {
            let metadata = InstallInfo {
                mode: if repository.is_some() {
                    InstallMode::Repository
                } else {
                    InstallMode::CustomPath
                },
                full_path: url.into(),
                repository_url: repository.unwrap_or_default().into(),
                version: ByteString::new(),
                etag: response.etag,
            };
            let result = write_installed_extension(destination, name, metadata, database, |output| {
                crate::extension_http::decompress(&mut response.body, output)
            });
            if !response.failed.load(Ordering::Relaxed) {
                return result;
            }
            last = Err("Failed to read connection".to_owned());
            continue;
        }
        // Failed downloads expose their response body through HTTPException.
        // Successful extension data above remains streamed to its destination.
        let mut body = Vec::new();
        if response.body.read_to_end(&mut body).is_err() {
            last = Err("Failed to read connection".to_owned());
            continue;
        }
        last = Ok((status, response.reason, response.headers, body));
        if !matches!(status, 408 | 418 | 429 | 500 | 503 | 504) {
            break;
        }
    }
    let (mut message, _) = create_suggestions(name);
    message.push_bytes("\nFor more info, visit ");
    message.push_bytes(documentation_link(name));
    Err(match last {
        Err(request_error) => Error::new_bytes(
            ErrorKind::Io,
            byte_format!(
                "Failed to download extension \"{name}\" at URL \"{url}\"\n{message} (ERROR {request_error})"
            ),
        ),
        Ok((status, reason, headers, body)) => Error::new_bytes(
            ErrorKind::Http,
            byte_format!(
                "Failed to download extension \"{name}\" at URL \"{url}\" (HTTP {status})\n{message}"
            ),
        )
        .with_http_response(status, body, headers, reason),
    })
}

/// Runs `LOAD` / `INSTALL`.
/// Returns the name of the statically linked extension `LOAD` refers to, which the
/// caller loads.
pub(crate) fn load_extension(
    info: &LoadInfo,
    config: &ExtensionConfig,
    database: &crate::Database,
) -> Result<Option<String>> {
    check_repository_alias(info)?;
    if info.load_type != LoadType::Load {
        install_extension(info, config, database)?;
        return Ok(None);
    }
    let extension = &info.filename;
    let filename = if is_full_path(extension) {
        extension.clone()
    } else {
        let name = apply_extension_alias(extension);
        if LINKED_EXTENSIONS
            .iter()
            .any(|linked| linked.as_bytes() == name.as_bytes())
        {
            return Ok(LINKED_EXTENSIONS
                .iter()
                .find(|linked| linked.as_bytes() == name.as_bytes())
                .map(|linked| (*linked).to_owned()));
        }
        config.extension_path(&name)?
    };
    // `TryInitialLoad`: external extensions need external access.
    if !roastedduck_function::file_system::external_access_enabled() {
        return Err(Error::new_bytes(
            ErrorKind::Permission,
            b"Loading external extensions is disabled through configuration",
        ));
    }
    if !filename.ends_with(b".duckdb_extension") {
        return Err(Error::new_bytes(
            ErrorKind::Permission,
            byte_format!(
                "DuckDB extensions are files ending with '.duckdb_extension', loading different files is not possible, error while loading from '{filename}', consider 'INSTALL <path>; LOAD <name>;'"
            ),
        ));
    }
    if !native_path(&filename)?.is_file() {
        let (mut message, exact_match) = create_suggestions(extension);
        if exact_match {
            message.push_bytes("\nInstall it first using \"INSTALL ");
            message.push_bytes(extension);
            message.push_bytes("\".");
        }
        return Err(Error::new_bytes(
            ErrorKind::Io,
            byte_format!("Extension \"{filename}\" not found.\n{message}"),
        ));
    }
    crate::external_extensions::load(database, &filename, extension, is_full_path(extension))?;
    Ok(None)
}

/// The version-only check used by C++ `UpdateExtensionInternal`: an old file is
/// inspected, never loaded, and only its footer magic determines validity here.
fn update_previous_version(filename: &[u8], name: &[u8], allow_mismatch: bool) -> Result<ByteString> {
    let io_error = |error: std::io::Error| Error::new(ErrorKind::Io, error.to_string());
    let mut file = std::fs::File::open(native_path(filename)?).map_err(io_error)?;
    if file.metadata().map_err(io_error)?.len() < 512 {
        return Err(Error::invalid_input(byte_format!(
            "File '{filename}' is not a DuckDB extension. Valid DuckDB extensions must be at least 512 bytes"
        )));
    }
    file.seek(SeekFrom::End(-512)).map_err(io_error)?;
    let mut footer = [0; 512];
    file.read_exact(&mut footer).map_err(io_error)?;
    let field = |index: usize| {
        let bytes = &footer[index * 32..(index + 1) * 32];
        &bytes[..bytes
            .iter()
            .rposition(|&byte| byte != 0)
            .map_or(0, |last| last + 1)]
    };
    if field(7) == b"4" {
        return Ok(field(4).into());
    }
    if allow_mismatch {
        return Ok(ByteString::new());
    }
    Err(Error::new_bytes(
        ErrorKind::Io,
        byte_format!(
            "Failed to update extension: '{name}', the metadata of the extension appears invalid! To resolve this, either reinstall the extension using 'FORCE INSTALL {name}', manually remove the file '{filename}', or enable 'SET allow_extensions_metadata_mismatch=true'"
        ),
    ))
}

fn update_row(
    name: &[u8],
    repository: &[u8],
    tag: &str,
    previous: &[u8],
    current: &[u8],
) -> Result<Vec<roastedduck_types::Value>> {
    use roastedduck_types::Value;
    Ok(vec![
        Value::try_varchar(name)?,
        Value::try_varchar(repository)?,
        Value::varchar(tag),
        Value::try_varchar(previous)?,
        Value::try_varchar(current)?,
    ])
}

fn update_extension(
    name: &[u8],
    config: &ExtensionConfig,
    database: &crate::Database,
    allow_mismatch: bool,
    explicit: bool,
) -> Result<Vec<roastedduck_types::Value>> {
    let filename = byte_format!("{}/{name}.duckdb_extension", config.directories[0]);
    if !native_path(&filename)?.is_file() {
        if explicit {
            return Err(Error::invalid_input(byte_format!(
                "Failed to update the extension '{name}', the extension is not installed!"
            )));
        }
        return update_row(name, b"", "NOT_INSTALLED", b"", b"");
    }
    let info_path = byte_format!("{filename}.info");
    if !native_path(&info_path)?.is_file() {
        return update_row(name, b"", "MISSING_INSTALL_INFO", b"", b"");
    }
    let previous = update_previous_version(&filename, name, allow_mismatch)?;
    let info = read_install_info(&info_path, name)?;
    if info.mode == InstallMode::Unknown {
        return update_row(name, b"", "MISSING_INSTALL_INFO", &previous, b"");
    }
    if info.mode != InstallMode::Repository {
        return update_row(name, b"", "NOT_A_REPOSITORY", &previous, &previous);
    }
    let load = LoadInfo {
        filename: name.into(),
        load_type: LoadType::ForceInstall,
        repository: info.repository_url.clone(),
        version: ByteString::new(),
        repo_is_alias: false,
    };
    install_extension_options(&load, config, database, true).map_err(|error| {
        Error::new_bytes(
            error.kind(),
            byte_format!(
                "Extension updating failed when trying to install '{name}', original error: {}",
                error.raw_message_bytes()
            ),
        )
    })?;
    let current = read_install_info(&info_path, name)?.version;
    let tag = if current.is_empty() {
        "REDOWNLOADED"
    } else if current != previous {
        "UPDATED"
    } else {
        "NO_UPDATE_AVAILABLE"
    };
    update_row(
        name,
        &repository_name(&info.repository_url),
        tag,
        &previous,
        &current,
    )
}

/// `UPDATE EXTENSIONS`, including explicit names and the primary-directory scan.
pub(crate) fn update_extensions(
    info: &roastedduck_ast::UpdateExtensionsInfo,
    config: &ExtensionConfig,
    database: &crate::Database,
    allow_mismatch: bool,
) -> Result<Vec<Vec<roastedduck_types::Value>>> {
    config.create_directory()?;
    if !info.extensions_to_update.is_empty() {
        return info
            .extensions_to_update
            .iter()
            .map(|name| update_extension(name, config, database, allow_mismatch, true))
            .collect();
    }
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&config.directories[0]) {
        for entry in entries {
            let entry = entry.map_err(|error| Error::new(ErrorKind::Io, error.to_string()))?;
            let filename = entry.file_name();
            let bytes = filename.as_encoded_bytes();
            if !bytes.ends_with(b".duckdb_extension") {
                continue;
            }
            let name = bytes.split(|&byte| byte == b'.').next().unwrap_or_default();
            result.push(update_extension(name, config, database, allow_mismatch, false)?);
        }
    }
    Ok(result)
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
    full_path: ByteString,
    repository_url: ByteString,
    version: ByteString,
    etag: ByteString,
}

impl roastedduck_serde::SerializeObject for InstallInfo {
    fn serialize_fields(&self, serializer: &mut dyn roastedduck_serde::Serializer) -> Result<()> {
        serializer.write_property(100, "mode", &(self.mode as u8))?;
        serializer.write_property_with_default(101, "full_path", &self.full_path)?;
        serializer.write_property_with_default(102, "repository_url", &self.repository_url)?;
        serializer.write_property_with_default(103, "version", &self.version)?;
        serializer.write_property_with_default(104, "etag", &self.etag)
    }
}

pub(crate) fn validate_installed_metadata(filename: &[u8], name: &[u8], version: &[u8]) -> Result<()> {
    let info = read_install_info(byte_format!("{filename}.info"), name)?;
    if info.mode != InstallMode::Unknown && info.version.as_bytes() != version {
        return Err(Error::new_bytes(
            ErrorKind::Io,
            byte_format!(
                "Metadata mismatch detected when loading extension '{filename}'\nPlease try reinstalling the extension using `FORCE INSTALL '{name}'`"
            ),
        ));
    }
    Ok(())
}

impl roastedduck_serde::DeserializeObject for InstallInfo {
    fn deserialize_fields(deserializer: &mut dyn roastedduck_serde::Deserializer) -> Result<Self> {
        let mode: u8 = deserializer.read_property(100, "mode")?;
        let full_path: ByteString = deserializer.read_property_with_default(101, "full_path")?;
        let repository_url: ByteString = deserializer.read_property_with_default(102, "repository_url")?;
        let version: ByteString = deserializer.read_property_with_default(103, "version")?;
        let etag: ByteString = deserializer.read_property_with_default(104, "etag")?;
        Ok(Self {
            mode: InstallMode::from_number(mode).unwrap_or(InstallMode::Unknown),
            full_path,
            repository_url,
            version,
            etag,
        })
    }
}

/// `ExtensionInstallInfo::TryReadInfoFile`.
fn read_install_info(path: impl AsRef<[u8]>, name: impl AsRef<[u8]>) -> Result<InstallInfo> {
    let path = path.as_ref();
    let name = name.as_ref();
    let missing = || InstallInfo {
        mode: InstallMode::Unknown,
        full_path: ByteString::new(),
        repository_url: ByteString::new(),
        version: ByteString::new(),
        etag: ByteString::new(),
    };
    let Ok(bytes) = std::fs::read(native_path(path)?) else {
        return Ok(missing());
    };
    let hint = byte_format!("Try reinstalling the extension using 'FORCE INSTALL {name};'");
    if bytes.is_empty() {
        return Err(Error::new_bytes(
            ErrorKind::Io,
            byte_format!(
                "Failed to read info file for '{name}' extension: '{path}'.\nThe file appears to be empty!\n{hint}"
            ),
        ));
    }
    roastedduck_serde::binary::BinaryDeserializer::deserialize::<InstallInfo>(&bytes).map_err(|error| {
        Error::new_bytes(
            ErrorKind::Io,
            byte_format!(
                "Failed to read info file for '{name}' extension: '{path}'.\nA serialization error occurred: '{}'\n{hint}",
                error.raw_message_bytes()
            ),
        )
    })
}

/// `ExtensionRepository::GetRepository`: the name of a known repository URL
/// (`TryConvertUrlToKnownRepository`).
fn repository_name(url: &[u8]) -> ByteString {
    match url {
        b"http://extensions.duckdb.org" => "core".into(),
        b"http://nightly-extensions.duckdb.org" => "core_nightly".into(),
        b"http://community-extensions.duckdb.org" => "community".into(),
        b"./build/release/repository" => "local_build_release".into(),
        b"./build/debug/repository" => "local_build_debug".into(),
        other => other.into(),
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
    external_extensions: &[crate::external_extensions::ExternalExtensionRecord],
) -> Result<Vec<Vec<roastedduck_types::Value>>> {
    use roastedduck_types::{LogicalType, Value};
    struct Information {
        loaded: bool,
        installed: bool,
        file_path: ByteString,
        install_mode: InstallMode,
        installed_from: ByteString,
        description: String,
        aliases: Vec<Value>,
        extension_version: ByteString,
    }
    let version = roastedduck_storage::storage_info::LIBRARY_VERSION;
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
                    "(BUILT-IN)".into()
                } else {
                    ByteString::new()
                },
                install_mode: if linked {
                    InstallMode::StaticallyLinked
                } else {
                    InstallMode::NotInstalled
                },
                installed_from: ByteString::new(),
                description: description.to_owned(),
                aliases: ALIASES
                    .iter()
                    .filter(|(_, extension)| *extension == name)
                    .map(|(alias, _)| Value::varchar(*alias))
                    .collect(),
                // A loaded built-in extension reports the library version.
                extension_version: if loaded { version.into() } else { ByteString::new() },
            },
        );
    }
    for (name, description) in client_extensions {
        extensions.insert(
            name.clone(),
            Information {
                loaded: true,
                installed: true,
                file_path: "(BUILT-IN)".into(),
                install_mode: InstallMode::StaticallyLinked,
                installed_from: ByteString::new(),
                description: description.clone(),
                aliases: Vec::new(),
                extension_version: version.into(),
            },
        );
    }
    for directory in &config.directories {
        // `fs.ListFiles` through the client's file system.
        roastedduck_function::file_system::verify_can_access_directory(directory)?;
        if let Ok(entries) = std::fs::read_dir(directory) {
            let mut files: Vec<String> = entries
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .filter(|file| file.ends_with(".duckdb_extension"))
                .collect();
            files.sort();
            for file in files {
                let name = file.trim_end_matches(".duckdb_extension").to_owned();
                let info = read_install_info(format!("{directory}/{file}.info"), &name)?;
                let installed_from = if info.mode == InstallMode::Repository {
                    repository_name(&info.repository_url)
                } else {
                    info.full_path.clone()
                };
                let file_path = byte_format!("{directory}/{file}");
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
    }
    for external in external_extensions {
        let name = std::str::from_utf8(&external.name)
            .map_err(|_| Error::invalid_input("Invalid UTF8 in extension name"))?;
        let entry = extensions.entry(name.to_owned()).or_insert_with(|| Information {
            loaded: false,
            installed: false,
            file_path: ByteString::new(),
            install_mode: InstallMode::NotInstalled,
            installed_from: ByteString::new(),
            description: String::new(),
            aliases: Vec::new(),
            extension_version: ByteString::new(),
        });
        entry.loaded = true;
        entry.extension_version = external.version.clone();
    }
    extensions
        .into_iter()
        .map(|(name, info)| {
            Ok(vec![
                Value::varchar(name),
                Value::boolean(info.loaded),
                Value::boolean(info.installed),
                Value::try_varchar(&info.file_path)?,
                Value::varchar(info.description),
                Value::list(LogicalType::VARCHAR, info.aliases),
                Value::try_varchar(&info.extension_version)?,
                Value::varchar(info.install_mode.name()),
                Value::try_varchar(&info.installed_from)?,
            ])
        })
        .collect::<Result<_>>()
}

#[cfg(test)]
mod install_tests {
    use super::*;
    #[test]
    fn install_info_mode_is_required_like_cpp() -> Result<()> {
        let error = roastedduck_serde::binary::BinaryDeserializer::deserialize::<InstallInfo>(
            b"not a valid info file",
        )
        .err()
        .ok_or_else(|| Error::internal("corrupt metadata must fail"))?;
        assert_eq!(
            error.raw_message(),
            "Failed to deserialize: field id mismatch, expected: 100, got: 28526"
        );
        Ok(())
    }

    #[test]
    fn local_install_info_matches_native_cpp_bytes() -> Result<()> {
        let bytes = include_bytes!("../tests/data/capi-extension-install-reference.info");
        let info = roastedduck_serde::binary::BinaryDeserializer::deserialize::<InstallInfo>(bytes)?;
        assert_eq!(info.mode, InstallMode::CustomPath);
        assert_eq!(info.version.as_bytes(), b"0.0.1");
        assert_eq!(
            info.full_path.as_bytes(),
            b"/artifacts/linux-amd64/capi-extension-control-build-338/c_only_probe.duckdb_extension"
        );
        assert!(info.repository_url.is_empty());
        let encoded = roastedduck_serde::binary::BinarySerializer::serialize(
            &info,
            roastedduck_serde::SerializationOptions::default(),
        )?;
        assert_eq!(encoded, bytes);
        Ok(())
    }
}
