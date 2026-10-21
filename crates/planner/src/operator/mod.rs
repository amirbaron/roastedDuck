//! Logical operators (DuckDB's `LogicalOperator` hierarchy in `planner/operator`).
//!
//! A [`LogicalOperator`] holds the members every operator has (children, expressions,
//! output types, cardinality estimate) and its operator-specific data in [`LogicalKind`].
//! The generic members let plan rewrites traverse any operator uniformly.

mod cardinality;
mod copy_to_file;
mod display;
mod explain;
mod expression_filter;
mod table_index;

use std::sync::{Arc, Mutex, PoisonError};

use rustdb_ast::{
    CteMaterialize, ExplainFormat, ExplainType, ExpressionType, JoinType, SampleOptions, SetScope,
    TransactionInfo,
};
use rustdb_common::{Error, Result};
use rustdb_function::{FunctionData, TableFunction};
use rustdb_types::{DataChunk, LogicalType, Value};

use crate::binder::CorrelatedColumns;
use crate::column_binding::generate_column_bindings;
use crate::expression::{BoundOrderByNode, BoundOrderModifier, Expression, ExpressionKind};
use crate::generated::enums::{DistinctType, LogicalOperatorType};
use crate::prepared::PreparedStatementData;
use crate::{ColumnBinding, SubqueryType};

pub use copy_to_file::{
    CopyFunctionReturnType, CopyOverwriteMode, ExportedTable, FilenamePattern, LogicalCopyToFile,
    LogicalExport,
};
pub use display::render_plan;
pub use explain::{logical_operator_name, renderer_kind};
pub use expression_filter::ExpressionTableFilter;

/// A LIMIT or OFFSET value (DuckDB's `BoundLimitNode`).
#[derive(Clone, Debug, Default)]
pub enum BoundLimitNode {
    #[default]
    Unset,
    /// `LIMIT 10`
    ConstantValue(u64),
    /// `LIMIT 10%`
    ConstantPercentage(f64),
    /// `LIMIT (expr)`
    ExpressionValue(Box<Expression>),
    /// `LIMIT (expr)%`
    ExpressionPercentage(Box<Expression>),
}

impl BoundLimitNode {
    /// The expression of a non-constant limit.
    pub fn expression_mut(&mut self) -> Option<&mut Expression> {
        match self {
            Self::ExpressionValue(expression) | Self::ExpressionPercentage(expression) => Some(expression),
            Self::Unset | Self::ConstantValue(_) | Self::ConstantPercentage(_) => None,
        }
    }

    /// The expression of a non-constant limit.
    pub fn expression(&self) -> Option<&Expression> {
        match self {
            Self::ExpressionValue(expression) | Self::ExpressionPercentage(expression) => Some(expression),
            Self::Unset | Self::ConstantValue(_) | Self::ConstantPercentage(_) => None,
        }
    }
}

/// A set of group indexes (DuckDB's `GroupingSet`).
pub type GroupingSet = std::collections::BTreeSet<u64>;

pub use rustdb_types::{ColumnIndex, ColumnIndexType};

/// The pseudo column id of `rowid` (DuckDB's `COLUMN_IDENTIFIER_ROW_ID`).
pub const COLUMN_IDENTIFIER_ROW_ID: u64 = u64::MAX;

/// The pseudo column id of a scan that needs no column (DuckDB's
/// `COLUMN_IDENTIFIER_EMPTY`).
pub const COLUMN_IDENTIFIER_EMPTY: u64 = u64::MAX - 1;

/// `GROUP BY` aggregation (DuckDB's `LogicalAggregate`). The aggregates are the
/// operator's `expressions`.
#[derive(Clone, Debug, Default)]
pub struct LogicalAggregate {
    pub group_index: u64,
    pub aggregate_index: u64,
    pub groupings_index: u64,
    pub groups: Vec<Expression>,
    pub grouping_sets: Vec<GroupingSet>,
    /// For each `GROUPING(...)` call: the group indexes it inspects.
    pub grouping_functions: Vec<Vec<u64>>,
    /// The statistics of each group, set by statistics propagation (DuckDB's
    /// `group_stats`); they decide on perfect hash aggregation.
    pub group_stats: Vec<Option<rustdb_storage::statistics::BaseStatistics>>,
}

/// A table or table function scan (DuckDB's `LogicalGet`).
#[derive(Clone, Debug)]
pub struct LogicalGet {
    pub table_index: u64,
    pub function: TableFunction,
    pub bind_data: Option<Arc<dyn FunctionData>>,
    /// All columns the function can produce.
    pub returned_types: Vec<LogicalType>,
    pub names: Vec<String>,
    /// The columns actually scanned (indexes into `returned_types`).
    pub column_ids: Vec<ColumnIndex>,
    /// The scanned columns that are output (empty: all of them).
    pub projection_ids: Vec<u64>,
    pub parameters: Vec<Value>,
    pub named_parameters: Vec<(String, Value)>,
    pub input_table_types: Vec<LogicalType>,
    pub input_table_names: Vec<String>,
    /// Input columns of a table-in/table-out function passed through to the output.
    pub projected_input: Vec<u64>,
    /// The column holding `WITH ORDINALITY` numbers.
    pub ordinality_index: Option<u64>,
    /// The columns bound so far through the scan's table binding, which shares them
    /// (DuckDB's `TableBinding` refers to the scan's `column_ids`).
    pub shared_column_ids: Option<SharedColumnIds>,
    /// The scanned table of a base table scan (DuckDB's `LogicalGet::GetTable`).
    pub table: Option<Arc<rustdb_catalog::CatalogEntry>>,
    /// The catalog and schema of the scanned table, for `EXPLAIN`.
    pub table_schema: Option<(String, String)>,
    /// Filters pushed into the scan, keyed by the filtered column's index in
    /// `returned_types` (DuckDB's `LogicalGet::table_filters`).
    pub table_filters: rustdb_storage::TableFilterSet,
    /// Filters that joins above set at run time, keyed by position in `column_ids`
    /// (DuckDB's `LogicalGet::dynamic_filters`).
    pub dynamic_filters: Option<Arc<rustdb_storage::table_filter::DynamicTableFilterSet>>,
    /// SYSTEM sampling pushed into the scan (DuckDB's `extra_info.sample_options`).
    pub sample_options: Option<Box<rustdb_ast::SampleOptions>>,
    /// The filters that pruned the scanned files, for `EXPLAIN` (the rest of DuckDB's
    /// `ExtraOperatorInfo`).
    pub file_filters: FileFilterInfo,
    /// The order a table scan reads its row groups in (DuckDB's
    /// `row_group_order_options`, set by the `RowGroupPruner`).
    pub row_group_order: Option<rustdb_storage::table::RowGroupOrderOptions>,
}

/// The file filters of a scan (DuckDB's `ExtraOperatorInfo::file_filters`, `total_files`
/// and `filtered_files`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileFilterInfo {
    /// The filters that pruned a file, concatenated.
    pub filters: String,
    pub total_files: Option<u64>,
    pub filtered_files: Option<u64>,
}

