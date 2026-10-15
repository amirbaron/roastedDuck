//! Parsed statements (DuckDB's `SQLStatement` hierarchy).
//!
//! DuckDB has no serialization spec for statements other than SELECT — the parse infos
//! they wrap (`CreateInfo`, `AlterInfo`, `CopyInfo`, ...) are what the catalog and WAL
//! store — so these types are written by hand after `src/include/duckdb/parser/statement/`.

use std::collections::{BTreeMap, BTreeSet};

use rustdb_common::CaseInsensitiveMap;

use crate::{
    AlterInfo, AttachInfo, CommonTableExpressionMap, CopyInfo, CreateInfo, DetachInfo, DropInfo,
    ExplainFormat, ExplainType, InsertColumnOrder, LoadInfo, MergeActionCondition, MergeActionType,
    OnConflictAction, ParsedExpression, PragmaInfo, SelectStatement, SetScope, StatementType, TableRef,
    TransactionInfo, UpdateExtensionsInfo, VacuumInfo,
};

/// One statement of a query string, with its position in it.
#[derive(Clone, Debug, PartialEq)]
pub struct SqlStatement {
    /// Byte offset of the statement in the query string.
    pub location: u64,
    /// Length of the statement in the query string.
    pub length: u64,
    /// The statement's text.
    pub query: String,
    /// The prepared-statement parameters by name (positional ones are named `1`, `2`,
    /// ...), with their indexes, in DuckDB's iteration order (`named_param_map`). For a
    /// SELECT this equals [`SelectStatement::named_param_map`].
    pub named_param_map: CaseInsensitiveMap<u64>,
    /// What the statement does.
    pub kind: SqlStatementKind,
}

impl SqlStatement {
    /// A statement without position information.
    pub fn new(kind: SqlStatementKind) -> Self {
        Self {
            location: 0,
            length: 0,
            query: String::new(),
            named_param_map: CaseInsensitiveMap::default(),
            kind,
        }
    }

    /// The `StatementType` of this statement.
    pub const fn statement_type(&self) -> StatementType {
        self.kind.statement_type()
    }
}

/// The statements the SQL parser produces. (DuckDB's `RelationStatement`,
/// `ExtensionStatement` and `LogicalPlanStatement` only come from its client APIs and
/// extensions, and have no SQL syntax.)
#[derive(Clone, Debug, PartialEq)]
pub enum SqlStatementKind {
    /// `SELECT`, `VALUES`, `FROM`, set operations, `WITH`, ...
    Select(SelectStatement),
    /// `INSERT INTO ...`
    Insert(Box<InsertStatement>),
    /// `UPDATE ... SET ...`
    Update(Box<UpdateStatement>),
    /// `DELETE FROM ...`
    Delete(Box<DeleteStatement>),
    /// `MERGE INTO ...`
    MergeInto(Box<MergeIntoStatement>),
    /// `CREATE ...`
    Create(Box<CreateInfo>),
    /// `ALTER ...`, `COMMENT ON ...`
    Alter(Box<AlterInfo>),
    /// `DROP ...`
    Drop(Box<DropInfo>),
    /// `PREPARE name AS statement`
    Prepare(PrepareStatement),
    /// `EXECUTE name(values)`
    Execute(ExecuteStatement),
    /// `BEGIN`, `COMMIT`, `ROLLBACK`
    Transaction(TransactionInfo),
    /// `COPY ... FROM/TO ...`
    Copy(Box<CopyInfo>),
    /// `COPY FROM DATABASE ... TO ...`
    CopyDatabase(CopyDatabaseStatement),
    /// `EXPORT DATABASE ...`
    Export(ExportStatement),
    /// `EXPLAIN [ANALYZE] statement`
    Explain(ExplainStatement),
    /// `PRAGMA ...`
    Pragma(Box<PragmaInfo>),
    /// `VACUUM`, `ANALYZE`
    Vacuum(Box<VacuumInfo>),
    /// `CALL function(...)`
    Call(CallStatement),
    /// `SET name = value`, `RESET name`
    Set(SetStatement),
    /// `LOAD`, `INSTALL`, `FORCE INSTALL`
    Load(LoadInfo),
    /// `ATTACH ...`
    Attach(Box<AttachInfo>),
    /// `DETACH ...`
    Detach(DetachInfo),
    /// `UPDATE EXTENSIONS ...`
    UpdateExtensions(UpdateExtensionsInfo),
    /// Several statements the parser expands one statement into.
    Multi(Vec<SqlStatement>),
}

