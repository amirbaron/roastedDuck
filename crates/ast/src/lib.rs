//! The parse tree: what the parser produces and the binder consumes.
//!
//! The tree mirrors DuckDB's parser classes one to one — statements, query nodes, table
//! refs, parsed expressions, result modifiers, CTEs, and the parse/create infos of DDL
//! statements — and serializes exactly like DuckDB's (`json_serialize_sql`, and the
//! binary format the catalog and WAL store). Class hierarchies are Rust enums:
//!
//! - a hierarchy whose root only holds the subclass tag is an enum of subclasses
//!   ([`ResultModifier`], [`Constraint`], [`ParseInfo`], ...);
//! - a hierarchy whose root has shared members is a struct with those members and a
//!   `kind` enum of subclasses ([`ParsedExpression`], [`QueryNode`], [`TableRef`],
//!   [`CreateInfo`], ...).
//!
//! Almost all types and their (de)serialization are generated from DuckDB's
//! serialization specs by `tools/codegen` (`src/generated/`); classes whose in-memory
//! form differs from their serialized form are written by hand next to it.
//!
//! [`SqlStatement`] wraps any statement with its location in the query text (DuckDB
//! does not serialize statements other than `SELECT`, so neither does this crate), and
//! [`visit`] walks a tree mutably.

mod create_sql;
mod display;
mod expression;
mod generated;
pub mod json;
pub mod keyword;
mod parsed_data;
mod query_node;
mod sql_statement;
mod tableref;
pub mod visit;

pub use create_sql::{columns_to_sql, index_expressions_to_sql, qualifier_to_string, sequence_to_sql};
pub use display::{
    expression_type_to_operator, expression_type_to_string, lambda_parameters, logical_type_to_string,
    value_to_sql_string,
};
pub use expression::{BoundExpression, StarExpression};
pub use generated::constraint::*;
pub use generated::create_info::*;
pub use generated::dependency::*;
pub use generated::enums::*;
pub use generated::extra_drop_info::*;
pub use generated::macro_function::*;
pub use generated::nodes::*;
pub use generated::parse_info::*;
pub use generated::parsed_expression::*;
pub use generated::query_node::*;
pub use generated::result_modifier::*;
pub use generated::statement::*;
pub use generated::tableref::*;
pub use parsed_data::{
    CreateMacroInfo, CreateSecretInfo, CreateViewInfo, MacroFunction, MacroFunctionKind, SampleOptions,
};
pub use query_node::SetOperationNode;
pub use sql_statement::{
    CallStatement, CopyDatabaseStatement, CopyDatabaseType, DeleteStatement, ExecuteStatement,
    ExplainStatement, ExportStatement, InsertStatement, MergeIntoAction, MergeIntoStatement, OnConflictInfo,
    PrepareStatement, SetStatement, SqlStatement, SqlStatementKind, UpdateSetInfo, UpdateStatement,
};
pub use tableref::{
    BoundPivotSpec, ExternalDependency, PivotValueElement, retain_external_replacement_list,
    retain_external_replacements,
};

#[cfg(test)]
mod tests;
