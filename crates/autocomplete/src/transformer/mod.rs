//! The PEG transformer (DuckDB's `PEGTransformer` and `PEGTransformerFactory`,
//! `extension/autocomplete/transformer/`): the parse tree the grammar matcher builds is
//! turned into the same parse tree DuckDB's Postgres transformer produces. With
//! `allow_parser_override_extension` set, it replaces the Postgres parser.
//!
//! DuckDB registers one transform function per grammar rule, by rule name, and
//! `Transform<T>(parse_result)` runs the function of the rule the parse result was
//! matched by, expecting a result of type `T`. Here every transform function is a
//! method of [`PegTransformer`] named after DuckDB's (`TransformSelectStatement` is
//! `transform_select_statement`), in a module named after DuckDB's file
//! (`transform_select.cpp` is `select`), and [`rules`] lists, per result type, the rules
//! and their methods; [`PegTransformer::transform`] dispatches through it.

#![allow(
    clippy::unused_self,
    clippy::unnecessary_wraps,
    reason = "every transform function has the signature the rule dispatch calls, like DuckDB's"
)]

mod nodes;
pub(crate) mod parse_result;
mod prelude;
mod rules;

mod alter;
mod analyze;
mod attach;
mod call;
mod checkpoint;
mod comment;
mod common;
mod copy;
mod create_index;
mod create_macro;
mod create_schema;
mod create_secret;
mod create_sequence;
mod create_table;
mod create_type;
mod create_view;
mod deallocate;
mod delete;
mod describe;
mod detach;
mod drop;
mod execute;
mod explain;
mod export;
mod expression;
mod import;
mod insert;
mod load;
mod merge_into;
mod pivot;
mod pragma;
mod prepare;
mod select;
mod set;
mod transaction;
mod update;
mod use_statement;
mod vacuum;

use rustdb_ast::{CommonTableExpressionMap, ParsedExpression, SqlStatement, SqlStatementKind, TableRef};
use rustdb_common::{CaseInsensitiveMap, Error, Result};
use rustdb_parser::{ParserOptions, SYNTAX_ERROR};

use crate::matcher::{Grammar, MatchContext, MatchState};
use crate::tokenizer::{MatcherToken, TokenizeState, TokenizerCallbacks, tokenize_input};

use self::nodes::{CreatePivotEntry, WindowExpr};
use self::parse_result::{List, ParseResult};

/// How a prepared-statement parameter was written (DuckDB's `PreparedParamType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreparedParamType {
    /// `?`
    AutoIncrement,
    /// `$1`
    Positional,
    /// `$name`
    Named,
    /// No parameter seen yet.
    Invalid,
}

/// The state of transforming one statement (DuckDB's `PEGTransformer`).
#[derive(Debug)]
pub(crate) struct PegTransformer {
    pub options: ParserOptions,
    /// Parameter names (and numbers) seen so far, with their indexes, in DuckDB's
    /// iteration order (`named_parameter_map`).
    pub named_parameter_map: rustdb_parser::hash_order::CiHashMap<u64>,
    /// The highest parameter index used so far (`prepared_statement_parameter_index`).
    pub prepared_statement_parameter_index: u64,
    pub last_param_type: PreparedParamType,
    /// `WINDOW name AS (...)` definitions (`window_clauses`).
    pub window_clauses: CaseInsensitiveMap<WindowExpr>,
    /// The enum types `PIVOT`s without `IN` lists need (`pivot_entries`).
    pub pivot_entries: Vec<CreatePivotEntry>,
    /// The CTEs of the enclosing `WITH` clauses, outermost first (`stored_cte_map`).
    pub stored_cte_map: Vec<CommonTableExpressionMap>,
    /// Whether a window definition is being transformed (`in_window_definition`).
    pub in_window_definition: bool,
}

/// A value transform rules produce (the `T` of DuckDB's `PEGTransformer::Transform<T>`).
/// A transform function (DuckDB's `PEGTransformerFactory::Transform*` functions).
pub(crate) type TransformFunction<T> = fn(&mut PegTransformer, &ParseResult) -> Result<T>;

pub(crate) trait TransformResult: Sized {
    /// The transform function of `rule`, if that rule produces `Self`. A function
    /// pointer rather than a call: the transform functions recurse through
    /// [`PegTransformer::transform`], and inlining them all into one dispatching frame
    /// would make every level of a deeply nested query that large.
    fn transform_function(rule: &str) -> Option<TransformFunction<Self>>;

    /// `SetResultLocation`: expressions and table refs without a location get the
    /// position of the parse result.
    fn set_location(&mut self, _offset: Option<usize>) {}
}

/// A value enum rules stand for (the `T` of DuckDB's `PEGTransformer::TransformEnum<T>`).
pub(crate) trait TransformEnum: Sized {
    /// The value of the enum rule `rule` (`RegisterEnum`), if it stands for a `Self`.
    fn from_rule(rule: &str) -> Option<Self>;
}