impl FileFilterInfo {
    /// The `EXPLAIN` entries: `File Filters` and `Scanning Files`.
    pub fn explain_params(&self) -> Vec<(String, String)> {
        if self.filters.is_empty() {
            return Vec::new();
        }
        let mut result = vec![("File Filters".to_owned(), self.filters.clone())];
        if let (Some(filtered), Some(total)) = (self.filtered_files, self.total_files) {
            result.push(("Scanning Files".to_owned(), format!("{filtered}/{total}")));
        }
        result
    }
}

/// The output bindings of INSERT/DELETE/UPDATE: the table's columns with `RETURNING`, the
/// row count otherwise.
fn modify_bindings(
    return_chunk: bool,
    table_index: u64,
    table: &rustdb_catalog::CatalogEntry,
) -> Vec<ColumnBinding> {
    if return_chunk {
        generate_column_bindings(table_index, table_types(table).len())
    } else {
        vec![ColumnBinding::new(0, 0)]
    }
}

/// Splits AND-ed predicates into separate expressions (DuckDB's
/// `LogicalFilter::SplitPredicates`): further children go to the end of the list.
pub fn split_predicates(expressions: &mut Vec<Expression>) -> bool {
    let mut found = false;
    let mut i = 0;
    while i < expressions.len() {
        if expressions[i].expression_type == ExpressionType::ConjunctionAnd
            && let ExpressionKind::Conjunction(conjunction) = &mut expressions[i].kind
            && !conjunction.children.is_empty()
        {
            found = true;
            let mut children = std::mem::take(&mut conjunction.children).into_iter();
            let first = children.next();
            expressions.extend(children);
            if let Some(first) = first {
                expressions[i] = first;
            }
            // Check the replacement again: it may be an AND too.
            continue;
        }
        i += 1;
    }
    found
}

/// The column ids of a scan, shared between its table binding and the scan.
pub type SharedColumnIds = Arc<Mutex<Vec<ColumnIndex>>>;

/// Copies the columns bound through table bindings into the scans of `op`'s tree.
pub fn sync_column_ids(op: &mut LogicalOperator) {
    if let LogicalKind::Get(get) = &mut op.kind {
        get.sync_column_ids();
    }
    for child in &mut op.children {
        sync_column_ids(child);
    }
}

impl LogicalGet {
    /// Takes over the columns bound through the table binding so far.
    pub fn sync_column_ids(&mut self) {
        if let Some(shared) = &self.shared_column_ids {
            self.column_ids
                .clone_from(&shared.lock().unwrap_or_else(PoisonError::into_inner));
        }
    }

    /// The type of a scanned column.
    pub fn column_type(&self, column: &ColumnIndex) -> LogicalType {
        if column.is_row_id_column() {
            return LogicalType::BIGINT;
        }
        if column.index == COLUMN_IDENTIFIER_EMPTY {
            return LogicalType::BOOLEAN;
        }
        if column.has_type() {
            // A pushed-down extract produces the extracted field (`GetScanType`).
            return column.scan_type().clone();
        }
        if let Some(ty) = self.returned_types.get(column.index as usize) {
            return ty.clone();
        }
        self.virtual_column(column.index)
            .map_or(LogicalType::BIGINT, |(_, ty)| ty)
    }

    /// The name of a scanned column, virtual columns included.
    pub fn column_name(&self, column: &ColumnIndex) -> String {
        if let Some(name) = self.names.get(column.index as usize) {
            return name.clone();
        }
        self.virtual_column(column.index)
            .map(|(name, _)| name)
            .unwrap_or_default()
    }

    /// The name and type of the function's virtual column `index` (`GetVirtualColumns`).
    pub fn virtual_column(&self, index: u64) -> Option<(String, LogicalType)> {
        let virtual_columns = self.function.get_virtual_columns.as_ref()?;
        virtual_columns(self.bind_data.as_deref())
            .into_iter()
            .find(|(id, _, _)| *id == index)
            .map(|(_, name, ty)| (name, ty))
    }
}

/// A join on comparisons between the two sides (DuckDB's `JoinCondition`).
#[derive(Clone, Debug)]
pub struct JoinCondition {
    pub left: Expression,
    pub right: Expression,
    pub comparison: ExpressionType,
}

/// Equi/range joins, also the delim and as-of joins (DuckDB's `LogicalComparisonJoin`).
#[derive(Clone, Debug)]
pub struct LogicalComparisonJoin {
    /// `LOGICAL_COMPARISON_JOIN`, `LOGICAL_DELIM_JOIN` or `LOGICAL_ASOF_JOIN`.
    pub operator_type: LogicalOperatorType,
    pub join_type: JoinType,
    /// The table index of the boolean column of a MARK join.
    pub mark_index: u64,
    pub left_projection_map: Vec<u64>,
    pub right_projection_map: Vec<u64>,
    pub conditions: Vec<JoinCondition>,
    pub mark_types: Vec<LogicalType>,
    /// Delim joins: the columns whose distinct values are passed to the right side.
    pub duplicate_eliminated_columns: Vec<Expression>,
    pub delim_flipped: bool,
    /// Extra non-comparison predicate.
    pub predicate: Option<Expression>,
    /// Whether filter pushdown may turn this MARK join into a SEMI or ANTI join (cleared
    /// when the marker is used above the join).
    pub convert_mark_to_semi: bool,
    /// The run-time filters this join sets on the scans of its probe side from the range
    /// of its build keys (DuckDB's `LogicalComparisonJoin::filter_pushdown`).
    pub join_filters: Vec<JoinFilter>,
}

/// The run-time filters of one join condition (DuckDB's `JoinFilterPushdownInfo` for a
/// condition): filters on the probe keys derived from the build keys, set on each target
/// scan.
#[derive(Clone, Debug)]
pub struct JoinFilter {
    /// The condition, by position after equalities were moved first.
    pub condition: usize,
    pub targets: Vec<JoinFilterTarget>,
    /// Whether the build side filters its input, so a bloom filter of its keys is likely
    /// selective (`build_side_has_filter`).
    pub build_side_has_filter: bool,
}

/// One scan column receiving a join filter (DuckDB's `JoinFilterPushdownFilter` with its
/// `JoinFilterPushdownColumn` for the condition).
#[derive(Clone, Debug)]
pub struct JoinFilterTarget {
    /// The scan's run-time filters.
    pub dynamic_filters: Arc<rustdb_storage::table_filter::DynamicTableFilterSet>,
    /// The filtered column, by position in the scan's `column_ids`.
    pub column_index: usize,
    /// The scanned column's type, which the build key values are cast to.
    pub storage_type: LogicalType,
}

impl JoinCondition {
    /// The condition as a comparison (DuckDB's `JoinCondition::CreateExpression`).
    pub fn into_expression(self) -> Expression {
        Expression::comparison(self.comparison, self.left, self.right)
    }
}

impl LogicalComparisonJoin {
    /// A join without conditions (DuckDB's `LogicalComparisonJoin(type, logical_type)`).
    pub fn new(join_type: JoinType, operator_type: LogicalOperatorType) -> Self {
        Self {
            operator_type,
            join_type,
            mark_index: 0,
            left_projection_map: Vec::new(),
            right_projection_map: Vec::new(),
            conditions: Vec::new(),
            mark_types: Vec::new(),
            duplicate_eliminated_columns: Vec::new(),
            delim_flipped: false,
            predicate: None,
            convert_mark_to_semi: true,
            join_filters: Vec::new(),
        }
    }
}

