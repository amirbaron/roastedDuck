use std::sync::{Arc, OnceLock};

use roastedduck_ast::OnCreateConflict;
use roastedduck_common::{ByteString, Error, Result};
use roastedduck_function::{AggregateFunctionSet, ScalarFunctionSet, TableFunctionSet};
use roastedduck_types::Value;

use crate::{
    CatalogEntry, CatalogName, CatalogSet, CatalogType, EntryKind, SchemaEntry, StorageHook, Transaction,
};

/// The schema new objects go to when none is given.
pub const DEFAULT_SCHEMA: &str = "main";
/// The name of a connection's catalog of temporary objects (`TEMP_CATALOG`).
pub const TEMP_CATALOG: &str = "temp";

/// One attached database's catalog (DuckDB's `DuckCatalog`).
#[derive(Debug)]
pub struct Catalog {
    name: CatalogName,
    schemas: Arc<CatalogSet>,
    /// Where the database's tables keep their data.
    io: OnceLock<Arc<roastedduck_storage::TableIoManager>>,
    /// The WAL changes to this database are logged to (database files only).
    wal: OnceLock<Arc<roastedduck_storage::wal::WriteAheadLog>>,
    /// The object id of the database (`AttachedDatabase::oid`; 0 for `system`).
    oid: OnceLock<u64>,
    /// Ordinary attached databases have an entry identity while initializing their
    /// catalog/storage; `FinalizeAttach` replaces it with the registered identity.
    provisional_oid: u64,
    /// How the database is attached, for `duckdb_databases()`.
    attach_info: OnceLock<AttachedInfo>,
    /// The table the catalog's name refers to, as (schema, name) (`DEFAULT_TABLE`;
    /// DuckDB's `Catalog::default_table`).
    default_table: std::sync::Mutex<Option<(ByteString, ByteString)>>,
    default_entries: Arc<std::sync::Mutex<Vec<(ByteString, ByteString, DefaultEntryKind)>>>,
    /// The storage of a database file (unset in memory), for `pragma_metadata_info`.
    storage: OnceLock<std::sync::Weak<roastedduck_storage::storage_manager::StorageManager>>,
    /// The locks transactions writing to the database hold and checkpoints take.
    locks: roastedduck_storage::checkpoint_lock::DatabaseLocks,
}

/// How a database is attached (the parts of DuckDB's `AttachedDatabase` the catalog
/// functions show).
#[derive(Clone, Debug, Default)]
pub struct AttachedInfo {
    /// The database file, empty for an in-memory database.
    pub path: ByteString,
    pub read_only: bool,
    /// Left out of catalog listings (`HIDDEN`).
    pub hidden: bool,
    /// The ATTACH options the database keeps, as text (`AttachedDatabase::GetAttachOptions`).
    pub options: Vec<(ByteString, ByteString)>,
    /// `AttachedDatabase::tags` (the file's `storage_version`).
    pub tags: Vec<(String, String)>,
    /// The cipher of an encrypted database (`DuckCatalog::GetEncryptionCipher`), `None`
    /// for a plain one.
    pub cipher: Option<String>,
}

#[derive(Clone, Debug)]
enum DefaultEntryKind {
    View(crate::ViewEntry),
    Macro(crate::MacroEntry),
    TableMacro(crate::MacroEntry),
}

impl DefaultEntryKind {
    fn into_entry_kind(self) -> EntryKind {
        match self {
            Self::View(value) => EntryKind::View(value),
            Self::Macro(value) => EntryKind::Macro(value),
            Self::TableMacro(value) => EntryKind::TableMacro(value),
        }
    }
}

impl Catalog {
    /// A catalog containing the empty `main` schema.
    pub fn new(name: impl Into<ByteString>) -> Result<Self> {
        Self::with_oid_allocator(name, Arc::default())
    }

    pub fn with_oid_allocator(
        name: impl Into<ByteString>,
        allocator: Arc<crate::OidAllocator>,
    ) -> Result<Self> {
        Self::with_registration(name, allocator, true)
    }

    pub fn unregistered(name: impl Into<ByteString>, allocator: Arc<crate::OidAllocator>) -> Result<Self> {
        Self::with_registration(name, allocator, false)
    }

    fn with_registration(
        name: impl Into<ByteString>,
        allocator: Arc<crate::OidAllocator>,
        registered: bool,
    ) -> Result<Self> {
        let name = CatalogName::with_oid_allocator(name.into(), allocator);
        let provisional_oid = if registered { 0 } else { name.next_oid() };
        let catalog = Self {
            schemas: Arc::new(CatalogSet::new(&name, "")),
            name,
            io: OnceLock::new(),
            wal: OnceLock::new(),
            oid: OnceLock::new(),
            provisional_oid,
            attach_info: OnceLock::new(),
            default_table: std::sync::Mutex::default(),
            default_entries: Arc::default(),
            storage: OnceLock::new(),
            locks: roastedduck_storage::checkpoint_lock::DatabaseLocks::default(),
        };
        if registered && !catalog.name().eq_ignore_ascii_case(b"system") {
            catalog.finalize_registration();
        }
        let mut main = catalog.new_entry(
            DEFAULT_SCHEMA,
            EntryKind::Schema(SchemaEntry::new(&catalog.name, DEFAULT_SCHEMA)),
        );
        main.internal = true;
        catalog.schemas.create_committed_chain(main)?;
        Ok(catalog)
    }

    /// Allocates from this instance, including constructors preceding conflicts.
    pub fn new_entry(&self, name: impl Into<ByteString>, kind: EntryKind) -> CatalogEntry {
        CatalogEntry::with_oid(name, kind, self.name.next_oid())
    }

    pub fn oid_allocator(&self) -> Arc<crate::OidAllocator> {
        self.name.oid_allocator()
    }

    /// Defers default entry allocation while keeping definition validation at startup.
    pub fn register_default_entry(&self, schema: &str, name: impl Into<ByteString>, kind: EntryKind) {
        let kind = match kind {
            EntryKind::View(value) => DefaultEntryKind::View(value),
            EntryKind::Macro(value) => DefaultEntryKind::Macro(value),
            EntryKind::TableMacro(value) => DefaultEntryKind::TableMacro(value),
            _ => unreachable!("only validated default views and macros are deferred"),
        };
        let schema_name = ByteString::from(schema);
        let name = name.into();
        self.default_entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((schema_name.clone(), name.clone(), kind.clone()));
        if let Some(entry) = self.schemas.get_committed_existing(schema)
            && let Some(schema) = entry.as_schema()
        {
            Self::install_default(&self.name, schema, name, kind);
            return;
        }
        let catalog_name = self.name.clone();
        let defaults = Arc::downgrade(&self.default_entries);
        self.schemas.register_default(
            schema_name.clone(),
            Arc::new(move || {
                let schema = SchemaEntry::new(&catalog_name, &schema_name);
                if let Some(defaults) = defaults.upgrade() {
                    for (owner, name, kind) in defaults
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .iter()
                    {
                        if owner.eq_ignore_ascii_case(&schema_name) {
                            Self::install_default(&catalog_name, &schema, name.clone(), kind.clone());
                        }
                    }
                }
                let mut entry = CatalogEntry::with_oid(
                    schema_name.clone(),
                    EntryKind::Schema(schema),
                    catalog_name.next_oid(),
                );
                entry.internal = true;
                entry
            }),
        );
    }

