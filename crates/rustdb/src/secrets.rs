//! Secrets (DuckDB's `SecretManager`, `KeyValueSecret` and the `memory` / `local_file`
//! secret storages). The core build knows the `http` secret type with its `config` and
//! `env` providers; the types of extensions are reported as such.
//!
//! Secret catalog versions follow the transaction manager; persistent file writes
//! happen immediately, independently of catalog rollback, as in DuckDB.
//! Persistent key/value secrets use DuckDB's `MAP(VARCHAR, ANY)` representation;
//! legacy port files remain readable and are rewritten in that format on replacement.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::extensions::native_path;
use rustdb_ast::{OnCreateConflict, OnEntryNotFound, SecretPersistType};
use rustdb_common::{ByteString, Error, ErrorKind, Result, byte_format};
use rustdb_function::cast::ValueCast;
use rustdb_function::logging::FileSystemLogger;
use rustdb_planner::operator::CreateSecretInput;
use rustdb_serde::binary::{BinaryDeserializer, BinarySerializer};
use rustdb_serde::{DeserializeObject, Deserializer, SerializationOptions, SerializeObject, Serializer};
use rustdb_types::{LogicalType, Value};

mod catalog;
use catalog::SecretCatalog;
use rustdb_storage::transaction::Transaction;

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
    pub name: ByteString,
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
    pub fn to_display_string(&self, redact: bool) -> Result<ByteString> {
        let mut result = byte_format!(
            "name={};type={};provider={};serializable=true;scope=",
            self.name,
            self.secret_type,
            self.provider
        );
        for prefix in &self.scope {
            result.push_bytes(prefix);
            result.push(b',');
        }
        result = ByteString::from(&result.as_bytes()[..result.len().saturating_sub(1)]);
        result.push(b';');
        let entries: Vec<ByteString> = self
            .map
            .iter()
            .map(|(key, value)| {
                let shown = if redact && self.redact_keys.contains(key) {
                    ByteString::from("redacted")
                } else {
                    value.to_varchar_bytes()?
                };
                Ok(byte_format!("{key}={shown}"))
            })
            .collect::<Result<_>>()?;
        result.push_bytes(ByteString::join(&entries, b";"));
        Ok(result)
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
        let entries = self
            .map
            .iter()
            .map(|(key, value)| (Value::varchar(key), value.clone()))
            .collect();
        let map = Value::map(LogicalType::VARCHAR, LogicalType::ANY, entries);
        serializer.write_property(201, "secret_map", &map)?;
        let redact = Value::list(
            LogicalType::VARCHAR,
            self.redact_keys.iter().map(Value::varchar).collect(),
        );
        serializer.write_property(202, "redact_keys", &redact)
    }
}

impl DeserializeObject for KeyValueSecret {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let secret_type: String = deserializer.read_property(100, "type")?;
        let provider: String = deserializer.read_property(101, "provider")?;
        let name: ByteString = deserializer.read_property(102, "name")?;
        let scope: Vec<String> = deserializer.read_property(103, "scope")?;
        let serialization_type: u8 =
            deserializer.read_property_with_explicit_default(104, "serialization_type", 1)?;
        if serialization_type != 1 {
            return Err(Error::not_implemented(
                "Custom secret deserializers are not supported",
            ));
        }
        let entries: Value = deserializer.read_property(201, "secret_map")?;
        let mut map = BTreeMap::new();
        for entry in entries
            .children()
            .ok_or_else(|| Error::serialization("Secret map is not a nested value"))?
        {
            let fields = entry
                .children()
                .filter(|fields| fields.len() == 2)
                .ok_or_else(|| Error::serialization("Secret map entry is not a key/value pair"))?;
            map.insert(fields[0].to_varchar()?.to_ascii_lowercase(), fields[1].clone());
        }
        let redact: Value = deserializer.read_property(202, "redact_keys")?;
        let redact_keys = redact
            .children()
            .ok_or_else(|| Error::serialization("Secret redact keys are not a list"))?
            .iter()
            .map(|value| value.to_varchar().map(|key| key.to_ascii_lowercase()))
            .collect::<Result<BTreeSet<_>>>()?;
        Ok(Self {
            name,
            secret_type,
            provider,
            scope,
            map,
            redact_keys,
        })
    }
}

/// Reader for files written by the port before DuckDB-compatible persistence.
/// New writes always use `KeyValueSecret`'s typed MAP/LIST representation.
struct LegacyKeyValueSecret(KeyValueSecret);

