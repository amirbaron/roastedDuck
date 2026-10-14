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

mod expression;
mod generated;
pub mod json;
mod parsed_data;
mod query_node;
mod tableref;

pub use expression::StarExpression;
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
pub use parsed_data::{CreateMacroInfo, CreateViewInfo, MacroFunction, MacroFunctionKind, SampleOptions};
pub use query_node::SetOperationNode;

#[cfg(test)]
mod tests;