/// `SetResultLocation` of expressions.
fn set_expression_location(expression: &mut ParsedExpression, offset: Option<usize>) {
    if let Some(offset) = offset
        && expression.query_location.is_none()
    {
        expression.query_location = Some(offset as u64);
    }
}

/// `SetResultLocation` of table refs.
fn set_table_ref_location(table_ref: &mut TableRef, offset: Option<usize>) {
    if let Some(offset) = offset
        && table_ref.query_location.is_none()
    {
        table_ref.query_location = Some(offset as u64);
    }
}

impl PegTransformer {
    pub fn new(options: ParserOptions) -> Self {
        Self {
            options,
            named_parameter_map: rustdb_parser::hash_order::CiHashMap::new(),
            prepared_statement_parameter_index: 0,
            last_param_type: PreparedParamType::Invalid,
            window_clauses: CaseInsensitiveMap::default(),
            pivot_entries: Vec::new(),
            stored_cte_map: Vec::new(),
            in_window_definition: false,
        }
    }

    /// `PEGTransformer::Transform<T>`: runs the transform function of the rule
    /// `parse_result` was matched by.
    pub fn transform<T: TransformResult>(&mut self, parse_result: &ParseResult) -> Result<T> {
        match T::transform_function(parse_result.name) {
            Some(function) => {
                let mut result = function(self, parse_result)?;
                result.set_location(parse_result.offset);
                Ok(result)
            }
            None if rules::is_transform_rule(parse_result.name) => Err(Error::internal(format!(
                "Transformer for rule '{}' returned an unexpected type.",
                parse_result.name
            ))),
            None => Err(Error::not_implemented(format!(
                "No transformer function found for rule '{}'",
                parse_result.name
            ))),
        }
    }

    /// `Transform<T>(list, index)`: transforms the child `index` of `list`.
    pub fn transform_child<T: TransformResult>(&mut self, list: List<'_>, index: usize) -> Result<T> {
        let child = list.child(index)?;
        self.transform(child)
    }

    /// `TransformOptional<T>(list, index, target)`: transforms the optional child
    /// `index` of `list`, if it matched.
    pub fn transform_optional<T: TransformResult>(
        &mut self,
        list: List<'_>,
        index: usize,
    ) -> Result<Option<T>> {
        list.optional(index)?
            .map(|child| self.transform(child))
            .transpose()
    }

    /// `PEGTransformer::TransformEnum<T>`: the value the enum rule `parse_result` was
    /// matched by stands for.
    pub fn transform_enum<T: TransformEnum>(parse_result: &ParseResult) -> Result<T> {
        if let Some(value) = T::from_rule(parse_result.name) {
            return Ok(value);
        }
        if rules::is_enum_rule(parse_result.name) {
            return Err(Error::internal(format!(
                "Enum mapping for rule '{}' has an unexpected type.",
                parse_result.name
            )));
        }
        Err(Error::parser(format!(
            "Enum transform failed: could not find mapping for '{}'",
            parse_result.name
        )))
    }

    /// `PEGTransformer::Clear`.
    fn clear(&mut self) {
        self.clear_parameters();
        self.pivot_entries.clear();
        self.stored_cte_map.clear();
    }

    /// `PEGTransformer::ClearParameters`.
    pub fn clear_parameters(&mut self) {
        self.prepared_statement_parameter_index = 0;
        self.named_parameter_map.clear();
    }

    /// `PEGTransformer::ParamTypeCheck`: named and positional parameters cannot be
    /// mixed.
    fn param_type_check(last_type: PreparedParamType, new_type: PreparedParamType) -> Result<()> {
        if last_type == PreparedParamType::Invalid {
            return Ok(());
        }
        if (last_type == PreparedParamType::Named) != (new_type == PreparedParamType::Named) {
            return Err(Error::not_implemented(
                "Mixing named and positional parameters is not supported yet",
            ));
        }
        Ok(())
    }

    /// `PEGTransformer::SetParam`.
    pub fn set_param(&mut self, name: &str, index: u64, param_type: PreparedParamType) -> Result<()> {
        Self::param_type_check(self.last_param_type, param_type)?;
        self.last_param_type = param_type;
        self.named_parameter_map.set(name.to_owned(), index);
        Ok(())
    }

    /// `PEGTransformer::GetParam`: the index of the parameter called `name`, if known.
    pub fn get_param(&self, name: &str, param_type: PreparedParamType) -> Result<Option<u64>> {
        Self::param_type_check(self.last_param_type, param_type)?;
        Ok(self.named_parameter_map.get(name).copied())
    }

    /// `PEGTransformer::ExtractCTEsRecursive`: copies of the CTEs of the enclosing `WITH`
    /// clauses, innermost first; a name defined twice keeps its innermost definition.
    pub fn extract_ctes_recursive(&self, cte_map: &mut CommonTableExpressionMap) {
        for scope in self.stored_cte_map.iter().rev() {
            for (name, info) in scope.map.iter() {
                if !cte_map.map.contains_key(name) {
                    cte_map.map.insert(name, info.clone());
                }
            }
        }
    }

