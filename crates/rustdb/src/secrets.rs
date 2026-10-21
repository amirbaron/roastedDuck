//! Secrets (DuckDB's `SecretManager`, `KeyValueSecret` and the `memory` / `local_file`
//! secret storages). The core build knows the `http` secret type with its `config` and
//! `env` providers; the types of extensions are reported as such.
//!
//! Gap: secrets are not transactional (DuckDB keeps them in catalog sets), and the
//! `local_file` storage writes its own serialization of the secret map (DuckDB writes a
//! `MAP(VARCHAR, ANY)` value).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rustdb_ast::{OnCreateConflict, OnEntryNotFound, SecretPersistType};
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_function::cast::ValueCast;
use rustdb_function::logging::FileSystemLogger;
use rustdb_planner::operator::CreateSecretInput;
use rustdb_serde::binary::{BinaryDeserializer, BinarySerializer};
use rustdb_serde::{DeserializeObject, Deserializer, SerializationOptions, SerializeObject, Serializer};
use rustdb_types::{LogicalType, Value};

/// The storage of temporary secrets (`SecretManager::TEMPORARY_STORAGE_NAME`).
pub(crate) const TEMPORARY_STORAGE: &str = "memory";
/// The storage of persistent secrets (`SecretManager::LOCAL_FILE_STORAGE_NAME`).
pub(crate) const LOCAL_FILE_STORAGE: &str = "local_file";
/// The extension of persistent secret files.
const SECRET_FILE_EXTENSION: &str = ".duckdb_secret";

/// `EXTENSION_SECRET_TYPES`.
const EXTENSION_SECRET_TYPES: [(&str, &str); 14] = [
    ("aws", "httpfs"),
    ("azure", "azure"),
    ("ducklake", "ducklake"),
    ("gcs", "httpfs"),
    ("huggingface", "httpfs"),
    ("iceberg", "iceberg"),
    ("mysql", "mysql_scanner"),
    ("postgres", "postgres_scanner"),
    ("quack", "quack"),
    ("r2", "httpfs"),
    ("rds", "postgres_scanner"),
    ("s3", "httpfs"),
    ("uc", "unity_catalog"),
    ("unity_catalog", "unity_catalog"),
];

/// `EXTENSION_SECRET_PROVIDERS` (DuckDB spells one extension `httfps`).
const EXTENSION_SECRET_PROVIDERS: [(&str, &str); 17] = [
    ("s3/config", "httpfs"),
    ("gcs/config", "httpfs"),
    ("r2/config", "httpfs"),
    ("s3/credential_chain", "aws"),
    ("gcs/credential_chain", "aws"),
    ("r2/credential_chain", "aws"),
    ("aws/credential_chain", "aws"),
    ("rds/credential_chain", "aws"),
    ("azure/access_token", "azure"),
    ("azure/config", "azure"),
    ("azure/credential_chain", "azure"),
    ("azure/service_principal", "azure"),
    ("huggingface/config", "httfps"),
    ("huggingface/credential_chain", "httpfs"),
    ("bearer/config", "httpfs"),
    ("mysql/config", "mysql_scanner"),
    ("postgres/config", "postgres_scanner"),
];

/// The named parameters of the `http` secret functions (`CreateHTTPSecretFunctions`).
fn http_parameter_type(name: &str) -> Option<LogicalType> {
    Some(match name {
        "verify_ssl" => LogicalType::BOOLEAN,
        "http_proxy" | "http_proxy_password" | "http_proxy_username" | "bearer_token" => LogicalType::VARCHAR,
        "extra_http_headers" => LogicalType::map(LogicalType::VARCHAR, LogicalType::VARCHAR),
        _ => return None,
    })
}

/// A secret as a map of keys to values (DuckDB's `KeyValueSecret`).
#[derive(Clone, Debug)]
pub(crate) struct KeyValueSecret {
    pub name: String,
    pub secret_type: String,
    pub provider: String,
    /// The path prefixes the secret applies to (`prefix_paths`).
    pub scope: Vec<String>,
    pub map: BTreeMap<String, Value>,
    /// Keys whose values are shown as `redacted`.
    pub redact_keys: BTreeSet<String>,
}

