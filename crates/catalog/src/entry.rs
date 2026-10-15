use std::sync::Arc;

use rustdb_ast::{ParsedExpression, SelectStatement};
use rustdb_function::{AggregateFunctionSet, ScalarFunctionSet, TableFunctionSet};
use rustdb_types::LogicalType;

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

impl CatalogType {
    /// The name DuckDB uses in messages (`CatalogTypeToString`).
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Schema => "Schema",
            Self::Table => "Table",
            Self::View => "View",
            Self::Index => "Index",
            Self::Sequence => "Sequence",
            Self::Type => "Type",
            Self::Collation => "Collation",
            Self::ScalarFunction => "Scalar Function",
            Self::AggregateFunction => "Aggregate Function",
            Self::TableFunction => "Table Function",
            Self::PragmaFunction => "Pragma Function",
            Self::CopyFunction => "Copy Function",
            Self::Macro => "Macro Function",
            Self::TableMacro => "Table Macro Function",
        }
    }
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

/// One column of a table (DuckDB's `ColumnDefinition`, catalog part).
#[derive(Clone, Debug)]
pub struct TableColumn {
    pub name: String,
    pub logical_type: LogicalType,
    /// `DEFAULT expr`.
    pub default_value: Option<ParsedExpression>,
    /// `GENERATED ALWAYS AS (expr)`: the column is computed, not stored.
    pub generated: Option<ParsedExpression>,
}

/// A table's definition (DuckDB's `TableCatalogEntry` / `DuckTableEntry`). Its data lives
/// in `storage`.
#[derive(Clone, Debug, Default)]
pub struct TableEntry {
    /// The columns in logical order (generated columns included).
    pub columns: Vec<TableColumn>,
    /// Columns that may not be NULL, by logical index.
    pub not_null_columns: Vec<usize>,
    /// The constraints as written (with column indexes resolved), DuckDB's
    /// `TableCatalogEntry::constraints`.
    pub constraints: Vec<rustdb_ast::Constraint>,
    /// The table's data (`DuckTableEntry::storage`); `None` for tables of other catalogs.
    pub storage: Option<Arc<rustdb_storage::DataTable>>,
}

impl TableEntry {
    /// The definition of a `CREATE TABLE` (without storage).
    pub fn from_create_info(info: &rustdb_ast::CreateTableInfo) -> Self {
        let columns = info
            .columns
            .columns
            .iter()
            .map(|column| {
                let generated = column.category == rustdb_ast::TableColumnType::Generated;
                let expression = column.expression.as_deref().cloned();
                TableColumn {
                    name: column.name.clone(),
                    logical_type: column.logical_type.clone(),
                    default_value: if generated { None } else { expression.clone() },
                    generated: if generated { expression } else { None },
                }
            })
            .collect();
        let not_null_columns = info
            .constraints
            .iter()
            .filter_map(|constraint| match constraint {
                rustdb_ast::Constraint::NotNull(not_null) => Some(not_null.index.0 as usize),
                _ => None,
            })
            .collect();
        Self {
            columns,
            not_null_columns,
            constraints: info.constraints.clone(),
            storage: None,
        }
    }

    /// The column called `name` (case-insensitive), with its index.
    pub fn column(&self, name: &str) -> Option<(usize, &TableColumn)> {
        self.columns
            .iter()
            .enumerate()
            .find(|(_, column)| column.name.eq_ignore_ascii_case(name))
    }

    /// The stored (non-generated) columns as `(physical index, logical index, column)`
    /// (DuckDB's `ColumnList::Physical`).
    pub fn physical_columns(&self) -> impl Iterator<Item = (usize, usize, &TableColumn)> {
        self.columns
            .iter()
            .enumerate()
            .filter(|(_, column)| column.generated.is_none())
            .enumerate()
            .map(|(physical, (logical, column))| (physical, logical, column))
    }

    /// The physical index of logical column `logical`, if it is stored.
    pub fn logical_to_physical(&self, logical: usize) -> Option<usize> {
        self.physical_columns()
            .find(|&(_, l, _)| l == logical)
            .map(|(physical, _, _)| physical)
    }

    /// The logical index of physical column `physical`.
    pub fn physical_to_logical(&self, physical: usize) -> Option<usize> {
        self.physical_columns()
            .find(|&(p, _, _)| p == physical)
            .map(|(_, logical, _)| logical)
    }

    /// The types of the stored columns (DuckDB's `GetTypes`).
    pub fn physical_types(&self) -> Vec<LogicalType> {
        self.physical_columns()
            .map(|(_, _, column)| column.logical_type.clone())
            .collect()
    }

    /// Whether any column is generated.
    pub fn has_generated_columns(&self) -> bool {
        self.columns.iter().any(|column| column.generated.is_some())
    }
}

/// A view: a stored query (DuckDB's `ViewCatalogEntry`).
#[derive(Clone, Debug)]
pub struct ViewEntry {
    pub query: SelectStatement,
    /// Column names given in `CREATE VIEW v(a, b)`.
    pub aliases: Vec<String>,
    /// The result types and names of the query when the view was created.
    pub types: Vec<LogicalType>,
    pub names: Vec<String>,
}

/// Type-specific contents of a catalog entry.
#[derive(Debug)]
pub enum EntryKind {
    Schema(SchemaEntry),
    ScalarFunction(ScalarFunctionSet),
    AggregateFunction(AggregateFunctionSet),
    TableFunction(TableFunctionSet),
    Table(TableEntry),
    View(ViewEntry),
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
            EntryKind::AggregateFunction(_) => CatalogType::AggregateFunction,
            EntryKind::TableFunction(_) => CatalogType::TableFunction,
            EntryKind::Table(_) => CatalogType::Table,
            EntryKind::View(_) => CatalogType::View,
        }
    }

    pub fn as_schema(&self) -> Option<&SchemaEntry> {
        match &self.kind {
            EntryKind::Schema(schema) => Some(schema),
            _ => None,
        }
    }
}
