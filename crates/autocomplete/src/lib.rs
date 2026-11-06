//! The autocomplete extension (DuckDB's `extension/autocomplete`): `sql_auto_complete`
//! suggests how to continue a SQL text from the PEG grammar of DuckDB's SQL dialect and
//! the live catalog, and `check_peg_parser` checks that a text matches that grammar.
//!
//! The grammar ([`matcher`]) is DuckDB's inlined grammar, read by a port of its PEG
//! reader ([`peg_parser`]) and compiled into the same matchers; the tokenizer
//! ([`tokenizer`]) and the ranking of suggestions ([`autocomplete`]) are ports as well,
//! so suggestions, scores and positions are DuckDB's.

mod autocomplete;
mod keyword_helper;
mod matcher;
mod peg_parser;
mod tokenizer;
mod transformer;

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::{
    FunctionData, TableFunction, TableFunctionBindInput, TableFunctionBindOutput, TableFunctionSet,
};
use rustdb_planner::ClientContext;
use rustdb_types::{DataChunk, LogicalType, Value, Vector};

use crate::autocomplete::{AutoCompleteParameters, AutoCompleteSuggestion, CompletionSources};
use crate::matcher::{Grammar, MatchContext, MatchState};
use crate::tokenizer::{MatcherToken, TokenizeState, TokenizerCallbacks, tokenize_input};

/// The extension's name (`AutocompleteExtension::Name`).
pub const EXTENSION_NAME: &str = "autocomplete";

pub use transformer::parse_query;

/// The names of the pragma functions of the database. DuckDB lists them from the
/// catalog; the embedding database provides them.
pub type PragmaNames = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// The suggestions of a `sql_auto_complete` call, computed at bind time (DuckDB's
/// `SQLAutoCompleteFunctionData`).
#[derive(Debug)]
struct SqlAutoCompleteFunctionData {
    suggestions: Vec<AutoCompleteSuggestion>,
}

impl FunctionData for SqlAutoCompleteFunctionData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The binder's client context of a table function call.
fn client_context<'a>(input: &TableFunctionBindInput<'a>) -> Result<&'a ClientContext> {
    input
        .binder
        .and_then(|binder| binder.downcast_ref::<ClientContext>())
        .ok_or_else(|| Error::internal("autocomplete function bound without a client context"))
}

/// The SQL text argument; NULL is an error.
fn sql_argument(input: &TableFunctionBindInput<'_>) -> Result<String> {
    let sql = input
        .inputs
        .first()
        .ok_or_else(|| Error::internal("autocomplete function called without arguments"))?;
    if sql.is_null() {
        return Err(Error::binder("sql_auto_complete first parameter cannot be NULL"));
    }
    sql.to_varchar()
}

/// The value of a UBIGINT named parameter.
fn count_parameter(value: &Value) -> usize {
    value
        .as_u64()
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(usize::MAX)
}

/// `SQLAutoCompleteBind`: computes the suggestions.
fn sql_auto_complete_bind(
    input: &TableFunctionBindInput<'_>,
    pragma_names: &PragmaNames,
) -> Result<TableFunctionBindOutput> {
    let sql = sql_argument(input)?;
    let mut parameters = AutoCompleteParameters::default();
    for (name, value) in input.named_parameters {
        match name.as_bytes() {
            b"max_suggestion_count" => parameters.max_suggestion_count = count_parameter(value),
            b"max_file_suggestion_count" => parameters.max_file_suggestion_count = count_parameter(value),
            b"max_exact_suggestion_count" => parameters.max_exact_suggestion_count = count_parameter(value),
            _ => return Err(Error::internal("Unsupported parameter for SQL auto complete")),
        }
    }
    let sources = CompletionSources {
        context: client_context(input)?,
        pragma_names: pragma_names.as_ref(),
    };
    let suggestions = autocomplete::generate_suggestions(&sources, &sql, &mut parameters)?;
    Ok(TableFunctionBindOutput {
        return_types: vec![
            LogicalType::VARCHAR,
            LogicalType::INTEGER,
            LogicalType::VARCHAR,
            LogicalType::UBIGINT,
            LogicalType::VARCHAR,
        ],
        names: [
            "suggestion",
            "suggestion_start",
            "suggestion_type",
            "suggestion_score",
            "extra_char",
        ]
        .map(Into::into)
        .to_vec(),
        bind_data: Some(Arc::new(SqlAutoCompleteFunctionData { suggestions })),
    })
}

/// `SQLAutoCompleteFunction`: returns the suggestions, a vector at a time.
fn sql_auto_complete_scan(
    bind_data: Option<&dyn FunctionData>,
    position: &mut u64,
    chunk: &mut DataChunk,
) -> Result<bool> {
    let data = bind_data
        .and_then(|data| data.as_any().downcast_ref::<SqlAutoCompleteFunctionData>())
        .ok_or_else(|| Error::internal("sql_auto_complete without bind data"))?;
    let start = usize::try_from(*position)
        .unwrap_or(usize::MAX)
        .min(data.suggestions.len());
    let end = (start + STANDARD_VECTOR_SIZE).min(data.suggestions.len());
    let types = [
        LogicalType::VARCHAR,
        LogicalType::INTEGER,
        LogicalType::VARCHAR,
        LogicalType::UBIGINT,
        LogicalType::VARCHAR,
    ];
    let mut columns: Vec<Vector> = types
        .into_iter()
        .map(|ty| Vector::flat(ty, (end - start).max(1)))
        .collect();
    for (row, entry) in data.suggestions[start..end].iter().enumerate() {
        let values = [
            Value::try_varchar(&entry.text)?,
            Value::integer(i32::try_from(entry.pos).unwrap_or(i32::MAX)),
            Value::varchar(entry.suggestion_type),
            Value::ubigint(entry.score as u64),
            entry.extra_char.map_or_else(
                || Value::null(LogicalType::VARCHAR),
                |extra_char| Value::varchar(extra_char.to_string()),
            ),
        ];
        for (column, value) in columns.iter_mut().zip(&values) {
            column.set_value(row, value)?;
        }
    }
    *chunk = DataChunk::new(columns, end - start);
    *position = end as u64;
    Ok(end < data.suggestions.len())
}

