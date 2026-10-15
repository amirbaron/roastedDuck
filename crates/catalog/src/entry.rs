use std::sync::Arc;

use rustdb_ast::{MacroFunction, ParsedExpression, SelectStatement};
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

impl SchemaEntry {
    /// The empty schema `schema` of catalog `catalog`.
    pub fn new(catalog: &str, schema: &str) -> Self {
        let set = || Arc::new(CatalogSet::new(catalog, schema));
        Self {
            tables: set(),
            functions: set(),
            table_functions: set(),
            pragma_functions: set(),
            copy_functions: set(),
            sequences: set(),
            types: set(),
            collations: set(),
            indexes: set(),
        }
    }
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

/// An index (DuckDB's `DuckIndexEntry`): its definition and the storage of the table
/// holding it.
#[derive(Clone, Debug)]
pub struct IndexEntry {
    /// The `CREATE INDEX` definition; `column_ids` are physical column indexes.
    pub info: rustdb_ast::CreateIndexInfo,
    /// The schema of the indexed table.
    pub schema: String,
    /// The indexed table's storage info (`IndexDataTableInfo`).
    pub storage: Option<Arc<rustdb_storage::DataTableInfo>>,
}

/// The state of a sequence (DuckDB's `SequenceData`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceData {
    /// How many values were taken.
    pub usage_count: u64,
    /// The next value.
    pub counter: i64,
    /// The value `currval` returns.
    pub last_value: i64,
    pub increment: i64,
    pub start_value: i64,
    pub min_value: i64,
    pub max_value: i64,
    pub cycle: bool,
}

/// A sequence (DuckDB's `SequenceCatalogEntry`). Its state is shared by every version of
/// the entry and is not transactional.
#[derive(Clone, Debug)]
pub struct SequenceEntry {
    pub data: Arc<std::sync::Mutex<SequenceData>>,
}

impl SequenceEntry {
    pub fn new(info: &rustdb_ast::CreateSequenceInfo) -> Self {
        Self {
            data: Arc::new(std::sync::Mutex::new(SequenceData {
                usage_count: info.usage_count,
                counter: info.start_value,
                last_value: info.start_value,
                increment: info.increment,
                start_value: info.start_value,
                min_value: info.min_value,
                max_value: info.max_value,
                cycle: info.cycle,
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SequenceData> {
        self.data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A copy of the state.
    pub fn data(&self) -> SequenceData {
        *self.lock()
    }

    /// `SequenceCatalogEntry::NextValue` of the sequence called `name`.
    pub fn next_value(&self, name: &str) -> rustdb_common::Result<i64> {
        let mut data = self.lock();
        let result = data.counter;
        let (next, overflow) = data.counter.overflowing_add(data.increment);
        data.counter = next;
        if data.cycle {
            if overflow {
                data.counter = if data.increment < 0 {
                    data.max_value
                } else {
                    data.min_value
                };
            } else if data.counter < data.min_value {
                data.counter = data.max_value;
            } else if data.counter > data.max_value {
                data.counter = data.min_value;
            }
        } else {
            if result < data.min_value || (overflow && data.increment < 0) {
                return Err(sequence_error(format!(
                    "nextval: reached minimum value of sequence \"{name}\" ({})",
                    data.min_value
                )));
            }
            if result > data.max_value || overflow {
                return Err(sequence_error(format!(
                    "nextval: reached maximum value of sequence \"{name}\" ({})",
                    data.max_value
                )));
            }
        }
        data.last_value = result;
        data.usage_count += 1;
        Ok(result)
    }

    /// Restores the state logged in the WAL, unless the sequence is further along
    /// already (`SequenceCatalogEntry::ReplayValue`).
    pub fn replay_value(&self, usage_count: u64, counter: i64) {
        let mut data = self.lock();
        if usage_count > data.usage_count {
            data.usage_count = usage_count;
            data.counter = counter;
        }
    }

    /// An identity shared by every version of the sequence while it exists.
    pub fn key(&self) -> usize {
        Arc::as_ptr(&self.data) as usize
    }

    /// `SequenceCatalogEntry::CurrentValue`.
    pub fn current_value(&self) -> rustdb_common::Result<i64> {
        let data = self.lock();
        if data.usage_count == 0 {
            return Err(sequence_error(
                "currval: sequence is not yet defined in this session".to_owned(),
            ));
        }
        Ok(data.last_value)
    }
}

fn sequence_error(message: String) -> rustdb_common::Error {
    rustdb_common::Error::new(rustdb_common::ErrorKind::Sequence, message)
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

/// A user-defined type (DuckDB's `TypeCatalogEntry`): `CREATE TYPE name AS ...`.
#[derive(Clone, Debug)]
pub struct TypeEntry {
    /// The type the name stands for, fully bound.
    pub user_type: LogicalType,
}

/// A macro (DuckDB's `MacroCatalogEntry`, the base of `ScalarMacroCatalogEntry` and
/// `TableMacroCatalogEntry`): its overloads.
#[derive(Clone, Debug)]
pub struct MacroEntry {
    /// The overloads, in definition order; never empty.
    pub macros: Vec<MacroFunction>,
}

/// Type-specific contents of a catalog entry.
#[derive(Debug)]
pub enum EntryKind {
    Schema(SchemaEntry),
    ScalarFunction(ScalarFunctionSet),
    AggregateFunction(AggregateFunctionSet),
    TableFunction(TableFunctionSet),
    /// A COPY file format (`CopyFunctionCatalogEntry`).
    CopyFunction(rustdb_function::CopyFunction),
    Table(TableEntry),
    View(ViewEntry),
    Index(IndexEntry),
    Sequence(SequenceEntry),
    /// `CREATE TYPE`.
    Type(TypeEntry),
    /// `CREATE MACRO ... AS expr` (`ScalarMacroCatalogEntry`).
    Macro(MacroEntry),
    /// `CREATE MACRO ... AS TABLE ...` (`TableMacroCatalogEntry`).
    TableMacro(MacroEntry),
}

/// The next object id handed out (DuckDB's `DatabaseManager::NextOid`; ids below
/// `FIRST_OID` belong to built-in entries).
static NEXT_OID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(FIRST_OID);

/// The first object id of entries created after start-up.
const FIRST_OID: u64 = 20_000;

/// A fresh object id (`DatabaseManager::NextOid`).
pub fn next_oid() -> u64 {
    NEXT_OID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A named object in the catalog.
#[derive(Debug)]
pub struct CatalogEntry {
    name: String,
    /// The object id (`CatalogEntry::oid`).
    pub oid: u64,
    /// Built-in objects that users cannot drop.
    pub internal: bool,
    pub temporary: bool,
    pub kind: EntryKind,
}

impl CatalogEntry {
    pub fn new(name: impl Into<String>, kind: EntryKind) -> Self {
        Self {
            name: name.into(),
            oid: next_oid(),
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
            EntryKind::CopyFunction(_) => CatalogType::CopyFunction,
            EntryKind::Table(_) => CatalogType::Table,
            EntryKind::View(_) => CatalogType::View,
            EntryKind::Index(_) => CatalogType::Index,
            EntryKind::Sequence(_) => CatalogType::Sequence,
            EntryKind::Type(_) => CatalogType::Type,
            EntryKind::Macro(_) => CatalogType::Macro,
            EntryKind::TableMacro(_) => CatalogType::TableMacro,
        }
    }

    pub fn as_schema(&self) -> Option<&SchemaEntry> {
        match &self.kind {
            EntryKind::Schema(schema) => Some(schema),
            _ => None,
        }
    }
}
