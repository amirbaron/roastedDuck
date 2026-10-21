use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

use rustdb_ast::OnCreateConflict;
use rustdb_common::{Error, Result};
use rustdb_function::{AggregateFunctionSet, ScalarFunctionSet, TableFunctionSet};
use rustdb_types::Value;

use crate::{CatalogEntry, CatalogSet, CatalogType, EntryKind, SchemaEntry, StorageHook, Transaction};

/// The schema new objects go to when none is given.
pub const DEFAULT_SCHEMA: &str = "main";
/// The name of a connection's catalog of temporary objects (`TEMP_CATALOG`).
pub const TEMP_CATALOG: &str = "temp";

/// One attached database's catalog (DuckDB's `DuckCatalog`).
#[derive(Debug)]
pub struct Catalog {
    name: String,
    schemas: Arc<CatalogSet>,
    /// Where the database's tables keep their data.
    io: OnceLock<Arc<rustdb_storage::TableIoManager>>,
    /// The WAL changes to this database are logged to (database files only).
    wal: OnceLock<Arc<rustdb_storage::wal::WriteAheadLog>>,
    /// The object id of the database (`AttachedDatabase::oid`; 0 for `system`).
    oid: u64,
    /// How the database is attached, for `duckdb_databases()`.
    attach_info: OnceLock<AttachedInfo>,
    /// The table the catalog's name refers to, as (schema, name) (`DEFAULT_TABLE`;
    /// DuckDB's `Catalog::default_table`).
    default_table: std::sync::Mutex<Option<(String, String)>>,
    /// The storage of a database file (unset in memory), for `pragma_metadata_info`.
    storage: OnceLock<std::sync::Weak<rustdb_storage::storage_manager::StorageManager>>,
    /// The locks transactions writing to the database hold and checkpoints take.
    locks: rustdb_storage::checkpoint_lock::DatabaseLocks,
}

/// How a database is attached (the parts of DuckDB's `AttachedDatabase` the catalog
/// functions show).
#[derive(Clone, Debug, Default)]
pub struct AttachedInfo {
    /// The database file, empty for an in-memory database.
    pub path: String,
    pub read_only: bool,
    /// Left out of catalog listings (`HIDDEN`).
    pub hidden: bool,
    /// The ATTACH options the database keeps, as text (`AttachedDatabase::GetAttachOptions`).
    pub options: Vec<(String, String)>,
    /// `AttachedDatabase::tags` (the file's `storage_version`).
    pub tags: Vec<(String, String)>,
}

impl Catalog {
    /// A catalog containing the empty `main` schema.
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        let catalog = Self {
            schemas: Arc::new(CatalogSet::new(name.clone(), "")),
            name,
            io: OnceLock::new(),
            wal: OnceLock::new(),
            oid: 0,
            attach_info: OnceLock::new(),
            default_table: std::sync::Mutex::default(),
            storage: OnceLock::new(),
            locks: rustdb_storage::checkpoint_lock::DatabaseLocks::default(),
        };
        let mut catalog = catalog;
        if !catalog.name.eq_ignore_ascii_case("system") {
            catalog.oid = crate::next_oid();
        }
        let mut main = CatalogEntry::new(
            DEFAULT_SCHEMA,
            EntryKind::Schema(SchemaEntry::new(&catalog.name, DEFAULT_SCHEMA)),
        );
        main.internal = true;
        catalog.schemas.create_committed(main)?;
        Ok(catalog)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The locks transactions writing to the database hold and checkpoints take
    /// (`DuckTransactionManager::checkpoint_lock` and `vacuum_lock`).
    pub fn locks(&self) -> &rustdb_storage::checkpoint_lock::DatabaseLocks {
        &self.locks
    }

    /// The object id of the database.
    pub fn oid(&self) -> u64 {
        self.oid
    }

    /// Records how the database is attached (once).
    pub fn set_attach_info(&self, info: AttachedInfo) {
        let _ = self.attach_info.set(info);
    }

    /// Makes the catalog's name refer to `schema.name` (`Catalog::SetDefaultTable`); an
    /// empty schema is `main`.
    pub fn set_default_table(&self, schema: &str, name: &str) {
        let schema = if schema.is_empty() { DEFAULT_SCHEMA } else { schema };
        *self
            .default_table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((schema.to_owned(), name.to_owned()));
    }

