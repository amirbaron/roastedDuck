//! Safe boundary between the engine and the embedding's C extension ABI adapter.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use roastedduck_common::{ByteString, Error, Result};

use crate::Database;

/// A successfully initialized external extension, distinct from a linked builtin.
#[derive(Clone, Debug)]
pub struct ExternalExtensionRecord {
    pub name: ByteString,
    pub version: ByteString,
}

/// Per-database initialization locks and loaded metadata. No query data is cached.
#[derive(Debug, Default)]
pub struct ExternalExtensionState {
    loaded: Mutex<BTreeMap<ByteString, ExternalExtensionRecord>>,
    locks: Mutex<BTreeMap<ByteString, Arc<Mutex<()>>>>,
}

impl ExternalExtensionState {
    pub fn records(&self) -> Vec<ExternalExtensionRecord> {
        self.loaded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }
    pub fn contains(&self, name: &[u8]) -> bool {
        self.loaded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(name)
    }
    pub fn initialization_lock(&self, name: &[u8]) -> Arc<Mutex<()>> {
        Arc::clone(
            self.locks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(name.into())
                .or_default(),
        )
    }
    pub fn finish_load(&self, record: ExternalExtensionRecord) {
        self.loaded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(record.name.clone(), record);
    }
}

/// The adapter owns native ABI details; the engine does not depend on its C API crate.
pub type ExternalExtensionLoader = fn(&Database, &[u8], &[u8], bool) -> Result<()>;
pub type ExternalExtensionInstallValidator = fn(&Database, &[u8], &[u8]) -> Result<ByteString>;
static LOADER: OnceLock<ExternalExtensionLoader> = OnceLock::new();
static INSTALL_VALIDATOR: OnceLock<ExternalExtensionInstallValidator> = OnceLock::new();
pub const EXTERNAL_EXTENSIONS_KEY: &str = "roastedduck.external_extensions";

/// Called once by an embedding that supplies the public C extension API.
pub fn register_external_extension_loader(loader: ExternalExtensionLoader) {
    LOADER.get_or_init(|| loader);
}

pub fn register_extension_install_validator(validator: ExternalExtensionInstallValidator) {
    INSTALL_VALIDATOR.get_or_init(|| validator);
}

pub(crate) fn validate_install(database: &Database, filename: &[u8], name: &[u8]) -> Result<ByteString> {
    let validator = INSTALL_VALIDATOR.get().ok_or_else(|| {
        Error::not_implemented(roastedduck_common::byte_format!(
            "installing the extension file \"{filename}\""
        ))
    })?;
    validator(database, filename, name)
}

pub(crate) fn load(database: &Database, filename: &[u8], requested: &[u8], direct: bool) -> Result<()> {
    let Some(loader) = LOADER.get() else {
        return Err(Error::not_implemented(roastedduck_common::byte_format!(
            "loading the extension file \"{filename}\""
        )));
    };
    let result = loader(database, filename, requested, direct);
    if let Err(error) = &result
        && error.kind() == roastedduck_common::ErrorKind::Fatal
    {
        database.invalidate_external_extension(error.raw_message_bytes());
    }
    result
}

/// Native extension platform identifier, shared with metadata and installation paths.
pub fn platform() -> &'static str {
    crate::extensions::platform()
}

/// Check an installed file's recorded version before initializing its native code.
pub fn validate_installed_metadata(filename: &[u8], name: &[u8], version: &[u8]) -> Result<()> {
    crate::extensions::validate_installed_metadata(filename, name, version)
}
