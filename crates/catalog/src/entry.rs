use std::sync::Arc;

use rustdb_function::ScalarFunctionSet;

use crate::CatalogSet;

/// The kind of a catalog entry (DuckDB's `CatalogType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CatalogType {
    Schema,
    Table,
    View,
    Index,
    Sequence,
    Type,
    Collation,
    ScalarFunction,
    AggregateFunction,
    TableFunction,
    PragmaFunction,
    CopyFunction,
    Macro,
    TableMacro,
}

/// A schema and the sets holding its objects (DuckDB's `DuckSchemaEntry`).
#[derive(Debug, Default)]
pub struct SchemaEntry {
    /// Tables and views share one namespace.
    pub tables: Arc<CatalogSet>,
    /// Scalar/aggregate functions and macros share one namespace.
    pub functions: Arc<CatalogSet>,
    pub table_functions: Arc<CatalogSet>,
    pub pragma_functions: Arc<CatalogSet>,
    pub copy_functions: Arc<CatalogSet>,
    pub sequences: Arc<CatalogSet>,
    pub types: Arc<CatalogSet>,
    pub collations: Arc<CatalogSet>,
    pub indexes: Arc<CatalogSet>,
}

/// Type-specific contents of a catalog entry.
#[derive(Debug)]
pub enum EntryKind {
    Schema(SchemaEntry),
    ScalarFunction(ScalarFunctionSet),
}

/// A named object in the catalog.
#[derive(Debug)]
pub struct CatalogEntry {
    name: String,
    /// Built-in objects that users cannot drop.
    pub internal: bool,
    pub temporary: bool,
    pub kind: EntryKind,
}

impl CatalogEntry {
    pub fn new(name: impl Into<String>, kind: EntryKind) -> Self {
        Self {
            name: name.into(),
            internal: false,
            temporary: false,
            kind,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn catalog_type(&self) -> CatalogType {
        match self.kind {
            EntryKind::Schema(_) => CatalogType::Schema,
            EntryKind::ScalarFunction(_) => CatalogType::ScalarFunction,
        }
    }

    pub fn as_schema(&self) -> Option<&SchemaEntry> {
        match &self.kind {
            EntryKind::Schema(schema) => Some(schema),
            EntryKind::ScalarFunction(_) => None,
        }
    }
}