impl DeserializeObject for LegacyKeyValueSecret {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let secret_type: String = deserializer.read_property(100, "type")?;
        let provider: String = deserializer.read_property(101, "provider")?;
        let name: ByteString = deserializer.read_property(102, "name")?;
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
        Ok(Self(KeyValueSecret {
            name,
            secret_type,
            provider,
            scope,
            map,
            redact_keys: redact.into_iter().collect(),
        }))
    }
}

/// A secret with where it is stored (DuckDB's `SecretEntry`).
#[derive(Clone, Debug)]
pub(crate) struct SecretEntry {
    pub secret: KeyValueSecret,
    pub persistent: bool,
    pub storage: String,
}

pub(crate) struct HttpSecretLookup(pub Arc<SecretManager>);

pub(crate) struct S3SecretLookup(pub Arc<SecretManager>);

pub(crate) struct HuggingFaceSecretLookup(pub Arc<SecretManager>);
impl std::fmt::Debug for HuggingFaceSecretLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HuggingFaceSecretLookup").finish_non_exhaustive()
    }
}
impl rustdb_httpfs::huggingface::ResolveSecret for HuggingFaceSecretLookup {
    fn token(&self, path: &str) -> Result<Option<String>> {
        self.0
            .lookup_secret(path, "huggingface")?
            .map(|entry| {
                entry
                    .secret
                    .map
                    .get("token")
                    .ok_or_else(|| Error::internal("HuggingFace secret missing token"))?
                    .to_varchar()
            })
            .transpose()
    }
}

fn create_huggingface_secret(input: &CreateSecretInput) -> Result<KeyValueSecret> {
    let provider = if input.provider.is_empty() {
        "config"
    } else {
        &input.provider
    };
    if !matches!(provider, "config" | "credential_chain") {
        return Err(provider_not_found(
            "huggingface",
            provider,
            input.provider.is_empty(),
        ));
    }
    let mut token = String::new();
    for (name, value) in &input.options {
        if provider != "config" || !name.as_bytes().eq_ignore_ascii_case(b"token") {
            let default_string = if input.provider.is_empty() { "default " } else { "" };
            return Err(Error::binder(byte_format!(
                "Unknown parameter '{name}' for secret type 'huggingface' with {default_string}provider '{provider}'"
            )));
        }
        token = value
            .default_cast_as(&LogicalType::VARCHAR)
            .map_err(|error| {
                Error::binder(format!(
                    "Failed to cast option 'token' to type 'VARCHAR': '{}'",
                    error.raw_message()
                ))
            })?
            .to_varchar()?;
    }
    if provider == "credential_chain" {
        token = if let Ok(token) = std::env::var("HF_TOKEN") {
            token
        } else if let Ok(path) = std::env::var("HF_TOKEN_PATH") {
            read_huggingface_token(&path, " fetched from HF_TOKEN_PATH env variable", true)?
        } else if let Ok(home) = std::env::var("HF_HOME") {
            read_huggingface_token(
                &if home.is_empty() {
                    "token".into()
                } else {
                    format!("{home}/token")
                },
                " constructed using the HF_HOME variable: '$HF_HOME/token'",
                true,
            )?
        } else {
            read_huggingface_token("~/.cache/huggingface/token", "", false)?
        };
    }
    Ok(KeyValueSecret {
        name: input.name.clone(),
        secret_type: "huggingface".into(),
        provider: provider.into(),
        scope: if input.scope.is_empty() {
            vec!["hf://".into()]
        } else {
            input.scope.clone()
        },
        map: [("token".into(), Value::varchar(token))].into(),
        redact_keys: ["token".into()].into(),
    })
}

fn read_huggingface_token(path: &str, source: &str, fail: bool) -> Result<String> {
    use std::io::BufRead as _;
    let expanded = rustdb_function::file_system::expand_path(&expand_home(path));
    let result = (|| {
        let file = std::fs::File::open(&expanded).map_err(|e| {
            Error::new(
                ErrorKind::Io,
                format!(
                    "Cannot open file \"{expanded}\": {}",
                    rustdb_common::os_error_text(&e)
                ),
            )
        })?;
        let mut bytes = Vec::new();
        std::io::BufReader::new(file)
            .read_until(b'\n', &mut bytes)
            .map_err(|e| {
                Error::new(
                    ErrorKind::Io,
                    format!(
                        "Could not read from file \"{expanded}\": {}",
                        rustdb_common::os_error_text(&e)
                    ),
                )
            })?;
        bytes.retain(|b| !matches!(b, b'\n' | b'\r'));
        String::from_utf8(bytes).map_err(|e| Error::new(ErrorKind::Io, e.to_string()))
    })();
    match result {
        Ok(token) => Ok(token),
        Err(error) if fail => Err(Error::new(
            ErrorKind::Io,
            format!(
                "Failed to read token path '{path}'{source}. (error: {})",
                error.raw_message()
            ),
        )),
        Err(_) => Ok(String::new()),
    }
}