impl LogicalDependentJoin {
    /// DuckDB's `LogicalDependentJoin(JoinType)`.
    pub fn new(join_type: JoinType) -> Self {
        Self {
            join: LogicalComparisonJoin::new(join_type, LogicalOperatorType::LogicalDependentJoin),
            join_condition: None,
            correlated_columns: CorrelatedColumns::default(),
            subquery_type: SubqueryType::Invalid,
            perform_delim: true,
            any_join: false,
            propagate_null_values: true,
            is_lateral_join: false,
            arbitrary_expressions: Vec::new(),
            expression_children: Vec::new(),
            child_types: Vec::new(),
            child_targets: Vec::new(),
            comparison_type: ExpressionType::Invalid,
        }
    }
}

/// A dependent (correlated) join to be decorrelated (DuckDB's `LogicalDependentJoin`,
/// which extends `LogicalComparisonJoin`: `join` holds the members it inherits).
// Mirrors DuckDB's `LogicalDependentJoin` field for field, flags included.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug)]
pub struct LogicalDependentJoin {
    pub join: LogicalComparisonJoin,
    pub join_condition: Option<Expression>,
    pub correlated_columns: CorrelatedColumns,
    pub subquery_type: SubqueryType,
    pub perform_delim: bool,
    pub any_join: bool,
    pub propagate_null_values: bool,
    pub is_lateral_join: bool,
    pub arbitrary_expressions: Vec<Expression>,
    pub expression_children: Vec<Expression>,
    pub child_types: Vec<LogicalType>,
    pub child_targets: Vec<LogicalType>,
    pub comparison_type: ExpressionType,
}

/// Operator-specific data of a logical operator.
#[derive(Clone, Debug)]
pub enum LogicalKind {
    /// Computes `expressions` for each input row.
    Projection {
        table_index: u64,
    },
    /// Keeps rows where all `expressions` are true.
    Filter {
        projection_map: Vec<u64>,
    },
    Aggregate(Box<LogicalAggregate>),
    /// Window functions (`expressions`) computed over the input.
    Window {
        window_index: u64,
    },
    /// `UNNEST` of list `expressions`.
    Unnest {
        unnest_index: u64,
    },
    Limit {
        limit: BoundLimitNode,
        offset: BoundLimitNode,
    },
    Order {
        orders: Vec<BoundOrderByNode>,
        projection_map: Vec<u64>,
    },
    TopN {
        orders: Vec<BoundOrderByNode>,
        limit: u64,
        offset: u64,
        /// The run-time filter on the first ORDER BY column pushed into scans, which
        /// the Top-N tightens as its heap fills (DuckDB's `LogicalTopN::dynamic_filter`).
        dynamic_filter: Option<Arc<rustdb_storage::table_filter::DynamicFilterData>>,
    },
    Distinct {
        distinct_type: DistinctType,
        distinct_targets: Vec<Expression>,
        order_by: Option<BoundOrderModifier>,
    },
    Sample {
        sample_options: Box<SampleOptions>,
    },
    Pivot {
        pivot_index: u64,
        bound_pivot: Box<BoundPivotInfo>,
    },
    Get(Box<LogicalGet>),
    /// Scans materialised data (DuckDB's `LogicalColumnDataGet`).
    ChunkGet {
        table_index: u64,
        chunk_types: Vec<LogicalType>,
        collection: Arc<[DataChunk]>,
    },
    /// The duplicate-eliminated input of a delim join.
    DelimGet {
        table_index: u64,
        chunk_types: Vec<LogicalType>,
    },
    /// Rows of expressions (`VALUES`), evaluated against a child row.
    ExpressionGet {
        table_index: u64,
        expr_types: Vec<LogicalType>,
        expressions: Vec<Vec<Expression>>,
    },
    /// A single row with no columns (`SELECT 42`).
    DummyScan {
        table_index: u64,
    },
    /// No rows at all.
    EmptyResult {
        return_types: Vec<LogicalType>,
        bindings: Vec<ColumnBinding>,
    },
    /// A reference to a CTE's result.
    CteRef {
        table_index: u64,
        cte_index: u64,
        chunk_types: Vec<LogicalType>,
        bound_columns: Vec<String>,
        correlated_columns: u64,
        is_recurring: bool,
    },
    ComparisonJoin(Box<LogicalComparisonJoin>),
    /// A join on an arbitrary condition.
    AnyJoin {
        join_type: JoinType,
        condition: Box<Expression>,
    },
    CrossProduct,
    PositionalJoin,
    DependentJoin(Box<LogicalDependentJoin>),
    /// `UNION`, `EXCEPT`, `INTERSECT` (the type is in `operator_type`).
    SetOperation {
        operator_type: LogicalOperatorType,
        table_index: u64,
        column_count: u64,
        setop_all: bool,
        allow_out_of_order: bool,
    },
    RecursiveCte {
        ctename: String,
        table_index: u64,
        column_count: u64,
        union_all: bool,
        key_targets: Vec<Expression>,
        correlated_columns: CorrelatedColumns,
        /// Whether the recursive part reads `recurring.<name>`.
        ref_recurring: bool,
    },
    MaterializedCte {
        ctename: String,
        table_index: u64,
        column_count: u64,
        materialize: CteMaterialize,
        correlated_columns: CorrelatedColumns,
    },
    Explain {
        explain_type: ExplainType,
        explain_format: ExplainFormat,
        /// The plan before optimization, rendered when it was bound.
        logical_plan_unopt: String,
    },
    Prepare {
        name: String,
        prepared: Arc<PreparedStatementData>,
    },
    Execute {
        prepared: Arc<PreparedStatementData>,
    },
    /// `SET name = value`.
    Set {
        name: String,
        value: Value,
        scope: SetScope,
    },
    /// `RESET name`.
    Reset {
        name: String,
        scope: SetScope,
    },
    /// `ATTACH`, options evaluated.
    Attach(Box<rustdb_ast::AttachInfo>),
    /// `DETACH`.
    Detach(rustdb_ast::DetachInfo),
    /// `ALTER DATABASE` (DuckDB's `LogicalSimple` of an `AlterDatabaseInfo`, run by the
    /// `DatabaseManager`): the database `catalog` of `info`.
    AlterDatabase(rustdb_ast::AlterInfo),
    /// `BEGIN` / `COMMIT` / `ROLLBACK`.
    Transaction {
        info: TransactionInfo,
    },
    /// `DEALLOCATE name`.
    Deallocate {
        name: String,
    },
    /// A `PRAGMA` call without a result.
    Pragma {
        name: String,
        parameters: Vec<Value>,
    },
    /// `CREATE TABLE`, with the query of `CREATE TABLE AS` as child.
    CreateTable(Box<BoundCreateTableInfo>),
    /// `CREATE SCHEMA` / `CREATE VIEW` (DuckDB's `LogicalCreate`).
    Create {
        operator_type: LogicalOperatorType,
        info: Box<rustdb_ast::CreateInfo>,
    },
    /// `DROP ...` (DuckDB's `LogicalSimple` of type `LOGICAL_DROP`).
    Drop(Box<rustdb_ast::DropInfo>),
    CreateIndex(Box<LogicalCreateIndex>),
    /// `ALTER TABLE` / `ALTER VIEW` (DuckDB's `LogicalSimple` of type `LOGICAL_ALTER`).
    Alter(Box<LogicalAlter>),
    /// `CREATE SECRET` with its expressions evaluated (DuckDB's `LogicalCreateSecret`).
    CreateSecret(Box<CreateSecretInput>),
    Insert(Box<LogicalInsert>),
    Delete(Box<LogicalDelete>),
    Update(Box<LogicalUpdate>),
    MergeInto(Box<LogicalMergeInto>),
    /// `COPY ... TO`.
    CopyToFile(Box<LogicalCopyToFile>),
    /// `EXPORT DATABASE`.
    Export(Box<LogicalExport>),
    /// `VACUUM` / `ANALYZE` of a table's columns, scanned by the child (DuckDB's
    /// `LogicalVacuum` with its `column_id_map`).
    Vacuum(Box<LogicalVacuum>),
}