impl SqlStatementKind {
    /// DuckDB's `SQLStatement::type`.
    pub const fn statement_type(&self) -> StatementType {
        match self {
            Self::Select(_) => StatementType::SelectStatement,
            Self::Insert(_) => StatementType::InsertStatement,
            Self::Update(_) => StatementType::UpdateStatement,
            Self::Delete(_) => StatementType::DeleteStatement,
            Self::MergeInto(_) => StatementType::MergeIntoStatement,
            Self::Create(_) => StatementType::CreateStatement,
            Self::Alter(_) => StatementType::AlterStatement,
            Self::Drop(_) => StatementType::DropStatement,
            Self::Prepare(_) => StatementType::PrepareStatement,
            Self::Execute(_) => StatementType::ExecuteStatement,
            Self::Transaction(_) => StatementType::TransactionStatement,
            Self::Copy(_) => StatementType::CopyStatement,
            Self::CopyDatabase(_) => StatementType::CopyDatabaseStatement,
            Self::Export(_) => StatementType::ExportStatement,
            Self::Explain(_) => StatementType::ExplainStatement,
            Self::Pragma(_) => StatementType::PragmaStatement,
            Self::Vacuum(_) => StatementType::VacuumStatement,
            Self::Call(_) => StatementType::CallStatement,
            Self::Set(_) => StatementType::SetStatement,
            Self::Load(_) => StatementType::LoadStatement,
            Self::Attach(_) => StatementType::AttachStatement,
            Self::Detach(_) => StatementType::DetachStatement,
            Self::UpdateExtensions(_) => StatementType::UpdateExtensionsStatement,
            Self::Multi(_) => StatementType::MultiStatement,
        }
    }
}

/// `INSERT` (DuckDB's `InsertStatement`).
#[derive(Clone, Debug, PartialEq)]
pub struct InsertStatement {
    /// The rows to insert (a `VALUES` list or a query); `None` for `DEFAULT VALUES`.
    pub select_statement: Option<Box<SelectStatement>>,
    /// The target columns, if listed.
    pub columns: Vec<String>,
    pub catalog: String,
    pub schema: String,
    pub table: String,
    /// The target table as a table reference (for aliases).
    pub table_ref: Option<Box<TableRef>>,
    /// `RETURNING ...`
    pub returning_list: Vec<ParsedExpression>,
    /// `ON CONFLICT ...` / `INSERT OR REPLACE` / `INSERT OR IGNORE`.
    pub on_conflict_info: Option<Box<OnConflictInfo>>,
    pub cte_map: CommonTableExpressionMap,
    /// `DEFAULT VALUES`.
    pub default_values: bool,
    /// `BY POSITION` or `BY NAME`.
    pub column_order: InsertColumnOrder,
}

/// `ON CONFLICT ...` of an `INSERT` (DuckDB's `OnConflictInfo`).
#[derive(Clone, Debug, PartialEq)]
pub struct OnConflictInfo {
    pub action_type: OnConflictAction,
    /// The conflict target columns.
    pub indexed_columns: Vec<String>,
    /// `DO UPDATE SET ...`
    pub set_info: Option<Box<UpdateSetInfo>>,
    /// The `WHERE` of the conflict target.
    pub condition: Option<Box<ParsedExpression>>,
}

/// The `SET` assignments of an `UPDATE` (DuckDB's `UpdateSetInfo`).
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateSetInfo {
    /// `WHERE ...`
    pub condition: Option<Box<ParsedExpression>>,
    /// The assigned columns, parallel to `expressions`.
    pub columns: Vec<String>,
    pub expressions: Vec<ParsedExpression>,
}

/// `UPDATE` (DuckDB's `UpdateStatement`).
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateStatement {
    pub table: Box<TableRef>,
    /// `FROM ...`
    pub from_table: Option<Box<TableRef>>,
    pub set_info: Box<UpdateSetInfo>,
    /// `RETURNING ...`
    pub returning_list: Vec<ParsedExpression>,
    pub cte_map: CommonTableExpressionMap,
    /// Bind names like `ALTER TABLE` does, preferring the table's catalog.
    pub prioritize_table_when_binding: bool,
}