impl KeyValueSecret {
    /// `KeyValueSecret::ToString`.
    pub fn to_display_string(&self, redact: bool) -> String {
        let mut result = format!(
            "name={};type={};provider={};serializable=true;scope=",
            self.name, self.secret_type, self.provider
        );
        for prefix in &self.scope {
            result.push_str(prefix);
            result.push(',');
        }
        result.pop();
        result.push(';');
        let entries: Vec<String> = self
            .map
            .iter()
            .map(|(key, value)| {
                let shown = if redact && self.redact_keys.contains(key) {
                    "redacted".to_owned()
                } else {
                    value.to_varchar().unwrap_or_default()
                };
                format!("{key}={shown}")
            })
            .collect();
        result.push_str(&entries.join(";"));
        result
    }

    /// `BaseSecret::MatchScore`: the length of the longest scope prefix of `path`.
    fn match_score(&self, path: &str) -> Option<i64> {
        if self.scope.is_empty() {
            return Some(0);
        }
        self.scope
            .iter()
            .filter(|prefix| prefix.is_empty() || path.starts_with(prefix.as_str()))
            .map(|prefix| i64::try_from(prefix.len()).unwrap_or(i64::MAX))
            .max()
    }
}

impl SerializeObject for KeyValueSecret {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_property(100, "type", &self.secret_type)?;
        serializer.write_property(101, "provider", &self.provider)?;
        serializer.write_property(102, "name", &self.name)?;
        serializer.write_property(103, "scope", &self.scope)?;
        let entries: Vec<(&String, &Value)> = self.map.iter().collect();
        serializer.write_list_property(201, "secret_map", entries.len(), |serializer, index| {
            let (key, value) = entries[index];
            serializer.on_object_begin();
            serializer.write_property(100, "key", key)?;
            serializer.write_property(101, "value", value)?;
            serializer.on_object_end();
            Ok(())
        })?;
        let redact: Vec<String> = self.redact_keys.iter().cloned().collect();
        serializer.write_property(202, "redact_keys", &redact)
    }
}

impl DeserializeObject for KeyValueSecret {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let secret_type: String = deserializer.read_property(100, "type")?;
        let provider: String = deserializer.read_property(101, "provider")?;
        let name: String = deserializer.read_property(102, "name")?;
        let scope: Vec<String> = deserializer.read_property(103, "scope")?;
        let mut map = BTreeMap::new();
        deserializer.read_list_property(201, "secret_map", |deserializer, _| {
            deserializer.on_object_begin()?;
            let key: String = deserializer.read_property(100, "key")?;
            let value: Value = deserializer.read_property(101, "value")?;
            deserializer.on_object_end()?;
            map.insert(key, value);
            Ok(())
        })?;
        let redact: Vec<String> = deserializer.read_property(202, "redact_keys")?;
        Ok(Self {
            name,
            secret_type,
            provider,
            scope,
            map,
            redact_keys: redact.into_iter().collect(),
        })
    }
}

/// A secret with where it is stored (DuckDB's `SecretEntry`).
#[derive(Clone, Debug)]
pub(crate) struct SecretEntry {
    pub secret: KeyValueSecret,
    pub persistent: bool,
    pub storage: String,
}

/// The `local_file` storage: one file per secret, read when first needed
/// (`LocalFileSecretStorage` with its `DefaultSecretGenerator`).
#[derive(Debug)]
struct LocalFileStorage {
    path: String,
    /// Secrets by lower-cased name; `None` until the file was read.
    secrets: BTreeMap<String, Option<KeyValueSecret>>,
}

impl LocalFileStorage {
    fn new(path: &str) -> Self {
        let mut secrets = BTreeMap::new();
        if let Ok(files) = std::fs::read_dir(path) {
            for file in files.flatten() {
                let file_name = file.file_name().to_string_lossy().into_owned();
                if let Some(name) = file_name.strip_suffix(SECRET_FILE_EXTENSION) {
                    secrets.insert(name.to_lowercase(), None);
                }
            }
        }
        Self {
            path: path.to_owned(),
            secrets,
        }
    }

