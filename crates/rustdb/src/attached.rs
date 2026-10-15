//! Attached databases (DuckDB's `AttachedDatabase` and `DatabaseManager`): the database
//! opened first and every `ATTACH`ed one, each with its own catalog, storage and WAL.

use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use rustdb_ast::{AttachInfo, OnCreateConflict};
use rustdb_catalog::Catalog;
use rustdb_common::{Error, ErrorKind, Result};
use rustdb_storage::storage_manager::StorageManager;
use rustdb_types::Value;

use crate::AccessMode;
use crate::storage::{database_name, open_storage};

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
    /// Left out of catalog listings (`ATTACH ... (HIDDEN)`).
    pub hidden: bool,
}

impl AttachedDatabase {
    /// Writes a checkpoint when the database is closed or detached
    /// (`AttachedDatabase::Close`); errors are dropped, as DuckDB logs them.
    pub fn close(&self, checkpoint: bool) {
        if checkpoint
            && let Some(storage) = &self.storage
            && !storage.is_read_only()
        {
            let _ = crate::storage::checkpoint_on_close(&self.catalog, storage);
        }
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
                "default_table" | "recovery_mode" | "vacuum_rebuild_indexes" | "compress" => {}
                "storage_version" => options.storage_version = Some(value.clone()),
                "block_size" => options.block_size = Some(value.clone()),
                "row_group_size" => options.row_group_size = Some(value.clone()),
                "encryption_key" | "encryption_cipher" | "debug_encryption_version" => {
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
    ) -> Result<Option<Arc<AttachedDatabase>>> {
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
            && let Some(existing) = self.read().iter().find(|database| {
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
            Some(storage)
        };
        let database = Arc::new(AttachedDatabase {
            name: name.clone(),
            path,
            catalog,
            storage,
            read_only: options.read_only(),
            hidden: options.hidden,
        });
        let mut databases = self.write();
        if let Some(position) = databases
            .iter()
            .position(|existing| existing.name.eq_ignore_ascii_case(&name))
        {
            // `ATTACH OR REPLACE`.
            let old = databases.remove(position);
            old.close(true);
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
