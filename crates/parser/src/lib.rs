//! DuckDB's SQL parser: SQL text in, parse trees ([`rustdb_ast::SqlStatement`]) out.
//!
//! Parsing happens in two steps, exactly as in DuckDB:
//!
//! 1. [`rustdb_pg_parser`] runs the Postgres-derived grammar and produces a
//!    `libpg_query`-style node tree;
//! 2. the [`transformer`](crate::transformer) turns that tree into DuckDB's parse tree —
//!    statements, query nodes, table refs and parsed expressions — applying the same
//!    rewrites and raising the same errors as `src/parser/transform/**` in DuckDB.
//!
//! [`parse`] is DuckDB's `Parser::ParseQuery`: it replaces Unicode spaces, wraps
//! grammar errors as syntax errors, and splits the query text among the statements.
//! [`json::serialize_sql`] reproduces `json_serialize_sql` on top of it.
//!
//! # Map ordering
//!
//! Some containers DuckDB serializes are `std::unordered_map`s (the named-parameter map,
//! the `EXCLUDE`/`REPLACE`/`RENAME` lists of `*`). Their order is part of the output, so
//! the transformer replays libc++'s hash-table algorithm (`hash_order`) and stores the
//! entries in DuckDB's iteration order.

mod equality;
mod hash_order;
pub mod json;
mod transformer;

use rustdb_ast::{CreateInfo, SqlStatement, SqlStatementKind};
use rustdb_common::{Error, Result};

pub use rustdb_pg_parser::{SimplifiedToken, SimplifiedTokenType, tokenize};

/// The `error_subtype` DuckDB attaches to errors of the grammar (`ParserException::SyntaxError`).
pub const SYNTAX_ERROR: &str = "SYNTAX_ERROR";

/// Options of the parser (DuckDB's `ParserOptions`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParserOptions {
    /// Keep the case of unquoted identifiers (`preserve_identifier_case`, default on).
    pub preserve_identifier_case: bool,
    /// Parse `/` as integer division (`integer_division`, default off).
    pub integer_division: bool,
    /// The nesting depth at which parsing fails (`max_expression_depth`, default 1000).
    pub max_expression_depth: u64,
}

impl Default for ParserOptions {
    fn default() -> Self {
        Self {
            preserve_identifier_case: true,
            integer_division: false,
            max_expression_depth: 1000,
        }
    }
}

/// Parses `query` into statements with DuckDB's default options.
pub fn parse(query: &str) -> Result<Vec<SqlStatement>> {
    parse_with_options(query, &ParserOptions::default())
}

/// Parses `query` into statements (DuckDB's `Parser::ParseQuery`).
///
/// Each statement carries its own text (`query`), with `location` 0 and `length` the
/// text's length, like DuckDB's statements after parsing. Errors of the grammar are
/// parser errors tagged with [`SYNTAX_ERROR`] and positioned in the query; errors of the
/// transformer are whatever DuckDB raises there (parser, binder, not-implemented, ...).
pub fn parse_with_options(query: &str, options: &ParserOptions) -> Result<Vec<SqlStatement>> {
    if let Some(stripped) = rustdb_pg_parser::strip_unicode_spaces(query) {
        return parse_with_options(&stripped, options);
    }
    let pg_options = rustdb_pg_parser::ParserOptions {
        preserve_identifier_case: options.preserve_identifier_case,
    };
    let raw = rustdb_pg_parser::parse_with_options(query, &pg_options).map_err(syntax_error)?;
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let mut statements = transformer::Transformer::new(*options, query).transform_parse_tree(&raw)?;
    if let Some(last) = statements.last_mut() {
        last.length = (query.len() as u64).saturating_sub(last.location);
    }
    for statement in &mut statements {
        let start = usize::try_from(statement.location)
            .unwrap_or(usize::MAX)
            .min(query.len());
        let end = start
            .saturating_add(usize::try_from(statement.length).unwrap_or(usize::MAX))
            .min(query.len());
        statement.query = substring(query, start, end);
        statement.location = 0;
        statement.length = statement.query.len() as u64;
        if let SqlStatementKind::Create(info) = &mut statement.kind {
            set_create_sql(info, &statement.query);
        }
    }
    Ok(statements)
}

/// `query.substr(start, end - start)` on byte offsets (lossy if they split a character).
fn substring(query: &str, start: usize, end: usize) -> String {
    query.get(start..end).map_or_else(
        || String::from_utf8_lossy(&query.as_bytes()[start..end]).into_owned(),
        str::to_owned,
    )
}

fn set_create_sql(info: &mut CreateInfo, sql: &str) {
    info.sql = sql.to_owned();
}

/// `ParserException::SyntaxError`: grammar errors keep their message and position and
/// are tagged as syntax errors.
fn syntax_error(error: Error) -> Error {
    error.with_subtype(SYNTAX_ERROR)
}

#[cfg(test)]
mod tests;