/// The tokenizer of `check_peg_parser` (DuckDB's `ParserTokenizer`): keeps the tokens
/// of every statement, including the trailing word.
#[derive(Debug, Default)]
struct ParserTokenizer {
    statements: Vec<Vec<MatcherToken>>,
}

impl TokenizerCallbacks for ParserTokenizer {
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

/// `CheckPEGParserBind`: an error unless every statement of the text matches the
/// grammar completely.
fn check_peg_parser_bind(input: &TableFunctionBindInput<'_>) -> Result<TableFunctionBindOutput> {
    let output = TableFunctionBindOutput {
        return_types: vec![LogicalType::BOOLEAN],
        names: vec!["success".to_owned().into()],
        bind_data: None,
    };
    let sql = sql_argument(input)?;
    let stripped = rustdb_pg_parser::strip_unicode_spaces(&sql);
    let clean_sql = stripped.as_deref().unwrap_or(&sql);
    let mut root_tokens = Vec::new();
    let mut tokenizer = ParserTokenizer::default();
    if !tokenize_input(clean_sql, &mut root_tokens, &mut tokenizer) {
        return Ok(output);
    }
    tokenizer.statements.push(root_tokens);
    let grammar = Grammar::get()?;
    for tokens in tokenizer.statements.iter().filter(|tokens| !tokens.is_empty()) {
        let mut ctx = MatchContext::new(tokens);
        let mut state = MatchState::default();
        let matched = grammar.match_root(&mut ctx, &mut state)?;
        if !matched || state.token_index < tokens.len() {
            let token_list: Vec<String> = tokens
                .iter()
                .enumerate()
                .map(|(i, token)| {
                    let pad = if i < 10 { " " } else { "" };
                    format!("{pad}{i}:{}", token.text)
                })
                .collect();
            let failed_at = tokens
                .get(state.token_index)
                .map_or("", |token| token.text.as_str());
            return Err(Error::binder(format!(
                "Failed to parse query \"{sql}\" - did not consume all tokens (got to token {} - {failed_at})\nTokens:\n{}",
                state.token_index,
                token_list.join("\n")
            )));
        }
    }
    Ok(output)
}

/// The bind data of `enable_peg_parser()` / `disable_peg_parser()`: the connection whose
/// configuration the call changes.
#[derive(Debug)]
struct PegParserSwitchData {
    client: Arc<rustdb_function::FunctionClient>,
}

impl FunctionData for PegParserSwitchData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `enable_peg_parser()` (`allow_parser_override_extension = 'strict'`) or
/// `disable_peg_parser()` (`'default'`): `EnablePEGParserBind` with
/// `EnablePEGParserFunction` / `DisablePEGParserFunction`.
fn peg_parser_switch(name: &'static str, setting: &'static str) -> TableFunction {
    TableFunction::new(name, Vec::new())
        .with_bind(|input| {
            Ok(TableFunctionBindOutput {
                return_types: vec![LogicalType::BOOLEAN],
                names: vec!["success".to_owned().into()],
                bind_data: Some(Arc::new(PegParserSwitchData {
                    client: Arc::clone(&client_context(input)?.function_client),
                })),
            })
        })
        .with_scan(move |bind_data, _, _, chunk| {
            let data = bind_data
                .and_then(|data| data.as_any().downcast_ref::<PegParserSwitchData>())
                .ok_or_else(|| Error::internal("peg parser switch without bind data"))?;
            data.client
                .push_client_request(rustdb_function::ClientRequest::SetGlobalOption {
                    name: "allow_parser_override_extension".to_owned(),
                    value: Value::varchar(setting),
                });
            chunk.set_len(0);
            Ok(false)
        })
}

/// The table functions of the extension (`LoadInternal`): `sql_auto_complete`,
/// `check_peg_parser`, `enable_peg_parser` and `disable_peg_parser`.
pub fn autocomplete_table_functions(pragma_names: PragmaNames) -> Vec<TableFunctionSet> {
    let mut auto_complete = TableFunction::new("sql_auto_complete", vec![LogicalType::VARCHAR])
        .with_bind(move |input| sql_auto_complete_bind(input, &pragma_names))
        .with_scan(|bind_data, _, state, chunk| {
            sql_auto_complete_scan(bind_data, &mut state.position, chunk)
        });
    auto_complete.named_parameters = [
        "max_suggestion_count",
        "max_file_suggestion_count",
        "max_exact_suggestion_count",
    ]
    .map(|name| (name.to_owned(), LogicalType::UBIGINT))
    .to_vec();
    let check_peg_parser = TableFunction::new("check_peg_parser", vec![LogicalType::VARCHAR])
        .with_bind(check_peg_parser_bind)
        .with_scan(|_, _, _, chunk| {
            chunk.set_len(0);
            Ok(false)
        });
    [
        auto_complete,
        check_peg_parser,
        peg_parser_switch("enable_peg_parser", "strict"),
        peg_parser_switch("disable_peg_parser", "default"),
    ]
    .into_iter()
    .map(|function| {
        let mut set = TableFunctionSet::new(Arc::clone(&function.name));
        set.add(function);
        set
    })
    .collect()
}