    fn install_default(
        catalog: &CatalogName,
        schema: &SchemaEntry,
        name: ByteString,
        kind: DefaultEntryKind,
    ) {
        let set = match &kind {
            DefaultEntryKind::View(_) => &schema.tables,
            DefaultEntryKind::Macro(_) => &schema.functions,
            DefaultEntryKind::TableMacro(_) => &schema.table_functions,
        };
        let allocator = catalog.clone();
        set.register_default(
            name.clone(),
            Arc::new(move || {
                let mut entry = CatalogEntry::with_oid(
                    name.clone(),
                    kind.clone().into_entry_kind(),
                    allocator.next_oid(),
                );
                entry.internal = true;
                entry
            }),
        );
    }

    pub fn name(&self) -> Arc<[u8]> {
        self.name.get()
    }

    /// Renames the catalog along with its attached database (`AttachedDatabase::SetName`).
    pub fn rename(&self, name: impl AsRef<[u8]>) {
        let name = name.as_ref();
        self.name.set(name);
    }

    /// The locks transactions writing to the database hold and checkpoints take
    /// (`DuckTransactionManager::checkpoint_lock` and `vacuum_lock`).
    pub fn locks(&self) -> &roastedduck_storage::checkpoint_lock::DatabaseLocks {
        &self.locks
    }

    /// The object id of the database.
    pub fn oid(&self) -> u64 {
        self.oid.get().copied().unwrap_or(self.provisional_oid)
    }

    /// Called once after attached storage/catalog initialization, before publication.
    pub fn finalize_registration(&self) {
        if !self.name().eq_ignore_ascii_case(b"system") {
            self.oid.get_or_init(|| self.name.next_oid());
        }
    }

    /// Records how the database is attached (once).
    pub fn set_attach_info(&self, info: AttachedInfo) {
        let _ = self.attach_info.set(info);
    }

    /// Makes the catalog's name refer to `schema.name` (`Catalog::SetDefaultTable`); an
    /// empty schema is `main`.
    pub fn set_default_table(&self, schema: impl AsRef<[u8]>, name: impl AsRef<[u8]>) {
        let name = name.as_ref();
        let schema = schema.as_ref();
        let schema = if schema.is_empty() {
            DEFAULT_SCHEMA.as_bytes()
        } else {
            schema
        };
        *self
            .default_table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((schema.into(), name.into()));
    }

