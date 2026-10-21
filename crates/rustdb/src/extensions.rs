//! `LOAD` / `INSTALL` (DuckDB's `ExtensionHelper::LoadExternalExtension` and
//! `InstallExtension`). The extensions DuckDB links statically are built in; loading
//! them does nothing. Extension files cannot be loaded.

use rustdb_ast::{LoadInfo, LoadType};
use rustdb_common::string_util::{candidates_message, top_n_jaro_winkler};
use rustdb_common::{Error, ErrorKind, Result};

/// The extensions DuckDB knows, with their descriptions (`internal_extensions`).
const DEFAULT_EXTENSIONS: [(&str, &str); 30] = [
    ("core_functions", "Core function library"),
    ("icu", "Adds support for time zones and collations using the ICU library"),
    ("excel", "Adds support for Excel-like format strings"),
    ("parquet", "Adds support for reading and writing parquet files"),
    ("tpch", "Adds TPC-H data generation and query support"),
    ("tpcds", "Adds TPC-DS data generation and query support"),
    ("httpfs", "Adds support for reading and writing files over a HTTP(S) connection"),
    ("json", "Adds support for JSON operations"),
    ("autocomplete", "Adds support for autocomplete in the shell"),
    ("motherduck", "Enables motherduck integration with the system"),
    ("mysql_scanner", "Adds support for connecting to a MySQL database"),
    ("odbc_scanner", "Adds support for connecting to remote databases over ODBC"),
    ("sqlite_scanner", "Adds support for reading and writing SQLite database files"),
    ("postgres_scanner", "Adds support for connecting to a Postgres database"),
    ("inet", "Adds support for IP-related data types and functions"),
    ("spatial", "Geospatial extension that adds support for working with spatial data and functions"),
    ("aws", "Provides features that depend on the AWS SDK"),
    ("azure", "Adds a filesystem abstraction for Azure blob storage to DuckDB"),
    ("encodings", "All unicode encodings to UTF-8"),
    ("iceberg", "Adds support for Apache Iceberg"),
    ("vss", "Adds indexing support to accelerate Vector Similarity Search"),
    ("delta", "Adds support for Delta Lake"),
    ("fts", "Adds support for Full-Text Search Indexes"),
    ("ui", "Adds local UI for DuckDB"),
    ("ducklake", "Adds support for DuckLake, SQL as a Lakehouse Format"),
    ("quack", "The DuckDB 'Quack' Client/Server Protocol"),
    ("vortex", "Adds support for reading and writing files using the Vortex file format"),
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
    let mut candidates: Vec<String> = DEFAULT_EXTENSIONS.iter().map(|(name, _)| (*name).to_owned()).collect();
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
            format!("Failed to read info file for '{name}' extension: '{path}'.\nThe file appears to be empty!\n{hint}"),
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
/// `is_loaded` tells whether a statically linked extension is loaded.
pub(crate) fn extension_rows(is_loaded: impl Fn(&str) -> bool) -> Result<Vec<Vec<rustdb_types::Value>>> {
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
                file_path: if linked { "(BUILT-IN)".to_owned() } else { String::new() },
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
    let directory = extension_path("");
    let directory = directory.rsplit_once('/').map_or("", |(directory, _)| directory);
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
