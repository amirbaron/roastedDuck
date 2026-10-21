//! Attached databases (DuckDB's `AttachedDatabase` and `DatabaseManager`): the database
//! opened first and every `ATTACH`ed one, each with its own catalog, storage and WAL.

use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use rustdb_ast::{AttachInfo, OnCreateConflict};
use rustdb_catalog::Catalog;
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_storage::storage_manager::{InMemoryStorageManager, StorageManager};
use rustdb_types::{LogicalTypeId, Value};

use crate::AccessMode;
use crate::storage::{database_name, open_storage};

/// Whether a database instance or an attached database can still be used (DuckDB's
/// `ValidChecker`): invalidated, with the reason, after a failure it cannot recover from.
#[derive(Debug, Default)]
pub(crate) struct ValidChecker {
    message: Mutex<Option<String>>,
}

impl ValidChecker {
    /// `Invalidate`.
    pub fn invalidate(&self, message: String) {
        *self.message.lock().unwrap_or_else(PoisonError::into_inner) = Some(message);
    }

    /// The reason the object was invalidated, if it was (`IsInvalidated` and
    /// `InvalidatedMessage`).
    pub fn invalidated(&self) -> Option<String> {
        self.message
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The storage of an attached database (DuckDB's `StorageManager`).
#[derive(Debug)]
pub(crate) enum DatabaseStorage {
    /// A database file.
    File(Arc<StorageManager>),
    /// An in-memory database (`StorageManager::InMemory`).
    InMemory(Arc<InMemoryStorageManager>),
}

impl DatabaseStorage {
    /// The database file, unless the database is in memory.
    pub fn file(&self) -> Option<&Arc<StorageManager>> {
        match self {
            Self::File(storage) => Some(storage),
            Self::InMemory(_) => None,
        }
    }

    /// Sets how checkpoints compress segments (`force_compression`).
    pub fn set_compression_config(&self, config: rustdb_storage::compression::CompressionConfig) {
        match self {
            Self::File(storage) => storage.set_compression_config(config),
            Self::InMemory(storage) => storage.set_compression_config(config),
        }
    }

    /// Sets how large appends are written to the database file before they commit; an
    /// in-memory database keeps them in memory.
    pub fn set_optimistic_write_config(&self, config: rustdb_storage::OptimisticWriteConfig) {
        if let Self::File(storage) = self {
            storage.io().set_optimistic_write_config(config);
        }
    }

    /// Sets how checkpoints shred VARIANT columns.
    pub fn set_variant_shredding_config(&self, config: rustdb_storage::table::VariantShreddingConfig) {
        match self {
            Self::File(storage) => storage.io().set_variant_shredding_config(config),
            Self::InMemory(storage) => storage.io().set_variant_shredding_config(config),
        }
    }

    /// Sets the settings checkpoints follow.
    pub fn set_checkpoint_config(&self, config: rustdb_storage::storage_manager::CheckpointConfig) {
        match self {
            Self::File(storage) => storage.set_checkpoint_config(config),
            Self::InMemory(storage) => storage.set_checkpoint_config(config),
        }
    }
}

/// How a database is closed (DuckDB's `DatabaseCloseAction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseAction {
    /// Shutting down: a failing checkpoint is ignored.
    TryCheckpoint,
    /// `DETACH`: a failing checkpoint is an error.
    Checkpoint,
}

/// One attached database.
#[derive(Debug)]
pub(crate) struct AttachedDatabase {
    pub name: String,
    /// The file, or `""` for an in-memory database.
    pub path: String,
    pub catalog: Arc<Catalog>,
    pub storage: DatabaseStorage,
    pub read_only: bool,
    /// Whether this is the database opened at start-up (`IsInitialDatabase`).
    pub is_initial: std::sync::atomic::AtomicBool,
    /// This database's validity (a failed checkpoint invalidates it).
    pub validity: ValidChecker,
    /// The instance's validity (a failed checkpoint of the initial database invalidates
    /// the instance).
    pub instance_validity: Arc<ValidChecker>,
    /// Set when closing starts (`is_closed`), so the database is closed once.
    closing: std::sync::atomic::AtomicBool,
    /// Set once the database is closed: its file is free for another attach.
    closed: std::sync::atomic::AtomicBool,
}

impl AttachedDatabase {
    /// Whether the database was closed.
    pub fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Whether this is the database opened at start-up.
    pub fn is_initial(&self) -> bool {
        self.is_initial.load(std::sync::atomic::Ordering::Acquire)
    }

    /// The error of using the database after it was invalidated (the `IOException` of
    /// `MetaTransaction::GetTransaction`).
    pub fn check_valid(&self) -> Result<()> {
        match self.validity.invalidated() {
            Some(message) => Err(Error::new(ErrorKind::Io, message)),
            None => Ok(()),
        }
    }

    /// `AttachedDatabase::Invalidate` after a failed checkpoint.
    fn invalidate(&self, reason: &str) {
        let recovery = if matches!(self.storage, DatabaseStorage::InMemory(_)) {
            "It is an in-memory database, so its data cannot be recovered."
        } else {
            "Detach and reattach it before using it again."
        };
        self.validity.invalidate(format!(
            "Database \"{}\" has been invalidated because checkpointing failed. {recovery} Original error: {reason}",
            self.name
        ));
    }

    /// The error of a checkpoint that failed while writing, after invalidating the
    /// instance (initial database) or this database (the `catch` of
    /// `SingleFileStorageManager::CreateCheckpoint`).
    pub fn checkpoint_failed(&self, error: &Error) -> Error {
        let reason = error.raw_message();
        if self.is_initial() {
            self.instance_validity.invalidate(reason.to_owned());
            return Error::new(
                ErrorKind::Fatal,
                format!("Failed to create checkpoint because of error: {reason}"),
            );
        }
        self.invalidate(reason);
        Error::new(
            ErrorKind::Io,
            format!(
                "Checkpoint failed for database \"{}\". The database has been invalidated. Original error: {reason}",
                self.name
            ),
        )
    }

    /// The error message of using the database once a failed checkpoint invalidated it or
    /// the instance (`MetaTransaction::GetTransaction`, `ValidChecker`).
    fn invalidated_message(&self) -> String {
        match self.validity.invalidated() {
            Some(message) => message,
            None => crate::database::invalidated_database_error(
                &self.instance_validity.invalidated().unwrap_or_default(),
            )
            .raw_message()
            .to_owned(),
        }
    }

    /// Writes a checkpoint of the database if it is still valid
    /// (`SingleFileStorageManager::CreateCheckpoint`).
    pub fn create_checkpoint(
        &self,
        mode: crate::storage::CheckpointMode,
        transactions: Option<&rustdb_catalog::TransactionManager>,
    ) -> Result<()> {
        // `SingleFileCheckpointWriter::CreateCheckpoint`: no checkpoint of an invalidated
        // instance; an invalidated database is not written to any more either.
        if self.instance_validity.invalidated().is_some() || self.validity.invalidated().is_some() {
            return Ok(());
        }
        let failed = |error: Error| self.checkpoint_failed(&error);
        match &self.storage {
            DatabaseStorage::File(storage) => crate::storage::create_checkpoint(
                &self.catalog,
                storage,
                mode,
                transactions,
                &failed,
                &|| self.invalidated_message(),
            ),
            DatabaseStorage::InMemory(storage) => crate::storage::create_in_memory_checkpoint(
                &self.catalog,
                storage,
                mode,
                transactions,
                &failed,
            ),
        }
    }

    /// Closes the database, writing a checkpoint if `checkpoint` (`checkpoint_on_shutdown`)
    /// is on and the database and instance are still valid (`AttachedDatabase::Close`).
    /// With `CloseAction::Checkpoint` (`DETACH`) a failing checkpoint is an error.
    pub fn close(&self, checkpoint: bool, action: CloseAction) -> Result<()> {
        if self.closing.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Ok(());
        }
        let result = self.close_checkpoint(checkpoint, action);
        self.closed.store(true, std::sync::atomic::Ordering::Release);
        result
    }

    /// The checkpoint of [`AttachedDatabase::close`].
    fn close_checkpoint(&self, checkpoint: bool, action: CloseAction) -> Result<()> {
        let create_checkpoint = checkpoint
            && self.storage.file().is_some_and(|storage| !storage.is_read_only())
            && self.instance_validity.invalidated().is_none()
            && self.validity.invalidated().is_none();
        if !create_checkpoint {
            return Ok(());
        }
        match self.create_checkpoint(crate::storage::CheckpointMode::Automatic, None) {
            Ok(()) => Ok(()),
            // DuckDB logs the error.
            Err(_) if action == CloseAction::TryCheckpoint => Ok(()),
            Err(error) => Err(Error::new(
                error.kind(),
                format!(
                    "Detached database '{}', but CHECKPOINT during DETACH failed. \n{}",
                    self.name,
                    error.raw_message()
                ),
            )),
        }
    }

    /// `InvokeCloseIfLastReference`: closes a detached database once `database` is the
    /// last reference to it.
    pub fn close_if_last_reference(database: &Arc<Self>, checkpoint: bool) -> Result<()> {
        if Arc::strong_count(database) == 1 {
            database.close(checkpoint, CloseAction::Checkpoint)?;
        }
        Ok(())
    }
}

/// How a database is attached (the options of `ATTACH` or the start-up configuration).
#[derive(Clone, Debug, Default)]
pub(crate) struct AttachOptions {
    pub access: AccessMode,
    /// `HIDDEN`: left out of catalog listings.
    pub hidden: bool,
    pub storage_version: Option<Value>,
    pub block_size: Option<Value>,
    pub row_group_size: Option<Value>,
    /// The options `AttachOptions` does not consume itself, in order
    /// (`AttachOptions::options`), shown by `duckdb_databases()`.
    pub kept: Vec<(String, Value)>,
    /// `DEFAULT_TABLE`: the (schema, name) the database's name refers to.
    pub default_table: Option<(String, String)>,
    /// `RECOVERY_MODE NO_WAL_WRITES`: commits are not written to the WAL.
    pub no_wal_writes: bool,
    /// `TYPE`: the storage type, lowercase (`AttachOptions::db_type`); `None` for DuckDB
    /// files.
    pub db_type: Option<String>,
    /// `COMPRESS`: whether checkpoints of an in-memory database compress its tables;
    /// `None` when not given (`CompressInMemory::AUTOMATIC`).
    pub compress: Option<bool>,
}

impl AttachOptions {
    /// Whether the database is opened read-only.
    pub fn read_only(&self) -> bool {
        self.access == AccessMode::ReadOnly
    }

