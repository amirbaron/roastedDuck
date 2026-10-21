//! DuckDB's SQL parser: SQL text in, parse trees ([`rustdb_ast::SqlStatement`]) out.
//!
//! Parsing happens in two steps, exactly as in DuckDB:
//!
//! 1. [`rustdb_pg_parser`] runs the Postgres-derived grammar and produces a
//!    `libpg_query`-style node tree;
//! 2. the transformer turns that tree into DuckDB's parse tree — statements, query
//!    nodes, table refs and parsed expressions — applying the same rewrites and raising
//!    the same errors as `src/parser/transform/**` in DuckDB.
//!
//! [`parse`] is DuckDB's `Parser::ParseQuery`: it replaces Unicode spaces, wraps
//! grammar errors as syntax errors, and splits the query text among the statements.
//! [`json::serialize_sql`] reproduces `json_serialize_sql` on top of it.
//!
//! # Layout
//!
//! `transformer/` has one module per area of DuckDB's `transform/` directory:
//! `expression` (the dispatch and small expressions), `constant`, `star` (column refs
//! and `*`), `operator`, `function` (calls and windows), `subquery`, `typename`,
//! `select` (SELECT / set operations / VALUES / SHOW), `tableref`, `cte`, `pivot`,
//! `helpers` (ORDER BY, GROUP BY, samples, aliases), `ddl`, `dml` and `misc`; `build`
//! holds the parse-tree constructors with DuckDB's defaults. `equality` ports
//! `ParsedExpression::Equals` (used to deduplicate GROUP BY expressions), and
//! `candidates` the "Candidates: ..." hints of unrecognized enum values.
//!
//! Two files are generated: `transformer/node_tag.rs` (node tag numbers and names for
//! error messages) by `scripts/gen_node_tag.py <duckdb checkout>`, and the libc++
//! iteration orders `hash_order_fixture.txt` by `scripts/hash_order_fixture.cpp`.
//!
//! # Map ordering
//!
//! Some containers DuckDB serializes are `std::unordered_map`s (the named-parameter map,
//! the `EXCLUDE`/`REPLACE`/`RENAME` lists of `*`). Their order is part of the output, so
//! the transformer replays libc++'s hash-table algorithm (`hash_order`) and stores the
//! entries in DuckDB's iteration order.
//!
//! # Validation
//!
//! `examples/json_check` compares [`json::serialize_sql`] with the output of the C++
//! `json_serialize_sql` that `tools/corpus` records for every statement of the
//! sqllogictest corpus (`cargo run -p corpus -- extract`, then `-- oracle`), in all five
//! option variants it records:
//!
//! ```text
//! cargo run --release -p rustdb-parser --example json_check -- \
//!     target/corpus/statements.jsonl target/corpus/serialized.jsonl
//! ```
//!
//! Besides whole queries, [`parse_expression_list`] and its siblings parse SQL
//! fragments, and [`highlight`] classifies the tokens of queries and error messages.
//!
//! `examples/parse_bench` and `scripts/parser_bench.cpp` time [`parse`] against
//! DuckDB's `Parser::ParseQuery` on a corpus file.

mod candidates;
pub mod equality;
pub mod hash_order;
pub mod highlight;
pub mod json;
mod lists;
mod transformer;

pub use transformer::build;
pub use transformer::helpers::{get_value_f64, get_value_i64};
pub use transformer::expression::{negate_comparison, operator_to_expression_type};
pub use transformer::function::{
    is_excludable_window_function, is_orderable_window_function, window_to_expression_type,
};
pub use transformer::star::{QualifiedColumnMap, serialized_exclude_list};

/// `ParsedExpression::IsScalar`, `HasSubquery` and child enumeration, for the PEG
/// transformer.
pub use equality::{children as expression_children, has_subquery, is_scalar};

use rustdb_ast::{CreateInfo, SqlStatement, SqlStatementKind};
use rustdb_common::{Error, Result};

pub use lists::{
    GroupBy, parse_column_definition, parse_column_list, parse_expression_list, parse_group_by_list,
    parse_order_list, parse_update_list, parse_values_list,
};
pub use rustdb_pg_parser::keywords::{Keyword, KeywordCategory, keyword_list};
pub use rustdb_pg_parser::{
    SimplifiedToken, SimplifiedTokenType, keyword_category, strip_unicode_spaces, tokenize,
};

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
    let raw = rustdb_pg_parser::parse_with_options(query, &pg_options).map_err(|error| syntax_error(&error))?;
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

/// `SplitQueries`: the statements of `query` at its `;` tokens, trimmed and each ending in
/// its `;` (the last one as it is).
pub fn split_queries(query: &str) -> Vec<String> {
    // `StringUtil::Trim`: the C locale's white space.
    let trim = |segment: &str| {
        segment
            .trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'))
            .to_owned()
    };
    let mut queries = Vec::new();
    let mut last_split = 0;
    for token in tokenize(query) {
        if token.kind == SimplifiedTokenType::Operator && query.as_bytes().get(token.start) == Some(&b';') {
            let segment = trim(&substring(query, last_split, token.start));
            if !segment.is_empty() {
                queries.push(segment + ";");
            }
            last_split = token.start + 1;
        }
    }
    let segment = trim(&substring(query, last_split, query.len()));
    if !segment.is_empty() {
        queries.push(segment);
    }
    queries
}

/// `query.substr(start, end - start)` on byte offsets (lossy if they split a character).
fn substring(query: &str, start: usize, end: usize) -> String {
    query.get(start..end).map_or_else(
        || String::from_utf8_lossy(&query.as_bytes()[start..end]).into_owned(),
        str::to_owned,
    )
}

fn set_create_sql(info: &mut CreateInfo, sql: &str) {
    sql.clone_into(&mut info.sql);
}

/// `ParserException::SyntaxError`: grammar errors keep their message and position and
/// are tagged as syntax errors (the subtype set before the position).
fn syntax_error(error: &Error) -> Error {
    let position = error.position();
    Error::new(error.kind(), error.raw_message().to_owned())
        .with_subtype(SYNTAX_ERROR)
        .with_position(position)
}

#[cfg(test)]
mod tests;