    /// `PEGTransformer::GetWindowClause`: a copy of the named window `name`.
    pub fn get_window_clause(&self, name: &str) -> Result<WindowExpr> {
        self.window_clauses
            .get(name)
            .cloned()
            .ok_or_else(|| Error::parser(format!("window \"{name}\" does not exist")))
    }

    /// `PEGTransformer::PivotEntryCheck`: CREATE VIEW / MACRO cannot use a PIVOT that
    /// reads its values from the data.
    pub fn pivot_entry_check(&self, kind: &str) -> Result<()> {
        if let Some(entry) = self.pivot_entries.first() {
            return Err(Error::parser(format!(
                "PIVOT statements with pivot elements extracted from the data cannot be used in {kind}s.\nIn order \
                 to use PIVOT in a {kind} the PIVOT values must be manually specified, e.g.:\nPIVOT ... ON {} IN \
                 (val1, val2, ...)",
                entry.column
            )));
        }
        Ok(())
    }

    /// `TransformStatement`: the statement, with the prepared-statement parameters.
    fn transform_statement(&mut self, parse_result: &ParseResult) -> Result<SqlStatement> {
        let list = parse_result.list()?;
        let mut statement: SqlStatement = self.transform(list.choice(0)?)?;
        if !self.named_parameter_map.is_empty() {
            let map: CaseInsensitiveMap<u64> = self
                .named_parameter_map
                .iter()
                .map(|(name, index)| (name, *index))
                .collect();
            if let SqlStatementKind::Select(select) = &mut statement.kind {
                select.named_param_map = map.clone();
            }
            statement.named_param_map = map;
        }
        Ok(statement)
    }
}

/// `PEGTransformerFactory::Transform`: the statement of `tokens`, or DuckDB's syntax
/// error at the furthest token the grammar reached.
fn transform_tokens(tokens: &[MatcherToken], options: ParserOptions) -> Result<SqlStatement> {
    let grammar = Grammar::get()?;
    let mut ctx = MatchContext::new(tokens);
    ctx.preserve_identifier_case = options.preserve_identifier_case;
    let mut state = MatchState::default();
    let parse_result = grammar.match_root_parse_result(&mut ctx, &mut state)?;
    let Some(mut parse_result) = parse_result.filter(|_| state.token_index >= tokens.len()) else {
        let error_index = ctx.max_token_index.min(tokens.len().saturating_sub(1));
        let error_token = tokens
            .get(error_index)
            .ok_or_else(|| Error::internal("syntax error without tokens"))?;
        return Err(
            Error::parser(format!("syntax error at or near \"{}\"", error_token.text))
                .with_subtype(SYNTAX_ERROR)
                .with_position(Some(error_token.offset as u64)),
        );
    };
    parse_result.name = "Statement";
    let mut transformer = PegTransformer::new(options);
    let mut statement = transformer.transform_statement(&parse_result)?;
    if !transformer.pivot_entries.is_empty() {
        statement = transformer.create_pivot_statement(statement)?;
    }
    transformer.clear();
    Ok(statement)
}

/// The tokenizer of the parser override (DuckDB's `ParserTokenizer`): the tokens of
/// every statement.
#[derive(Debug, Default)]
struct StatementTokenizer {
    statements: Vec<Vec<MatcherToken>>,
}

impl TokenizerCallbacks for StatementTokenizer {
    fn on_statement_end(&mut self, tokens: &mut Vec<MatcherToken>, _pos: usize) {
        self.statements.push(std::mem::take(tokens));
    }

    fn on_last_token(
        &mut self,
        tokens: &mut Vec<MatcherToken>,
        _: TokenizeState,
        last_word: String,
        last_pos: usize,
    ) {
        if !last_word.is_empty() {
            tokens.push(MatcherToken::new(last_word, last_pos));
        }
    }
}

/// Parses `query` with the PEG grammar (DuckDB's `PEGParserExtension::PEGParser`): the
/// statements with their text, like `Parser::ParseQuery`.
pub fn parse_query(query: &str, options: ParserOptions) -> Result<Vec<SqlStatement>> {
    let mut root_tokens = Vec::new();
    let mut tokenizer = StatementTokenizer::default();
    tokenize_input(query, &mut root_tokens, &mut tokenizer);
    tokenizer.statements.push(root_tokens);
    let mut statements = Vec::new();
    for tokens in tokenizer.statements.iter().filter(|tokens| !tokens.is_empty()) {
        let mut statement = transform_tokens(tokens, options)?;
        let first = tokens[0].offset;
        let last = &tokens[tokens.len() - 1];
        statement.location = first as u64;
        statement.length = (last.offset + last.text.len()).saturating_sub(first) as u64;
        statements.push(statement);
    }
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
        statement.query = String::from_utf8_lossy(&query.as_bytes()[start..end]).into_owned();
        statement.location = 0;
        statement.length = statement.query.len() as u64;
        if let SqlStatementKind::Create(info) = &mut statement.kind {
            statement.query.clone_into(&mut info.sql);
        }
    }
    Ok(statements)
}