/// A bound `VACUUM`/`ANALYZE` of a table.
#[derive(Clone, Debug)]
pub struct LogicalVacuum {
    pub table: Arc<rustdb_catalog::CatalogEntry>,
    /// The physical column of each column the child produces.
    pub column_id_map: Vec<usize>,
}

/// A bound `CREATE TABLE` (DuckDB's `BoundCreateTableInfo`): the table definition with
/// its column types bound and its foreign keys resolved.
#[derive(Clone, Debug)]
pub struct BoundCreateTableInfo {
    pub catalog: String,
    pub schema: String,
    /// The `CREATE TABLE` info (`CreateInfoKind::Table`).
    pub info: rustdb_ast::CreateInfo,
}

/// A foreign key bound to its tables (DuckDB's `BoundForeignKeyConstraint`).
#[derive(Clone, Debug)]
pub struct BoundForeignKeyInfo {
    pub fk_type: rustdb_ast::ForeignKeyType,
    /// The other table of the constraint.
    pub schema: String,
    pub table: String,
    /// Key columns (physical indexes) of the referenced table.
    pub pk_keys: Vec<usize>,
    /// Key columns (physical indexes) of the referencing table.
    pub fk_keys: Vec<usize>,
}

/// A constraint bound to the physical columns of its table (DuckDB's `BoundConstraint`).
#[derive(Clone, Debug)]
pub enum BoundConstraint {
    NotNull(usize),
    /// `CHECK (expression)`; the expression refers to physical columns by index.
    Check {
        expression: Expression,
        bound_columns: Vec<usize>,
        /// The constraint as written, for error messages.
        text: String,
    },
    Unique {
        keys: Vec<usize>,
        is_primary_key: bool,
    },
    ForeignKey(BoundForeignKeyInfo),
    /// The expression of a generated column, evaluated over appended rows so that values
    /// it cannot produce fail at INSERT (`VerifyGeneratedExpressionSuccess`).
    Generated {
        expression: Expression,
        /// `name TYPE AS (expression)`, for the error message.
        text: String,
    },
}

/// The input of a secret function (DuckDB's `CreateSecretInput`): type, provider and
/// name lower-cased, the options evaluated.
#[derive(Clone, Debug)]
pub struct CreateSecretInput {
    pub secret_type: String,
    /// Empty for the type's default provider.
    pub provider: String,
    /// `IN storage`, or empty.
    pub storage: String,
    pub name: String,
    pub scope: Vec<String>,
    pub options: Vec<(String, Value)>,
    pub on_conflict: rustdb_ast::OnCreateConflict,
    pub persist_type: rustdb_ast::SecretPersistType,
}

/// A bound `ALTER TABLE` / `ALTER VIEW`: the checks of DuckDB's
/// `DuckTableEntry::AlterEntry` are done while binding; what is left changes storage
/// and the catalog.
#[derive(Clone, Debug)]
pub struct LogicalAlter {
    pub catalog: String,
    pub schema: String,
    /// The altered entry; `None` with `IF EXISTS` when it does not exist.
    pub entry: Option<Arc<rustdb_catalog::CatalogEntry>>,
    pub action: AlterAction,
    /// Whether the alter is allowed while other entries (indexes) depend on the table
    /// (`DependencyManager::AlterObject`: ADD COLUMN and SET DEFAULT).
    pub dependents_allowed: bool,
    /// The statement with the entry's catalog and schema filled in, as the WAL logs it.
    pub info: Option<Box<rustdb_ast::AlterInfo>>,
}

/// What an `ALTER` does. `definition` is the table's new definition (without storage).
#[derive(Clone, Debug)]
pub enum AlterAction {
    /// Nothing (`IF EXISTS` / `IF NOT EXISTS` found nothing to do).
    Nothing,
    /// `RENAME TO`.
    Rename(String),
    /// `COMMENT ON <entry> IS value`.
    SetComment(rustdb_types::Value),
    /// `COMMENT ON COLUMN <table or view>.column IS value`.
    SetColumnComment {
        column: String,
        comment: rustdb_types::Value,
    },
    /// `ALTER SEQUENCE ... OWNED BY schema.name`.
    ChangeOwnership {
        owner_schema: String,
        owner_name: String,
    },
    /// A new definition over the same data (rename column, defaults, drop NOT NULL).
    Definition(rustdb_catalog::TableEntry),
    /// `ADD COLUMN`, filled with `default`.
    AddColumn {
        definition: rustdb_catalog::TableEntry,
        name: String,
        logical_type: LogicalType,
        default: Expression,
    },
    /// `DROP COLUMN` of a physical column.
    RemoveColumn {
        definition: rustdb_catalog::TableEntry,
        column: usize,
    },
    /// `ALTER COLUMN ... TYPE`: physical column `column` becomes `target`, computed by
    /// `expression` over the physical columns `bound_columns`.
    AlterType {
        definition: rustdb_catalog::TableEntry,
        column: usize,
        target: LogicalType,
        expression: Expression,
        bound_columns: Vec<usize>,
    },
    /// `ALTER COLUMN ... SET NOT NULL` of a physical column without NOT NULL.
    SetNotNull {
        definition: rustdb_catalog::TableEntry,
        column: usize,
    },
    /// `ADD PRIMARY KEY`: an index named `index_name` over the physical `keys`.
    AddPrimaryKey {
        definition: rustdb_catalog::TableEntry,
        keys: Vec<usize>,
        index_name: String,
        /// The existing primary key, as `PRIMARY KEY(a, b)`, if any.
        existing: Option<String>,
    },
}

/// `CREATE INDEX` (DuckDB's `LogicalCreateIndex`). The table is scanned by the index
/// build itself.
#[derive(Clone, Debug)]
pub struct LogicalCreateIndex {
    /// The statement (`CreateInfoKind::Index`) with catalog and schema resolved;
    /// `column_ids` are the physical columns the keys read.
    pub info: rustdb_ast::CreateInfo,
    pub table: Arc<rustdb_catalog::CatalogEntry>,
    /// The key expressions, referencing the table's physical columns by index
    /// (`unbound_expressions`).
    pub expressions: Vec<Expression>,
}