impl std::fmt::Debug for S3SecretLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3SecretLookup").finish_non_exhaustive()
    }
}

impl rustdb_httpfs::s3::ResolveS3Secret for S3SecretLookup {
    fn resolve(&self, path: &str) -> Result<Option<BTreeMap<String, Value>>> {
        for kind in ["s3", "r2", "gcs", "aws"] {
            if let Some(entry) = self.0.lookup_secret(path, kind)? {
                return Ok(Some(entry.secret.map));
            }
        }
        Ok(None)
    }
    fn refresh(&self, path: &str) -> Result<bool> {
        let Some(context) = rustdb_function::file_system::opener()
            .and_then(|client| client.transaction_context())
            .and_then(|context| context.downcast::<rustdb_planner::ClientContext>().ok())
        else {
            return Ok(false);
        };
        let _refresh = self.0.refresh_lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut refreshed = false;
        for kind in ["s3", "r2", "gcs", "aws"] {
            let Some(entry) = self.0.lookup_secret(path, kind)? else {
                continue;
            };
            let Some(info) = entry.secret.map.get("refresh_info") else {
                continue;
            };
            let fields = info
                .logical_type()
                .struct_children()
                .ok_or_else(|| Error::invalid_input("Invalid input passed to refresh_info"))?;
            let values = info
                .children()
                .ok_or_else(|| Error::invalid_input("Invalid input passed to refresh_info"))?;
            let input = CreateSecretInput {
                secret_type: entry.secret.secret_type.clone(),
                provider: entry.secret.provider.clone(),
                name: entry.secret.name.clone(),
                storage: entry.storage.as_str().into(),
                scope: entry.secret.scope.clone(),
                options: fields
                    .iter()
                    .zip(values)
                    .map(|((name, _), value)| (name.clone(), value.clone()))
                    .collect(),
                on_conflict: OnCreateConflict::ReplaceOnConflict,
                persist_type: if entry.persistent {
                    SecretPersistType::Persistent
                } else {
                    SecretPersistType::Temporary
                },
            };
            self.0.create_refreshed_secret(&input, &context.transaction).map_err(|error| {
                Error::new_bytes(error.kind(), byte_format!(
                    "Exception thrown while trying to refresh secret {}. To fix this, please recreate or remove the secret and try again. Error: '{}'",
                    entry.secret.name.as_bytes(), error.raw_message_bytes()
                ))
            })?;
            refreshed = true;
        }
        Ok(refreshed)
    }
    fn batch_identity(&self, path: &str) -> Result<Vec<Vec<u8>>> {
        let mut result = Vec::new();
        for kind in ["s3", "r2", "gcs", "aws"] {
            result.push(kind.as_bytes().to_vec());
            if let Some(entry) = self.0.lookup_secret(path, kind)? {
                result.push(b"1".to_vec());
                result.push(entry.secret.name.as_bytes().to_vec());
                result.push(entry.secret.provider.as_bytes().to_vec());
                result.push(entry.storage.as_bytes().to_vec());
                result.push(entry.persistent.to_string().into_bytes());
                result.push(
                    entry
                        .secret
                        .match_score(path)
                        .unwrap_or_default()
                        .to_string()
                        .into_bytes(),
                );
                result.push(entry.secret.scope.len().to_string().into_bytes());
                result.extend(entry.secret.scope.iter().map(|s| s.as_bytes().to_vec()));
            } else {
                result.push(b"0".to_vec());
            }
        }
        Ok(result)
    }
}