/// `DELETE` (DuckDB's `DeleteStatement`).
#[derive(Clone, Debug, PartialEq)]
pub struct DeleteStatement {
    pub table: Box<TableRef>,
    /// `WHERE ...`
    pub condition: Option<Box<ParsedExpression>>,
    /// `USING ...`
    pub using_clauses: Vec<TableRef>,
    /// `RETURNING ...`
    pub returning_list: Vec<ParsedExpression>,
    pub cte_map: CommonTableExpressionMap,
}

/// `MERGE INTO` (DuckDB's `MergeIntoStatement`).
#[derive(Clone, Debug, PartialEq)]
pub struct MergeIntoStatement {
    pub target: Box<TableRef>,
    pub source: Box<TableRef>,
    /// `ON ...`
    pub join_condition: Option<Box<ParsedExpression>>,
    /// `USING (...)`
    pub using_columns: Vec<String>,
    /// The `WHEN ... THEN ...` actions, by condition, in order.
    pub actions: BTreeMap<MergeActionCondition, Vec<MergeIntoAction>>,
    /// `RETURNING ...`
    pub returning_list: Vec<ParsedExpression>,
    pub cte_map: CommonTableExpressionMap,
}

/// One `WHEN ... THEN ...` of a `MERGE INTO` (DuckDB's `MergeIntoAction`).
#[derive(Clone, Debug, PartialEq)]
pub struct MergeIntoAction {
    pub action_type: MergeActionType,
    /// `AND ...` after `WHEN [NOT] MATCHED`.
    pub condition: Option<Box<ParsedExpression>>,
    /// `UPDATE SET ...`
    pub update_info: Option<Box<UpdateSetInfo>>,
    /// `INSERT (columns)`
    pub insert_columns: Vec<String>,
    /// `INSERT VALUES (...)`
    pub expressions: Vec<ParsedExpression>,
    /// `BY POSITION` or `BY NAME`.
    pub column_order: InsertColumnOrder,
    /// `INSERT DEFAULT VALUES`.
    pub default_values: bool,
    /// Columns left out of an `INSERT BY NAME` / `UPDATE BY NAME`.
    pub exclude_columns: BTreeSet<String>,
}

/// `PREPARE name AS statement` (DuckDB's `PrepareStatement`).
#[derive(Clone, Debug, PartialEq)]
pub struct PrepareStatement {
    pub name: String,
    pub statement: Box<SqlStatement>,
}

/// `EXECUTE name(values)` (DuckDB's `ExecuteStatement`).
#[derive(Clone, Debug, PartialEq)]
pub struct ExecuteStatement {
    pub name: String,
    /// The parameter values, by parameter name (positional ones are named `1`, `2`, ...).
    pub named_values: CaseInsensitiveMap<ParsedExpression>,
}

/// What `COPY FROM DATABASE` copies (DuckDB's `CopyDatabaseType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CopyDatabaseType {
    /// `(SCHEMA)`: the catalog entries only.
    CopySchema,
    /// `(DATA)`: the table contents only.
    CopyData,
}

/// `COPY FROM DATABASE source TO target` (DuckDB's `CopyDatabaseStatement`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CopyDatabaseStatement {
    pub from_database: String,
    pub to_database: String,
    pub copy_type: CopyDatabaseType,
}

/// `EXPORT DATABASE [name TO] 'path' (options)` (DuckDB's `ExportStatement`).
#[derive(Clone, Debug, PartialEq)]
pub struct ExportStatement {
    pub info: Box<CopyInfo>,
    /// The database to export, or empty for the default one.
    pub database: String,
}

/// `EXPLAIN [ANALYZE] statement` (DuckDB's `ExplainStatement`).
#[derive(Clone, Debug, PartialEq)]
pub struct ExplainStatement {
    pub statement: Box<SqlStatement>,
    pub explain_type: ExplainType,
    pub explain_format: ExplainFormat,
}

/// `CALL function(...)` (DuckDB's `CallStatement`).
#[derive(Clone, Debug, PartialEq)]
pub struct CallStatement {
    pub function: Box<ParsedExpression>,
}

/// `SET` / `RESET` of a setting or variable (DuckDB's `SetVariableStatement` and
/// `ResetVariableStatement`).
#[derive(Clone, Debug, PartialEq)]
pub struct SetStatement {
    pub name: String,
    pub scope: SetScope,
    /// The new value for `SET`; `None` for `RESET`.
    pub value: Option<Box<ParsedExpression>>,
}