/// `INSERT` (DuckDB's `LogicalInsert`).
#[derive(Clone, Debug)]
pub struct LogicalInsert {
    pub table: Arc<rustdb_catalog::CatalogEntry>,
    pub schema: String,
    pub table_index: u64,
    /// For each physical column, the input column holding its value, or `None` to use the
    /// default; empty when the input holds every column in order.
    pub column_index_map: Vec<Option<usize>>,
    pub expected_types: Vec<LogicalType>,
    /// The default of each physical column.
    pub bound_defaults: Vec<Expression>,
    pub bound_constraints: Vec<BoundConstraint>,
    pub return_chunk: bool,
}

/// `DELETE` (DuckDB's `LogicalDelete`); `expressions` holds the row id reference.
#[derive(Clone, Debug)]
pub struct LogicalDelete {
    pub table: Arc<rustdb_catalog::CatalogEntry>,
    pub schema: String,
    pub table_index: u64,
    pub bound_constraints: Vec<BoundConstraint>,
    pub return_chunk: bool,
}

/// `UPDATE` (DuckDB's `LogicalUpdate`); `expressions` holds the new value of each column
/// in `columns`, and the input's last column is the row id.
#[derive(Clone, Debug)]
pub struct LogicalUpdate {
    pub table: Arc<rustdb_catalog::CatalogEntry>,
    pub schema: String,
    pub table_index: u64,
    /// The updated physical columns.
    pub columns: Vec<usize>,
    pub bound_defaults: Vec<Expression>,
    pub bound_constraints: Vec<BoundConstraint>,
    pub update_is_del_and_insert: bool,
    pub return_chunk: bool,
}

/// One `WHEN ... THEN ...` of a bound `MERGE INTO` (DuckDB's `BoundMergeIntoAction`).
/// Expressions reference the columns of the merge's input projection.
#[derive(Clone, Debug)]
pub struct BoundMergeIntoAction {
    pub action_type: rustdb_ast::MergeActionType,
    /// The `AND ...` condition, if any.
    pub condition: Option<Expression>,
    /// The updated physical columns (UPDATE).
    pub columns: Vec<usize>,
    /// The new values (UPDATE, in `columns` order), the inserted values (INSERT) or the
    /// error message (ERROR).
    pub expressions: Vec<Expression>,
    /// For each physical column, the INSERT expression holding its value, or `None` for
    /// the default; empty when the expressions cover every column in order.
    pub column_index_map: Vec<Option<usize>>,
    pub update_is_del_and_insert: bool,
}

/// `MERGE INTO` (DuckDB's `LogicalMergeInto`). Its child is a projection whose column
/// `row_id_start` is the target's row id (NULL when there is no matching target row).
#[derive(Clone, Debug)]
pub struct LogicalMergeInto {
    pub table: Arc<rustdb_catalog::CatalogEntry>,
    pub table_index: u64,
    /// The default of each physical column.
    pub bound_defaults: Vec<Expression>,
    pub row_id_start: usize,
    /// The input column that is NULL for target rows without a matching source row.
    pub source_marker: Option<usize>,
    pub bound_constraints: Vec<BoundConstraint>,
    pub return_chunk: bool,
    pub actions: std::collections::BTreeMap<rustdb_ast::MergeActionCondition, Vec<BoundMergeIntoAction>>,
}

impl LogicalInsert {
    fn table_types(&self) -> Vec<LogicalType> {
        table_types(&self.table)
    }
}

/// The types of the stored columns of a table entry.
pub fn table_types(entry: &rustdb_catalog::CatalogEntry) -> Vec<LogicalType> {
    match &entry.kind {
        rustdb_catalog::EntryKind::Table(table) => table.physical_types(),
        _ => Vec::new(),
    }
}

/// What a `PIVOT` computes (DuckDB's `BoundPivotInfo`).
#[derive(Clone, Debug, Default)]
pub struct BoundPivotInfo {
    pub group_count: u64,
    pub types: Vec<LogicalType>,
    pub pivot_values: Vec<String>,
    pub aggregates: Vec<Expression>,
}

/// A node of a logical plan (DuckDB's `LogicalOperator`).
#[derive(Debug)]
pub struct LogicalOperator {
    pub kind: LogicalKind,
    pub children: Vec<LogicalOperator>,
    pub expressions: Vec<Expression>,
    /// Output column types, filled by [`LogicalOperator::resolve_operator_types`].
    pub types: Vec<LogicalType>,
    pub estimated_cardinality: Option<u64>,
    /// Identifies this operator in analyses keyed by operator (DuckDB keys them by the
    /// operator's address); unique per created or cloned operator.
    node_id: u64,
}

/// The next operator identity.
fn next_node_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Clone for LogicalOperator {
    fn clone(&self) -> Self {
        Self {
            kind: self.kind.clone(),
            children: self.children.clone(),
            expressions: self.expressions.clone(),
            types: self.types.clone(),
            estimated_cardinality: self.estimated_cardinality,
            node_id: next_node_id(),
        }
    }
}

fn map_list<T: Clone>(items: Vec<T>, projection_map: &[u64]) -> Vec<T> {
    if projection_map.is_empty() {
        return items;
    }
    projection_map
        .iter()
        .filter_map(|&index| items.get(index as usize).cloned())
        .collect()
}

impl LogicalOperator {
    /// The operator's identity (see [`LogicalOperator::node_id`]).
    pub const fn node_id(&self) -> u64 {
        self.node_id
    }

    /// An operator without children or expressions.
    pub fn new(kind: LogicalKind) -> Self {
        Self {
            node_id: next_node_id(),
            kind,
            children: Vec::new(),
            expressions: Vec::new(),
            types: Vec::new(),
            estimated_cardinality: None,
        }
    }

    /// An operator with the given expressions.
    #[must_use]
    pub fn with_expressions(mut self, expressions: Vec<Expression>) -> Self {
        self.expressions = expressions;
        self
    }

    /// An operator with the given children.
    #[must_use]
    pub fn with_children(mut self, children: Vec<Self>) -> Self {
        self.children = children;
        self
    }

    /// Appends a child (DuckDB's `AddChild`).
    pub fn add_child(&mut self, child: Self) {
        self.children.push(child);
    }

    /// `LOGICAL_PROJECTION` over `child`.
    pub fn projection(table_index: u64, expressions: Vec<Expression>, child: Self) -> Self {
        Self::new(LogicalKind::Projection { table_index })
            .with_expressions(expressions)
            .with_children(vec![child])
    }

    /// `LOGICAL_FILTER` with one predicate over `child`, split at its ANDs (DuckDB's
    /// `LogicalFilter(expression)`).
    pub fn filter(predicate: Expression, child: Self) -> Self {
        let mut expressions = vec![predicate];
        split_predicates(&mut expressions);
        Self::new(LogicalKind::Filter {
            projection_map: Vec::new(),
        })
        .with_expressions(expressions)
        .with_children(vec![child])
    }