    fn file_path(&self, name: &str) -> String {
        std::path::Path::new(&self.path)
            .join(format!("{name}{SECRET_FILE_EXTENSION}"))
            .to_string_lossy()
            .into_owned()
    }

    /// The secret `name`, read from its file if needed.
    fn get(&mut self, name: &str) -> Result<Option<KeyValueSecret>> {
        let key = name.to_lowercase();
        let Some(slot) = self.secrets.get(&key) else {
            return Ok(None);
        };
        if let Some(secret) = slot {
            return Ok(Some(secret.clone()));
        }
        let path = self.file_path(&key);
        // `BufferedFileReader`: one read of the whole file.
        let logger = FileSystemLogger::open(&path);
        let bytes = std::fs::read(&path);
        if let Some(logger) = &logger {
            if let Ok(bytes) = &bytes {
                logger.read(bytes.len() as u64, 0);
            }
            logger.close();
        }
        let bytes = bytes.map_err(|error| {
            Error::new(
                ErrorKind::Io,
                format!(
                    "Failed to open the persistent secret file: '{path}'. Some other process may have removed it, \
                     please restart and try again. (error message: '{error}')"
                ),
            )
        })?;
        if bytes.is_empty() {
            return Err(Error::new(
                ErrorKind::Serialization,
                format!(
                    "Failed to deserialize secret '{key}' from '{path}': file appears empty! Please remove the file, \
                     restart and try again"
                ),
            ));
        }
        let secret: KeyValueSecret = BinaryDeserializer::deserialize(&bytes).map_err(|error| {
            Error::new(
                ErrorKind::Serialization,
                format!(
                    "Failed to deserialize the persistent secret file: '{path}'. The file maybe be corrupt, please \
                     remove the file, restart and try again. (error message: '{}')",
                    error.raw_message()
                ),
            )
        })?;
        self.secrets.insert(key, Some(secret.clone()));
        Ok(Some(secret))
    }

    fn all(&mut self) -> Result<Vec<KeyValueSecret>> {
        let names: Vec<String> = self.secrets.keys().cloned().collect();
        let mut result = Vec::with_capacity(names.len());
        for name in names {
            result.extend(self.get(&name)?);
        }
        Ok(result)
    }

    /// `LocalFileSecretStorage::WriteSecret`: a private file, written to a temporary
    /// path first.
    fn write(&mut self, secret: &KeyValueSecret) -> Result<()> {
        std::fs::create_dir_all(&self.path).map_err(|error| {
            Error::new(
                ErrorKind::Io,
                format!("Failed to initialize persistent storage directory. (original error: '{error}')"),
            )
        })?;
        let key = secret.name.to_lowercase();
        let path = self.file_path(&key);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let temp_path = format!("{path}.tmp-{}-{nonce}", std::process::id());
        let bytes = BinarySerializer::serialize(secret, SerializationOptions::default())?;
        let io_error = |error: std::io::Error| {
            Error::new(ErrorKind::Io, format!("Could not write file \"{path}\": {error}"))
        };
        let _ = std::fs::remove_file(&path);
        // `BufferedFileWriter`: one write when flushed.
        let logger = FileSystemLogger::open(&temp_path);
        let written = write_private_file(&temp_path, &bytes);
        if let Some(logger) = &logger {
            if written.is_ok() {
                logger.write(bytes.len() as u64, 0);
            }
            logger.close();
        }
        written.map_err(io_error)?;
        std::fs::rename(&temp_path, &path).map_err(io_error)?;
        self.secrets.insert(key, Some(secret.clone()));
        Ok(())
    }

    fn remove(&mut self, name: &str) -> Result<()> {
        let key = name.to_lowercase();
        self.secrets.remove(&key);
        let file = self.file_path(&key);
        std::fs::remove_file(&file).map_err(|error| {
            Error::new(
                ErrorKind::Io,
                format!(
                    "Failed to remove secret file '{file}', the file may have been removed by another duckdb \
                     instance. (original error: '{error}')"
                ),
            )
        })
    }
}

