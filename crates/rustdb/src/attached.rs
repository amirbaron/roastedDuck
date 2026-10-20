//! Attached databases (DuckDB's `AttachedDatabase` and `DatabaseManager`): the database
//! opened first and every `ATTACH`ed one, each with its own catalog, storage and WAL.

use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use rustdb_ast::{AttachInfo, OnCreateConflict};
use rustdb_catalog::Catalog;
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_storage::storage_manager::StorageManager;
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
    /// The database file, unless the database is in memory.
    pub storage: Option<Arc<StorageManager>>,
    pub read_only: bool,
    /// Whether this is the database opened at start-up (`IsInitialDatabase`).
    pub is_initial: std::sync::atomic::AtomicBool,
    /// This database's validity (a failed checkpoint invalidates it).
    pub validity: ValidChecker,
    /// The instance's validity (a failed checkpoint of the initial database invalidates
    /// the instance).
    pub instance_validity: Arc<ValidChecker>,
    /// Set once the database was closed (`is_closed`).
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
        let recovery = if self.storage.is_none() {
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

    /// Writes a checkpoint of the database if it has storage and is still valid
    /// (`SingleFileStorageManager::CreateCheckpoint`).
    pub fn create_checkpoint(
        &self,
        mode: crate::storage::CheckpointMode,
        transactions: Option<&rustdb_catalog::TransactionManager>,
    ) -> Result<()> {
        let Some(storage) = &self.storage else {
            return Ok(());
        };
        // `SingleFileCheckpointWriter::CreateCheckpoint`: no checkpoint of an invalidated
        // instance; an invalidated database is not written to any more either.
        if self.instance_validity.invalidated().is_some() || self.validity.invalidated().is_some() {
            return Ok(());
        }
        crate::storage::create_checkpoint(&self.catalog, storage, mode, transactions, &|error| {
            self.checkpoint_failed(&error)
        })
    }

    /// Closes the database, writing a checkpoint if `checkpoint` (`checkpoint_on_shutdown`)
    /// is on and the database and instance are still valid (`AttachedDatabase::Close`).
    /// With `CloseAction::Checkpoint` (`DETACH`) a failing checkpoint is an error.
    pub fn close(&self, checkpoint: bool, action: CloseAction) -> Result<()> {
        if self.closed.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Ok(());
        }
        let create_checkpoint = checkpoint
            && self
                .storage
                .as_ref()
                .is_some_and(|storage| !storage.is_read_only())
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
    /// `DEFAULT_TABLE`: the (schema, name) the database's name refers to.
    pub default_table: Option<(String, String)>,
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
                    if !ty.is_empty() && ty != "duckdb" {
                        return Err(Error::new(
                            ErrorKind::Binder,
                            format!("Unknown database type \"{ty}\""),
                        ));
                    }
                }
                "hidden" => options.hidden = value_as_bool(value)?,
                "default_table" => {
                    // `QualifiedName::Parse`.
                    let (_, schema, name) = crate::sequence::parse_qualified_name(&value.to_varchar()?)?;
                    options.default_table = Some((schema, name));
                }
                "recovery_mode" | "vacuum_rebuild_indexes" | "compress" => {}
                "storage_version" => options.storage_version = Some(value.clone()),
                "block_size" => options.block_size = Some(value.clone()),
                "row_group_size" => options.row_group_size = Some(value.clone()),
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
    detached: Mutex<Vec<std::sync::Weak<AttachedDatabase>>>,
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
                .push(Arc::downgrade(&database));
        }
        Some(result)
    }

    /// The detached databases that are not closed yet.
    fn detached_open(&self) -> Vec<Arc<AttachedDatabase>> {
        let mut detached = self.detached.lock().unwrap_or_else(PoisonError::into_inner);
        detached.retain(|database| database.upgrade().is_some_and(|database| !database.is_closed()));
        detached.iter().filter_map(std::sync::Weak::upgrade).collect()
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
            path.to_owned()
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
            if on_conflict != OnCreateConflict::ReplaceOnConflict {
                return Err(Error::binder(format!(
                    "Failed to attach database: database with name \"{}\" already exists",
                    existing.name
                )));
            }
        }
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
            return Err(Error::binder(format!(
                "Unique file handle conflict: Cannot attach \"{name}\" - the database file \"{path}\" is already \
                 attached by database \"{}\"",
                existing.name
            )));
        }
        let catalog = Arc::new(Catalog::new(name.clone())?);
        let storage = if path.is_empty() {
            catalog.set_io(Arc::new(rustdb_storage::TableIoManager::new(
                buffer_manager.clone(),
                Arc::new(rustdb_buffer::InMemoryBlockManager::default()),
            )))?;
            None
        } else {
            let storage = open_storage(&path, options, buffer_manager.clone(), &catalog)?;
            catalog.set_io(Arc::clone(storage.io()))?;
            catalog.set_storage(&storage);
            Some(storage)
        };
        if let Some((schema, table)) = &options.default_table {
            catalog.set_default_table(schema, table);
        }
        catalog.set_attach_info(rustdb_catalog::AttachedInfo {
            path: path.clone(),
            read_only: options.read_only(),
            hidden: options.hidden,
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