    /// `LOGICAL_CROSS_PRODUCT` of two inputs; a dummy scan (one row, no columns) on
    /// either side is dropped (DuckDB's `LogicalCrossProduct::Create`).
    pub fn cross_product(left: Self, right: Self) -> Self {
        if matches!(left.kind, LogicalKind::DummyScan { .. }) {
            return right;
        }
        if matches!(right.kind, LogicalKind::DummyScan { .. }) {
            return left;
        }
        Self::new(LogicalKind::CrossProduct).with_children(vec![left, right])
    }

    /// The operator's type (DuckDB's `LogicalOperator::type`).
    pub fn operator_type(&self) -> LogicalOperatorType {
        use LogicalOperatorType as T;
        match &self.kind {
            LogicalKind::Projection { .. } => T::LogicalProjection,
            LogicalKind::Filter { .. } => T::LogicalFilter,
            LogicalKind::Aggregate(_) => T::LogicalAggregateAndGroupBy,
            LogicalKind::Window { .. } => T::LogicalWindow,
            LogicalKind::Unnest { .. } => T::LogicalUnnest,
            LogicalKind::Limit { .. } => T::LogicalLimit,
            LogicalKind::Order { .. } => T::LogicalOrderBy,
            LogicalKind::TopN { .. } => T::LogicalTopN,
            LogicalKind::Distinct { .. } => T::LogicalDistinct,
            LogicalKind::Sample { .. } => T::LogicalSample,
            LogicalKind::Pivot { .. } => T::LogicalPivot,
            LogicalKind::Get(_) => T::LogicalGet,
            LogicalKind::ChunkGet { .. } => T::LogicalChunkGet,
            LogicalKind::DelimGet { .. } => T::LogicalDelimGet,
            LogicalKind::ExpressionGet { .. } => T::LogicalExpressionGet,
            LogicalKind::DummyScan { .. } => T::LogicalDummyScan,
            LogicalKind::EmptyResult { .. } => T::LogicalEmptyResult,
            LogicalKind::CteRef { .. } => T::LogicalCteRef,
            LogicalKind::ComparisonJoin(join) => join.operator_type,
            LogicalKind::AnyJoin { .. } => T::LogicalAnyJoin,
            LogicalKind::CrossProduct => T::LogicalCrossProduct,
            LogicalKind::PositionalJoin => T::LogicalPositionalJoin,
            LogicalKind::DependentJoin(_) => T::LogicalDependentJoin,
            LogicalKind::SetOperation { operator_type, .. } | LogicalKind::Create { operator_type, .. } => {
                *operator_type
            }
            LogicalKind::RecursiveCte { .. } => T::LogicalRecursiveCte,
            LogicalKind::MaterializedCte { .. } => T::LogicalMaterializedCte,
            LogicalKind::Explain { .. } => T::LogicalExplain,
            LogicalKind::Prepare { .. } => T::LogicalPrepare,
            LogicalKind::Execute { .. } => T::LogicalExecute,
            LogicalKind::Set { .. } => T::LogicalSet,
            LogicalKind::Reset { .. } => T::LogicalReset,
            LogicalKind::Attach(_) => T::LogicalAttach,
            LogicalKind::Detach(_) => T::LogicalDetach,
            LogicalKind::Transaction { .. } => T::LogicalTransaction,
            LogicalKind::Deallocate { .. } | LogicalKind::Drop(_) => T::LogicalDrop,
            LogicalKind::Pragma { .. } => T::LogicalPragma,
            LogicalKind::CreateSecret(_) => T::LogicalCreateSecret,
            LogicalKind::CreateTable(_) => T::LogicalCreateTable,
            LogicalKind::CreateIndex(_) => T::LogicalCreateIndex,
            LogicalKind::Alter(_) | LogicalKind::AlterDatabase(_) => T::LogicalAlter,
            LogicalKind::Insert(_) => T::LogicalInsert,
            LogicalKind::Delete(_) => T::LogicalDelete,
            LogicalKind::Update(_) => T::LogicalUpdate,
            LogicalKind::MergeInto(_) => T::LogicalMergeInto,
            LogicalKind::CopyToFile(_) => T::LogicalCopyToFile,
            LogicalKind::Export(_) => T::LogicalExport,
            LogicalKind::Vacuum(_) => T::LogicalVacuum,
        }
    }

    /// The join type of a join operator.
    pub fn join_type(&self) -> Option<JoinType> {
        match &self.kind {
            LogicalKind::ComparisonJoin(join) => Some(join.join_type),
            LogicalKind::AnyJoin { join_type, .. } => Some(*join_type),
            LogicalKind::DependentJoin(dependent) => Some(dependent.join.join_type),
            _ => None,
        }
    }

    fn child_bindings(&self, index: usize) -> Vec<ColumnBinding> {
        self.children
            .get(index)
            .map(Self::column_bindings)
            .unwrap_or_default()
    }

    fn child_types(&self, index: usize) -> Vec<LogicalType> {
        self.children
            .get(index)
            .map(|child| child.types.clone())
            .unwrap_or_default()
    }