fn create_s3_secret(input: &CreateSecretInput) -> Result<KeyValueSecret> {
    let provider = if input.provider.is_empty() {
        "config"
    } else {
        input.provider.as_str()
    };
    if provider != "config" {
        return Err(provider_not_found(
            &input.secret_type,
            provider,
            input.provider.is_empty(),
        ));
    }
    let default_string = if input.provider.is_empty() { "default " } else { "" };
    let mut map = BTreeMap::new();
    // r2 defaults precede explicit endpoint/url_style overrides.
    if input.secret_type == "r2" {
        for (name, value) in &input.options {
            if name.as_bytes().eq_ignore_ascii_case(b"account_id") {
                map.insert(
                    "endpoint".into(),
                    Value::varchar(format!("{}.r2.cloudflarestorage.com", value.to_varchar()?)),
                );
                map.insert("url_style".into(), Value::varchar("path"));
            }
        }
    }
    let mut refresh_fields = Vec::new();
    for (name, value) in &input.options {
        let native = name.to_ascii_lowercase();
        let lower = std::str::from_utf8(native.as_bytes()).unwrap_or("").to_owned();
        let ty = match lower.as_str() {
            "key_id"
            | "secret"
            | "region"
            | "session_token"
            | "endpoint"
            | "url_style"
            | "kms_key_id"
            | "refresh"
            | "http_proxy"
            | "http_proxy_password"
            | "http_proxy_username" => LogicalType::VARCHAR,
            "use_ssl" | "verify_ssl" | "url_compatibility_mode" | "requester_pays" => LogicalType::BOOLEAN,
            "bearer_token" if input.secret_type == "gcs" => LogicalType::VARCHAR,
            "account_id" if input.secret_type == "r2" => LogicalType::VARCHAR,
            "extra_http_headers" | "refresh_info" => {
                LogicalType::map(LogicalType::VARCHAR, LogicalType::VARCHAR)
            }
            _ => {
                return Err(Error::binder(byte_format!(
                    "Unknown parameter '{name}' for secret type '{}' with {default_string}provider '{provider}'",
                    input.secret_type
                )));
            }
        };
        let cast = value.default_cast_as(&ty).map_err(|error| {
            Error::binder(format!(
                "Failed to cast option '{lower}' to type '{ty}': '{}'",
                error.raw_message()
            ))
        })?;
        refresh_fields.push((native, cast.clone()));
        if lower == "refresh_info" {
            if map.contains_key("refresh") || map.contains_key("refresh_info") {
                return Err(Error::invalid_input(
                    "Can not set `refresh` and `refresh_info` at the same time",
                ));
            }
            let mut fields = Vec::new();
            for pair in cast.children().unwrap_or_default() {
                let values = pair
                    .children()
                    .ok_or_else(|| Error::invalid_input("Invalid input passed to refresh_info"))?;
                let [key, value] = values else {
                    return Err(Error::invalid_input("Invalid input passed to refresh_info"));
                };
                fields.push((key.to_varchar_bytes()?, value.clone()));
            }
            map.insert(lower, Value::struct_value(fields));
        } else if lower == "refresh" {
            if map.contains_key("refresh_info") {
                return Err(Error::invalid_input(
                    "Can not set `refresh` and `refresh_info` at the same time",
                ));
            }
            map.insert(lower, Value::varchar("auto"));
        } else if lower != "account_id" {
            map.insert(lower, cast);
        }
    }
    if map.contains_key("refresh") {
        map.insert("refresh_info".into(), Value::struct_value(refresh_fields));
    }
    let mut scope = input.scope.clone();
    if scope.is_empty() {
        scope = match input.secret_type.as_str() {
            "s3" => vec!["s3://".into(), "s3n://".into(), "s3a://".into()],
            "r2" => vec!["r2://".into()],
            "gcs" => vec!["gcs://".into(), "gs://".into()],
            _ => vec![String::new()],
        };
    }
    let mut redact_keys: BTreeSet<String> = ["secret", "session_token"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    if map.contains_key("refresh_info") {
        // Nested refresh options can contain replacement credentials too.
        redact_keys.insert("refresh_info".into());
    }
    if input.secret_type == "gcs" && map.contains_key("bearer_token") {
        redact_keys.insert("bearer_token".into());
    }
    Ok(KeyValueSecret {
        name: input.name.clone(),
        secret_type: input.secret_type.clone(),
        provider: provider.into(),
        scope,
        map,
        redact_keys,
    })
}

impl std::fmt::Debug for HttpSecretLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSecretLookup").finish_non_exhaustive()
    }
}