    /// The (schema, name) of the default table, if any (`Catalog::HasDefaultTable`).
    pub fn default_table(&self) -> Option<(ByteString, ByteString)> {
        self.default_table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Records the storage of the database file (once).
    pub fn set_storage(&self, storage: &Arc<roastedduck_storage::storage_manager::StorageManager>) {
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
    pub fn metadata_info(&self) -> Vec<roastedduck_storage::metadata::MetadataBlockInfo> {
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
    pub fn set_io(&self, io: Arc<roastedduck_storage::TableIoManager>) -> Result<()> {
        self.io
            .set(io)
            .map_err(|_| Error::internal("the storage of a catalog is set once"))
    }

    /// Where the database's tables keep their data.
    pub fn io(&self) -> Result<&Arc<roastedduck_storage::TableIoManager>> {
        self.io.get().ok_or_else(|| {
            Error::internal(roastedduck_common::byte_format!(
                "catalog \"{}\" has no storage",
                self.name()
            ))
        })
    }

    /// Logs every following commit to this database to `wal` (once, after replay).
    pub fn set_wal(&self, wal: Arc<roastedduck_storage::wal::WriteAheadLog>) -> Result<()> {
        self.wal
            .set(wal)
            .map_err(|_| Error::internal("the WAL of a database is set once"))
    }

    /// The WAL changes to this database are logged to.
    pub fn wal(&self) -> Option<&Arc<roastedduck_storage::wal::WriteAheadLog>> {
        self.wal.get()
    }

    pub fn schemas(&self) -> &Arc<CatalogSet> {
        &self.schemas
    }

    /// The schema called `name`, if visible to `transaction`.
    pub fn schema(&self, transaction: &Transaction, name: impl AsRef<[u8]>) -> Option<Arc<CatalogEntry>> {
        let name = name.as_ref();

        self.schemas.get(transaction, name)
    }

    /// Registers a built-in scalar function set in `main`.
    pub fn register_scalar_functions(&self, set: ScalarFunctionSet) -> Result<()> {
        let name = set.name.clone();
        self.register_builtin(name, EntryKind::ScalarFunction(set))
    }

    /// Registers a built-in aggregate function set in `main`.
    pub fn register_aggregate_functions(&self, set: AggregateFunctionSet) -> Result<()> {
        let name = set.name.to_string();
        self.register_builtin(name.into(), EntryKind::AggregateFunction(set))
    }

    /// Registers a built-in table function set in `main`.
    pub fn register_table_functions(&self, set: TableFunctionSet) -> Result<()> {
        let name = set.name.to_string();
        self.register_builtin(name.into(), EntryKind::TableFunction(set))
    }

    /// Registers a built-in pragma in `main`.
    pub fn register_pragma_functions(&self, set: roastedduck_function::PragmaFunctionSet) -> Result<()> {
        let name = set.name.to_string();
        self.register_builtin(name.into(), EntryKind::PragmaFunction(set))
    }

    /// Registers a built-in COPY file format in `main`.
    pub fn register_copy_function(&self, function: roastedduck_function::CopyFunction) -> Result<()> {
        let name = function.name.to_string();
        self.register_builtin(name.into(), EntryKind::CopyFunction(Box::new(function)))
    }

    /// Registers a built-in view in `main` (DuckDB's `DefaultViewGenerator`).
    pub fn register_view(&self, name: impl AsRef<[u8]>, view: crate::ViewEntry) -> Result<()> {
        let name = name.as_ref();

        self.register_builtin(name.into(), EntryKind::View(view))
    }

    /// Registers a built-in view in `schema`, creating the (internal) schema if needed
    /// (DuckDB's `DefaultSchemaGenerator` and `DefaultViewGenerator`).
    pub fn register_view_in(
        &self,
        schema: impl AsRef<[u8]>,
        name: impl AsRef<[u8]>,
        view: crate::ViewEntry,
    ) -> Result<()> {
        let name = name.as_ref();
        let schema = schema.as_ref();

        if self.schema(&Transaction::system(), schema).is_none() {
            let mut entry = self.new_entry(schema, EntryKind::Schema(SchemaEntry::new(&self.name, schema)));
            entry.internal = true;
            self.schemas.create_committed(entry)?;
        }
        let mut entry = self.new_entry(name, EntryKind::View(view));
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
        let existing = schema.functions.get_committed(set.name.as_bytes());
        let Some(existing) = existing else {
            return self.register_scalar_functions(set);
        };
        let EntryKind::ScalarFunction(current) = &existing.kind else {
            return Err(roastedduck_common::Error::internal(
                roastedduck_common::byte_format!("\"{}\" is not a scalar function", set.name),
            ));
        };
        let mut merged = current.clone();
        merged.merge_function_set(set, true);
        let mut kind = EntryKind::ScalarFunction(merged);
        kind.set_function_catalog(self.name(), DEFAULT_SCHEMA);
        let mut entry = self.new_entry(existing.name(), kind);
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
        let existing = schema.table_functions.get_committed(set.name.as_bytes());
        let Some(existing) = existing else {
            return self.register_table_functions(set);
        };
        let EntryKind::TableFunction(current) = &existing.kind else {
            return Err(roastedduck_common::Error::internal(
                roastedduck_common::byte_format!("\"{}\" is not a table function", set.name),
            ));
        };
        let mut merged = current.clone();
        let name = Arc::clone(&set.name);
        if !merged.merge_function_set(set) {
            return Err(roastedduck_common::Error::catalog(
                roastedduck_common::byte_format!(
                    "Failed to add new function overloads to function \"{name}\": function overload already exists"
                ),
            ));
        }
        let mut kind = EntryKind::TableFunction(merged);
        kind.set_function_catalog(self.name(), DEFAULT_SCHEMA);
        let mut entry = self.new_entry(existing.name(), kind);
        entry.oid = existing.oid;
        entry.internal = true;
        schema.table_functions.replace_committed(entry);
        Ok(())
    }

    /// Applies `alter` to the overloads of the built-in scalar function `name` (an
    /// extension patching a built-in function).
    pub fn alter_builtin_scalar_functions(
        &self,
        name: impl AsRef<[u8]>,
        alter: impl FnOnce(&mut ScalarFunctionSet) -> Result<()>,
    ) -> Result<()> {
        let name = name.as_ref();

        let schema = self
            .schema(&Transaction::system(), DEFAULT_SCHEMA)
            .ok_or_else(missing_main)?;
        let schema = schema.as_schema().ok_or_else(missing_main)?;
        let existing = schema.functions.get_committed(name).ok_or_else(|| {
            roastedduck_common::Error::internal(roastedduck_common::byte_format!(
                "Function \"{name}\" not found"
            ))
        })?;
        let EntryKind::ScalarFunction(current) = &existing.kind else {
            return Err(roastedduck_common::Error::internal(
                roastedduck_common::byte_format!("\"{name}\" is not a scalar function"),
            ));
        };
        let mut altered = current.clone();
        alter(&mut altered)?;
        let mut kind = EntryKind::ScalarFunction(altered);
        kind.set_function_catalog(self.name(), DEFAULT_SCHEMA);
        let mut entry = self.new_entry(existing.name(), kind);
        entry.oid = existing.oid;
        entry.internal = true;
        schema.functions.replace_committed(entry);
        Ok(())
    }

    /// Registers a built-in collation without a separate serialization function
    /// (`BuiltinFunctions::AddCollation`).
    pub fn register_builtin_collation(
        &self,
        name: impl Into<ByteString>,
        collation: crate::CollationEntry,
    ) -> Result<()> {
        self.register_builtin(name.into(), EntryKind::Collation(collation))
    }

    /// Registers an extension collation in `main` (`ExtensionLoader::RegisterCollation`),
    /// and its function as a scalar function unless one of that name exists (for
    /// serialization, `IGNORE_ON_CONFLICT`).
    pub fn register_collation(&self, name: impl AsRef<[u8]>, collation: crate::CollationEntry) -> Result<()> {
        let name = name.as_ref();

        let schema = self
            .schema(&Transaction::system(), DEFAULT_SCHEMA)
            .ok_or_else(missing_main)?;
        let schema = schema.as_schema().ok_or_else(missing_main)?;
        let function = collation.function.clone();
        let mut entry = self.new_entry(name, EntryKind::Collation(collation));
        entry.internal = true;
        if schema.collations.get_committed(name).is_none() {
            schema.collations.create_committed_chain(entry)?;
        }
        let mut set = ScalarFunctionSet::new(function.name.clone());
        set.add(function);
        let name = set.name.clone();
        let mut kind = EntryKind::ScalarFunction(set);
        kind.set_function_catalog(self.name(), DEFAULT_SCHEMA);
        let mut entry = self.new_entry(name.clone(), kind);
        entry.internal = true;
        if schema.functions.get_committed(name.as_bytes()).is_none() {
            schema.functions.create_committed_chain(entry)?;
        }
        Ok(())
    }

    /// Adds an internal entry to the set of `main` its kind belongs to.
    fn register_builtin(&self, name: ByteString, mut kind: EntryKind) -> Result<()> {
        let schema = self
            .schema(&Transaction::system(), DEFAULT_SCHEMA)
            .ok_or_else(missing_main)?;
        let schema = schema.as_schema().ok_or_else(missing_main)?;
        kind.set_function_catalog(self.name(), DEFAULT_SCHEMA);
        let mut entry = self.new_entry(name, kind);
        entry.internal = true;
        let set = match entry.catalog_type() {
            CatalogType::TableFunction => &schema.table_functions,
            CatalogType::Table | CatalogType::View => &schema.tables,
            CatalogType::CopyFunction => &schema.copy_functions,
            CatalogType::PragmaFunction => &schema.pragma_functions,
            CatalogType::Collation => &schema.collations,
            _ => &schema.functions,
        };
        set.create_committed_chain(entry)
    }

    /// Looks up an entry of type `ty` in `schema`.
    pub fn get_entry(
        &self,
        transaction: &Transaction,
        ty: CatalogType,
        schema: impl AsRef<[u8]>,
        name: impl AsRef<[u8]>,
    ) -> Option<Arc<CatalogEntry>> {
        let name = name.as_ref();
        let schema = schema.as_ref();

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

    fn schema_entry(&self, transaction: &Transaction, schema: impl AsRef<[u8]>) -> Result<Arc<CatalogEntry>> {
        let schema = schema.as_ref();

        self.schema(transaction, schema)
            .ok_or_else(|| missing_entry(CatalogType::Schema, schema))
    }

    /// Creates a schema read from a checkpoint, visible to every transaction; an existing
    /// schema is kept (`IGNORE_ON_CONFLICT`).
    pub fn create_schema_committed(&self, name: impl AsRef<[u8]>) -> Result<()> {
        let name = name.as_ref();

        if self.schema(&Transaction::system(), name).is_some() {
            return Ok(());
        }
        let mut entry = self.new_entry(name, EntryKind::Schema(SchemaEntry::new(&self.name, name)));
        // Every catalog generates these schemas on demand, as internal ones; older files
        // stored them (`DefaultSchemaGenerator`).
        entry.internal = ["information_schema", "pg_catalog"]
            .iter()
            .any(|schema| schema.as_bytes().eq_ignore_ascii_case(name));
        self.schemas.create_committed(entry)
    }

    /// Adds an entry read from a checkpoint or replayed from the WAL to `schema`, visible
    /// to every transaction.
    pub fn create_entry_committed(&self, schema: impl AsRef<[u8]>, entry: CatalogEntry) -> Result<()> {
        let schema = schema.as_ref();

        let schema_entry = self.schema_entry(&Transaction::system(), schema)?;
        let schema_entry = schema_entry.as_schema().ok_or_else(missing_main)?;
        Self::set_for(schema_entry, entry.catalog_type()).create_committed(entry)
    }

    /// `CREATE SCHEMA` (DuckDB's `DuckCatalog::CreateSchema`).
    pub fn create_schema(
        &self,
        transaction: &Transaction,
        name: impl AsRef<[u8]>,
        on_conflict: OnCreateConflict,
    ) -> Result<()> {
        let name = name.as_ref();

        let entry = self.new_entry(name, EntryKind::Schema(SchemaEntry::new(&self.name, name)));
        if self.schemas.create(transaction, entry)? {
            return Ok(());
        }
        match on_conflict {
            OnCreateConflict::IgnoreOnConflict => Ok(()),
            OnCreateConflict::ReplaceOnConflict => {
                // Dependent entries make the drop fail (no CASCADE).
                self.drop_schema(transaction, name, false, false)?;
                let entry = self.new_entry(name, EntryKind::Schema(SchemaEntry::new(&self.name, name)));
                if !self.schemas.create(transaction, entry)? {
                    return Err(Error::internal(
                        "Failed to create schema entry in CREATE_OR_REPLACE",
                    ));
                }
                Ok(())
            }
            _ => Err(entry_already_exists(CatalogType::Schema, name)),
        }
    }

    /// Adds `entry` to `schema` (DuckDB's `DuckSchemaEntry::AddEntryInternal`). Returns
    /// whether it was created (`IF NOT EXISTS` on an existing entry creates nothing).
    pub fn create_entry(
        &self,
        transaction: &Transaction,
        schema: impl AsRef<[u8]>,
        entry: CatalogEntry,
        on_conflict: OnCreateConflict,
    ) -> Result<bool> {
        let schema = schema.as_ref();

        let created = self.create_entry_in_schema(transaction, schema, entry, on_conflict)?;
        if created {
            // The entry depends on its schema, which must still exist when it commits.
            transaction.push_catalog_undo(Box::new(crate::transaction::CommitCheck::SubjectExists {
                name: schema.into(),
                set: Arc::clone(&self.schemas),
            }));
        }
        Ok(created)
    }

    fn create_entry_in_schema(
        &self,
        transaction: &Transaction,
        schema: impl AsRef<[u8]>,
        entry: CatalogEntry,
        on_conflict: OnCreateConflict,
    ) -> Result<bool> {
        let schema = schema.as_ref();

        let schema_entry = self.schema_entry(transaction, schema)?;
        let schema_entry = schema_entry.as_schema().ok_or_else(missing_main)?;
        let set = Self::set_for(schema_entry, entry.catalog_type());
        let name = entry.name().to_owned();
        let ty = entry.catalog_type();
        if ty == CatalogType::Type && is_builtin_type(schema, &name) {
            // The builtin types of `main` exist in every catalog (DuckDB's
            // `DefaultTypeGenerator`) and are internal entries.
            let _ = set.get(transaction, &name);
            return match on_conflict {
                OnCreateConflict::IgnoreOnConflict => Ok(false),
                OnCreateConflict::ReplaceOnConflict => Err(internal_entry_error(&name)),
                _ => Err(entry_already_exists(CatalogType::Type, &name)),
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
                return Err(Error::catalog(roastedduck_common::byte_format!(
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
        Err(entry_already_exists(existing, &name))
    }

    /// Replaces the entry `name` of type `ty` in `schema` by `entry` (DuckDB's `Alter`).
    pub fn replace_entry(
        &self,
        transaction: &Transaction,
        schema: impl AsRef<[u8]>,
        ty: CatalogType,
        name: impl AsRef<[u8]>,
        entry: CatalogEntry,
    ) -> Result<()> {
        let name = name.as_ref();
        let schema = schema.as_ref();

        let schema_entry = self.schema_entry(transaction, schema)?;
        let schema_entry = schema_entry.as_schema().ok_or_else(missing_main)?;
        let set = Self::set_for(schema_entry, ty);
        if !set.alter(transaction, name, entry)? {
            return Err(missing_entry(ty, name));
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
        schema: impl AsRef<[u8]>,
        ty: CatalogType,
        name: impl AsRef<[u8]>,
        entry: CatalogEntry,
        hook: StorageHook,
        info: Option<roastedduck_ast::AlterInfo>,
    ) -> Result<()> {
        let name = name.as_ref();
        let schema = schema.as_ref();

        let schema_entry = self.schema_entry(transaction, schema)?;
        let schema_entry = schema_entry.as_schema().ok_or_else(missing_main)?;
        let set = Self::set_for(schema_entry, ty);
        if !set.alter_with_hook(transaction, name, entry, hook, info)? {
            return Err(missing_entry(ty, name));
        }
        Ok(())
    }

    /// Renames the entry `name` of type `ty` in `schema`; `entry` is the renamed entry
    /// (`CatalogSet::AlterEntry` with `RENAME`), `info` the logged ALTER.
    #[allow(clippy::too_many_arguments, reason = "mirrors CatalogSet::AlterEntry")]
    pub fn rename_entry(
        &self,
        transaction: &Transaction,
        schema: impl AsRef<[u8]>,
        ty: CatalogType,
        name: impl AsRef<[u8]>,
        entry: CatalogEntry,
        hook: StorageHook,
        info: Option<roastedduck_ast::AlterInfo>,
    ) -> Result<()> {
        let name = name.as_ref();
        let schema = schema.as_ref();

        let schema_entry = self.schema_entry(transaction, schema)?;
        let schema_entry = schema_entry.as_schema().ok_or_else(missing_main)?;
        let set = Self::set_for(schema_entry, ty);
        // A rename that only changes the case replaces the entry in place
        // (`CatalogSet::AlterEntry` skips `RenameEntryInternal`).
        if entry.name().eq_ignore_ascii_case(name) {
            if !set.alter_with_hook(transaction, name, entry, hook, info)? {
                return Err(missing_entry(ty, name));
            }
            return Ok(());
        }
        if set.get(transaction, entry.name()).is_some() {
            return Err(Error::catalog(roastedduck_common::byte_format!(
                "Could not rename \"{name}\" to \"{}\": another entry with this name already exists!",
                entry.name()
            )));
        }
        let previous = set.get(transaction, name);
        let renamed = entry.info(schema);
        if !set.rename(transaction, name, entry, hook, info)? {
            return Err(missing_entry(ty, name));
        }
        // The entries it owns follow the new name (`DependencyManager::AlterObject`
        // re-creates the ownership links).
        if let Some(previous) = previous {
            for (owned_schema, owned) in self.owned_entries(transaction, schema, &previous) {
                let mut copy = owned
                    .try_copy_with_oid(self.name.next_oid())
                    .ok_or_else(|| Error::internal("an owned entry that cannot be copied"))?;
                copy.owned_by = Some(renamed.clone());
                self.alter_entry(
                    transaction,
                    &owned_schema,
                    owned.catalog_type(),
                    owned.name(),
                    copy,
                    StorageHook::None,
                    None,
                )?;
            }
        }
        Ok(())
    }

    /// The index entries of the table `table` in `schema` (the entries depending on it).
    pub fn table_indexes(
        &self,
        transaction: &Transaction,
        schema: impl AsRef<[u8]>,
        table: impl AsRef<[u8]>,
    ) -> Vec<Arc<CatalogEntry>> {
        let schema = schema.as_ref();
        let table = table.as_ref();

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
        schema: impl AsRef<[u8]>,
        name: impl AsRef<[u8]>,
        cascade: bool,
        if_exists: bool,
    ) -> Result<()> {
        self.drop_entry_with_internal(transaction, ty, schema, name, cascade, if_exists, false)
    }

    /// Client API removal of an internal entry (`DropInfo::allow_drop_internal`).
    /// SQL DROP continues to reject internal objects through `drop_entry`.
    #[allow(clippy::too_many_arguments)]
    pub fn drop_entry_with_internal(
        &self,
        transaction: &Transaction,
        ty: CatalogType,
        schema: impl AsRef<[u8]>,
        name: impl AsRef<[u8]>,
        cascade: bool,
        if_exists: bool,
        allow_internal: bool,
    ) -> Result<()> {
        let name = name.as_ref();
        let schema = schema.as_ref();

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
            let _ = set.get(transaction, name);
            return Err(internal_entry_error(name));
        }
        let Some(existing) = set.get(transaction, name) else {
            if if_exists {
                return Ok(());
            }
            return Err(missing_entry(ty, name));
        };
        if existing.catalog_type() != ty {
            return Err(Error::catalog(roastedduck_common::byte_format!(
                "Existing object {name} is of type {}, trying to drop type {}",
                existing.catalog_type().display_name(),
                ty.display_name()
            )));
        }
        if existing.internal && !allow_internal {
            return Err(Error::catalog(roastedduck_common::byte_format!(
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
        set.drop_entry(transaction, name, allow_internal)?;
        // The entries it owns go with it (`DependencyManager::CheckDropDependencies`).
        let schema_name = schema.to_owned();
        for (owned_schema, owned) in self.owned_entries(transaction, &schema_name, &existing) {
            self.drop_entry(
                transaction,
                owned.catalog_type(),
                &owned_schema,
                owned.name(),
                cascade,
                true,
            )?;
        }
        // `CatalogSet::CommitDrop`: entries other transactions made depend on it or
        // owned by it since this one started make the drop fail to commit.
        transaction.push_catalog_undo(Box::new(crate::transaction::CommitCheck::EntryDropped {
            entry: existing.info(&schema_name),
            sets: self.dependent_sets(transaction),
        }));
        Ok(())
    }

    /// The sets of this catalog whose entries can depend on others or be owned.
    fn dependent_sets(&self, transaction: &Transaction) -> Vec<Arc<CatalogSet>> {
        let mut result = Vec::new();
        for schema_entry in self.schemas.scan(transaction) {
            let Some(contents) = schema_entry.as_schema() else {
                continue;
            };
            result.extend(
                [
                    &contents.tables,
                    &contents.functions,
                    &contents.table_functions,
                    &contents.sequences,
                    &contents.types,
                    &contents.indexes,
                ]
                .into_iter()
                .map(Arc::clone),
            );
        }
        result
    }

    /// The table or sequence that owns `entry`, if it is visible to `transaction`.
    fn owner(&self, transaction: &Transaction, entry: &CatalogEntry) -> Option<Arc<CatalogEntry>> {
        let owner = entry.owned_by.as_ref()?;
        let schema_entry = self.schema(transaction, &owner.schema)?;
        let contents = schema_entry.as_schema()?;
        [&contents.tables, &contents.sequences]
            .into_iter()
            .filter_map(|set| set.get(transaction, &owner.name))
            .find(|candidate| candidate.is_described_by(schema_entry.name(), owner))
    }

    /// The tables and sequences of this catalog that `entry` of `schema` owns, with
    /// their schemas.
    fn owned_entries(
        &self,
        transaction: &Transaction,
        schema: impl AsRef<[u8]>,
        entry: &CatalogEntry,
    ) -> Vec<(ByteString, Arc<CatalogEntry>)> {
        let schema = schema.as_ref();

        let mut result = Vec::new();
        for schema_entry in self.schemas.scan(transaction) {
            let Some(contents) = schema_entry.as_schema() else {
                continue;
            };
            for set in [&contents.tables, &contents.sequences] {
                for candidate in set.scan(transaction) {
                    if candidate
                        .owned_by
                        .as_ref()
                        .is_some_and(|owner| entry.is_described_by(schema, owner))
                    {
                        result.push((schema_entry.name().to_owned(), candidate));
                    }
                }
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
        schema: impl AsRef<[u8]>,
        entry: &CatalogEntry,
        (owner_schema, owner_name): (impl AsRef<[u8]>, impl AsRef<[u8]>),
        info: Option<roastedduck_ast::AlterInfo>,
    ) -> Result<()> {
        let schema = schema.as_ref();
        let owner_schema = owner_schema.as_ref();
        let owner_name = owner_name.as_ref();

        let missing = || {
            Error::catalog(roastedduck_common::byte_format!(
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
        let dependency_error =
            |message: ByteString| Error::new_bytes(roastedduck_common::ErrorKind::Dependency, message);
        if let Some(other) = self.owner(transaction, &owner) {
            return Err(dependency_error(roastedduck_common::byte_format!(
                "{} can not become the owner, it is already owned by {}",
                owner.name(),
                other.name()
            )));
        }
        if let Some((_, owned)) = self.owned_entries(transaction, schema, entry).first() {
            return Err(dependency_error(roastedduck_common::byte_format!(
                "{} already owns {}. Cannot have circular dependencies",
                entry.name(),
                owned.name()
            )));
        }
        if let Some(other) = self.owner(transaction, entry) {
            if Arc::ptr_eq(&other, &owner) {
                return Ok(());
            }
            return Err(dependency_error(roastedduck_common::byte_format!(
                "{} is already owned by {}",
                entry.name(),
                other.name()
            )));
        }
        let mut owned = entry
            .try_copy_with_oid(self.name.next_oid())
            .ok_or_else(|| Error::internal("OWNED BY an entry that cannot be copied"))?;
        owned.owned_by = Some(owner.info(&owner_schema));
        self.alter_entry(
            transaction,
            schema,
            entry.catalog_type(),
            entry.name(),
            owned,
            StorageHook::None,
            info,
        )?;
        // `VerifyExistence`: the owner must still exist when this commits.
        let owner_set = if owner.catalog_type() == CatalogType::Table {
            &contents.tables
        } else {
            &contents.sequences
        };
        transaction.push_catalog_undo(Box::new(crate::transaction::CommitCheck::SubjectExists {
            name: owner.name().to_owned(),
            set: Arc::clone(owner_set),
        }));
        Ok(())
    }

    /// `COMMENT ON`: a copy of `entry` with `comment` replaces it (the `SET_COMMENT` case
    /// of `CatalogSet::AlterEntry`). `info` is the logged ALTER.
    pub fn set_comment(
        &self,
        transaction: &Transaction,
        schema: impl AsRef<[u8]>,
        entry: &CatalogEntry,
        comment: Value,
        info: Option<roastedduck_ast::AlterInfo>,
    ) -> Result<()> {
        let schema = schema.as_ref();

        let mut copy = entry
            .try_copy_with_oid(self.name.next_oid())
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
        schema: impl AsRef<[u8]>,
        entry: &CatalogEntry,
        (column, comment): (impl AsRef<[u8]>, Value),
        info: Option<roastedduck_ast::AlterInfo>,
    ) -> Result<()> {
        let schema = schema.as_ref();
        let column = column.as_ref();

        let mut copy = entry
            .try_copy_with_oid(self.name.next_oid())
            .ok_or_else(|| Error::internal("COMMENT ON COLUMN of an entry that cannot be copied"))?;
        match &mut copy.kind {
            EntryKind::Table(table) => {
                let Some(target) = table
                    .columns
                    .iter_mut()
                    .find(|candidate| candidate.name.eq_ignore_ascii_case(column))
                else {
                    return Err(Error::binder(roastedduck_common::byte_format!(
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
                        .is_some_and(|name| name.as_bytes() == column)
                });
                if !exists {
                    return Err(Error::binder(roastedduck_common::byte_format!(
                        "View \"{}\" does not have a column with name \"{column}\"",
                        entry.name()
                    )));
                }
                match view.column_comments.get_mut(column) {
                    Some(existing) => *existing = comment,
                    None => view.column_comments.push(column.into(), comment),
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
        schema: impl AsRef<[u8]>,
        entry: &CatalogEntry,
    ) -> Vec<(ByteString, Arc<CatalogEntry>)> {
        let schema = schema.as_ref();

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
                        || entry
                            .owned_by
                            .as_ref()
                            .is_some_and(|owner| candidate.is_described_by(schema_entry.name(), owner))
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
        schema: impl AsRef<[u8]>,
        entry: &CatalogEntry,
        cascade: bool,
    ) -> Result<()> {
        let schema = schema.as_ref();

        let dependents = self.dependents(transaction, schema, entry);
        if dependents.is_empty() {
            return Ok(());
        }
        if !cascade {
            let mut message = roastedduck_common::byte_format!(
                "Cannot drop entry \"{}\" because there are entries that depend on it.\n",
                entry.name()
            );
            self.collect_dependents(transaction, &dependents, entry, &mut message);
            message.push_bytes("Use DROP...CASCADE to drop all dependents.");
            return Err(Error::new_bytes(
                roastedduck_common::ErrorKind::Dependency,
                message,
            ));
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
        dependents: &[(ByteString, Arc<CatalogEntry>)],
        subject: &CatalogEntry,
        message: &mut ByteString,
    ) {
        for (schema, dependent) in dependents {
            let _ = roastedduck_common::byte_write!(
                message,
                "{} depends on {}.\n",
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
        name: impl AsRef<[u8]>,
        cascade: bool,
        if_exists: bool,
    ) -> Result<()> {
        let name = name.as_ref();

        let Some(schema) = self.schema(transaction, name) else {
            if if_exists {
                return Ok(());
            }
            return Err(missing_entry(CatalogType::Schema, name));
        };
        if schema.internal {
            return Err(Error::catalog(roastedduck_common::byte_format!(
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
            let mut message = roastedduck_common::byte_format!(
                "Cannot drop entry \"{}\" because there are entries that depend on it.\n",
                schema.name()
            );
            for entry in dependents.iter().rev() {
                let _ = roastedduck_common::byte_write!(
                    message,
                    "{} \"{}\" depends on schema \"{}\".\n",
                    entry.catalog_type().display_name().to_lowercase(),
                    entry.name(),
                    schema.name()
                );
            }
            message.push_bytes("Use DROP...CASCADE to drop all dependents.");
            return Err(Error::new_bytes(
                roastedduck_common::ErrorKind::Dependency,
                message,
            ));
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
fn entry_to_string(entry: &CatalogEntry) -> ByteString {
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
    roastedduck_common::byte_format!("{kind} \"{}\"", entry.name())
}

/// Whether `name` is one of the builtin types every `main` schema holds (DuckDB's
/// `DefaultTypeGenerator::CreateDefaultEntry`).
fn is_builtin_type(schema: impl AsRef<[u8]>, name: impl AsRef<[u8]>) -> bool {
    let name = name.as_ref();
    let schema = schema.as_ref();

    schema.eq_ignore_ascii_case(DEFAULT_SCHEMA.as_bytes())
        && roastedduck_types::builtin_type_id(name).is_some()
}

/// DuckDB's error for dropping or replacing an internal entry.
fn internal_entry_error(name: impl AsRef<[u8]>) -> Error {
    let name = name.as_ref();

    Error::catalog(roastedduck_common::byte_format!(
        "Cannot drop entry \"{name}\" because it is an internal system entry"
    ))
}

fn missing_main() -> Error {
    Error::internal("the main schema is missing from the catalog")
}

/// `CatalogException::MissingEntry` of a lookup without suggestions, caught into the
/// lookup's `ErrorData` (the position of the lookup, if any, follows the subtype).
fn missing_entry(ty: CatalogType, name: impl AsRef<[u8]>) -> Error {
    let name = name.as_ref();

    Error::catalog(roastedduck_common::byte_format!(
        "{} with name {name} does not exist!",
        ty.display_name()
    ))
    .with_subtype("MISSING_ENTRY")
    .with_extra_info_slot("position")
    .with_extra_info("name", name)
    .with_extra_info("type", ty.display_name())
    .with_extra_info_round_trip()
}

/// `CatalogException::EntryAlreadyExists`, thrown while the statement executes, so that
/// it is caught into an `ErrorData` and thrown again before the client sees it.
fn entry_already_exists(ty: CatalogType, name: impl AsRef<[u8]>) -> Error {
    let name = name.as_ref();

    Error::catalog(roastedduck_common::byte_format!(
        "{} with name \"{name}\" already exists!",
        ty.display_name()
    ))
    .with_subtype("ENTRY_ALREADY_EXISTS")
    .with_extra_info("name", name)
    .with_extra_info("type", ty.display_name())
    .with_extra_info_round_trip()
}

#[cfg(test)]
mod tests {
    use roastedduck_types::LogicalType;

    use super::*;
    use crate::{TransactionManager, TypeEntry};

    #[test]
    fn collation_entries_keep_their_transform_and_ignore_duplicate_extension_registration() {
        let catalog = Catalog::new("system").unwrap();
        let collation = |combinable| crate::CollationEntry {
            function: roastedduck_function::ScalarFunction::new(
                "test_transform",
                vec![LogicalType::VARCHAR],
                LogicalType::VARCHAR,
                |_, _, _| Ok(()),
            ),
            combinable,
            not_required_for_equality: false,
        };
        catalog
            .register_builtin_collation("builtin_test", collation(true))
            .unwrap();
        let schema = catalog.schema(&Transaction::system(), "main").unwrap();
        let schema = schema.as_schema().unwrap();
        assert!(schema.collations.get_committed("builtin_test").is_some());
        assert!(schema.functions.get_committed("test_transform").is_none());

        catalog
            .register_collation("extension_test", collation(true))
            .unwrap();
        let original = schema.collations.get_committed("extension_test").unwrap();
        let transform = schema.functions.get_committed("test_transform").unwrap();
        catalog
            .register_collation("extension_test", collation(false))
            .unwrap();
        let repeated = schema.collations.get_committed("extension_test").unwrap();
        assert!(Arc::ptr_eq(&original, &repeated));
        let EntryKind::Collation(entry) = &repeated.kind else {
            panic!("a collation must keep its catalog type");
        };
        assert!(entry.combinable);
        assert!(Arc::ptr_eq(
            &transform,
            &schema.functions.get_committed("test_transform").unwrap()
        ));
    }

    #[test]
    fn attached_identity_is_allocated_after_initialization_and_only_once() {
        let allocator = Arc::new(crate::OidAllocator::default());
        let catalog = Catalog::unregistered("attached", Arc::clone(&allocator)).unwrap();
        let provisional = catalog.oid();
        assert_eq!(provisional, 20_000);
        let schema = catalog.schema(&Transaction::system(), "main").unwrap();
        assert!(schema.oid > provisional);
        assert_eq!(catalog.oid(), provisional);
        let initialized = catalog.new_entry("loaded", EntryKind::Schema(SchemaEntry::default()));
        catalog.finalize_registration();
        assert!(catalog.oid() > initialized.oid);
        let identity = catalog.oid();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| catalog.finalize_registration());
            }
        });
        assert_eq!(catalog.oid(), identity);
        assert_eq!(allocator.next_oid(), identity + 1);
    }

    #[test]
    fn default_macro_entries_and_schemas_are_allocated_on_first_lookup() {
        let catalog = Catalog::new("system").unwrap();
        let defaults = Arc::downgrade(&catalog.default_entries);
        catalog.register_default_entry(
            "pg_catalog",
            "lazy_macro",
            EntryKind::Macro(crate::MacroEntry { macros: vec![] }),
        );
        assert!(catalog.schemas.get_committed_existing("pg_catalog").is_none());
        let marker = catalog.new_entry("marker", EntryKind::Schema(SchemaEntry::default()));
        assert_eq!(
            marker.oid, 20_002,
            "registration must consume no object IDs beyond main's chain root"
        );
        let schema = catalog.schema(&Transaction::system(), "pg_catalog").unwrap();
        assert_eq!(schema.oid, marker.oid + 1);
        let functions = &schema.as_schema().unwrap().functions;
        assert!(functions.get_committed_existing("lazy_macro").is_none());
        let entry = functions.get(&Transaction::system(), "lazy_macro").unwrap();
        assert_eq!(entry.oid, schema.oid + 1);
        assert!(Arc::ptr_eq(
            &entry,
            &functions.get(&Transaction::system(), "lazy_macro").unwrap()
        ));
        assert_eq!(functions.scan(&Transaction::system()).len(), 1);
        drop(schema);
        drop(catalog);
        assert!(
            defaults.upgrade().is_none(),
            "default factories must not retain the catalog registry"
        );
    }

    #[test]
    fn deferred_default_is_published_once_for_concurrent_lookups() {
        let catalog = Catalog::new("system").unwrap();
        catalog.register_default_entry(
            "main",
            "lazy_macro",
            EntryKind::Macro(crate::MacroEntry { macros: vec![] }),
        );
        let entries = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        catalog
                            .get_entry(&Transaction::system(), CatalogType::Macro, "main", "lazy_macro")
                            .unwrap()
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(entries.iter().all(|entry| Arc::ptr_eq(entry, &entries[0])));
        assert_eq!(
            catalog
                .new_entry("marker", EntryKind::Schema(SchemaEntry::default()))
                .oid,
            20_003
        );
    }

    #[test]
    fn unobserved_default_cannot_be_shadowed_and_survives_rollback() {
        let catalog = Catalog::new("system").unwrap();
        catalog.register_default_entry(
            "main",
            "lazy_macro",
            EntryKind::Macro(crate::MacroEntry { macros: vec![] }),
        );
        let manager = TransactionManager::default();
        let transaction = manager.begin();
        let schema = catalog.schema(&transaction, "main").unwrap();
        let functions = &schema.as_schema().unwrap().functions;
        assert!(functions.get_committed_existing("lazy_macro").is_none());
        let attempted = catalog.new_entry(
            "lazy_macro",
            EntryKind::Macro(crate::MacroEntry { macros: vec![] }),
        );
        assert!(!functions.create(&transaction, attempted).unwrap());
        let generated = functions.get(&transaction, "lazy_macro").unwrap();
        assert!(generated.internal);
        manager.rollback(&transaction);
        assert!(Arc::ptr_eq(
            &generated,
            &functions.get(&Transaction::system(), "lazy_macro").unwrap()
        ));
    }

    #[test]
    fn transaction_chain_and_successful_drop_nodes_have_instance_owned_ids() {
        let catalog = Catalog::new("system").unwrap();
        let manager = TransactionManager::default();
        let schema = catalog.schema(&Transaction::system(), "main").unwrap();
        let functions = &schema.as_schema().unwrap().functions;
        let transaction = manager.begin();
        let mut entry = catalog.new_entry(
            "user_macro",
            EntryKind::Macro(crate::MacroEntry { macros: vec![] }),
        );
        entry.temporary = true;
        let original_oid = entry.oid;
        assert!(functions.create(&transaction, entry).unwrap());
        manager.commit(&transaction).unwrap();
        let marker = catalog.new_entry("marker", EntryKind::Schema(SchemaEntry::default()));
        assert_eq!(
            marker.oid,
            original_oid + 2,
            "the actual INVALID chain node consumes one ID"
        );
        let transaction = manager.begin();
        assert!(!functions.drop_entry(&transaction, "missing", false).unwrap());
        assert!(functions.drop_entry(&transaction, "user_macro", false).unwrap());
        manager.rollback(&transaction);
        assert_eq!(
            functions.get(&Transaction::system(), "user_macro").unwrap().oid,
            original_oid
        );
        assert_eq!(
            catalog
                .new_entry("after", EntryKind::Schema(SchemaEntry::default()))
                .oid,
            marker.oid + 2,
            "only the actual DELETED node consumes an ID, including after rollback"
        );
    }

    #[test]
    fn rename_nodes_keep_snapshot_visibility_and_rollback_identity() {
        let catalog = Catalog::new("system").unwrap();
        let manager = TransactionManager::default();
        let schema = catalog.schema(&Transaction::system(), "main").unwrap();
        let set = &schema.as_schema().unwrap().functions;
        let create = manager.begin();
        let mut entry = catalog.new_entry("old", EntryKind::Macro(crate::MacroEntry { macros: vec![] }));
        entry.temporary = true;
        let oid = entry.oid;
        assert!(set.create(&create, entry).unwrap());
        manager.commit(&create).unwrap();
        let reader = manager.begin();
        let rename = manager.begin();
        let renamed = catalog.new_entry("new", EntryKind::Macro(crate::MacroEntry { macros: vec![] }));
        let allocated_before = renamed.oid;
        assert!(
            set.rename(&rename, "old", renamed, StorageHook::None, None)
                .unwrap()
        );
        assert!(set.get(&rename, "old").is_none());
        assert_eq!(set.get(&rename, "new").unwrap().oid, oid);
        assert_eq!(set.get(&reader, "old").unwrap().oid, oid);
        assert!(set.get(&reader, "new").is_none());
        assert_eq!(
            catalog
                .new_entry("marker", EntryKind::Schema(SchemaEntry::default()))
                .oid,
            allocated_before + 5,
            "rename creates three rename/delete nodes plus a new chain root"
        );
        manager.rollback(&rename);
        assert_eq!(set.get(&Transaction::system(), "old").unwrap().oid, oid);
        assert!(set.get(&Transaction::system(), "new").is_none());
        manager.rollback(&reader);
        let rename = manager.begin();
        let renamed = catalog.new_entry("new", EntryKind::Macro(crate::MacroEntry { macros: vec![] }));
        assert!(
            set.rename(&rename, "old", renamed, StorageHook::None, None)
                .unwrap()
        );
        manager.commit(&rename).unwrap();
        assert!(set.get(&Transaction::system(), "old").is_none());
        assert_eq!(set.get(&Transaction::system(), "new").unwrap().oid, oid);
    }

    #[test]
    fn object_ids_are_owned_by_the_instance_and_shared_with_attached_catalogs() {
        let left = Catalog::with_oid_allocator("system", Arc::default()).unwrap();
        let right = Catalog::with_oid_allocator("system", Arc::default()).unwrap();
        assert_eq!(
            left.new_entry("a", EntryKind::Schema(SchemaEntry::default())).oid,
            20_002
        );
        assert_eq!(
            right
                .new_entry("a", EntryKind::Schema(SchemaEntry::default()))
                .oid,
            20_002
        );
        let attached = Catalog::with_oid_allocator("attached", left.oid_allocator()).unwrap();
        let temp = Catalog::with_oid_allocator("temp", left.oid_allocator()).unwrap();
        let ids = [
            attached.oid(),
            attached
                .schema(&Transaction::system(), DEFAULT_SCHEMA)
                .unwrap()
                .oid,
            temp.oid(),
            temp.schema(&Transaction::system(), DEFAULT_SCHEMA).unwrap().oid,
            left.new_entry("b", EntryKind::Schema(SchemaEntry::default())).oid,
        ];
        assert_eq!(ids, [20_003, 20_004, 20_006, 20_007, 20_009]);
        assert_eq!(
            right
                .new_entry("b", EntryKind::Schema(SchemaEntry::default()))
                .oid,
            20_003
        );
    }

    #[test]
    fn lazy_default_type_generation_uses_its_catalogs_allocator() {
        let left = Catalog::new("system").unwrap();
        let right = Catalog::new("system").unwrap();
        let a = left
            .get_entry(
                &Transaction::system(),
                CatalogType::Type,
                DEFAULT_SCHEMA,
                "INTEGER",
            )
            .unwrap();
        let b = right
            .get_entry(
                &Transaction::system(),
                CatalogType::Type,
                DEFAULT_SCHEMA,
                "INTEGER",
            )
            .unwrap();
        assert_eq!(a.oid, b.oid);
        let next = left.new_entry("next", EntryKind::Schema(SchemaEntry::default()));
        assert!(next.oid > a.oid);
    }

    fn type_entry(name: impl AsRef<[u8]>) -> CatalogEntry {
        let name = name.as_ref();

        CatalogEntry::new(
            name,
            EntryKind::Type(TypeEntry {
                user_type: LogicalType::INTEGER,
            }),
        )
    }

    #[test]
    fn client_function_removal_is_opt_in_and_transactional() {
        let catalog = Catalog::new("system").unwrap();
        let manager = TransactionManager::default();
        let transaction = manager.begin();
        let mut entry = CatalogEntry::new(
            "python_udf",
            EntryKind::ScalarFunction(roastedduck_function::ScalarFunctionSet::new("python_udf")),
        );
        entry.internal = true;
        catalog
            .create_entry(&transaction, "main", entry, OnCreateConflict::ErrorOnConflict)
            .unwrap();
        manager.commit(&transaction).unwrap();

        let transaction = manager.begin();
        let error = catalog
            .drop_entry(
                &transaction,
                CatalogType::ScalarFunction,
                "main",
                "python_udf",
                false,
                false,
            )
            .unwrap_err();
        assert!(error.to_string().contains("internal system entry"));
        catalog
            .drop_entry_with_internal(
                &transaction,
                CatalogType::ScalarFunction,
                "main",
                "python_udf",
                false,
                false,
                true,
            )
            .unwrap();
        assert!(
            catalog
                .get_entry(&transaction, CatalogType::ScalarFunction, "main", "python_udf")
                .is_none()
        );
        manager.rollback(&transaction);

        let transaction = manager.begin();
        assert!(
            catalog
                .get_entry(&transaction, CatalogType::ScalarFunction, "main", "python_udf")
                .is_some()
        );
        catalog
            .drop_entry_with_internal(
                &transaction,
                CatalogType::ScalarFunction,
                "main",
                "python_udf",
                false,
                false,
                true,
            )
            .unwrap();
        manager.commit(&transaction).unwrap();
        let transaction = manager.begin();
        assert!(
            catalog
                .get_entry(&transaction, CatalogType::ScalarFunction, "main", "python_udf")
                .is_none()
        );
        assert!(
            catalog
                .drop_entry_with_internal(
                    &transaction,
                    CatalogType::Type,
                    "main",
                    "INTEGER",
                    false,
                    false,
                    true
                )
                .is_err()
        );
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