    /// The bindings of the columns this operator outputs (DuckDB's `GetColumnBindings`).
    pub fn column_bindings(&self) -> Vec<ColumnBinding> {
        match &self.kind {
            LogicalKind::Projection { table_index } => {
                generate_column_bindings(*table_index, self.expressions.len())
            }
            LogicalKind::Filter { projection_map } | LogicalKind::Order { projection_map, .. } => {
                map_list(self.child_bindings(0), projection_map)
            }
            LogicalKind::Aggregate(aggregate) => {
                let mut bindings = generate_column_bindings(aggregate.group_index, aggregate.groups.len());
                bindings.extend(generate_column_bindings(
                    aggregate.aggregate_index,
                    self.expressions.len(),
                ));
                bindings.extend(generate_column_bindings(
                    aggregate.groupings_index,
                    aggregate.grouping_functions.len(),
                ));
                bindings
            }
            LogicalKind::Window { window_index: index } | LogicalKind::Unnest { unnest_index: index } => {
                let mut bindings = self.child_bindings(0);
                bindings.extend(generate_column_bindings(*index, self.expressions.len()));
                bindings
            }
            LogicalKind::Limit { .. }
            | LogicalKind::TopN { .. }
            | LogicalKind::Distinct { .. }
            | LogicalKind::Sample { .. } => self.child_bindings(0),
            LogicalKind::Pivot {
                pivot_index,
                bound_pivot,
            } => generate_column_bindings(*pivot_index, bound_pivot.types.len()),
            LogicalKind::Get(get) => {
                if get.column_ids.is_empty() {
                    return vec![ColumnBinding::new(get.table_index, 0)];
                }
                let mut bindings = if get.projection_ids.is_empty() {
                    generate_column_bindings(get.table_index, get.column_ids.len())
                } else {
                    get.projection_ids
                        .iter()
                        .map(|&id| ColumnBinding::new(get.table_index, id))
                        .collect()
                };
                if !get.projected_input.is_empty() {
                    let child = self.child_bindings(0);
                    bindings.extend(
                        get.projected_input
                            .iter()
                            .filter_map(|&entry| child.get(entry as usize).copied()),
                    );
                }
                bindings
            }
            LogicalKind::ChunkGet {
                table_index,
                chunk_types,
                ..
            }
            | LogicalKind::DelimGet {
                table_index,
                chunk_types,
            }
            | LogicalKind::CteRef {
                table_index,
                chunk_types,
                ..
            } => generate_column_bindings(*table_index, chunk_types.len()),
            LogicalKind::ExpressionGet {
                table_index,
                expr_types,
                ..
            } => generate_column_bindings(*table_index, expr_types.len()),
            LogicalKind::DummyScan { table_index } => vec![ColumnBinding::new(*table_index, 0)],
            LogicalKind::EmptyResult { bindings, .. } => bindings.clone(),
            LogicalKind::ComparisonJoin(join) => self.join_bindings(
                join.join_type,
                join.mark_index,
                &join.left_projection_map,
                &join.right_projection_map,
            ),
            LogicalKind::AnyJoin { join_type, .. } => self.join_bindings(*join_type, 0, &[], &[]),
            LogicalKind::DependentJoin(dependent) => self.join_bindings(
                dependent.join.join_type,
                dependent.join.mark_index,
                &dependent.join.left_projection_map,
                &dependent.join.right_projection_map,
            ),
            LogicalKind::CrossProduct | LogicalKind::PositionalJoin => {
                let mut bindings = self.child_bindings(0);
                bindings.extend(self.child_bindings(1));
                bindings
            }
            LogicalKind::SetOperation {
                table_index,
                column_count,
                ..
            }
            | LogicalKind::RecursiveCte {
                table_index,
                column_count,
                ..
            } => generate_column_bindings(*table_index, *column_count as usize),
            LogicalKind::MaterializedCte { .. } => self.child_bindings(1),
            LogicalKind::Explain { .. } => vec![ColumnBinding::new(0, 0), ColumnBinding::new(0, 1)],
            LogicalKind::Execute { prepared } => generate_column_bindings(0, prepared.types.len()),
            LogicalKind::Prepare { .. }
            | LogicalKind::Export(_)
            | LogicalKind::Vacuum(_)
            | LogicalKind::Set { .. }
            | LogicalKind::Reset { .. }
            | LogicalKind::Attach(_)
            | LogicalKind::Detach(_)
            | LogicalKind::AlterDatabase(_)
            | LogicalKind::Transaction { .. }
            | LogicalKind::Deallocate { .. }
            | LogicalKind::Pragma { .. }
            | LogicalKind::CreateSecret(_)
            | LogicalKind::CreateTable(_)
            | LogicalKind::CreateIndex(_)
            | LogicalKind::Alter(_)
            | LogicalKind::Create { .. }
            | LogicalKind::Drop(_) => vec![ColumnBinding::new(0, 0)],
            LogicalKind::Insert(insert) => {
                modify_bindings(insert.return_chunk, insert.table_index, &insert.table)
            }
            LogicalKind::Delete(delete) => {
                modify_bindings(delete.return_chunk, delete.table_index, &delete.table)
            }
            LogicalKind::Update(update) => {
                modify_bindings(update.return_chunk, update.table_index, &update.table)
            }
            LogicalKind::CopyToFile(copy) => generate_column_bindings(0, copy.return_type.types().len()),
            LogicalKind::MergeInto(merge) => {
                if merge.return_chunk {
                    generate_column_bindings(merge.table_index, table_types(&merge.table).len() + 1)
                } else {
                    vec![ColumnBinding::new(0, 0)]
                }
            }
        }
    }

    /// The single table index all output columns share (DuckDB's `GetRootIndex`).
    pub fn root_index(&self) -> Result<u64> {
        let bindings = self.column_bindings();
        let first = bindings
            .first()
            .ok_or_else(|| Error::internal("Empty bindings in GetRootIndex"))?
            .table_index;
        if bindings.iter().any(|binding| binding.table_index != first) {
            return Err(Error::internal("GetRootIndex - multiple column bindings found"));
        }
        Ok(first)
    }

    /// DuckDB's `LogicalJoin::GetColumnBindings`.
    fn join_bindings(
        &self,
        join_type: JoinType,
        mark_index: u64,
        left_projection_map: &[u64],
        right_projection_map: &[u64],
    ) -> Vec<ColumnBinding> {
        let mut left = map_list(self.child_bindings(0), left_projection_map);
        match join_type {
            JoinType::Semi | JoinType::Anti => left,
            JoinType::Mark => {
                left.push(ColumnBinding::new(mark_index, 0));
                left
            }
            _ => {
                let right = map_list(self.child_bindings(1), right_projection_map);
                if matches!(join_type, JoinType::RightSemi | JoinType::RightAnti) {
                    return right;
                }
                left.extend(right);
                left
            }
        }
    }

    /// DuckDB's `LogicalJoin::ResolveTypes`.
    fn join_types(&self, join_type: JoinType, left_map: &[u64], right_map: &[u64]) -> Vec<LogicalType> {
        let mut left = map_list(self.child_types(0), left_map);
        match join_type {
            JoinType::Semi | JoinType::Anti => left,
            JoinType::Mark => {
                left.push(LogicalType::BOOLEAN);
                left
            }
            _ => {
                let right = map_list(self.child_types(1), right_map);
                if matches!(join_type, JoinType::RightSemi | JoinType::RightAnti) {
                    return right;
                }
                left.extend(right);
                left
            }
        }
    }