impl rustdb_httpfs::ResolveHttpSecret for HttpSecretLookup {
    fn resolve(&self, url: &str) -> Result<Option<rustdb_httpfs::HttpSecret>> {
        let Some(entry) = self.0.lookup_secret(url, "http")? else {
            return Ok(None);
        };
        let text = |key| {
            entry
                .secret
                .map
                .get(key)
                .filter(|v| !v.is_null())
                .map(Value::to_varchar)
                .transpose()
        };
        let mut result = rustdb_httpfs::HttpSecret {
            verify_ssl: entry
                .secret
                .map
                .get("verify_ssl")
                .filter(|v| !v.is_null())
                .and_then(Value::as_bool),
            proxy: text("http_proxy")?,
            proxy_user: text("http_proxy_username")?,
            proxy_password: text("http_proxy_password")?,
            ..Default::default()
        };
        if let Some(bearer) = text("bearer_token")?.filter(|v| !v.is_empty()) {
            result.bearer_token = Some(bearer);
        }
        if let Some(headers) = entry
            .secret
            .map
            .get("extra_http_headers")
            .and_then(Value::children)
        {
            for pair in headers {
                let values = pair
                    .children()
                    .ok_or_else(|| Error::internal("HTTP header must be a key/value pair"))?;
                let [key, value] = values else {
                    return Err(Error::internal("HTTP header must be a key/value pair"));
                };
                let name = key.to_varchar()?;
                result.headers.retain(|(key, _)| !key.eq_ignore_ascii_case(&name));
                result.headers.push((name, value.to_varchar()?));
            }
        }
        Ok(Some(result))
    }
}

/// The `local_file` storage: one file per secret, read when first needed
/// (`LocalFileSecretStorage` with its `DefaultSecretGenerator`).
#[derive(Debug)]
struct LocalFileStorage {
    path: String,
    /// Secrets by lower-cased name; `None` until the file was read.
    secrets: BTreeMap<ByteString, Option<KeyValueSecret>>,
    catalog: Arc<SecretCatalog>,
}

impl LocalFileStorage {
    fn new(path: &str) -> Self {
        let mut secrets = BTreeMap::new();
        if let Ok(files) = std::fs::read_dir(path) {
            for file in files.flatten() {
                let file_name = file.file_name();
                let file_name = ByteString::from(file_name.as_encoded_bytes());
                if let Some(name) = file_name.strip_suffix(SECRET_FILE_EXTENSION.as_bytes()) {
                    secrets.insert(ByteString::from(name.to_ascii_lowercase()), None);
                }
            }
        }
        Self {
            path: path.to_owned(),
            secrets,
            catalog: Arc::default(),
        }
    }

    fn file_path(&self, name: &[u8]) -> ByteString {
        byte_format!("{}/{name}{SECRET_FILE_EXTENSION}", self.path)
    }

    /// The secret `name`, read from its file if needed.
    fn get(&mut self, name: &[u8]) -> Result<Option<KeyValueSecret>> {
        let key = ByteString::from(name.to_ascii_lowercase());
        let Some(slot) = self.secrets.get(&key) else {
            return Ok(None);
        };
        if let Some(secret) = slot {
            return Ok(Some(secret.clone()));
        }
        let path = self.file_path(&key);
        // `BufferedFileReader`: one read of the whole file.
        let logger = FileSystemLogger::open(&path);
        let bytes = std::fs::read(native_path(&path)?);
        if let Some(logger) = &logger {
            if let Ok(bytes) = &bytes {
                logger.read(bytes.len() as u64, 0);
            }
            logger.close();
        }
        let bytes = bytes.map_err(|error| {
            Error::new_bytes(
                ErrorKind::Io,
                byte_format!(
                    "Failed to open the persistent secret file: '{path}'. Some other process may have removed it, \
                     please restart and try again. (error message: '{error}')"
                ),
            )
        })?;
        if bytes.is_empty() {
            return Err(Error::new_bytes(
                ErrorKind::Serialization,
                byte_format!(
                    "Failed to deserialize secret '{key}' from '{path}': file appears empty! Please remove the file, \
                     restart and try again"
                ),
            ));
        }
        let secret: KeyValueSecret = BinaryDeserializer::deserialize(&bytes)
            .or_else(|error| {
                BinaryDeserializer::deserialize::<LegacyKeyValueSecret>(&bytes)
                    .map(|secret| secret.0)
                    .map_err(|_| error)
            })
            .map_err(|error| {
            Error::new_bytes(
                ErrorKind::Serialization,
                byte_format!(
                    "Failed to deserialize the persistent secret file: '{path}'. The file maybe be corrupt, please \
                     remove the file, restart and try again. (error message: '{}')",
                    error.raw_message_bytes()
                ),
            )
        })?;
        self.secrets.insert(key, Some(secret.clone()));
        Ok(Some(secret))
    }

