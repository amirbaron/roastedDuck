//! The intermediate values transform rules hand to each other (DuckDB's
//! `extension/autocomplete/include/ast/*.hpp`, `QualifiedName` and `GroupByNode`), and
//! the newtypes that keep DuckDB's distinction between a node and its base class where
//! the parse tree of the Rust port merges them.

use std::collections::BTreeSet;

use rustdb_ast::{
    ColumnDefinition, ColumnList, Constraint, ConstraintType, JoinRefType, JoinType, ParsedExpression,
    PivotColumn, QueryNode, SelectStatement, SetScope, TableRef, WindowBoundary, WindowExcludeMode,
};
use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_types::{LogicalType, Value};

/// A possibly qualified name, `[catalog.][schema.]name` (DuckDB's `QualifiedName`);
/// missing parts are empty (`INVALID_CATALOG` / `INVALID_SCHEMA`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct QualifiedName {
    pub catalog: String,
    pub schema: String,
    pub name: String,
}

impl QualifiedName {
    /// `StringToQualifiedName`: `[name]`, `[schema, name]` or `[catalog, schema, name]`.
    pub fn from_parts(mut parts: Vec<String>) -> Result<Self> {
        let name = parts
            .pop()
            .ok_or_else(|| Error::internal("QualifiedName cannot be made with an empty input."))?;
        let mut result = Self {
            name,
            ..Self::default()
        };
        match parts.len() {
            0 => {}
            1 => result.schema = parts.remove(0),
            2 => {
                result.schema = parts.remove(1);
                result.catalog = parts.remove(0);
            }
            _ => {
                return Err(Error::parser(
                    "Too many qualifications found - expected [catalog.schema.name] or [schema.name]",
                ));
            }
        }
        Ok(result)
    }
}

/// An option of `COPY`, `ATTACH`, `EXPORT`, ... (DuckDB's `GenericCopyOption`): a name
/// with constant values, or with an expression.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GenericCopyOption {
    pub name: String,
    pub children: Vec<Value>,
    pub expression: Option<ParsedExpression>,
}

impl GenericCopyOption {
    /// An option with one value.
    pub fn new(name: impl Into<String>, value: Value) -> Self {
        Self {
            name: name.into(),
            children: vec![value],
            expression: None,
        }
    }

    /// `GetFirstChildOrExpression`.
    pub fn first_child_or_expression(&self) -> Result<ParsedExpression> {
        if let Some(value) = self.children.first() {
            return Ok(ParsedExpression::constant(value.clone()));
        }
        self.expression
            .clone()
            .ok_or_else(|| Error::invalid_input("No expression in GenericCopyOption"))
    }
}

/// `LIMIT` / `OFFSET` values, possibly a percentage (DuckDB's `LimitPercentResult`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LimitPercentResult {
    pub is_percent: bool,
    pub expression: Option<ParsedExpression>,
}

/// The kind of a join before its right side (DuckDB's `JoinPrefix`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JoinPrefix {
    pub ref_type: JoinRefType,
    pub join_type: JoinType,
}

/// `ON expr` or `USING (columns)` of a join (DuckDB's `JoinQualifier`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct JoinQualifier {
    pub on_clause: Option<ParsedExpression>,
    pub using_columns: Vec<String>,
}

/// `DISTINCT [ON (...)]` / `ALL` (DuckDB's `DistinctClause`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DistinctClause {
    pub is_distinct: bool,
    pub distinct_targets: Vec<ParsedExpression>,
}

/// `AS name (columns)` (DuckDB's `TableAlias`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TableAlias {
    pub name: String,
    pub column_name_alias: Vec<String>,
}

/// A parameter of a macro (DuckDB's `MacroParameter`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MacroParameter {
    pub expression: Option<ParsedExpression>,
    pub name: String,
    pub ty: LogicalType,
    pub is_default: bool,
}

impl Default for MacroParameter {
    fn default() -> Self {
        Self {
            expression: None,
            name: String::new(),
            ty: LogicalType::UNKNOWN,
            is_default: false,
        }
    }
}

/// `PARTITIONED BY (...)` / `SORTED BY (...)` (DuckDB's `PartitionSortedOptions`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PartitionSortedOptions {
    pub partition_keys: Vec<ParsedExpression>,
    pub sort_keys: Vec<ParsedExpression>,
}

/// `ON CONFLICT (columns) WHERE ...` (DuckDB's `OnConflictExpressionTarget`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct OnConflictExpressionTarget {
    pub indexed_columns: Vec<String>,
    pub where_clause: Option<ParsedExpression>,
}

/// One end of a window frame (DuckDB's `WindowBoundaryExpression`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WindowBoundaryExpression {
    pub boundary: WindowBoundary,
    pub expr: Option<ParsedExpression>,
}

/// A window frame (DuckDB's `WindowFrame`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WindowFrame {
    pub start: WindowBoundary,
    pub end: WindowBoundary,
    pub exclude_clause: WindowExcludeMode,
    pub start_expr: Option<ParsedExpression>,
    pub end_expr: Option<ParsedExpression>,
}

/// The columns, constraints and options of `CREATE TABLE (...)` (DuckDB's
/// `ColumnElements`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ColumnElements {
    pub columns: ColumnList,
    pub constraints: Vec<Constraint>,
    pub partition_keys: Vec<ParsedExpression>,
    pub sort_keys: Vec<ParsedExpression>,
    pub options: CaseInsensitiveMap<ParsedExpression>,
}

/// The column of `ALTER TABLE ... ADD COLUMN` (DuckDB's `AddColumnEntry`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AddColumnEntry {
    pub ty: LogicalType,
    pub column_path: Vec<String>,
    pub default_value: Option<ParsedExpression>,
}