    /// The (schema, name) of the default table, if any (`Catalog::HasDefaultTable`).
    pub fn default_table(&self) -> Option<(String, String)> {
        self.default_table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Records the storage of the database file (once).
    pub fn set_storage(&self, storage: &Arc<rustdb_storage::storage_manager::StorageManager>) {
        let _ = self.storage.set(Arc::downgrade(storage));
    }

    /// The serialization version the database file is written with; `None` in memory
    /// (`StorageManager::GetStorageVersion`).
    pub fn storage_version(&self) -> Option<u64> {
        self.storage
            .get()
            .and_then(std::sync::Weak::upgrade)
            .map(|storage| storage.block_manager().storage_version())
    }

    /// The metadata blocks of the database file; empty in memory
    /// (`DuckCatalog::GetMetadataInfo`).
    pub fn metadata_info(&self) -> Vec<rustdb_storage::metadata::MetadataBlockInfo> {
        self.storage
            .get()
            .and_then(std::sync::Weak::upgrade)
            .map(|storage| storage.metadata().metadata_info())
            .unwrap_or_default()
    }

    /// How the database is attached; `None` for `system`.
    pub fn attach_info(&self) -> Option<&AttachedInfo> {
        self.attach_info.get()
    }

    /// Sets where the database's tables keep their data (once).
    pub fn set_io(&self, io: Arc<rustdb_storage::TableIoManager>) -> Result<()> {
        self.io
            .set(io)
            .map_err(|_| Error::internal("the storage of a catalog is set once"))
    }

    /// Where the database's tables keep their data.
    pub fn io(&self) -> Result<&Arc<rustdb_storage::TableIoManager>> {
        self.io
            .get()
            .ok_or_else(|| Error::internal(format!("catalog \"{}\" has no storage", self.name)))
    }

    /// Logs every following commit to this database to `wal` (once, after replay).
    pub fn set_wal(&self, wal: Arc<rustdb_storage::wal::WriteAheadLog>) -> Result<()> {
        self.wal
            .set(wal)
            .map_err(|_| Error::internal("the WAL of a database is set once"))
    }

    /// The WAL changes to this database are logged to.
    pub fn wal(&self) -> Option<&Arc<rustdb_storage::wal::WriteAheadLog>> {
        self.wal.get()
    }

    pub fn schemas(&self) -> &Arc<CatalogSet> {
        &self.schemas
    }

    /// The schema called `name`, if visible to `transaction`.
    pub fn schema(&self, transaction: &Transaction, name: &str) -> Option<Arc<CatalogEntry>> {
        self.schemas.get(transaction, name)
    }

    /// Registers a built-in scalar function set in `main`.
    pub fn register_scalar_functions(&self, set: ScalarFunctionSet) -> Result<()> {
        let name = set.name.to_string();
        self.register_builtin(name, EntryKind::ScalarFunction(set))
    }

    /// Registers a built-in aggregate function set in `main`.
    pub fn register_aggregate_functions(&self, set: AggregateFunctionSet) -> Result<()> {
        let name = set.name.to_string();
        self.register_builtin(name, EntryKind::AggregateFunction(set))
    }

    /// Registers a built-in table function set in `main`.
    pub fn register_table_functions(&self, set: TableFunctionSet) -> Result<()> {
        let name = set.name.to_string();
        self.register_builtin(name, EntryKind::TableFunction(set))
    }

    /// Registers a built-in pragma in `main`.
    pub fn register_pragma_functions(&self, set: rustdb_function::PragmaFunctionSet) -> Result<()> {
        let name = set.name.to_string();
        self.register_builtin(name, EntryKind::PragmaFunction(set))
    }

    /// Registers a built-in COPY file format in `main`.
    pub fn register_copy_function(&self, function: rustdb_function::CopyFunction) -> Result<()> {
        let name = function.name.to_string();
        self.register_builtin(name, EntryKind::CopyFunction(Box::new(function)))
    }

    /// Registers a built-in view in `main` (DuckDB's `DefaultViewGenerator`).
    pub fn register_view(&self, name: &str, view: crate::ViewEntry) -> Result<()> {
        self.register_builtin(name.to_owned(), EntryKind::View(view))
    }

    /// Registers a built-in view in `schema`, creating the (internal) schema if needed
    /// (DuckDB's `DefaultSchemaGenerator` and `DefaultViewGenerator`).
    pub fn register_view_in(&self, schema: &str, name: &str, view: crate::ViewEntry) -> Result<()> {
        if self.schema(&Transaction::system(), schema).is_none() {
            let mut entry =
                CatalogEntry::new(schema, EntryKind::Schema(SchemaEntry::new(&self.name, schema)));
            entry.internal = true;
            self.schemas.create_committed(entry)?;
        }
        let mut entry = CatalogEntry::new(name, EntryKind::View(view));
        entry.internal = true;
        self.create_entry_committed(schema, entry)
    }

    /// Adds the overloads of `set` to the built-in function of that name (DuckDB's
    /// `ExtensionLoader::RegisterFunction`, `ALTER_ON_CONFLICT` with
    /// `MergeFunctionSet(new, override = true)`): an overload with the same arguments,
    /// varargs and return type replaces the existing one in place, the others are appended.
    pub fn register_scalar_functions_merged(&self, set: ScalarFunctionSet) -> Result<()> {
        let schema = self
            .schema(&Transaction::system(), DEFAULT_SCHEMA)
            .ok_or_else(missing_main)?;
        let schema = schema.as_schema().ok_or_else(missing_main)?;
        let existing = schema.functions.get_committed(&set.name);
        let Some(existing) = existing else {
            return self.register_scalar_functions(set);
        };
        let EntryKind::ScalarFunction(current) = &existing.kind else {
            return Err(rustdb_common::Error::internal(format!(
                "\"{}\" is not a scalar function",
                set.name
            )));
        };
        let mut merged = current.clone();
        merged.merge_function_set(set, true);
        let mut entry = CatalogEntry::new(existing.name(), EntryKind::ScalarFunction(merged));
        entry.oid = existing.oid;
        entry.internal = true;
        schema.functions.replace_committed(entry);
        Ok(())
    }

    /// Adds the overloads of `set` to the built-in table function of that name
    /// (`MergeFunctionSet` without override: an existing overload is an error).
    pub fn register_table_functions_merged(&self, set: TableFunctionSet) -> Result<()> {
        let schema = self
            .schema(&Transaction::system(), DEFAULT_SCHEMA)
            .ok_or_else(missing_main)?;
        let schema = schema.as_schema().ok_or_else(missing_main)?;
        let existing = schema.table_functions.get_committed(&set.name);
        let Some(existing) = existing else {
            return self.register_table_functions(set);
        };
        let EntryKind::TableFunction(current) = &existing.kind else {
            return Err(rustdb_common::Error::internal(format!(
                "\"{}\" is not a table function",
                set.name
            )));
        };
        let mut merged = current.clone();
        let name = Arc::clone(&set.name);
        if !merged.merge_function_set(set) {
            return Err(rustdb_common::Error::catalog(format!(
                "Failed to add new function overloads to function \"{name}\": function overload already exists"
            )));
        }
        let mut entry = CatalogEntry::new(existing.name(), EntryKind::TableFunction(merged));
        entry.oid = existing.oid;
        entry.internal = true;
        schema.table_functions.replace_committed(entry);
        Ok(())
    }

    /// Applies `alter` to the overloads of the built-in scalar function `name` (an
    /// extension patching a built-in function).
    pub fn alter_builtin_scalar_functions(
        &self,
        name: &str,
        alter: impl FnOnce(&mut ScalarFunctionSet) -> Result<()>,
    ) -> Result<()> {
        let schema = self
            .schema(&Transaction::system(), DEFAULT_SCHEMA)
            .ok_or_else(missing_main)?;
        let schema = schema.as_schema().ok_or_else(missing_main)?;
        let existing = schema
            .functions
            .get_committed(name)
            .ok_or_else(|| rustdb_common::Error::internal(format!("Function \"{name}\" not found")))?;
        let EntryKind::ScalarFunction(current) = &existing.kind else {
            return Err(rustdb_common::Error::internal(format!(
                "\"{name}\" is not a scalar function"
            )));
        };
        let mut altered = current.clone();
        alter(&mut altered)?;
        let mut entry = CatalogEntry::new(existing.name(), EntryKind::ScalarFunction(altered));
        entry.oid = existing.oid;
        entry.internal = true;
        schema.functions.replace_committed(entry);
        Ok(())
    }

    /// Registers a built-in collation in `main` (DuckDB's `ExtensionLoader::RegisterCollation`).
    pub fn register_collation(&self, name: &str, collation: crate::CollationEntry) -> Result<()> {
        let schema = self
            .schema(&Transaction::system(), DEFAULT_SCHEMA)
            .ok_or_else(missing_main)?;
        let schema = schema.as_schema().ok_or_else(missing_main)?;
        let mut entry = CatalogEntry::new(name, EntryKind::Collation(collation));
        entry.internal = true;
        schema.collations.replace_committed(entry);
        Ok(())
    }

    /// Adds an internal entry to the set of `main` its kind belongs to.
    fn register_builtin(&self, name: String, kind: EntryKind) -> Result<()> {
        let schema = self
            .schema(&Transaction::system(), DEFAULT_SCHEMA)
            .ok_or_else(missing_main)?;
        let schema = schema.as_schema().ok_or_else(missing_main)?;
        let mut entry = CatalogEntry::new(name, kind);
        entry.internal = true;
        let set = match entry.catalog_type() {
            CatalogType::TableFunction => &schema.table_functions,
            CatalogType::Table | CatalogType::View => &schema.tables,
            CatalogType::CopyFunction => &schema.copy_functions,
            CatalogType::PragmaFunction => &schema.pragma_functions,
            _ => &schema.functions,
        };
        set.create_committed(entry)
    }

    /// Looks up an entry of type `ty` in `schema`.
    pub fn get_entry(
        &self,
        transaction: &Transaction,
        ty: CatalogType,
        schema: &str,
        name: &str,
    ) -> Option<Arc<CatalogEntry>> {
        let schema_entry = self.schema(transaction, schema)?;
        let schema_entry = schema_entry.as_schema()?;
        let set = match ty {
            CatalogType::Table | CatalogType::View => &schema_entry.tables,
            CatalogType::ScalarFunction | CatalogType::AggregateFunction | CatalogType::Macro => {
                &schema_entry.functions
            }
            CatalogType::TableFunction | CatalogType::TableMacro => &schema_entry.table_functions,
            CatalogType::PragmaFunction => &schema_entry.pragma_functions,
            CatalogType::CopyFunction => &schema_entry.copy_functions,
            CatalogType::Sequence => &schema_entry.sequences,
            CatalogType::Type => &schema_entry.types,
            CatalogType::Collation => &schema_entry.collations,
            CatalogType::Index => &schema_entry.indexes,
            CatalogType::Schema => return self.schema(transaction, name),
        };
        set.get(transaction, name)
    }

    /// The set of `schema` holding entries of type `ty` (`DuckSchemaEntry::GetCatalogSet`).
    fn set_for(schema: &SchemaEntry, ty: CatalogType) -> &Arc<CatalogSet> {
        match ty {
            CatalogType::Table | CatalogType::View => &schema.tables,
            CatalogType::TableFunction | CatalogType::TableMacro => &schema.table_functions,
            CatalogType::PragmaFunction => &schema.pragma_functions,
            CatalogType::CopyFunction => &schema.copy_functions,
            CatalogType::Sequence => &schema.sequences,
            CatalogType::Type => &schema.types,
            CatalogType::Collation => &schema.collations,
            CatalogType::Index => &schema.indexes,
            CatalogType::Schema
            | CatalogType::ScalarFunction
            | CatalogType::AggregateFunction
            | CatalogType::Macro => &schema.functions,
        }
    }

    fn schema_entry(&self, transaction: &Transaction, schema: &str) -> Result<Arc<CatalogEntry>> {
        self.schema(transaction, schema)
            .ok_or_else(|| Error::catalog(format!("Schema with name {schema} does not exist!")))
    }

    /// Creates a schema read from a checkpoint, visible to every transaction; an existing
    /// schema is kept (`IGNORE_ON_CONFLICT`).
    pub fn create_schema_committed(&self, name: &str) -> Result<()> {
        if self.schema(&Transaction::system(), name).is_some() {
            return Ok(());
        }
        let mut entry = CatalogEntry::new(name, EntryKind::Schema(SchemaEntry::new(&self.name, name)));
        // Every catalog generates these schemas on demand, as internal ones; older files
        // stored them (`DefaultSchemaGenerator`).
        entry.internal = ["information_schema", "pg_catalog"]
            .iter()
            .any(|schema| schema.eq_ignore_ascii_case(name));
        self.schemas.create_committed(entry)
    }

    /// Adds an entry read from a checkpoint or replayed from the WAL to `schema`, visible
    /// to every transaction.
    pub fn create_entry_committed(&self, schema: &str, entry: CatalogEntry) -> Result<()> {
        let schema_entry = self.schema_entry(&Transaction::system(), schema)?;
        let schema_entry = schema_entry.as_schema().ok_or_else(missing_main)?;
        Self::set_for(schema_entry, entry.catalog_type()).create_committed(entry)
    }

    /// `CREATE SCHEMA` (DuckDB's `DuckCatalog::CreateSchema`).
    pub fn create_schema(
        &self,
        transaction: &Transaction,
        name: &str,
        on_conflict: OnCreateConflict,
    ) -> Result<()> {
        let entry = CatalogEntry::new(name, EntryKind::Schema(SchemaEntry::new(&self.name, name)));
        if self.schemas.create(transaction, entry)? {
            return Ok(());
        }
        match on_conflict {
            OnCreateConflict::IgnoreOnConflict => Ok(()),
            OnCreateConflict::ReplaceOnConflict => {
                // Dependent entries make the drop fail (no CASCADE).
                self.drop_schema(transaction, name, false, false)?;
                let entry = CatalogEntry::new(name, EntryKind::Schema(SchemaEntry::new(&self.name, name)));
                if !self.schemas.create(transaction, entry)? {
                    return Err(Error::internal(
                        "Failed to create schema entry in CREATE_OR_REPLACE",
                    ));
                }
                Ok(())
            }
            _ => Err(Error::catalog(format!(
                "Schema with name \"{name}\" already exists!"
            ))),
        }
    }

    /// Adds `entry` to `schema` (DuckDB's `DuckSchemaEntry::AddEntryInternal`). Returns
    /// whether it was created (`IF NOT EXISTS` on an existing entry creates nothing).
    pub fn create_entry(
        &self,
        transaction: &Transaction,
        schema: &str,
        entry: CatalogEntry,
        on_conflict: OnCreateConflict,
    ) -> Result<bool> {
        let created = self.create_entry_in_schema(transaction, schema, entry, on_conflict)?;
        if created {
            // The entry depends on its schema, which must still exist when it commits.
            transaction.push_catalog_undo(Box::new(crate::transaction::CommitCheck::SchemaExists {
                name: schema.to_owned(),
                schemas: Arc::clone(&self.schemas),
            }));
        }
        Ok(created)
    }

    fn create_entry_in_schema(
        &self,
        transaction: &Transaction,
        schema: &str,
        entry: CatalogEntry,
        on_conflict: OnCreateConflict,
    ) -> Result<bool> {
        let schema_entry = self.schema_entry(transaction, schema)?;
        let schema_entry = schema_entry.as_schema().ok_or_else(missing_main)?;
        let set = Self::set_for(schema_entry, entry.catalog_type());
        let name = entry.name().to_owned();
        let ty = entry.catalog_type();
        if ty == CatalogType::Type && is_builtin_type(schema, &name) {
            // The builtin types of `main` exist in every catalog (DuckDB's
            // `DefaultTypeGenerator`) and are internal entries.
            return match on_conflict {
                OnCreateConflict::IgnoreOnConflict => Ok(false),
                OnCreateConflict::ReplaceOnConflict => Err(internal_entry_error(&name)),
                _ => Err(Error::catalog(format!(
                    "Type with name \"{name}\" already exists!"
                ))),
            };
        }
        if on_conflict == OnCreateConflict::IgnoreOnConflict && set.get(transaction, &name).is_some() {
            return Ok(false);
        }
        if on_conflict == OnCreateConflict::ReplaceOnConflict
            && let Some(old) = set.get(transaction, &name)
        {
            if entry
                .dependencies
                .iter()
                .any(|dependency| old.is_described_by(schema, &dependency.entry))
            {
                return Err(Error::catalog(
                    "CREATE OR REPLACE is not allowed to depend on itself",
                ));
            }
            if old.catalog_type() != ty {
                return Err(Error::catalog(format!(
                    "Existing object {name} is of type {}, trying to replace with type {}",
                    old.catalog_type().display_name(),
                    ty.display_name()
                )));
            }
            self.check_drop_dependencies(transaction, schema, &old, false)?;
            set.drop_entry(transaction, &name, entry.internal)?;
        }
        if set.create(transaction, entry)? {
            return Ok(true);
        }
        let existing = set
            .get(transaction, &name)
            .map_or(ty, |existing| existing.catalog_type());
        Err(Error::catalog(format!(
            "{} with name \"{name}\" already exists!",
            existing.display_name()
        )))
    }

    /// Replaces the entry `name` of type `ty` in `schema` by `entry` (DuckDB's `Alter`).
    pub fn replace_entry(
        &self,
        transaction: &Transaction,
        schema: &str,
        ty: CatalogType,
        name: &str,
        entry: CatalogEntry,
    ) -> Result<()> {
        let schema_entry = self.schema_entry(transaction, schema)?;
        let schema_entry = schema_entry.as_schema().ok_or_else(missing_main)?;
        let set = Self::set_for(schema_entry, ty);
        if !set.alter(transaction, name, entry)? {
            return Err(Error::catalog(format!(
                "{} with name {name} does not exist!",
                ty.display_name()
            )));
        }
        Ok(())
    }

    /// Replaces the entry `name` of type `ty` in `schema` by `entry` after the ALTER
    /// `info` (which is logged) changed storage as described by `hook`, which a rollback
    /// undoes.
    #[allow(clippy::too_many_arguments, reason = "mirrors CatalogSet::AlterEntry")]
    pub fn alter_entry(
        &self,
        transaction: &Transaction,
        schema: &str,
        ty: CatalogType,
        name: &str,
        entry: CatalogEntry,
        hook: StorageHook,
        info: Option<rustdb_ast::AlterInfo>,
    ) -> Result<()> {
        let schema_entry = self.schema_entry(transaction, schema)?;
        let schema_entry = schema_entry.as_schema().ok_or_else(missing_main)?;
        let set = Self::set_for(schema_entry, ty);
        if !set.alter_with_hook(transaction, name, entry, hook, info)? {
            return Err(Error::catalog(format!(
                "{} with name {name} does not exist!",
                ty.display_name()
            )));
        }
        Ok(())
    }

    /// Renames the entry `name` of type `ty` in `schema`; `entry` is the renamed entry
    /// (`CatalogSet::AlterEntry` with `RENAME`), `info` the logged ALTER.
    #[allow(clippy::too_many_arguments, reason = "mirrors CatalogSet::AlterEntry")]
    pub fn rename_entry(
        &self,
        transaction: &Transaction,
        schema: &str,
        ty: CatalogType,
        name: &str,
        entry: CatalogEntry,
        hook: StorageHook,
        info: Option<rustdb_ast::AlterInfo>,
    ) -> Result<()> {
        let schema_entry = self.schema_entry(transaction, schema)?;
        let schema_entry = schema_entry.as_schema().ok_or_else(missing_main)?;
        let set = Self::set_for(schema_entry, ty);
        // A rename that only changes the case replaces the entry in place
        // (`CatalogSet::AlterEntry` skips `RenameEntryInternal`).
        if entry.name().eq_ignore_ascii_case(name) {
            if !set.alter_with_hook(transaction, name, entry, hook, info)? {
                return Err(Error::catalog(format!(
                    "{} with name {name} does not exist!",
                    ty.display_name()
                )));
            }
            return Ok(());
        }
        if set.get(transaction, entry.name()).is_some() {
            return Err(Error::catalog(format!(
                "Could not rename \"{name}\" to \"{}\": another entry with this name already exists!",
                entry.name()
            )));
        }
        if !set.rename(transaction, name, entry, hook, info)? {
            return Err(Error::catalog(format!(
                "{} with name {name} does not exist!",
                ty.display_name()
            )));
        }
        Ok(())
    }

    /// The index entries of the table `table` in `schema` (the entries depending on it).
    pub fn table_indexes(
        &self,
        transaction: &Transaction,
        schema: &str,
        table: &str,
    ) -> Vec<Arc<CatalogEntry>> {
        let Some(schema_entry) = self.schema(transaction, schema) else {
            return Vec::new();
        };
        let Some(schema_entry) = schema_entry.as_schema() else {
            return Vec::new();
        };
        schema_entry
            .indexes
            .scan(transaction)
            .into_iter()
            .filter(|index| matches!(&index.kind, EntryKind::Index(info) if info.info.table.eq_ignore_ascii_case(table)))
            .collect()
    }

    /// Drops the entry `name` of type `ty` from `schema` (DuckDB's
    /// `DuckSchemaEntry::DropEntry`); schemas are dropped with their contents only with
    /// `cascade`. A missing entry is an error unless `if_exists`.
    pub fn drop_entry(
        &self,
        transaction: &Transaction,
        ty: CatalogType,
        schema: &str,
        name: &str,
        cascade: bool,
        if_exists: bool,
    ) -> Result<()> {
        if ty == CatalogType::Schema {
            return self.drop_schema(transaction, name, cascade, if_exists);
        }
        if if_exists && self.schema(transaction, schema).is_none() {
            return Ok(());
        }
        let schema_entry = self.schema_entry(transaction, schema)?;
        let schema_entry = schema_entry.as_schema().ok_or_else(missing_main)?;
        let set = Self::set_for(schema_entry, ty);
        if ty == CatalogType::Type && is_builtin_type(schema, name) {
            return Err(internal_entry_error(name));
        }
        let Some(existing) = set.get(transaction, name) else {
            if if_exists {
                return Ok(());
            }
            return Err(Error::catalog(format!(
                "{} with name {name} does not exist!",
                ty.display_name()
            )));
        };
        if existing.catalog_type() != ty {
            return Err(Error::catalog(format!(
                "Existing object {name} is of type {}, trying to drop type {}",
                existing.catalog_type().display_name(),
                ty.display_name()
            )));
        }
        if existing.internal {
            return Err(Error::catalog(format!(
                "Cannot drop entry \"{}\" because it is an internal system entry",
                existing.name()
            )));
        }
        self.check_drop_dependencies(transaction, schema, &existing, cascade)?;
        if set.get(transaction, name).is_none() {
            // Dropped along with a dependent that owned it.
            return Ok(());
        }
        if ty == CatalogType::Table {
            // The table's indexes depend on it automatically and are dropped first
            // (`DependencyManager::DropObject`), so the WAL drops them before the table.
            for index in schema_entry.indexes.scan(transaction) {
                if let EntryKind::Index(info) = &index.kind
                    && info.info.table.eq_ignore_ascii_case(name)
                {
                    schema_entry.indexes.drop_entry(transaction, index.name(), true)?;
                }
            }
        }
        set.drop_entry(transaction, name, false)?;
        // The entries it owns go with it (`DependencyManager::CheckDropDependencies`).
        for owned in &existing.owned {
            if let Some(owned_type) = CatalogType::from_ast(owned.catalog_type) {
                self.drop_entry(transaction, owned_type, &owned.schema, &owned.name, cascade, true)?;
            }
        }
        Ok(())
    }

    /// The tables and sequences of this catalog that own `entry` of `schema`.
    fn owners(
        &self,
        transaction: &Transaction,
        schema: &str,
        entry: &CatalogEntry,
    ) -> Vec<Arc<CatalogEntry>> {
        let mut result = Vec::new();
        for schema_entry in self.schemas.scan(transaction) {
            let Some(contents) = schema_entry.as_schema() else {
                continue;
            };
            for set in [&contents.tables, &contents.sequences] {
                result.extend(set.scan(transaction).into_iter().filter(|candidate| {
                    candidate
                        .owned
                        .iter()
                        .any(|owned| entry.is_described_by(schema, owned))
                }));
            }
        }
        result
    }

    /// `CatalogSet::AlterOwnership` and `DependencyManager::AddOwnership`: `entry` of
    /// `schema` becomes owned by the table or sequence `owner_name` of `owner_schema`,
    /// which then drops it along with itself. `info` is the logged ALTER.
    pub fn change_ownership(
        &self,
        transaction: &Transaction,
        schema: &str,
        entry: &CatalogEntry,
        (owner_schema, owner_name): (&str, &str),
        info: Option<rustdb_ast::AlterInfo>,
    ) -> Result<()> {
        let missing = || {
            Error::catalog(format!(
                "CatalogElement \"{owner_schema}.{owner_name}\" does not exist!"
            ))
        };
        // `GetEntryInternal`: the entry is about to get a new version.
        if let Some(contents) = self
            .schema(transaction, schema)
            .as_deref()
            .and_then(CatalogEntry::as_schema)
        {
            Self::set_for(contents, entry.catalog_type()).check_write_conflict(transaction, entry.name())?;
        }
        let owner_schema_entry = self.schema(transaction, owner_schema).ok_or_else(missing)?;
        let contents = owner_schema_entry.as_schema().ok_or_else(missing)?;
        let owner = contents
            .tables
            .get(transaction, owner_name)
            .or_else(|| contents.sequences.get(transaction, owner_name))
            .ok_or_else(missing)?;
        let owner_schema = owner_schema_entry.name().to_owned();
        let dependency_error = |message: String| Error::new(rustdb_common::ErrorKind::Dependency, message);
        if let Some(other) = self.owners(transaction, &owner_schema, &owner).first() {
            return Err(dependency_error(format!(
                "{} can not become the owner, it is already owned by {}",
                owner.name(),
                other.name()
            )));
        }
        if let Some(owned) = entry.owned.first() {
            return Err(dependency_error(format!(
                "{} already owns {}. Cannot have circular dependencies",
                entry.name(),
                owned.name
            )));
        }
        let mut already_owned = false;
        for other in self.owners(transaction, schema, entry) {
            if Arc::ptr_eq(&other, &owner) {
                already_owned = true;
            } else {
                return Err(dependency_error(format!(
                    "{} is already owned by {}",
                    entry.name(),
                    other.name()
                )));
            }
        }
        if already_owned {
            return Ok(());
        }
        let kind = match &owner.kind {
            EntryKind::Table(table) => EntryKind::Table(table.clone()),
            EntryKind::View(view) => EntryKind::View(view.clone()),
            EntryKind::Sequence(sequence) => EntryKind::Sequence(sequence.clone()),
            _ => return Err(missing()),
        };
        let mut new_owner = CatalogEntry::new(owner.name(), kind);
        new_owner.inherit_links(&owner);
        new_owner.owned.push(rustdb_ast::CatalogEntryInfo {
            catalog_type: entry.dependency_type(),
            schema: schema.to_owned(),
            name: entry.name().to_owned(),
        });
        self.alter_entry(
            transaction,
            &owner_schema,
            owner.catalog_type(),
            owner.name(),
            new_owner,
            StorageHook::None,
            info,
        )
    }

    /// `COMMENT ON`: a copy of `entry` with `comment` replaces it (the `SET_COMMENT` case
    /// of `CatalogSet::AlterEntry`). `info` is the logged ALTER.
    pub fn set_comment(
        &self,
        transaction: &Transaction,
        schema: &str,
        entry: &CatalogEntry,
        comment: Value,
        info: Option<rustdb_ast::AlterInfo>,
    ) -> Result<()> {
        let mut copy = entry
            .try_copy()
            .ok_or_else(|| Error::internal("COMMENT ON an entry that cannot be copied"))?;
        copy.comment = comment;
        self.alter_entry(
            transaction,
            schema,
            entry.catalog_type(),
            entry.name(),
            copy,
            StorageHook::None,
            info,
        )
    }

    /// `COMMENT ON COLUMN` of a table (`DuckTableEntry::SetColumnComment`) or a view
    /// (`ViewCatalogEntry::AlterEntry`). `info` is the logged ALTER.
    pub fn set_column_comment(
        &self,
        transaction: &Transaction,
        schema: &str,
        entry: &CatalogEntry,
        (column, comment): (&str, Value),
        info: Option<rustdb_ast::AlterInfo>,
    ) -> Result<()> {
        let mut copy = entry
            .try_copy()
            .ok_or_else(|| Error::internal("COMMENT ON COLUMN of an entry that cannot be copied"))?;
        match &mut copy.kind {
            EntryKind::Table(table) => {
                let Some(target) = table
                    .columns
                    .iter_mut()
                    .find(|candidate| candidate.name.eq_ignore_ascii_case(column))
                else {
                    return Err(Error::binder(format!(
                        "Table \"{}\" does not have a column with name \"{column}\"",
                        entry.name()
                    )));
                };
                target.comment = comment;
            }
            EntryKind::View(view) => {
                let exists = (0..view.types.len()).any(|index| {
                    view.aliases
                        .get(index)
                        .or_else(|| view.names.get(index))
                        .is_some_and(|name| name == column)
                });
                if !exists {
                    return Err(Error::binder(format!(
                        "View \"{}\" does not have a column with name \"{column}\"",
                        entry.name()
                    )));
                }
                match view.column_comments.get_mut(column) {
                    Some(existing) => *existing = comment,
                    None => view.column_comments.push(column.to_owned(), comment),
                }
            }
            _ => return Err(Error::internal("COMMENT ON COLUMN of an unexpected entry")),
        }
        self.alter_entry(
            transaction,
            schema,
            entry.catalog_type(),
            entry.name(),
            copy,
            StorageHook::None,
            info,
        )
    }

    /// The entries of this catalog that depend on `entry` of `schema` (DuckDB's
    /// `DependencyManager::ScanDependents`).
    pub fn dependents(
        &self,
        transaction: &Transaction,
        schema: &str,
        entry: &CatalogEntry,
    ) -> Vec<(String, Arc<CatalogEntry>)> {
        let mut result = Vec::new();
        for schema_entry in self.schemas.scan(transaction) {
            let Some(contents) = schema_entry.as_schema() else {
                continue;
            };
            for set in [
                &contents.tables,
                &contents.functions,
                &contents.table_functions,
                &contents.sequences,
                &contents.types,
                &contents.indexes,
            ] {
                for candidate in set.scan(transaction) {
                    if candidate
                        .dependencies
                        .iter()
                        .any(|dependency| entry.is_described_by(schema, &dependency.entry))
                        || candidate
                            .owned
                            .iter()
                            .any(|owned| entry.is_described_by(schema, owned))
                    {
                        result.push((schema_entry.name().to_owned(), candidate));
                    }
                }
            }
        }
        result
    }

    /// `DependencyManager::CheckDropDependencies` / `DropObject`: without CASCADE the
    /// dependents block the drop; with it they are dropped first.
    fn check_drop_dependencies(
        &self,
        transaction: &Transaction,
        schema: &str,
        entry: &CatalogEntry,
        cascade: bool,
    ) -> Result<()> {
        let dependents = self.dependents(transaction, schema, entry);
        if dependents.is_empty() {
            return Ok(());
        }
        if !cascade {
            let mut message = format!(
                "Cannot drop entry \"{}\" because there are entries that depend on it.\n",
                entry.name()
            );
            self.collect_dependents(transaction, &dependents, entry, &mut message);
            message.push_str("Use DROP...CASCADE to drop all dependents.");
            return Err(Error::new(rustdb_common::ErrorKind::Dependency, message));
        }
        for (dependent_schema, dependent) in dependents {
            self.drop_entry(
                transaction,
                dependent.catalog_type(),
                &dependent_schema,
                dependent.name(),
                true,
                true,
            )?;
        }
        Ok(())
    }

    /// `DependencyManager::CollectDependents`.
    fn collect_dependents(
        &self,
        transaction: &Transaction,
        dependents: &[(String, Arc<CatalogEntry>)],
        subject: &CatalogEntry,
        message: &mut String,
    ) {
        for (schema, dependent) in dependents {
            let _ = writeln!(
                message,
                "{} depends on {}.",
                entry_to_string(dependent),
                entry_to_string(subject)
            );
            let nested = self.dependents(transaction, schema, dependent);
            if !nested.is_empty() {
                self.collect_dependents(transaction, &nested, dependent, message);
            }
        }
    }

    fn drop_schema(
        &self,
        transaction: &Transaction,
        name: &str,
        cascade: bool,
        if_exists: bool,
    ) -> Result<()> {
        let Some(schema) = self.schema(transaction, name) else {
            if if_exists {
                return Ok(());
            }
            return Err(Error::catalog(format!("Schema with name {name} does not exist!")));
        };
        if schema.internal {
            return Err(Error::catalog(format!(
                "Cannot drop entry \"{}\" because it is an internal system entry",
                schema.name()
            )));
        }
        let contents = schema.as_schema().ok_or_else(missing_main)?;
        let sets = [
            &contents.tables,
            &contents.functions,
            &contents.table_functions,
            &contents.sequences,
            &contents.types,
            &contents.indexes,
        ];
        let dependents: Vec<Arc<CatalogEntry>> = sets
            .iter()
            .flat_map(|set| set.scan(transaction))
            .filter(|entry| !entry.internal)
            .collect();
        if !dependents.is_empty() && !cascade {
            let mut message = format!(
                "Cannot drop entry \"{}\" because there are entries that depend on it.\n",
                schema.name()
            );
            for entry in dependents.iter().rev() {
                let _ = writeln!(
                    message,
                    "{} \"{}\" depends on schema \"{}\".",
                    entry.catalog_type().display_name().to_lowercase(),
                    entry.name(),
                    schema.name()
                );
            }
            message.push_str("Use DROP...CASCADE to drop all dependents.");
            return Err(Error::new(rustdb_common::ErrorKind::Dependency, message));
        }
        for set in sets {
            for entry in set.scan(transaction) {
                if !entry.internal {
                    set.drop_entry(transaction, entry.name(), false)?;
                }
            }
        }
        self.schemas.drop_entry(transaction, name, false)?;
        transaction.push_catalog_undo(Box::new(crate::transaction::CommitCheck::SchemaDropped {
            name: schema.name().to_owned(),
            sets: vec![
                Arc::clone(&contents.tables),
                Arc::clone(&contents.functions),
                Arc::clone(&contents.table_functions),
                Arc::clone(&contents.sequences),
                Arc::clone(&contents.types),
                Arc::clone(&contents.indexes),
            ],
        }));
        Ok(())
    }
}

/// `EntryToString` of the dependency manager (sequences print as indexes, like DuckDB).
fn entry_to_string(entry: &CatalogEntry) -> String {
    let kind = match entry.catalog_type() {
        CatalogType::Table => "table",
        CatalogType::Schema => "schema",
        CatalogType::View => "view",
        CatalogType::Index | CatalogType::Sequence => "index",
        CatalogType::Collation => "collation",
        CatalogType::Type => "type",
        CatalogType::TableFunction => "table function",
        CatalogType::ScalarFunction => "scalar function",
        CatalogType::AggregateFunction => "aggregate function",
        CatalogType::PragmaFunction => "pragma function",
        CatalogType::CopyFunction => "copy function",
        CatalogType::Macro => "macro function",
        CatalogType::TableMacro => "table macro function",
    };
    format!("{kind} \"{}\"", entry.name())
}

/// Whether `name` is one of the builtin types every `main` schema holds (DuckDB's
/// `DefaultTypeGenerator::CreateDefaultEntry`).
fn is_builtin_type(schema: &str, name: &str) -> bool {
    schema.eq_ignore_ascii_case(DEFAULT_SCHEMA) && rustdb_types::builtin_type_id(name).is_some()
}

/// DuckDB's error for dropping or replacing an internal entry.
fn internal_entry_error(name: &str) -> Error {
    Error::catalog(format!(
        "Cannot drop entry \"{name}\" because it is an internal system entry"
    ))
}

fn missing_main() -> Error {
    Error::internal("the main schema is missing from the catalog")
}

#[cfg(test)]
mod tests {
    use rustdb_types::LogicalType;