    fn catalog_get(
        &mut self,
        name: &[u8],
        transaction: Option<&Transaction>,
    ) -> Result<Option<KeyValueSecret>> {
        if !self.catalog.has_chain(name)
            && let Some(secret) = self.get(name)?
        {
            self.catalog.seed(secret);
        }
        Ok(self.catalog.get(name, transaction))
    }

    fn all(&mut self) -> Result<Vec<KeyValueSecret>> {
        let names: Vec<ByteString> = self.secrets.keys().cloned().collect();
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
            Error::new_bytes(
                ErrorKind::Io,
                byte_format!(
                    "Failed to initialize persistent storage directory. (original error: '{error}')"
                ),
            )
        })?;
        let key = secret.name.to_ascii_lowercase();
        let path = self.file_path(&key);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let temp_path = byte_format!("{path}.tmp-{}-{nonce}", std::process::id());
        let bytes = BinarySerializer::serialize(secret, SerializationOptions::default())?;
        let io_error = |error: std::io::Error| {
            Error::new_bytes(
                ErrorKind::Io,
                byte_format!("Could not write file \"{path}\": {error}"),
            )
        };
        let _ = std::fs::remove_file(native_path(&path)?);
        // `BufferedFileWriter`: one write when flushed.
        let logger = FileSystemLogger::open(&temp_path);
        let written = write_private_file(&native_path(&temp_path)?, &bytes);
        if let Some(logger) = &logger {
            if written.is_ok() {
                logger.write(bytes.len() as u64, 0);
            }
            logger.close();
        }
        written.map_err(io_error)?;
        std::fs::rename(native_path(&temp_path)?, native_path(&path)?).map_err(io_error)?;
        Ok(())
    }

    fn remove(&mut self, name: &[u8]) -> Result<()> {
        let key = ByteString::from(name.to_ascii_lowercase());
        self.secrets.remove(&key);
        let file = self.file_path(&key);
        std::fs::remove_file(native_path(&file)?).map_err(|error| {
            Error::new_bytes(
                ErrorKind::Io,
                byte_format!(
                    "Failed to remove secret file '{file}', the file may have been removed by another duckdb \
                     instance. (original error: '{error}')"
                ),
            )
        })
    }
}

/// Writes `bytes` to a new file only the owner can read (`FILE_FLAGS_PRIVATE`).
fn write_private_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
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
    temporary: Arc<SecretCatalog>,
    local_file: Option<LocalFileStorage>,
    httpfs_registered: bool,
}