/// The table and columns of `ANALYZE` / `VACUUM` (DuckDB's `AnalyzeTarget`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AnalyzeTarget {
    pub table: Option<TableRef>,
    pub columns: Vec<String>,
}

/// `FROM repository` of `INSTALL` (DuckDB's `ExtensionRepositoryInfo`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ExtensionRepositoryInfo {
    pub name: String,
    pub repository_is_alias: bool,
}

/// `CREATE TABLE ... AS SELECT ...` (DuckDB's `CreateTableAs`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CreateTableAs {
    pub with_data: bool,
    pub select_statement: Option<SelectStatement>,
    pub column_names: ColumnList,
    pub partition_keys: Vec<ParsedExpression>,
    pub sort_keys: Vec<ParsedExpression>,
    pub options: CaseInsensitiveMap<ParsedExpression>,
}

/// A column definition with its constraints (DuckDB's `ConstraintColumnDefinition`):
/// the constraints whose column index is only known later are kept as types.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConstraintColumnDefinition {
    pub column_definition: ColumnDefinition,
    pub constraint_types: Vec<(bool, ConstraintType)>,
    pub constraints: Vec<Constraint>,
}

/// `GENERATED ALWAYS AS (expr) [VIRTUAL | STORED]` (DuckDB's
/// `GeneratedColumnDefinition`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GeneratedColumnDefinition {
    pub expr: Option<ParsedExpression>,
    pub virtual_column: bool,
    pub default_column: bool,
}

/// `ON UPDATE` / `ON DELETE` actions of a foreign key (DuckDB's `KeyActions`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct KeyActions {
    pub update_action: String,
    pub delete_action: String,
}

/// `DEFAULT VALUES` or the `SELECT` of an `INSERT` (DuckDB's `InsertValues`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct InsertValues {
    pub default_values: bool,
    pub select_statement: Option<SelectStatement>,
}

/// `INTO NAME n VALUE v` of `UNPIVOT` (DuckDB's `UnpivotNameValues`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UnpivotNameValues {
    pub unpivot_names: Vec<String>,
    pub column: PivotColumn,
}

/// The name and scope of `SET` / `RESET` (DuckDB's `SettingInfo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SettingInfo {
    pub name: String,
    pub scope: SetScope,
}

/// What a sequence option sets (DuckDB's `SequenceInfo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SequenceInfo {
    Start,
    Increment,
    Min,
    Max,
    Cycle,
    Owner,
}

/// One option of `CREATE` / `ALTER SEQUENCE` (DuckDB's `SequenceOption`,
/// `ValueSequenceOption` and `QualifiedSequenceOption`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SequenceOption {
    /// An option without a value (`NO MINVALUE`).
    Plain(SequenceInfo),
    Value(SequenceInfo, Value),
    Qualified(SequenceInfo, QualifiedName),
}

impl SequenceOption {
    /// The option's `SequenceInfo`.
    pub const fn info(&self) -> SequenceInfo {
        match self {
            Self::Plain(info) | Self::Value(info, _) | Self::Qualified(info, _) => *info,
        }
    }
}

/// A `CREATE TYPE ... AS ENUM` a `PIVOT` without an `IN` list needs (DuckDB's
/// `CreatePivotEntry`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CreatePivotEntry {
    pub enum_name: String,
    pub base: Option<QueryNode>,
    pub column: ParsedExpression,
    pub subquery: Option<QueryNode>,
    pub has_parameters: bool,
}

/// The expressions and grouping sets of `GROUP BY` (DuckDB's `GroupByNode`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GroupByNode {
    pub group_expressions: Vec<ParsedExpression>,
    pub grouping_sets: Vec<BTreeSet<u64>>,
}

/// A window expression (DuckDB's `unique_ptr<WindowExpression>`): unlike a
/// `ParsedExpression` result, the transformer does not set its location.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WindowExpr(pub ParsedExpression);

/// A base table reference (DuckDB's `unique_ptr<BaseTableRef>`): unlike a `TableRef`
/// result, the transformer does not set its location.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BaseTable(pub TableRef);

/// A part of a date or time (the members of DuckDB's `DatePartSpecifier` the grammar
/// names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DatePartSpecifier {
    Year,
    Month,
    Day,
    Decade,
    Century,
    Millennium,
    Microseconds,
    Milliseconds,
    Second,
    Minute,
    Hour,
    Week,
    Quarter,
}

impl DatePartSpecifier {
    /// The name of the member (`EnumUtil::ToString`).
    pub const fn enum_name(self) -> &'static str {
        match self {
            Self::Year => "YEAR",
            Self::Month => "MONTH",
            Self::Day => "DAY",
            Self::Decade => "DECADE",
            Self::Century => "CENTURY",
            Self::Millennium => "MILLENNIUM",
            Self::Microseconds => "MICROSECONDS",
            Self::Milliseconds => "MILLISECONDS",
            Self::Second => "SECOND",
            Self::Minute => "MINUTE",
            Self::Hour => "HOUR",
            Self::Week => "WEEK",
            Self::Quarter => "QUARTER",
        }
    }

    /// `GetIntervalTargetType`: the type an interval of this unit is counted in.
    pub fn interval_target_type(self) -> LogicalType {
        match self {
            Self::Year
            | Self::Month
            | Self::Day
            | Self::Week
            | Self::Quarter
            | Self::Decade
            | Self::Century
            | Self::Millennium => LogicalType::INTEGER,
            Self::Hour | Self::Minute | Self::Microseconds => LogicalType::BIGINT,
            Self::Milliseconds | Self::Second => LogicalType::DOUBLE,
        }
    }
}