    /// The options of an `ATTACH` statement (`AttachOptions::AttachOptions` and
    /// `StorageOptions::Initialize`).
    pub fn from_info(info: &AttachInfo, default_access: AccessMode) -> Result<Self> {
        let mut options = Self {
            access: default_access,
            ..Self::default()
        };
        for (name, value) in info.options.iter() {
            match name.to_lowercase().as_str() {
                "readonly" | "read_only" => {
                    options.access = if value_as_bool(value)? {
                        AccessMode::ReadOnly
                    } else {
                        AccessMode::ReadWrite
                    };
                }
                "readwrite" | "read_write" => {
                    options.access = if value_as_bool(value)? {
                        AccessMode::ReadWrite
                    } else {
                        AccessMode::ReadOnly
                    };
                }
                "type" => {
                    let ty = value.to_varchar()?.to_lowercase();
                    options.db_type = (!ty.is_empty()).then_some(ty);
                }
                "hidden" => options.hidden = value_as_bool(value)?,
                "default_table" => {
                    // `QualifiedName::Parse`.
                    let (_, schema, name) = crate::sequence::parse_qualified_name(&value.to_varchar()?)?;
                    options.default_table = Some((schema, name));
                }
                "recovery_mode" => {
                    // `EnumUtil::FromString<RecoveryMode>`.
                    let mode = value.to_varchar()?;
                    options.no_wal_writes = match mode.to_ascii_uppercase().as_str() {
                        "DEFAULT" => false,
                        "NO_WAL_WRITES" => true,
                        _ => {
                            return Err(rustdb_function::logging::enum_error(
                                &mode,
                                "RecoveryMode",
                                &["DEFAULT", "NO_WAL_WRITES"],
                            ));
                        }
                    };
                }
                "vacuum_rebuild_indexes" => {}
                "compress" => {
                    options.compress = Some(value_as_bool(value)?);
                    options.kept.push((name.to_lowercase(), value.clone()));
                }
                "storage_version" => {
                    options.storage_version = Some(value.clone());
                    options.kept.push((name.to_lowercase(), value.clone()));
                }
                "block_size" => {
                    options.block_size = Some(value.clone());
                    options.kept.push((name.to_lowercase(), value.clone()));
                }
                "row_group_size" => {
                    options.row_group_size = Some(value.clone());
                    options.kept.push((name.to_lowercase(), value.clone()));
                }
                "encryption_key" => {
                    // `StorageOptions::Initialize` checks the key before anything is opened.
                    if value.logical_type().id() != LogicalTypeId::Varchar {
                        return Err(Error::binder(format!(
                            "\"{}\" is not a valid key. A key must be of type VARCHAR",
                            value.to_varchar()?
                        )));
                    }
                    if value.to_varchar()?.is_empty() {
                        return Err(Error::binder("Not a valid key. A key cannot be empty"));
                    }
                    return Err(Error::not_implemented("attaching encrypted databases"));
                }
                "encryption_cipher" | "debug_encryption_version" => {
                    return Err(Error::not_implemented("attaching encrypted databases"));
                }
                other => {
                    return Err(Error::binder(format!(
                        "Unrecognized option for attach \"{other}\""
                    )));
                }
            }
        }
        Ok(options)
    }
}

/// The database type an `ATTACH` path names with a `type:` prefix
/// (`ExtensionHelper::ExtractExtensionPrefixFromPath`): at least two alphanumeric
/// characters (a single one is a Windows drive) before a colon not starting `://`.
pub(crate) fn extension_prefix(path: &str) -> Option<&str> {
    let first_colon = path.find(':')?;
    if first_colon < 2 || path[first_colon..].starts_with("://") {
        return None;
    }
    let extension = &path[..first_colon];
    extension
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        .then_some(extension)
}

/// Whether `ATTACH` opens the file at `path` as a file to query rather than a database
/// (`DBPathAndType::CheckMagicBytes` resolving to `__open_file__`): a Parquet file, or a
/// file of unknown type, whose name a replacement scan reads
/// (`ReplacementScan::CanReplace` with Parquet, CSV and JSON suffixes).
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the name is lowercased"
)]
pub(crate) fn is_open_file(path: &str) -> bool {
    use std::io::Read as _;
    // `MagicBytes::CheckMagicBytes`.
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut buffer = [0u8; 16];
    let mut read = 0;
    while read < buffer.len() {
        match file.read(&mut buffer[read..]) {
            Ok(0) | Err(_) => break,
            Ok(count) => read += count,
        }
    }
    let magic_offset = rustdb_storage::storage_info::MAGIC_BYTE_OFFSET;
    let duckdb_magic = rustdb_storage::storage_info::MAGIC_BYTES;
    if &buffer == b"SQLite format 3\0"
        || (&buffer[..4] != b"PAR1"
            && &buffer[magic_offset..magic_offset + duckdb_magic.len()] == duckdb_magic)
    {
        return false;
    }
    let mut lower_name = path.to_lowercase();
    if lower_name.ends_with(".gz") {
        lower_name.truncate(lower_name.len() - 3);
    } else if lower_name.ends_with(".zst") {
        lower_name.truncate(lower_name.len() - 4);
    }
    ["parquet", "csv", "tsv", "json", "jsonl", "ndjson"]
        .iter()
        .any(|suffix| {
            lower_name.ends_with(&format!(".{suffix}")) || lower_name.contains(&format!(".{suffix}?"))
        })
}

