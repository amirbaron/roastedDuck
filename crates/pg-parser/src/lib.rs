//! DuckDB's Postgres-dialect SQL parser, ported to Rust.
//!
//! This crate is stage one of the parser port: it turns SQL text into the Postgres
//! parse trees that DuckDB's `libpg_query` produces (`PGRawStmt` lists), with the
//! same accept/reject decisions and byte-identical error messages. Converting these
//! trees into DuckDB's AST (`src/parser/transform`) is the next stage.
//!
//! # Structure
//!
//! * [`scanner`](crate::scanner) — a hand-written port of the flex scanner `scan.l`
//!   and of the `base_yylex` lookahead filter.
//! * [`keywords`] — the keyword list with a hash lookup.
//! * `tables` — bison's LALR(1) tables, extracted from DuckDB's generated
//!   `src_backend_parser_gram.cpp` (generated code, not edited by hand).
//! * `driver` — a port of bison's `yyparse` skeleton that runs the tables.
//! * `actions` — the semantic actions of every grammar rule, one function per
//!   nonterminal, building [`nodes`].
//! * `helpers` — `makefuncs` and the helper functions of `gram.y`.
//!
//! # Regenerating after a grammar change
//!
//! The parse tables are data and are regenerated mechanically:
//!
//! 1. Regenerate DuckDB's grammar with its own tooling (`python3 scripts/generate_grammar.py`
//!    in the DuckDB tree, bison 2.3), which rewrites
//!    `third_party/libpg_query/src_backend_parser_gram.cpp`.
//! 2. Run `python3 crates/pg-parser/scripts/extract_tables.py <duckdb-root>` to rewrite
//!    `src/tables.rs` and `src/keywords/list.rs`.
//! 3. Port the changed actions: rule numbers shift when rules are added, so re-run
//!    `scripts/translate_actions.py` into a scratch copy, diff it against `src/actions`
//!    and carry the changes over. The `rule_actions_match_grammar` test fails until
//!    every action is attached to the right production.

mod actions;
mod driver;
mod error;
mod helpers;
pub mod keywords;
pub mod nodes;
mod scanner;
mod semantic;
#[rustfmt::skip]
#[allow(clippy::unreadable_literal, non_upper_case_globals, dead_code)]
mod tables;
mod tokenize;
mod unicode_space;

use rustdb_common::Result;

pub use keywords::{Keyword, KeywordCategory};
pub use nodes::{Node, RawStmt};
pub use tokenize::{SimplifiedToken, SimplifiedTokenType, tokenize};
pub use unicode_space::strip_unicode_spaces;

use crate::scanner::{ScanOptions, Scanner};

/// Options of the Postgres parser (the subset of DuckDB's `ParserOptions` it reads).
#[derive(Clone, Copy, Debug)]
pub struct ParserOptions {
    /// Keep the case of unquoted identifiers (DuckDB's default) instead of folding
    /// them to lower case.
    pub preserve_identifier_case: bool,
}

impl Default for ParserOptions {
    fn default() -> Self {
        Self {
            preserve_identifier_case: true,
        }
    }
}

/// Parses `sql` into raw statements with DuckDB's default options.
///
/// Each [`RawStmt`] records where its statement starts (`stmt_location`, a byte
/// offset) and how long it is (`stmt_len`; 0 means "to the end of the input"),
/// exactly as `PostgresParser::Parse` reports them. Empty statements (`;;`) are
/// dropped. A query that is empty or only whitespace yields no statements.
pub fn parse(sql: &str) -> Result<Vec<RawStmt>> {
    parse_with_options(sql, &ParserOptions::default())
}

/// Parses `sql` into raw statements.
///
/// Errors are parser errors whose message and query location match DuckDB's
/// (`syntax error at or near "..."`, `unterminated quoted string ...`, ...).
pub fn parse_with_options(sql: &str, options: &ParserOptions) -> Result<Vec<RawStmt>> {
    let mut scanner = Scanner::new(
        sql,
        ScanOptions {
            preserve_identifier_case: options.preserve_identifier_case,
        },
    );
    let list = driver::parse(&mut scanner)?;
    driver::into_raw_statements(list)
}

/// The category of `text` if it is a keyword (DuckDB's `Parser::IsKeyword`).
pub fn keyword_category(text: &str) -> Option<KeywordCategory> {
    keywords::lookup(text).map(|keyword| keyword.category)
}

#[cfg(test)]
mod tests;