    use super::*;
    use crate::{TransactionManager, TypeEntry};

    fn type_entry(name: &str) -> CatalogEntry {
        CatalogEntry::new(
            name,
            EntryKind::Type(TypeEntry {
                user_type: LogicalType::INTEGER,
            }),
        )
    }

    #[test]
    fn builtin_types_exist_in_every_main_schema() {
        let catalog = Catalog::new("memory").unwrap();
        let manager = TransactionManager::default();
        let transaction = manager.begin();
        let error = catalog
            .create_entry(
                &transaction,
                "main",
                type_entry("INTEGER"),
                OnCreateConflict::ErrorOnConflict,
            )
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Catalog Error: Type with name \"INTEGER\" already exists!"
        );
        let created = catalog
            .create_entry(
                &transaction,
                "main",
                type_entry("integer"),
                OnCreateConflict::IgnoreOnConflict,
            )
            .unwrap();
        assert!(!created);
        assert!(
            catalog
                .create_entry(
                    &transaction,
                    "main",
                    type_entry("mood"),
                    OnCreateConflict::ErrorOnConflict
                )
                .unwrap()
        );
        assert!(
            catalog
                .get_entry(&transaction, CatalogType::Type, "main", "MOOD")
                .is_some()
        );
        catalog
            .drop_entry(&transaction, CatalogType::Type, "main", "mood", false, false)
            .unwrap();
        assert!(
            catalog
                .get_entry(&transaction, CatalogType::Type, "main", "mood")
                .is_none()
        );
    }
}
