use std::fmt::Write as _;
use std::sync::Arc;

use rustdb_ast::OnCreateConflict;
use rustdb_common::{Error, Result};
use rustdb_function::{AggregateFunctionSet, ScalarFunctionSet, TableFunctionSet};

use crate::{CatalogEntry, CatalogSet, CatalogType, EntryKind, SchemaEntry, Transaction};

/// The schema new objects go to when none is given.
pub const DEFAULT_SCHEMA: &str = "main";

/// One attached database's catalog (DuckDB's `DuckCatalog`).
#[derive(Debug)]
pub struct Catalog {
    name: String,
    schemas: Arc<CatalogSet>,
}

impl Catalog {
    /// A catalog containing the empty `main` schema.
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let catalog = Self {
            name: name.into(),
            schemas: Arc::default(),
        };
        let mut main = CatalogEntry::new(DEFAULT_SCHEMA, EntryKind::Schema(SchemaEntry::default()));
        main.internal = true;
        catalog.schemas.create_committed(main)?;
        Ok(catalog)
    }

    pub fn name(&self) -> &str {
        &self.name
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

    /// `CREATE SCHEMA` (DuckDB's `DuckCatalog::CreateSchema`).
    pub fn create_schema(
        &self,
        transaction: &Transaction,
        name: &str,
        on_conflict: OnCreateConflict,
    ) -> Result<()> {
        let entry = CatalogEntry::new(name, EntryKind::Schema(SchemaEntry::default()));
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
