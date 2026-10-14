use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::ScalarFunctionSet;

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
        let schema = self
            .schema(&Transaction::system(), DEFAULT_SCHEMA)
            .ok_or_else(missing_main)?;
        let schema = schema.as_schema().ok_or_else(missing_main)?;
        let mut entry = CatalogEntry::new(set.name.to_string(), EntryKind::ScalarFunction(set));
        entry.internal = true;
        schema.functions.create_committed(entry)
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
}

fn missing_main() -> Error {
    Error::internal("the main schema is missing from the catalog")
}