/// The secrets of a database instance (DuckDB's `SecretManager`).
#[derive(Debug)]
pub(crate) struct SecretManager {
    state: Mutex<ManagerState>,
    refresh_lock: Mutex<()>,
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
            refresh_lock: Mutex::new(()),
            state: Mutex::new(ManagerState {
                config: SecretConfig {
                    secret_path: default_secret_path.clone(),
                    default_secret_path,
                    allow_persistent_secrets: true,
                    default_storage: LOCAL_FILE_STORAGE.to_owned(),
                },
                initialized: false,
                temporary: Arc::default(),
                local_file: None,
                httpfs_registered: false,
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
/// A secret type the secret manager knows (DuckDB's `SecretType`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct SecretType {
    pub name: &'static str,
    pub default_provider: &'static str,
    /// The extension that registers the type; empty for built-in ones.
    pub extension: &'static str,
}

/// The secret types without extensions (`SecretManager::Initialize`): `http`.
pub(crate) const SECRET_TYPES: [SecretType; 1] = [SecretType {
    name: "http",
    default_provider: "config",
    extension: "",
}];

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
    fn create_refreshed_secret(&self, input: &CreateSecretInput, transaction: &Transaction) -> Result<()> {
        // Refresh calls the provider directly, without SQL's named-parameter
        // casts. A VARCHAR from REFRESH_INFO is not a BOOLEAN provider value.
        for (name, value) in &input.options {
            let lower = name.to_ascii_lowercase();
            if [
                b"use_ssl".as_slice(),
                b"verify_ssl",
                b"url_compatibility_mode",
                b"requester_pays",
            ]
            .contains(&lower.as_bytes())
                && value.logical_type() != &LogicalType::BOOLEAN
            {
                return Err(Error::new_bytes(
                    ErrorKind::InvalidInput,
                    byte_format!(
                        "Invalid type past to secret option: '{lower}', found '{}', expected: 'BOOLEAN'",
                        value.logical_type().to_string()
                    ),
                ));
            }
        }
        self.create_secret(input, transaction)
    }
    pub fn register_s3_types(&self) {
        self.lock().httpfs_registered = true;
    }

    pub fn secret_types(&self) -> Vec<SecretType> {
        let mut types = SECRET_TYPES.to_vec();
        if self.lock().httpfs_registered {
            types.extend(["s3", "aws", "r2", "gcs", "huggingface"].map(|name| SecretType {
                name,
                default_provider: "config",
                extension: "httpfs",
            }));
        }
        types
    }

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
    pub fn create_secret(&self, input: &CreateSecretInput, transaction: &Transaction) -> Result<()> {
        let mut state = self.lock();
        Self::initialize(&mut state)?;
        // Without a provider the type must exist (its default provider is used);
        // otherwise the type's provider must.
        let was_default = input.provider.is_empty();
        if state.httpfs_registered && input.secret_type == "huggingface" {
            return Self::register(&mut state, create_huggingface_secret(input)?, input, transaction);
        }
        if state.httpfs_registered && matches!(input.secret_type.as_str(), "s3" | "aws" | "r2" | "gcs") {
            let secret = create_s3_secret(input)?;
            return Self::register(&mut state, secret, input, transaction);
        }
        let known_type = SECRET_TYPES
            .iter()
            .any(|secret_type| secret_type.name == input.secret_type);
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
            let native = name.to_ascii_lowercase();
            let Some(lower) = [
                "verify_ssl",
                "http_proxy",
                "http_proxy_password",
                "http_proxy_username",
                "bearer_token",
                "extra_http_headers",
            ]
            .into_iter()
            .find(|option| option.as_bytes() == native.as_bytes()) else {
                return Err(Error::binder(byte_format!(
                    "Unknown parameter '{name}' for secret type '{}' with {default_string}provider '{provider}'",
                    input.secret_type
                )));
            };
            let ty = http_parameter_type(lower)
                .ok_or_else(|| Error::internal("known HTTP parameter missing type"))?;
            let cast = value.default_cast_as(&ty).map_err(|error| {
                Error::binder(format!(
                    "Failed to cast option '{lower}' to type '{ty}': '{}'",
                    error.raw_message()
                ))
            })?;
            options.insert(lower.to_owned(), cast);
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
        Self::register(&mut state, secret, input, transaction)
    }

    /// `SecretManager::RegisterSecretInternal` and `CatalogSetSecretStorage::StoreSecret`.
    fn register(
        state: &mut ManagerState,
        secret: KeyValueSecret,
        input: &CreateSecretInput,
        transaction: &Transaction,
    ) -> Result<()> {
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
                ByteString::from(&state.config.default_storage)
            } else {
                ByteString::from(TEMPORARY_STORAGE)
            }
        } else {
            input.storage.clone()
        };
        let persistent_storage = match storage.as_bytes() {
            b"memory" => false,
            b"local_file" if state.local_file.is_some() => true,
            _ => {
                if !state.config.allow_persistent_secrets
                    && (persist_type == SecretPersistType::Persistent || storage == LOCAL_FILE_STORAGE)
                {
                    return Err(Error::invalid_input(
                        "Persistent secrets are disabled. Restart DuckDB and enable persistent secrets through 'SET \
                         allow_persistent_secrets=true'",
                    ));
                }
                return Err(Error::invalid_input(byte_format!(
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
        let key = secret.name.to_ascii_lowercase();
        let catalog = if persistent_storage {
            let local = state
                .local_file
                .as_mut()
                .ok_or_else(|| Error::internal("Missing validated persistent secret storage"))?;
            local.catalog_get(&key, Some(transaction))?;
            Arc::clone(&local.catalog)
        } else {
            Arc::clone(&state.temporary)
        };
        let exists = catalog.get(&key, Some(transaction)).is_some();
        if exists {
            match input.on_conflict {
                OnCreateConflict::IgnoreOnConflict => return Ok(()),
                OnCreateConflict::ReplaceOnConflict => {
                    catalog.change(key.clone(), None, transaction, "alter")?;
                }
                _ => {
                    let (kind, storage_string) = if persistent_storage {
                        ("Persistent", byte_format!(" in secret storage '{storage}'"))
                    } else {
                        ("Temporary", ByteString::new())
                    };
                    return Err(Error::invalid_input(byte_format!(
                        "{kind} secret with name '{}' already exists{storage_string}!",
                        secret.name
                    )));
                }
            }
        }
        if persistent_storage && let Some(local) = state.local_file.as_mut() {
            local.write(&secret)?;
        }
        catalog.change(key, Some(secret), transaction, "create")?;
        Ok(())
    }

    /// `SecretManager::DropSecretByName`.
    pub fn drop_secret(
        &self,
        name: &[u8],
        if_not_found: OnEntryNotFound,
        persist_type: SecretPersistType,
        storage: impl AsRef<[u8]>,
        transaction: &Transaction,
    ) -> Result<()> {
        let storage = storage.as_ref();
        let mut state = self.lock();
        Self::initialize(&mut state)?;
        let key = ByteString::from(name.to_ascii_lowercase());
        let in_local = match state.local_file.as_mut() {
            Some(local) => local.catalog_get(&key, Some(transaction))?.is_some(),
            None => false,
        };
        let in_memory = state.temporary.get(&key, Some(transaction)).is_some();
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
                b"local_file" if state.local_file.is_some() => {
                    if in_local {
                        matches.push(LOCAL_FILE_STORAGE);
                    } else if if_not_found == OnEntryNotFound::ThrowException {
                        return Err(Error::invalid_input(byte_format!(
                            "Failed to remove non-existent persistent secret '{name}' in secret storage \
                             '{LOCAL_FILE_STORAGE}'"
                        )));
                    }
                }
                b"memory" => {
                    if in_memory {
                        matches.push(TEMPORARY_STORAGE);
                    } else if if_not_found == OnEntryNotFound::ThrowException {
                        return Err(Error::invalid_input(byte_format!(
                            "Failed to remove non-existent temporary secret '{name}'"
                        )));
                    }
                }
                _ => {
                    return Err(Error::invalid_input(byte_format!(
                        "Unknown storage type found for drop secret: '{storage}'"
                    )));
                }
            }
        }
        if matches.len() > 1 {
            return Err(Error::invalid_input(byte_format!(
                "Ambiguity found for secret name '{name}', secret occurs in multiple storages: [{}] Please specify \
                 which secret to drop using: 'DROP <PERSISTENT|TEMPORARY> SECRET [FROM <storage>]'.",
                matches.join(",")
            )));
        }
        match matches.first() {
            None if storage.is_empty() && if_not_found == OnEntryNotFound::ThrowException => {
                Err(Error::invalid_input(byte_format!(
                    "Failed to remove non-existent secret with name '{name}'"
                )))
            }
            None => Ok(()),
            Some(&TEMPORARY_STORAGE) => state.temporary.change(key, None, transaction, "alter"),
            Some(_) => match state.local_file.as_mut() {
                Some(local) => {
                    local.catalog.change(key.clone(), None, transaction, "alter")?;
                    local.remove(&key)
                }
                None => Ok(()),
            },
        }
    }

    /// `SecretManager::AllSecrets`: the storages in name order (`local_file`, `memory`).
    pub fn all_secrets(&self) -> Result<Vec<SecretEntry>> {
        let transaction = rustdb_function::file_system::opener()
            .and_then(|client| client.transaction_context())
            .and_then(|context| context.downcast::<rustdb_planner::ClientContext>().ok())
            .map(|context| Arc::clone(&context.transaction));
        self.all_secrets_in(transaction.as_deref())
    }

    pub fn all_secrets_in(&self, transaction: Option<&Transaction>) -> Result<Vec<SecretEntry>> {
        let mut state = self.lock();
        Self::initialize(&mut state)?;
        // Each storage lists its secrets by upper-cased name, like a catalog set.
        let by_name = |secrets: &mut Vec<KeyValueSecret>| {
            secrets.sort_by_cached_key(|secret| secret.name.as_bytes().to_ascii_uppercase());
        };
        let mut result = Vec::new();
        if let Some(local) = state.local_file.as_mut() {
            for secret in local.all()? {
                if !local.catalog.has_chain(&secret.name.to_ascii_lowercase()) {
                    local.catalog.seed(secret);
                }
            }
            let mut secrets = local.catalog.all(transaction);
            by_name(&mut secrets);
            result.extend(secrets.into_iter().map(|secret| SecretEntry {
                secret,
                persistent: true,
                storage: LOCAL_FILE_STORAGE.to_owned(),
            }));
        }
        let mut secrets = state.temporary.all(transaction);
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
            name: "s".into(),
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
            secret(&["a", "b"]).to_display_string(true).unwrap(),
            "name=s;type=http;provider=config;serializable=true;scope=a,b;bearer_token=redacted;verify_ssl=true"
        );
        assert!(
            secret(&[])
                .to_display_string(false)
                .unwrap()
                .windows(b"scope;bearer_token=t;".len())
                .any(|part| part == b"scope;bearer_token=t;")
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