/// Writes `bytes` to a new file only the owner can read (`FILE_FLAGS_PRIVATE`).
fn write_private_file(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// The secret manager's settings (DuckDB's `SecretManagerConfig`).
#[derive(Debug)]
struct SecretConfig {
    secret_path: String,
    default_secret_path: String,
    allow_persistent_secrets: bool,
    default_storage: String,
}

#[derive(Debug)]
struct ManagerState {
    config: SecretConfig,
    initialized: bool,
    temporary: BTreeMap<String, KeyValueSecret>,
    local_file: Option<LocalFileStorage>,
}

/// The secrets of a database instance (DuckDB's `SecretManager`).
#[derive(Debug)]
pub(crate) struct SecretManager {
    state: Mutex<ManagerState>,
}

impl Default for SecretManager {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let default_secret_path = std::path::Path::new(&home)
            .join(".duckdb")
            .join("stored_secrets")
            .to_string_lossy()
            .into_owned();
        Self {
            state: Mutex::new(ManagerState {
                config: SecretConfig {
                    secret_path: default_secret_path.clone(),
                    default_secret_path,
                    allow_persistent_secrets: true,
                    default_storage: LOCAL_FILE_STORAGE.to_owned(),
                },
                initialized: false,
                temporary: BTreeMap::new(),
                local_file: None,
            }),
        }
    }
}

/// `ExtensionHelper::AddExtensionInstallHintToErrorMsg` under the default settings (known
/// extensions are neither autoloaded nor autoinstalled).
fn install_hint(message: &str, extension: &str) -> String {
    rustdb_function::extension_entries::add_install_hint(message, extension, false, false)
}

/// `SecretManager::ThrowTypeNotFoundError`.
fn type_not_found(secret_type: &str) -> Error {
    let lower = secret_type.to_lowercase();
    match EXTENSION_SECRET_TYPES.iter().find(|(name, _)| *name == lower) {
        Some((_, extension)) => Error::invalid_input(install_hint(
            &format!(
                "Secret type '{secret_type}' does not exist, but it exists in the {extension} extension."
            ),
            extension,
        )),
        None => Error::invalid_input(format!("Secret type '{secret_type}' not found")),
    }
}

/// `SecretManager::ThrowProviderNotFoundError`.
fn provider_not_found(secret_type: &str, provider: &str, was_default: bool) -> Error {
    let key = format!("{}/{}", secret_type.to_lowercase(), provider.to_lowercase());
    match EXTENSION_SECRET_PROVIDERS.iter().find(|(name, _)| *name == key) {
        Some((_, extension)) => Error::invalid_input(install_hint(
            &format!(
                "{} '{provider}' for type '{secret_type}' does not exist, but it exists in the {extension} extension.",
                if was_default {
                    "Default secret provider"
                } else {
                    "Secret provider"
                }
            ),
            extension,
        )),
        None => Error::invalid_input(format!(
            "Secret provider '{provider}' not found for type '{secret_type}'"
        )),
    }
}

impl SecretManager {
    fn lock(&self) -> MutexGuard<'_, ManagerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `SET secret_directory` / `allow_persistent_secrets` / `default_secret_storage`
    /// (and their `RESET`, with `value` `None`).
    pub fn set_option(&self, name: &str, value: Option<&Value>) -> Result<()> {
        let mut state = self.lock();
        let lower = name.to_lowercase();
        if lower != "default_secret_storage" && state.initialized {
            return Err(Error::invalid_input(
                "Changing Secret Manager settings after the secret manager is used is not allowed!",
            ));
        }
        match lower.as_str() {
            "secret_directory" => {
                state.config.secret_path = match value {
                    Some(value) => value.to_varchar()?,
                    None => state.config.default_secret_path.clone(),
                };
            }
            "allow_persistent_secrets" => {
                state.config.allow_persistent_secrets = match value {
                    Some(value) => value
                        .default_cast_as(&LogicalType::BOOLEAN)?
                        .as_bool()
                        .unwrap_or(true),
                    None => true,
                };
            }
            "default_secret_storage" => {
                state.config.default_storage = match value {
                    Some(value) => value.to_varchar()?,
                    None => LOCAL_FILE_STORAGE.to_owned(),
                };
            }
            _ => {}
        }
        Ok(())
    }