    /// Computes `types` from the children's types and the expressions (DuckDB's
    /// `ResolveTypes`).
    fn resolve_types(&mut self) {
        if let LogicalKind::Get(get) = &mut self.kind {
            get.sync_column_ids();
            get.shared_column_ids = None;
            if get.column_ids.is_empty() {
                // No projection: scan any column to know how many rows there are.
                get.column_ids.push(ColumnIndex::new(COLUMN_IDENTIFIER_ROW_ID));
            }
        }
        let expression_types = || {
            self.expressions
                .iter()
                .map(|e| e.return_type.clone())
                .collect::<Vec<_>>()
        };
        self.types = match &self.kind {
            LogicalKind::Projection { .. } => expression_types(),
            LogicalKind::Filter { projection_map } | LogicalKind::Order { projection_map, .. } => {
                map_list(self.child_types(0), projection_map)
            }
            LogicalKind::Aggregate(aggregate) => {
                let mut types: Vec<LogicalType> =
                    aggregate.groups.iter().map(|g| g.return_type.clone()).collect();
                types.extend(expression_types());
                types.extend(std::iter::repeat_n(
                    LogicalType::BIGINT,
                    aggregate.grouping_functions.len(),
                ));
                types
            }
            LogicalKind::Window { .. } | LogicalKind::Unnest { .. } => {
                let mut types = self.child_types(0);
                types.extend(expression_types());
                types
            }
            LogicalKind::Limit { .. }
            | LogicalKind::TopN { .. }
            | LogicalKind::Distinct { .. }
            | LogicalKind::Sample { .. }
            | LogicalKind::SetOperation { .. }
            | LogicalKind::RecursiveCte { .. } => self.child_types(0),
            LogicalKind::MaterializedCte { .. } => self.child_types(1),
            LogicalKind::Pivot { bound_pivot, .. } => bound_pivot.types.clone(),
            LogicalKind::Get(get) => {
                let mut types: Vec<LogicalType> = if get.projection_ids.is_empty() {
                    get.column_ids
                        .iter()
                        .map(|column| get.column_type(column))
                        .collect()
                } else {
                    get.projection_ids
                        .iter()
                        .filter_map(|&id| get.column_ids.get(id as usize))
                        .map(|column| get.column_type(column))
                        .collect()
                };
                let input = self
                    .children
                    .first()
                    .map(|child| child.types.clone())
                    .unwrap_or_default();
                types.extend(
                    get.projected_input
                        .iter()
                        .filter_map(|&entry| input.get(entry as usize).cloned()),
                );
                types
            }
            LogicalKind::ChunkGet { chunk_types, .. }
            | LogicalKind::DelimGet { chunk_types, .. }
            | LogicalKind::CteRef { chunk_types, .. } => chunk_types.clone(),
            LogicalKind::ExpressionGet { expr_types, .. } => expr_types.clone(),
            LogicalKind::DummyScan { .. } => vec![LogicalType::INTEGER],
            LogicalKind::EmptyResult { return_types, .. } => return_types.clone(),
            LogicalKind::ComparisonJoin(join) => {
                let (join_type, left, right) = (
                    join.join_type,
                    join.left_projection_map.clone(),
                    join.right_projection_map.clone(),
                );
                self.join_types(join_type, &left, &right)
            }
            LogicalKind::AnyJoin { join_type, .. } => {
                let join_type = *join_type;
                self.join_types(join_type, &[], &[])
            }
            LogicalKind::DependentJoin(dependent) => {
                let (join_type, left, right) = (
                    dependent.join.join_type,
                    dependent.join.left_projection_map.clone(),
                    dependent.join.right_projection_map.clone(),
                );
                self.join_types(join_type, &left, &right)
            }
            LogicalKind::CrossProduct | LogicalKind::PositionalJoin => {
                let mut types = self.child_types(0);
                types.extend(self.child_types(1));
                types
            }
            LogicalKind::Explain { .. } => vec![LogicalType::VARCHAR, LogicalType::VARCHAR],
            LogicalKind::Execute { prepared } => prepared.types.clone(),
            LogicalKind::Prepare { .. }
            | LogicalKind::Export(_)
            | LogicalKind::Vacuum(_)
            | LogicalKind::Deallocate { .. }
            | LogicalKind::Set { .. }
            | LogicalKind::Reset { .. }
            | LogicalKind::Attach(_)
            | LogicalKind::Detach(_)
            | LogicalKind::AlterDatabase(_)
            | LogicalKind::Transaction { .. }
            | LogicalKind::Pragma { .. }
            | LogicalKind::CreateSecret(_)
            | LogicalKind::Alter(_)
            | LogicalKind::Drop(_) => vec![LogicalType::BOOLEAN],
            LogicalKind::Insert(insert) if insert.return_chunk => insert.table_types(),
            LogicalKind::Delete(delete) if delete.return_chunk => table_types(&delete.table),
            LogicalKind::Update(update) if update.return_chunk => table_types(&update.table),
            LogicalKind::CopyToFile(copy) => copy.return_type.types(),
            LogicalKind::MergeInto(merge) if merge.return_chunk => {
                // The RETURNING input ends with the `merge_action` column.
                let mut types = table_types(&merge.table);
                types.push(LogicalType::VARCHAR);
                types
            }
            LogicalKind::CreateTable(_)
            | LogicalKind::CreateIndex(_)
            | LogicalKind::Create { .. }
            | LogicalKind::Insert(_)
            | LogicalKind::Delete(_)
            | LogicalKind::Update(_)
            | LogicalKind::MergeInto(_) => vec![LogicalType::BIGINT],
        };
    }

    /// Resolves the output types of the whole tree, children first (DuckDB's
    /// `ResolveOperatorTypes`).
    pub fn resolve_operator_types(&mut self) -> Result<()> {
        for child in &mut self.children {
            child.resolve_operator_types()?;
        }
        self.resolve_types();
        if self.types.len() != self.column_bindings().len() {
            return Err(Error::internal(format!(
                "{} has {} types but {} column bindings",
                self.operator_type().name(),
                self.types.len(),
                self.column_bindings().len()
            )));
        }
        Ok(())
    }

    /// Calls `callback` on every expression of this operator (not its children), including
    /// expressions stored outside `expressions` (DuckDB's
    /// `LogicalOperatorVisitor::EnumerateExpressions`).
    pub fn try_for_each_expression_mut<E>(
        &mut self,
        mut callback: impl FnMut(&mut Expression) -> Result<(), E>,
    ) -> Result<(), E> {
        match &mut self.kind {
            LogicalKind::ExpressionGet { expressions, .. } => {
                for row in expressions {
                    for expression in row {
                        callback(expression)?;
                    }
                }
            }
            LogicalKind::Order { orders, .. } | LogicalKind::TopN { orders, .. } => {
                for order in orders {
                    callback(&mut order.expression)?;
                }
            }
            LogicalKind::Distinct {
                distinct_targets,
                order_by,
                ..
            } => {
                for target in distinct_targets {
                    callback(target)?;
                }
                if let Some(order_by) = order_by {
                    for order in &mut order_by.orders {
                        callback(&mut order.expression)?;
                    }
                }
            }
            LogicalKind::ComparisonJoin(join) => {
                for target in &mut join.duplicate_eliminated_columns {
                    callback(target)?;
                }
                for condition in &mut join.conditions {
                    callback(&mut condition.left)?;
                    callback(&mut condition.right)?;
                }
                if let Some(predicate) = &mut join.predicate {
                    callback(predicate)?;
                }
            }
            LogicalKind::AnyJoin { condition, .. } => callback(condition)?,
            LogicalKind::DependentJoin(dependent) => {
                for target in &mut dependent.join.duplicate_eliminated_columns {
                    callback(target)?;
                }
                for condition in &mut dependent.join.conditions {
                    callback(&mut condition.left)?;
                    callback(&mut condition.right)?;
                }
                for expression in &mut dependent.arbitrary_expressions {
                    callback(expression)?;
                }
                for expression in &mut dependent.expression_children {
                    callback(expression)?;
                }
            }
            LogicalKind::Limit { limit, offset } => {
                for node in [limit, offset] {
                    if let Some(expression) = node.expression_mut() {
                        callback(expression)?;
                    }
                }
            }
            LogicalKind::Aggregate(aggregate) => {
                for group in &mut aggregate.groups {
                    callback(group)?;
                }
            }
            LogicalKind::RecursiveCte { key_targets, .. } => {
                for target in key_targets {
                    callback(target)?;
                }
            }
            LogicalKind::MergeInto(merge) => {
                for action in merge.actions.values_mut().flatten() {
                    if let Some(condition) = &mut action.condition {
                        callback(condition)?;
                    }
                    for expression in &mut action.expressions {
                        callback(expression)?;
                    }
                }
            }
            _ => {}
        }
        for expression in &mut self.expressions {
            callback(expression)?;
        }
        Ok(())
    }

    /// Calls `callback` on every expression of this operator.
    pub fn for_each_expression_mut(&mut self, mut callback: impl FnMut(&mut Expression)) {
        let _: Result<(), ()> = self.try_for_each_expression_mut(|expression| {
            callback(expression);
            Ok(())
        });
    }
}