/// `DefaultCastAs(BOOLEAN)` of an option value.
fn value_as_bool(value: &Value) -> Result<bool> {
    if let Some(flag) = value.as_bool() {
        return Ok(flag);
    }
    if let Some(number) = value.as_i64() {
        return Ok(number != 0);
    }
    let text = value.to_varchar()?.to_lowercase();
    match text.as_str() {
        "true" | "t" | "1" => Ok(true),
        "false" | "f" | "0" => Ok(false),
        _ => Err(Error::new(
            ErrorKind::Conversion,
            format!("Could not convert string '{text}' to BOOL"),
        )),
    }
}

/// The attached databases of an instance, in attach order.
#[derive(Debug, Default)]
pub(crate) struct DatabaseManager {
    databases: RwLock<Vec<Arc<AttachedDatabase>>>,
    /// Serializes attaching and detaching, so that a file is never opened while another
    /// attach or detach of it is under way (DuckDB reserves the path in its
    /// `DatabaseFilePathManager` for the same reason).
    attach_lock: Mutex<()>,
    /// Detached databases a transaction still uses: their files stay reserved until
    /// they are closed (`DatabaseFilePathManager`).
    /// With whether to checkpoint when closing (the `checkpoint` of the detach).
    detached: Mutex<Vec<(std::sync::Weak<AttachedDatabase>, bool)>>,
}