    /// `SecretManager::InitializeSecrets`: the storages, on first use. Reading the
    /// persistent secret directory needs file system access.
    fn initialize(state: &mut ManagerState) -> Result<()> {
        if state.initialized {
            return Ok(());
        }
        if state.config.allow_persistent_secrets {
            let path = expand_home(&state.config.secret_path);
            rustdb_function::file_system::verify_can_access_directory(&path)?;
            state.local_file = Some(LocalFileStorage::new(&path));
        }
        state.initialized = true;
        Ok(())
    }

    /// `SecretManager::BindCreateSecret` and `CreateSecret`: runs the secret function of
    /// the type and provider, then stores the secret.
    pub fn create_secret(&self, input: &CreateSecretInput) -> Result<()> {
        let mut state = self.lock();
        Self::initialize(&mut state)?;
        // Without a provider the type must exist (its default provider is used);
        // otherwise the type's provider must.
        let was_default = input.provider.is_empty();
        let known_type = input.secret_type == "http";
        if was_default && !known_type {
            return Err(type_not_found(&input.secret_type));
        }
        let provider = if was_default {
            "config".to_owned()
        } else {
            input.provider.clone()
        };
        if !known_type || (provider != "config" && provider != "env") {
            return Err(provider_not_found(&input.secret_type, &provider, was_default));
        }
        let default_string = if was_default { "default " } else { "" };
        let mut options = BTreeMap::new();
        for (name, value) in &input.options {
            let lower = name.to_lowercase();
            let Some(ty) = http_parameter_type(&lower) else {
                return Err(Error::binder(format!(
                    "Unknown parameter '{name}' for secret type '{}' with {default_string}provider '{provider}'",
                    input.secret_type
                )));
            };
            let cast = value.default_cast_as(&ty).map_err(|error| {
                Error::binder(format!(
                    "Failed to cast option '{lower}' to type '{ty}': '{}'",
                    error.raw_message()
                ))
            })?;
            options.insert(lower, cast);
        }
        // `CreateHTTPSecretFromEnv` / `CreateHTTPSecretFromConfig`.
        let mut map = BTreeMap::new();
        if provider == "env" {
            for key in ["http_proxy", "http_proxy_password", "http_proxy_username"] {
                let found = std::env::var(key).or_else(|_| std::env::var(key.to_uppercase()));
                if let Ok(found) = found {
                    map.insert(key.to_owned(), Value::varchar(found));
                }
            }
        }
        map.extend(options);
        let secret = KeyValueSecret {
            name: input.name.clone(),
            secret_type: input.secret_type.clone(),
            provider,
            scope: input.scope.clone(),
            map,
            redact_keys: ["http_proxy_password", "bearer_token"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        };
        Self::register(&mut state, secret, input)
    }

    /// `SecretManager::RegisterSecretInternal` and `CatalogSetSecretStorage::StoreSecret`.
    fn register(state: &mut ManagerState, secret: KeyValueSecret, input: &CreateSecretInput) -> Result<()> {
        let mut persist_type = input.persist_type;
        if persist_type == SecretPersistType::Default {
            persist_type = if input.storage.is_empty() || input.storage == TEMPORARY_STORAGE {
                SecretPersistType::Temporary
            } else {
                SecretPersistType::Persistent
            };
        }
        let storage = if input.storage.is_empty() {
            if persist_type == SecretPersistType::Persistent {
                state.config.default_storage.clone()
            } else {
                TEMPORARY_STORAGE.to_owned()
            }
        } else {
            input.storage.clone()
        };
        let persistent_storage = match storage.as_str() {
            TEMPORARY_STORAGE => false,
            LOCAL_FILE_STORAGE if state.local_file.is_some() => true,
            _ => {
                if !state.config.allow_persistent_secrets
                    && (persist_type == SecretPersistType::Persistent || storage == LOCAL_FILE_STORAGE)
                {
                    return Err(Error::invalid_input(
                        "Persistent secrets are disabled. Restart DuckDB and enable persistent secrets through 'SET \
                         allow_persistent_secrets=true'",
                    ));
                }
                return Err(Error::invalid_input(format!(
                    "Secret storage '{storage}' not found!"
                )));
            }
        };
        if persist_type == SecretPersistType::Persistent && !persistent_storage {
            return Err(Error::invalid_input(
                "Cannot create persistent secrets in a temporary secret storage!",
            ));
        }
        if persist_type == SecretPersistType::Temporary && persistent_storage {
            return Err(Error::invalid_input(
                "Cannot create temporary secrets in a persistent secret storage!",
            ));
        }
        let key = secret.name.to_lowercase();
        let exists = if persistent_storage {
            state
                .local_file
                .as_ref()
                .is_some_and(|local| local.secrets.contains_key(&key))
        } else {
            state.temporary.contains_key(&key)
        };
        if exists {
            match input.on_conflict {
                OnCreateConflict::IgnoreOnConflict => return Ok(()),
                OnCreateConflict::ReplaceOnConflict => {}
                _ => {
                    let (kind, storage_string) = if persistent_storage {
                        ("Persistent", format!(" in secret storage '{storage}'"))
                    } else {
                        ("Temporary", String::new())
                    };
                    return Err(Error::invalid_input(format!(
                        "{kind} secret with name '{}' already exists{storage_string}!",
                        secret.name
                    )));
                }
            }
        }
        if persistent_storage {
            if let Some(local) = state.local_file.as_mut() {
                local.write(&secret)?;
            }
        } else {
            state.temporary.insert(key, secret);
        }
        Ok(())
    }

    /// `SecretManager::DropSecretByName`.
    pub fn drop_secret(
        &self,
        name: &str,
        if_not_found: OnEntryNotFound,
        persist_type: SecretPersistType,
        storage: &str,
    ) -> Result<()> {
        let mut state = self.lock();
        Self::initialize(&mut state)?;
        let key = name.to_lowercase();
        let in_local = state
            .local_file
            .as_ref()
            .is_some_and(|local| local.secrets.contains_key(&key));
        let in_memory = state.temporary.contains_key(&key);
        let mut matches = Vec::new();
        if storage.is_empty() {
            if in_local && persist_type != SecretPersistType::Temporary {
                matches.push(LOCAL_FILE_STORAGE);
            }
            if in_memory && persist_type != SecretPersistType::Persistent {
                matches.push(TEMPORARY_STORAGE);
            }
        } else {
            match storage {
                LOCAL_FILE_STORAGE if state.local_file.is_some() => {
                    if in_local {
                        matches.push(LOCAL_FILE_STORAGE);
                    } else if if_not_found == OnEntryNotFound::ThrowException {
                        return Err(Error::invalid_input(format!(
                            "Failed to remove non-existent persistent secret '{name}' in secret storage \
                             '{LOCAL_FILE_STORAGE}'"
                        )));
                    }
                }
                TEMPORARY_STORAGE => {
                    if in_memory {
                        matches.push(TEMPORARY_STORAGE);
                    } else if if_not_found == OnEntryNotFound::ThrowException {
                        return Err(Error::invalid_input(format!(
                            "Failed to remove non-existent temporary secret '{name}'"
                        )));
                    }
                }
                _ => {
                    return Err(Error::invalid_input(format!(
                        "Unknown storage type found for drop secret: '{storage}'"
                    )));
                }
            }
        }
        if matches.len() > 1 {
            return Err(Error::invalid_input(format!(
                "Ambiguity found for secret name '{name}', secret occurs in multiple storages: [{}] Please specify \
                 which secret to drop using: 'DROP <PERSISTENT|TEMPORARY> SECRET [FROM <storage>]'.",
                matches.join(",")
            )));
        }
        match matches.first() {
            None if storage.is_empty() && if_not_found == OnEntryNotFound::ThrowException => Err(
                Error::invalid_input(format!("Failed to remove non-existent secret with name '{name}'")),
            ),
            None => Ok(()),
            Some(&TEMPORARY_STORAGE) => {
                state.temporary.remove(&key);
                Ok(())
            }
            Some(_) => match state.local_file.as_mut() {
                Some(local) => local.remove(&key),
                None => Ok(()),
            },
        }
    }

    /// `SecretManager::AllSecrets`: the storages in name order (`local_file`, `memory`).
    pub fn all_secrets(&self) -> Result<Vec<SecretEntry>> {
        let mut state = self.lock();
        Self::initialize(&mut state)?;
        // Each storage lists its secrets by upper-cased name, like a catalog set.
        let by_name = |secrets: &mut Vec<KeyValueSecret>| {
            secrets.sort_by_cached_key(|secret| secret.name.to_uppercase());
        };
        let mut result = Vec::new();
        if let Some(local) = state.local_file.as_mut() {
            let mut secrets = local.all()?;
            by_name(&mut secrets);
            result.extend(secrets.into_iter().map(|secret| SecretEntry {
                secret,
                persistent: true,
                storage: LOCAL_FILE_STORAGE.to_owned(),
            }));
        }
        let mut secrets: Vec<KeyValueSecret> = state.temporary.values().cloned().collect();
        by_name(&mut secrets);
        result.extend(secrets.into_iter().map(|secret| SecretEntry {
            secret,
            persistent: false,
            storage: TEMPORARY_STORAGE.to_owned(),
        }));
        Ok(result)
    }

    /// `SecretManager::LookupSecret`: the secret of `secret_type` whose scope matches
    /// `path` best (`SecretStorage::SelectBestMatch`).
    pub fn lookup_secret(&self, path: &str, secret_type: &str) -> Result<Option<SecretEntry>> {
        let mut best: Option<(i64, SecretEntry)> = None;
        for entry in self.all_secrets()? {
            if !entry.secret.secret_type.eq_ignore_ascii_case(secret_type) {
                continue;
            }
            let Some(score) = entry.secret.match_score(path) else {
                continue;
            };
            let offset = if entry.persistent { 20 } else { 10 };
            let score = 100 * score - offset;
            let better = match &best {
                None => true,
                Some((best_score, best_entry)) => {
                    score > *best_score
                        || (score == *best_score && entry.secret.name < best_entry.secret.name)
                }
            };
            if better {
                best = Some((score, entry));
            }
        }
        Ok(best.map(|(_, entry)| entry))
    }
}

/// `FileSystem::ExpandPath`: a leading `~` is the home directory.
fn expand_home(path: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) => format!("{}{rest}", std::env::var("HOME").unwrap_or_default()),
        None => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(scope: &[&str]) -> KeyValueSecret {
        KeyValueSecret {
            name: "s".to_owned(),
            secret_type: "http".to_owned(),
            provider: "config".to_owned(),
            scope: scope.iter().map(|prefix| (*prefix).to_owned()).collect(),
            map: [
                ("bearer_token".to_owned(), Value::varchar("t")),
                ("verify_ssl".to_owned(), Value::boolean(true)),
            ]
            .into_iter()
            .collect(),
            redact_keys: ["bearer_token".to_owned()].into_iter().collect(),
        }
    }

    #[test]
    fn display_string_redacts_like_duckdb() {
        assert_eq!(
            secret(&["a", "b"]).to_display_string(true),
            "name=s;type=http;provider=config;serializable=true;scope=a,b;bearer_token=redacted;verify_ssl=true"
        );
        assert!(
            secret(&[])
                .to_display_string(false)
                .contains("scope;bearer_token=t;")
        );
    }

    #[test]
    fn longest_scope_prefix_wins() {
        let scoped = secret(&["s3://a", "s3://a/b"]);
        assert_eq!(scoped.match_score("s3://a/b/c"), Some(8));
        assert_eq!(scoped.match_score("s3://x"), None);
        assert_eq!(secret(&[]).match_score("anything"), Some(0));
    }

    #[test]
    fn secrets_round_trip_through_the_binary_format() {
        let original = secret(&["https://x"]);
        let bytes = BinarySerializer::serialize(&original, SerializationOptions::default()).unwrap();
        let read: KeyValueSecret = BinaryDeserializer::deserialize(&bytes).unwrap();
        assert_eq!(read.to_display_string(false), original.to_display_string(false));
    }
}
