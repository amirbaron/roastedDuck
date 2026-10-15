use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

use rustdb_ast::OnCreateConflict;
use rustdb_common::{Error, Result};
use rustdb_function::{AggregateFunctionSet, ScalarFunctionSet, TableFunctionSet};

use crate::{CatalogEntry, CatalogSet, CatalogType, EntryKind, SchemaEntry, StorageHook, Transaction};

/// The schema new objects go to when none is given.
pub const DEFAULT_SCHEMA: &str = "main";

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

    /// The object id of the database.
    pub fn oid(&self) -> u64 {
        self.oid
    }

    /// Records how the database is attached (once).
    pub fn set_attach_info(&self, info: AttachedInfo) {
        let _ = self.attach_info.set(info);
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

    /// Registers a built-in view in `main` (DuckDB's `DefaultViewGenerator`).
    pub fn register_view(&self, name: &str, view: crate::ViewEntry) -> Result<()> {
        self.register_builtin(name.to_owned(), EntryKind::View(view))
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
        self.schemas.create_committed(CatalogEntry::new(
            name,
            EntryKind::Schema(SchemaEntry::new(&self.name, name)),
        ))
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
        let schema_entry = self.schema_entry(transaction, schema)?;
        let schema_entry = schema_entry.as_schema().ok_or_else(missing_main)?;
        let set = Self::set_for(schema_entry, entry.catalog_type());
        let name = entry.name().to_owned();
        let ty = entry.catalog_type();
        if on_conflict == OnCreateConflict::IgnoreOnConflict && set.get(transaction, &name).is_some() {
            return Ok(false);
        }
        if on_conflict == OnCreateConflict::ReplaceOnConflict
            && let Some(old) = set.get(transaction, &name)
        {
            if old.catalog_type() != ty {
                return Err(Error::catalog(format!(
                    "Existing object {name} is of type {}, trying to replace with type {}",
                    old.catalog_type().display_name(),
                    ty.display_name()
                )));
            }
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
        Ok(())
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
        Ok(())
    }
}

fn missing_main() -> Error {
    Error::internal("the main schema is missing from the catalog")
}