impl DatabaseManager {
    fn read(&self) -> RwLockReadGuard<'_, Vec<Arc<AttachedDatabase>>> {
        self.databases.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Vec<Arc<AttachedDatabase>>> {
        self.databases.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// The database called `name` (case-insensitive).
    pub fn get(&self, name: &str) -> Option<Arc<AttachedDatabase>> {
        self.read()
            .iter()
            .find(|database| database.name.eq_ignore_ascii_case(name))
            .cloned()
    }

    /// Every attached database, in attach order.
    pub fn all(&self) -> Vec<Arc<AttachedDatabase>> {
        self.read().clone()
    }

    /// Detaches the database called `name`: removes it and closes it once no transaction
    /// uses it any more, writing a checkpoint if `checkpoint` (`checkpoint_on_shutdown`) is
    /// on (`DatabaseManager::DetachDatabase`). `None` if there is no such database.
    pub fn detach(&self, name: &str, checkpoint: bool) -> Option<Result<()>> {
        let _guard = self.attach_lock.lock().unwrap_or_else(PoisonError::into_inner);
        let database = self.remove(name)?;
        let result = AttachedDatabase::close_if_last_reference(&database, checkpoint);
        if !database.is_closed() {
            self.detached
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((Arc::downgrade(&database), checkpoint));
        }
        Some(result)
    }

    /// The detached databases that are not closed yet. One whose other references are
    /// gone (they were held only briefly when it was detached) is closed now, as DuckDB
    /// closes it when its last reference goes away (`InvokeCloseIfLastReference`).
    fn detached_open(&self) -> Vec<Arc<AttachedDatabase>> {
        let mut detached = self.detached.lock().unwrap_or_else(PoisonError::into_inner);
        let mut open = Vec::new();
        detached.retain(|(database, checkpoint)| {
            let Some(database) = database.upgrade() else {
                return false;
            };
            if database.is_closed() {
                return false;
            }
            if Arc::strong_count(&database) == 1 {
                let _ = AttachedDatabase::close_if_last_reference(&database, *checkpoint);
                return false;
            }
            open.push(database);
            true
        });
        open
    }

    /// Detaches `database` if it is still attached (`ptr_eq`) under its name.
    pub fn detach_if_current(&self, database: &Arc<AttachedDatabase>, checkpoint: bool) {
        let _guard = self.attach_lock.lock().unwrap_or_else(PoisonError::into_inner);
        let current = self.get(&database.name);
        if current.is_some_and(|current| Arc::ptr_eq(&current, database))
            && let Some(removed) = self.remove(&database.name)
        {
            let _ = removed.close(checkpoint, CloseAction::TryCheckpoint);
        }
    }

    /// Removes the database called `name`.
    pub fn remove(&self, name: &str) -> Option<Arc<AttachedDatabase>> {
        let mut databases = self.write();
        let position = databases
            .iter()
            .position(|database| database.name.eq_ignore_ascii_case(name))?;
        Some(databases.remove(position))
    }

    /// Opens the database at `path` (`""` or `:memory:` for an in-memory one) as `name`
    /// and registers it (`DatabaseManager::AttachDatabase`). Returns `None` when
    /// `IF NOT EXISTS` found the name taken.
    pub fn attach(
        &self,
        name: &str,
        path: &str,
        options: &AttachOptions,
        on_conflict: OnCreateConflict,
        buffer_manager: &rustdb_buffer::BufferManager,
        instance_validity: &Arc<ValidChecker>,
    ) -> Result<Option<Arc<AttachedDatabase>>> {
        let _guard = self.attach_lock.lock().unwrap_or_else(PoisonError::into_inner);
        let path = if path == ":memory:" || path.starts_with(":memory:") {
            String::new()
        } else {
            // `FileSystem::ExpandPath`: `~` (with `home_directory`) and `file:` URLs.
            rustdb_function::file_system::expand_path(path)
        };
        let name = if name.is_empty() {
            database_name(&path)
        } else {
            name.to_owned()
        };
        if is_reserved_name(&name) {
            return Err(Error::binder(format!(
                "Attached database name \"{name}\" cannot be used because it is a reserved name"
            )));
        }
        if path.is_empty() && options.read_only() {
            return Err(Error::catalog(
                "Cannot launch in-memory database in read-only mode!",
            ));
        }
        if let Some(existing) = self.get(&name) {
            if matches!(
                on_conflict,
                OnCreateConflict::IgnoreOnConflict | OnCreateConflict::ReplaceOnConflict
            ) {
                let conflicting = match options.access {
                    AccessMode::ReadOnly => !existing.read_only,
                    AccessMode::ReadWrite => existing.read_only,
                    AccessMode::Automatic => false,
                };
                if conflicting {
                    let mode = |read_only: bool| if read_only { "READ_ONLY" } else { "READ_WRITE" };
                    return Err(Error::binder(format!(
                        "Database \"{name}\" is already attached in {} mode, cannot re-attach in {} mode",
                        mode(existing.read_only),
                        mode(options.read_only())
                    )));
                }
            }
            if let Some((schema, table)) = &options.default_table {
                existing.catalog.set_default_table(schema, table);
            }
            if on_conflict == OnCreateConflict::IgnoreOnConflict {
                return Ok(None);
            }
        }
        // A plain ATTACH registers the file path before the name, so a file conflict is
        // reported first (`InsertDatabasePath`, then the catalog set's `CreateEntry`).
        let name_taken = if on_conflict == OnCreateConflict::ReplaceOnConflict {
            None
        } else {
            self.get(&name).map(|existing| existing.name.clone())
        };
        // `DatabaseFilePathManager::InsertDatabasePath`: a file is attached once, unless
        // every attach is read-only.
        if !path.is_empty()
            && let Some(existing) = self
                .read()
                .iter()
                .chain(self.detached_open().iter())
                .find(|database| {
                    canonical_path(&database.path) == canonical_path(&path)
                        && !(database.read_only && options.read_only())
                        && !(on_conflict == OnCreateConflict::ReplaceOnConflict
                            && database.name.eq_ignore_ascii_case(&name))
                })
        {
            // `DatabaseFilePathManager::InsertDatabasePath`: the same name with IF NOT
            // EXISTS finds the file still held by the detached database.
            if on_conflict == OnCreateConflict::IgnoreOnConflict
                && existing.name.eq_ignore_ascii_case(&name)
                && !existing.is_closed()
                && self
                    .get(&name)
                    .is_none_or(|current| !Arc::ptr_eq(&current, existing))
            {
                return Err(Error::binder(format!(
                    "Unique file handle conflict: Cannot attach \"{name}\" - the database file \"{path}\" is in the \
                     process of being detached"
                )));
            }
            return Err(Error::binder(format!(
                "Unique file handle conflict: Cannot attach \"{name}\" - the database file \"{path}\" is already \
                 attached by database \"{}\"",
                existing.name
            )));
        }
        if let Some(existing) = name_taken {
            return Err(Error::binder(format!(
                "Failed to attach database: database with name \"{existing}\" already exists"
            )));
        }
        let catalog = Arc::new(Catalog::new(name.clone())?);
        let storage = if path.is_empty() {
            let storage = Arc::new(InMemoryStorageManager::new(
                buffer_manager.clone(),
                options.compress == Some(true),
            ));
            catalog.set_io(Arc::clone(storage.io()))?;
            DatabaseStorage::InMemory(storage)
        } else {
            if options.compress.is_some() {
                return Err(Error::invalid_input(
                    "COMPRESS can only be set for in-memory databases",
                ));
            }
            let storage = open_storage(&path, options, buffer_manager.clone(), &catalog)?;
            catalog.set_io(Arc::clone(storage.io()))?;
            catalog.set_storage(&storage);
            DatabaseStorage::File(storage)
        };
        if let Some((schema, table)) = &options.default_table {
            catalog.set_default_table(schema, table);
        }
        let tags = storage
            .file()
            .map(|storage| {
                // `SingleFileBlockManager::AddStorageVersionTag`.
                vec![(
                    "storage_version".to_owned(),
                    rustdb_storage::storage_info::storage_version_name(
                        storage.block_manager().storage_version(),
                        true,
                    ),
                )]
            })
            .unwrap_or_default();
        catalog.set_attach_info(rustdb_catalog::AttachedInfo {
            path: path.clone(),
            read_only: options.read_only(),
            hidden: options.hidden,
            options: options
                .kept
                .iter()
                .map(|(name, value)| Ok((name.clone(), value.to_varchar()?)))
                .collect::<Result<_>>()?,
            tags,
        });
        let database = Arc::new(AttachedDatabase {
            name: name.clone(),
            path,
            catalog,
            storage,
            read_only: options.read_only(),
            is_initial: std::sync::atomic::AtomicBool::new(false),
            validity: ValidChecker::default(),
            instance_validity: Arc::clone(instance_validity),
            closing: std::sync::atomic::AtomicBool::new(false),
            closed: std::sync::atomic::AtomicBool::new(false),
        });
        let mut databases = self.write();
        if let Some(position) = databases
            .iter()
            .position(|existing| existing.name.eq_ignore_ascii_case(&name))
        {
            // `ATTACH OR REPLACE`.
            let old = databases.remove(position);
            let _ = old.close(true, CloseAction::TryCheckpoint);
        }
        databases.push(Arc::clone(&database));
        Ok(Some(database))
    }
}

/// The absolute form of a database path, for recognising the same file under different
/// names (the parent directory resolved, the file itself may not exist yet).
fn canonical_path(path: &str) -> String {
    let path = std::path::Path::new(path);
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return resolved.to_string_lossy().into_owned();
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(file)) => {
            let parent = if parent.as_os_str().is_empty() {
                std::path::Path::new(".")
            } else {
                parent
            };
            std::fs::canonicalize(parent).map_or_else(
                |_| path.to_string_lossy().into_owned(),
                |parent| parent.join(file).to_string_lossy().into_owned(),
            )
        }
        _ => path.to_string_lossy().into_owned(),
    }
}

/// Names attached databases cannot have (`DatabaseManager::CheckReservedName`).
fn is_reserved_name(name: &str) -> bool {
    ["system", "temp", "main"]
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
}
